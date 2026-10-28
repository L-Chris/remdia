//! Real DLL integration tests: run after scripts/fixtures.py and scripts/stage.py.
use remdia::*;
use std::path::PathBuf;
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
#[test]
#[ignore = "requires staged capability pack and generated fixtures"]
fn decode_drain_seek_and_frame_lifetimes() {
    let e = engine();
    let mut s = open(&e, "av.mkv", Task::Playback);
    assert_eq!(s.info().tracks.len(), 2);
    assert!(s.info().metadata.values().any(|v| v == "Remdia fixture"));
    let (mut video, mut audio) = (0, 0);
    let mut retained = None;
    while let Some(f) = s.read().unwrap() {
        match f.info() {
            FrameInfo::Video { format, .. } => {
                video += 1;
                assert_eq!(format, "yuv420p");
                if retained.is_none() {
                    retained = Some(f.clone());
                }
            }
            FrameInfo::Audio { samples, .. } => audio += samples,
            _ => {}
        }
    }
    assert_eq!(video, 72, "B-frames must drain");
    assert_eq!(audio, 48000, "resampler tail must drain");
    assert!(s.read().unwrap().is_none());
    let generation = s.seek(1_250_000).unwrap();
    for _ in 0..12 {
        let f = s.read().unwrap().unwrap();
        assert_eq!(f.generation(), generation);
        assert!(f.info().pts_us().unwrap() >= 1_250_000);
    }
    let f = retained.unwrap();
    let prefix = f.bytes()[..32].to_vec();
    drop(s);
    drop(e);
    let f = std::thread::spawn(move || {
        assert_eq!(&f.bytes()[..32], prefix);
        f
    })
    .join()
    .unwrap();
    assert_eq!(f.generation(), 1);
}
#[test]
#[ignore = "requires staged capability pack and generated fixtures"]
fn chunks_mp3_gapless_and_artwork() {
    let e = engine();
    let mut s = open(&e, "audio.wav", Task::Audio);
    let chunk = s.audio_chunk(333_000, 1_123_000).unwrap();
    assert_eq!(chunk.samples.len(), 17968);
    assert_eq!(&chunk.wav()[..4], b"RIFF");
    assert_eq!(chunk.wav().len(), 44 + 17968 * 2);
    let mp3 = s.encode_mp3(&chunk).unwrap();
    assert!(mp3.bytes().len() < chunk.wav().len());
    let path = PathBuf::from(std::env::var("REMDIA_TEST_FIXTURES").unwrap()).join("roundtrip.mp3");
    std::fs::write(&path, mp3.bytes()).unwrap();
    drop(mp3);
    let mut r = Request::playback(path.to_str().unwrap());
    r.task = Task::Audio;
    r.audio_rate = 16000;
    r.audio_channels = 1;
    let mut decoded = unsafe { e.open(r, Cancellation::default()) }.unwrap();
    let mut count = 0;
    while let Some(f) = decoded.read().unwrap() {
        if let FrameInfo::Audio { samples, .. } = f.info() {
            count += samples;
        }
    }
    assert!(
        count.abs_diff(chunk.samples.len()) <= 1,
        "MP3 delay/padding changed sample count: {count}"
    );
    drop(chunk);
    let mut p = open(&e, "cover.flac", Task::Probe);
    assert!(p.info().tracks.iter().any(|t| t.attached_picture));
    let art = p.artwork().unwrap().unwrap();
    assert!(art.bytes().starts_with(b"\x89PNG"));
    let mut t = open(&e, "av.mkv", Task::Thumbnail);
    t.seek(1_000_000).unwrap();
    let thumb = t.read().unwrap().unwrap();
    assert!(
        matches!(thumb.info(),FrameInfo::Video {format,width:160,height:90,..} if format=="rgba")
    );
}
#[test]
#[ignore = "requires staged capability pack and generated fixtures"]
fn budgets_cancellation_bad_input_and_origin() {
    let e = engine();
    let mut r = Request::playback(fixture("av.mkv"));
    r.max_frame_bytes = 1024;
    let mut s = unsafe { e.open(r, Cancellation::default()) }.unwrap();
    assert!(s.read().is_err());
    drop(s);
    let small = Engine::discover(std::env::var("REMDIA_TEST_PLUGIN_DIR").unwrap(), 1024).unwrap();
    assert!(small.plan(&Request::playback("x")).is_err());
    let mut s = open(&e, "av.mkv", Task::Playback);
    let cancel = s.cancellation();
    std::thread::spawn(move || cancel.cancel()).join().unwrap();
    assert!(s.read().is_err());
    let mut r = Request::playback(fixture("broken.bin"));
    r.task = Task::Probe;
    assert!(unsafe { e.open(r, Cancellation::default()) }.is_err());
    let mut s = open(&e, "origin.ts", Task::Playback);
    assert!(s.info().origin_us > 0);
    let f = s.read().unwrap().unwrap();
    assert!(f.info().pts_us().unwrap().abs() < 100000);
}
