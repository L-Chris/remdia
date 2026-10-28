//! System demuxers emit compressed packets into the ordinary component graph.
#[cfg(target_os = "macos")]
mod apple;
use anyhow::Result;
use remdia_graph::*;
fn descriptors() -> Vec<ComponentDescriptor> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    vec![ComponentDescriptor {
        id: "macos.demux".into(),
        role: "demux".into(),
        input: None,
        output: PortCaps::new(DataKind::Packet),
    }]
}
fn create(config: &NodeConfig, control: Control) -> Result<Box<dyn Component>> {
    #[cfg(target_os = "macos")]
    if config.component == "macos.demux" {
        return Ok(Box::new(apple::Demux::open(config, control)?));
    }
    let _ = (config, control);
    anyhow::bail!("unknown system demux component")
}
remdia_graph::export_plugin!(descriptors, create);
