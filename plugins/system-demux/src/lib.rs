//! System demuxers emit compressed packets into the ordinary component graph.
#[cfg(target_os = "macos")]
mod apple;
mod capabilities;
#[cfg(any(windows, target_os = "macos"))]
mod flac;
#[cfg(any(windows, target_os = "macos"))]
mod metadata;
#[cfg(windows)]
mod windows;
use anyhow::Result;
use remdia_graph::*;
fn descriptors() -> Vec<ComponentDescriptor> {
    if !cfg!(any(windows, target_os = "macos")) {
        return Vec::new();
    }
    vec![
        ComponentDescriptor {
            id: format!("{}.demux.capabilities", std::env::consts::OS),
            role: "source".into(),
            input: None,
            output: PortCaps::new(DataKind::Bytes),
        },
        ComponentDescriptor {
            id: format!("{}.demux", std::env::consts::OS),
            role: "demux".into(),
            input: None,
            output: PortCaps::new(DataKind::Packet),
        },
    ]
}
fn create(config: &NodeConfig, control: Control) -> Result<Box<dyn Component>> {
    if config.component == format!("{}.demux.capabilities", std::env::consts::OS) {
        return Ok(Box::new(capabilities::Capabilities::new(config)?));
    }
    #[cfg(target_os = "macos")]
    if config.component == "macos.demux" {
        return Ok(Box::new(apple::Demux::open(config, control)?));
    }
    #[cfg(windows)]
    if config.component == "windows.demux" {
        return Ok(Box::new(windows::Demux::open(config, control)?));
    }
    let _ = (config, control);
    anyhow::bail!("unknown system demux component")
}
remdia_graph::export_plugin!(descriptors, create);
