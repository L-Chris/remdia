//! FFmpeg-free modular media SDK; the playback facade runs the same public DAG.
pub mod image;
mod jobs;
pub mod plan;
pub mod registry;
use anyhow::{Context, Result, ensure};
pub use jobs::{JobPlan, JobSession, MediaRequest};
use registry::{Registry, TrustedFactory};
pub use remdia_buffer::Budget;
use remdia_buffer::Reservation;
use remdia_graph::Factory;
pub use remdia_graph::{
    self as graph, BufferHeader, Cancellation, EdgeSpec, FrameInfo, Graph, GraphEvent, GraphSpec,
    Interrupted, MediaBuffer, MediaInfo, NodeConfig, NodeSpec, ProviderPolicy, Rational, Request,
    Task,
};
use serde_json::json;
use std::{
    path::Path,
    time::{Duration, Instant},
};
#[derive(Clone)]
pub struct Frame(MediaBuffer);
impl Frame {
    pub fn info(&self) -> &FrameInfo {
        &self.0.header.info
    }
    pub fn generation(&self) -> u64 {
        self.0.header.generation
    }
    pub fn plane(&self, i: usize) -> Option<&[u8]> {
        (i < 4).then(|| self.0.plane(i))
    }
    pub fn bytes(&self) -> &[u8] {
        self.0.bytes()
    }
    pub fn buffer(&self) -> &MediaBuffer {
        &self.0
    }
}
pub struct Engine {
    registry: Registry,
    budget: Budget,
}
impl Engine {
    fn capability_value(
        &self,
        component: &str,
        settings: serde_json::Value,
        cancel: Cancellation,
    ) -> Result<serde_json::Value> {
        let spec = GraphSpec {
            nodes: vec![NodeSpec {
                id: "capabilities".into(),
                config: NodeConfig {
                    component: component.into(),
                    settings,
                },
            }],
            edges: vec![],
        };
        let mut graph = Graph::build(
            &spec,
            &TrustedFactory(&self.registry),
            self.budget.clone(),
            cancel,
        )?;
        let started = Instant::now();
        loop {
            ensure!(
                started.elapsed() < Duration::from_secs(30),
                "system capability query timed out"
            );
            match graph.poll()? {
                GraphEvent::Output { buffer, .. } => {
                    return Ok(serde_json::from_slice(buffer.bytes())?);
                }
                GraphEvent::End => anyhow::bail!("missing system capabilities"),
                GraphEvent::Pending => {}
            }
        }
    }
    /// Enumerate installed system audio factories through an ordinary source graph.
    /// # Safety
    /// Installed capability packs must be trusted and obey the plugin ABI.
    pub unsafe fn audio_capabilities(&self) -> Result<serde_json::Value> {
        self.capability_value(
            &format!("{}.audio.capabilities", std::env::consts::OS),
            json!({}),
            Cancellation::default(),
        )
    }
    /// # Safety
    /// Installed capability packs must be trusted and obey the plugin ABI.
    pub unsafe fn demux_capabilities(&self) -> Result<serde_json::Value> {
        self.capability_value(
            &format!("{}.demux.capabilities", std::env::consts::OS),
            json!({}),
            Cancellation::default(),
        )
    }
    /// Bind caller-owned buffers to explicit core.buffer.source nodes. All
    /// downstream nodes use the ordinary plugin factory and graph scheduler.
    /// # Safety
    /// Component libraries must be trusted and obey the negotiated plugin ABI.
    pub unsafe fn graph_with_inputs(
        &self,
        spec: &GraphSpec,
        inputs: std::collections::BTreeMap<String, Vec<MediaBuffer>>,
        cancel: Cancellation,
    ) -> Result<Graph> {
        jobs::bound_graph(self, spec, inputs, cancel)
    }
    pub fn discover(directory: impl AsRef<Path>, managed_budget_bytes: usize) -> Result<Self> {
        Ok(Self {
            registry: Registry::discover(directory)?,
            budget: Budget::new(managed_budget_bytes),
        })
    }
    pub fn encode_mp3(&self, samples: &[f32]) -> Result<Frame> {
        self.encode_mp3_with_policy(samples, ProviderPolicy::SystemPreferred)
    }
    pub fn encode_mp3_with_policy(&self, samples: &[f32], policy: ProviderPolicy) -> Result<Frame> {
        let directory = self
            .registry
            .candidates
            .first()
            .and_then(|c| c.path.parent())
            .context("no capability packs installed")?;
        encode_pcm_mp3(
            directory,
            &self.budget,
            Cancellation::default(),
            1,
            samples,
            policy,
        )
    }
    pub fn registry(&self) -> &Registry {
        &self.registry
    }
    pub fn budget(&self) -> &Budget {
        &self.budget
    }
    pub fn plan(&self, request: &Request) -> Result<plan::Plan> {
        plan::Plan::structure(&self.registry, request, self.budget.limit())
    }
    /// # Safety
    /// Every component DLL in the selected graph must be trusted and obey ABI v2.
    pub unsafe fn graph(&self, spec: &GraphSpec, cancel: Cancellation) -> Result<Graph> {
        cancel.begin();
        Graph::build(
            spec,
            &TrustedFactory(&self.registry),
            self.budget.clone(),
            cancel,
        )
    }
    /// # Safety
    /// Installed native component DLLs must be trusted and obey ABI v2.
    pub unsafe fn open(&self, request: Request, cancel: Cancellation) -> Result<Session> {
        self.plan(&request)?;
        cancel.begin();
        let factory = TrustedFactory(&self.registry);
        let platform = std::env::consts::OS;
        let caps_component = format!("{platform}.demux.capabilities");
        let demux_component = if self.registry.select(&caps_component).is_ok() {
            let caps = self.capability_value(
                &caps_component,
                serde_json::to_value(&request)?,
                cancel.clone(),
            )?;
            if caps["supported"] == true {
                format!("{platform}.demux")
            } else {
                ensure!(
                    request.policy != ProviderPolicy::SystemOnly,
                    "system demux unavailable: {}",
                    caps["reason"]
                );
                "fallback.demux".into()
            }
        } else {
            ensure!(
                request.policy != ProviderPolicy::SystemOnly,
                "system demux capability pack is not installed"
            );
            "fallback.demux".into()
        };
        let demux = factory.create(
            &NodeConfig {
                component: demux_component.clone(),
                settings: serde_json::to_value(&request)?,
            },
            cancel.clone(),
        )?;
        let info = demux
            .info()
            .media
            .clone()
            .context("demuxer did not provide stream configurations")?;
        let audio_component = format!("{platform}.audio.capabilities");
        let audio_caps = if matches!(request.task, Task::Playback | Task::Audio)
            && info.tracks.iter().any(|t| t.kind == "audio")
            && self.registry.select(&audio_component).is_ok()
        {
            self.capability_value(
                &audio_component,
                json!({"operation":"decode"}),
                cancel.clone(),
            )?
        } else {
            serde_json::Value::Null
        };
        let plan = plan::Plan::for_media(
            &request,
            &info,
            self.budget.limit(),
            &demux_component,
            &audio_caps,
        )?;
        let mut instances = vec![("demux".into(), demux)];
        for node in plan.graph.nodes.iter().skip(1) {
            instances.push((
                node.id.clone(),
                factory
                    .create(&node.config, cancel.clone())
                    .with_context(|| format!("configure {}", node.id))?,
            ));
        }
        let graph = Graph::from_instances(
            instances,
            &plan.graph.edges,
            self.budget.clone(),
            cancel.clone(),
        )?;
        Ok(Session {
            graph,
            info,
            plan,
            request,
            cancel,
            target_us: 0,
            at_end: false,
            budget: self.budget.clone(),
            registry_dir: self
                .registry
                .candidates
                .first()
                .and_then(|c| c.path.parent())
                .map(Path::to_path_buf)
                .context("missing registry directory")?,
        })
    }
}
fn encode_pcm_mp3(
    directory: &Path,
    budget: &Budget,
    cancel: Cancellation,
    generation: u64,
    samples: &[f32],
    policy: ProviderPolicy,
) -> Result<Frame> {
    ensure!(
        !samples.is_empty() && samples.len() <= 120 * 16000,
        "invalid MP3 chunk"
    );
    cancel.begin();
    let registry = Registry::discover(directory)?;
    let engine = Engine {
        registry,
        budget: budget.clone(),
    };
    let caps = engine.capability_value(
        &format!("{}.audio.capabilities", std::env::consts::OS),
        json!({"operation":"encode"}),
        cancel.clone(),
    )?;
    let available = cfg!(windows)
        && caps["mp3_gapless"] == true
        && caps["mp3_input_rates"]
            .as_array()
            .is_some_and(|rates| rates.iter().any(|rate| rate == 16000));
    let component = if available {
        "windows.mp3.encode"
    } else {
        ensure!(
            policy != ProviderPolicy::SystemOnly,
            "system MP3 encoder does not support the 16-kHz gapless request profile"
        );
        "fallback.mp3.encode"
    };
    let spec = GraphSpec {
        nodes: vec![
            NodeSpec {
                id: "input".into(),
                config: NodeConfig {
                    component: "core.buffer.source".into(),
                    settings: json!({}),
                },
            },
            NodeSpec {
                id: "encode".into(),
                config: NodeConfig {
                    component: component.into(),
                    settings: json!({"rate":16000,"channels":1}),
                },
            },
        ],
        edges: vec![EdgeSpec::between("input", "encode", None)],
    };
    let mut header = BufferHeader {
        stream_id: 0,
        generation,
        info: FrameInfo::Audio {
            pts_us: 0,
            rate: 16000,
            channels: 1,
            samples: samples.len(),
            format: "f32le".into(),
        },
    };
    let input = MediaBuffer::new(
        header.clone(),
        vec![samples.iter().flat_map(|s| s.to_le_bytes()).collect()],
    );
    // The encoding job owns its own graph generation; the returned frame keeps
    // the requesting session's generation after byte assembly.
    let mut input = input;
    input.header.generation = 1;
    let mut graph = jobs::bound_graph(
        &engine,
        &spec,
        std::collections::BTreeMap::from([("input".into(), vec![input])]),
        cancel,
    )?;
    let reservation = budget.reserve(16 * 1024 * 1024)?;
    let mut output = Vec::new();
    let mut iterations = 0;
    loop {
        iterations += 1;
        ensure!(iterations < 10000, "MP3 encoder failed to drain");
        match graph.poll()? {
            GraphEvent::Output { buffer: b, .. } => {
                let offset = match b.header.info {
                    FrameInfo::Bytes { offset, .. } => {
                        offset.unwrap_or(output.len() as u64) as usize
                    }
                    _ => output.len(),
                };
                let end = offset
                    .checked_add(b.len())
                    .context("encoded offset overflow")?;
                ensure!(end <= 16 * 1024 * 1024, "MP3 output exceeds budget");
                output.resize(output.len().max(end), 0);
                output[offset..end].copy_from_slice(b.bytes());
            }
            GraphEvent::End => break,
            GraphEvent::Pending => {}
        }
    }
    header.info = FrameInfo::Blob {
        format: "mp3".into(),
    };
    Ok(Frame(
        MediaBuffer::new(header, vec![output]).with_budget(reservation),
    ))
}
pub struct Session {
    graph: Graph,
    info: MediaInfo,
    plan: plan::Plan,
    request: Request,
    cancel: Cancellation,
    target_us: i64,
    at_end: bool,
    budget: Budget,
    registry_dir: std::path::PathBuf,
}
impl Session {
    pub fn info(&self) -> &MediaInfo {
        &self.info
    }
    pub fn plan(&self) -> &plan::Plan {
        &self.plan
    }
    pub fn cancellation(&self) -> Cancellation {
        self.cancel.clone()
    }
    pub fn generation(&self) -> u64 {
        self.graph.generation()
    }
    pub fn seek(&mut self, target_us: i64) -> Result<u64> {
        ensure!(target_us >= 0, "negative seek");
        let at_end = self
            .info
            .duration_us
            .is_some_and(|d| d > 0 && target_us >= d);
        let seek_to = if at_end {
            self.info.duration_us.unwrap().saturating_sub(1)
        } else {
            target_us
        };
        let generation = self.graph.reset(seek_to)?;
        self.target_us = target_us;
        self.at_end = at_end;
        Ok(generation)
    }
    pub fn read(&mut self) -> Result<Option<Frame>> {
        ensure!(
            self.request.task != Task::Probe,
            "probe session does not decode"
        );
        self.cancel.begin();
        if self.at_end {
            return Ok(None);
        }
        let started = Instant::now();
        loop {
            ensure!(
                started.elapsed() < Duration::from_millis(self.request.timeout_ms),
                "media graph timed out"
            );
            match self.graph.poll()? {
                GraphEvent::End => return Ok(None),
                GraphEvent::Pending => {}
                GraphEvent::Output { mut buffer, .. } => {
                    match buffer.header.info.clone() {
                        FrameInfo::Packet { .. } => continue,
                        FrameInfo::Video { pts_us, .. } | FrameInfo::Image { pts_us, .. } => {
                            if pts_us.is_some_and(|p| p - self.info.origin_us < self.target_us) {
                                continue;
                            }
                            if let FrameInfo::Video { pts_us, .. } | FrameInfo::Image { pts_us, .. } =
                                &mut buffer.header.info
                                && let Some(p) = pts_us
                            {
                                *p -= self.info.origin_us;
                            }
                        }
                        FrameInfo::Audio {
                            pts_us,
                            rate,
                            channels,
                            samples,
                            ..
                        } => {
                            let pts = pts_us - self.info.origin_us;
                            let skip =
                                (((self.target_us - pts).max(0) as i128 * rate as i128 + 999999)
                                    / 1000000)
                                    .min(samples as i128) as usize;
                            if skip == samples {
                                continue;
                            }
                            let mut header = buffer.header.clone();
                            if let FrameInfo::Audio {
                                pts_us, samples, ..
                            } = &mut header.info
                            {
                                *pts_us = pts + skip as i64 * 1_000_000 / rate as i64;
                                *samples -= skip;
                            }
                            if skip > 0 {
                                let stride = channels as usize * 4;
                                let bytes = buffer.bytes()[skip * stride..].to_vec();
                                let reservation = self.budget.reserve(bytes.len())?;
                                buffer =
                                    MediaBuffer::new(header, vec![bytes]).with_budget(reservation);
                            } else {
                                buffer.header = header;
                            }
                        }
                        _ => {}
                    }
                    return Ok(Some(Frame(buffer)));
                }
            }
        }
    }
    pub fn artwork(&mut self) -> Result<Option<Frame>> {
        let reservation = self.budget.reserve(8 * 1024 * 1024)?;
        let b = self.graph.command("demux", &json!({"op":"artwork"}))?;
        b.map(|b| {
            ensure!(b.len() <= 8 * 1024 * 1024, "artwork exceeds budget");
            Ok(Frame(b.with_budget(reservation)))
        })
        .transpose()
    }
    pub fn audio_chunk(&mut self, start_us: i64, duration_us: i64) -> Result<AudioChunk> {
        ensure!(
            self.request.task == Task::Audio
                && self.request.audio_rate == 16000
                && self.request.audio_channels == 1,
            "audio chunks require a 16kHz mono Audio session"
        );
        ensure!(
            (1..=120_000_000).contains(&duration_us),
            "chunk duration must be <=120 seconds"
        );
        let end = start_us
            .checked_add(duration_us)
            .context("range overflow")?;
        self.seek(start_us)?;
        let capacity = usize::try_from((i128::from(duration_us) * 16000 + 999999) / 1_000_000)?;
        let reservation = self.budget.reserve(capacity * 4)?;
        let mut samples = Vec::with_capacity(capacity);
        while let Some(frame) = self.read()? {
            let FrameInfo::Audio { pts_us, .. } = *frame.info() else {
                continue;
            };
            if pts_us >= end {
                break;
            }
            let offset = (((i128::from(pts_us) - i128::from(start_us)) * 16000 + 500000)
                / 1_000_000)
                .max(0) as usize;
            if offset > samples.len() {
                samples.resize(offset.min(capacity), 0.0);
            }
            let skip = samples.len().saturating_sub(offset);
            for v in frame
                .bytes()
                .as_chunks::<4>()
                .0
                .iter()
                .skip(skip)
                .take(capacity - samples.len())
            {
                samples.push(f32::from_le_bytes(*v));
            }
            if samples.len() == capacity {
                break;
            }
        }
        Ok(AudioChunk {
            start_us,
            samples,
            _reservation: reservation,
        })
    }
    pub fn encode_mp3(&self, chunk: &AudioChunk) -> Result<Frame> {
        encode_pcm_mp3(
            &self.registry_dir,
            &self.budget,
            self.cancel.clone(),
            self.generation(),
            &chunk.samples,
            self.request.policy,
        )
    }
}
pub struct AudioChunk {
    pub start_us: i64,
    pub samples: Vec<f32>,
    _reservation: Reservation,
}
impl AudioChunk {
    pub fn wav(&self) -> Vec<u8> {
        let bytes = (self.samples.len() * 2) as u32;
        let mut out = Vec::with_capacity(bytes as usize + 44);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + bytes).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&16000u32.to_le_bytes());
        out.extend_from_slice(&32000u32.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&bytes.to_le_bytes());
        for s in &self.samples {
            out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
        }
        out
    }
}

#[cfg(all(test, any(windows, target_os = "macos")))]
mod system_audio_tests;

#[cfg(all(test, any(windows, target_os = "macos")))]
mod system_demux_tests;
