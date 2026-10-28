//! Raw Source Reader diagnostics, deliberately independent of the MFT decoder.
use super::*;

unsafe extern "C" fn never_cancel(_: *mut std::ffi::c_void) -> u8 {
    0
}

#[test]
#[ignore = "Windows native FLAC seek diagnostic; run via scripts/probe-windows-flac.py"]
fn native_flac_seek_ordering() -> Result<()> {
    let mode = std::env::var("REMDIA_FLAC_SEEK_MODE")?;
    for (name, target) in [("audio-tail.flac", 33_500_000), ("cover.flac", 23_500_000)] {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name);
        let control = Control(remdia_plugin_abi::Control {
            context: std::ptr::null_mut(),
            cancelled: never_cancel,
        });
        let config = NodeConfig {
            component: "windows.demux".into(),
            settings: serde_json::to_value(Request::playback(source.to_str().unwrap()))?,
        };
        let demux = Demux::open(&config, control)?;
        let read = || -> Result<bool> {
            unsafe {
                let mut flags = 0;
                let mut pts = 0;
                let mut sample = None;
                demux.reader.ReadSample(
                    MF_SOURCE_READER_ANY_STREAM.0 as u32,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut pts),
                    Some(&mut sample),
                )?;
                let bytes = if let Some(sample) = sample {
                    let buffer = sample.ConvertToContiguousBuffer()?;
                    let len = buffer.GetCurrentLength()?;
                    let mut ptr = std::ptr::null_mut();
                    buffer.Lock(&mut ptr, None, None)?;
                    let head = std::slice::from_raw_parts(ptr, (len as usize).min(16)).to_vec();
                    buffer.Unlock()?;
                    format!("{len} bytes, head={head:02x?}")
                } else {
                    "no sample".into()
                };
                eprintln!("{name} {mode}: pts={pts} flags={flags:#x} {bytes}");
                Ok(flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0)
            }
        };
        if mode.starts_with("primed") {
            read()?;
        } else if mode.starts_with("eof") {
            for n in 0..10000 {
                ensure!(n < 9999, "source never reached EOF");
                if read()? {
                    break;
                }
            }
        }
        for target in [target, 2_000_000, target, 0] {
            unsafe {
                if mode.ends_with("flush") {
                    demux.reader.Flush(MF_SOURCE_READER_ALL_STREAMS.0 as u32)?;
                }
                let mut value = PROPVARIANT::default();
                (*value.Anonymous.Anonymous).vt = VT_I8;
                (*value.Anonymous.Anonymous).Anonymous.hVal = target;
                eprintln!("{name} {mode}: SetCurrentPosition({target})");
                demux.reader.SetCurrentPosition(&GUID::zeroed(), &value)?;
            }
            for _ in 0..3 {
                if read()? {
                    break;
                }
            }
        }
    }
    Ok(())
}
