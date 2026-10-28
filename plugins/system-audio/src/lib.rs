//! Independent system audio components; no bundled audio codec implementations.
#[cfg(target_os = "macos")]
mod apple_audio;
#[cfg(any(target_os = "macos", test))]
mod apple_config;
#[cfg(target_os = "macos")]
mod apple_pcm;
#[cfg(target_os = "macos")]
mod apple_resample;
#[cfg(target_os = "macos")]
mod audio_toolbox_sys;
#[cfg(windows)]
mod windows_audio;
#[cfg(windows)]
mod windows_resample;
use anyhow::Result;
use remdia_graph::*;
fn descriptors() -> Vec<ComponentDescriptor> {
    if !cfg!(any(windows, target_os = "macos")) {
        return vec![];
    }
    let mut result = vec![ComponentDescriptor {
        id: format!("{}.audio.decode", std::env::consts::OS),
        role: "decoder".into(),
        input: Some(PortCaps::new(DataKind::Packet)),
        output: PortCaps::new(DataKind::Audio),
    }];
    #[cfg(any(windows, target_os = "macos"))]
    result.push(ComponentDescriptor {
        id: format!("{}.audio.resample", std::env::consts::OS),
        role: "filter".into(),
        input: Some(PortCaps::new(DataKind::Audio)),
        output: PortCaps::new(DataKind::Audio),
    });
    result
}
fn create(config: &NodeConfig, control: Control) -> Result<Box<dyn Component>> {
    let _ = control;
    #[cfg(target_os = "macos")]
    if config.component == "macos.audio.resample" {
        return Ok(Box::new(apple_resample::Resample::new(config, control)?));
    }
    #[cfg(target_os = "macos")]
    if config.component == "macos.audio.decode" {
        if config.settings["track"]["codec"]
            .as_str()
            .is_some_and(|codec| codec.starts_with("pcm_"))
        {
            return Ok(Box::new(apple_pcm::Pcm::new(config)?));
        }
        return Ok(Box::new(apple_audio::Decoder::new(config)?));
    }
    #[cfg(windows)]
    if config.component == "windows.audio.resample" {
        return Ok(Box::new(windows_resample::Resample::new(config, control)?));
    }
    #[cfg(windows)]
    if config.component == "windows.audio.decode" {
        return Ok(Box::new(windows_audio::Decoder::new(config)?));
    }
    let _ = config;
    anyhow::bail!("unknown system audio component")
}
remdia_graph::export_plugin!(descriptors, create);
