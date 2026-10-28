use crate::audio::codec_registry;
use anyhow::{Context, Result, ensure};
use remdia_graph::*;
use serde_json::json;
use std::collections::BTreeMap;
use symphonia::core::audio::Channels;
use symphonia::core::{
    codecs::{CodecParameters, audio::AudioCodecId, video::well_known as video},
    formats::probe::Hint,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType},
    io::MediaSourceStream,
    meta::{MetadataOptions, StandardTag},
    units::Time,
};

pub struct Demux {
    format: Box<dyn FormatReader>,
    info: NodeInfo,
    control: Control,
    request: Request,
    generation: u64,
    ended: bool,
}
fn codec_name(id: AudioCodecId) -> String {
    codec_registry()
        .get_audio_decoder(id)
        .map(|d| d.codec.info.short_name.to_owned())
        .unwrap_or_else(|| id.to_string())
}
fn video_name(p: &symphonia::core::codecs::video::VideoCodecParameters) -> String {
    match p.codec {
        video::CODEC_ID_H264 => "h264",
        video::CODEC_ID_HEVC => "hevc",
        video::CODEC_ID_VP8 => "vp8",
        video::CODEC_ID_VP9 => "vp9",
        video::CODEC_ID_MPEG4 => "mpeg4",
        video::CODEC_ID_MPEG2 => "mpeg2",
        video::CODEC_ID_VC1 => "vc1",
        _ => "unsupported_video",
    }
    .into()
}
impl Demux {
    pub fn open(config: &NodeConfig, control: Control) -> Result<Self> {
        let request: Request = serde_json::from_value(config.settings.clone())?;
        request.validate()?;
        let source = super::source::open(&request, control)?;
        let mss = MediaSourceStream::new(source, Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = std::path::Path::new(&request.source)
            .extension()
            .and_then(|s| s.to_str())
        {
            hint.with_extension(ext);
        }
        let mut format = symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .with_context(|| format!("probe {}", request.source))?;
        let mut metadata = BTreeMap::new();
        if let Some(revision) = format.metadata().skip_to_latest() {
            for tag in &revision.media.tags {
                let pair = match &tag.std {
                    Some(StandardTag::TrackTitle(v) | StandardTag::MovieTitle(v)) => {
                        Some(("title", v.as_str()))
                    }
                    Some(StandardTag::Artist(v)) => Some(("artist", v.as_str())),
                    Some(StandardTag::Album(v)) => Some(("album", v.as_str())),
                    _ => None,
                };
                if let Some((key, value)) = pair {
                    metadata.insert(key.into(), value.into());
                } else {
                    metadata
                        .entry(tag.raw.key.to_lowercase())
                        .or_insert_with(|| tag.raw.value.to_string());
                }
            }
        }
        let mut tracks = Vec::new();
        for t in format.tracks() {
            let Some(params) = &t.codec_params else {
                continue;
            };
            let tb = t.time_base.unwrap_or_else(|| {
                symphonia::core::units::TimeBase::try_new(1, 1_000_000).unwrap()
            });
            let mut track = Track {
                index: t.id as usize,
                kind: String::new(),
                codec: String::new(),
                time_base: Rational {
                    num: i32::try_from(tb.numer.get())?,
                    den: i32::try_from(tb.denom.get())?,
                },
                width: 0,
                height: 0,
                sample_rate: 0,
                channels: 0,
                attached_picture: false,
                metadata: BTreeMap::new(),
                initialization: serde_json::Value::Null,
            };
            match params {
                CodecParameters::Audio(p) => {
                    track.kind = "audio".into();
                    track.codec = codec_name(p.codec);
                    track.sample_rate = p.sample_rate.unwrap_or(0);
                    track.channels = p.channels.as_ref().map_or(0, |c| c.count() as u16);
                    let mask = match &p.channels {
                        Some(Channels::Positioned(p)) => p.bits(),
                        _ => 0,
                    };
                    track.initialization = json!({"codec_id":p.codec.to_string(),"sample_rate":p.sample_rate,"channels":track.channels,"channel_mask":mask,"bits_per_sample":p.bits_per_sample,"bits_per_coded_sample":p.bits_per_coded_sample,"max_frames_per_packet":p.max_frames_per_packet,"frames_per_block":p.frames_per_block,"extra_data":p.extra_data.as_deref().unwrap_or(&[])});
                }
                CodecParameters::Video(p) => {
                    track.kind = "video".into();
                    track.codec = video_name(p);
                    track.width = p.width.unwrap_or(0) as u32;
                    track.height = p.height.unwrap_or(0) as u32;
                    track.initialization = json!({"profile":p.profile.map(|p|p.get()),"level":p.level,"extra_data":p.extra_data.iter().map(|d|d.data.to_vec()).collect::<Vec<_>>()});
                }
                _ => continue,
            }
            tracks.push(track);
        }
        let media = format.media_info();
        let duration_us = media
            .time_base
            .zip(media.duration)
            .and_then(|(tb, d)| tb.calc_duration(d))
            .and_then(|t| i64::try_from(t.as_micros()).ok());
        // Symphonia timestamps include signed composition offsets. Keep their original
        // integer time bases, normalize only public output to media start.
        let origin_us = media
            .time_base
            .and_then(|tb| tb.calc_time(media.start_ts))
            .and_then(|t| i64::try_from(t.as_micros()).ok())
            .unwrap_or(0);
        let media = MediaInfo {
            container: format.format_info().short_name.into(),
            duration_us,
            origin_us,
            metadata,
            tracks,
        };
        let info = NodeInfo {
            input: None,
            output: PortCaps::new(DataKind::Packet),
            max_output_bytes: request.max_packet_bytes,
            media: Some(media),
        };
        Ok(Self {
            format,
            info,
            control,
            request,
            generation: 1,
            ended: false,
        })
    }
}
impl Component for Demux {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn receive(&mut self) -> Result<Receive> {
        self.control.check()?;
        if self.ended {
            return Ok(Receive::End);
        }
        let result = self.format.next_packet();
        self.control.check()?;
        let Some(packet) = result? else {
            self.ended = true;
            return Ok(Receive::End);
        };
        ensure!(
            packet.data.len() <= self.request.max_packet_bytes,
            "packet exceeds byte budget"
        );
        let track = self
            .info
            .media
            .as_ref()
            .unwrap()
            .tracks
            .iter()
            .find(|t| t.index == packet.track_id as usize)
            .context("packet has unknown track")?;
        let header = BufferHeader {
            stream_id: packet.track_id,
            generation: self.generation,
            info: FrameInfo::Packet {
                pts: packet.pts.get(),
                dts: packet.dts.get(),
                duration: packet.dur.get(),
                time_base: track.time_base,
                trim_start: packet.trim_start.get(),
                trim_end: packet.trim_end.get(),
                keyframe: false,
            },
        };
        Ok(Receive::Output(MediaBuffer::new(
            header,
            vec![packet.data.into_vec()],
        )))
    }
    fn drain(&mut self) -> Result<()> {
        self.ended = true;
        Ok(())
    }
    fn reset(&mut self, generation: u64, target: i64) -> Result<()> {
        self.control.check()?;
        let kind = if self.request.task == Task::Audio {
            TrackType::Audio
        } else if self.format.default_track(TrackType::Video).is_some() {
            TrackType::Video
        } else {
            TrackType::Audio
        };
        let track_id = self.format.default_track(kind).map(|t| t.id);
        self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: Time::from_micros(target),
                track_id,
            },
        )?;
        self.generation = generation;
        self.ended = false;
        Ok(())
    }
    fn command(&mut self, command: &serde_json::Value) -> Result<Option<MediaBuffer>> {
        if command.get("op").and_then(|v| v.as_str()) != Some("artwork") {
            return Ok(None);
        }
        let revision = self.format.metadata().skip_to_latest().cloned();
        if let Some(revision) = revision
            && let Some(v) = revision.media.visuals.first()
        {
            ensure!(
                v.data.len() <= 8 * 1024 * 1024,
                "artwork exceeds 8MiB limit"
            );
            return Ok(Some(MediaBuffer::new(
                BufferHeader {
                    stream_id: 0,
                    generation: self.generation,
                    info: FrameInfo::Artwork {
                        codec: v.media_type.clone().unwrap_or_default(),
                    },
                },
                vec![v.data.to_vec()],
            )));
        }
        Ok(None)
    }
}
