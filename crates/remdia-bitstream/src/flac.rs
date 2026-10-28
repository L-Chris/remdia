//! Native FLAC framing and bounded random access (RFC 9639). No PCM decoding.
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};

const HEADER: usize = 16;
const INDEX_LIMIT: usize = 4096;
const LINEAR_WINDOW: u64 = 65536;

struct CountedFile {
    file: File,
    bytes: u64,
}
impl Read for CountedFile {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let count = self.file.read(bytes)?;
        self.bytes += count as u64;
        Ok(count)
    }
}
impl Seek for CountedFile {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.file.seek(position)
    }
}

#[derive(Clone, Copy, Debug)]
struct Header {
    sample: u64,
    frames: u32,
    length: usize,
    variable: bool,
}
pub struct Packet {
    pub sample: u64,
    pub frames: u32,
    pub bytes: Vec<u8>,
}
pub struct Reader {
    file: BufReader<CountedFile>,
    pub streaminfo: [u8; 34],
    pub rate: u32,
    pub channels: u16,
    pub bits: u8,
    pub samples: u64,
    pub metadata: BTreeMap<String, String>,
    max_block: u32,
    length: u64,
    position: u64,
    limit: usize,
    index: BTreeMap<u64, u64>,
    expected: Option<u64>,
    variable: Option<bool>,
    pub frames_read: u64,
}
impl Reader {
    pub fn open(path: &Path, limit: usize, check: &impl Fn() -> Result<()>) -> Result<Self> {
        ensure!(
            (16..=16 * 1024 * 1024).contains(&limit),
            "invalid FLAC packet budget"
        );
        check()?;
        let file = File::open(path)?;
        let length = file.metadata()?.len();
        let mut file = BufReader::with_capacity(65536, CountedFile { file, bytes: 0 });
        let mut magic = [0; 8];
        file.read_exact(&mut magic)?;
        ensure!(
            &magic[..4] == b"fLaC" && magic[4] & 127 == 0 && magic[5..] == [0, 0, 34],
            "invalid FLAC STREAMINFO"
        );
        let mut streaminfo = [0; 34];
        file.read_exact(&mut streaminfo)?;
        let packed = u64::from_be_bytes(streaminfo[10..18].try_into()?);
        let rate = (packed >> 44) as u32;
        let channels = ((packed >> 41) & 7) as u16 + 1;
        let bits = ((packed >> 36) & 31) as u8 + 1;
        let samples = packed & ((1 << 36) - 1);
        let min_block = u16::from_be_bytes(streaminfo[..2].try_into()?) as u32;
        let max_block = u16::from_be_bytes(streaminfo[2..4].try_into()?) as u32;
        ensure!(
            (1..=655350).contains(&rate)
                && (4..=32).contains(&bits)
                && min_block >= 16
                && max_block >= min_block,
            "invalid FLAC stream format"
        );
        let mut index = BTreeMap::new();
        let mut last = magic[4] & 128 != 0;
        let mut seen_table = false;
        let mut metadata = BTreeMap::new();
        for n in 0..4096 {
            if last {
                break;
            }
            ensure!(n < 4095, "too many FLAC metadata blocks");
            check()?;
            let mut header = [0; 4];
            file.read_exact(&mut header)?;
            last = header[0] & 128 != 0;
            ensure!(
                !matches!(header[0] & 127, 0 | 127),
                "invalid FLAC metadata block type"
            );
            let size = u32::from_be_bytes([0, header[1], header[2], header[3]]) as u64;
            let end = file
                .stream_position()?
                .checked_add(size)
                .context("FLAC metadata overflow")?;
            ensure!(
                end <= length && end <= 64 * 1024 * 1024,
                "FLAC metadata exceeds bound"
            );
            if header[0] & 127 == 3 {
                ensure!(!seen_table, "duplicate FLAC SEEKTABLE");
                seen_table = true;
                ensure!(size.is_multiple_of(18), "invalid FLAC SEEKTABLE size");
                let count = size / 18;
                let stride = count.div_ceil(INDEX_LIMIT as u64).max(1);
                let mut previous = None;
                for point in 0..count {
                    if point.is_multiple_of(256) {
                        check()?;
                    }
                    let mut entry = [0; 18];
                    file.read_exact(&mut entry)?;
                    let sample = u64::from_be_bytes(entry[..8].try_into()?);
                    if sample == u64::MAX {
                        continue;
                    }
                    let offset = u64::from_be_bytes(entry[8..16].try_into()?);
                    ensure!(sample != 0 || offset == 0, "invalid FLAC first seek point");
                    ensure!(
                        previous.is_none_or(|(s, o)| sample > s && offset > o),
                        "unordered FLAC SEEKTABLE"
                    );
                    previous = Some((sample, offset));
                    ensure!(
                        samples == 0 || sample < samples,
                        "FLAC seek point outside timeline"
                    );
                    if point.is_multiple_of(stride) && index.len() < INDEX_LIMIT - 1 {
                        index.insert(sample, offset);
                    }
                }
            } else if header[0] & 127 == 4 {
                fn integer(file: &mut impl Read) -> Result<u32> {
                    let mut bytes = [0; 4];
                    file.read_exact(&mut bytes)?;
                    Ok(u32::from_le_bytes(bytes))
                }
                let vendor = u64::from(integer(&mut file)?);
                ensure!(
                    file.stream_position()?.saturating_add(vendor + 4) <= end,
                    "truncated FLAC comments"
                );
                file.seek_relative(vendor as i64)?;
                let count = integer(&mut file)?;
                ensure!(count <= 65536, "too many FLAC comments");
                for n in 0..count {
                    if n.is_multiple_of(256) {
                        check()?;
                    }
                    ensure!(
                        file.stream_position()?.saturating_add(4) <= end,
                        "truncated FLAC comment"
                    );
                    let size = u64::from(integer(&mut file)?);
                    ensure!(
                        file.stream_position()?.saturating_add(size) <= end,
                        "truncated FLAC comment value"
                    );
                    if size <= 8192 {
                        let mut text = vec![0; size as usize];
                        file.read_exact(&mut text)?;
                        if let Ok(text) = std::str::from_utf8(&text)
                            && let Some((key, value)) = text.split_once('=')
                            && !value.is_empty()
                            && let Some(key) = match key.to_ascii_lowercase().as_str() {
                                "title" => Some("title"),
                                "artist" => Some("artist"),
                                "album" => Some("album"),
                                _ => None,
                            }
                        {
                            metadata.entry(key.into()).or_insert_with(|| value.into());
                        }
                    } else {
                        file.seek_relative(size as i64)?;
                    }
                }
                ensure!(
                    file.stream_position()? == end,
                    "invalid FLAC comment length"
                );
            } else {
                file.seek(SeekFrom::Start(end))?;
            }
        }
        let first = file.stream_position()?;
        ensure!(first < length, "FLAC has no audio frames");
        for offset in index.values_mut() {
            *offset = first
                .checked_add(*offset)
                .context("FLAC seek offset overflow")?;
            ensure!(*offset < length, "FLAC seek point outside file");
        }
        index.insert(0, first);
        let mut reader = Self {
            file,
            streaminfo,
            rate,
            channels,
            bits,
            samples,
            metadata,
            max_block,
            length,
            position: first,
            limit,
            index,
            expected: Some(0),
            variable: None,
            frames_read: 0,
        };
        let first_header = reader
            .header()?
            .context("invalid first FLAC frame header")?;
        ensure!(
            first_header.sample == 0,
            "FLAC does not start at sample zero"
        );
        reader.variable = Some(first_header.variable);
        Ok(reader)
    }
    pub fn bytes_read(&self) -> u64 {
        self.file.get_ref().bytes
    }
    pub fn index_entries(&self) -> usize {
        self.index.len()
    }
    fn position(&mut self, position: u64) -> Result<()> {
        // Keep nearby header probes inside the existing buffer. Invalidating
        // it at every 0xff in a compressed payload causes large read amplification.
        if let Ok(delta) = i64::try_from(i128::from(position) - i128::from(self.position)) {
            self.file.seek_relative(delta)?;
        } else {
            self.file.seek(SeekFrom::Start(position))?;
        }
        self.position = position;
        Ok(())
    }
    fn byte(&mut self) -> Result<Option<u8>> {
        let mut byte = [0];
        if self.file.read(&mut byte)? == 0 {
            return Ok(None);
        }
        self.position += 1;
        Ok(Some(byte[0]))
    }
    fn header(&mut self) -> Result<Option<Header>> {
        let mut bytes = [0; HEADER];
        let mut count = 0;
        while count < HEADER {
            let n = self.file.read(&mut bytes[count..])?;
            if n == 0 {
                break;
            }
            count += n;
        }
        self.file.seek_relative(-(count as i64))?;
        Ok(parse_header(&bytes[..count], self))
    }
    pub fn next(&mut self, check: &impl Fn() -> Result<()>) -> Result<Option<Packet>> {
        check()?;
        if self.position == self.length {
            ensure!(
                self.samples == 0 || self.expected == Some(self.samples),
                "truncated FLAC timeline"
            );
            return Ok(None);
        }
        let header = self.header()?.context("invalid FLAC frame header")?;
        ensure!(
            self.expected.is_none_or(|sample| header.sample == sample),
            "discontinuous FLAC frames"
        );
        let next = header
            .sample
            .checked_add(u64::from(header.frames))
            .context("FLAC sample overflow")?;
        ensure!(
            self.samples == 0 || next <= self.samples,
            "FLAC frame outside timeline"
        );
        let mut bytes = Vec::with_capacity(4096.min(self.limit));
        let mut crc = 0;
        loop {
            if bytes.len().is_multiple_of(4096) {
                check()?;
            }
            let Some(byte) = self.byte()? else {
                break;
            };
            if byte == 255 && bytes.len() >= header.length + 2 && crc == 0 {
                self.file.seek_relative(-1)?;
                self.position -= 1;
                if self.header()?.is_some_and(|h| h.sample == next) {
                    break;
                }
                self.byte()?;
            }
            ensure!(bytes.len() < self.limit, "FLAC frame exceeds packet budget");
            bytes.push(byte);
            crc = crc16(crc, byte);
        }
        ensure!(
            bytes.len() >= header.length + 2 && crc == 0,
            "invalid FLAC frame CRC16"
        );
        self.expected = Some(next);
        self.frames_read += 1;
        Ok(Some(Packet {
            sample: header.sample,
            frames: header.frames,
            bytes,
        }))
    }
    fn find(
        &mut self,
        position: u64,
        check: &impl Fn() -> Result<()>,
    ) -> Result<Option<(u64, Header)>> {
        self.position(position)?;
        let bound = self.length.min(position.saturating_add(self.limit as u64));
        let mut candidates = 0;
        while self.position < bound {
            if self.position.is_multiple_of(4096) {
                check()?;
            }
            if self.byte()? != Some(255) {
                continue;
            }
            let start = self.position - 1;
            self.position(start)?;
            if let Some(header) = self.header()? {
                candidates += 1;
                ensure!(candidates <= 64, "too many FLAC sync candidates");
                self.expected = None;
                // Header CRC8 alone can match compressed payload. A complete
                // CRC16-valid frame and its successor establish a seek point.
                match self.next(check) {
                    Ok(Some(_)) => return Ok(Some((start, header))),
                    Err(error) if error.downcast_ref::<std::io::Error>().is_some() => {
                        return Err(error);
                    }
                    _ => {}
                }
                check()?;
            }
            self.position(start + 1)?;
        }
        Ok(None)
    }
    pub fn seek(&mut self, sample: u64, check: &impl Fn() -> Result<()>) -> Result<()> {
        check()?;
        let sample = if self.samples > 0 {
            sample.min(self.samples - 1)
        } else {
            sample
        };
        let (&lower_sample, &lower) = self.index.range(..=sample).next_back().unwrap();
        let mut best = (lower_sample, lower);
        // Validate even container-provided offsets before trusting their sample numbers.
        self.position(lower)?;
        ensure!(
            self.header()?.is_some_and(|h| h.sample == lower_sample),
            "invalid FLAC seek point"
        );
        let mut low = lower;
        let mut high = self
            .index
            .range(sample.saturating_add(1)..)
            .next()
            .map_or(self.length, |(_, &offset)| offset);
        while high.saturating_sub(low) > LINEAR_WINDOW {
            check()?;
            let middle = low + (high - low) / 2;
            match self.find(middle, check)? {
                Some((offset, header)) if header.sample <= sample => {
                    best = (header.sample, offset);
                    if self.index.len() < INDEX_LIMIT {
                        self.index.insert(header.sample, offset);
                    }
                    if sample < header.sample + u64::from(header.frames) {
                        break;
                    }
                    low = self.position;
                }
                _ => high = middle,
            }
        }
        self.position(best.1)?;
        self.expected = Some(best.0);
        loop {
            check()?;
            let offset = self.position;
            let header = self.header()?.context("missing FLAC target frame")?;
            if sample < header.sample + u64::from(header.frames) {
                self.position(offset)?;
                self.expected = Some(header.sample);
                if self.index.len() < INDEX_LIMIT {
                    self.index.insert(header.sample, offset);
                }
                return Ok(());
            }
            self.next(check)?.context("seek outside FLAC audio")?;
        }
    }
}

fn parse_header(data: &[u8], stream: &Reader) -> Option<Header> {
    if data.len() < 6 || data[0] != 255 || data[1] & 0xfe != 0xf8 || data[3] & 1 != 0 {
        return None;
    }
    let assignment = data[3] >> 4;
    let channels = match assignment {
        0..=7 => u16::from(assignment) + 1,
        8..=10 => 2,
        _ => return None,
    };
    let bits = match (data[3] >> 1) & 7 {
        0 => stream.bits,
        1 => 8,
        2 => 12,
        4 => 16,
        5 => 20,
        6 => 24,
        7 => 32,
        _ => return None,
    };
    if channels != stream.channels || bits != stream.bits {
        return None;
    }
    let mut i = 4;
    let first = *data.get(i)?;
    i += 1;
    let count = first.leading_ones() as usize;
    let mut number = if count == 0 {
        u64::from(first)
    } else {
        if !(2..=7).contains(&count) {
            return None;
        }
        u64::from(first & (127 >> count))
    };
    for _ in 1..count {
        let byte = *data.get(i)?;
        i += 1;
        if byte & 0xc0 != 0x80 {
            return None;
        }
        number = (number << 6) | u64::from(byte & 63);
    }
    if count > 0
        && number
            < (if count == 2 {
                128
            } else {
                1 << (5 * count - 4)
            })
    {
        return None;
    }
    let mut byte = || {
        let value = data.get(i).copied();
        i += 1;
        value
    };
    let frames = match data[2] >> 4 {
        1 => 192,
        n @ 2..=5 => 576 << (n - 2),
        6 => u32::from(byte()?) + 1,
        7 => u32::from(u16::from_be_bytes([byte()?, byte()?])) + 1,
        n @ 8..=15 => 256 << (n - 8),
        _ => return None,
    };
    let rate = match data[2] & 15 {
        0 => stream.rate,
        n @ 1..=11 => [
            88200, 176400, 192000, 8000, 16000, 22050, 24000, 32000, 44100, 48000, 96000,
        ][n as usize - 1],
        12 => u32::from(byte()?) * 1000,
        n @ 13..=14 => {
            u32::from(u16::from_be_bytes([byte()?, byte()?])) * if n == 14 { 10 } else { 1 }
        }
        _ => return None,
    };
    if frames > stream.max_block || rate != stream.rate || crc8(data.get(..=i)?) != 0 {
        return None;
    }
    let variable = data[1] & 1 != 0;
    if stream.variable.is_some_and(|expected| expected != variable) {
        return None;
    }
    if number >= (1u64 << if variable { 36 } else { 31 }) {
        return None;
    }
    let sample = if variable {
        number
    } else {
        number.checked_mul(u64::from(stream.max_block))?
    };
    Some(Header {
        sample,
        frames,
        length: i + 1,
        variable,
    })
}
fn crc8(bytes: &[u8]) -> u8 {
    let mut crc = 0u8;
    for byte in bytes {
        crc ^= byte;
        for _ in 0..8 {
            crc = (crc << 1) ^ if crc & 128 != 0 { 7 } else { 0 };
        }
    }
    crc
}
const CRC16: [u16; 256] = {
    let mut table = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut value = (i as u16) << 8;
        let mut bit = 0;
        while bit < 8 {
            value = (value << 1) ^ if value & 0x8000 != 0 { 0x8005 } else { 0 };
            bit += 1;
        }
        table[i] = value;
        i += 1;
    }
    table
};
fn crc16(crc: u16, byte: u8) -> u16 {
    (crc << 8) ^ CRC16[((crc >> 8) as u8 ^ byte) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }
    #[test]
    fn real_flac_frames_seek_before_read_and_after_eof() -> Result<()> {
        for (name, total, block) in [
            ("audio-tail.flac", 176400, 1024),
            ("cover.flac", 132300, 4096),
        ] {
            let mut reader = Reader::open(&fixture(name), 16 * 1024 * 1024, &|| Ok(()))?;
            assert_eq!(reader.samples, total);
            if name == "audio-tail.flac" {
                assert_eq!(reader.metadata["title"], "QQ adapter test");
            }
            for target in [total - 10, 0, total / 2, total - 1, 0] {
                reader.seek(target, &|| Ok(()))?;
                let first = reader.next(&|| Ok(()))?.unwrap();
                assert_eq!(first.sample, target / block * block);
                assert_eq!(first.bytes[..2], [255, 248]);
                let mut end = first.sample + u64::from(first.frames);
                while let Some(packet) = reader.next(&|| Ok(()))? {
                    assert_eq!(packet.sample, end);
                    end += u64::from(packet.frames);
                }
                assert_eq!(end, total);
            }
        }
        Ok(())
    }
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new(label: &str, bytes: &[u8]) -> Self {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target")
                .join(format!("flac-{label}-{}.flac", std::process::id()));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    #[test]
    fn corruption_truncation_and_packet_budget_are_errors() -> Result<()> {
        let source = std::fs::read(fixture("audio-tail.flac"))?;
        let mut bytes = source.clone();
        bytes[8325] ^= 1;
        let bad = Temp::new("corrupt", &bytes);
        assert!(
            Reader::open(&bad.0, 16 * 1024 * 1024, &|| Ok(()))?
                .next(&|| Ok(()))
                .is_err()
        );
        let truncated = Temp::new("truncated", &source[..source.len() - 1]);
        let mut reader = Reader::open(&truncated.0, 16 * 1024 * 1024, &|| Ok(()))?;
        loop {
            match reader.next(&|| Ok(())) {
                Err(_) => break,
                Ok(Some(_)) => {}
                Ok(None) => panic!("accepted truncated frame"),
            }
        }
        assert!(
            Reader::open(&fixture("audio-tail.flac"), 128, &|| Ok(()))?
                .next(&|| Ok(()))
                .is_err()
        );
        Ok(())
    }
    #[test]
    fn seek_table_offsets_and_cancellation_are_checked() -> Result<()> {
        let source = std::fs::read(fixture("audio-tail.flac"))?;
        // Insert a legal SEEKTABLE after STREAMINFO, pointing sample 1024 at
        // the first frame (sample zero). Reject rather than trusting the table.
        let mut bytes = source[..42].to_vec();
        bytes.extend_from_slice(&[3, 0, 0, 18]);
        bytes.extend_from_slice(&1024u64.to_be_bytes());
        bytes.extend_from_slice(&0u64.to_be_bytes());
        bytes.extend_from_slice(&1024u16.to_be_bytes());
        bytes.extend_from_slice(&source[42..]);
        let bad = Temp::new("seek-table", &bytes);
        assert!(
            Reader::open(&bad.0, 16 * 1024 * 1024, &|| Ok(()))?
                .seek(1024, &|| Ok(()))
                .is_err()
        );
        let mut reader = Reader::open(&fixture("audio-tail.flac"), 16 * 1024 * 1024, &|| Ok(()))?;
        assert!(reader.seek(100000, &|| anyhow::bail!("cancelled")).is_err());
        let polls = std::cell::Cell::new(0);
        assert!(
            reader
                .seek(100000, &|| {
                    polls.set(polls.get() + 1);
                    ensure!(polls.get() < 10, "cancelled during frame scan");
                    Ok(())
                })
                .is_err()
        );
        reader.seek(100000, &|| Ok(()))?;
        assert_eq!(reader.next(&|| Ok(()))?.unwrap().sample, 99328);
        Ok(())
    }
}
