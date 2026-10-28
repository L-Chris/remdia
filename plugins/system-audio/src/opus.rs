use anyhow::{Context, Result, ensure};
pub fn packet_frames(packet: &[u8], rate: u32) -> Result<u32> {
    let toc = *packet.first().context("empty Opus packet")?;
    let config = toc >> 3;
    let per_frame = if config < 12 {
        [480u32, 960, 1920, 2880][(config % 4) as usize]
    } else if config < 16 {
        [480, 960][(config % 2) as usize]
    } else {
        [120, 240, 480, 960][(config % 4) as usize]
    };
    let frames = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => u32::from(*packet.get(1).context("truncated Opus packet")? & 63),
    };
    ensure!(
        frames > 0 && per_frame * frames <= 5760,
        "invalid Opus packet duration"
    );
    Ok(per_frame * frames * rate / 48000)
}
