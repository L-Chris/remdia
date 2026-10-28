//! ISO MPEG-4 decoder cookie and canonical PCM speaker layout; platform-free tests.
use anyhow::{Result, ensure};

fn descriptor(tag: u8, data: &[u8]) -> Vec<u8> {
    let mut n = data.len();
    let mut length = vec![(n & 0x7f) as u8];
    n >>= 7;
    while n > 0 {
        length.push(((n & 0x7f) as u8) | 0x80);
        n >>= 7;
    }
    length.reverse();
    let mut out = vec![tag];
    out.extend(length);
    out.extend_from_slice(data);
    out
}
pub fn aac_cookie(asc: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        (2..=65536).contains(&asc.len()),
        "invalid AAC AudioSpecificConfig size"
    );
    let mut decoder = vec![0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    decoder.extend(descriptor(5, asc));
    let mut es = vec![0, 1, 0];
    es.extend(descriptor(4, &decoder));
    es.extend(descriptor(6, &[2]));
    Ok(descriptor(3, &es))
}
pub fn pcm_layout(channels: u16) -> Result<[u32; 3]> {
    let bitmap = match channels {
        1 => 4,
        2 => 3,
        3 => 7,
        4 => 0x107,
        5 => 0x37,
        6 => 0x3f,
        _ => anyhow::bail!("unsupported AudioToolbox channel layout"),
    };
    // kAudioChannelLayoutTag_UseChannelBitmap; FL,FR,FC,LFE,BL,BR for 5.1.
    Ok([1 << 16, bitmap, 0])
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aac_cookie_wraps_asc_in_esds_instead_of_passing_raw_asc() {
        let cookie = aac_cookie(&[0x11, 0xb0]).unwrap();
        assert_eq!(cookie.len(), 27);
        assert_eq!(&cookie[..9], &[3, 25, 0, 1, 0, 4, 17, 0x40, 0x15]);
        assert_eq!(&cookie[cookie.len() - 7..], &[5, 2, 0x11, 0xb0, 6, 1, 2]);
        assert!(aac_cookie(&[]).is_err());
        let large = aac_cookie(&vec![0xa5; 256]).unwrap();
        assert_eq!(large[1] & 0x80, 0x80);
        assert!(large.windows(256).any(|v| v == [0xa5; 256]));
    }
    #[test]
    fn surround_output_uses_wave_speaker_order() {
        assert_eq!(pcm_layout(6).unwrap(), [65536, 63, 0]);
        assert_eq!(pcm_layout(2).unwrap(), [65536, 3, 0]);
        assert!(pcm_layout(0).is_err());
    }
}
