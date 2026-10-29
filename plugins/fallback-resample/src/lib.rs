//! Optional standalone SRC. No demuxer, decoder, device I/O or codec dependency.
use anyhow::{Context, Result, ensure};
use remdia_graph::*;
use std::ffi::c_void;
#[path = "../../system-audio/src/mix.rs"]
mod mix;
unsafe extern "C" {
    fn remdia_src_resampler_init(
        channels: u32,
        input: u32,
        output: u32,
        quality: i32,
        error: *mut i32,
    ) -> *mut c_void;
    fn remdia_src_resampler_destroy(state: *mut c_void);
    fn remdia_src_resampler_skip_zeros(state: *mut c_void) -> i32;
    fn remdia_src_resampler_get_input_latency(state: *mut c_void) -> i32;
    fn remdia_src_resampler_process_interleaved_float(
        state: *mut c_void,
        input: *const f32,
        input_len: *mut u32,
        output: *mut f32,
        output_len: *mut u32,
    ) -> i32;
}
struct Segment {
    converter: *mut c_void,
    header: BufferHeader,
    source_rate: u32,
    source_channels: u16,
    submitted: u64,
    emitted: u64,
    anchor: i64,
    flushing: bool,
    padding: u32,
}
impl Drop for Segment {
    fn drop(&mut self) {
        unsafe { remdia_src_resampler_destroy(self.converter) }
    }
}
struct Resample {
    info: NodeInfo,
    control: Control,
    rate: u32,
    channels: u16,
    segment: Option<Segment>,
    pending: Option<MediaBuffer>,
    offset: usize,
    draining: bool,
}
impl Resample {
    fn new(config: &NodeConfig, control: Control) -> Result<Self> {
        let rate = u32::try_from(config.settings["rate"].as_u64().unwrap_or(16000))?;
        let channels = u16::try_from(config.settings["channels"].as_u64().unwrap_or(1))?;
        ensure!(
            (8000..=192000).contains(&rate) && (1..=2).contains(&channels),
            "invalid SRC output"
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
            segment: None,
            pending: None,
            offset: 0,
            draining: false,
        })
    }
    fn flush(&mut self) -> Result<()> {
        if let Some(s) = self.segment.as_mut().filter(|s| !s.flushing) {
            let latency = unsafe { remdia_src_resampler_get_input_latency(s.converter) };
            ensure!((0..=16384).contains(&latency), "unbounded SRC latency");
            s.padding = latency as u32 + 64;
            s.flushing = true;
        }
        Ok(())
    }
}
impl Component for Resample {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, input: &MediaBuffer) -> Result<Submit> {
        self.control.check()?;
        ensure!(!self.draining, "SRC is draining");
        input.validate()?;
        let FrameInfo::Audio {
            pts_us,
            rate,
            channels,
            samples,
            ref format,
        } = input.header.info
        else {
            anyhow::bail!("expected PCM");
        };
        ensure!(
            format == "f32le"
                && (8000..=192000).contains(&rate)
                && (1..=6).contains(&channels)
                && samples <= 4 * 1024 * 1024,
            "invalid SRC input"
        );
        if samples == 0 {
            return Ok(Submit::Accepted);
        }
        if let Some(s) = &self.segment {
            let expected = i128::from(s.anchor)
                + i128::from(s.submitted) * 1_000_000 / i128::from(s.source_rate);
            if rate != s.source_rate
                || channels != s.source_channels
                || input.header.stream_id != s.header.stream_id
                || input.header.generation != s.header.generation
                || (i128::from(pts_us) - expected).abs() > 20_000
            {
                self.flush()?;
                return Ok(Submit::WouldBlock);
            }
        }
        if self.pending.is_some() {
            return Ok(Submit::WouldBlock);
        }
        if self.segment.is_none() {
            let mut error = 0;
            let converter = unsafe {
                remdia_src_resampler_init(u32::from(self.channels), rate, self.rate, 5, &mut error)
            };
            ensure!(
                !converter.is_null() && error == 0,
                "SRC allocation failed: {error}"
            );
            let s = Segment {
                converter,
                header: input.header.clone(),
                source_rate: rate,
                source_channels: channels,
                submitted: 0,
                emitted: 0,
                anchor: pts_us,
                flushing: false,
                padding: 0,
            };
            ensure!(
                unsafe { remdia_src_resampler_skip_zeros(s.converter) } == 0,
                "SRC initialization failed"
            );
            self.segment = Some(s);
        }
        let s = self.segment.as_mut().unwrap();
        s.submitted = s
            .submitted
            .checked_add(samples as u64)
            .context("sample count overflow")?;
        self.pending = Some(input.clone());
        self.offset = 0;
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> Result<Receive> {
        for _ in 0..64 {
            self.control.check()?;
            let Some(s) = &mut self.segment else {
                return Ok(if self.draining {
                    Receive::End
                } else {
                    Receive::NeedInput
                });
            };
            let total = (u128::from(s.submitted) * u128::from(self.rate))
                .div_ceil(u128::from(s.source_rate));
            let remaining = u64::try_from(total)?.saturating_sub(s.emitted);
            if s.flushing && self.pending.is_none() && remaining == 0 {
                self.segment = None;
                continue;
            }
            let count;
            let mut mixed = Vec::new();
            if let Some(input) = &self.pending {
                let FrameInfo::Audio { samples, .. } = input.header.info else {
                    unreachable!()
                };
                count = (samples - self.offset).min(1024);
                let matrix = mix::matrix(s.source_channels, self.channels)?;
                mixed.reserve(count * self.channels as usize);
                for frame in self.offset..self.offset + count {
                    for out in 0..self.channels as usize {
                        let mut value = 0.0;
                        for c in 0..s.source_channels as usize {
                            let i = (frame * s.source_channels as usize + c) * 4;
                            let sample = f32::from_le_bytes(input.bytes()[i..i + 4].try_into()?);
                            ensure!(sample.is_finite(), "non-finite PCM");
                            value += sample * matrix[c * self.channels as usize + out];
                        }
                        mixed.push(value);
                    }
                }
            } else if s.flushing {
                ensure!(
                    s.padding > 0 || remaining == 0,
                    "SRC tail did not produce the admitted sample count"
                );
                count = s.padding.min(1024) as usize;
                mixed.resize(count * self.channels as usize, 0.0);
            } else {
                return Ok(Receive::NeedInput);
            }
            let mut input_count = count as u32;
            let mut output_count = remaining.min(1024) as u32;
            let mut output = vec![0f32; output_count as usize * self.channels as usize];
            ensure!(
                unsafe {
                    remdia_src_resampler_process_interleaved_float(
                        s.converter,
                        mixed.as_ptr(),
                        &mut input_count,
                        output.as_mut_ptr(),
                        &mut output_count,
                    )
                } == 0,
                "SRC processing failed"
            );
            ensure!(
                input_count <= count as u32
                    && output_count as usize * self.channels as usize <= output.len(),
                "invalid SRC result"
            );
            if let Some(input) = &self.pending {
                self.offset += input_count as usize;
                let FrameInfo::Audio { samples, .. } = input.header.info else {
                    unreachable!()
                };
                if self.offset == samples {
                    self.pending = None;
                    self.offset = 0;
                }
            } else {
                s.padding -= input_count;
            }
            if output_count == 0 {
                ensure!(input_count > 0, "SRC made no progress");
                continue;
            }
            output.truncate(output_count as usize * self.channels as usize);
            let mut header = s.header.clone();
            header.info = FrameInfo::Audio {
                pts_us: s.anchor + i64::try_from(s.emitted * 1_000_000 / u64::from(self.rate))?,
                rate: self.rate,
                channels: self.channels,
                samples: output_count as usize,
                format: "f32le".into(),
            };
            s.emitted += u64::from(output_count);
            let bytes = output
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect();
            return Ok(Receive::Output(MediaBuffer::new(header, vec![bytes])));
        }
        Ok(Receive::NeedInput)
    }
    fn drain(&mut self) -> Result<()> {
        self.draining = true;
        self.flush()
    }
    fn reset(&mut self, _: u64, _: i64) -> Result<()> {
        self.segment = None;
        self.pending = None;
        self.offset = 0;
        self.draining = false;
        Ok(())
    }
}
fn descriptors() -> Vec<ComponentDescriptor> {
    vec![ComponentDescriptor {
        id: "fallback.audio.resample".into(),
        role: "filter".into(),
        input: Some(PortCaps::new(DataKind::Audio)),
        output: PortCaps::new(DataKind::Audio),
    }]
}
fn create(config: &NodeConfig, control: Control) -> Result<Box<dyn Component>> {
    ensure!(
        config.component == "fallback.audio.resample",
        "unknown SRC component"
    );
    Ok(Box::new(Resample::new(config, control)?))
}
remdia_graph::export_plugin!(descriptors, create);

#[cfg(test)]
mod tests {
    use super::*;
    unsafe extern "C" fn active(_: *mut c_void) -> u8 {
        0
    }
    fn converter(rate: u32) -> Resample {
        Resample::new(
            &NodeConfig {
                component: "fallback.audio.resample".into(),
                settings: serde_json::json!({"rate":rate,"channels":1}),
            },
            Control(remdia_plugin_abi::Control {
                context: std::ptr::null_mut(),
                cancelled: active,
            }),
        )
        .unwrap()
    }
    fn input(rate: u32, start: usize, count: usize, frequency: f32) -> MediaBuffer {
        MediaBuffer::new(
            BufferHeader {
                generation: 3,
                stream_id: 0,
                info: FrameInfo::Audio {
                    pts_us: start as i64 * 1_000_000 / i64::from(rate),
                    rate,
                    channels: 1,
                    samples: count,
                    format: "f32le".into(),
                },
            },
            vec![
                (start..start + count)
                    .flat_map(|n| {
                        (0.5 * (n as f32 * frequency * std::f32::consts::TAU / rate as f32).sin())
                            .to_le_bytes()
                    })
                    .collect(),
            ],
        )
    }
    fn collect(converter: &mut Resample, output: &mut Vec<f32>) {
        for _ in 0..10000 {
            match converter.receive().unwrap() {
                Receive::Output(buffer) => {
                    let FrameInfo::Audio {
                        pts_us, samples, ..
                    } = buffer.header.info
                    else {
                        panic!()
                    };
                    assert_eq!(
                        pts_us,
                        output.len() as i64 * 1_000_000 / i64::from(converter.rate)
                    );
                    assert!(samples <= 1024);
                    assert_eq!(buffer.header.generation, 3);
                    output.extend(
                        buffer
                            .bytes()
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|b| f32::from_le_bytes(*b)),
                    );
                }
                Receive::NeedInput | Receive::End => return,
                Receive::Reconfigured => panic!(),
            }
        }
        panic!("SRC failed to make progress")
    }
    fn run(rate: u32, target: u32, total: usize, block: usize, frequency: f32) -> Vec<f32> {
        let mut src = converter(target);
        let mut output = Vec::new();
        for start in (0..total).step_by(block) {
            assert_eq!(
                src.submit(&input(rate, start, (total - start).min(block), frequency))
                    .unwrap(),
                Submit::Accepted
            );
            collect(&mut src, &mut output);
        }
        src.drain().unwrap();
        collect(&mut src, &mut output);
        assert!(matches!(src.receive().unwrap(), Receive::End));
        assert_eq!(
            output.len(),
            (total as u128 * u128::from(target)).div_ceil(u128::from(rate)) as usize
        );
        assert!(output.iter().all(|v| v.is_finite()));
        output
    }
    #[test]
    fn streaming_src_flushes_exact_rational_counts_from_one_sample_and_across_blocks() {
        for (rate, target) in [
            (44100, 16000),
            (48000, 16000),
            (16000, 48000),
            (192000, 8000),
        ] {
            for count in [1, 17, 577, 32001] {
                run(rate, target, count, 333, 440.0);
            }
            let first = run(rate, target, 32001, 333, 440.0);
            let second = run(rate, target, 32001, 2048, 440.0);
            assert!(first.iter().zip(second).all(|(a, b)| (a - b).abs() < 1e-5));
        }
    }
    #[test]
    fn downsampling_rejects_out_of_band_energy_and_reset_discards_previous_generation() {
        let samples = run(48000, 16000, 48000, 1024, 18000.0);
        let center = &samples[1000..samples.len() - 1000];
        let rms = (center.iter().map(|v| v * v).sum::<f32>() / center.len() as f32).sqrt();
        assert!(rms < 0.01, "alias RMS {rms}");
        let mut src = converter(16000);
        src.submit(&input(48000, 0, 1000, 440.0)).unwrap();
        src.reset(4, 0).unwrap();
        src.drain().unwrap();
        assert!(matches!(src.receive().unwrap(), Receive::End));
    }
}
