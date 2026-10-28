//! Streaming CoreAudio PCM conversion. EOF flushes the system SRC before ending.
use super::{apple_config, audio_toolbox_sys as sys};
use anyhow::{Context, Result, ensure};
use remdia_graph::*;
use std::{collections::VecDeque, ffi::c_void};

struct Chunk {
    buffer: MediaBuffer,
    offset: usize,
}
struct Input {
    queue: VecDeque<Chunk>,
    held: Option<MediaBuffer>,
    channels: u16,
    draining: bool,
    padding: Vec<u8>,
    padding_frames: usize,
}
unsafe extern "C" fn supply(
    _: sys::AudioConverterRef,
    count: *mut u32,
    data: *mut sys::AudioBufferList1,
    descriptions: *mut *mut sys::AudioStreamPacketDescription,
    context: *mut c_void,
) -> i32 {
    unsafe {
        let input = &mut *context.cast::<Input>();
        input.held = None;
        if !descriptions.is_null() {
            *descriptions = std::ptr::null_mut()
        }
        let Some(chunk) = input.queue.front_mut() else {
            if input.draining && input.padding_frames > 0 {
                let frames = (*count as usize).min(input.padding_frames);
                input.padding_frames -= frames;
                *count = frames as u32;
                (*data).number_buffers = 1;
                (*data).buffers[0] = sys::AudioBuffer {
                    number_channels: u32::from(input.channels),
                    data_byte_size: (frames * input.channels as usize * 4) as u32,
                    data: input.padding.as_mut_ptr(),
                };
                return 0;
            }
            *count = 0;
            (*data).buffers[0].data_byte_size = 0;
            (*data).buffers[0].data = std::ptr::null_mut();
            return if input.draining { 0 } else { 1 };
        };
        let stride = input.channels as usize * 4;
        let total = chunk.buffer.bytes().len() / stride;
        let frames = (*count as usize).min(total - chunk.offset);
        let start = chunk.offset * stride;
        *count = frames as u32;
        (*data).number_buffers = 1;
        (*data).buffers[0] = sys::AudioBuffer {
            number_channels: u32::from(input.channels),
            data_byte_size: (frames * stride) as u32,
            data: chunk.buffer.bytes().as_ptr().add(start).cast_mut(),
        };
        chunk.offset += frames;
        if chunk.offset == total {
            input.held = input.queue.pop_front().map(|chunk| chunk.buffer)
        }
        0
    }
}
fn status(code: i32, operation: &str) -> Result<()> {
    ensure!(code == 0, "CoreAudio {operation}: OSStatus {code}");
    Ok(())
}
struct Segment {
    converter: sys::AudioConverterRef,
    input: Input,
    header: BufferHeader,
    rate: u32,
    anchor: i64,
    submitted: u64,
    emitted: u64,
    flushing: bool,
}
impl Drop for Segment {
    fn drop(&mut self) {
        if let Ok(fw) = sys::framework() {
            unsafe {
                sys::audio_converter_dispose(fw, self.converter);
            }
        }
    }
}
pub struct Resample {
    info: NodeInfo,
    control: Control,
    rate: u32,
    channels: u16,
    segment: Option<Segment>,
    draining: bool,
}
impl Resample {
    pub fn new(config: &NodeConfig, control: Control) -> Result<Self> {
        let rate = u32::try_from(config.settings["rate"].as_u64().unwrap_or(48000))?;
        let channels = u16::try_from(config.settings["channels"].as_u64().unwrap_or(2))?;
        ensure!(
            (8000..=192000).contains(&rate) && (1..=2).contains(&channels),
            "invalid CoreAudio resampling output"
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
            draining: false,
        })
    }
    fn start(&mut self, buffer: &MediaBuffer, rate: u32, channels: u16, pts: i64) -> Result<()> {
        let fw = sys::framework().map_err(anyhow::Error::msg)?;
        let source =
            sys::AudioStreamBasicDescription::pcm_float32(f64::from(rate), u32::from(channels));
        let destination = sys::AudioStreamBasicDescription::pcm_float32(
            f64::from(self.rate),
            u32::from(self.channels),
        );
        let mut converter = std::ptr::null_mut();
        status(
            unsafe { sys::audio_converter_new(fw, &source, &destination, &mut converter) },
            "create resampler",
        )?;
        ensure!(!converter.is_null(), "missing resampler");
        let mut segment = Segment {
            converter,
            input: Input {
                queue: VecDeque::new(),
                held: None,
                channels,
                draining: false,
                padding: Vec::new(),
                padding_frames: 0,
            },
            header: buffer.header.clone(),
            rate,
            anchor: pts,
            submitted: 0,
            emitted: 0,
            flushing: false,
        };
        for (property, channels) in [
            (u32::from_be_bytes(*b"icl "), channels),
            (u32::from_be_bytes(*b"ocl "), self.channels),
        ] {
            let layout = apple_config::pcm_layout(channels)?;
            status(
                unsafe {
                    sys::audio_converter_set_property(
                        fw,
                        converter,
                        property,
                        std::mem::size_of_val(&layout) as u32,
                        layout.as_ptr().cast(),
                    )
                },
                "set resampler layout",
            )?;
        }
        if rate != self.rate {
            let quality = 127u32;
            status(
                unsafe {
                    sys::audio_converter_set_property(
                        fw,
                        converter,
                        u32::from_be_bytes(*b"srcq"),
                        4,
                        (&quality as *const u32).cast(),
                    )
                },
                "set SRC quality",
            )?;
            let mut prime = [0u32; 2];
            let mut size = 8u32;
            status(
                unsafe {
                    sys::audio_converter_get_property(
                        fw,
                        converter,
                        u32::from_be_bytes(*b"prim"),
                        &mut size,
                        prime.as_mut_ptr().cast(),
                    )
                },
                "query SRC padding",
            )?;
            ensure!(size == 8 && prime[1] <= 8192, "unbounded SRC padding");
            segment.input.padding_frames = prime[1] as usize + 1;
            segment.input.padding = vec![0; segment.input.padding_frames * channels as usize * 4];
        }
        self.segment = Some(segment);
        Ok(())
    }
}
impl Component for Resample {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, buffer: &MediaBuffer) -> Result<Submit> {
        self.control.check()?;
        ensure!(!self.draining, "resampler input after drain");
        buffer.validate()?;
        let FrameInfo::Audio {
            pts_us,
            rate,
            channels,
            samples,
            ..
        } = buffer.header.info
        else {
            anyhow::bail!("expected PCM")
        };
        ensure!(
            (8000..=192000).contains(&rate)
                && (1..=6).contains(&channels)
                && samples <= 4 * 1024 * 1024,
            "invalid system resampler input"
        );
        if samples == 0 {
            return Ok(Submit::Accepted);
        }
        if let Some(segment) = &mut self.segment {
            let expected = i128::from(segment.anchor)
                + i128::from(segment.submitted) * 1_000_000 / i128::from(segment.rate);
            if segment.rate != rate
                || segment.input.channels != channels
                || segment.header.stream_id != buffer.header.stream_id
                || (i128::from(pts_us) - expected).abs() > 20_000
            {
                segment.flushing = true;
                segment.input.draining = true;
                return Ok(Submit::WouldBlock);
            }
            if segment.input.queue.len() >= 2 {
                return Ok(Submit::WouldBlock);
            }
        } else {
            self.start(buffer, rate, channels, pts_us)?;
        }
        let segment = self.segment.as_mut().unwrap();
        segment.submitted = segment
            .submitted
            .checked_add(samples as u64)
            .context("resampling sample count overflow")?;
        segment.input.queue.push_back(Chunk {
            buffer: buffer.clone(),
            offset: 0,
        });
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> Result<Receive> {
        self.control.check()?;
        let Some(segment) = &mut self.segment else {
            return Ok(if self.draining {
                Receive::End
            } else {
                Receive::NeedInput
            });
        };
        let expected = (u128::from(segment.submitted) * u128::from(self.rate))
            .div_ceil(u128::from(segment.rate));
        let remaining = u64::try_from(expected)?.saturating_sub(segment.emitted);
        if remaining == 0 {
            if segment.flushing {
                self.segment = None;
                return Ok(if self.draining {
                    Receive::End
                } else {
                    Receive::NeedInput
                });
            }
            return Ok(Receive::NeedInput);
        }
        let capacity = remaining.min(1024) as u32;
        let mut frames = capacity;
        let mut bytes = vec![0u8; capacity as usize * self.channels as usize * 4];
        let mut output = sys::AudioBufferList1 {
            number_buffers: 1,
            buffers: [sys::AudioBuffer {
                number_channels: u32::from(self.channels),
                data_byte_size: bytes.len() as u32,
                data: bytes.as_mut_ptr(),
            }],
        };
        let code = unsafe {
            sys::audio_converter_fill_complex_buffer(
                sys::framework().map_err(anyhow::Error::msg)?,
                segment.converter,
                supply,
                (&mut segment.input as *mut Input).cast(),
                &mut frames,
                &mut output,
                std::ptr::null_mut(),
            )
        };
        ensure!(
            code == 0 || code == 1,
            "CoreAudio resampling failed: OSStatus {code}"
        );
        ensure!(
            frames <= capacity
                && output.buffers[0].data_byte_size as usize
                    == frames as usize * self.channels as usize * 4,
            "invalid system SRC output"
        );
        if frames == 0 {
            ensure!(
                !segment.flushing,
                "system resampler ended before the admitted sample count"
            );
            return Ok(Receive::NeedInput);
        }
        bytes.truncate(frames as usize * self.channels as usize * 4);
        ensure!(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| f32::from_le_bytes(*p).is_finite()),
            "non-finite system SRC output"
        );
        let pts = i64::try_from(
            i128::from(segment.anchor)
                + i128::from(segment.emitted) * 1_000_000 / i128::from(self.rate),
        )?;
        segment.emitted += u64::from(frames);
        let mut header = segment.header.clone();
        header.info = FrameInfo::Audio {
            pts_us: pts,
            rate: self.rate,
            channels: self.channels,
            samples: frames as usize,
            format: "f32le".into(),
        };
        Ok(Receive::Output(MediaBuffer::new(header, vec![bytes])))
    }
    fn drain(&mut self) -> Result<()> {
        self.draining = true;
        if let Some(segment) = &mut self.segment {
            segment.flushing = true;
            segment.input.draining = true;
        }
        Ok(())
    }
    fn reset(&mut self, _generation: u64, _target: i64) -> Result<()> {
        self.segment = None;
        self.draining = false;
        Ok(())
    }
}
