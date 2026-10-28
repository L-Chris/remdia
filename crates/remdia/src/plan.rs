use crate::{Request, Task, registry::Registry};
use anyhow::{Result, ensure};
use serde::Serialize;
#[derive(Debug, Serialize)]
pub struct Plan {
    pub plugin: String,
    pub task: Task,
    pub nodes: Vec<String>,
    pub managed_budget_bytes: usize,
    pub output_reservation_bytes: usize,
    pub threads: u32,
    pub memory_domain: String,
    pub notes: Vec<String>,
}
impl Plan {
    pub fn build(registry: &Registry, request: &Request, budget: usize) -> Result<Self> {
        request.validate()?;
        let capability = match request.task {
            Task::Probe => "probe",
            Task::Playback => "decode",
            Task::Audio => "audio",
            Task::Thumbnail => "thumbnail",
        };
        let plugin = registry.select(capability)?;
        let output_reservation_bytes = if request.task == Task::Probe {
            8 * 1024 * 1024
        } else {
            request.max_frame_bytes
        };
        ensure!(
            budget >= output_reservation_bytes,
            "budget below minimum output reservation"
        );
        let nodes = match request.task {
            Task::Probe => vec!["source", "demux headers / tags"],
            Task::Playback => vec![
                "source",
                "demux",
                "selected audio/video decoders",
                "native CPU video / f32 PCM",
            ],
            Task::Audio => vec![
                "source",
                "demux audio only",
                "audio decoder",
                "resample / remix f32",
            ],
            Task::Thumbnail => vec![
                "source",
                "demux video only",
                "video decoder",
                "scale RGBA <= 600px",
            ],
        }
        .into_iter()
        .map(String::from)
        .collect();
        Ok(Self {
            plugin: plugin.manifest.id.clone(),
            task: request.task,
            nodes,
            managed_budget_bytes: budget,
            output_reservation_bytes,
            threads: request.threads,
            memory_domain: "cpu".into(),
            notes: vec![
                "Demand-driven: no background decoded-frame queue".into(),
                "Codec/driver private allocations are outside the managed budget".into(),
                if request.hardware {
                    "Windows attempts D3D11VA at decoder creation; output is downloaded to CPU. No GPU zero-copy guarantee.".into()
                } else {
                    "Software decoding requested".into()
                },
            ],
        })
    }
}
