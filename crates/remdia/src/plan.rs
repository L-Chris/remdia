use crate::{ProviderPolicy, Request, Task, registry::Registry};
use anyhow::{Result, ensure};
use remdia_graph::*;
use serde::Serialize;
use serde_json::json;
#[derive(Debug, Serialize)]
pub struct Plan {
    pub task: Task,
    pub graph: GraphSpec,
    pub managed_budget_bytes: usize,
    pub notes: Vec<String>,
}
impl Plan {
    pub fn structure(registry: &Registry, request: &Request, budget: usize) -> Result<Self> {
        request.validate()?;
        ensure!(
            budget >= request.max_frame_bytes.max(request.max_packet_bytes),
            "budget below maximum output reservation"
        );
        let system = format!("{}.demux", std::env::consts::OS);
        let demux = if registry.select(&system).is_ok() {
            system
        } else {
            ensure!(
                request.policy != ProviderPolicy::SystemOnly,
                "system demux capability pack is not installed"
            );
            "fallback.demux".into()
        };
        registry.select(&demux)?;
        Ok(Self {task:request.task,graph:GraphSpec {nodes:vec![NodeSpec {id:"demux".into(),config:NodeConfig {component:demux,settings:serde_json::to_value(request)?}}],edges:Vec::new()},managed_budget_bytes:budget,
   notes:vec!["Structural probe plan; open/explain discovers tracks and builds decoder/filter branches".into(),"No FFmpeg runtime, build or codec dependency".into(),"Managed CPU buffer ledger excludes codec/driver private memory".into()]})
    }
    pub(crate) fn for_media(
        request: &Request,
        media: &MediaInfo,
        budget: usize,
        demux: &str,
        audio_caps: &serde_json::Value,
    ) -> Result<Self> {
        let mut nodes = vec![NodeSpec {
            id: "demux".into(),
            config: NodeConfig {
                component: demux.into(),
                settings: serde_json::to_value(request)?,
            },
        }];
        let mut edges = Vec::new();
        if request.task != Task::Probe {
            let mut selected = Vec::new();
            if matches!(request.task, Task::Playback | Task::Thumbnail)
                && let Some(track) = media
                    .tracks
                    .iter()
                    .find(|t| t.kind == "video" && !t.attached_picture)
            {
                selected.push(track);
            }
            if matches!(request.task, Task::Playback | Task::Audio)
                && let Some(track) = media.tracks.iter().find(|t| t.kind == "audio")
            {
                selected.push(track);
            }
            ensure!(!selected.is_empty(), "no matching audio or video track");
            for track in selected {
                let id = format!("decode_{}", track.index);
                let component = if track.kind == "video" {
                    match std::env::consts::OS {
                        "windows" => "windows.video.decode".to_owned(),
                        "macos" => "macos.video.decode".to_owned(),
                        os => anyhow::bail!("no default video decoder for {os}"),
                    }
                } else {
                    let available = audio_caps["decode"]
                        .as_array()
                        .is_some_and(|list| list.iter().any(|codec| codec == &track.codec));
                    ensure!(
                        available,
                        "system audio decoder unavailable for {}; software decoding is not enabled",
                        track.codec
                    );
                    format!("{}.audio.decode", std::env::consts::OS)
                };
                nodes.push(NodeSpec {id:id.clone(),config:NodeConfig {component,settings:json!({"track":track,"max_frame_bytes":request.max_frame_bytes,"hardware":request.hardware})}});
                edges.push(EdgeSpec::between(
                    "demux",
                    id.clone(),
                    Some(track.index as u32),
                ));
                if track.kind == "audio" {
                    let filter = format!("audio_{}", track.index);
                    ensure!(
                        audio_caps["resample"] == true,
                        "system resampler capability pack is unavailable"
                    );
                    let resample = format!("{}.audio.resample", std::env::consts::OS);
                    nodes.push(NodeSpec {id:filter.clone(),config:NodeConfig {component:resample,settings:json!({"rate":request.audio_rate,"channels":request.audio_channels})}});
                    edges.push(EdgeSpec::between(id, filter, None));
                } else if request.task == Task::Thumbnail {
                    nodes.push(NodeSpec {
                        id: "thumbnail_pixels".into(),
                        config: NodeConfig {
                            component: "native.video.rgba".into(),
                            settings: json!({}),
                        },
                    });
                    edges.push(EdgeSpec::between(id, "thumbnail_pixels", None));
                    let platform = std::env::consts::OS;
                    nodes.push(NodeSpec {
                        id: "thumbnail_image".into(),
                        config: NodeConfig {
                            component: format!("{platform}.image.from_video"),
                            settings: json!({}),
                        },
                    });
                    nodes.push(NodeSpec {
                        id: "thumbnail".into(),
                        config: NodeConfig {
                            component: format!("{platform}.image.scale"),
                            settings: json!({"width":600,"height":600,"fit":true}),
                        },
                    });
                    edges.push(EdgeSpec::between(
                        "thumbnail_pixels",
                        "thumbnail_image",
                        None,
                    ));
                    edges.push(EdgeSpec::between("thumbnail_image", "thumbnail", None));
                }
            }
        }
        Ok(Self {task:request.task,graph:GraphSpec {nodes,edges},managed_budget_bytes:budget,notes:vec!["Independent demux, decoder and filter factories; compatible nodes can come from different DLLs".into(),"Per-stream routing; compressed packets are not forced through a decoder".into()]})
    }
}
