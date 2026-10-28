//! Narrow Windows fallbacks selected only when the system decoder is unavailable.
#[cfg(windows)]
mod audio;
use anyhow::Result;
use remdia_graph::*;
fn descriptors() -> Vec<ComponentDescriptor> {
    if !cfg!(windows) {
        return vec![];
    }
    ["opus", "vorbis"]
        .into_iter()
        .map(|codec| ComponentDescriptor {
            id: format!("fallback.{codec}.decode"),
            role: "decoder".into(),
            input: Some(PortCaps::new(DataKind::Packet)),
            output: PortCaps::new(DataKind::Audio),
        })
        .collect()
}
fn create(config: &NodeConfig, control: Control) -> Result<Box<dyn Component>> {
    let _ = control;
    #[cfg(windows)]
    if matches!(
        config.component.as_str(),
        "fallback.opus.decode" | "fallback.vorbis.decode"
    ) {
        anyhow::ensure!(
            config.settings["track"]["codec"]
                .as_str()
                .is_some_and(|codec| config.component == format!("fallback.{codec}.decode")),
            "codec does not match narrow decoder"
        );
        return Ok(Box::new(audio::Decoder::new(config)?));
    }
    let _ = config;
    anyhow::bail!("unsupported fallback decoder")
}
remdia_graph::export_plugin!(descriptors, create);
