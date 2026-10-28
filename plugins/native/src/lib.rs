//! Self-contained buffer filters and simple mux/sink components. No codec libraries.
mod filters;
mod gapless;
mod mux;
use anyhow::{Result, bail};
use remdia_graph::*;
fn descriptors() -> Vec<ComponentDescriptor> {
    [
        (
            "native.audio.gain",
            "filter",
            Some(DataKind::Audio),
            DataKind::Audio,
        ),
        (
            "native.video.rgba",
            "filter",
            Some(DataKind::Video),
            DataKind::Video,
        ),
        (
            "native.wav.mux",
            "mux",
            Some(DataKind::Audio),
            DataKind::Bytes,
        ),
        (
            "native.packet.copy",
            "bitstream",
            Some(DataKind::Packet),
            DataKind::Packet,
        ),
        (
            "native.mp3.mux",
            "mux",
            Some(DataKind::Packet),
            DataKind::Bytes,
        ),
        (
            "native.mp3.gapless_mux",
            "mux",
            Some(DataKind::Bytes),
            DataKind::Bytes,
        ),
        (
            "native.file.sink",
            "sink",
            Some(DataKind::Bytes),
            DataKind::Bytes,
        ),
    ]
    .into_iter()
    .map(|(id, role, input, output)| ComponentDescriptor {
        id: id.into(),
        role: role.into(),
        input: input.map(PortCaps::new),
        output: PortCaps::new(output),
    })
    .collect()
}
fn create(config: &NodeConfig, control: Control) -> Result<Box<dyn Component>> {
    let _ = control;
    Ok(match config.component.as_str() {
        "native.audio.gain" => Box::new(filters::Gain::new(config)?),
        "native.video.rgba" => Box::new(filters::Rgba::new(config)?),
        "native.wav.mux" => Box::new(mux::Wav::new(config)?),
        "native.packet.copy" => Box::new(filters::Copy::new()),
        "native.mp3.mux" => Box::new(mux::PacketMux::new(config)?),
        "native.mp3.gapless_mux" => Box::new(gapless::Mux::new(config)?),
        "native.file.sink" => Box::new(mux::FileSink::new(config)?),
        other => bail!("unknown native component {other}"),
    })
}
remdia_graph::export_plugin!(descriptors, create);
