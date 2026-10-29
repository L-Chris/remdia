#![cfg(any(windows, target_os = "macos"))]
use remdia::image::{Image, ImageInput, ImageOperation as Op, ImageRequest};
use remdia::{Cancellation, Engine, FrameInfo, MediaRequest};
use serde_json::json;

fn engine() -> Engine {
    Engine::discover(
        std::env::var_os("REMDIA_TEST_PLUGIN_DIR").expect("staged plugins"),
        128 * 1024 * 1024,
    )
    .unwrap()
}
fn run(engine: &Engine, input: ImageInput, ops: Vec<Op>) -> Vec<remdia::Frame> {
    let request = MediaRequest::Image(ImageRequest::new(input, ops));
    let mut session = unsafe { engine.open_job(request, Cancellation::default()).unwrap() };
    let mut out = Vec::new();
    while let Some(frame) = session.read().unwrap() {
        out.push(frame)
    }
    out
}
#[test]
#[ignore = "requires staged system image plugin"]
fn system_image_roundtrip_and_graph_filters() {
    let engine = engine();
    let pixels = vec![
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255, 0, 255, 255, 255, 255, 0,
        255, 255,
    ];
    let original = Image::from_rgba(2, 3, pixels.clone()).unwrap();
    let encoded = run(
        &engine,
        ImageInput::Pixels(original.clone()),
        vec![Op::new("encode", json!({"format":"png"}))],
    );
    assert_eq!(&encoded[0].bytes()[..8], b"\x89PNG\r\n\x1a\n");
    let decoded = run(
        &engine,
        ImageInput::Bytes(encoded[0].bytes().into()),
        vec![Op::new("decode", json!({}))],
    );
    assert_eq!(decoded[0].bytes(), pixels);
    let scaled = run(
        &engine,
        ImageInput::Pixels(original.clone()),
        vec![
            Op::new("scale", json!({"width":4,"height":6})),
            Op::new("tile", json!({"height":4})),
        ],
    );
    assert_eq!(scaled.len(), 2);
    assert!(
        matches!(scaled[0].info(),FrameInfo::Image {image, ..} if image.width==4&&image.height==4)
    );
    assert!(
        matches!(scaled[1].info(),FrameInfo::Image {image, ..} if image.width==4&&image.height==2)
    );
    let crop = run(
        &engine,
        ImageInput::Pixels(original.clone()),
        vec![Op::new("crop", json!({"x":1,"y":1,"width":1,"height":2}))],
    );
    assert_eq!(crop[0].bytes(), &[255, 255, 0, 255, 255, 0, 255, 255]);
    let rotate = run(
        &engine,
        ImageInput::Pixels(original.clone()),
        vec![Op::new("orient", json!({"orientation":6}))],
    );
    assert_eq!(
        rotate[0].bytes(),
        &[
            0, 255, 255, 255, 0, 0, 255, 255, 255, 0, 0, 255, 255, 0, 255, 255, 255, 255, 0, 255,
            0, 255, 0, 255
        ]
    );
    let patch = Image::from_rgba(1, 1, vec![12, 34, 56, 255]).unwrap();
    let composed = run(
        &engine,
        ImageInput::Sequence(vec![original, patch]),
        vec![Op::new("compose", json!({"x":1,"y":1}))],
    );
    let mut expected = pixels;
    expected[12..16].copy_from_slice(&[12, 34, 56, 255]);
    assert_eq!(composed[0].bytes(), expected);
    drop((encoded, decoded, scaled, crop, rotate, composed));
    assert_eq!(engine.budget().used(), 0);
}

#[test]
#[ignore = "requires staged system image plugin"]
fn image_metadata_limits_cancellation_and_alpha() {
    let engine = engine();
    let input = Image::from_rgba(1, 1, vec![255, 0, 0, 128]).unwrap();
    let flat = run(
        &engine,
        ImageInput::Pixels(input),
        vec![Op::new("alpha", json!({"background":[255,255,255]}))],
    );
    assert_eq!(flat[0].bytes(), &[255, 127, 127, 255]);
    let fixture =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/cover.png");
    let probe = run(
        &engine,
        ImageInput::File(fixture.clone()),
        vec![Op::new("probe", json!({}))],
    );
    let meta: serde_json::Value = serde_json::from_slice(probe[0].bytes()).unwrap();
    assert_eq!(meta["format"], "public.png");
    let request = MediaRequest::Image(ImageRequest::new(
        ImageInput::File(fixture.clone()),
        vec![Op::new("decode", json!({"max_source_width":1}))],
    ));
    let mut session = unsafe { engine.open_job(request, Cancellation::default()).unwrap() };
    assert!(session.read().is_err());
    let request = MediaRequest::Image(ImageRequest::new(
        ImageInput::File(fixture),
        vec![Op::new("decode", json!({}))],
    ));
    let cancel = Cancellation::default();
    let mut session = unsafe { engine.open_job(request, cancel.clone()).unwrap() };
    cancel.cancel();
    assert!(session.read().is_err());
}

#[test]
#[ignore = "requires staged system image plugins"]
fn webp_uses_only_the_installed_system_codec_or_reports_absence() {
    let engine = engine();
    let caps = unsafe { engine.image_capabilities().unwrap() };
    let supported = caps["decode"]
        .as_array()
        .unwrap()
        .iter()
        .any(|format| format == "org.webmproject.webp");
    let request = MediaRequest::Image(ImageRequest::new(
        ImageInput::Bytes(
            include_bytes!("../../../tests/fixtures/legacy-white.webp")
                .as_slice()
                .into(),
        ),
        vec![Op::new("decode", json!({}))],
    ));
    let plan = unsafe { engine.explain_job(&request).unwrap() };
    assert!(
        plan.graph
            .nodes
            .iter()
            .all(|node| !node.config.component.starts_with("fallback."))
    );
    if !supported {
        let mut session = unsafe { engine.open_job(request, Cancellation::default()).unwrap() };
        assert!(
            session.read().is_err(),
            "missing system codec unexpectedly decoded WebP"
        );
        return;
    }
    let frames = run(
        &engine,
        ImageInput::Bytes(
            include_bytes!("../../../tests/fixtures/legacy-white.webp")
                .as_slice()
                .into(),
        ),
        vec![Op::new("decode", json!({}))],
    );
    assert_eq!(frames.len(), 1);
    assert!(
        matches!(frames[0].info(),FrameInfo::Image {image,..} if image.width==32&&image.height==64)
    );
    assert!(
        frames[0]
            .bytes()
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p == &[250, 250, 250, 255])
    );
}
