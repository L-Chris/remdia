//! Compatibility demux/source pack. It does not register or create audio decoders.
mod demux;
mod source;
use anyhow::Result;
use remdia_graph::*;
fn descriptors() -> Vec<ComponentDescriptor> {
    vec![ComponentDescriptor {
        id: "fallback.demux".into(),
        role: "demux".into(),
        input: None,
        output: PortCaps::new(DataKind::Packet),
    }]
}
fn create(config: &NodeConfig, control: Control) -> Result<Box<dyn Component>> {
    anyhow::ensure!(
        config.component == "fallback.demux",
        "unknown fallback demux component"
    );
    Ok(Box::new(demux::Demux::open(config, control)?))
}
remdia_graph::export_plugin!(descriptors, create);
