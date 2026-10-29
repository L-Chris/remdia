//! Bounded extraction for FLAC artwork that system metadata APIs omit.
//! This reads container metadata only; images are decoded by the image graph.
use anyhow::{Context, Result, ensure};
use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
};
/// Source Reader can omit FLAC codec private data. Preserve the native container's
/// STREAMINFO rather than depending on a particular Windows codec's blob layout.
#[cfg(windows)]
pub fn flac_streaminfo(path: &Path) -> Result<Option<Vec<u8>>> {
    let mut file = std::fs::File::open(path)?;
    let mut header = [0; 8];
    if file.read(&mut header)? != header.len() || &header[..4] != b"fLaC" {
        return Ok(None);
    }
    ensure!(
        header[4] & 127 == 0 && header[5..] == [0, 0, 34],
        "invalid FLAC STREAMINFO block"
    );
    let mut info = vec![0; 34];
    file.read_exact(&mut info)?;
    Ok(Some(info))
}
pub fn flac_artwork(path: &Path, limit: usize) -> Result<Option<Vec<u8>>> {
    let mut file = std::fs::File::open(path)?;
    let mut magic = [0; 4];
    file.read_exact(&mut magic)?;
    if &magic != b"fLaC" {
        return Ok(None);
    }
    let mut scanned = 4usize;
    for _ in 0..4096 {
        let mut header = [0; 4];
        file.read_exact(&mut header)?;
        let length =
            ((header[1] as usize) << 16) | ((header[2] as usize) << 8) | header[3] as usize;
        scanned = scanned
            .checked_add(4 + length)
            .context("FLAC metadata overflow")?;
        ensure!(
            scanned <= 64 * 1024 * 1024,
            "FLAC metadata exceeds scan limit"
        );
        if header[0] & 127 == 6 {
            ensure!(
                length <= limit + 1024 * 1024,
                "FLAC picture metadata exceeds budget"
            );
            let mut data = vec![0; length];
            file.read_exact(&mut data)?;
            let mut rest = &data[..];
            fn integer(rest: &mut &[u8]) -> Result<usize> {
                let (head, tail) = rest.split_at_checked(4).context("truncated FLAC picture")?;
                *rest = tail;
                Ok(u32::from_be_bytes(head.try_into()?) as usize)
            }
            let _kind = integer(&mut rest)?;
            for _ in 0..2 {
                let size = integer(&mut rest)?;
                rest = rest.get(size..).context("truncated FLAC picture string")?;
            }
            for _ in 0..4 {
                integer(&mut rest)?;
            }
            let size = integer(&mut rest)?;
            ensure!(size <= limit, "FLAC artwork exceeds budget");
            return Ok(Some(
                rest.get(..size).context("truncated FLAC artwork")?.to_vec(),
            ));
        }
        file.seek(SeekFrom::Current(i64::try_from(length)?))?;
        if header[0] & 128 != 0 {
            return Ok(None);
        }
    }
    anyhow::bail!("too many FLAC metadata blocks")
}
