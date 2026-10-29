//! Bounded PCM representation/channel conversion, without sample-rate conversion.
use anyhow::{Result, ensure};
use remdia_graph::*;
#[path = "../../system-audio/src/mix.rs"]
mod mix;
pub struct Convert {
    info: NodeInfo,
    control: Control,
    rate: u32,
    channels: u16,
    pending: Option<MediaBuffer>,
    offset: usize,
    draining: bool,
}
impl Convert {
    pub fn new(config: &NodeConfig, control: Control) -> Result<Self> {
        let rate = u32::try_from(config.settings["rate"].as_u64().unwrap_or(48000))?;
        let channels = u16::try_from(config.settings["channels"].as_u64().unwrap_or(2))?;
        ensure!(
            (8000..=192000).contains(&rate) && (1..=2).contains(&channels),
            "invalid PCM output"
        );
        let mut output = PortCaps::new(DataKind::Audio);
        output.rate = Some(rate);
        output.channels = Some(channels);
        output.format = Some("f32le".into());
        Ok(Self {
            info: NodeInfo {
                input: Some(PortCaps::new(DataKind::Audio)),
                output,
                max_output_bytes: 1024 * channels as usize * 4,
                media: None,
            },
            control,
            rate,
            channels,
            pending: None,
            offset: 0,
            draining: false,
        })
    }
}
impl Component for Convert {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, input: &MediaBuffer) -> Result<Submit> {
        self.control.check()?;
        ensure!(!self.draining, "PCM converter is draining");
        if self.pending.is_some() {
            return Ok(Submit::WouldBlock);
        }
        let FrameInfo::Audio {
            rate,
            channels,
            format,
            ..
        } = &input.header.info
        else {
            anyhow::bail!("expected PCM");
        };
        ensure!(
            *rate == self.rate && format == "f32le" && (1..=6).contains(channels),
            "PCM conversion does not resample"
        );
        input.validate()?;
        self.pending = Some(input.clone());
        self.offset = 0;
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> Result<Receive> {
        self.control.check()?;
        let Some(input) = &self.pending else {
            return Ok(if self.draining {
                Receive::End
            } else {
                Receive::NeedInput
            });
        };
        let FrameInfo::Audio {
            channels,
            samples,
            pts_us,
            ..
        } = &input.header.info
        else {
            unreachable!()
        };
        let count = (samples - self.offset).min(1024);
        let matrix = mix::matrix(*channels, self.channels)?;
        let mut bytes = Vec::with_capacity(count * self.channels as usize * 4);
        for frame in self.offset..self.offset + count {
            for out in 0..self.channels as usize {
                let mut value = 0.0;
                for channel in 0..*channels as usize {
                    let start = (frame * *channels as usize + channel) * 4;
                    let sample = f32::from_le_bytes(input.bytes()[start..start + 4].try_into()?);
                    ensure!(sample.is_finite(), "non-finite PCM");
                    value += sample * matrix[channel * self.channels as usize + out];
                }
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        let mut header = input.header.clone();
        header.info = FrameInfo::Audio {
            pts_us: pts_us + (self.offset as i64 * 1_000_000 / i64::from(self.rate)),
            rate: self.rate,
            channels: self.channels,
            samples: count,
            format: "f32le".into(),
        };
        self.offset += count;
        if self.offset == *samples {
            self.pending = None;
        }
        Ok(Receive::Output(MediaBuffer::new(header, vec![bytes])))
    }
    fn drain(&mut self) -> Result<()> {
        self.draining = true;
        Ok(())
    }
    fn reset(&mut self, _: u64, _: i64) -> Result<()> {
        self.pending = None;
        self.offset = 0;
        self.draining = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    unsafe extern "C" fn active(_: *mut std::ffi::c_void) -> u8 {
        0
    }
    #[test]
    fn pcm_conversion_preserves_timeline_backpressure_and_shared_input() {
        let control = Control(remdia_plugin_abi::Control {
            context: std::ptr::null_mut(),
            cancelled: active,
        });
        let mut converter = Convert::new(
            &NodeConfig {
                component: "android.audio.convert".into(),
                settings: serde_json::json!({"rate":48000,"channels":1}),
            },
            control,
        )
        .unwrap();
        let input = MediaBuffer::new(
            BufferHeader {
                generation: 7,
                stream_id: 2,
                info: FrameInfo::Audio {
                    pts_us: 500000,
                    rate: 48000,
                    channels: 2,
                    samples: 1500,
                    format: "f32le".into(),
                },
            },
            vec![
                (0..1500)
                    .flat_map(|_| [0.75f32, 0.25].into_iter().flat_map(f32::to_le_bytes))
                    .collect(),
            ],
        );
        assert_eq!(converter.submit(&input).unwrap(), Submit::Accepted);
        assert_eq!(converter.submit(&input).unwrap(), Submit::WouldBlock);
        converter.drain().unwrap();
        let mut samples = 0;
        while let Receive::Output(buffer) = converter.receive().unwrap() {
            assert_eq!(buffer.header.generation, 7);
            assert_eq!(buffer.header.stream_id, 2);
            let FrameInfo::Audio {
                pts_us,
                samples: count,
                ..
            } = buffer.header.info
            else {
                panic!()
            };
            assert_eq!(pts_us, 500000 + samples * 1_000_000 / 48000);
            for sample in buffer.bytes().as_chunks::<4>().0 {
                assert_eq!(f32::from_le_bytes(*sample), 0.5);
            }
            samples += count as i64;
        }
        assert_eq!(samples, 1500);
        assert_eq!(
            f32::from_le_bytes(input.bytes()[..4].try_into().unwrap()),
            0.75
        );
        converter.reset(8, 0).unwrap();
        assert!(matches!(converter.receive().unwrap(), Receive::NeedInput));
    }
}
