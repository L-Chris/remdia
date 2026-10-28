//! Independent Media Foundation Audio Resampler DSP component.
use super::windows_audio::{Apartment, startup};
use anyhow::{Context, Result, ensure};
use remdia_graph::*;
use std::mem::ManuallyDrop;
use windows::Win32::{
    Media::MediaFoundation::*,
    System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
};

unsafe fn pcm_type(rate: u32, channels: u16) -> Result<IMFMediaType> {
    unsafe {
        let format = MFCreateMediaType()?;
        format.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        format.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
        format.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)?;
        format.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, u32::from(channels))?;
        format.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 32)?;
        format.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, u32::from(channels) * 4)?;
        format.SetUINT32(
            &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
            rate * u32::from(channels) * 4,
        )?;
        let mask = match channels {
            1 => 4,
            2 => 3,
            3 => 7,
            4 => 0x33,
            5 => 0x37,
            6 => 0x3f,
            _ => anyhow::bail!("unsupported channel layout"),
        };
        format.SetUINT32(&MF_MT_AUDIO_CHANNEL_MASK, mask)?;
        Ok(format)
    }
}
struct Segment {
    transform: IMFTransform,
    header: BufferHeader,
    source_rate: u32,
    source_channels: u16,
    anchor: i64,
    submitted: u64,
    emitted: u64,
    flushing: bool,
}
pub struct Resample {
    info: NodeInfo,
    control: Control,
    rate: u32,
    channels: u16,
    segment: Option<Segment>,
    draining: bool,
    _apartment: Apartment,
}
impl Resample {
    pub fn new(config: &NodeConfig, control: Control) -> Result<Self> {
        let apartment = Apartment::new()?;
        startup()?;
        let rate = u32::try_from(config.settings["rate"].as_u64().unwrap_or(48000))?;
        let channels = u16::try_from(config.settings["channels"].as_u64().unwrap_or(2))?;
        ensure!(
            (8000..=192000).contains(&rate) && (1..=2).contains(&channels),
            "invalid system resampling output"
        );
        let mut output = PortCaps::new(DataKind::Audio);
        output.rate = Some(rate);
        output.channels = Some(channels);
        output.format = Some("f32le".into());
        Ok(Self {
            info: NodeInfo {
                input: Some(PortCaps::new(DataKind::Audio)),
                output,
                max_output_bytes: 1024 * 1024,
                media: None,
            },
            control,
            rate,
            channels,
            segment: None,
            draining: false,
            _apartment: apartment,
        })
    }
    fn start(&mut self, buffer: &MediaBuffer, rate: u32, channels: u16, pts: i64) -> Result<()> {
        unsafe {
            let transform: IMFTransform =
                CoCreateInstance(&CResamplerMediaObject, None, CLSCTX_INPROC_SERVER)?;
            transform.SetInputType(0, &pcm_type(rate, channels)?, 0)?;
            transform.SetOutputType(0, &pcm_type(self.rate, self.channels)?, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            self.segment = Some(Segment {
                transform,
                header: buffer.header.clone(),
                source_rate: rate,
                source_channels: channels,
                anchor: pts,
                submitted: 0,
                emitted: 0,
                flushing: false,
            });
        }
        Ok(())
    }
    fn flush(segment: &mut Segment) -> Result<()> {
        if !segment.flushing {
            unsafe {
                segment
                    .transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
                segment
                    .transform
                    .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
            }
            segment.flushing = true;
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
        input.validate()?;
        ensure!(!self.draining, "input after resampler drain");
        let FrameInfo::Audio {
            pts_us,
            rate,
            channels,
            samples,
            ..
        } = input.header.info
        else {
            anyhow::bail!("expected PCM")
        };
        ensure!(
            (8000..=192000).contains(&rate)
                && (1..=6).contains(&channels)
                && input.len() <= 16 * 1024 * 1024,
            "invalid system resampler input"
        );
        if samples == 0 {
            return Ok(Submit::Accepted);
        }
        if let Some(segment) = &mut self.segment {
            let expected = i128::from(segment.anchor)
                + i128::from(segment.submitted) * 1_000_000 / i128::from(segment.source_rate);
            if rate != segment.source_rate
                || channels != segment.source_channels
                || input.header.stream_id != segment.header.stream_id
                || (i128::from(pts_us) - expected).abs() > 20_000
            {
                Self::flush(segment)?;
                return Ok(Submit::WouldBlock);
            }
        } else {
            self.start(input, rate, channels, pts_us)?;
        }
        let segment = self.segment.as_mut().unwrap();
        unsafe {
            let buffer = MFCreateMemoryBuffer(u32::try_from(input.len())?)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            std::ptr::copy_nonoverlapping(input.bytes().as_ptr(), data, input.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(input.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(i64::try_from(
                u128::from(segment.submitted) * 10_000_000 / u128::from(rate),
            )?)?;
            sample.SetSampleDuration(i64::try_from(
                samples as u128 * 10_000_000 / u128::from(rate),
            )?)?;
            match segment.transform.ProcessInput(0, &sample, 0) {
                Ok(()) => {
                    segment.submitted = segment
                        .submitted
                        .checked_add(samples as u64)
                        .context("sample count overflow")?;
                    Ok(Submit::Accepted)
                }
                Err(error) if error.code() == MF_E_NOTACCEPTING => Ok(Submit::WouldBlock),
                Err(error) => Err(error).context("system resampler ProcessInput"),
            }
        }
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
            .div_ceil(u128::from(segment.source_rate));
        let remaining = u64::try_from(expected)?.saturating_sub(segment.emitted);
        unsafe {
            let requirements = segment.transform.GetOutputStreamInfo(0)?;
            ensure!(
                requirements.cbSize as usize <= self.info.max_output_bytes,
                "resampler output exceeds budget"
            );
            let sample = MFCreateSample()?;
            let buffer = MFCreateAlignedMemoryBuffer(
                self.info.max_output_bytes as u32,
                requirements.cbAlignment.saturating_sub(1),
            )?;
            sample.AddBuffer(&buffer)?;
            let mut out = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(Some(sample)),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            };
            let mut flags = 0;
            let result =
                segment
                    .transform
                    .ProcessOutput(0, std::slice::from_mut(&mut out), &mut flags);
            let sample = ManuallyDrop::take(&mut out.pSample);
            drop(ManuallyDrop::take(&mut out.pEvents));
            match result {
                Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                    if segment.flushing {
                        // Rational conversion can leave one fractional output
                        // sample at EOF. Larger deficits indicate a broken drain.
                        ensure!(
                            remaining <= 1,
                            "system resampler ended {remaining} samples early"
                        );
                        if remaining == 1 {
                            let pts = i64::try_from(
                                i128::from(segment.anchor)
                                    + i128::from(segment.emitted) * 1_000_000
                                        / i128::from(self.rate),
                            )?;
                            segment.emitted += 1;
                            let mut header = segment.header.clone();
                            header.info = FrameInfo::Audio {
                                pts_us: pts,
                                rate: self.rate,
                                channels: self.channels,
                                samples: 1,
                                format: "f32le".into(),
                            };
                            return Ok(Receive::Output(MediaBuffer::new(
                                header,
                                vec![vec![0; self.channels as usize * 4]],
                            )));
                        }
                        self.segment = None;
                    }
                    return Ok(if self.draining && self.segment.is_none() {
                        Receive::End
                    } else {
                        Receive::NeedInput
                    });
                }
                Err(error) => return Err(error).context("system resampler ProcessOutput"),
                Ok(()) => {}
            }
            let sample = sample.context("missing system resampler sample")?;
            let buffer = sample.ConvertToContiguousBuffer()?;
            let length = buffer.GetCurrentLength()? as usize;
            let stride = self.channels as usize * 4;
            ensure!(
                length <= self.info.max_output_bytes && length.is_multiple_of(stride),
                "invalid resampler output"
            );
            let frames = (length / stride).min(remaining as usize);
            if frames == 0 {
                return Ok(Receive::NeedInput);
            }
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            let bytes = std::slice::from_raw_parts(data, frames * stride).to_vec();
            buffer.Unlock()?;
            ensure!(
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|v| f32::from_le_bytes(*v).is_finite()),
                "non-finite resampler output"
            );
            let pts = i64::try_from(
                i128::from(segment.anchor)
                    + i128::from(segment.emitted) * 1_000_000 / i128::from(self.rate),
            )?;
            segment.emitted += frames as u64;
            let mut header = segment.header.clone();
            header.info = FrameInfo::Audio {
                pts_us: pts,
                rate: self.rate,
                channels: self.channels,
                samples: frames,
                format: "f32le".into(),
            };
            Ok(Receive::Output(MediaBuffer::new(header, vec![bytes])))
        }
    }
    fn drain(&mut self) -> Result<()> {
        self.draining = true;
        if let Some(segment) = &mut self.segment {
            Self::flush(segment)?;
        }
        Ok(())
    }
    fn reset(&mut self, _generation: u64, _target: i64) -> Result<()> {
        self.segment = None;
        self.draining = false;
        Ok(())
    }
}
