use std::io::{self, Write, Seek, SeekFrom, ErrorKind};
use byteorder::{LittleEndian, ReadBytesExt};
use std::io::Read;

use crate::errors::{ParseError, Result};

//
// BitOggStream: writing bits and constructing Ogg pages
//
const HEADER_BYTES: usize = 27;
const MAX_SEGMENTS: usize = 255;
const SEGMENT_SIZE: usize = 255;

pub struct BitOggStream<W: Write> {
    writer: W,
    bit_buffer: u8,
    bits_stored: u8,
    page_buffer: Vec<u8>,
    payload_bytes: usize,
    first: bool,
    continued: bool,
    granule: u32,
    seqno: u32,
}

impl<W: Write> BitOggStream<W> {
    pub fn new(writer: W) -> Self {
        let capacity = HEADER_BYTES + MAX_SEGMENTS + SEGMENT_SIZE * MAX_SEGMENTS;
        Self {
            writer,
            bit_buffer: 0,
            bits_stored: 0,
            page_buffer: vec![0u8; capacity],
            payload_bytes: 0,
            first: true,
            continued: false,
            granule: 0,
            seqno: 0,
        }
    }

    pub fn put_bit(&mut self, bit: bool) -> Result<()> {
        if bit {
            self.bit_buffer |= 1 << self.bits_stored;
        }
        self.bits_stored += 1;
        if self.bits_stored == 8 {
            self.flush_bits()?;
        }
        Ok(())
    }

    pub fn flush_bits(&mut self) -> Result<()> {
        if self.bits_stored != 0 {
            if self.payload_bytes == SEGMENT_SIZE * MAX_SEGMENTS {
                self.flush_page_internal(true, false);
                return Err(ParseError::Message("ran out of space in an Ogg packet".into()));
            }
            let pos = HEADER_BYTES + MAX_SEGMENTS + self.payload_bytes;
            if pos >= self.page_buffer.len() {
                return Err(ParseError::Message("page buffer overflow".into()));
            }
            self.page_buffer[pos] = self.bit_buffer;
            self.payload_bytes += 1;
            self.bits_stored = 0;
            self.bit_buffer = 0;
        }
        Ok(())
    }

    pub fn set_granule(&mut self, g: u32) {
        self.granule = g;
    }

    /// Flush the current Ogg page.
    /// (Renamed from flush_page to flush_page_internal so the trait implementation can call it.)
    pub fn flush_page_internal(&mut self, next_continued: bool, last: bool) -> Result<()> {
        self.flush_bits()?;
        if self.payload_bytes == 0 {
            return Ok(());
        }
        let mut segments = (self.payload_bytes + SEGMENT_SIZE) / SEGMENT_SIZE;
        if segments > MAX_SEGMENTS + 1 {
            segments = MAX_SEGMENTS;
        }
        for i in 0..self.payload_bytes {
            let src = HEADER_BYTES + MAX_SEGMENTS + i;
            let dst = HEADER_BYTES + segments + i;
            self.page_buffer[dst] = self.page_buffer[src];
        }
        self.page_buffer[0..4].copy_from_slice(b"OggS");
        self.page_buffer[4] = 0; // stream_structure_version
        self.page_buffer[5] = (if self.continued { 1 } else { 0 })
            | (if self.first { 2 } else { 0 })
            | (if last { 4 } else { 0 });
        {
            let mut tmp = [0u8; 4];
            write_32_le(&mut tmp, self.granule);
            self.page_buffer[6..10].copy_from_slice(&tmp);
        }
        self.page_buffer[10..14].fill(0);
        {
            let mut tmp = [0u8; 4];
            write_32_le(&mut tmp, 1); // stream serial number (dummy)
            self.page_buffer[14..18].copy_from_slice(&tmp);
        }
        {
            let mut tmp = [0u8; 4];
            write_32_le(&mut tmp, self.seqno);
            self.page_buffer[18..22].copy_from_slice(&tmp);
        }
        {
            let mut tmp = [0u8; 4];
            write_32_le(&mut tmp, 0); // checksum placeholder
            self.page_buffer[22..26].copy_from_slice(&tmp);
        }
        self.page_buffer[26] = segments as u8;
        let mut bytes_left = self.payload_bytes;
        for i in 0..segments {
            let lace = if bytes_left >= SEGMENT_SIZE {
                SEGMENT_SIZE as u8
            } else {
                bytes_left as u8
            };
            self.page_buffer[27 + i] = lace;
            bytes_left = bytes_left.saturating_sub(SEGMENT_SIZE);
        }
        let total = HEADER_BYTES + segments + self.payload_bytes;
        let crc = checksum(&self.page_buffer[0..total], total as i32);
        {
            let mut tmp = [0u8; 4];
            write_32_le(&mut tmp, crc);
            self.page_buffer[22..26].copy_from_slice(&tmp);
        }
        self.writer.write_all(&self.page_buffer[0..(HEADER_BYTES + segments + self.payload_bytes)])?;
        self.seqno += 1;
        self.first = false;
        self.continued = next_continued;
        self.payload_bytes = 0;
        Ok(())
    }
}

impl<W: Write> Drop for BitOggStream<W> {
    fn drop(&mut self) {
        let _ = self.flush_page_internal(false, false);
    }
}

pub fn write_32_le(buf: &mut [u8; 4], mut v: u32) {
    for i in 0..4 {
        buf[i] = (v & 0xFF) as u8;
        v >>= 8;
    }
}
pub fn write_16_le(buf: &mut [u8; 2], mut v: u16) {
    for i in 0..2 {
        buf[i] = (v & 0xFF) as u8;
        v >>= 8;
    }
}

pub trait BitOggStreamT {
    fn write_bits(&mut self, value: u32, bits: u8) -> Result<()>;
    fn write_all(&mut self, buf: &[u8]) -> Result<()>;
    fn flush_page(&mut self, next_continued: bool, last: bool) -> Result<()>;
}

impl<W: Write> BitOggStreamT for BitOggStream<W> {
    fn write_bits(&mut self, value: u32, bits: u8) -> Result<()> {
        if bits % 8 == 0 {
            let byte_count = bits / 8;
            for i in 0..byte_count {
                let pos = HEADER_BYTES + MAX_SEGMENTS + self.payload_bytes;
                if pos >= self.page_buffer.len() {
                    return Err(ParseError::Message("page buffer overflow".into()));
                }
                self.page_buffer[pos] = ((value >> (i * 8)) & 0xFF) as u8;
                self.payload_bytes += 1;
            }
            Ok(())
        } else {
            for i in 0..bits {
                let bit = (value >> i) & 1;
                self.put_bit(bit != 0)?;
            }
            Ok(())
        }
    }
    
    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        for &byte in buf {
            let pos = HEADER_BYTES + MAX_SEGMENTS + self.payload_bytes;
            if pos >= self.page_buffer.len() {
                return Err(ParseError::Message("page buffer overflow".into()));
            }
            self.page_buffer[pos] = byte;
            self.payload_bytes += 1;
        }
        Ok(())
    }
    
    fn flush_page(&mut self, next_continued: bool, last: bool) -> Result<()> {
        self.flush_page_internal(next_continued, last)
    }
}


pub struct BitStream<R: Read> {
    reader: R,
    bit_buffer: u8,
    pub bits_left: u8,
    total_bits_read: u64,
}

impl<R: Read + Seek> BitStream<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            bit_buffer: 0,
            bits_left: 0,
            total_bits_read: 0,
        }
    }

    /// Reads a single bit (LSB-first within the byte).
    pub fn get_bit(&mut self) -> Result<bool> {
        if self.bits_left == 0 {
            let mut buf = [0u8; 1];
            self.reader.read_exact(&mut buf).map_err(|e| {
                if e.kind() == ErrorKind::UnexpectedEof {
                    ParseError::Message("Out of bits".into())
                } else {
                    e.into()
                }
            })?;
            self.bit_buffer = buf[0];
            self.bits_left = 8;
        }
        self.total_bits_read += 1;
        self.bits_left -= 1;
        // Return the bit at position: 0x80 >> bits_left.
        Ok((self.bit_buffer & (0x80 >> self.bits_left)) != 0)
    }

    /// Returns the total number of bits read so far.
    pub fn get_total_bits_read(&self) -> u64 {
        self.total_bits_read
    }

    /// Returns the current byte position in the underlying reader.
    /// If some bits are buffered, it subtracts one byte.
    pub fn get_position(&mut self) -> io::Result<u64> {
        let pos = self.reader.seek(SeekFrom::Current(0))?;
        if self.bits_left < 8 {
            Ok(pos - 1)
        } else {
            Ok(pos)
        }
    }
}


#[derive(Debug)]
pub struct BitUint<const BIT_SIZE: usize> {
    pub total: u32,
}

impl<const BIT_SIZE: usize> BitUint<BIT_SIZE> {
    /// Create a new BitUint from a u32 value.
    /// Returns an error if BIT_SIZE is greater than 32 or if the value doesn't fit.
    pub fn new(v: u32) -> Result<Self> {
        if BIT_SIZE > 32 {
            return Err(ParseError::Message("Too many bits".into()));
        }
        if BIT_SIZE < 32 && v >= (1 << BIT_SIZE) {
            return Err(ParseError::Message("Integer too big".into()));
        }
        Ok(Self { total: v })
    }

    pub fn read_from<R: Read + Seek>(stream: &mut BitStream<R>) -> Result<Self> {
        let mut total = 0;
        for i in 0..BIT_SIZE {
            if stream.get_bit()? {
                total |= 1 << i;
            }
        }
        Self::new(total)
    }

    pub fn write_to<O: crate::bit_stream::BitOggStreamT>(&self, stream: &mut O) -> Result<()> {
        for i in 0..BIT_SIZE {
            let bit = (self.total & (1 << i)) != 0;
            stream.write_bits(if bit { 1 } else { 0 }, 1)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct BitUintV {
    pub size: usize,
    pub total: u32,
}

impl BitUintV {
    pub fn new(size: usize, v: u32) -> Result<Self> {
        if size > 32 {
            return Err(ParseError::Message("Too many bits".into()));
        }
        if v >= (1 << size) {
            return Err(ParseError::Message("Integer too big".into()));
        }
        Ok(Self { size, total: v })
    }

    pub fn read_from<R: Read + Seek>(stream: &mut BitStream<R>, size: usize) -> Result<Self> {
        let mut total = 0;
        for i in 0..size {
            if stream.get_bit()? {
                total |= 1 << i;
            }
        }
        Self::new(size, total)
    }

    /// Writes this BitUintV bit–by–bit to the BitOggStreamT.
    pub fn write_to<O: crate::bit_stream::BitOggStreamT>(&self, stream: &mut O) -> Result<()> {
        for i in 0..self.size {
            let bit = (self.total & (1 << i)) != 0;
            stream.write_bits(if bit { 1 } else { 0 }, 1)?;
        }
        Ok(())
    }
}


/// CRC checksum calculation using a lookup table.
pub fn checksum(data: &[u8], bytes: i32) -> u32 {
    let mut crc_reg: u32 = 0;
    for &b in data.iter().take(bytes as usize) {
        let index = ((crc_reg >> 24) as u8 ^ b) as usize;
        crc_reg = (crc_reg << 8) ^ CRC_LOOKUP[index];
    }
    crc_reg
}

static CRC_LOOKUP: [u32; 256] = [
    0x00000000,0x04c11db7,0x09823b6e,0x0d4326d9,
    0x130476dc,0x17c56b6b,0x1a864db2,0x1e475005,
    0x2608edb8,0x22c9f00f,0x2f8ad6d6,0x2b4bcb61,
    0x350c9b64,0x31cd86d3,0x3c8ea00a,0x384fbdbd,
    0x4c11db70,0x48d0c6c7,0x4593e01e,0x4152fda9,
    0x5f15adac,0x5bd4b01b,0x569796c2,0x52568b75,
    0x6a1936c8,0x6ed82b7f,0x639b0da6,0x675a1011,
    0x791d4014,0x7ddc5da3,0x709f7b7a,0x745e66cd,
    0x9823b6e0,0x9ce2ab57,0x91a18d8e,0x95609039,
    0x8b27c03c,0x8fe6dd8b,0x82a5fb52,0x8664e6e5,
    0xbe2b5b58,0xbaea46ef,0xb7a96036,0xb3687d81,
    0xad2f2d84,0xa9ee3033,0xa4ad16ea,0xa06c0b5d,
    0xd4326d90,0xd0f37027,0xddb056fe,0xd9714b49,
    0xc7361b4c,0xc3f706fb,0xceb42022,0xca753d95,
    0xf23a8028,0xf6fb9d9f,0xfbb8bb46,0xff79a6f1,
    0xe13ef6f4,0xe5ffeb43,0xe8bccd9a,0xec7dd02d,
    0x34867077,0x30476dc0,0x3d044b19,0x39c556ae,
    0x278206ab,0x23431b1c,0x2e003dc5,0x2ac12072,
    0x128e9dcf,0x164f8078,0x1b0ca6a1,0x1fcdbb16,
    0x018aeb13,0x054bf6a4,0x0808d07d,0x0cc9cdca,
    0x7897ab07,0x7c56b6b0,0x71159069,0x75d48dde,
    0x6b93dddb,0x6f52c06c,0x6211e6b5,0x66d0fb02,
    0x5e9f46bf,0x5a5e5b08,0x571d7dd1,0x53dc6066,
    0x4d9b3063,0x495a2dd4,0x44190b0d,0x40d816ba,
    0xaca5c697,0xa864db20,0xa527fdf9,0xa1e6e04e,
    0xbfa1b04b,0xbb60adfc,0xb6238b25,0xb2e29692,
    0x8aad2b2f,0x8e6c3698,0x832f1041,0x87ee0df6,
    0x99a95df3,0x9d684044,0x902b669d,0x94ea7b2a,
    0xe0b41de7,0xe4750050,0xe9362689,0xedf73b3e,
    0xf3b06b3b,0xf771768c,0xfa325055,0xfef34de2,
    0xc6bcf05f,0xc27dede8,0xcf3ecb31,0xcbffd686,
    0xd5b88683,0xd1799b34,0xdc3abded,0xd8fba05a,
    0x690ce0ee,0x6dcdfd59,0x608edb80,0x644fc637,
    0x7a089632,0x7ec98b85,0x738aad5c,0x774bb0eb,
    0x4f040d56,0x4bc510e1,0x46863638,0x42472b8f,
    0x5c007b8a,0x58c1663d,0x558240e4,0x51435d53,
    0x251d3b9e,0x21dc2629,0x2c9f00f0,0x285e1d47,
    0x36194d42,0x32d850f5,0x3f9b762c,0x3b5a6b9b,
    0x0315d626,0x07d4cb91,0x0a97ed48,0x0e56f0ff,
    0x1011a0fa,0x14d0bd4d,0x19939b94,0x1d528623,
    0xf12f560e,0xf5ee4bb9,0xf8ad6d60,0xfc6c70d7,
    0xe22b20d2,0xe6ea3d65,0xeba91bbc,0xef68060b,
    0xd727bbb6,0xd3e6a601,0xdea580d8,0xda649d6f,
    0xc423cd6a,0xc0e2d0dd,0xcda1f604,0xc960ebb3,
    0xbd3e8d7e,0xb9ff90c9,0xb4bcb610,0xb07daba7,
    0xae3afba2,0xaafbe615,0xa7b8c0cc,0xa379dd7b,
    0x9b3660c6,0x9ff77d71,0x92b45ba8,0x9675461f,
    0x8832161a,0x8cf30bad,0x81b02d74,0x857130c3,
    0x5d8a9099,0x594b8d2e,0x5408abf7,0x50c9b640,
    0x4e8ee645,0x4a4ffbf2,0x470cdd2b,0x43cdc09c,
    0x7b827d21,0x7f436096,0x7200464f,0x76c15bf8,
    0x68860bfd,0x6c47164a,0x61043093,0x65c52d24,
    0x119b4be9,0x155a565e,0x18197087,0x1cd86d30,
    0x029f3d35,0x065e2082,0x0b1d065b,0x0fdc1bec,
    0x3793a651,0x3352bbe6,0x3e119d3f,0x3ad08088,
    0x2497d08d,0x2056cd3a,0x2d15ebe3,0x29d4f654,
    0xc5a92679,0xc1683bce,0xcc2b1d17,0xc8ea00a0,
    0xd6ad50a5,0xd26c4d12,0xdf2f6bcb,0xdbee767c,
    0xe3a1cbc1,0xe760d676,0xea23f0af,0xeee2ed18,
    0xf0a5bd1d,0xf464a0aa,0xf9278673,0xfde69bc4,
    0x89b8fd09,0x8d79e0be,0x803ac667,0x84fbdbd0,
    0x9abc8bd5,0x9e7d9662,0x933eb0bb,0x97ffad0c,
    0xafb010b1,0xab710d06,0xa6322bdf,0xa2f33668,
    0xbcb4666d,0xb8757bda,0xb5365d03,0xb1f740b4
];
