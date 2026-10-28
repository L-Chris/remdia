use anyhow::{Context, Result, ensure};
use remdia_graph::*;
use serde::Deserialize;
use std::sync::OnceLock;
use symphonia_core::{
    audio::{Channels, Position},
    codecs::{
        audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions},
        registry::CodecRegistry,
    },
    packet::PacketRef,
    units::{Duration, Timestamp},
};
pub fn codec_registry() -> &'static CodecRegistry {
    static REGISTRY: OnceLock<CodecRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = CodecRegistry::new();
        registry.register_audio_decoder::<symphonia_codec_vorbis::VorbisDecoder>();
        registry.register_audio_decoder::<symphonia_adapter_libopus::OpusDecoder>();
        registry
    })
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct Wire {
    sample_rate: Option<u32>,
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
        let codec = match track.codec.as_str() {
            "opus" => symphonia_core::codecs::audio::well_known::CODEC_ID_OPUS,
            "vorbis" => symphonia_core::codecs::audio::well_known::CODEC_ID_VORBIS,
            _ => anyhow::bail!("codec is outside this narrow fallback"),
        };
        let channels = if wire.channel_mask != 0 {
            Some(Channels::Positioned(Position::from_bits_truncate(
                wire.channel_mask,
            )))
        } else {
            Some(Channels::Discrete(track.channels))
        };
        let params = AudioCodecParameters {
            codec,
            sample_rate: Some(wire.sample_rate.unwrap_or(track.sample_rate)),
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
        let mut timestamp = time_base.rescale(pts, Rational::MICROS)?;
        timestamp = timestamp
            .checked_add((u128::from(trim_start) * 1_000_000 / u128::from(rate)) as i64)
            .context("trim timestamp overflow")?;
        if self.track.codec == "opus" {
            let playable = time_base
                .rescale(
                    i64::try_from(duration)?,
                    Rational {
                        num: 1,
                        den: rate as i32,
                    },
                )?
                .max(0) as u64;
            let implicit = playable.saturating_sub(frames as u64);
            let shift = if self.track.initialization["container"] == "ogg" {
                let extra: Vec<u8> =
                    serde_json::from_value(self.track.initialization["extra_data"].clone())?;
                ensure!(extra.len() >= 19, "invalid OpusHead");
                u64::from(u16::from_le_bytes([extra[10], extra[11]]))
            } else {
                0
            };
            timestamp = i64::try_from(
                i128::from(timestamp)
                    + (i128::from(implicit) - i128::from(shift)) * 1_000_000 / i128::from(rate),
            )?;
        }
        let info = FrameInfo::Audio {
            pts_us: timestamp,
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
        *self = Self::new(&NodeConfig {
            component: format!("fallback.{}.decode", self.track.codec),
            settings: serde_json::json!({"track":self.track}),
        })?;
        Ok(())
    }
}
