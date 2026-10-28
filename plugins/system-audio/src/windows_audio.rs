//! Windows system audio decoder MFTs. Output is canonical interleaved float PCM.
use anyhow::{Context, Result, ensure};
use remdia_graph::*;
use std::{collections::VecDeque, mem::ManuallyDrop, sync::OnceLock};
use windows::Win32::{
    Media::MediaFoundation::*,
    System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    },
};

pub(crate) struct Apartment;
impl Apartment {
    pub(crate) fn new() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
pub(crate) fn startup() -> Result<()> {
    static START: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    START
        .get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|e| e.to_string()))
        .clone()
        .map_err(anyhow::Error::msg)
}

struct Stamp {
    header: BufferHeader,
    frames: usize,
    position: usize,
}
pub struct Decoder {
    transform: IMFTransform,
    track: Track,
    info: NodeInfo,
    rate: u32,
    channels: u16,
    generation: u64,
    next_pts: Option<i64>,
    draining: bool,
    discontinuity: bool,
    output_bits: u32,
    output_float: bool,
    stamps: VecDeque<Stamp>,
    pending: VecDeque<MediaBuffer>,
    first: bool,
    seen_input: bool,
    trace: bool,
    _apartment: Apartment,
}
impl Decoder {
    pub fn new(config: &NodeConfig) -> Result<Self> {
        let apartment = Apartment::new()?;
        startup()?;
        let track: Track = serde_json::from_value(config.settings["track"].clone())?;
        ensure!(
            track.kind == "audio" && (1..=6).contains(&track.channels),
            "Windows audio decoder supports up to six channels"
        );
        let extra: Vec<u8> = serde_json::from_value(track.initialization["extra_data"].clone())?;
        let codec = track.codec.as_str();
        let (class, subtype, aac) = match codec {
            "aac" => (CMSAACDecMFT, MFAudioFormat_AAC, true),
            "ac3" => (CMSDDPlusDecMFT, MFAudioFormat_Dolby_AC3, false),
            "eac3" => (CMSDDPlusDecMFT, MFAudioFormat_Dolby_DDPlus, false),
            "mp3" => (CMP3DecMediaObject, MFAudioFormat_MP3, false),
            "flac" => (CMSFLACDecMFT, MFAudioFormat_FLAC, false),
            "alac" => (CMSALACDecMFT, MFAudioFormat_ALAC, false),
            other => anyhow::bail!("unsupported Windows system audio codec {other}"),
        };
        ensure!(
            !aac || (2..=65536).contains(&extra.len()),
            "AAC requires AudioSpecificConfig"
        );
        unsafe {
            let transform: IMFTransform = CoCreateInstance(&class, None, CLSCTX_INPROC_SERVER)
                .context("create Windows audio decoder")?;
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            input.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
            input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, track.sample_rate)?;
            input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, track.channels.into())?;
            if aac {
                input.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
            }
            // HEAACWAVEINFO after WAVEFORMATEX, followed by AudioSpecificConfig.
            let mut user = vec![0u8; 12];
            user[2] = 0xfe; // unspecified audio profile/level
            user.extend_from_slice(&extra);
            if aac {
                input.SetBlob(
                    &MF_MT_USER_DATA,
                    if track.initialization["system_user_data"] == true {
                        &extra
                    } else {
                        &user
                    },
                )?;
            }
            if matches!(codec, "flac" | "alac") {
                input.SetBlob(&MF_MT_USER_DATA, &extra)?;
                if let Some(bits) = track.initialization["bits_per_sample"].as_u64() {
                    input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, u32::try_from(bits)?)?;
                }
            }
            transform
                .SetInputType(0, &input, 0)
                .context("configure Windows audio input")?;
            select_output(&transform, track.channels)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            let mut output = PortCaps::new(DataKind::Audio);
            output.format = Some("f32le".into());
            let mut decoder = Self {
                transform,
                track,
                info: NodeInfo {
                    input: Some(PortCaps::new(DataKind::Packet)),
                    output,
                    max_output_bytes: 16 * 1024 * 1024,
                    media: None,
                },
                rate: 0,
                channels: 0,
                generation: 1,
                next_pts: None,
                draining: false,
                discontinuity: true,
                output_bits: 0,
                output_float: false,
                stamps: VecDeque::new(),
                pending: VecDeque::new(),
                first: true,
                seen_input: false,
                trace: config.settings["trace"].as_bool().unwrap_or(false),
                _apartment: apartment,
            };
            decoder.refresh_format()?;
            Ok(decoder)
        }
    }
    fn mp3_output(&mut self, bytes: Vec<u8>) -> Result<Receive> {
        let stride = self.channels as usize * 4;
        let frames = bytes.len() / stride;
        if self.first {
            self.first = false;
            let nominal = if self.rate < 32000 { 576 } else { 1152 };
            let mut implicit = (nominal - frames % nominal) % nominal;
            while implicit > 0 {
                let stamp = self
                    .stamps
                    .front_mut()
                    .context("missing MP3 priming stamp")?;
                let n = implicit.min(stamp.frames - stamp.position);
                stamp.position += n;
                implicit -= n;
                if stamp.position == stamp.frames {
                    self.stamps.pop_front();
                }
            }
        }
        let mut offset = 0;
        while offset < frames {
            let Some(stamp) = self.stamps.front_mut() else {
                ensure!(self.draining, "MP3 decoder returned unstamped samples");
                break;
            };
            let FrameInfo::Packet {
                pts,
                time_base,
                trim_start,
                trim_end,
                ..
            } = stamp.header.info
            else {
                unreachable!()
            };
            let n = (frames - offset).min(stamp.frames - stamp.position);
            let start = usize::try_from(trim_start)?
                .saturating_sub(stamp.position)
                .min(n);
            let end = stamp
                .frames
                .saturating_sub(usize::try_from(trim_end)?)
                .saturating_sub(stamp.position)
                .min(n);
            if start < end {
                let mut header = stamp.header.clone();
                header.info = FrameInfo::Audio {
                    pts_us: time_base
                        .rescale(pts, Rational::MICROS)?
                        .checked_add(
                            ((stamp.position + start) as i128 * 1_000_000 / i128::from(self.rate))
                                as i64,
                        )
                        .context("MP3 timestamp overflow")?,
                    rate: self.rate,
                    channels: self.channels,
                    samples: end - start,
                    format: "f32le".into(),
                };
                self.pending.push_back(MediaBuffer::new(
                    header,
                    vec![bytes[(offset + start) * stride..(offset + end) * stride].to_vec()],
                ));
            }
            stamp.position += n;
            offset += n;
            if stamp.position == stamp.frames {
                self.stamps.pop_front();
            }
        }
        Ok(self
            .pending
            .pop_front()
            .map_or(Receive::NeedInput, Receive::Output))
    }
    fn refresh_format(&mut self) -> Result<()> {
        unsafe {
            let format = self.transform.GetOutputCurrentType(0)?;
            self.output_float = format.GetGUID(&MF_MT_SUBTYPE)? == MFAudioFormat_Float;
            self.output_bits = format.GetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE)?;
            ensure!(
                self.output_float && self.output_bits == 32
                    || !self.output_float && matches!(self.output_bits, 16 | 24 | 32),
                "unsupported system PCM output"
            );
            self.rate = format.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?;
            self.channels = u16::try_from(format.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)?)?;
            ensure!(
                (8000..=192000).contains(&self.rate) && (1..=6).contains(&self.channels),
                "invalid AAC output format"
            );
            self.info.output.rate = Some(self.rate);
            self.info.output.channels = Some(self.channels);
        }
        Ok(())
    }
}
unsafe fn select_output(transform: &IMFTransform, channels: u16) -> Result<()> {
    unsafe {
        for float in [true, false] {
            for index in 0..128 {
                let format = match transform.GetOutputAvailableType(0, index) {
                    Ok(format) => format,
                    Err(error) if error.code() == MF_E_NO_MORE_TYPES => break,
                    Err(error) => return Err(error.into()),
                };
                let subtype = format.GetGUID(&MF_MT_SUBTYPE)?;
                let bits = format.GetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE)?;
                if subtype
                    == if float {
                        MFAudioFormat_Float
                    } else {
                        MFAudioFormat_PCM
                    }
                    && (if float {
                        bits == 32
                    } else {
                        matches!(bits, 16 | 24 | 32)
                    })
                    && format.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)? == u32::from(channels)
                {
                    transform.SetOutputType(0, &format, 0)?;
                    return Ok(());
                }
            }
        }
    }
    anyhow::bail!("Windows audio decoder cannot negotiate canonical PCM")
}
impl Component for Decoder {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, input: &MediaBuffer) -> Result<Submit> {
        if !self.pending.is_empty() {
            return Ok(Submit::WouldBlock);
        }
        ensure!(
            self.stamps.len() < 64,
            "system decoder made no output progress"
        );
        ensure!(
            !self.draining && input.header.stream_id == self.track.index as u32,
            "invalid AAC decoder input"
        );
        let FrameInfo::Packet {
            pts,
            duration,
            time_base,
            ..
        } = input.header.info
        else {
            anyhow::bail!("AAC decoder requires compressed packets")
        };
        let mut prefix = Vec::new();
        if self.track.codec == "flac" && self.discontinuity {
            let extra: Vec<u8> =
                serde_json::from_value(self.track.initialization["extra_data"].clone())?;
            ensure!(extra.len() == 34, "invalid FLAC streaminfo");
            prefix.extend_from_slice(b"fLaC\x80\0\0\x22");
            prefix.extend_from_slice(&extra);
            prefix.extend_from_slice(input.bytes());
        }
        let data = if prefix.is_empty() {
            input.bytes()
        } else {
            &prefix
        };
        ensure!(
            !data.is_empty() && data.len() <= 16 * 1024 * 1024,
            "AAC packet exceeds budget"
        );
        unsafe {
            let buffer = MFCreateMemoryBuffer(u32::try_from(data.len())?)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            if self.discontinuity {
                sample.SetUINT32(&MFSampleExtension_Discontinuity, 1)?;
            }
            let tb = Rational {
                num: 1,
                den: 10_000_000,
            };
            sample.SetSampleTime(time_base.rescale(pts, tb)?)?;
            if duration > 0 {
                sample.SetSampleDuration(time_base.rescale(i64::try_from(duration)?, tb)?)?;
            }
            if self.trace {
                eprintln!("{}: ProcessInput {} bytes", self.track.codec, data.len());
            }
            match self.transform.ProcessInput(0, &sample, 0) {
                Ok(()) => {
                    if self.track.codec == "mp3" {
                        self.stamps.push_back(Stamp {
                            header: input.header.clone(),
                            frames: if self.track.sample_rate < 32000 {
                                576
                            } else {
                                1152
                            },
                            position: 0,
                        });
                    }
                    self.seen_input = true;
                    self.discontinuity = false;
                    self.generation = input.header.generation;
                    if self.next_pts.is_none() {
                        self.next_pts = Some(time_base.rescale(pts, Rational::MICROS)?);
                    }
                    Ok(Submit::Accepted)
                }
                Err(e) if e.code() == MF_E_NOTACCEPTING => Ok(Submit::WouldBlock),
                Err(e) => Err(e).context("Windows AAC ProcessInput"),
            }
        }
    }
    fn receive(&mut self) -> Result<Receive> {
        if !self.seen_input {
            return Ok(if self.draining {
                Receive::End
            } else {
                Receive::NeedInput
            });
        }
        if let Some(buffer) = self.pending.pop_front() {
            return Ok(Receive::Output(buffer));
        }
        unsafe {
            if self.trace {
                eprintln!("{}: query output requirement", self.track.codec);
            }
            let requirements = self.transform.GetOutputStreamInfo(0)?;
            if self.trace {
                eprintln!(
                    "{}: output requirement {}",
                    self.track.codec, requirements.cbSize
                );
            }
            ensure!(
                requirements.cbSize as usize <= self.info.max_output_bytes,
                "system audio output requirement {} exceeds budget",
                requirements.cbSize
            );
            let sample = if requirements.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0
            {
                None
            } else {
                let buffer = MFCreateAlignedMemoryBuffer(
                    requirements.cbSize.max(65536),
                    requirements.cbAlignment.saturating_sub(1),
                )?;
                let sample = MFCreateSample()?;
                sample.AddBuffer(&buffer)?;
                Some(sample)
            };
            let mut output = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            };
            let mut status = 0;
            if self.trace {
                eprintln!("{}: ProcessOutput", self.track.codec);
            }
            let result =
                self.transform
                    .ProcessOutput(0, std::slice::from_mut(&mut output), &mut status);
            if self.trace {
                eprintln!("{}: ProcessOutput returned {result:?}", self.track.codec);
            }
            let sample = ManuallyDrop::take(&mut output.pSample);
            drop(ManuallyDrop::take(&mut output.pEvents));
            match result {
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                    return Ok(if self.draining {
                        Receive::End
                    } else {
                        Receive::NeedInput
                    });
                }
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    select_output(&self.transform, self.track.channels)?;
                    self.refresh_format()?;
                    return Ok(Receive::Reconfigured);
                }
                Err(e) => return Err(e).context("Windows AAC ProcessOutput"),
                Ok(()) => {}
            }
            let Some(sample) = sample else {
                return Ok(Receive::NeedInput);
            };
            let timestamp = sample
                .GetSampleTime()
                .map(|pts| pts / 10)
                .unwrap_or(self.next_pts.unwrap_or(0));
            let buffer = sample.ConvertToContiguousBuffer()?;
            let len = buffer.GetCurrentLength()? as usize;
            ensure!(
                len <= self.info.max_output_bytes
                    && len.is_multiple_of(
                        usize::from(self.channels) * (self.output_bits as usize / 8)
                    ),
                "invalid AAC PCM length"
            );
            if len == 0 {
                return Ok(Receive::NeedInput);
            }
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            let raw = std::slice::from_raw_parts(ptr, len).to_vec();
            buffer.Unlock()?;
            let bytes = if self.output_float {
                raw
            } else {
                // A scalar representation conversion after a system decoder;
                // no codec, resampling or channel mixing is performed here.
                raw.chunks_exact(self.output_bits as usize / 8)
                    .flat_map(|value| {
                        let sample = match self.output_bits {
                            16 => f32::from(i16::from_le_bytes([value[0], value[1]])) / 32768.,
                            24 => {
                                (i32::from_le_bytes([0, value[0], value[1], value[2]]) as f32)
                                    / 2147483648.
                            }
                            _ => {
                                (i32::from_le_bytes(value.try_into().unwrap()) as f32) / 2147483648.
                            }
                        };
                        sample.to_le_bytes()
                    })
                    .collect::<Vec<_>>()
            };
            ensure!(
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|v| f32::from_le_bytes(*v).is_finite()),
                "non-finite AAC PCM"
            );
            ensure!(
                bytes.len() <= self.info.max_output_bytes,
                "system PCM exceeds output budget"
            );
            if self.track.codec == "mp3" {
                return self.mp3_output(bytes);
            }
            let samples = bytes.len() / usize::from(self.channels) / 4;
            self.next_pts =
                Some(timestamp + (samples as i128 * 1_000_000 / i128::from(self.rate)) as i64);
            Ok(Receive::Output(MediaBuffer::new(
                BufferHeader {
                    stream_id: self.track.index as u32,
                    generation: self.generation,
                    info: FrameInfo::Audio {
                        pts_us: timestamp,
                        rate: self.rate,
                        channels: self.channels,
                        samples,
                        format: "f32le".into(),
                    },
                },
                vec![bytes],
            )))
        }
    }
    fn drain(&mut self) -> Result<()> {
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
        }
        self.draining = true;
        Ok(())
    }
    fn reset(&mut self, generation: u64, _target: i64) -> Result<()> {
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        self.draining = false;
        self.discontinuity = true;
        self.next_pts = None;
        self.stamps.clear();
        self.pending.clear();
        self.first = true;
        self.seen_input = false;
        self.generation = generation;
        Ok(())
    }
}

pub fn capabilities() -> Result<serde_json::Value> {
    let _apartment = Apartment::new()?;
    startup()?;
    let mut decode = Vec::new();
    for (codec, class) in [
        ("aac", CMSAACDecMFT),
        ("mp3", CMP3DecMediaObject),
        ("flac", CMSFLACDecMFT),
        ("alac", CMSALACDecMFT),
        ("ac3", CMSDDPlusDecMFT),
        ("eac3", CMSDDPlusDecMFT),
    ] {
        if unsafe { CoCreateInstance::<_, IMFTransform>(&class, None, CLSCTX_INPROC_SERVER) }
            .is_ok()
        {
            decode.push(codec);
        }
    }
    decode.extend(["pcm_u8", "pcm_s16le", "pcm_s24le", "pcm_s32le", "pcm_f32le"]);
    Ok(
        serde_json::json!({"decode":decode,"resample":unsafe {CoCreateInstance::<_,IMFTransform>(&CResamplerMediaObject,None,CLSCTX_INPROC_SERVER)}.is_ok(),"mp3_encode":unsafe {CoCreateInstance::<_,IMFTransform>(&MP3ACMCodecWrapper,None,CLSCTX_INPROC_SERVER)}.is_ok()}),
    )
}
