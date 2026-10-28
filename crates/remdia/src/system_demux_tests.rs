#![cfg(any(windows, target_os = "macos"))]
use crate::graph::{EdgeSpec, Factory, Graph, NodeConfig};
use crate::registry::TrustedFactory;
use crate::{Cancellation, Engine, FrameInfo, GraphEvent, Request};
use serde_json::json;
#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires staged system media plugins"]
fn compressed_system_demux_decodes_and_replays_after_eof() {
    let engine = Engine::discover(
        std::env::var_os("REMDIA_TEST_PLUGIN_DIR").unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    for (name, expected) in [
        ("av.mp4", 144000),
        ("audio.mp3", 132300),
        ("audio.opus", 144000),
        ("audio-vorbis.ogg", 132352),
        ("audio-alac.m4a", 132300),
        ("cover.flac", 132300),
        ("audio.wav", 132300),
    ] {
        let request = Request::playback(root.join(name).to_str().unwrap());
        let cancel = Cancellation::default();
        let factory = TrustedFactory(engine.registry());
        let demux = factory
            .create(
                &NodeConfig {
                    component: "macos.demux".into(),
                    settings: serde_json::to_value(&request).unwrap(),
                },
                cancel.clone(),
            )
            .unwrap();
        let media = demux.info().media.as_ref().unwrap();
        let track = media.tracks.iter().find(|t| t.kind == "audio").unwrap();
        let decode = factory
            .create(
                &NodeConfig {
                    component: "macos.audio.decode".into(),
                    settings: json!({"track":track}),
                },
                cancel.clone(),
            )
            .unwrap();
        let edge = EdgeSpec::between("demux", "decode", Some(track.index as u32));
        let mut graph = Graph::from_instances(
            vec![("demux".into(), demux), ("decode".into(), decode)],
            &[edge],
            engine.budget().clone(),
            cancel,
        )
        .unwrap();
        for generation in [1, 2] {
            let mut total = 0;
            let mut previous: Option<i64> = None;
            for attempts in 0..10000 {
                assert!(attempts < 9999, "{name} stalled");
                match graph.poll().unwrap() {
                    GraphEvent::End => break,
                    GraphEvent::Pending => {}
                    GraphEvent::Output { buffer, .. } => {
                        assert_eq!(buffer.header.generation, generation);
                        let FrameInfo::Audio {
                            pts_us,
                            samples,
                            rate,
                            ..
                        } = buffer.header.info
                        else {
                            panic!("expected audio")
                        };
                        if let Some(end) = previous {
                            assert!(
                                (pts_us - end).abs() <= 1,
                                "{name}: gap at {pts_us} after {end}"
                            );
                        } else {
                            assert_eq!(pts_us, 0, "{name}");
                        }
                        previous =
                            Some(pts_us + (samples as u128 * 1_000_000 / u128::from(rate)) as i64);
                        total += samples;
                    }
                }
            }
            assert_eq!(total, expected, "{name}");
            graph.reset(0).unwrap();
        }
    }
}

#[cfg(windows)]
#[test]
#[ignore = "requires staged Windows system media plugins"]
fn windows_source_reader_keeps_compressed_video_separate_from_decode() {
    let engine = Engine::discover(
        std::env::var_os("REMDIA_TEST_PLUGIN_DIR").unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();
    let source =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/av.mp4");
    let request = Request::playback(source.to_str().unwrap());
    let factory = TrustedFactory(engine.registry());
    let cancel = Cancellation::default();
    let demux = factory
        .create(
            &NodeConfig {
                component: "windows.demux".into(),
                settings: serde_json::to_value(&request).unwrap(),
            },
            cancel.clone(),
        )
        .unwrap();
    let track = demux
        .info()
        .media
        .as_ref()
        .unwrap()
        .tracks
        .iter()
        .find(|t| t.kind == "video")
        .unwrap();
    let decoder = factory
        .create(
            &NodeConfig {
                component: "windows.video.decode".into(),
                settings: json!({"track":track,"hardware":false,"max_frame_bytes":134217728}),
            },
            cancel.clone(),
        )
        .unwrap();
    let edge = EdgeSpec::between("demux", "video", Some(track.index as u32));
    let mut graph = Graph::from_instances(
        vec![("demux".into(), demux), ("video".into(), decoder)],
        &[edge],
        engine.budget().clone(),
        cancel,
    )
    .unwrap();
    for generation in [1, 2] {
        let mut count = 0;
        for attempts in 0..10000 {
            assert!(attempts < 9999, "compressed MP4 graph stalled");
            match graph.poll().unwrap() {
                GraphEvent::End => break,
                GraphEvent::Pending => {}
                GraphEvent::Output { buffer, .. } => {
                    assert_eq!(buffer.header.generation, generation);
                    assert!(matches!(buffer.header.info, FrameInfo::Video { .. }));
                    count += 1;
                }
            }
        }
        assert_eq!(count, 72);
        graph.reset(0).unwrap();
    }
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires staged system media plugins"]
fn system_only_playback_does_not_require_a_compatibility_pack() {
    let mut engine = Engine::discover(
        std::env::var_os("REMDIA_TEST_PLUGIN_DIR").unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();
    engine.registry.candidates.retain(|candidate| {
        candidate.manifest.id != "native" && !candidate.manifest.id.starts_with("fallback_")
    });
    let source =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/av.mp4");
    let mut request = Request::playback(source.to_str().unwrap());
    request.policy = crate::ProviderPolicy::SystemOnly;
    let mut session = unsafe { engine.open(request, Cancellation::default()).unwrap() };
    assert!(
        session
            .plan()
            .graph
            .nodes
            .iter()
            .all(|node| !node.config.component.starts_with("native."))
    );
    session.seek(1_250_000).unwrap();
    let mut audio = 0;
    let mut video = 0;
    while let Some(frame) = session.read().unwrap() {
        assert!(frame.info().pts_us().unwrap() >= 1_250_000);
        match frame.info() {
            FrameInfo::Audio { .. } => audio += 1,
            FrameInfo::Video { .. } => video += 1,
            _ => {}
        }
    }
    assert!(audio > 0 && video > 0);
    assert!(
        engine
            .encode_mp3_with_policy(&[0.1; 16000], crate::ProviderPolicy::SystemOnly)
            .is_err()
    );
}
