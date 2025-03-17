use std::fs::File;
use std::path::Path;
use std::io::{self, Read, Seek, BufReader, BufWriter, SeekFrom, Cursor};
use byteorder::{LittleEndian, BigEndian, ReadBytesExt};
use tracing;

use crate::bit_stream::{BitOggStream, BitOggStreamT, BitUint, BitUintV, BitStream};
use crate::codebook::{ilog};
use crate::errors::{ParseError, Result};



// Helper functions.
pub fn read_16_le<R: Read + ?Sized>(reader: &mut R) -> Result<u16> {
    Ok(reader.read_u16::<LittleEndian>()?)
}
pub fn read_16_be<R: Read + ?Sized>(reader: &mut R) -> Result<u16> {
    Ok(reader.read_u16::<BigEndian>()?)
}
pub fn read_32_le<R: Read + ?Sized>(reader: &mut R) -> Result<u32> {
    Ok(reader.read_u32::<LittleEndian>()?)
}
pub fn read_32_be<R: Read + ?Sized>(reader: &mut R) -> Result<u32> {
    Ok(reader.read_u32::<BigEndian>()?)
}

pub fn read_16_le_dyn(reader: &mut dyn Read) -> Result<u16> {
    read_16_le(reader)
}
pub fn read_16_be_dyn(reader: &mut dyn Read) -> Result<u16> {
    read_16_be(reader)
}
pub fn read_32_le_dyn(reader: &mut dyn Read) -> Result<u32> {
    read_32_le(reader)
}
pub fn read_32_be_dyn(reader: &mut dyn Read) -> Result<u32> {
    read_32_be(reader)
}

#[derive(Debug)]
pub enum ForcePacketFormat {
    NoModPackets,
    ModPackets,
}

// -------------------- Packet (modern 2 or 6 byte header) ---------------------
pub struct Packet {
    offset: i64,
    size: u16,
    absolute_granule: u32,
    no_granule: bool,
}

impl Packet {
    pub fn new<R: Read + Seek>(
        reader: &mut R,
        offset: i64,
        little_endian: bool,
        no_granule: bool,
    ) -> Result<Self> {
        reader.seek(SeekFrom::Start(offset as u64))?;
        let size = if little_endian {
            read_16_le(reader)?
        } else {
            read_16_be(reader)?
        };
        let absolute_granule = if !no_granule {
            if little_endian {
                read_32_le(reader)?
            } else {
                read_32_be(reader)?
            }
        } else { 0 };
        Ok(Self { offset, size, absolute_granule, no_granule })
    }
    pub fn header_size(&self) -> i64 {
        if self.no_granule { 2 } else { 6 }
    }
    pub fn offset(&self) -> i64 {
        self.offset + self.header_size()
    }
    pub fn size(&self) -> u16 {
        self.size
    }
    pub fn granule(&self) -> u32 {
        self.absolute_granule
    }
    pub fn next_offset(&self) -> i64 {
        self.offset + self.header_size() + self.size as i64
    }
}

// -------------------- Packet8 (old 8 byte header) ----------------------------
pub struct Packet8 {
    offset: i64,
    size: u32,
    absolute_granule: u32,
    next_offset: i64,
}

impl Packet8 {
    pub fn new<R: Read + Seek>(reader: &mut R, offset: i64, little_endian: bool) -> Result<Self> {
        reader.seek(SeekFrom::Start(offset as u64))?;
        let size = if little_endian {
            read_32_le(reader)?
        } else {
            read_32_be(reader)?
        };
        let absolute_granule = if little_endian {
            read_32_le(reader)?
        } else {
            read_32_be(reader)?
        };
        Ok(Self {
            offset,
            size,
            absolute_granule,
            next_offset: offset + 8 + size as i64,
        })
    }
    pub fn size(&self) -> u32 { self.size }
    pub fn offset(&self) -> i64 { self.offset + 8 }
    pub fn granule(&self) -> u32 { self.absolute_granule }
    pub fn next_offset(&self) -> i64 { self.next_offset }
    pub fn header_size(&self) -> i64 {8}
}

// -------------------- VorbisPacketHeader -------------------------------------
pub struct VorbisPacketHeader {
    type_: u8,
}

impl VorbisPacketHeader {
    pub fn new(t: u8) -> Self {
        Self { type_: t }
    }
    pub fn write_to<O: BitOggStreamT>(&self, os: &mut O) -> Result<()> {
        os.write_bits(self.type_ as u32, 8)?;
        os.write_all(b"vorbis")?;
        Ok(())
    }
}

// -------------------- WwiseRiffVorbis -----------------------------------------
#[derive(Debug)]
pub struct WwiseRiffVorbis<R: Read + Seek> {
    pub file_name: String,
    pub codebooks_name: String,
    pub infile: BufReader<R>,
    pub file_size: i64,

    pub little_endian: bool,

    pub riff_size: i64,
    pub fmt_offset: i64,
    pub cue_offset: i64,
    pub list_offset: i64,
    pub smpl_offset: i64,
    pub vorb_offset: i64,
    pub data_offset: i64,
    pub fmt_size: i64,
    pub cue_size: i64,
    pub list_size: i64,
    pub smpl_size: i64,
    pub vorb_size: i64,
    pub data_size: i64,

    // RIFF fmt
    pub channels: u16,
    pub sample_rate: u32,
    pub avg_bytes_per_second: u32,

    // RIFF extended fmt
    pub ext_unk: u16,
    pub subtype: u32,

    // cue info
    pub cue_count: u32,

    // smpl info
    pub loop_count: u32,
    pub loop_start: u32,
    pub loop_end: u32,

    // vorbis info
    pub sample_count: u32,
    pub setup_packet_offset: u32,
    pub first_audio_packet_offset: u32,
    pub uid: u32,
    pub blocksize_0_pow: u8,
    pub blocksize_1_pow: u8,

    pub inline_codebooks: bool,
    pub full_setup: bool,
    pub header_triad_present: bool,
    pub old_packet_headers: bool,
    pub no_granule: bool,
    pub mod_packets: bool,

    pub read_16: fn(&mut dyn Read) -> Result<u16>,
    pub read_32: fn(&mut dyn Read) -> Result<u32>,
}

impl WwiseRiffVorbis<File>{
    pub fn new(
        name: &str,
        codebooks_name: &str,
        inline_codebooks: bool,
        full_setup: bool,
        force_packet_format: ForcePacketFormat,
    ) -> Result<Self> {
        let file = File::open(name).map_err(|_| ParseError::File(name.to_string()))?;
        let infile = BufReader::new(file);

        let mut instance = WwiseRiffVorbis {
            file_name: name.to_string(),
            codebooks_name: codebooks_name.to_string(),
            infile,
            file_size: -1,
            little_endian: true,
            riff_size: -1,
            fmt_offset: -1,
            cue_offset: -1,
            list_offset: -1,
            smpl_offset: -1,
            vorb_offset: -1,
            data_offset: -1,
            fmt_size: -1,
            cue_size: -1,
            list_size: -1,
            smpl_size: -1,
            vorb_size: -1,
            data_size: -1,
            channels: 0,
            sample_rate: 0,
            avg_bytes_per_second: 0,
            ext_unk: 0,
            subtype: 0,
            cue_count: 0,
            loop_count: 0,
            loop_start: 0,
            loop_end: 0,
            sample_count: 0,
            setup_packet_offset: 0,
            first_audio_packet_offset: 0,
            uid: 0,
            blocksize_0_pow: 0,
            blocksize_1_pow: 0,
            inline_codebooks,
            full_setup,
            header_triad_present: false,
            old_packet_headers: false,
            no_granule: false,
            mod_packets: false,
            read_16: read_16_le_dyn,
            read_32: read_32_le_dyn,
        };

        instance.file_size = instance.infile.get_ref().metadata()?.len() as i64;
        if instance.file_size < 12 {
            return Err(ParseError::Message("File too small".to_string()));
        }

        let mut riff_head = [0u8; 4];
        instance.infile.seek(SeekFrom::Start(0))?;
        instance.infile.read_exact(&mut riff_head)?;

        if &riff_head == b"RIFX" {
            instance.little_endian = false;
        } else if &riff_head == b"RIFF" {
            instance.little_endian = true;
        } else {
            return Err(ParseError::Message("missing RIFF".to_string()));
        }
        instance.read_16 = if instance.little_endian { read_16_le_dyn } else { read_16_be_dyn };
        instance.read_32 = if instance.little_endian { read_32_le_dyn } else { read_32_be_dyn };

        let read_32_fn = instance.read_32;
        instance.riff_size = read_32_fn(&mut instance.infile)? as i64 + 8;
        if instance.riff_size > instance.file_size {
            return Err(ParseError::Message("RIFF truncated".to_string()));
        }

        let mut wave_head = [0u8; 4];
        instance.infile.read_exact(&mut wave_head)?;
        if &wave_head != b"WAVE" {
            return Err(ParseError::Message("missing WAVE".to_string()));
        }

        let mut chunk_offset = 12;
        while chunk_offset < instance.riff_size {
            instance.infile.seek(SeekFrom::Start(chunk_offset as u64))?;
            if chunk_offset + 8 > instance.riff_size {
                return Err(ParseError::Message("chunk header truncated".to_string()));
            }
            let mut chunk_type = [0u8; 4];
            instance.infile.read_exact(&mut chunk_type)?;
            let chunk_size = read_32_fn(&mut instance.infile)? as i64;

            match &chunk_type {
                b"fmt " => {
                    instance.fmt_offset = chunk_offset + 8;
                    instance.fmt_size = chunk_size;
                },
                b"cue " => {
                    instance.cue_offset = chunk_offset + 8;
                    instance.cue_size = chunk_size;
                },
                b"LIST" => {
                    instance.list_offset = chunk_offset + 8;
                    instance.list_size = chunk_size;
                },
                b"smpl" => {
                    instance.smpl_offset = chunk_offset + 8;
                    instance.smpl_size = chunk_size;
                },
                b"vorb" => {
                    instance.vorb_offset = chunk_offset + 8;
                    instance.vorb_size = chunk_size;
                },
                b"data" => {
                    instance.data_offset = chunk_offset + 8;
                    instance.data_size = chunk_size;
                },
                _ => { }
            }
            chunk_offset += 8 + chunk_size;
        }
        if chunk_offset > instance.riff_size {
            return Err(ParseError::Message("chunk truncated".to_string()));
        }
        if instance.fmt_offset == -1 || instance.data_offset == -1 {
            return Err(ParseError::Message("expected fmt, data chunks".to_string()));
        }

        instance.infile.seek(SeekFrom::Start(instance.fmt_offset as u64))?;
        if read_16_le(&mut instance.infile)? != 0xFFFF {
            return Err(ParseError::Message("bad codec id".to_string()));
        }
        instance.channels = read_16_le(&mut instance.infile)?;
        instance.sample_rate = read_32_le(&mut instance.infile)?;
        instance.avg_bytes_per_second = read_32_le(&mut instance.infile)?;
        if read_16_le(&mut instance.infile)? != 0 {
            return Err(ParseError::Message("bad block align".to_string()));
        }
        if read_16_le(&mut instance.infile)? != 0 {
            return Err(ParseError::Message("expected 0 bps".to_string()));
        }
        let extra_len = read_16_le(&mut instance.infile)?;
        if (instance.fmt_size - 0x12) as u16 != extra_len {
            return Err(ParseError::Message("bad extra fmt length".to_string()));
        }
        if instance.fmt_size - 0x12 >= 2 {
            instance.ext_unk = read_16_le(&mut instance.infile)?;
            if instance.fmt_size - 0x12 >= 6 {
                instance.subtype = read_32_le(&mut instance.infile)?;
            }
        }
        if instance.fmt_size == 0x28 {
            let mut whoknowsbuf = [0u8; 16];
            let whoknowsbuf_check: [u8; 16] =
                [1,0,0,0,0,0,0x10,0,0x80,0,0,0xAA,0,0x38,0x9b,0x71];
            instance.infile.read_exact(&mut whoknowsbuf)?;
            if whoknowsbuf != whoknowsbuf_check {
                return Err(ParseError::Message("expected signature in extra fmt?".to_string()));
            }
        }

        if instance.cue_offset != -1 {
            instance.infile.seek(SeekFrom::Start(instance.cue_offset as u64))?;
            instance.cue_count = read_32_le(&mut instance.infile)?;
        }

        if instance.smpl_offset != -1 {
            instance.infile.seek(SeekFrom::Start((instance.smpl_offset + 0x1C) as u64))?;
            instance.loop_count = read_32_le(&mut instance.infile)?;
            if instance.loop_count != 1 {
                return Err(ParseError::Message("expected one loop".to_string()));
            }
            instance.infile.seek(SeekFrom::Start((instance.smpl_offset + 0x2C) as u64))?;
            instance.loop_start = read_32_le(&mut instance.infile)?;
            instance.loop_end = read_32_le(&mut instance.infile)?;
        }

        if instance.vorb_offset == -1 {
            if instance.fmt_size == 0x42 {
                instance.vorb_offset = instance.fmt_offset + 0x18;
            } else {
                return Err(ParseError::Message("expected vorb chunk".to_string()));
            }
        }
        match instance.vorb_size {
            -1 | 0x28 | 0x2A | 0x2C | 0x32 | 0x34 => {
                instance.infile.seek(SeekFrom::Start((instance.vorb_offset + 0x00) as u64))?;
            },
            _ => return Err(ParseError::Message("bad vorb size".to_string())),
        }
        instance.sample_count = read_32_le(&mut instance.infile)?;

        match instance.vorb_size {
            -1 | 0x2A => {
                instance.no_granule = true;
                instance.infile.seek(SeekFrom::Start((instance.vorb_offset + 0x4) as u64))?;
                let mod_signal = read_32_le(&mut instance.infile)?;
                if mod_signal != 0x4A && mod_signal != 0x4B &&
                   mod_signal != 0x69 && mod_signal != 0x70 {
                    instance.mod_packets = true;
                }
                instance.infile.seek(SeekFrom::Start((instance.vorb_offset + 0x10) as u64))?;
            },
            _ => {
                instance.infile.seek(SeekFrom::Start((instance.vorb_offset + 0x18) as u64))?;
            }
        }

        match force_packet_format {
            ForcePacketFormat::NoModPackets => instance.mod_packets = false,
            ForcePacketFormat::ModPackets => instance.mod_packets = true,
        }

        instance.setup_packet_offset = read_32_le(&mut instance.infile)?;
        instance.first_audio_packet_offset = read_32_le(&mut instance.infile)?;

        match instance.vorb_size {
            -1 | 0x2A => {
                instance.infile.seek(SeekFrom::Start((instance.vorb_offset + 0x24) as u64))?;
            },
            0x32 | 0x34 => {
                instance.infile.seek(SeekFrom::Start((instance.vorb_offset + 0x2C) as u64))?;
            },
            _ => {}
        }

        match instance.vorb_size {
            0x28 | 0x2C => {
                instance.header_triad_present = true;
                instance.old_packet_headers = true;
            },
            -1 | 0x2A | 0x32 | 0x34 => {
                instance.uid = read_32_le(&mut instance.infile)?;
                instance.blocksize_0_pow = instance.infile.read_u8()?;
                instance.blocksize_1_pow = instance.infile.read_u8()?;
            },
            _ => {}
        }

        if instance.loop_count != 0 {
            if instance.loop_end == 0 {
                instance.loop_end = instance.sample_count;
            } else {
                instance.loop_end += 1;
            }
            if instance.loop_start >= instance.sample_count ||
               instance.loop_end > instance.sample_count ||
               instance.loop_start > instance.loop_end {
                return Err(ParseError::Message("loops out of range".to_string()));
            }
        }

        match instance.subtype {
            4 | 3 | 0x33 | 0x37 | 0x3b | 0x3f => { },
            _ => { }
        }

        Ok(instance)
    }
}

impl WwiseRiffVorbis<Cursor<Vec<u8>>>{
    pub fn new(
        buf: Cursor<Vec<u8>>,
        file_name: &str,
        codebooks_name: &str,
        inline_codebooks: bool,
        full_setup: bool,
        force_packet_format: ForcePacketFormat,
    ) -> Result<Self> {
        let mut infile = BufReader::new(buf.clone());
    
        let mut instance = WwiseRiffVorbis {
            // Since there is no file name, we use a placeholder.
            file_name: file_name.to_string(),
            codebooks_name: codebooks_name.to_string(),
            infile,
            file_size: -1,
            little_endian: true,
            riff_size: -1,
            fmt_offset: -1,
            cue_offset: -1,
            list_offset: -1,
            smpl_offset: -1,
            vorb_offset: -1,
            data_offset: -1,
            fmt_size: -1,
            cue_size: -1,
            list_size: -1,
            smpl_size: -1,
            vorb_size: -1,
            data_size: -1,
            channels: 0,
            sample_rate: 0,
            avg_bytes_per_second: 0,
            ext_unk: 0,
            subtype: 0,
            cue_count: 0,
            loop_count: 0,
            loop_start: 0,
            loop_end: 0,
            sample_count: 0,
            setup_packet_offset: 0,
            first_audio_packet_offset: 0,
            uid: 0,
            blocksize_0_pow: 0,
            blocksize_1_pow: 0,
            inline_codebooks,
            full_setup,
            header_triad_present: false,
            old_packet_headers: false,
            no_granule: false,
            mod_packets: false,
            // Start with the little-endian functions by default.
            read_16: read_16_le_dyn,
            read_32: read_32_le_dyn,
        };
    
        // Set the file size from the length of the buffer.
        instance.file_size = buf.get_ref().len() as i64;
        if instance.file_size < 12 {
            return Err(ParseError::Message("Buffer too small".to_string()));
        }
    
        // Read RIFF header.
        let mut riff_head = [0u8; 4];
        instance.infile.seek(SeekFrom::Start(0))?;
        instance.infile.read_exact(&mut riff_head)?;
    
        if &riff_head == b"RIFX" {
            instance.little_endian = false;
        } else if &riff_head == b"RIFF" {
            instance.little_endian = true;
        } else {
            return Err(ParseError::Message("missing RIFF".to_string()));
        }
        instance.read_16 = if instance.little_endian { read_16_le_dyn } else { read_16_be_dyn };
        instance.read_32 = if instance.little_endian { read_32_le_dyn } else { read_32_be_dyn };
    
        let read_32_fn = instance.read_32;
        instance.riff_size = read_32_fn(&mut instance.infile)? as i64 + 8;
        if instance.riff_size > instance.file_size {
            return Err(ParseError::Message("RIFF truncated".to_string()));
        }
    
        // Check the WAVE header.
        let mut wave_head = [0u8; 4];
        instance.infile.read_exact(&mut wave_head)?;
        if &wave_head != b"WAVE" {
            return Err(ParseError::Message("missing WAVE".to_string()));
        }
    
        // Iterate through chunks.
        let mut chunk_offset = 12;
        while chunk_offset < instance.riff_size {
            instance.infile.seek(SeekFrom::Start(chunk_offset as u64))?;
            if chunk_offset + 8 > instance.riff_size {
                return Err(ParseError::Message("chunk header truncated".to_string()));
            }
            let mut chunk_type = [0u8; 4];
            instance.infile.read_exact(&mut chunk_type)?;
            let chunk_size = read_32_fn(&mut instance.infile)? as i64;
    
            match &chunk_type {
                b"fmt " => {
                    instance.fmt_offset = chunk_offset + 8;
                    instance.fmt_size = chunk_size;
                },
                b"cue " => {
                    instance.cue_offset = chunk_offset + 8;
                    instance.cue_size = chunk_size;
                },
                b"LIST" => {
                    instance.list_offset = chunk_offset + 8;
                    instance.list_size = chunk_size;
                },
                b"smpl" => {
                    instance.smpl_offset = chunk_offset + 8;
                    instance.smpl_size = chunk_size;
                },
                b"vorb" => {
                    instance.vorb_offset = chunk_offset + 8;
                    instance.vorb_size = chunk_size;
                },
                b"data" => {
                    instance.data_offset = chunk_offset + 8;
                    instance.data_size = chunk_size;
                },
                _ => { /* ignore unrecognized chunks */ }
            }
            chunk_offset += 8 + chunk_size;
        }
        if chunk_offset > instance.riff_size {
            return Err(ParseError::Message("chunk truncated".to_string()));
        }
        if instance.fmt_offset == -1 || instance.data_offset == -1 {
            return Err(ParseError::Message("expected fmt, data chunks".to_string()));
        }
    
        // Parse the fmt chunk.
        instance.infile.seek(SeekFrom::Start(instance.fmt_offset as u64))?;
        if read_16_le(&mut instance.infile)? != 0xFFFF {
            return Err(ParseError::Message("bad codec id".to_string()));
        }
        instance.channels = read_16_le(&mut instance.infile)?;
        instance.sample_rate = read_32_le(&mut instance.infile)?;
        instance.avg_bytes_per_second = read_32_le(&mut instance.infile)?;
        if read_16_le(&mut instance.infile)? != 0 {
            return Err(ParseError::Message("bad block align".to_string()));
        }
        if read_16_le(&mut instance.infile)? != 0 {
            return Err(ParseError::Message("expected 0 bps".to_string()));
        }
        let extra_len = read_16_le(&mut instance.infile)?;
        if (instance.fmt_size - 0x12) as u16 != extra_len {
            return Err(ParseError::Message("bad extra fmt length".to_string()));
        }
        if instance.fmt_size - 0x12 >= 2 {
            instance.ext_unk = read_16_le(&mut instance.infile)?;
            if instance.fmt_size - 0x12 >= 6 {
                instance.subtype = read_32_le(&mut instance.infile)?;
            }
        }
        if instance.fmt_size == 0x28 {
            let mut whoknowsbuf = [0u8; 16];
            let whoknowsbuf_check: [u8; 16] =
                [1, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38, 0x9b, 0x71];
            instance.infile.read_exact(&mut whoknowsbuf)?;
            if whoknowsbuf != whoknowsbuf_check {
                return Err(ParseError::Message("expected signature in extra fmt?".to_string()));
            }
        }
    
        // Parse cue chunk if present.
        if instance.cue_offset != -1 {
            instance.infile.seek(SeekFrom::Start(instance.cue_offset as u64))?;
            instance.cue_count = read_32_le(&mut instance.infile)?;
        }
    
        // Parse smpl chunk if present.
        if instance.smpl_offset != -1 {
            instance.infile.seek(SeekFrom::Start((instance.smpl_offset + 0x1C) as u64))?;
            instance.loop_count = read_32_le(&mut instance.infile)?;
            if instance.loop_count != 1 {
                return Err(ParseError::Message("expected one loop".to_string()));
            }
            instance.infile.seek(SeekFrom::Start((instance.smpl_offset + 0x2C) as u64))?;
            instance.loop_start = read_32_le(&mut instance.infile)?;
            instance.loop_end = read_32_le(&mut instance.infile)?;
        }
    
        // Handle vorb chunk.
        if instance.vorb_offset == -1 {
            if instance.fmt_size == 0x42 {
                instance.vorb_offset = instance.fmt_offset + 0x18;
            } else {
                return Err(ParseError::Message("expected vorb chunk".to_string()));
            }
        }
        match instance.vorb_size {
            -1 | 0x28 | 0x2A | 0x2C | 0x32 | 0x34 => {
                instance.infile.seek(SeekFrom::Start((instance.vorb_offset + 0x00) as u64))?;
            },
            _ => return Err(ParseError::Message("bad vorb size".to_string())),
        }
        instance.sample_count = read_32_le(&mut instance.infile)?;
    
        match instance.vorb_size {
            -1 | 0x2A => {
                instance.no_granule = true;
                instance.infile.seek(SeekFrom::Start((instance.vorb_offset + 0x4) as u64))?;
                let mod_signal = read_32_le(&mut instance.infile)?;
                if mod_signal != 0x4A && mod_signal != 0x4B &&
                   mod_signal != 0x69 && mod_signal != 0x70 {
                    instance.mod_packets = true;
                }
                instance.infile.seek(SeekFrom::Start((instance.vorb_offset + 0x10) as u64))?;
            },
            _ => {
                instance.infile.seek(SeekFrom::Start((instance.vorb_offset + 0x18) as u64))?;
            }
        }
    
        // Apply forced packet format.
        match force_packet_format {
            ForcePacketFormat::NoModPackets => instance.mod_packets = false,
            ForcePacketFormat::ModPackets => instance.mod_packets = true,
        }
    
        instance.setup_packet_offset = read_32_le(&mut instance.infile)?;
        instance.first_audio_packet_offset = read_32_le(&mut instance.infile)?;
    
        match instance.vorb_size {
            -1 | 0x2A => {
                instance.infile.seek(SeekFrom::Start((instance.vorb_offset + 0x24) as u64))?;
            },
            0x32 | 0x34 => {
                instance.infile.seek(SeekFrom::Start((instance.vorb_offset + 0x2C) as u64))?;
            },
            _ => {}
        }
    
        match instance.vorb_size {
            0x28 | 0x2C => {
                instance.header_triad_present = true;
                instance.old_packet_headers = true;
            },
            -1 | 0x2A | 0x32 | 0x34 => {
                instance.uid = read_32_le(&mut instance.infile)?;
                instance.blocksize_0_pow = instance.infile.read_u8()?;
                instance.blocksize_1_pow = instance.infile.read_u8()?;
            },
            _ => {}
        }
    
        if instance.loop_count != 0 {
            if instance.loop_end == 0 {
                instance.loop_end = instance.sample_count;
            } else {
                instance.loop_end += 1;
            }
            if instance.loop_start >= instance.sample_count ||
               instance.loop_end > instance.sample_count ||
               instance.loop_start > instance.loop_end {
                return Err(ParseError::Message("loops out of range".to_string()));
            }
        }

        match instance.subtype {
            4 | 3 | 0x33 | 0x37 | 0x3b | 0x3f => { },
            _ => { }
        }
    
        Ok(instance)
    }
}


impl<R: Read + Seek> WwiseRiffVorbis<R> {

    pub fn print_info(&self) {
        let waveform = if self.little_endian { "RIFF WAVE" } else { "RIFX WAVE" };
        tracing::trace!("{} {} channel{} {} Hz {} bps", 
            waveform, 
            self.channels, 
            if self.channels != 1 { "s" } else { "" },
            self.sample_rate,
            self.avg_bytes_per_second * 8
        );
        if self.loop_count != 0 {
            tracing::trace!("loop from {} to {}", self.loop_start, self.loop_end);
        }
        if self.old_packet_headers {
            tracing::trace!("8 byte (old) packet headers");
        } else if self.no_granule {
            tracing::trace!("2 byte packet headers, no granule");
        } else {
            tracing::trace!("6 byte packet headers");
        }
        if self.header_triad_present {
            tracing::trace!("Vorbis header triad present");
        }
        if self.full_setup || self.header_triad_present {
            tracing::trace!("full setup header");
        } else {
            tracing::trace!("stripped setup header");
        }
        if self.inline_codebooks || self.header_triad_present {
            tracing::trace!("inline codebooks");
        } else {
            tracing::trace!("external codebooks ({})", self.codebooks_name);
        }
        if self.mod_packets {
            tracing::trace!("modified Vorbis packets");
        } else {
            tracing::trace!("standard Vorbis packets");
        }
    }

    pub fn generate_ogg(&mut self) -> Result<()> {
        let path = Path::new(&self.file_name);
        let ogg_path = path.with_extension("ogg");
        let file = File::create(&ogg_path)?;
        let writer = BufWriter::new(file);
        let mut ogg_stream = BitOggStream::new(writer);

        let mut mode_blockflag = Vec::new();
        let mut prev_blockflag = false;
        let mut mode_bits = 0;
        if self.header_triad_present {
            // (Call generate_ogg_header_with_triad here) 
            //self.generate_ogg_header_with_triad(&mut ogg_stream)?;
            unimplemented!("Have not created this case since our project wont need it yet");
        } else {
            self.generate_ogg_header(&mut ogg_stream, &mut mode_blockflag, &mut mode_bits)?;
        }

        // Audio pages: start at the first audio packet offset.
        let mut offset = self.data_offset + self.first_audio_packet_offset as i64;
        while offset < self.data_offset + self.data_size {
            let (packet_header_size, size, packet_payload_offset, granule, next_offset) =
                if self.old_packet_headers {
                    let audio_packet = Packet8::new(&mut self.infile, offset, self.little_endian)?;
                    (
                        audio_packet.header_size(),
                        audio_packet.size(),
                        audio_packet.offset(),
                        audio_packet.granule(),
                        audio_packet.next_offset(),
                    )
                } else {
                    let audio_packet = Packet::new(&mut self.infile, offset, self.little_endian, self.no_granule)?;
                    (
                        audio_packet.header_size(),
                        audio_packet.size() as u32,
                        audio_packet.offset(),
                        audio_packet.granule(),
                        audio_packet.next_offset(),
                    )
                };

            if offset + packet_header_size > self.data_offset + self.data_size {
                return Err(ParseError::Message("page header truncated".into()));
            }

            offset = packet_payload_offset;
            self.infile.seek(SeekFrom::Start(offset as u64))?;

            if granule == 0xFFFFFFFF {
                ogg_stream.set_granule(1);
            } else {
                ogg_stream.set_granule(granule);
            }
            if self.mod_packets {
                if mode_blockflag.is_empty() {
                    return Err(ParseError::Message("didn't load mode_blockflag".into()));
                }
                // Output one bit for packet type (0 == audio)
                BitUint::<1>::new(0)?.write_to(&mut ogg_stream)?;

                let mut ss = BitStream::new(&mut self.infile);

                let mode_number = BitUintV::read_from(&mut ss, mode_bits as usize)?;
                mode_number.write_to(&mut ogg_stream)?;
                let remainder = BitUintV::read_from(&mut ss, 8 - mode_bits as usize)?;
                // Peek at the next frame’s mode if necessary.

                if mode_blockflag[mode_number.total as usize]{
                    self.infile.seek(SeekFrom::Start(next_offset as u64))?;
                    let mut next_blockflag = false;
                    if next_offset + packet_header_size <= self.data_offset + self.data_size{
                        let audio_packet = Packet::new(&mut self.infile, next_offset, self.little_endian, self.no_granule)?;
                        let next_packet_size = audio_packet.size();
                        if next_packet_size > 0{
                            self.infile.seek(SeekFrom::Start(audio_packet.offset() as u64))?;
                            let mut ss = BitStream::new(&mut self.infile);
                            let next_mode_number = BitUintV::read_from(&mut ss, mode_bits as usize)?;
                            next_blockflag = mode_blockflag[next_mode_number.total as usize];
                        }
                    }

                    BitUint::<1>::new(prev_blockflag as u32)?.write_to(&mut ogg_stream)?;
                    BitUint::<1>::new(if next_blockflag { 1 } else { 0 })?.write_to(&mut ogg_stream)?;
                    self.infile.seek(SeekFrom::Start(offset as u64 + 1))?;
                }
                
                prev_blockflag = mode_blockflag[mode_number.total as usize];
                remainder.write_to(&mut ogg_stream)?;
            } else {
                let byte = self.infile.read_u8()?;
                BitUint::<8>::new(byte as u32)?.write_to(&mut ogg_stream)?;
            }

            // Write remaining bytes of the packet.
            for _ in 1..size {
                let byte = self.infile.read_u8()?;
                BitUint::<8>::new(byte as u32)?.write_to(&mut ogg_stream)?;
            }
            offset = next_offset;
            ogg_stream.flush_page(false, offset == self.data_offset + self.data_size)?;
        }
        if offset > self.data_offset + self.data_size {
            return Err(ParseError::Message("page truncated".into()));
        }

        Ok(())
    }

    pub fn generate_ogg_header<O: BitOggStreamT>(
        &mut self,
        os: &mut O,
        mode_blockflag: &mut Vec<bool>,
        mode_bits: &mut i32,
    ) -> Result<()> {

        // generate identification packet
        {
            let vhead = VorbisPacketHeader::new(1);
            vhead.write_to(os)?;
            os.write_bits(0, 32)?;
            os.write_bits(self.channels as u32, 8)?;
            os.write_bits(self.sample_rate, 32)?;
            os.write_bits(0, 32)?;
            os.write_bits(self.avg_bytes_per_second * 8, 32)?;
            os.write_bits(0, 32)?;
            os.write_bits(self.blocksize_0_pow as u32, 4)?;
            os.write_bits(self.blocksize_1_pow as u32, 4)?;
            os.write_bits(1, 1)?;
            os.flush_page(false, false)?;
        }

        // generate comment packet
        {
            let vhead = VorbisPacketHeader::new(3);
            vhead.write_to(os)?;
            let vendor = format!("converted from Audiokinetic Wwise by wem_converter {}", env!("CARGO_PKG_VERSION"));
            let vendor_size = BitUint::<32>::new(vendor.len() as u32)?;
            os.write_bits(vendor_size.total, 32)?;
            for &b in vendor.as_bytes() {
                let c = BitUint::<8>::new(b as u32)?;
                os.write_bits(c.total, 8)?;
            }
            
            if self.loop_count == 0 {
                let user_comment_count = BitUint::<32>::new(0)?;
                os.write_bits(user_comment_count.total, 32)?;
            } else {
                let user_comment_count = BitUint::<32>::new(2)?;
                os.write_bits(user_comment_count.total, 32)?;
                
                let loop_start_str = format!("LoopStart={}", self.loop_start);
                let loop_end_str   = format!("LoopEnd={}", self.loop_end);
                
                let loop_start_comment_length = BitUint::<32>::new(loop_start_str.len() as u32)?;
                os.write_bits(loop_start_comment_length.total, 32)?;
                for &b in loop_start_str.as_bytes() {
                    let c = BitUint::<8>::new(b as u32)?;
                    os.write_bits(c.total, 8)?;
                }
                
                let loop_end_comment_length = BitUint::<32>::new(loop_end_str.len() as u32)?;
                os.write_bits(loop_end_comment_length.total, 32)?;
                for &b in loop_end_str.as_bytes() {
                    let c = BitUint::<8>::new(b as u32)?;
                    os.write_bits(c.total, 8)?;
                }
            }
            let framing = BitUint::<1>::new(1)?;
            os.write_bits(framing.total, 1)?;
            os.flush_page(false, false)?;
        }

        // generate setup packet
        {
            let vhead = VorbisPacketHeader::new(5);
            vhead.write_to(os)?;
            let setup_packet = Packet::new(
                &mut self.infile,
                self.data_offset + self.setup_packet_offset as i64,
                self.little_endian,
                self.no_granule,
            )?;
            
            self.infile.seek(SeekFrom::Start(setup_packet.offset() as u64))?;
            if setup_packet.granule() != 0 {
                return Err(ParseError::Message("setup packet granule != 0".into()));
            }
            let mut ss = BitStream::new(&mut self.infile);
            
            let codebook_count_less1_val = BitUint::<8>::read_from(&mut ss)?.total;
            let codebook_count_less1 = BitUint::<8>::new(codebook_count_less1_val)?;
            let codebook_count = codebook_count_less1.total + 1;
            os.write_bits(codebook_count_less1.total, 8)?;
            if self.inline_codebooks {
                let mut cbl = crate::codebook::CodebookLibrary::new_empty();
                for _ in 0..(codebook_count as usize) {
                    if self.full_setup {
                        cbl.copy(&mut ss, os)?;
                    } else {
                        cbl.rebuild(0, os)?;
                    }
                }
            } else {
                let cbl = crate::codebook::CodebookLibrary::new_from_file(&self.codebooks_name)?;
                for i in 0..(codebook_count as usize) {
                    let codebook_id = BitUint::<10>::read_from(&mut ss)?;
                    if let Err(e) = cbl.rebuild(codebook_id.total as usize, os) {
                        if codebook_id.total == 0x342 {
                            let codebook_identifier = BitUint::<14>::read_from(&mut ss)?;
                            if codebook_identifier.total == 0x1590 {
                                return Err(ParseError::Message("invalid codebook id 0x342, try --full-setup".into()));
                            }
                        }
                        return Err(e);
                    }
                }
            }
            
            // --- Time Domain Transforms (placeholder) ---
            let time_count_less1 = BitUint::<6>::new(0)?;
            os.write_bits(time_count_less1.total, 6)?;
            let dummy_time_value = BitUint::<16>::new(0)?;
            os.write_bits(dummy_time_value.total, 16)?;
            
            if self.full_setup {
                // For full setup, copy the remaining bits of the setup packet.
                while ss.get_total_bits_read() < (setup_packet.size() as u64 * 8) {
                    let bit = ss.get_bit()?;
                    let bit_val = BitUint::<1>::new(if bit { 1 } else { 0 })?;
                    os.write_bits(bit_val.total, 1)?;
                }
            } else {
                // --- Process floors, residues, mappings, and modes ---
                // Floor count:
                let floor_count_less1 = BitUint::<6>::read_from(&mut ss)?;
                let floor_count = floor_count_less1.total + 1;
                floor_count_less1.write_to(os)?;
                
                // Rebuild floors.
                for _ in 0..(floor_count as usize) {
                    // Floor type is always 1.
                    let floor_type = BitUint::<16>::new(1)?;
                    floor_type.write_to(os)?;
                    
                    let floor1_partitions = BitUint::<5>::read_from(&mut ss)?;
                    floor1_partitions.write_to(os)?;

                    
                    // Allocate storage for partition class list.
                    let mut floor1_partition_class_list = vec![0u32; floor1_partitions.total as usize];
                    let mut maximum_class = 0;
                    for j in 0..(floor1_partitions.total as usize) {
                        let class_val = BitUint::<4>::read_from(&mut ss)?;
                        class_val.write_to(os)?;
                        floor1_partition_class_list[j] = class_val.total;
                        if class_val.total > maximum_class {
                            maximum_class = class_val.total;
                        }
                    }
                    
                    // Allocate dimensions for each class.
                    let mut floor1_class_dimensions_list = vec![0u32; (maximum_class + 1) as usize];
                    for j in 0..=maximum_class {
                        let class_dimensions_less1 = BitUint::<3>::read_from(&mut ss)?;
                        class_dimensions_less1.write_to(os)?;
                        floor1_class_dimensions_list[j as usize] = class_dimensions_less1.total + 1;
                        
                        let class_subclasses = BitUint::<2>::read_from(&mut ss)?;
                        class_subclasses.write_to(os)?;
                        if class_subclasses.total != 0 {
                            let masterbook = BitUint::<8>::read_from(&mut ss)?;
                            masterbook.write_to(os)?;
                            if masterbook.total >= codebook_count {
                                return Err(ParseError::Message("invalid floor1 masterbook".into()));
                            }
                        }
                        for _ in 0..(1 << class_subclasses.total) {
                            let subclass_book_plus1 = BitUint::<8>::read_from(&mut ss)?;
                            subclass_book_plus1.write_to(os)?;
                            let subclass_book = (subclass_book_plus1.total as i32) - 1;
                            if subclass_book >= 0 && (subclass_book as u32) >= codebook_count {
                                return Err(ParseError::Message("invalid floor1 subclass book".into()));
                            }
                        }
                    }
                    
                    let floor1_multiplier_less1 = BitUint::<2>::read_from(&mut ss)?;
                    floor1_multiplier_less1.write_to(os)?;
                    let rangebits = BitUint::<4>::read_from(&mut ss)?;
                    rangebits.write_to(os)?;
                    for i in 0..(floor1_partitions.total as usize) {
                        let current_class_number = floor1_partition_class_list[i];
                        for _ in 0..(floor1_class_dimensions_list[current_class_number as usize]) {
                            let x = BitUintV::read_from(&mut ss, rangebits.total as usize)?;
                            x.write_to(os)?;
                        }
                    }
                }
                // Residue count.
                let residue_count_less1 = BitUint::<6>::read_from(&mut ss)?;
                let residue_count = residue_count_less1.total + 1;
                residue_count_less1.write_to(os)?;
                for i in 0..(residue_count as usize) {
                    let residue_type = BitUint::<2>::read_from(&mut ss)?;
                    BitUint::<16>::new(residue_type.total)?.write_to(os)?;
                    if residue_type.total > 2 {
                        return Err(ParseError::Message("invalid residue type".into()));
                    }
                    let residue_begin = BitUint::<24>::read_from(&mut ss)?;
                    let residue_end = BitUint::<24>::read_from(&mut ss)?;
                    let residue_partition_size_less1 = BitUint::<24>::read_from(&mut ss)?;
                    let residue_classifications_less1 = BitUint::<6>::read_from(&mut ss)?;
                    let residue_classbook = BitUint::<8>::read_from(&mut ss)?;
                    let residue_classifications = residue_classifications_less1.total + 1;
                    residue_begin.write_to(os)?;
                    residue_end.write_to(os)?;
                    residue_partition_size_less1.write_to(os)?;
                    residue_classifications_less1.write_to(os)?;
                    residue_classbook.write_to(os)?;
                    if residue_classbook.total >= codebook_count {
                        return Err(ParseError::Message("invalid residue classbook".into()));
                    }

                    let mut residue_cascade = vec![0u32; residue_classifications as usize];
                    for j in 0..(residue_classifications as usize) {
                        // Read 3 bits for low_bits.
                        let low_bits = BitUint::<3>::read_from(&mut ss)?;
                        low_bits.write_to(os)?;
                        let bitflag = BitUint::<1>::read_from(&mut ss)?;
                        bitflag.write_to(os)?;

                        let high_bits = if bitflag.total != 0 {
                            BitUint::<5>::read_from(&mut ss)?
                        } else {
                            BitUint::<5>::new(0)?
                        };
                        if bitflag.total != 0 {
                            high_bits.write_to(os)?;
                        }
                        
                        residue_cascade[j] = high_bits.total * 8 + low_bits.total;
                    }
                    
                    for j in 0..(residue_classifications as usize) {
                        for k in 0..8 {
                            if residue_cascade[j] & (1 << k) != 0 {
                                let residue_book = BitUint::<8>::read_from(&mut ss)?;
                                residue_book.write_to(os)?;
                                if residue_book.total >= codebook_count {
                                    return Err(ParseError::Message("invalid residue book".into()));
                                }
                            }
                        }
                    }
                }
                    
                // Mapping count.
                let mapping_count_less1 = BitUint::<6>::read_from(&mut ss)?;
                let mapping_count = mapping_count_less1.total + 1;
                mapping_count_less1.write_to(os)?;
                for _ in 0..(mapping_count as usize) {
                    let mapping_type = BitUint::<16>::new(0)?;
                    mapping_type.write_to(os)?;
                    let submaps_flag = BitUint::<1>::read_from(&mut ss)?;
                    submaps_flag.write_to(os)?;
                    let mut submaps = 1;
                    if submaps_flag.total != 0 {
                        let submaps_less1 = BitUint::<4>::read_from(&mut ss)?;
                        submaps = submaps_less1.total + 1;
                        submaps_less1.write_to(os)?;
                    }
                    let square_polar_flag = BitUint::<1>::read_from(&mut ss)?;
                    square_polar_flag.write_to(os)?;
                    if square_polar_flag.total != 0 {
                        let coupling_steps_less1 = BitUint::<8>::read_from(&mut ss)?;
                        let coupling_steps = coupling_steps_less1.total + 1;
                        coupling_steps_less1.write_to(os)?;
                        for _ in 0..(coupling_steps as usize) {
                            let magnitude = BitUintV::read_from(&mut ss, ilog((self.channels - 1) as u32) as usize)?;
                            let angle = BitUintV::read_from(&mut ss, ilog((self.channels - 1) as u32) as usize)?;
                            magnitude.write_to(os)?;
                            angle.write_to(os)?;
                            if angle.total == magnitude.total
                                || magnitude.total >= self.channels as u32
                                || angle.total >= self.channels as u32 {
                                return Err(ParseError::Message("invalid coupling".into()));
                            }
                        }
                    }
                    let mapping_reserved = BitUint::<2>::read_from(&mut ss)?;
                    mapping_reserved.write_to(os)?;
                    if mapping_reserved.total != 0 {
                        return Err(ParseError::Message("mapping reserved field nonzero".into()));
                    }
                    if submaps > 1 {
                        for _ in 0..self.channels {
                            let mapping_mux = BitUint::<4>::read_from(&mut ss)?;
                            mapping_mux.write_to(os)?;
                            if mapping_mux.total >= submaps {
                                return Err(ParseError::Message("mapping_mux >= submaps".into()));
                            }
                        }
                    }
                    for _ in 0..submaps {
                        let time_config = BitUint::<8>::read_from(&mut ss)?;
                        time_config.write_to(os)?;
                        let floor_number = BitUint::<8>::read_from(&mut ss)?;
                        floor_number.write_to(os)?;
                        if floor_number.total >= mapping_count {
                            return Err(ParseError::Message("invalid floor mapping".into()));
                        }
                        let residue_number = BitUint::<8>::read_from(&mut ss)?;
                        residue_number.write_to(os)?;
                        if residue_number.total >= mapping_count {
                            return Err(ParseError::Message("invalid residue mapping".into()));
                        }
                    }
                }
                
                let mode_count_less1 = BitUint::<6>::read_from(&mut ss)?;
                let mode_count = mode_count_less1.total + 1;
                mode_count_less1.write_to(os)?;
                
                *mode_blockflag = Vec::with_capacity(mode_count as usize);
                *mode_bits = ilog(mode_count - 1);
                for _ in 0..(mode_count as usize) {
                    let block_flag = BitUint::<1>::read_from(&mut ss)?;
                    block_flag.write_to(os)?;
                    mode_blockflag.push(block_flag.total != 0);
                    let windowtype = BitUint::<16>::new(0)?;
                    windowtype.write_to(os)?;
                    let transformtype = BitUint::<16>::new(0)?;
                    transformtype.write_to(os)?;
                    let mapping = BitUint::<8>::read_from(&mut ss)?;
                    mapping.write_to(os)?;
                    if mapping.total >= mapping_count {
                        return Err(ParseError::Message("invalid mode mapping".into()));
                    }
                }
                
                let framing = BitUint::<1>::new(1)?;
                framing.write_to(os)?;
            }
            
            os.flush_page(false, false)?;

            if (ss.get_total_bits_read() + 6) / 8 != setup_packet.size() as u64 {
                return Err(ParseError::Message("didn't read exactly setup packet".into()));
            }
            if setup_packet.next_offset() != self.data_offset + self.first_audio_packet_offset as i64 {
                return Err(ParseError::Message("first audio packet doesn't follow setup packet".into()));
            }
        }

        Ok(())
    }
}
