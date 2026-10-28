//! VideoToolbox packet decoder. Only this capability pack links Apple frameworks.
#[cfg(any(target_os = "macos", test))]
mod bitstream;
#[cfg(target_os = "macos")]
mod decoder;
use anyhow::Result;
use remdia_graph::*;
fn descriptors() -> Vec<ComponentDescriptor> {
    #[cfg(target_os = "macos")]
    return vec![ComponentDescriptor {
        id: "macos.video.decode".into(),
        role: "decoder".into(),
        input: Some(PortCaps::new(DataKind::Packet)),
        output: PortCaps::new(DataKind::Video),
    }];
    #[cfg(not(target_os = "macos"))]
    Vec::new()
}
fn create(config: &NodeConfig, control: Control) -> Result<Box<dyn Component>> {
    #[cfg(target_os = "macos")]
    if config.component == "macos.video.decode" {
        return Ok(Box::new(decoder::Decoder::new(config, control)?));
    }
    let _ = (config, control);
    anyhow::bail!("macOS video decoding requires macOS and a compatible VideoToolbox decoder")
}
remdia_graph::export_plugin!(descriptors, create);
