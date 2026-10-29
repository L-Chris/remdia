//! MP3 encoding fallback for system encoder/profile gaps.
#[cfg(target_os = "macos")]
mod encode;
use anyhow::Result;
use remdia_graph::*;
fn descriptors() -> Vec<ComponentDescriptor> {
    if !cfg!(target_os = "macos") {
        return vec![];
    }
    vec![ComponentDescriptor {
        id: "fallback.mp3.encode".into(),
        role: "encoder".into(),
        input: Some(PortCaps::new(DataKind::Audio)),
        output: PortCaps::new(DataKind::Bytes),
    }]
}
fn create(config: &NodeConfig, control: Control) -> Result<Box<dyn Component>> {
    let _ = control;
    #[cfg(target_os = "macos")]
    if config.component == "fallback.mp3.encode" {
        return Ok(Box::new(encode::Mp3::new(config)?));
    }
    let _ = config;
    anyhow::bail!("unsupported MP3 encoder fallback")
}
remdia_graph::export_plugin!(descriptors, create);
