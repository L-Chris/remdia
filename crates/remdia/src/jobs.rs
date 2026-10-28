use crate::*;
use remdia_graph::{Component, DataKind, NodeInfo, PlaneOwner, PortCaps, Receive};
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};

pub enum MediaRequest {
    Media(Request),
    Image(image::ImageRequest),
}
#[derive(Serialize)]
pub struct JobPlan {
    pub graph: GraphSpec,
    pub notes: Vec<String>,
}
pub enum JobSession {
    Media(Box<Session>),
    Graph {
        graph: Graph,
        timeout: Duration,
        started: Instant,
    },
}
impl JobSession {
    pub fn read(&mut self) -> Result<Option<Frame>> {
        match self {
            Self::Media(session) => session.read(),
            Self::Graph {
                graph,
                timeout,
                started,
            } => loop {
                ensure!(started.elapsed() < *timeout, "media job timed out");
                match graph.poll()? {
                    GraphEvent::Output { buffer, .. } => return Ok(Some(Frame(buffer))),
                    GraphEvent::End => return Ok(None),
                    GraphEvent::Pending => {}
                }
            },
        }
    }
}
impl Engine {
    /// Resolve runtime capabilities as well as structural graph constraints.
    /// # Safety
    /// Installed capability packs must be trusted and obey the plugin ABI.
    pub unsafe fn explain_job(&self, request: &MediaRequest) -> Result<JobPlan> {
        match request {
            MediaRequest::Media(request) => {
                let session = unsafe { self.open(request.clone(), Cancellation::default())? };
                Ok(JobPlan {
                    graph: session.plan().graph.clone(),
                    notes: session.plan().notes.clone(),
                })
            }
            MediaRequest::Image(request) => unsafe { request.resolved_plan(self) },
        }
    }
    /// # Safety
    /// The system image plugin must be trusted and obey the plugin ABI.
    pub unsafe fn image_capabilities(&self) -> Result<serde_json::Value> {
        let spec = GraphSpec {
            nodes: vec![graph::NodeSpec {
                id: "capabilities".into(),
                config: NodeConfig {
                    component: format!("{}.image.capabilities", std::env::consts::OS),
                    settings: json!({}),
                },
            }],
            edges: vec![],
        };
        let mut graph = unsafe { self.graph(&spec, Cancellation::default())? };
        let start = Instant::now();
        loop {
            ensure!(
                start.elapsed() < Duration::from_secs(30),
                "capability query timed out"
            );
            match graph.poll()? {
                GraphEvent::Output { buffer, .. } => {
                    return Ok(serde_json::from_slice(buffer.bytes())?);
                }
                GraphEvent::End => anyhow::bail!("missing image capabilities"),
                GraphEvent::Pending => {}
            }
        }
    }
    pub fn plan_job(&self, request: &MediaRequest) -> Result<JobPlan> {
        match request {
            MediaRequest::Media(request) => {
                let plan = self.plan(request)?;
                Ok(JobPlan {
                    graph: plan.graph,
                    notes: plan.notes,
                })
            }
            MediaRequest::Image(request) => request.plan(self),
        }
    }
    /// # Safety
    /// Installed capability packs must be trusted and obey the plugin ABI.
    pub unsafe fn open_job(
        &self,
        request: MediaRequest,
        cancel: Cancellation,
    ) -> Result<JobSession> {
        match request {
            MediaRequest::Media(request) => Ok(JobSession::Media(Box::new(unsafe {
                self.open(request, cancel)?
            }))),
            MediaRequest::Image(request) => {
                let plan = unsafe { request.resolved_plan(self)? };
                let input = request.input.buffers(request.max_input_bytes)?;
                let graph = bound_graph(
                    self,
                    &plan.graph,
                    BTreeMap::from([("input".into(), input)]),
                    cancel,
                )?;
                Ok(JobSession::Graph {
                    graph,
                    timeout: Duration::from_millis(request.timeout_ms),
                    started: Instant::now(),
                })
            }
        }
    }
}
pub(crate) fn bound_graph(
    engine: &Engine,
    spec: &GraphSpec,
    mut inputs: BTreeMap<String, Vec<MediaBuffer>>,
    cancel: Cancellation,
) -> Result<Graph> {
    cancel.begin();
    let mut nodes = Vec::new();
    for node in &spec.nodes {
        let component: Box<dyn Component> = if node.config.component == "core.buffer.source" {
            let frames = inputs
                .remove(&node.id)
                .context("missing graph input binding")?;
            ensure!(!frames.is_empty(), "empty graph input");
            let kind = match frames[0].header.info {
                FrameInfo::Image { .. } => DataKind::Image,
                FrameInfo::Audio { .. } => DataKind::Audio,
                FrameInfo::Video { .. } => DataKind::Video,
                FrameInfo::Packet { .. } => DataKind::Packet,
                _ => DataKind::Bytes,
            };
            for frame in &frames {
                frame.validate()?;
            }
            let bound = frames.iter().map(MediaBuffer::len).max().unwrap().max(1);
            Box::new(Input {
                info: NodeInfo {
                    input: None,
                    output: PortCaps::new(kind),
                    max_output_bytes: bound,
                    media: None,
                },
                frames: frames.into(),
            })
        } else {
            TrustedFactory(&engine.registry).create(&node.config, cancel.clone())?
        };
        nodes.push((node.id.clone(), component));
    }
    ensure!(inputs.is_empty(), "unused graph input binding");
    Graph::from_instances(nodes, &spec.edges, engine.budget.clone(), cancel)
}
struct Input {
    info: NodeInfo,
    frames: VecDeque<MediaBuffer>,
}
impl Component for Input {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn receive(&mut self) -> Result<Receive> {
        Ok(self
            .frames
            .pop_front()
            .map_or(Receive::End, Receive::Output))
    }
    fn drain(&mut self) -> Result<()> {
        Ok(())
    }
    fn reset(&mut self, _generation: u64, _target_us: i64) -> Result<()> {
        anyhow::bail!("bound input is single pass; create a new graph")
    }
}
pub(crate) struct SharedBytes(pub std::sync::Arc<[u8]>);
impl PlaneOwner for SharedBytes {
    fn plane(&self, i: usize) -> &[u8] {
        if i == 0 { &self.0 } else { &[] }
    }
}
