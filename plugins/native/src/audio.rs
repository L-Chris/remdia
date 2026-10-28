use anyhow::{Context, Result, ensure};
use remdia_graph::*;
use serde::Deserialize;
use std::sync::OnceLock;
use symphonia::core::{
    audio::{Channels, Position},
    codecs::{
        audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions},
        registry::CodecRegistry,
    },
    packet::PacketRef,
    units::{Duration, Timestamp},
};
#[cfg(test)]
mod tests;
pub fn codec_registry() -> &'static CodecRegistry {
    static REGISTRY: OnceLock<CodecRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = CodecRegistry::new();
        symphonia::default::register_enabled_codecs(&mut registry);
        registry.register_audio_decoder::<symphonia_adapter_libopus::OpusDecoder>();
        registry
    })
}
pub fn create(config: &NodeConfig) -> Result<Box<dyn Component>> {
    use symphonia::core::codecs::audio::well_known::{CODEC_ID_AAC, CODEC_ID_AC3, CODEC_ID_EAC3};
    let id = config.settings["track"]["initialization"]["codec_id"].as_str();
    if id == Some(CODEC_ID_AC3.to_string().as_str())
        || id == Some(CODEC_ID_EAC3.to_string().as_str())
        || id == Some(CODEC_ID_AAC.to_string().as_str())
            && config.settings["track"]["channels"].as_u64().unwrap_or(0) > 2
    {
        let codec = if id == Some(CODEC_ID_AC3.to_string().as_str()) {
            "AC-3"
        } else if id == Some(CODEC_ID_EAC3.to_string().as_str()) {
            "E-AC-3"
        } else {
            "multichannel AAC"
        };
        #[cfg(windows)]
        {
            Ok(Box::new(
                super::windows_audio::Decoder::new(config).with_context(|| {
                    format!("Windows system {codec} decoder is unavailable or incompatible")
                })?,
            ))
        }
        #[cfg(target_os = "macos")]
        {
            Ok(Box::new(
                super::apple_audio::Decoder::new(config).with_context(|| {
                    format!("macOS system {codec} decoder is unavailable or incompatible")
                })?,
            ))
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            anyhow::bail!("{codec} requires a supported system audio decoder");
        }
    } else {
        Ok(Box::new(Decoder::new(config)?))
    }
}
#[derive(Deserialize)]
struct Wire {
    codec_id: String,
    sample_rate: Option<u32>,
    channels: u16,
    channel_mask: u64,
    bits_per_sample: Option<u32>,
    bits_per_coded_sample: Option<u32>,
    max_frames_per_packet: Option<u64>,
    frames_per_block: Option<u64>,
    extra_data: Vec<u8>,
}
pub struct Decoder {
    decoder: Box<dyn AudioDecoder>,
    track: Track,
    info: NodeInfo,
    pending: Option<MediaBuffer>,
    draining: bool,
}
impl Decoder {
    pub fn new(config: &NodeConfig) -> Result<Self> {
        let track: Track = serde_json::from_value(
            config
                .settings
                .get("track")
                .cloned()
                .context("decoder requires a track")?,
        )?;
        ensure!(
            track.kind == "audio",
            "audio decoder requires an audio track"
        );
        let wire: Wire = serde_json::from_value(track.initialization.clone())?;
        let codec = known_codecs()
            .into_iter()
            .find(|id| id.to_string() == wire.codec_id)
            .context("unknown audio codec id")?;
        let channels = if wire.channel_mask != 0 {
            Some(Channels::Positioned(Position::from_bits_truncate(
                wire.channel_mask,
            )))
        } else {
            (wire.channels > 0).then_some(Channels::Discrete(wire.channels))
        };
        let params = AudioCodecParameters {
            codec,
            sample_rate: wire.sample_rate,
            channels,
            bits_per_sample: wire.bits_per_sample,
            bits_per_coded_sample: wire.bits_per_coded_sample,
            max_frames_per_packet: wire.max_frames_per_packet,
            frames_per_block: wire.frames_per_block,
            extra_data: (!wire.extra_data.is_empty()).then(|| wire.extra_data.into_boxed_slice()),
            ..Default::default()
        };
        let decoder = codec_registry()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .with_context(|| format!("open {} audio decoder", track.codec))?;
        let mut output = PortCaps::new(DataKind::Audio);
        output.format = Some("f32le".into());
        Ok(Self {
            decoder,
            track,
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
impl Component for Decoder {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, input: &MediaBuffer) -> Result<Submit> {
        ensure!(!self.draining, "submit after decoder drain");
        ensure!(
            input.header.stream_id == self.track.index as u32,
            "wrong decoder track"
        );
        if self.pending.is_some() {
            return Ok(Submit::WouldBlock);
        }
        self.pending = Some(input.clone());
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> Result<Receive> {
        let Some(input) = self.pending.take() else {
            return Ok(if self.draining {
                Receive::End
            } else {
                Receive::NeedInput
            });
        };
        let FrameInfo::Packet {
            pts,
            dts,
            duration,
            trim_start,
            trim_end,
            time_base,
            ..
        } = input.header.info
        else {
            anyhow::bail!("decoder expected a packet")
        };
        let mut packet = PacketRef::new(
            input.header.stream_id,
            Timestamp::new(pts),
            Duration::new(duration),
            input.bytes(),
        );
        packet.dts = Timestamp::new(dts);
        packet.trim_start = Duration::new(trim_start);
        packet.trim_end = Duration::new(trim_end);
        let decoded = match self.decoder.decode_ref(&packet) {
            Ok(v) => v,
            Err(symphonia::core::errors::Error::DecodeError(_)) => return Ok(Receive::NeedInput),
            Err(e) => return Err(e.into()),
        };
        let count = decoded.samples_interleaved();
        if count == 0 {
            return Ok(Receive::NeedInput);
        }
        ensure!(count <= 4 * 1024 * 1024, "decoded PCM exceeds budget");
        let mut samples = vec![0f32; count];
        decoded.copy_to_slice_interleaved(&mut samples);
        let spec = decoded.spec();
        let channels = spec.channels().count() as u16;
        let frames = decoded.frames();
        let rate = spec.rate();
        let bytes = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let info = FrameInfo::Audio {
            pts_us: time_base.rescale(
                pts.checked_add(i64::try_from(trim_start)?)
                    .context("trimmed audio timestamp overflow")?,
                Rational::MICROS,
            )?,
            rate,
            channels,
            samples: frames,
            format: "f32le".into(),
        };
        Ok(Receive::Output(MediaBuffer::new(
            BufferHeader {
                stream_id: input.header.stream_id,
                generation: input.header.generation,
                info,
            },
            vec![bytes],
        )))
    }
    fn drain(&mut self) -> Result<()> {
        self.draining = true;
        Ok(())
    }
    fn reset(&mut self, _generation: u64, _target: i64) -> Result<()> {
        self.decoder.reset();
        self.pending = None;
        self.draining = false;
        Ok(())
    }
}

fn known_codecs() -> Vec<symphonia::core::codecs::audio::AudioCodecId> {
    use symphonia::core::codecs::audio::well_known::*;
    vec![
        CODEC_ID_PCM_S32LE,
        CODEC_ID_PCM_S32LE_PLANAR,
        CODEC_ID_PCM_S32BE,
        CODEC_ID_PCM_S32BE_PLANAR,
        CODEC_ID_PCM_S24LE,
        CODEC_ID_PCM_S24LE_PLANAR,
        CODEC_ID_PCM_S24BE,
        CODEC_ID_PCM_S24BE_PLANAR,
        CODEC_ID_PCM_S16LE,
        CODEC_ID_PCM_S16LE_PLANAR,
        CODEC_ID_PCM_S16BE,
        CODEC_ID_PCM_S16BE_PLANAR,
        CODEC_ID_PCM_S8,
        CODEC_ID_PCM_S8_PLANAR,
        CODEC_ID_PCM_U32LE,
        CODEC_ID_PCM_U32LE_PLANAR,
        CODEC_ID_PCM_U32BE,
        CODEC_ID_PCM_U32BE_PLANAR,
        CODEC_ID_PCM_U24LE,
        CODEC_ID_PCM_U24LE_PLANAR,
        CODEC_ID_PCM_U24BE,
        CODEC_ID_PCM_U24BE_PLANAR,
        CODEC_ID_PCM_U16LE,
        CODEC_ID_PCM_U16LE_PLANAR,
        CODEC_ID_PCM_U16BE,
        CODEC_ID_PCM_U16BE_PLANAR,
        CODEC_ID_PCM_U8,
        CODEC_ID_PCM_U8_PLANAR,
        CODEC_ID_PCM_F32LE,
        CODEC_ID_PCM_F32LE_PLANAR,
        CODEC_ID_PCM_F32BE,
        CODEC_ID_PCM_F32BE_PLANAR,
        CODEC_ID_PCM_F64LE,
        CODEC_ID_PCM_F64LE_PLANAR,
        CODEC_ID_PCM_F64BE,
        CODEC_ID_PCM_F64BE_PLANAR,
        CODEC_ID_PCM_ALAW,
        CODEC_ID_PCM_MULAW,
        CODEC_ID_ADPCM_G722,
        CODEC_ID_ADPCM_G726,
        CODEC_ID_ADPCM_G726LE,
        CODEC_ID_ADPCM_MS,
        CODEC_ID_ADPCM_IMA_WAV,
        CODEC_ID_ADPCM_IMA_QT,
        CODEC_ID_VORBIS,
        CODEC_ID_OPUS,
        CODEC_ID_SPEEX,
        CODEC_ID_MUSEPACK,
        CODEC_ID_MP1,
        CODEC_ID_MP2,
        CODEC_ID_MP3,
        CODEC_ID_AAC,
        CODEC_ID_AC3,
        CODEC_ID_EAC3,
        CODEC_ID_AC4,
        CODEC_ID_DCA,
        CODEC_ID_ATRAC1,
        CODEC_ID_ATRAC3,
        CODEC_ID_ATRAC3PLUS,
        CODEC_ID_ATRAC9,
        CODEC_ID_WMA,
        CODEC_ID_RA10,
        CODEC_ID_RA20,
        CODEC_ID_SIPR,
        CODEC_ID_COOK,
        CODEC_ID_SBC,
        CODEC_ID_APTX,
        CODEC_ID_APTX_HD,
        CODEC_ID_LDAC,
        CODEC_ID_BINK_AUDIO,
        CODEC_ID_SMACKER_AUDIO,
        CODEC_ID_FLAC,
        CODEC_ID_WAVPACK,
        CODEC_ID_MONKEYS_AUDIO,
        CODEC_ID_ALAC,
        CODEC_ID_TTA,
        CODEC_ID_RALF,
        CODEC_ID_TRUEHD,
    ]
}
