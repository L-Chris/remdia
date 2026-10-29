//! System PCM representation conversion; no bundled PCM decoder.
use super::audio_toolbox_sys as sys;
use anyhow::{Context, Result, ensure};
use remdia_graph::*;

pub struct Pcm {
    converter: sys::AudioConverterRef,
    track: Track,
    stride: usize,
    info: NodeInfo,
    pending: Option<MediaBuffer>,
    draining: bool,
}
impl Pcm {
    pub fn new(config: &NodeConfig) -> Result<Self> {
        let track: Track = serde_json::from_value(config.settings["track"].clone())?;
        ensure!(
            (1..=6).contains(&track.channels) && (8000..=192000).contains(&track.sample_rate),
            "invalid PCM parameters"
        );
        let name = track
            .codec
            .strip_prefix("pcm_")
            .context("expected PCM codec")?;
        let (bits, flags) = match name {
            "u8" => (8, 8),
            "s8" => (8, 8 | 4),
            "s16le" => (16, 8 | 4),
            "s16be" => (16, 8 | 4 | 2),
            "s24le" => (24, 8 | 4),
            "s24be" => (24, 8 | 4 | 2),
            "s32le" => (32, 8 | 4),
            "s32be" => (32, 8 | 4 | 2),
            "f32le" => (32, 8 | 1),
            "f32be" => (32, 8 | 1 | 2),
            "f64le" => (64, 8 | 1),
            "f64be" => (64, 8 | 1 | 2),
            _ => anyhow::bail!("unsupported system PCM representation {name}"),
        };
        let stride = bits / 8 * u32::from(track.channels);
        let source = sys::AudioStreamBasicDescription {
            sample_rate: f64::from(track.sample_rate),
            format_id: u32::from_be_bytes(*b"lpcm"),
            format_flags: flags,
            bytes_per_packet: stride,
            frames_per_packet: 1,
            bytes_per_frame: stride,
            channels_per_frame: u32::from(track.channels),
            bits_per_channel: bits,
            reserved: 0,
        };
        let destination = sys::AudioStreamBasicDescription::pcm_float32(
            f64::from(track.sample_rate),
            u32::from(track.channels),
        );
        let mut converter = std::ptr::null_mut();
        let status = unsafe {
            sys::audio_converter_new(
                sys::framework().map_err(anyhow::Error::msg)?,
                &source,
                &destination,
                &mut converter,
            )
        };
        ensure!(
            status == 0 && !converter.is_null(),
            "create system PCM converter: OSStatus {status}"
        );
        let mut output = PortCaps::new(DataKind::Audio);
        output.rate = Some(track.sample_rate);
        output.channels = Some(track.channels);
        output.format = Some("f32le".into());
        Ok(Self {
            converter,
            track,
            stride: stride as usize,
            info: NodeInfo {
                input: Some(PortCaps::new(DataKind::Packet)),
                output,
                max_output_bytes: 16 * 1024 * 1024,
                media: None,
            },
            pending: None,
            draining: false,
        })
    }
}
impl Drop for Pcm {
    fn drop(&mut self) {
        if let Ok(fw) = sys::framework() {
            unsafe {
                sys::audio_converter_dispose(fw, self.converter);
            }
        }
    }
}
impl Component for Pcm {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, input: &MediaBuffer) -> Result<Submit> {
        ensure!(
            !self.draining && input.header.stream_id == self.track.index as u32,
            "invalid PCM input"
        );
        if self.pending.is_some() {
            return Ok(Submit::WouldBlock);
        }
        let FrameInfo::Packet {
            pts,
            time_base,
            trim_start,
            trim_end,
            ..
        } = input.header.info
        else {
            anyhow::bail!("expected PCM packet")
        };
        let bytes = input.bytes();
        ensure!(bytes.len().is_multiple_of(self.stride), "partial PCM frame");
        let frames = bytes.len() / self.stride;
        let length = frames
            .checked_mul(self.track.channels as usize * 4)
            .context("PCM size overflow")?;
        ensure!(
            length <= self.info.max_output_bytes,
            "PCM exceeds output budget"
        );
        let mut output = vec![0u8; length];
        let mut size = u32::try_from(length)?;
        let status = unsafe {
            sys::audio_converter_convert_buffer(
                sys::framework().map_err(anyhow::Error::msg)?,
                self.converter,
                u32::try_from(bytes.len())?,
                bytes.as_ptr().cast(),
                &mut size,
                output.as_mut_ptr().cast(),
            )
        };
        ensure!(
            status == 0 && size as usize == length,
            "system PCM conversion: OSStatus {status}"
        );
        ensure!(
            output
                .as_chunks::<4>()
                .0
                .iter()
                .all(|v| f32::from_le_bytes(*v).is_finite()),
            "non-finite PCM"
        );
        let start = usize::try_from(trim_start)?.min(frames);
        let end = frames.saturating_sub(usize::try_from(trim_end)?);
        if start < end {
            let stride = self.track.channels as usize * 4;
            let mut header = input.header.clone();
            header.info = FrameInfo::Audio {
                pts_us: time_base
                    .rescale(pts, Rational::MICROS)?
                    .checked_add(
                        (start as i128 * 1_000_000 / i128::from(self.track.sample_rate)) as i64,
                    )
                    .context("PCM timestamp overflow")?,
                rate: self.track.sample_rate,
                channels: self.track.channels,
                samples: end - start,
                format: "f32le".into(),
            };
            self.pending = Some(MediaBuffer::new(
                header,
                vec![output[start * stride..end * stride].to_vec()],
            ));
        }
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> Result<Receive> {
        Ok(self.pending.take().map_or(
            if self.draining {
                Receive::End
            } else {
                Receive::NeedInput
            },
            Receive::Output,
        ))
    }
    fn drain(&mut self) -> Result<()> {
        self.draining = true;
        Ok(())
    }
    fn reset(&mut self, _generation: u64, _target: i64) -> Result<()> {
        self.pending = None;
        self.draining = false;
        let status = unsafe {
            sys::audio_converter_reset(
                sys::framework().map_err(anyhow::Error::msg)?,
                self.converter,
            )
        };
        ensure!(status == 0, "reset PCM converter: {status}");
        Ok(())
    }
}
