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
    keyframe_gate: Option<u32>,
    keys: BTreeMap<u32, Vec<(i64, i64)>>,
    scanned: BTreeMap<u32, i64>,
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
            keyframe_gate: None,
            keys: BTreeMap::new(),
            scanned: BTreeMap::new(),
        })
    }
}
impl Demux {
    fn reopen(&mut self) -> Result<()> {
        let source = super::source::open(&self.request, self.control)?;
        self.format = symphonia::default::get_probe().probe(
            &Hint::new(),
            MediaSourceStream::new(source, Default::default()),
            FormatOptions::default(),
            MetadataOptions::default(),
        )?;
        Ok(())
    }
    fn seek_at(&mut self, track: u32, time: i64) -> Result<()> {
        // Recreate the reader before random access. In Symphonia 0.6 MP4,
        // reaching EOF consumes the pending atom needed by subsequent reads.
        self.reopen()?;
        if self
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time: Time::from_micros(time.max(0)),
                    track_id: Some(track),
                },
            )
            .is_err()
        {
            self.reopen()?;
        }
        Ok(())
    }
    fn seek_video(&mut self, id: u32, target: i64) -> Result<()> {
        let track = self
            .info
            .media
            .as_ref()
            .unwrap()
            .tracks
            .iter()
            .find(|t| t.index == id as usize)
            .context("unknown seek track")?
            .clone();
        // Symphonia's experimental video seek locates a timestamp, not a sync
        // sample. Find a preceding random-access point without decoding frames.
        if self.scanned.get(&id).is_some_and(|p| *p >= target)
            && let Some((_, dts)) = self
                .keys
                .get(&id)
                .and_then(|v| v.iter().rev().find(|p| p.0 <= target))
                .copied()
        {
            return self.seek_at(id, dts.saturating_sub(1));
        }
        if !matches!(track.codec.as_str(), "h264" | "hevc") {
            // No reliable random-access parser: correctness requires pre-roll
            // from the beginning, rather than skipping silently to a later GOP.
            return self.reopen();
        }
        let started = std::time::Instant::now();
        let mut window = 4_000_000i64;
        loop {
            self.control.check()?;
            ensure!(
                started.elapsed().as_millis() < self.request.timeout_ms as u128,
                "keyframe pre-roll search timed out"
            );
            let start = target.saturating_sub(window).max(0);
            self.seek_at(id, start)?;
            let mut candidate = None;
            for _ in 0..100000 {
                self.control.check()?;
                let result = self.format.next_packet();
                self.control.check()?;
                let Some(packet) = result? else { break };
                ensure!(
                    packet.data.len() <= self.request.max_packet_bytes,
                    "seek packet exceeds budget"
                );
                if packet.track_id != id {
                    continue;
                }
                let pts = track
                    .time_base
                    .rescale(packet.pts.get(), Rational::MICROS)?;
                if packet_keyframe(&track, &packet.data) == Some(true) && pts <= target {
                    let dts = track
                        .time_base
                        .rescale(packet.dts.get(), Rational::MICROS)?;
                    candidate = Some(dts);
                }
                if pts > target {
                    break;
                }
            }
            if let Some(dts) = candidate {
                return self.seek_at(id, dts.saturating_sub(1));
            }
            if start == 0 {
                return self.reopen();
            }
            window = window.saturating_mul(2);
        }
    }
}
fn packet_keyframe(track: &Track, data: &[u8]) -> Option<bool> {
    if track.kind != "video" {
        return Some(true);
    }
    if !matches!(track.codec.as_str(), "h264" | "hevc") {
        return None;
    }
    let config = track
        .initialization
        .get("extra_data")
        .and_then(|v| v.as_array())
        .and_then(|v| v.first())
        .and_then(|v| v.as_array());
    let length = config
        .and_then(|v| v.get(if track.codec == "h264" { 4 } else { 21 }))
        .and_then(|v| v.as_u64())
        .map(|n| (n as usize & 3) + 1);
    let key = |nal: u8| {
        if track.codec == "h264" {
            nal & 31 == 5
        } else {
            (16..=21).contains(&((nal >> 1) & 63))
        }
    };
    if let Some(length) = length {
        let mut at = 0;
        while at < data.len() {
            let prefix = data.get(at..at.checked_add(length)?)?;
            let len = prefix.iter().fold(0usize, |n, &b| (n << 8) | b as usize);
            at += length;
            let nal = data.get(at..at.checked_add(len)?)?;
            if nal.first().is_some_and(|b| key(*b)) {
                return Some(true);
            }
            at += len;
        }
        Some(false)
    } else {
        for i in 0..data.len().saturating_sub(4) {
            if data[i..].starts_with(&[0, 0, 1]) && key(data[i + 3]) {
                return Some(true);
            }
            if data[i..].starts_with(&[0, 0, 0, 1]) && key(data[i + 4]) {
                return Some(true);
            }
        }
        Some(false)
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
        loop {
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
            let keyframe = packet_keyframe(track, &packet.data);
            if self.keyframe_gate == Some(packet.track_id) {
                if keyframe != Some(true) {
                    continue;
                }
                self.keyframe_gate = None;
            }

            if track.kind == "video" {
                let pts = track
                    .time_base
                    .rescale(packet.pts.get(), Rational::MICROS)?;
                self.scanned
                    .entry(packet.track_id)
                    .and_modify(|p| *p = (*p).max(pts))
                    .or_insert(pts);
                if keyframe == Some(true) {
                    let dts = track
                        .time_base
                        .rescale(packet.dts.get(), Rational::MICROS)?;
                    let keys = self.keys.entry(packet.track_id).or_default();
                    if !keys.iter().any(|p| p.0 == pts) {
                        ensure!(keys.len() < 100000, "keyframe index exceeds limit");
                        keys.push((pts, dts));
                        keys.sort_unstable();
                    }
                }
            }
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
                    keyframe,
                },
            };
            return Ok(Receive::Output(MediaBuffer::new(
                header,
                vec![packet.data.into_vec()],
            )));
        }
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
        if kind == TrackType::Video && target > 0 {
            self.seek_video(track_id.context("no video track")?, target)?;
        } else {
            self.reopen()?;
            if self
                .format
                .seek(
                    SeekMode::Accurate,
                    SeekTo::Time {
                        time: Time::from_micros(target),
                        track_id,
                    },
                )
                .is_err()
            {
                self.reopen()?;
            }
        }
        self.keyframe_gate = if kind == TrackType::Video
            && self.info.media.as_ref().unwrap().tracks.iter().any(|t| {
                Some(t.index as u32) == track_id && matches!(t.codec.as_str(), "h264" | "hevc")
            }) {
            track_id
        } else {
            None
        };
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
