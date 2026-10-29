//! Calibrate the installed system encoder using the system decoder. No bundled
//! decoder and no vendor-specific hard-coded encoder priming value.
use super::{windows_audio::Decoder, windows_encode::Encoder};
use anyhow::{Context, Result, ensure};
use remdia_graph::*;
use serde_json::json;
use std::sync::Mutex;
const RATE: u32 = 16000;
#[derive(Clone, Copy)]
pub struct Calibration {
    pub delay: u32,
    pub decoder_trim: usize,
    pub score: f64,
}
fn audio(samples: &[f32]) -> MediaBuffer {
    MediaBuffer::new(
        BufferHeader {
            stream_id: 0,
            generation: 1,
            info: FrameInfo::Audio {
                pts_us: 0,
                rate: RATE,
                channels: 1,
                samples: samples.len(),
                format: "f32le".into(),
            },
        },
        vec![samples.iter().flat_map(|v| v.to_le_bytes()).collect()],
    )
}
pub fn get(control: Control) -> Result<Calibration> {
    static CACHE: Mutex<Option<Calibration>> = Mutex::new(None);
    control.check()?;
    let mut cache = CACHE
        .lock()
        .map_err(|_| anyhow::anyhow!("MP3 calibration lock poisoned"))?;
    if let Some(result) = *cache {
        return Ok(result);
    }
    let result = calibrate(control)?;
    *cache = Some(result);
    Ok(result)
}
fn calibrate(control: Control) -> Result<Calibration> {
    let mut state = 0xa75f190du32;
    let mut filtered = 0f32;
    let input = (0..8192)
        .map(|i| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let noise = (state as f64 / u32::MAX as f64 * 2. - 1.) as f32;
            filtered = filtered * 0.65 + noise * 0.35;
            if (512..7168).contains(&i) {
                filtered * 0.7
            } else {
                0.
            }
        })
        .collect::<Vec<_>>();
    let mut encoder = Encoder::new(&NodeConfig {
        component: "windows.mp3.encode".into(),
        settings: json!({"rate":RATE,"channels":1}),
    })?;
    ensure!(
        encoder.submit(&audio(&input))? == Submit::Accepted,
        "system MP3 calibration input rejected"
    );
    encoder.drain()?;
    let mut encoded = Vec::new();
    for n in 0..1000 {
        control.check()?;
        ensure!(n < 999, "system MP3 calibration encode stalled");
        match encoder.receive()? {
            Receive::Output(buffer) => {
                encoded.extend_from_slice(buffer.bytes());
                ensure!(encoded.len() <= 65536, "oversize MP3 calibration output");
            }
            Receive::End => break,
            _ => anyhow::bail!("system MP3 calibration requested input after drain"),
        }
    }
    let total = remdia_bitstream::mp3::packet_samples(&encoded, RATE)?;
    let track = Track {
        index: 0,
        kind: "audio".into(),
        codec: "mp3".into(),
        time_base: Rational {
            num: 1,
            den: RATE as i32,
        },
        width: 0,
        height: 0,
        sample_rate: RATE,
        channels: 1,
        attached_picture: false,
        metadata: Default::default(),
        initialization: json!({"extra_data":[],"container":"mp3"}),
    };
    let mut decoder = Decoder::new(&NodeConfig {
        component: "windows.audio.decode".into(),
        settings: json!({"track":track}),
    })?;
    let packet = MediaBuffer::new(
        BufferHeader {
            stream_id: 0,
            generation: 1,
            info: FrameInfo::Packet {
                pts: 0,
                dts: 0,
                duration: total as u64,
                time_base: track.time_base,
                keyframe: Some(true),
                trim_start: 0,
                trim_end: 0,
            },
        },
        vec![encoded],
    );
    ensure!(
        decoder.submit(&packet)? == Submit::Accepted,
        "system MP3 calibration decode input rejected"
    );
    decoder.drain()?;
    let mut decoded = Vec::new();
    for n in 0..1000 {
        control.check()?;
        ensure!(n < 999, "system MP3 calibration decode stalled");
        match decoder.receive()? {
            Receive::Output(buffer) => {
                decoded.extend(
                    buffer
                        .bytes()
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|v| f32::from_le_bytes(*v)),
                );
                ensure!(decoded.len() <= 16384, "oversize MP3 calibration PCM");
            }
            Receive::End => break,
            Receive::NeedInput => {}
            Receive::Reconfigured => {}
        }
    }
    let decoder_trim = total
        .checked_sub(decoded.len())
        .context("system MP3 calibration produced excess samples")?;
    ensure!(
        decoder_trim < 1152 && decoded.len() >= 8192,
        "unexpected system MP3 decoder priming"
    );
    let reference = &input[1024..6144];
    let energy = reference.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
    let mut scores = Vec::with_capacity(2048);
    for lag in 0..2048 {
        if lag % 32 == 0 {
            control.check()?;
        }
        let candidate = &decoded[1024 + lag..6144 + lag];
        let mut dot = 0.;
        let mut power = 0.;
        for (&a, &b) in reference.iter().zip(candidate) {
            dot += f64::from(a) * f64::from(b);
            power += f64::from(b).powi(2);
        }
        scores.push(dot / (energy * power).sqrt().max(1e-20));
    }
    let (lag, &score) = scores
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .context("empty MP3 calibration")?;
    ensure!(
        score > 0.85,
        "system MP3 calibration correlation is too low: {score}"
    );
    ensure!(
        scores
            .iter()
            .enumerate()
            .filter(|(i, _)| i.abs_diff(lag) > 8)
            .all(|(_, v)| *v < score - 0.05),
        "ambiguous system MP3 calibration lag"
    );
    // The tag convention includes a standard 529-sample decoder delay. The
    // installed decoder may already remove some priming; measure that removal.
    let delay = u32::try_from(
        (lag + decoder_trim)
            .checked_sub(529)
            .context("invalid system MP3 calibrated delay")?,
    )?;
    ensure!(
        delay <= 2047,
        "system MP3 delay exceeds validated profile: {delay}"
    );
    Ok(Calibration {
        delay,
        decoder_trim,
        score,
    })
}
