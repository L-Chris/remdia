//! Narrow Windows WebP fallback for systems without a WIC WebP codec.
#[cfg(any(windows, test))]
mod decoder;
use anyhow::Result;
use remdia_graph::*;
fn descriptors() -> Vec<ComponentDescriptor> {
    if !cfg!(windows) {
        return Vec::new();
    }
    ["decode", "probe"]
        .into_iter()
        .map(|op| ComponentDescriptor {
            id: format!("fallback.webp.{op}"),
            role: if op == "decode" { "decoder" } else { "probe" }.into(),
            input: Some(PortCaps::new(DataKind::Bytes)),
            output: PortCaps::new(if op == "decode" {
                DataKind::Image
            } else {
                DataKind::Bytes
            }),
        })
        .collect()
}
fn create(config: &NodeConfig, control: Control) -> Result<Box<dyn Component>> {
    #[cfg(windows)]
    return Ok(Box::new(decoder::Decoder::new(config, control)?));
    #[cfg(not(windows))]
    {
        let _ = (config, control);
        anyhow::bail!("WebP fallback is packaged only on Windows")
    }
}
remdia_graph::export_plugin!(descriptors, create);
