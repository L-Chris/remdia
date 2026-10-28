//! AudioToolbox compressed audio adapter. System symbols are runtime-loaded.
use super::audio_toolbox_sys::{
    self as sys, AudioBuffer, AudioBufferList1, AudioConverterRef, AudioStreamBasicDescription,
    AudioStreamPacketDescription,
};
use anyhow::{Context, Result, ensure};
use remdia_graph::*;
use std::{collections::VecDeque, ffi::c_void};

const NO_DATA_NOW: i32 = 1;
struct Input {
    queue: VecDeque<(Vec<u8>, u32)>,
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
        let Some((packet, frames)) = input.queue.pop_front() else {
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
            variable_frames_in_packet: frames,
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
    stamps: VecDeque<(BufferHeader, u32)>,
    frames: u32,
    ended: bool,
    first: bool,
    private_skip: usize,
    timestamp_shift: i64,
}
impl Decoder {
    pub fn new(config: &NodeConfig) -> Result<Self> {
        let track: Track = serde_json::from_value(config.settings["track"].clone())?;
        ensure!(
            track.kind == "audio"
                && (1..=6).contains(&track.channels)
                && (8000..=192000).contains(&track.sample_rate),
            "invalid AudioToolbox stream parameters"
        );
        let fw = sys::framework().map_err(anyhow::Error::msg)?;
        let (format, frames, aac) = match track.codec.as_str() {
            "aac" => (sys::K_AUDIO_FORMAT_MPEG4_AAC, 1024, true),
            "ac3" => (u32::from_be_bytes(*b"ac-3"), 1536, false),
            "eac3" => (u32::from_be_bytes(*b"ec-3"), 1536, false),
            "mp3" => (
                u32::from_be_bytes(*b".mp3"),
                if track.sample_rate < 32000 { 576 } else { 1152 },
                false,
            ),
            "flac" => (u32::from_be_bytes(*b"flac"), 4096, false),
            "alac" => (u32::from_be_bytes(*b"alac"), 4096, false),
            "opus" => (u32::from_be_bytes(*b"opus"), 960, false),
            "vorbis" => (u32::from_be_bytes(*b"vorb"), 4096, false),
            other => anyhow::bail!("unsupported system audio codec {other}"),
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
        let pre_skip = if track.codec == "opus" {
            let extra: Vec<u8> =
                serde_json::from_value(track.initialization["extra_data"].clone())?;
            ensure!(
                extra.len() >= 19 && &extra[..8] == b"OpusHead",
                "invalid OpusHead"
            );
            usize::from(u16::from_le_bytes([extra[10], extra[11]]))
        } else {
            0
        };
        let timestamp_shift =
            if let Some(shift) = track.initialization["timestamp_shift_samples"].as_i64() {
                shift
            } else if track.codec == "opus" && track.initialization["container"] == "ogg" {
                pre_skip as i64
            } else {
                0
            };
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
            first: true,
            private_skip: pre_skip,
            timestamp_shift,
        };
        // Own the converter before setting properties so every error disposes it.
        if aac {
            let asc: Vec<u8> =
                serde_json::from_value(decoder.track.initialization["extra_data"].clone())?;
            let cookie = if decoder.track.initialization["system_cookie"] == true {
                asc
            } else {
                super::apple_config::aac_cookie(&asc)?
            };
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
        if matches!(
            decoder.track.codec.as_str(),
            "flac" | "alac" | "opus" | "vorbis"
        ) {
            let mut cookie: Vec<u8> =
                serde_json::from_value(decoder.track.initialization["extra_data"].clone())?;
            if decoder.track.codec == "flac"
                && decoder.track.initialization["system_cookie"] != true
            {
                ensure!(cookie.len() == 34, "invalid FLAC streaminfo");
                let mut header = Vec::new();
                header.extend_from_slice(&50u32.to_be_bytes());
                header.extend_from_slice(b"dfLa");
                header.extend_from_slice(&[0, 0, 0, 0, 0x80, 0, 0, 34]);
                header.extend(cookie);
                cookie = header;
            }
            if decoder.track.codec == "vorbis"
                && decoder.track.initialization["system_cookie"] != true
            {
                // Symphonia publishes identification + setup, without comments.
                // CoreAudio accepts the standard three-packet Xiph codec private
                // format. Comments do not affect decoding; use an empty header.
                ensure!(
                    cookie.len() > 37
                        && cookie[..7] == *b"\x01vorbis"
                        && cookie[30..37] == *b"\x05vorbis",
                    "invalid Vorbis initialization"
                );
                let mut xiph = vec![2, 30, 16];
                xiph.extend_from_slice(&cookie[..30]);
                xiph.extend_from_slice(b"\x03vorbis\0\0\0\0\0\0\0\0\x01");
                xiph.extend_from_slice(&cookie[30..]);
                cookie = xiph;
            }
            if !cookie.is_empty() {
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
                    "set compressed decoder cookie",
                )?;
            }
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
    fn packet_frames(&self, input: &MediaBuffer) -> Result<u32> {
        match self.track.codec.as_str() {
            "opus" => super::opus::packet_frames(input.bytes(), self.track.sample_rate),
            "mp3" | "aac" | "ac3" | "eac3" => Ok(self.frames),
            "vorbis" => {
                if let FrameInfo::Packet {
                    duration,
                    trim_end,
                    time_base,
                    ..
                } = input.header.info
                {
                    let frames = u128::from(duration)
                        * time_base.num as u128
                        * u128::from(self.track.sample_rate)
                        / time_base.den as u128
                        + u128::from(trim_end);
                    ensure!(frames <= 32768, "Vorbis duration exceeds budget");
                    Ok(frames as u32)
                } else {
                    anyhow::bail!("expected Vorbis packet")
                }
            }
            _ => packet_frames(&input.header, self.track.sample_rate, self.frames),
        }
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
        let frames = self.packet_frames(input)?;
        self.input.queue.push_back((input.bytes().to_vec(), frames));
        if !(self.track.codec == "vorbis"
            && matches!(input.header.info, FrameInfo::Packet { duration: 0, .. }))
        {
            self.stamps.push_back((input.header.clone(), frames));
        }
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> Result<Receive> {
        if self.ended {
            return Ok(Receive::End);
        }
        let fw = sys::framework().map_err(anyhow::Error::msg)?;
        let expected = self.stamps.front().map_or(self.frames, |stamp| stamp.1);
        let mut bytes = vec![0u8; expected as usize * usize::from(self.track.channels) * 4];
        let mut output = AudioBufferList1 {
            number_buffers: 1,
            buffers: [AudioBuffer {
                number_channels: u32::from(self.track.channels),
                data_byte_size: bytes.len() as u32,
                data: bytes.as_mut_ptr(),
            }],
        };
        let mut frames = expected;
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
            length <= bytes.len() && frames <= expected,
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
        let Some((header, _)) = self.stamps.pop_front() else {
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
        let implicit = if self.first && matches!(self.track.codec.as_str(), "mp3" | "opus") {
            expected.saturating_sub(frames) as usize
        } else {
            0
        };
        if self.first && pts > 0 {
            self.private_skip = 0;
        }
        let private = self.private_skip.min(expected as usize);
        self.private_skip -= private;
        self.first = false;
        let requested_trim = usize::try_from(trim_start)?
            .checked_add(private)
            .context("trim overflow")?;
        let start = requested_trim.saturating_sub(implicit).min(frames as usize);
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
            .checked_add(
                ((start as i128 + implicit as i128 - i128::from(self.timestamp_shift)) * 1_000_000
                    / i128::from(self.track.sample_rate)) as i64,
            )
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
        self.first = true;
        self.private_skip = if self.track.codec == "opus" {
            let extra: Vec<u8> =
                serde_json::from_value(self.track.initialization["extra_data"].clone())?;
            usize::from(u16::from_le_bytes([extra[10], extra[11]]))
        } else {
            0
        };
        Ok(())
    }
}

fn packet_frames(header: &BufferHeader, rate: u32, fallback: u32) -> Result<u32> {
    let FrameInfo::Packet {
        duration,
        time_base,
        ..
    } = header.info
    else {
        anyhow::bail!("expected packet")
    };
    ensure!(
        time_base.num > 0 && time_base.den > 0,
        "invalid packet timebase"
    );
    let count =
        u128::from(duration) * time_base.num as u128 * u128::from(rate) / time_base.den as u128;
    let frames = if count == 0 {
        fallback
    } else {
        u32::try_from(count)?
    };
    ensure!(
        frames > 0 && frames <= 32768,
        "audio packet duration exceeds budget"
    );
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compressed_data_survives_fill_return_until_the_next_callback() {
        let mut input = Input {
            queue: VecDeque::from([(vec![1, 2, 3], 0)]),
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
