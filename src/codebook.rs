use std::fs::File;
use std::io::{self, Read, Seek, BufReader, SeekFrom};
use crate::errors::{ParseError, Result};
use crate::bit_stream::{BitStream, BitOggStreamT, BitUint, BitUintV};

/// Compute ilog (number of bits required to represent v)
pub fn ilog(mut v: u32) -> i32 {
    let mut ret = 0;
    while v != 0 {
        ret += 1;
        v >>= 1;
    }
    ret
}

/// Compute quantized values for lookup type 1.
pub fn book_maptype1_quantvals(entries: u32, dimensions: u32) -> u32 {
    // Get a starting hint.
    let bits = ilog(entries);
    let shift = ((bits - 1) * ((dimensions - 1) as i32)) / (dimensions as i32);
    let mut vals = entries >> shift;
    loop {
        let mut acc: u64 = 1;
        let mut acc1: u64 = 1;
        for _ in 0..dimensions {
            acc *= vals as u64;
            acc1 *= (vals + 1) as u64;
        }
        if acc <= entries as u64 && acc1 > entries as u64 {
            return vals;
        } else {
            if acc > entries as u64 {
                vals -= 1;
            } else {
                vals += 1;
            }
        }
    }
}

/// CodebookLibrary holds codebook data loaded from a file.
/// For inline codebooks, codebook_data and codebook_offsets remain None.
pub struct CodebookLibrary {
    codebook_data: Option<Vec<u8>>,
    codebook_offsets: Option<Vec<i64>>,
    codebook_count: i64,
}

impl CodebookLibrary {
    pub fn new_empty() -> Self {
        Self {
            codebook_data: None,
            codebook_offsets: None,
            codebook_count: 0,
        }
    }

    pub fn new_from_file(filename: &str) -> Result<Self> {
        let mut file = File::open(filename)
            .map_err(|_| ParseError::Message(format!("File open error: {}", filename)))?;
        let metadata = file.metadata()?;
        let file_size = metadata.len() as i64;
        if file_size < 4 {
            return Err(ParseError::Message("File too small".into()));
        }
        file.seek(SeekFrom::End(-4))?;
        let offset_offset = {
            let mut buf = [0u8; 4];
            file.read_exact(&mut buf)?;
            u32::from_le_bytes(buf) as i64
        };
        let codebook_count = (file_size - offset_offset) / 4;

        let mut codebook_data = vec![0u8; offset_offset as usize];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut codebook_data)?;

        let mut codebook_offsets = Vec::with_capacity(codebook_count as usize);
        for _ in 0..codebook_count {
            let mut buf = [0u8; 4];
            file.read_exact(&mut buf)?;
            codebook_offsets.push(i32::from_le_bytes(buf) as i64);
        }
        Ok(Self {
            codebook_data: Some(codebook_data),
            codebook_offsets: Some(codebook_offsets),
            codebook_count,
        })
    }

    pub fn get_codebook(&self, i: usize) -> Result<&[u8]> {
        if let (Some(ref data), Some(ref offsets)) = (&self.codebook_data, &self.codebook_offsets) {
            if i >= (self.codebook_count - 1) as usize {
                return Err(ParseError::Message("Invalid codebook index".into()));
            }
            let start = offsets[i] as usize;
            let end = offsets[i+1] as usize;
            Ok(&data[start..end])
        } else {
            Err(ParseError::Message("codebook library not loaded".into()))
        }
    }

    pub fn get_codebook_size(&self, i: usize) -> Result<i64> {
        if let Some(ref offsets) = self.codebook_offsets {
            if i >= (self.codebook_count - 1) as usize {
                return Err(ParseError::Message("Invalid codebook index".into()));
            }
            Ok(offsets[i+1] - offsets[i])
        } else {
            Err(ParseError::Message("codebook library not loaded".into()))
        }
    }

    pub fn rebuild(&self, codebook_id: usize, os: &mut impl BitOggStreamT) -> Result<()> {
        let cb = self.get_codebook(codebook_id)?;
        let cb_size = self.get_codebook_size(codebook_id)?;
        if cb.is_empty() || cb_size == -1 {
            return Err(ParseError::Message("Invalid codebook id".into()));
        }
        use std::io::Cursor;
        let mut cursor = Cursor::new(cb);
        let mut bis = BitStream::new(&mut cursor);
        self.rebuild_from_stream(&mut bis, cb_size as u32, os)
    }

    pub fn rebuild_from_stream<R: Read + Seek>(
        &self,
        bis: &mut BitStream<R>,
        cb_size: u32,
        os: &mut impl BitOggStreamT,
    ) -> Result<()> {
        let dimensions = BitUint::<4>::read_from(bis)?;
        let entries = BitUint::<14>::read_from(bis)?;
        BitUint::<24>::new(0x564342)?.write_to(os)?;
        BitUint::<16>::new(dimensions.total)?.write_to(os)?;
        BitUint::<24>::new(entries.total)?.write_to(os)?;
        
        // Gather codeword lengths.
        let ordered = BitUint::<1>::read_from(bis)?;
        ordered.write_to(os)?;
        if ordered.total != 0 {
            let initial_length = BitUint::<5>::read_from(bis)?;
            initial_length.write_to(os)?;
            let mut current_entry: u32 = 0;
            while current_entry < entries.total {
                let bits = ilog(entries.total - current_entry) as usize;
                let number = BitUintV::read_from(bis, bits)?;
                number.write_to(os)?;
                current_entry += number.total;
            }
            if current_entry > entries.total {
                return Err(ParseError::Message("current_entry out of range".into()));
            }
        } else {
            let codeword_length_length = BitUint::<3>::read_from(bis)?;
            let sparse = BitUint::<1>::read_from(bis)?;
            if codeword_length_length.total == 0 || codeword_length_length.total > 5 {
                return Err(ParseError::Message("nonsense codeword length".into()));
            }
            sparse.write_to(os)?;

            for i in 0..entries.total {
                let mut present_bool = true;
                if sparse.total != 0 {
                    let present = BitUint::<1>::read_from(bis)?;
                    present.write_to(os)?;
                    present_bool = present.total != 0;
                }
                if present_bool{
                    let codeword_length = BitUintV::read_from(bis, codeword_length_length.total as usize)?;
                    BitUint::<5>::new(codeword_length.total)?.write_to(os)?;
                }
            }
        }
        let lookup_type = BitUint::<1>::read_from(bis)?;
        BitUint::<4>::new(lookup_type.total)?.write_to(os)?;
        if lookup_type.total == 0 {
            // nothing
        } else if lookup_type.total == 1 {
            let min = BitUint::<32>::read_from(bis)?;
            let max = BitUint::<32>::read_from(bis)?;
            let value_length = BitUint::<4>::read_from(bis)?;
            let sequence_flag = BitUint::<1>::read_from(bis)?;
            min.write_to(os)?;
            max.write_to(os)?;
            value_length.write_to(os)?;
            sequence_flag.write_to(os)?;
            let quantvals = book_maptype1_quantvals(entries.total, dimensions.total);
            for _ in 0..quantvals {
                let val = BitUintV::read_from(bis, (value_length.total + 1) as usize)?;
                val.write_to(os)?;
            }
        } else if lookup_type.total == 2 {
            return Err(ParseError::Message("didn't expect lookup type 2".into()));
        } else {
            return Err(ParseError::Message("invalid lookup type".into()));
        }
        if cb_size != 0 && (bis.get_total_bits_read() / 8 + 1) != cb_size as u64 {
            return Err(ParseError::Message(format!(
                "Size mismatch: expected {}, got {}",
                cb_size,
                bis.get_total_bits_read() / 8 + 1
            )));
        }
        
        Ok(())
    }

    pub fn copy<R: Read + Seek, O: BitOggStreamT>(&self, bis: &mut BitStream<R>, os: &mut O) -> Result<()> {
        let id = BitUint::<24>::read_from(bis)?;
        let dimensions = BitUint::<16>::read_from(bis)?;
        let entries = BitUint::<24>::read_from(bis)?;
        if id.total != 0x564342 {
            return Err(ParseError::Message("invalid codebook identifier".into()));
        }
        id.write_to(os)?;
        BitUint::<16>::new(dimensions.total)?.write_to(os)?;
        BitUint::<24>::new(entries.total)?.write_to(os)?;

        let ordered = BitUint::<1>::read_from(bis)?;
        ordered.write_to(os)?;
        if ordered.total != 0 {
            let initial_length = BitUint::<5>::read_from(bis)?;
            initial_length.write_to(os)?;
            let mut current_entry: u32 = 0;
            while current_entry < entries.total {
                let bits = ilog(entries.total - current_entry) as usize;
                let number = BitUintV::read_from(bis, bits)?;
                number.write_to(os)?;
                current_entry += number.total;
            }
            if current_entry > entries.total {
                return Err(ParseError::Message("current_entry out of range".into()));
            }
        } else {
            let sparse = BitUint::<1>::read_from(bis)?;
            sparse.write_to(os)?;
            for _ in 0..entries.total {
                let present = if sparse.total != 0 {
                    BitUint::<1>::read_from(bis)?
                } else {
                    BitUint::<1>::new(1)?
                };
                present.write_to(os)?;
                if present.total != 0 {
                    let codeword_length = BitUint::<5>::read_from(bis)?;
                    codeword_length.write_to(os)?;
                }
            }
        }

        let lookup_type = BitUint::<4>::read_from(bis)?;
        lookup_type.write_to(os)?;
        if lookup_type.total == 0 {
            // nothing
        } else if lookup_type.total == 1 {
            let min = BitUint::<32>::read_from(bis)?;
            let max = BitUint::<32>::read_from(bis)?;
            let value_length = BitUint::<4>::read_from(bis)?;
            let sequence_flag = BitUint::<1>::read_from(bis)?;
            min.write_to(os)?;
            max.write_to(os)?;
            value_length.write_to(os)?;
            sequence_flag.write_to(os)?;
            let quantvals = book_maptype1_quantvals(entries.total, dimensions.total);
            for _ in 0..quantvals {
                let val = BitUintV::read_from(bis, (value_length.total + 1) as usize)?;
                val.write_to(os)?;
            }
        } else if lookup_type.total == 2 {
            return Err(ParseError::Message("didn't expect lookup type 2".into()));
        } else {
            return Err(ParseError::Message("invalid lookup type".into()));
        }
        
        Ok(())
    }
}
