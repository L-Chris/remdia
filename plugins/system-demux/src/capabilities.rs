use anyhow::Result;
use remdia_graph::*;
use serde_json::json;
use std::io::Read;
fn container(path: &str) -> Result<&'static str> {
    let mut file = std::fs::File::open(path)?;
    let mut header = [0; 16];
    let size = file.read(&mut header)?;
    let data = &header[..size];
    Ok(if data.get(4..8) == Some(b"ftyp") {
        "mp4"
    } else if data.starts_with(b"fLaC") {
        "flac"
    } else if data.starts_with(b"OggS") {
        "ogg"
    } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WAVE") {
        "wav"
    } else if data.starts_with(b"FORM") {
        "aiff"
    } else if data.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        "mkv"
    } else if data.starts_with(b"ID3")
        || data.len() >= 2 && data[0] == 255 && data[1] & 0xe6 == 0xe2
    {
        "mp3"
    } else if data.len() >= 2 && data[0] == 255 && data[1] & 0xf6 == 0xf0 {
        "aac"
    } else {
        "unknown"
    })
}
pub fn query(config: &NodeConfig) -> Result<serde_json::Value> {
    let mut supported = Vec::new();
    for kind in ["mp4", "mp3", "wav", "flac", "ogg", "aiff", "aac", "mkv"] {
        #[cfg(target_os = "macos")]
        let available = super::apple::supports(kind)?;
        #[cfg(windows)]
        let available = super::windows::supports(kind)?;
        #[cfg(not(any(windows, target_os = "macos")))]
        let available = false;
        if available {
            supported.push(kind);
        }
    }
    let Some(source) = config.settings["source"].as_str() else {
        return Ok(json!({"containers":supported,"sources":["local-file"]}));
    };
    if source.contains("://") {
        return Ok(
            json!({"supported":false,"reason":"system source bridge supports complete local files"}),
        );
    }
    let kind = container(source)?;
    let available = supported.contains(&kind);
    Ok(
        json!({"supported":available,"container":kind,"reason":if available {"system container reader available"} else {"system container type unavailable"}}),
    )
}
pub struct Capabilities {
    info: NodeInfo,
    pending: Option<MediaBuffer>,
}
impl Capabilities {
    pub fn new(config: &NodeConfig) -> Result<Self> {
        let bytes = serde_json::to_vec(&query(config)?)?;
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
        anyhow::bail!("create a fresh capability query")
    }
}
