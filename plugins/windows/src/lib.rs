//! Windows Media Foundation video decoder capability pack, with no FFmpeg.
#[cfg(windows)]
mod decoder;
use anyhow::Result;
use remdia_graph::*;
fn descriptors() -> Vec<ComponentDescriptor> {
    #[cfg(windows)]
    return vec![ComponentDescriptor {
        id: "windows.video.decode".into(),
        role: "decoder".into(),
        input: Some(PortCaps::new(DataKind::Packet)),
        output: PortCaps::new(DataKind::Video),
    }];
    #[cfg(not(windows))]
    Vec::new()
}
fn create(config: &NodeConfig, control: Control) -> Result<Box<dyn Component>> {
    #[cfg(windows)]
    if config.component == "windows.video.decode" {
        return Ok(Box::new(decoder::Decoder::new(config, control)?));
    }
    let _ = (config, control);
    anyhow::bail!(
        "Windows video decoding requires Windows 10/11 and an installed compatible Media Foundation decoder"
    )
}
remdia_graph::export_plugin!(descriptors, create);
