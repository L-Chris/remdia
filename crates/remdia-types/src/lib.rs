//! Backend-independent media and control types. Time is integer/rational.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rational {
    pub num: i32,
    pub den: i32,
}
impl Rational {
    pub const MICROS: Self = Self {
        num: 1,
        den: 1_000_000,
    };
    pub fn rescale(self, value: i64, to: Self) -> Result<i64> {
        ensure!(
            self.num > 0 && self.den > 0 && to.num > 0 && to.den > 0,
            "invalid time base"
        );
        Ok(i64::try_from(
            i128::from(value) * i128::from(self.num) * i128::from(to.den)
                / (i128::from(self.den) * i128::from(to.num)),
        )?)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Task {
    Probe,
    Playback,
    Audio,
    Thumbnail,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub source: String,
    pub task: Task,
    pub audio_rate: u32,
    pub audio_channels: u16,
    pub hardware: bool,
    pub threads: u32,
    pub timeout_ms: u64,
    pub max_frame_bytes: usize,
    pub max_packet_bytes: usize,
}
impl Request {
    pub fn playback(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            task: Task::Playback,
            audio_rate: 48_000,
            audio_channels: 2,
            hardware: true,
            threads: 2,
            timeout_ms: 30_000,
            max_frame_bytes: 128 * 1024 * 1024,
            max_packet_bytes: 16 * 1024 * 1024,
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.source.is_empty() && !self.source.contains('\0'),
            "invalid source"
        );
        ensure!(
            (8000..=192000).contains(&self.audio_rate),
            "unsupported output sample rate"
        );
        ensure!(
            (1..=2).contains(&self.audio_channels),
            "output must be mono or stereo"
        );
        ensure!(
            (1..=64).contains(&self.threads) && self.timeout_ms > 0,
            "invalid resource limits"
        );
        ensure!(
            self.max_frame_bytes > 0
                && self.max_frame_bytes <= 1024 * 1024 * 1024
                && self.max_packet_bytes > 0,
            "invalid buffer limits"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track {
    pub index: usize,
    pub kind: String,
    pub codec: String,
    pub time_base: Rational,
    pub width: u32,
    pub height: u32,
    pub sample_rate: u32,
    pub channels: u16,
    pub attached_picture: bool,
    pub metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub initialization: serde_json::Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaInfo {
    pub container: String,
    pub duration_us: Option<i64>,
    pub origin_us: i64,
    pub metadata: BTreeMap<String, String>,
    pub tracks: Vec<Track>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plane {
    pub stride: usize,
    pub rows: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FrameInfo {
    Packet {
        pts: i64,
        dts: i64,
        duration: u64,
        time_base: Rational,
        trim_start: u64,
        trim_end: u64,
        keyframe: Option<bool>,
    },
    Bytes {
        format: String,
        offset: Option<u64>,
    },
    Video {
        pts_us: Option<i64>,
        duration_us: i64,
        width: u32,
        height: u32,
        format: String,
        planes: Vec<Plane>,
        sample_aspect_ratio: Rational,
        color_space: String,
        color_range: String,
        color_primaries: String,
        color_transfer: String,
        hardware_decoded: bool,
    },
    Audio {
        pts_us: i64,
        rate: u32,
        channels: u16,
        samples: usize,
        format: String,
    },
    Artwork {
        codec: String,
    },
    Blob {
        format: String,
    },
}
impl FrameInfo {
    pub fn pts_us(&self) -> Option<i64> {
        match self {
            Self::Video { pts_us, .. } => *pts_us,
            Self::Audio { pts_us, .. } => Some(*pts_us),
            _ => None,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rescaling_is_checked_and_preserves_negative_time() {
        let tb = Rational { num: 1, den: 90000 };
        assert_eq!(tb.rescale(-90000, Rational::MICROS).unwrap(), -1_000_000);
        assert!(Rational { num: 1, den: 0 }.rescale(1, tb).is_err());
        assert!(
            Rational {
                num: i32::MAX,
                den: 1
            }
            .rescale(i64::MAX, Rational::MICROS)
            .is_err()
        );
    }
}
