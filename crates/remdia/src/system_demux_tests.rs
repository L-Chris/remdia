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

#[test]
#[ignore = "requires staged system media plugins"]
fn system_file_source_uses_content_instead_of_temporary_file_extension() {
    let engine = Engine::discover(
        std::env::var_os("REMDIA_TEST_PLUGIN_DIR").unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let directory = root.join("../../target/system-tests");
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join(format!("system-source-{}.part", std::process::id()));
    std::fs::copy(root.join("../../tests/fixtures/audio.wav"), &source).unwrap();
    let mut request = Request::playback(source.to_str().unwrap());
    request.policy = crate::ProviderPolicy::SystemOnly;
    request.task = crate::Task::Audio;
    let mut session = unsafe { engine.open(request, Cancellation::default()).unwrap() };
    assert!(matches!(
        session.read().unwrap().unwrap().info(),
        FrameInfo::Audio { .. }
    ));
    drop(session);
    std::fs::remove_file(source).unwrap();
}

#[test]
#[ignore = "requires staged system media plugins"]
fn system_lossless_source_seeks_to_last_partial_block_and_replays() {
    let engine = Engine::discover(
        std::env::var_os("REMDIA_TEST_PLUGIN_DIR").unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut request = Request::playback(root.join("audio-tail.flac").to_str().unwrap());
    request.task = crate::Task::Audio;
    request.policy = crate::ProviderPolicy::SystemOnly;
    for pre_read in [false, true, false] {
        let mut session = unsafe { engine.open(request.clone(), Cancellation::default()) }.unwrap();
        eprintln!("lossless source, pre_read={pre_read}: {:?}", session.info());
        if pre_read {
            assert!(session.read().unwrap().is_some());
        }
        for target in [3_600_000, 200_000, 3_999_000, 3_600_000, 0] {
            session.seek(target).unwrap();
            let mut count = 0usize;
            let mut first = None;
            let mut last = target;
            while let Some(frame) = session.read().unwrap() {
                if let FrameInfo::Audio {
                    samples,
                    pts_us,
                    rate,
                    ..
                } = *frame.info()
                {
                    first.get_or_insert(pts_us);
                    assert!(pts_us >= last - 1 && pts_us <= 4_000_000);
                    count += samples;
                    last = pts_us + (samples as u64 * 1_000_000 / u64::from(rate)) as i64;
                }
            }
            eprintln!("seek={target} first={first:?} end={last} samples={count}");
            let expected = ((4_000_000 - target) as u64 * 48000 / 1_000_000) as usize;
            assert!(
                count.abs_diff(expected) <= 1,
                "seek={target} samples={count}, expected={expected}"
            );
            assert!(last.abs_diff(4_000_000) <= 21, "seek={target} end={last}");
        }
    }
}

#[test]
#[ignore = "requires staged plugins and generated long FLAC fixtures"]
fn system_long_flac_seek_preserves_samples_and_rapid_reset_generations() {
    let engine = Engine::discover(
        std::env::var_os("REMDIA_TEST_PLUGIN_DIR").unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();
    let root = std::path::PathBuf::from(std::env::var_os("REMDIA_TEST_FIXTURES").unwrap());
    for (name, blocks) in [
        ("flac-long-1024.flac", &[1024u64][..]),
        ("flac-long-4096.flac", &[4096][..]),
        ("flac-long-table.flac", &[1024][..]),
        ("flac-long-variable.flac", &[1024, 4096, 2048][..]),
    ] {
        let mut request = Request::playback(root.join(name).to_str().unwrap());
        request.task = crate::Task::Audio;
        request.policy = crate::ProviderPolicy::SystemOnly;
        request.audio_rate = 48000;
        request.audio_channels = 1;
        let mut session = unsafe { engine.open(request, Cancellation::default()).unwrap() };
        assert_eq!(session.info().duration_us, Some(1_800_000_000));
        // Supersede two seeks before the very first input, then repeat after EOF.
        for _ in 0..3 {
            session.seek(0).unwrap();
            session.seek(900_000_000).unwrap();
            let started = std::time::Instant::now();
            let generation = session.seek(1_798_000_000).unwrap();
            let mut count = 0usize;
            while let Some(frame) = session.read().unwrap() {
                assert_eq!(frame.generation(), generation);
                let FrameInfo::Audio {
                    pts_us,
                    samples,
                    rate,
                    ..
                } = *frame.info()
                else {
                    panic!("expected audio")
                };
                assert_eq!(rate, 48000);
                assert!(
                    (pts_us - 1_798_000_000 - count as i64 * 1_000_000 / 48000).abs() <= 21,
                    "{name}: pts={pts_us}, emitted={count}"
                );
                if count == 0 {
                    let sample = 1798 * 48000;
                    let cycle = blocks.iter().sum::<u64>();
                    let mut first = sample / cycle * cycle;
                    for size in blocks {
                        if sample < first + size {
                            break;
                        }
                        first += size;
                    }
                    let expected = ((first / 1024) % 30000) as f32 - 15000.;
                    let actual = f32::from_le_bytes(frame.bytes()[..4].try_into().unwrap());
                    assert!(
                        (actual - expected / 32768.).abs() < 1. / 32768.,
                        "{name}: wrong seek sample {actual}, expected {}",
                        expected / 32768.
                    );
                    eprintln!(
                        "{name}: seek-to-first-PCM={:.3}ms",
                        started.elapsed().as_secs_f64() * 1000.
                    );
                }
                count += samples;
            }
            assert!(count.abs_diff(96000) <= 1, "{name}: tail samples={count}");
        }
        session.seek(1_800_000_000).unwrap();
        assert!(session.read().unwrap().is_none());
        session.seek(1_900_000_000).unwrap();
        assert!(session.read().unwrap().is_none());
        let generation = session.seek(0).unwrap();
        let frame = session.read().unwrap().expect("replay after EOF");
        assert_eq!(frame.generation(), generation);
        assert_eq!(frame.info().pts_us(), Some(0));
    }
}

#[test]
#[ignore = "requires staged plugins and generated long FLAC fixtures"]
fn system_flac_seek_decodes_only_target_packets() {
    use crate::graph::{Component, MediaBuffer, NodeInfo, Receive, Submit};
    use std::{cell::RefCell, rc::Rc};
    #[derive(Default)]
    struct Stats {
        packets: usize,
        bytes: usize,
        container: serde_json::Value,
    }
    struct Counted {
        inner: Box<dyn Component>,
        stats: Rc<RefCell<Stats>>,
    }
    impl Component for Counted {
        fn info(&self) -> &NodeInfo {
            self.inner.info()
        }
        fn receive(&mut self) -> anyhow::Result<Receive> {
            let result = self.inner.receive()?;
            if let Some(buffer) = self.inner.command(&json!({"op":"flac.stats"}))? {
                self.stats.borrow_mut().container = serde_json::from_slice(buffer.bytes())?;
            }
            Ok(result)
        }
        fn submit(&mut self, buffer: &MediaBuffer) -> anyhow::Result<Submit> {
            let result = self.inner.submit(buffer)?;
            if result == Submit::Accepted {
                let mut stats = self.stats.borrow_mut();
                stats.packets += 1;
                stats.bytes += buffer.bytes().len();
            }
            Ok(result)
        }
        fn drain(&mut self) -> anyhow::Result<()> {
            self.inner.drain()
        }
        fn reset(&mut self, generation: u64, target: i64) -> anyhow::Result<()> {
            *self.stats.borrow_mut() = Stats::default();
            self.inner.reset(generation, target)
        }
        fn command(&mut self, command: &serde_json::Value) -> anyhow::Result<Option<MediaBuffer>> {
            self.inner.command(command)
        }
    }
    let engine = Engine::discover(
        std::env::var_os("REMDIA_TEST_PLUGIN_DIR").unwrap(),
        128 * 1024 * 1024,
    )
    .unwrap();
    let root = std::path::PathBuf::from(std::env::var_os("REMDIA_TEST_FIXTURES").unwrap());
    for name in [
        "flac-long-1024.flac",
        "flac-long-4096.flac",
        "flac-long-table.flac",
        "flac-long-variable.flac",
    ] {
        let factory = TrustedFactory(engine.registry());
        let cancel = Cancellation::default();
        let mut demux = factory
            .create(
                &NodeConfig {
                    component: format!("{}.demux", std::env::consts::OS),
                    settings: serde_json::to_value(Request::playback(
                        root.join(name).to_str().unwrap(),
                    ))
                    .unwrap(),
                },
                cancel.clone(),
            )
            .unwrap();
        let track = &demux.info().media.as_ref().unwrap().tracks[0];
        let stream = track.index as u32;
        let decoder = factory
            .create(
                &NodeConfig {
                    component: format!("{}.audio.decode", std::env::consts::OS),
                    settings: json!({"track":track}),
                },
                cancel.clone(),
            )
            .unwrap();
        let initial: serde_json::Value = serde_json::from_slice(
            demux
                .command(&json!({"op":"flac.stats"}))
                .unwrap()
                .unwrap()
                .bytes(),
        )
        .unwrap();
        let stats = Rc::new(RefCell::new(Stats::default()));
        let mut graph = Graph::from_instances(
            vec![
                (
                    "demux".into(),
                    Box::new(Counted {
                        inner: demux,
                        stats: stats.clone(),
                    }) as Box<dyn Component>,
                ),
                (
                    "decode".into(),
                    Box::new(Counted {
                        inner: decoder,
                        stats: stats.clone(),
                    }),
                ),
            ],
            &[EdgeSpec::between("demux", "decode", Some(stream))],
            engine.budget().clone(),
            cancel,
        )
        .unwrap();
        let started = std::time::Instant::now();
        graph.reset(1_798_000_000).unwrap();
        let first = loop {
            match graph.poll().unwrap() {
                GraphEvent::Output { buffer, .. } => break buffer.header.info.pts_us().unwrap(),
                GraphEvent::Pending => {
                    assert!(started.elapsed().as_secs() < 10, "FLAC seek stalled")
                }
                GraphEvent::End => panic!("FLAC seek returned premature EOF"),
            }
        };
        let stats = stats.borrow();
        eprintln!(
            "FLAC_SEEK {}",
            json!({"fixture":name,"elapsed_ms":started.elapsed().as_secs_f64()*1000.,"first_pts_us":first,
            "decoder_packets":stats.packets,"decoder_bytes":stats.bytes,"container_read_bytes":stats.container["bytes_read"].as_u64().unwrap()-initial["bytes_read"].as_u64().unwrap(),
            "container_frames_validated":stats.container["frames_read"],"index_entries":stats.container["index_entries"]})
        );
        assert!(
            (1_797_900_000..=1_798_000_000).contains(&first),
            "decoded from the beginning: {first}"
        );
        assert!(
            stats.packets <= 8,
            "decoded {} preroll packets",
            stats.packets
        );
        assert!(stats.container["index_entries"].as_u64().unwrap() <= 4096);
    }
}
