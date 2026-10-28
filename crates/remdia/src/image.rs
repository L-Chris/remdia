//! Image requests are plans for the ordinary media Graph, not an image runtime.
use crate::*;
use remdia_graph::ImageInfo;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};

#[derive(Clone)]
pub struct Image(pub(crate) MediaBuffer);
impl Image {
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width(), self.height())
    }
    pub fn pixels(&self) -> impl Iterator<Item = &[u8; 4]> {
        self.bytes()
            .chunks(self.info().stride)
            .flat_map(|row| row[..self.width() as usize * 4].as_chunks::<4>().0.iter())
    }
    pub fn from_rgba(width: u32, height: u32, bytes: Vec<u8>) -> Result<Self> {
        let info = ImageInfo::rgba(width, height)?;
        info.validate(&bytes)?;
        Ok(Self(MediaBuffer::new(
            BufferHeader {
                stream_id: 0,
                generation: 0,
                info: FrameInfo::Image {
                    image: info,
                    pts_us: None,
                },
            },
            vec![bytes],
        )))
    }
    pub fn from_buffer(buffer: MediaBuffer) -> Result<Self> {
        ensure!(
            matches!(buffer.header.info, FrameInfo::Image { .. }),
            "expected image frame"
        );
        buffer.validate()?;
        Ok(Self(buffer))
    }
    pub fn info(&self) -> &ImageInfo {
        let FrameInfo::Image { image, .. } = &self.0.header.info else {
            unreachable!()
        };
        image
    }
    pub fn width(&self) -> u32 {
        self.info().width
    }
    pub fn height(&self) -> u32 {
        self.info().height
    }
    pub fn bytes(&self) -> &[u8] {
        self.0.bytes()
    }
    pub fn buffer(&self) -> &MediaBuffer {
        &self.0
    }
    pub fn row(&self, y: u32) -> Result<&[u8]> {
        ensure!(y < self.height(), "row outside image");
        let start = y as usize * self.info().stride;
        Ok(&self.bytes()[start..start + self.width() as usize * 4])
    }
    pub fn pixel(&self, x: u32, y: u32) -> Result<[u8; 4]> {
        ensure!(x < self.width(), "pixel outside image");
        let row = self.row(y)?;
        Ok(row[x as usize * 4..x as usize * 4 + 4].try_into()?)
    }
}
impl PartialEq for Image {
    fn eq(&self, other: &Self) -> bool {
        self.info() == other.info() && self.bytes() == other.bytes()
    }
}
#[derive(Clone)]
pub enum ImageInput {
    File(PathBuf),
    Bytes(Arc<[u8]>),
    Pixels(Image),
    Sequence(Vec<Image>),
}
impl ImageInput {
    pub(crate) fn buffers(self, limit: usize) -> Result<Vec<MediaBuffer>> {
        let data = match self {
            Self::Pixels(image) => return Ok(vec![image.0]),
            Self::Sequence(images) => {
                ensure!(!images.is_empty(), "empty image sequence");
                return Ok(images.into_iter().map(|i| i.0).collect());
            }
            Self::Bytes(bytes) => bytes,
            Self::File(path) => {
                use std::io::Read;
                let file = std::fs::File::open(path)?;
                ensure!(
                    file.metadata()?.len() <= limit as u64,
                    "image input exceeds limit"
                );
                let mut bytes = Vec::new();
                file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
                Arc::from(bytes)
            }
        };
        ensure!(
            !data.is_empty() && data.len() <= limit,
            "image input exceeds limit"
        );
        Ok(vec![MediaBuffer::from_owner(
            BufferHeader {
                stream_id: 0,
                generation: 0,
                info: FrameInfo::Bytes {
                    format: "encoded_image".into(),
                    offset: None,
                },
            },
            Box::new(crate::jobs::SharedBytes(data)),
            None,
        )])
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageOperation {
    pub operation: String,
    #[serde(default)]
    pub settings: serde_json::Value,
}
impl ImageOperation {
    pub fn new(operation: impl Into<String>, settings: serde_json::Value) -> Self {
        Self {
            operation: operation.into(),
            settings,
        }
    }
}
pub struct ImageRequest {
    pub input: ImageInput,
    pub operations: Vec<ImageOperation>,
    pub max_input_bytes: usize,
    pub timeout_ms: u64,
    pub policy: ProviderPolicy,
}
impl ImageRequest {
    pub fn new(input: ImageInput, operations: Vec<ImageOperation>) -> Self {
        Self {
            input,
            operations,
            max_input_bytes: 40 * 1024 * 1024,
            timeout_ms: 30_000,
            policy: ProviderPolicy::SystemPreferred,
        }
    }
    pub(crate) fn plan(&self, engine: &Engine) -> Result<JobPlan> {
        ensure!(
            self.max_input_bytes > 0
                && self.max_input_bytes <= 256 * 1024 * 1024
                && self.timeout_ms > 0
                && self.operations.len() <= 64,
            "invalid image job limits"
        );
        let mut nodes = vec![graph::NodeSpec {
            id: "input".into(),
            config: NodeConfig {
                component: "core.buffer.source".into(),
                settings: json!({}),
            },
        }];
        let mut edges = Vec::new();
        for (i, operation) in self.operations.iter().enumerate() {
            let component = format!("{}.image.{}", std::env::consts::OS, operation.operation);
            engine.registry.select(&component)?;
            let id = format!("image_{i}");
            let previous = nodes.last().unwrap().id.clone();
            let mut settings = operation.settings.clone();
            ensure!(settings.is_object(), "image settings must be an object");
            if matches!(operation.operation.as_str(), "decode" | "probe") {
                settings["max_input_bytes"] = json!(self.max_input_bytes);
            }
            nodes.push(graph::NodeSpec {
                id: id.clone(),
                config: NodeConfig {
                    component,
                    settings,
                },
            });
            let mut edge = EdgeSpec::between(previous, id, None);
            edge.max_bytes = engine.budget.limit();
            edge.max_buffers = 2;
            edges.push(edge);
        }
        Ok(JobPlan {
            graph: GraphSpec { nodes, edges },
            notes: vec![
                "System image components run through the shared media graph and plugin ABI".into(),
            ],
        })
    }
}
