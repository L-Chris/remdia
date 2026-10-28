#![cfg(any(windows, target_os = "macos"))]
use crate::graph::{Factory, Receive, Submit};
use crate::registry::TrustedFactory;
use crate::{
    BufferHeader, Cancellation, Engine, FrameInfo, GraphEvent, GraphSpec, MediaBuffer, NodeConfig,
    NodeSpec,
};
use serde_json::json;

fn engine() -> Engine {
    Engine::discover(
        std::env::var_os("REMDIA_TEST_PLUGIN_DIR").expect("staged system audio plugins"),
        128 * 1024 * 1024,
    )
    .unwrap()
}
fn pcm(samples: usize, rate: u32, channels: u16, pts_us: i64, generation: u64) -> MediaBuffer {
    MediaBuffer::new(
        BufferHeader {
            stream_id: 1,
            generation,
            info: FrameInfo::Audio {
                pts_us,
                rate,
                channels,
                samples,
                format: "f32le".into(),
            },
        },
        vec![
            (0..samples * channels as usize)
                .flat_map(|i| ((i as f32 * 0.13).sin() * 0.25).to_le_bytes())
                .collect(),
        ],
    )
}
#[test]
#[ignore = "requires staged system audio plugins"]
fn system_src_counts_fractional_tail_and_preserves_reset_generations() {
    let engine = engine();
    for source_rate in [16000u32, 44100, 48000] {
        for target_rate in [16000u32, 48000] {
            let mut filter = TrustedFactory(engine.registry())
                .create(
                    &NodeConfig {
                        component: format!("{}.audio.resample", std::env::consts::OS),
                        settings: json!({"rate":target_rate,"channels":2}),
                    },
                    Cancellation::default(),
                )
                .unwrap();
            for generation in [1, 2] {
                let mut submitted = 0usize;
                let mut emitted = 0usize;
                let mut energy = 0f64;
                for length in [1, 31, 1024, 4096, 7] {
                    let input = pcm(
                        length,
                        source_rate,
                        1,
                        (submitted as u128 * 1_000_000 / u128::from(source_rate)) as i64,
                        generation,
                    );
                    for attempts in 0..100 {
                        assert!(attempts < 99, "resampler input stalled");
                        let accepted = filter.submit(&input).unwrap() == Submit::Accepted;
                        while let Receive::Output(output) = filter.receive().unwrap() {
                            if let FrameInfo::Audio {
                                samples,
                                pts_us,
                                rate,
                                channels,
                                ..
                            } = output.header.info
                            {
                                assert_eq!(rate, target_rate);
                                assert_eq!(channels, 2);
                                assert_eq!(output.header.generation, generation);
                                assert_eq!(
                                    pts_us,
                                    (emitted as u128 * 1_000_000 / u128::from(target_rate)) as i64
                                );
                                emitted += samples;
                                energy += output
                                    .bytes()
                                    .as_chunks::<4>()
                                    .0
                                    .iter()
                                    .map(|v| f64::from(f32::from_le_bytes(*v)).powi(2))
                                    .sum::<f64>();
                            } else {
                                panic!("expected audio");
                            }
                        }
                        if accepted {
                            submitted += length;
                            break;
                        }
                    }
                }
                filter.drain().unwrap();
                for attempts in 0..1000 {
                    assert!(attempts < 999, "resampler drain stalled");
                    match filter.receive().unwrap() {
                        Receive::Output(output) => {
                            if let FrameInfo::Audio {
                                samples, pts_us, ..
                            } = output.header.info
                            {
                                assert_eq!(output.header.generation, generation);
                                assert_eq!(
                                    pts_us,
                                    (emitted as u128 * 1_000_000 / u128::from(target_rate)) as i64
                                );
                                emitted += samples;
                            } else {
                                panic!("expected audio");
                            }
                        }
                        Receive::End => break,
                        _ => panic!("drained resampler requested input"),
                    }
                }
                assert_eq!(
                    emitted,
                    (submitted as u128 * u128::from(target_rate)).div_ceil(u128::from(source_rate))
                        as usize
                );
                assert!(energy > 1., "system SRC lost the signal");
                filter.reset(generation + 1, 0).unwrap();
            }
        }
    }
}

#[test]
#[ignore = "requires staged system audio plugins"]
fn system_src_flushes_before_timestamp_discontinuity() {
    let engine = engine();
    let spec = GraphSpec {
        nodes: vec![
            NodeSpec {
                id: "source".into(),
                config: NodeConfig {
                    component: "core.buffer.source".into(),
                    settings: json!({}),
                },
            },
            NodeSpec {
                id: "src".into(),
                config: NodeConfig {
                    component: format!("{}.audio.resample", std::env::consts::OS),
                    settings: json!({"rate":16000,"channels":1}),
                },
            },
        ],
        edges: vec![crate::EdgeSpec::between("source", "src", None)],
    };
    let mut graph = unsafe {
        engine
            .graph_with_inputs(
                &spec,
                std::collections::BTreeMap::from([(
                    "source".into(),
                    vec![pcm(1024, 44100, 2, 0, 1), pcm(1024, 44100, 2, 2_000_000, 1)],
                )]),
                Cancellation::default(),
            )
            .unwrap()
    };
    let mut first = 0;
    let mut second = 0;
    for attempts in 0..10000 {
        assert!(attempts < 9999, "discontinuous graph stalled");
        match graph.poll().unwrap() {
            GraphEvent::End => break,
            GraphEvent::Pending => {}
            GraphEvent::Output { buffer, .. } => {
                let FrameInfo::Audio {
                    samples, pts_us, ..
                } = buffer.header.info
                else {
                    panic!("expected audio")
                };
                if pts_us < 2_000_000 {
                    first += samples;
                } else {
                    second += samples;
                }
            }
        }
    }
    assert_eq!(first, 372);
    assert_eq!(second, 372);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires staged system audio plugins"]
fn system_decoders_match_the_certified_fixture_timeline() {
    let engine = engine();
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    for (name, expected) in [
        ("audio.mp3", 132300),
        ("audio.opus", 144000),
        ("cover.flac", 132300),
        ("audio.wav", 132300),
        ("audio-alac.m4a", 132300),
        ("audio-vorbis.ogg", 132352),
        ("av.mp4", 145408),
    ] {
        let mut request = crate::Request::playback(root.join(name).to_str().unwrap());
        request.task = crate::Task::Probe;
        let session = unsafe {
            engine
                .open(request.clone(), Cancellation::default())
                .unwrap()
        };
        let track = session
            .info()
            .tracks
            .iter()
            .find(|t| t.kind == "audio")
            .unwrap();
        let spec = GraphSpec {
            nodes: vec![
                NodeSpec {
                    id: "demux".into(),
                    config: NodeConfig {
                        component: "native.demux".into(),
                        settings: serde_json::to_value(&request).unwrap(),
                    },
                },
                NodeSpec {
                    id: "decode".into(),
                    config: NodeConfig {
                        component: "macos.audio.decode".into(),
                        settings: json!({"track":track}),
                    },
                },
            ],
            edges: vec![crate::EdgeSpec::between(
                "demux",
                "decode",
                Some(track.index as u32),
            )],
        };
        let mut graph = unsafe { engine.graph(&spec, Cancellation::default()).unwrap() };
        let mut total = 0;
        let mut previous: Option<i64> = None;
        for attempts in 0..10000 {
            assert!(attempts < 9999, "{name} graph stalled");
            match graph.poll().unwrap() {
                GraphEvent::End => break,
                GraphEvent::Pending => {}
                GraphEvent::Output { buffer, .. } => {
                    let FrameInfo::Audio {
                        samples,
                        pts_us,
                        rate,
                        ..
                    } = buffer.header.info
                    else {
                        panic!("expected PCM")
                    };
                    if let Some(end) = previous {
                        assert!(
                            (pts_us - end).abs() <= 1,
                            "{name}: discontinuous PCM at {pts_us} after {end}"
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
    }
}

#[cfg(windows)]
fn windows_decode_fixture(name: &str, expected: usize) {
    let engine = engine();
    let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name);
    let mut request = crate::Request::playback(source.to_str().unwrap());
    request.task = crate::Task::Probe;
    let session = unsafe {
        engine
            .open(request.clone(), Cancellation::default())
            .unwrap()
    };
    let track = session
        .info()
        .tracks
        .iter()
        .find(|t| t.kind == "audio")
        .unwrap();
    let spec = GraphSpec {
        nodes: vec![
            NodeSpec {
                id: "demux".into(),
                config: NodeConfig {
                    component: "native.demux".into(),
                    settings: serde_json::to_value(&request).unwrap(),
                },
            },
            NodeSpec {
                id: "decode".into(),
                config: NodeConfig {
                    component: "windows.audio.decode".into(),
                    settings: json!({"track":track}),
                },
            },
        ],
        edges: vec![crate::EdgeSpec::between(
            "demux",
            "decode",
            Some(track.index as u32),
        )],
    };
    let mut graph = unsafe { engine.graph(&spec, Cancellation::default()).unwrap() };
    let mut total = 0;
    let mut first = None;
    let mut last = None;
    for attempts in 0..10000 {
        assert!(attempts < 9999, "{name} stalled");
        match graph.poll().unwrap() {
            GraphEvent::End => break,
            GraphEvent::Pending => {}
            GraphEvent::Output { buffer, .. } => {
                if let FrameInfo::Audio {
                    samples, pts_us, ..
                } = buffer.header.info
                {
                    total += samples;
                    first.get_or_insert((samples, pts_us));
                    last = Some((samples, pts_us));
                }
            }
        }
    }
    eprintln!("{name}: total {total}, first {first:?}, last {last:?}");
    assert_eq!(total, expected, "{name}");
}
#[cfg(windows)]
#[test]
#[ignore = "requires staged Windows system audio plugins"]
fn windows_system_mp3_gapless() {
    windows_decode_fixture("audio.mp3", 132300);
}
#[cfg(windows)]
#[test]
#[ignore = "requires staged Windows system audio plugins"]
fn windows_system_flac_lossless() {
    windows_decode_fixture("cover.flac", 132300);
}
#[cfg(windows)]
#[test]
#[ignore = "requires staged Windows system audio plugins"]
fn windows_system_alac_lossless() {
    windows_decode_fixture("audio-alac.m4a", 132300);
}
#[cfg(windows)]
#[test]
#[ignore = "requires staged Windows system audio plugins"]
fn windows_system_aac() {
    windows_decode_fixture("av.mp4", 145408);
}
