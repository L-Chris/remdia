//! Real component-DLL integration. Fixture preparation requires no media tools.
use remdia::*;
use serde_json::json;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
fn engine() -> Engine {
    Engine::discover(
        std::env::var("REMDIA_TEST_PLUGIN_DIR").expect("set REMDIA_TEST_PLUGIN_DIR"),
        256 * 1024 * 1024,
    )
    .unwrap()
}
fn fixture(name: &str) -> String {
    PathBuf::from(std::env::var("REMDIA_TEST_FIXTURES").expect("set REMDIA_TEST_FIXTURES"))
        .join(name)
        .to_str()
        .unwrap()
        .into()
}
fn open(e: &Engine, name: &str, task: Task) -> Session {
    let mut r = Request::playback(fixture(name));
    r.task = task;
    r.hardware = false;
    r.audio_rate = 16000;
    r.audio_channels = 1;
    unsafe { e.open(r, Cancellation::default()) }.unwrap()
}
fn samples(s: &mut Session) -> usize {
    let mut count = 0;
    while let Some(f) = s.read().unwrap() {
        if let FrameInfo::Audio { samples, .. } = f.info() {
            count += samples;
        }
    }
    count
}
#[test]
#[ignore = "requires staged component packs and fixtures"]
fn audio_ranges_gapless_mp3_and_frame_lifetimes() {
    let e = engine();
    let mut s = open(&e, "audio.wav", Task::Audio);
    let chunk = s.audio_chunk(333000, 1123000).unwrap();
    assert_eq!(chunk.samples.len(), 17968);
    assert_eq!(chunk.wav().len(), 44 + 17968 * 2);
    let mp3 = s.encode_mp3(&chunk).unwrap();
    assert!(mp3.bytes().len() < chunk.wav().len());
    let path = PathBuf::from(fixture("roundtrip-native.mp3"));
    std::fs::write(&path, mp3.bytes()).unwrap();
    let mut decoded = open(&e, "roundtrip-native.mp3", Task::Audio);
    assert_eq!(samples(&mut decoded), chunk.samples.len());
    let frame = s.encode_mp3(&chunk).unwrap();
    let bytes = frame.bytes()[..32].to_vec();
    drop(s);
    drop(e);
    std::thread::spawn(move || assert_eq!(&frame.bytes()[..32], bytes))
        .join()
        .unwrap();
}
#[test]
#[ignore = "requires staged component packs and fixtures"]
fn audio_decoders_resampling_and_artwork() {
    let e = engine();
    for name in ["audio.wav", "cover.flac", "audio.mp3"] {
        let mut s = open(&e, name, Task::Audio);
        let count = samples(&mut s);
        assert_eq!(
            count, 48000,
            "wrong resampler/gapless tail for {name}: {count}"
        );
    }
    let mut opus = open(&e, "audio.opus", Task::Audio);
    let count = samples(&mut opus);
    assert!(count.abs_diff(48000) <= 1, "Opus pre-skip/tail: {count}");
    let mut p = open(&e, "cover.flac", Task::Probe);
    assert_eq!(p.info().metadata.get("artist").unwrap(), "Remdia");
    let art = p.artwork().unwrap().unwrap();
    assert!(art.bytes().starts_with(b"\x89PNG"));
}
#[test]
#[ignore = "requires staged component packs and fixtures"]
fn arbitrary_filter_fanout_and_wav_mux_graph() {
    let e = engine();
    let s = open(&e, "audio.wav", Task::Audio);
    let mut spec = s.plan().graph.clone();
    let last = spec.nodes.last().unwrap().id.clone();
    let id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut paths = Vec::new();
    for (branch, gain) in [("half", 0.5), ("full", 1.0)] {
        let path = PathBuf::from(fixture(&format!("graph-{branch}-{id}.wav")));
        paths.push(path.clone());
        for (node, component, settings) in [
            (
                format!("gain_{branch}"),
                "native.audio.gain",
                json!({"gain":gain}),
            ),
            (
                format!("mux_{branch}"),
                "native.wav.mux",
                json!({"rate":16000,"channels":1}),
            ),
            (
                format!("sink_{branch}"),
                "native.file.sink",
                json!({"path":path}),
            ),
        ] {
            spec.nodes.push(NodeSpec {
                id: node,
                config: NodeConfig {
                    component: component.into(),
                    settings,
                },
            });
        }
        spec.edges.extend([
            EdgeSpec::between(&last, format!("gain_{branch}"), None),
            EdgeSpec::between(format!("gain_{branch}"), format!("mux_{branch}"), None),
            EdgeSpec::between(format!("mux_{branch}"), format!("sink_{branch}"), None),
        ]);
    }
    let mut graph = unsafe { e.graph(&spec, Cancellation::default()) }.unwrap();
    let start = Instant::now();
    loop {
        if matches!(graph.poll().unwrap(), GraphEvent::End) {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(10));
    }
    drop(graph);
    let half = std::fs::read(&paths[0]).unwrap();
    let full = std::fs::read(&paths[1]).unwrap();
    assert_eq!(half.len(), 44 + 48000 * 2);
    assert_eq!(full.len(), half.len());
    assert_eq!(u32::from_le_bytes(half[40..44].try_into().unwrap()), 96000);
    let energy = |v: &[u8]| {
        v[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b) as f64)
            .map(|v| v * v)
            .sum::<f64>()
    };
    assert!((energy(&half) / energy(&full) - 0.25).abs() < 0.001);
    for path in paths {
        std::fs::remove_file(path).unwrap();
    }
}
#[test]
#[ignore = "requires staged component packs and fixtures"]
fn cancellation_budgets_and_invalid_input() {
    let e = engine();
    let mut s = open(&e, "audio.wav", Task::Audio);
    let reservation = e.budget().reserve(256 * 1024 * 1024).unwrap();
    assert!(s.read().is_err());
    drop(reservation);
    assert_eq!(samples(&mut s), 48000, "budget rejection consumed input");
    let mut s = open(&e, "audio.wav", Task::Audio);
    s.cancellation().cancel();
    assert!(s.read().is_err());
    let small = Engine::discover(std::env::var("REMDIA_TEST_PLUGIN_DIR").unwrap(), 1024).unwrap();
    assert!(small.plan(&Request::playback("x")).is_err());
    let mut r = Request::playback(fixture("broken.bin"));
    r.task = Task::Probe;
    assert!(unsafe { e.open(r, Cancellation::default()) }.is_err());
}
#[test]
#[ignore = "requires staged component packs"]
fn cancellation_interrupts_blocked_network_open() {
    use std::{net::TcpListener, sync::mpsc};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, wait) = mpsc::channel();
    let (accepted, ready) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            match listener.accept() {
                Ok((_stream, _)) => {
                    let _ = accepted.send(());
                    let _ = wait.recv_timeout(Duration::from_secs(5));
                    return;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(_) => return,
            }
        }
    });
    let c = Cancellation::default();
    let cancel = c.clone();
    let worker = std::thread::spawn(move || {
        let _ = ready.recv_timeout(Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(100));
        cancel.cancel();
    });
    let e = engine();
    let mut r = Request::playback(format!("http://{address}/blocked"));
    r.task = Task::Probe;
    let start = Instant::now();
    assert!(unsafe { e.open(r, c) }.is_err());
    assert!(start.elapsed() < Duration::from_secs(3));
    let _ = stop.send(());
    server.join().unwrap();
    worker.join().unwrap();
}

#[cfg(windows)]
#[test]
#[ignore = "requires Windows MFT decoder and staged component packs"]
fn video_packets_cross_dlls_to_windows_decoder_and_seek() {
    let e = engine();
    for name in ["av.mp4", "av.mkv"] {
        let mut s = open(&e, name, Task::Playback);
        assert!(
            s.plan()
                .graph
                .nodes
                .iter()
                .any(|n| n.config.component == "native.demux")
        );
        assert!(
            s.plan()
                .graph
                .nodes
                .iter()
                .any(|n| n.config.component == "windows.video.decode")
        );
        let mut video = 0;
        let mut retained = None;
        while let Some(f) = s.read().unwrap() {
            if let FrameInfo::Video { format, .. } = f.info() {
                assert_eq!(format, "nv12");
                video += 1;
                if retained.is_none() {
                    retained = Some(f);
                }
            }
        }
        assert_eq!(video, 72, "B-frame drain for {name}");
        let generation = s.seek(1250000).unwrap();
        for _ in 0..12 {
            let f = s.read().unwrap().unwrap();
            assert_eq!(f.generation(), generation);
            assert!(f.info().pts_us().unwrap() >= 1250000);
        }
        drop(s);
        let f = retained.unwrap();
        assert!(!f.bytes().is_empty());
        let mut t = open(&e, name, Task::Thumbnail);
        t.seek(1000000).unwrap();
        let f = t.read().unwrap().unwrap();
        assert!(
            matches!(f.info(),FrameInfo::Video {format,width:160,height:90,..} if format=="rgba")
        );
    }
}
