//! Windows system AAC and Dolby MFTs, behind the native audio ABI.
use anyhow::{Context, Result, ensure};
use remdia_graph::*;
use std::{mem::ManuallyDrop, sync::OnceLock};
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
        ensure!(
            matches!(codec, "aac" | "ac3" | "eac3"),
            "unsupported Windows system audio configuration"
        );
        let (class, subtype, aac) = if codec == "ac3" {
            (CMSDDPlusDecMFT, MFAudioFormat_Dolby_AC3, false)
        } else if codec == "eac3" {
            (CMSDDPlusDecMFT, MFAudioFormat_Dolby_DDPlus, false)
        } else {
            (CMSAACDecMFT, MFAudioFormat_AAC, true)
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
                input.SetBlob(&MF_MT_USER_DATA, &user)?;
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
                    max_output_bytes: 1024 * 1024,
                    media: None,
                },
                rate: 0,
                channels: 0,
                generation: 1,
                next_pts: None,
                draining: false,
                discontinuity: true,
                _apartment: apartment,
            };
            decoder.refresh_format()?;
            Ok(decoder)
        }
    }
    fn refresh_format(&mut self) -> Result<()> {
        unsafe {
            let format = self.transform.GetOutputCurrentType(0)?;
            ensure!(
                format.GetGUID(&MF_MT_SUBTYPE)? == MFAudioFormat_Float,
                "AAC output must be float PCM"
            );
            self.rate = format.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?;
            self.channels = u16::try_from(format.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)?)?;
            ensure!(
                (8000..=48000).contains(&self.rate) && (1..=6).contains(&self.channels),
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
        for index in 0..128 {
            let format = match transform.GetOutputAvailableType(0, index) {
                Ok(format) => format,
                Err(e) if e.code() == MF_E_NO_MORE_TYPES => break,
                Err(e) => return Err(e.into()),
            };
            if format.GetGUID(&MF_MT_SUBTYPE)? == MFAudioFormat_Float
                && format.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)? == u32::from(channels)
            {
                transform.SetOutputType(0, &format, 0)?;
                return Ok(());
            }
        }
    }
    anyhow::bail!("Windows AAC cannot negotiate multichannel float PCM")
}
impl Component for Decoder {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, input: &MediaBuffer) -> Result<Submit> {
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
        let data = input.bytes();
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
            match self.transform.ProcessInput(0, &sample, 0) {
                Ok(()) => {
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
        unsafe {
            let requirements = self.transform.GetOutputStreamInfo(0)?;
            ensure!(
                requirements.cbSize as usize <= self.info.max_output_bytes,
                "AAC output exceeds budget"
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
            let result =
                self.transform
                    .ProcessOutput(0, std::slice::from_mut(&mut output), &mut status);
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
                    && len.is_multiple_of(usize::from(self.channels) * 4),
                "invalid AAC PCM length"
            );
            if len == 0 {
                return Ok(Receive::NeedInput);
            }
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            let bytes = std::slice::from_raw_parts(ptr, len).to_vec();
            buffer.Unlock()?;
            ensure!(
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|v| f32::from_le_bytes(*v).is_finite()),
                "non-finite AAC PCM"
            );
            let samples = len / usize::from(self.channels) / 4;
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
        self.generation = generation;
        Ok(())
    }
}
