use super::*;
use serde_json::json;

fn config(id: &str, channels: u16, extra: &[u8]) -> NodeConfig {
    NodeConfig {
        component: "native.audio.decode".into(),
        settings: json!({"track":{
            "index":2,"kind":"audio","codec":id,"time_base":{"num":1,"den":1000},
            "width":0,"height":0,"sample_rate":48000,"channels":channels,
            "attached_picture":false,"metadata":{},"initialization":{"codec_id":id,"extra_data":extra}
        }}),
    }
}
#[cfg(any(windows, target_os = "macos"))]
fn packets(id: &str) -> Vec<Vec<u8>> {
    let mut bytes = match id {
        "0x1007" => include_bytes!("../../../../tests/fixtures/aac-surround.aac").as_slice(),
        "0x1008" => include_bytes!("../../../../tests/fixtures/ac3-surround.ac3").as_slice(),
        "0x1009" => include_bytes!("../../../../tests/fixtures/eac3-surround.eac3").as_slice(),
        _ => unreachable!(),
    };
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let (length, header) = match id {
            "0x1007" => (
                (((usize::from(bytes[3]) & 3) << 11)
                    | (usize::from(bytes[4]) << 3)
                    | usize::from(bytes[5] >> 5)),
                if bytes[1] & 1 != 0 { 7 } else { 9 },
            ),
            "0x1008" => (1536, 0), // synthetic 48-kHz, 384-kbps AC-3 syncframe
            _ => (
                ((usize::from(u16::from_be_bytes([bytes[2], bytes[3]])) & 2047) + 1) * 2,
                0,
            ),
        };
        out.push(bytes[header..length].to_vec());
        bytes = &bytes[length..];
    }
    out
}
#[cfg(any(windows, target_os = "macos"))]
fn round(decoder: &mut dyn Component, packets: &[Vec<u8>], generation: u64) -> Vec<u8> {
    decoder.reset(generation, 500000).unwrap();
    let mut decoded = Vec::new();
    for compressed in packets {
        let packet = MediaBuffer::new(
            BufferHeader {
                stream_id: 2,
                generation,
                info: FrameInfo::Packet {
                    pts: 500,
                    dts: 500,
                    duration: 32,
                    time_base: Rational { num: 1, den: 1000 },
                    keyframe: Some(true),
                    trim_start: 0,
                    trim_end: 0,
                },
            },
            vec![compressed.clone()],
        );
        assert!(matches!(decoder.submit(&packet).unwrap(), Submit::Accepted));
        while let Receive::Output(pcm) = decoder.receive().unwrap() {
            assert_eq!(pcm.header.generation, generation);
            assert!(matches!(
                pcm.header.info,
                FrameInfo::Audio {
                    channels: 6,
                    rate: 48000,
                    ..
                }
            ));
            decoded.extend_from_slice(pcm.bytes());
        }
    }
    decoder.drain().unwrap();
    for attempt in 0..32 {
        match decoder.receive().unwrap() {
            Receive::Output(pcm) => {
                assert_eq!(pcm.header.generation, generation);
                decoded.extend_from_slice(pcm.bytes());
            }
            Receive::End => break,
            Receive::NeedInput | Receive::Reconfigured => {
                assert!(attempt < 31, "system decoder did not drain")
            }
        }
    }
    assert!(!decoded.is_empty());
    assert!(
        decoded
            .as_chunks::<4>()
            .0
            .iter()
            .all(|v| f32::from_le_bytes(*v).is_finite())
    );
    assert!(decoded.iter().any(|b| *b != 0));
    decoded
}
#[cfg(any(windows, target_os = "macos"))]
#[test]
fn system_surround_codecs_drain_and_reset_without_stale_pcm() {
    for id in ["0x1007", "0x1008", "0x1009"] {
        let config = config(id, 6, if id == "0x1007" { &[0x11, 0xb0] } else { &[] });
        let mut decoder = match create(&config) {
            Ok(decoder) => decoder,
            Err(error) if id != "0x1007" => {
                // Clean OS installations may omit Dolby. Verify the explicit
                // failure contract, without loading a third-party decoder.
                assert!(error.to_string().contains("system"));
                eprintln!("System Dolby codec {id} unavailable: {error:#}");
                continue;
            }
            Err(error) => panic!("system AAC decoder must be available: {error:#}"),
        };
        let packets = packets(id);
        let first = round(&mut *decoder, &packets, 1);
        let after_seek = round(&mut *decoder, &packets, 8);
        assert_eq!(
            first.len(),
            after_seek.len(),
            "PCM length changed after reset: {id}"
        );
        let (mut error, mut signal) = (0.0, 0.0);
        for (a, b) in first
            .as_chunks::<4>()
            .0
            .iter()
            .zip(after_seek.as_chunks::<4>().0)
        {
            let (a, b) = (
                f64::from(f32::from_le_bytes(*a)),
                f64::from(f32::from_le_bytes(*b)),
            );
            error += (a - b).powi(2);
            signal += a * a;
        }
        assert!(
            error < signal * 0.02,
            "old overlap leaked through reset for {id}"
        );
    }
}
#[test]
fn invalid_system_layout_fails_instead_of_falling_back_to_a_decoder() {
    assert!(create(&config("0x1009", 0, &[])).is_err());
}
