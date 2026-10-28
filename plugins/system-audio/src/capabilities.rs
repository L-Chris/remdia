use anyhow::Result;
use remdia_graph::*;
#[cfg(not(windows))]
use serde_json::json;
pub fn query() -> Result<serde_json::Value> {
    #[cfg(target_os = "macos")]
    {
        let formats = super::audio_toolbox_sys::format_ids(false)?;
        let decode = [
            ("aac", *b"aac "),
            ("mp3", *b".mp3"),
            ("flac", *b"flac"),
            ("alac", *b"alac"),
            ("opus", *b"opus"),
            ("vorbis", *b"vorb"),
            ("ac3", *b"ac-3"),
            ("eac3", *b"ec-3"),
        ]
        .into_iter()
        .filter(|(_, id)| formats.contains(&u32::from_be_bytes(*id)))
        .map(|(codec, _)| codec)
        .chain(if formats.contains(&u32::from_be_bytes(*b"lpcm")) {
            pcm_codecs()
        } else {
            vec![]
        })
        .collect::<Vec<_>>();
        let encode = super::audio_toolbox_sys::format_ids(true)?;
        Ok(
            json!({"decode":decode,"resample":true,"mp3_encode":encode.contains(&u32::from_be_bytes(*b".mp3"))}),
        )
    }
    #[cfg(windows)]
    {
        super::windows_audio::capabilities()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    Ok(json!({"decode":[],"resample":false,"mp3_encode":false}))
}
#[cfg(target_os = "macos")]
fn pcm_codecs() -> Vec<&'static str> {
    vec![
        "pcm_u8",
        "pcm_s8",
        "pcm_s16le",
        "pcm_s16be",
        "pcm_s24le",
        "pcm_s24be",
        "pcm_s32le",
        "pcm_s32be",
        "pcm_f32le",
        "pcm_f32be",
        "pcm_f64le",
        "pcm_f64be",
    ]
}
pub struct Capabilities {
    info: NodeInfo,
    pending: Option<MediaBuffer>,
}
impl Capabilities {
    pub fn new() -> Result<Self> {
        let bytes = serde_json::to_vec(&query()?)?;
        Ok(Self {
            info: NodeInfo {
                input: None,
                output: PortCaps::new(DataKind::Bytes),
                max_output_bytes: bytes.len().max(1),
                media: None,
            },
            pending: Some(MediaBuffer::new(
                BufferHeader {
                    stream_id: 0,
                    generation: 1,
                    info: FrameInfo::Blob {
                        format: "application/json".into(),
                    },
                },
                vec![bytes],
            )),
        })
    }
}
impl Component for Capabilities {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn receive(&mut self) -> Result<Receive> {
        Ok(self.pending.take().map_or(Receive::End, Receive::Output))
    }
    fn drain(&mut self) -> Result<()> {
        Ok(())
    }
    fn reset(&mut self, _generation: u64, _target: i64) -> Result<()> {
        *self = Self::new()?;
        Ok(())
    }
}
