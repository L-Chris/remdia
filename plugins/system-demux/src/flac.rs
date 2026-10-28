//! Compensate for system container handlers' unframed/native-FLAC seek gaps.
//! Only compressed framing is owned here; PCM still comes from system decoders.
use anyhow::{Result, ensure};
use remdia_graph::*;

pub struct Source {
    reader: remdia_bitstream::flac::Reader,
    stream: u32,
}
impl Source {
    pub fn open(
        path: &std::path::Path,
        media: &mut MediaInfo,
        limit: usize,
        control: Control,
    ) -> Result<Option<Self>> {
        if media.container != "flac" {
            return Ok(None);
        }
        ensure!(
            media.tracks.len() == 1 && media.tracks[0].codec == "flac",
            "unexpected native FLAC tracks"
        );
        let reader = remdia_bitstream::flac::Reader::open(path, limit, &|| control.check())?;
        let track = &mut media.tracks[0];
        ensure!(
            track.sample_rate == reader.rate && track.channels == reader.channels,
            "system FLAC format disagrees with STREAMINFO"
        );
        track.time_base = Rational {
            num: 1,
            den: reader.rate as i32,
        };
        // Use the portable STREAMINFO contract on both platforms. Each system
        // decoder adapter constructs its own native initialization envelope.
        track.initialization["extra_data"] = serde_json::json!(reader.streaminfo.as_slice());
        track.initialization["system_cookie"] = false.into();
        track.initialization["bits_per_sample"] = reader.bits.into();
        if reader.samples > 0 {
            media.duration_us =
                Some((u128::from(reader.samples) * 1_000_000 / u128::from(reader.rate)) as i64);
        }
        Ok(Some(Self {
            reader,
            stream: track.index as u32,
        }))
    }
    pub fn receive(&mut self, generation: u64, control: Control) -> Result<Receive> {
        let Some(packet) = self.reader.next(&|| control.check())? else {
            return Ok(Receive::End);
        };
        Ok(Receive::Output(MediaBuffer::new(
            BufferHeader {
                stream_id: self.stream,
                generation,
                info: FrameInfo::Packet {
                    pts: packet.sample as i64,
                    dts: packet.sample as i64,
                    duration: u64::from(packet.frames),
                    time_base: Rational {
                        num: 1,
                        den: self.reader.rate as i32,
                    },
                    keyframe: Some(true),
                    trim_start: 0,
                    trim_end: 0,
                },
            },
            vec![packet.bytes],
        )))
    }
    pub fn seek(&mut self, target: i64, control: Control) -> Result<()> {
        let target =
            (u128::try_from(target.max(0))? * u128::from(self.reader.rate) / 1_000_000) as u64;
        self.reader.seek(target, &|| control.check())
    }
    pub fn stats(&self, generation: u64) -> MediaBuffer {
        MediaBuffer::new(
            BufferHeader {
                stream_id: self.stream,
                generation,
                info: FrameInfo::Bytes {
                    format: "json".into(),
                    offset: None,
                },
            },
            vec![
                serde_json::to_vec(&serde_json::json!({
                    "bytes_read":self.reader.bytes_read(), "frames_read":self.reader.frames_read,
                    "index_entries":self.reader.index_entries()
                }))
                .unwrap(),
            ],
        )
    }
}
