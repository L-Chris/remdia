//! AudioToolbox AAC/Dolby adapter. System symbols are runtime-loaded by the bridge.
use super::audio_toolbox_sys::{
    self as sys, AudioBuffer, AudioBufferList1, AudioConverterRef, AudioStreamBasicDescription,
    AudioStreamPacketDescription,
};
use anyhow::{Context, Result, ensure};
use remdia_graph::*;
use std::{collections::VecDeque, ffi::c_void};

const NO_DATA_NOW: i32 = 1;
struct Input {
    queue: VecDeque<Vec<u8>>,
    held: Vec<Vec<u8>>,
    description: AudioStreamPacketDescription,
    draining: bool,
}
unsafe extern "C" fn supply(
    _converter: AudioConverterRef,
    count: *mut u32,
    data: *mut AudioBufferList1,
    descriptions: *mut *mut AudioStreamPacketDescription,
    user: *mut c_void,
) -> i32 {
    // AudioConverter invokes this serially during FillComplexBuffer, with the
    // documented non-null callback pointers. Held allocations outlive the call.
    unsafe {
        let input = &mut *user.cast::<Input>();
        // CoreAudio may retain the previous buffer until this next callback,
        // even when a FillComplexBuffer call has already returned.
        input.held.clear();
        let Some(packet) = input.queue.pop_front() else {
            *count = 0;
            (*data).buffers[0].data_byte_size = 0;
            (*data).buffers[0].data = std::ptr::null_mut();
            if !descriptions.is_null() {
                *descriptions = std::ptr::null_mut();
            }
            return if input.draining { 0 } else { NO_DATA_NOW };
        };
        input.description = AudioStreamPacketDescription {
            start_offset: 0,
            variable_frames_in_packet: 0,
            data_byte_size: packet.len() as u32,
        };
        *count = 1;
        (*data).number_buffers = 1;
        (*data).buffers[0] = AudioBuffer {
            number_channels: 0,
            data_byte_size: packet.len() as u32,
            data: packet.as_ptr().cast_mut(),
        };
        if !descriptions.is_null() {
            *descriptions = &mut input.description;
        }
        input.held.push(packet);
        0
    }
}
fn status(value: i32, operation: &str) -> Result<()> {
    ensure!(
        value == 0,
        "AudioToolbox {operation} failed: OSStatus {value}"
    );
    Ok(())
}
pub struct Decoder {
    converter: AudioConverterRef,
    track: Track,
    info: NodeInfo,
    input: Input,
    stamps: VecDeque<BufferHeader>,
    frames: u32,
    ended: bool,
}
impl Decoder {
    pub fn new(config: &NodeConfig) -> Result<Self> {
        use symphonia::core::codecs::audio::well_known::{CODEC_ID_AC3, CODEC_ID_EAC3};
        let track: Track = serde_json::from_value(config.settings["track"].clone())?;
        ensure!(
            track.kind == "audio"
                && (1..=6).contains(&track.channels)
                && (8000..=192000).contains(&track.sample_rate),
            "invalid AudioToolbox stream parameters"
        );
        let fw = sys::framework().map_err(anyhow::Error::msg)?;
        let codec = track.initialization["codec_id"]
            .as_str()
            .unwrap_or_default();
        let (format, frames, aac) = if codec == CODEC_ID_AC3.to_string() {
            (u32::from_be_bytes(*b"ac-3"), 1536, false)
        } else if codec == CODEC_ID_EAC3.to_string() {
            (u32::from_be_bytes(*b"ec-3"), 1536, false)
        } else {
            (sys::K_AUDIO_FORMAT_MPEG4_AAC, 1024, true)
        };
        let mut source = AudioStreamBasicDescription::mpeg4_aac(
            f64::from(track.sample_rate),
            u32::from(track.channels),
        );
        source.format_id = format;
        source.frames_per_packet = frames;
        if !aac {
            source.format_flags = 0;
        }
        let output = AudioStreamBasicDescription::pcm_float32(
            f64::from(track.sample_rate),
            u32::from(track.channels),
        );
        let mut converter = std::ptr::null_mut();
        status(
            unsafe { sys::audio_converter_new(fw, &source, &output, &mut converter) },
            "create audio decoder",
        )?;
        ensure!(!converter.is_null(), "AudioToolbox returned no converter");
        let mut caps = PortCaps::new(DataKind::Audio);
        caps.format = Some("f32le".into());
        caps.channels = Some(track.channels);
        caps.rate = Some(track.sample_rate);
        let decoder = Self {
            converter,
            track,
            info: NodeInfo {
                input: Some(PortCaps::new(DataKind::Packet)),
                output: caps,
                max_output_bytes: 1024 * 1024,
                media: None,
            },
            input: Input {
                queue: VecDeque::new(),
                held: Vec::new(),
                description: AudioStreamPacketDescription::default(),
                draining: false,
            },
            stamps: VecDeque::new(),
            frames,
            ended: false,
        };
        // Own the converter before setting properties so every error disposes it.
        if aac {
            let asc: Vec<u8> =
                serde_json::from_value(decoder.track.initialization["extra_data"].clone())?;
            let cookie = super::apple_config::aac_cookie(&asc)?;
            status(
                unsafe {
                    sys::audio_converter_set_property(
                        fw,
                        converter,
                        sys::K_AUDIO_CONVERTER_DECOMPRESSION_MAGIC_COOKIE,
                        cookie.len() as u32,
                        cookie.as_ptr().cast(),
                    )
                },
                "set AAC decoder cookie",
            )?;
        }
        let layout = super::apple_config::pcm_layout(decoder.track.channels)?;
        status(
            unsafe {
                sys::audio_converter_set_property(
                    fw,
                    converter,
                    // kAudioConverterOutputChannelLayout (AudioConverter.h).
                    u32::from_be_bytes(*b"ocl "),
                    std::mem::size_of_val(&layout) as u32,
                    layout.as_ptr().cast(),
                )
            },
            "set PCM speaker layout",
        )?;
        Ok(decoder)
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        if let Ok(fw) = sys::framework() {
            unsafe {
                sys::audio_converter_dispose(fw, self.converter);
            }
        }
    }
}
impl Component for Decoder {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, input: &MediaBuffer) -> Result<Submit> {
        ensure!(
            !self.input.draining && input.header.stream_id == self.track.index as u32,
            "invalid AudioToolbox decoder input"
        );
        ensure!(
            matches!(input.header.info, FrameInfo::Packet { .. }),
            "AudioToolbox requires a compressed packet"
        );
        ensure!(
            input.bytes().len() <= 16 * 1024 * 1024,
            "AudioToolbox packet exceeds budget"
        );
        if self.input.queue.len() >= 2 {
            return Ok(Submit::WouldBlock);
        }
        ensure!(
            self.stamps.len() < 32,
            "AudioToolbox decoder made no output progress"
        );
        self.input.queue.push_back(input.bytes().to_vec());
        self.stamps.push_back(input.header.clone());
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> Result<Receive> {
        if self.ended {
            return Ok(Receive::End);
        }
        let fw = sys::framework().map_err(anyhow::Error::msg)?;
        let mut bytes = vec![0u8; self.frames as usize * usize::from(self.track.channels) * 4];
        let mut output = AudioBufferList1 {
            number_buffers: 1,
            buffers: [AudioBuffer {
                number_channels: u32::from(self.track.channels),
                data_byte_size: bytes.len() as u32,
                data: bytes.as_mut_ptr(),
            }],
        };
        let mut frames = self.frames;
        let result = unsafe {
            sys::audio_converter_fill_complex_buffer(
                fw,
                self.converter,
                supply,
                (&mut self.input as *mut Input).cast(),
                &mut frames,
                &mut output,
                std::ptr::null_mut(),
            )
        };
        ensure!(
            result == 0 || result == NO_DATA_NOW,
            "AudioToolbox decode failed: OSStatus {result}"
        );
        let length = output.buffers[0].data_byte_size as usize;
        ensure!(
            length <= bytes.len() && frames <= self.frames,
            "AudioToolbox output exceeds buffer"
        );
        if frames == 0 || length == 0 {
            if self.input.draining && self.input.queue.is_empty() {
                self.ended = true;
                return Ok(Receive::End);
            }
            return Ok(Receive::NeedInput);
        }
        ensure!(
            length == frames as usize * usize::from(self.track.channels) * 4,
            "invalid AudioToolbox PCM length"
        );
        let Some(header) = self.stamps.pop_front() else {
            // A system decoder may return a trailing priming block beyond the
            // submitted packet count. It does not extend the media timeline.
            return Ok(if self.input.draining {
                self.ended = true;
                Receive::End
            } else {
                Receive::NeedInput
            });
        };
        let FrameInfo::Packet {
            pts,
            time_base,
            trim_start,
            trim_end,
            ..
        } = header.info
        else {
            unreachable!()
        };
        let start = usize::try_from(trim_start)?.min(frames as usize);
        let end = (frames as usize).saturating_sub(usize::try_from(trim_end)?);
        if start >= end {
            return Ok(Receive::NeedInput);
        }
        let stride = usize::from(self.track.channels) * 4;
        let pcm = bytes[start * stride..end * stride].to_vec();
        ensure!(
            pcm.as_chunks::<4>()
                .0
                .iter()
                .all(|v| f32::from_le_bytes(*v).is_finite()),
            "non-finite AudioToolbox PCM"
        );
        let timestamp = time_base
            .rescale(pts, Rational::MICROS)?
            .checked_add((start as i128 * 1_000_000 / i128::from(self.track.sample_rate)) as i64)
            .context("AudioToolbox timestamp overflow")?;
        Ok(Receive::Output(MediaBuffer::new(
            BufferHeader {
                stream_id: header.stream_id,
                generation: header.generation,
                info: FrameInfo::Audio {
                    pts_us: timestamp,
                    rate: self.track.sample_rate,
                    channels: self.track.channels,
                    samples: end - start,
                    format: "f32le".into(),
                },
            },
            vec![pcm],
        )))
    }
    fn drain(&mut self) -> Result<()> {
        self.input.draining = true;
        Ok(())
    }
    fn reset(&mut self, _generation: u64, _target: i64) -> Result<()> {
        status(
            unsafe {
                sys::audio_converter_reset(
                    sys::framework().map_err(anyhow::Error::msg)?,
                    self.converter,
                )
            },
            "reset audio decoder",
        )?;
        self.input.queue.clear();
        self.input.held.clear();
        self.stamps.clear();
        self.input.draining = false;
        self.ended = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compressed_data_survives_fill_return_until_the_next_callback() {
        let mut input = Input {
            queue: VecDeque::from([vec![1, 2, 3]]),
            held: Vec::new(),
            description: AudioStreamPacketDescription::default(),
            draining: false,
        };
        let mut data = AudioBufferList1 {
            number_buffers: 1,
            buffers: [AudioBuffer {
                number_channels: 0,
                data_byte_size: 0,
                data: std::ptr::null_mut(),
            }],
        };
        let mut count = 1;
        let mut desc = std::ptr::null_mut();
        let result = unsafe {
            supply(
                std::ptr::null_mut(),
                &mut count,
                &mut data,
                &mut desc,
                (&mut input as *mut Input).cast(),
            )
        };
        assert_eq!(result, 0);
        assert_eq!(count, 1);
        assert_eq!(input.held.len(), 1);
        assert_eq!(
            unsafe { std::slice::from_raw_parts(data.buffers[0].data, 3) },
            &[1, 2, 3]
        );
        count = 1;
        let result = unsafe {
            supply(
                std::ptr::null_mut(),
                &mut count,
                &mut data,
                &mut desc,
                (&mut input as *mut Input).cast(),
            )
        };
        assert_eq!(result, NO_DATA_NOW);
        assert_eq!(count, 0);
        assert!(input.held.is_empty());
        input.draining = true;
        count = 1;
        assert_eq!(
            unsafe {
                supply(
                    std::ptr::null_mut(),
                    &mut count,
                    &mut data,
                    &mut desc,
                    (&mut input as *mut Input).cast(),
                )
            },
            0
        );
        assert_eq!(count, 0);
    }
    #[test]
    fn system_ffi_layouts_match_the_64_bit_audio_converter_abi() {
        assert_eq!(std::mem::size_of::<AudioStreamBasicDescription>(), 40);
        assert_eq!(std::mem::size_of::<AudioBuffer>(), 16);
        assert_eq!(std::mem::size_of::<AudioBufferList1>(), 24);
        assert_eq!(std::mem::size_of::<AudioStreamPacketDescription>(), 16);
    }
}
