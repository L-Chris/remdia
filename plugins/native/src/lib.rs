//! FFmpeg-free demux, audio codecs, filters, encoders and muxers.
#[cfg(target_os = "macos")]
mod apple_audio;
#[cfg(any(target_os = "macos", test))]
mod apple_config;
mod audio;
#[cfg(target_os = "macos")]
mod audio_toolbox_sys;
mod demux;
mod filters;
mod mux;
mod source;
#[cfg(windows)]
mod windows_audio;
use anyhow::{Result, bail};
use remdia_graph::*;
fn descriptors() -> Vec<ComponentDescriptor> {
    [
        ("native.demux", "demux", None, DataKind::Packet),
        (
            "native.audio.decode",
            "decoder",
            Some(DataKind::Packet),
            DataKind::Audio,
        ),
        (
            "native.audio.resample",
            "filter",
            Some(DataKind::Audio),
            DataKind::Audio,
        ),
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
            "native.mp3.encode",
            "encoder",
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
    Ok(match config.component.as_str() {
        "native.demux" => Box::new(demux::Demux::open(config, control)?),
        "native.audio.decode" => audio::create(config)?,
        "native.audio.resample" => Box::new(filters::Resample::new(config)?),
        "native.audio.gain" => Box::new(filters::Gain::new(config)?),
        "native.video.rgba" => Box::new(filters::Rgba::new(config)?),
        "native.wav.mux" => Box::new(mux::Wav::new(config)?),
        "native.mp3.encode" => Box::new(mux::Mp3::new(config)?),
        "native.packet.copy" => Box::new(filters::Copy::new()),
        "native.mp3.mux" => Box::new(mux::PacketMux::new(config)?),
        "native.file.sink" => Box::new(mux::FileSink::new(config)?),
        other => bail!("unknown native component {other}"),
    })
}
remdia_graph::export_plugin!(descriptors, create);
