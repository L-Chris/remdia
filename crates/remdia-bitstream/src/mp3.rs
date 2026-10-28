use anyhow::{Context, Result, ensure};
use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
};
#[derive(Debug)]
struct Header {
    bytes: usize,
    samples: u32,
    rate: u32,
    side: usize,
}
fn header(data: &[u8]) -> Result<Header> {
    ensure!(
        data.len() >= 4 && data[0] == 255 && data[1] & 0xe6 == 0xe2,
        "invalid MPEG layer III header"
    );
    let version = (data[1] >> 3) & 3;
    ensure!(version != 1, "reserved MPEG version");
    let index = (data[2] >> 4) as usize;
    ensure!(
        (1..15).contains(&index),
        "unsupported free/reserved MP3 bitrate"
    );
    let rate_index = ((data[2] >> 2) & 3) as usize;
    ensure!(rate_index < 3, "reserved MPEG sample rate");
    let mpeg1 = version == 3;
    let rate = [44100u32, 48000, 32000][rate_index]
        / match version {
            3 => 1,
            2 => 2,
            _ => 4,
        };
    let kbps = if mpeg1 {
        [
            0u32, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ][index]
    } else {
        [
            0u32, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160,
        ][index]
    };
    let bytes = ((if mpeg1 { 144 } else { 72 }) * kbps * 1000 / rate
        + u32::from((data[2] >> 1) & 1)) as usize;
    let mono = data[3] & 0xc0 == 0xc0;
    let side = 4
        + if data[1] & 1 == 0 { 2 } else { 0 }
        + if mpeg1 {
            if mono { 17 } else { 32 }
        } else if mono {
            9
        } else {
            17
        };
    Ok(Header {
        bytes,
        samples: if mpeg1 { 1152 } else { 576 },
        rate,
        side,
    })
}
pub fn packet_samples(data: &[u8], rate: u32) -> Result<usize> {
    let mut rest = data;
    let mut samples = 0usize;
    while !rest.is_empty() {
        let head = header(rest)?;
        ensure!(
            head.rate == rate && head.bytes <= rest.len(),
            "truncated or changing MP3 access unit"
        );
        samples = samples
            .checked_add(head.samples as usize)
            .context("MP3 duration overflow")?;
        rest = &rest[head.bytes..];
    }
    ensure!(samples > 0, "empty MP3 packet");
    Ok(samples)
}
pub fn metadata_frame_bytes(data: &[u8]) -> Result<usize> {
    let head = header(data)?;
    Ok(
        if matches!(data.get(head.side..head.side + 4), Some(b"Xing" | b"Info")) {
            ensure!(head.bytes <= data.len(), "truncated Xing frame");
            head.bytes
        } else {
            0
        },
    )
}
#[derive(Clone, Copy, Debug)]
pub struct Gapless {
    pub delay: u32,
    pub padding: u32,
    pub valid_samples: u64,
}
pub fn gapless(path: &Path) -> Result<Option<Gapless>> {
    let mut file = std::fs::File::open(path)?;
    let mut prefix = [0u8; 10];
    let size = file.read(&mut prefix)?;
    if size == 10 && prefix.starts_with(b"ID3") {
        ensure!(
            prefix[6..10].iter().all(|b| b & 128 == 0),
            "invalid ID3 size"
        );
        let size = prefix[6..10]
            .iter()
            .fold(0u64, |n, b| (n << 7) | u64::from(*b));
        file.seek(SeekFrom::Start(
            10 + size + if prefix[5] & 0x10 != 0 { 10 } else { 0 },
        ))?;
    } else {
        file.seek(SeekFrom::Start(0))?;
    }
    let mut data = [0u8; 4096];
    let size = file.read(&mut data)?;
    let data = &data[..size];
    let Ok(head) = header(data) else {
        return Ok(None);
    };
    if !matches!(data.get(head.side..head.side + 4), Some(b"Xing" | b"Info")) {
        return Ok(None);
    }
    let mut rest = data.get(head.side + 4..).context("truncated Xing header")?;
    fn number(data: &mut &[u8]) -> Result<u32> {
        let (head, tail) = data.split_at_checked(4).context("truncated Xing field")?;
        *data = tail;
        Ok(u32::from_be_bytes(head.try_into()?))
    }
    let flags = number(&mut rest)?;
    let mut count = 0;
    for bit in 0..4 {
        if flags & (1 << bit) != 0 {
            if bit == 2 {
                rest = rest.get(100..).context("truncated Xing TOC")?;
            } else {
                let value = number(&mut rest)?;
                if bit == 0 {
                    count = value;
                }
            }
        }
    }
    if rest.len() < 24
        || ![b"LAME", b"Lavc", b"Lavf"]
            .iter()
            .any(|id| rest.starts_with(*id))
    {
        return Ok(None);
    }
    let delay = (u32::from(rest[21]) << 4) | u32::from(rest[22] >> 4);
    let padding = (u32::from(rest[22] & 15) << 8) | u32::from(rest[23]);
    let total = u64::from(count) * u64::from(head.samples);
    let trim = u64::from(delay) + u64::from(padding);
    ensure!(total >= trim, "invalid MP3 gapless duration");
    Ok(Some(Gapless {
        delay: delay + 529,
        padding: padding.saturating_sub(529),
        valid_samples: total - trim,
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packet_count_handles_aggregate_samples_and_rejects_partial_frames() {
        let mut packet = vec![0u8; 417 * 2];
        packet[..4].copy_from_slice(&[255, 251, 144, 0]);
        packet[417..421].copy_from_slice(&[255, 251, 144, 0]);
        assert_eq!(packet_samples(&packet, 44100).unwrap(), 2304);
        assert!(packet_samples(&packet[..833], 44100).is_err());
        assert!(packet_samples(&packet, 48000).is_err());
    }
    #[test]
    fn synthetic_gapless_tone_has_exact_playable_sample_count() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/audio.mp3");
        let info = gapless(&path).unwrap().unwrap();
        assert_eq!(info.valid_samples, 132300);
        assert_eq!(info.delay, 1105);
        assert_eq!(info.padding, 227);
    }
}
