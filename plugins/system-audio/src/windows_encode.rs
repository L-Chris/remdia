//! The system MP3 encoder accepts 16-bit PCM and emits raw MP3 frames.
use super::windows_audio::{Apartment, startup};
use anyhow::{Context, Result, ensure};
use remdia_graph::*;
use std::mem::ManuallyDrop;
use windows::Win32::{
    Media::MediaFoundation::*,
    System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
};
pub struct Encoder {
    transform: IMFTransform,
    info: NodeInfo,
    rate: u32,
    channels: u16,
    submitted: u64,
    draining: bool,
    generation: u64,
    _apartment: Apartment,
}
impl Encoder {
    pub fn new(config: &NodeConfig) -> Result<Self> {
        let apartment = Apartment::new()?;
        startup()?;
        let rate = u32::try_from(config.settings["rate"].as_u64().unwrap_or(32000))?;
        let channels = u16::try_from(config.settings["channels"].as_u64().unwrap_or(1))?;
        ensure!(
            matches!(rate, 16000 | 22050 | 24000 | 32000 | 44100 | 48000)
                && (1..=2).contains(&channels),
            "unsupported system MP3 format"
        );
        unsafe {
            let transform: IMFTransform =
                CoCreateInstance(&MP3ACMCodecWrapper, None, CLSCTX_INPROC_SERVER)?;
            let mut selected = None;
            for index in 0..1024 {
                let format = match transform.GetOutputAvailableType(0, index) {
                    Ok(format) => format,
                    Err(error) if error.code() == MF_E_NO_MORE_TYPES => break,
                    Err(error) => return Err(error).context("enumerate system MP3 output"),
                };
                if format.GetGUID(&MF_MT_SUBTYPE)? == MFAudioFormat_MP3
                    && format.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)? == rate
                    && format.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)? == u32::from(channels)
                {
                    let preferred = format.GetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND)? == 8000;
                    if selected.is_none() || preferred {
                        selected = Some(format);
                    }
                    if preferred {
                        break;
                    }
                }
            }
            let output = selected
                .ok_or_else(|| windows::core::Error::from_hresult(MF_E_INVALIDMEDIATYPE))
                .with_context(|| format!("no advertised system MP3 profile at {rate} Hz"))?;
            transform
                .SetOutputType(0, &output, 0)
                .with_context(|| format!("configure system MP3 output at {rate} Hz"))?;
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)?;
            input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, u32::from(channels))?;
            input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, u32::from(channels) * 2)?;
            input.SetUINT32(
                &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                rate * u32::from(channels) * 2,
            )?;
            transform
                .SetInputType(0, &input, 0)
                .with_context(|| format!("configure system MP3 PCM at {rate} Hz"))?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            let mut input = PortCaps::new(DataKind::Audio);
            input.rate = Some(rate);
            input.channels = Some(channels);
            input.format = Some("f32le".into());
            let mut output = PortCaps::new(DataKind::Bytes);
            output.format = Some("mp3".into());
            Ok(Self {
                transform,
                info: NodeInfo {
                    input: Some(input),
                    output,
                    max_output_bytes: 16 * 1024 * 1024,
                    media: None,
                },
                rate,
                channels,
                submitted: 0,
                draining: false,
                generation: 1,
                _apartment: apartment,
            })
        }
    }
}
impl Component for Encoder {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, input: &MediaBuffer) -> Result<Submit> {
        input.validate()?;
        ensure!(!self.draining, "MP3 input after drain");
        let FrameInfo::Audio {
            rate,
            channels,
            samples,
            ..
        } = input.header.info
        else {
            anyhow::bail!("MP3 encoder requires PCM")
        };
        ensure!(
            rate == self.rate && channels == self.channels && samples <= 120 * rate as usize,
            "invalid MP3 encoder PCM"
        );
        let mut bytes = Vec::with_capacity(input.len() / 2);
        for value in input.bytes().as_chunks::<4>().0 {
            let value = f32::from_le_bytes(*value);
            ensure!(value.is_finite(), "non-finite MP3 input");
            bytes
                .extend_from_slice(&((value.clamp(-1., 1.) * 32767.).round() as i16).to_le_bytes());
        }
        unsafe {
            let buffer = MFCreateMemoryBuffer(u32::try_from(bytes.len())?)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(bytes.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(i64::try_from(
                u128::from(self.submitted) * 10_000_000 / u128::from(rate),
            )?)?;
            sample.SetSampleDuration(i64::try_from(
                samples as u128 * 10_000_000 / u128::from(rate),
            )?)?;
            match self.transform.ProcessInput(0, &sample, 0) {
                Ok(()) => {
                    self.submitted = self
                        .submitted
                        .checked_add(samples as u64)
                        .context("MP3 duration overflow")?;
                    self.generation = input.header.generation;
                    Ok(Submit::Accepted)
                }
                Err(error) if error.code() == MF_E_NOTACCEPTING => Ok(Submit::WouldBlock),
                Err(error) => Err(error).context("system MP3 ProcessInput"),
            }
        }
    }
    fn receive(&mut self) -> Result<Receive> {
        unsafe {
            let requirements = self.transform.GetOutputStreamInfo(0)?;
            ensure!(
                requirements.cbSize as usize <= self.info.max_output_bytes,
                "MP3 output exceeds budget"
            );
            let buffer = MFCreateAlignedMemoryBuffer(
                requirements.cbSize.max(65536),
                requirements.cbAlignment.saturating_sub(1),
            )?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            let mut out = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(Some(sample)),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            };
            let mut flags = 0;
            let result =
                self.transform
                    .ProcessOutput(0, std::slice::from_mut(&mut out), &mut flags);
            let sample = ManuallyDrop::take(&mut out.pSample);
            drop(ManuallyDrop::take(&mut out.pEvents));
            match result {
                Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                    return Ok(if self.draining {
                        Receive::End
                    } else {
                        Receive::NeedInput
                    });
                }
                Err(error) => return Err(error).context("system MP3 ProcessOutput"),
                Ok(()) => {}
            }
            let sample = sample.context("missing system MP3 output")?;
            let buffer = sample.ConvertToContiguousBuffer()?;
            let length = buffer.GetCurrentLength()? as usize;
            ensure!(
                length <= self.info.max_output_bytes,
                "MP3 output exceeds budget"
            );
            if length == 0 {
                return Ok(Receive::NeedInput);
            }
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            let bytes = std::slice::from_raw_parts(data, length).to_vec();
            buffer.Unlock()?;
            Ok(Receive::Output(MediaBuffer::new(
                BufferHeader {
                    stream_id: 0,
                    generation: self.generation,
                    info: FrameInfo::Bytes {
                        format: "mp3".into(),
                        offset: None,
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
    fn reset(&mut self, _generation: u64, _target: i64) -> Result<()> {
        anyhow::bail!("create a new graph to reset an MP3 encoder")
    }
}

pub fn supported_rates() -> Result<Vec<u32>> {
    let mut rates = Vec::new();
    for rate in [16000, 22050, 24000, 32000, 44100, 48000] {
        let config = NodeConfig {
            component: "windows.mp3.encode".into(),
            settings: serde_json::json!({"rate":rate,"channels":1}),
        };
        match Encoder::new(&config) {
            Ok(_) => rates.push(rate),
            Err(error)
                if error.chain().any(|cause| {
                    cause
                        .downcast_ref::<windows::core::Error>()
                        .is_some_and(|error| {
                            matches!(error.code(), MF_E_INVALIDMEDIATYPE | MF_E_NO_MORE_TYPES)
                        })
                }) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(rates)
}
