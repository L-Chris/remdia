//! Compressed native-FLAC framing for the ordinary system decoder graph.
use anyhow::Result;
use remdia_graph::*;
use serde_json::json;

pub struct Demux {
    reader: remdia_bitstream::flac::Reader,
    info: NodeInfo,
    source: std::path::PathBuf,
    control: Control,
    generation: u64,
    ended: bool,
}
impl Demux {
    pub fn open(request: &Request, control: Control) -> Result<Self> {
        let source = std::path::Path::new(&request.source).canonicalize()?;
        let reader =
            remdia_bitstream::flac::Reader::open(&source, request.max_packet_bytes, &|| {
                control.check()
            })?;
        let track = Track {
            index: 0,
            kind: "audio".into(),
            codec: "flac".into(),
            time_base: Rational {
                num: 1,
                den: reader.rate as i32,
            },
            width: 0,
            height: 0,
            sample_rate: reader.rate,
            channels: reader.channels,
            attached_picture: false,
            metadata: Default::default(),
            initialization: json!({"extra_data":reader.streaminfo.as_slice(),"bits_per_sample":reader.bits,"container":"flac"}),
        };
        let media = MediaInfo {
            container: "flac".into(),
            origin_us: 0,
            duration_us: (reader.samples > 0)
                .then(|| (u128::from(reader.samples) * 1_000_000 / u128::from(reader.rate)) as i64),
            metadata: reader.metadata.clone(),
            tracks: vec![track],
        };
        Ok(Self {
            reader,
            source,
            control,
            generation: 1,
            ended: false,
            info: NodeInfo {
                input: None,
                output: PortCaps::new(DataKind::Packet),
                max_output_bytes: request.max_packet_bytes,
                media: Some(media),
            },
        })
    }
    fn stats(&self) -> MediaBuffer {
        MediaBuffer::new(BufferHeader { stream_id: 0, generation: self.generation,
            info: FrameInfo::Blob { format: "application/json".into() } },
            vec![serde_json::to_vec(&json!({"bytes_read":self.reader.bytes_read(), "frames_read":self.reader.frames_read,
                "index_entries":self.reader.index_entries()})).unwrap()])
    }
}
impl Component for Demux {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn receive(&mut self) -> Result<Receive> {
        self.control.check()?;
        if self.ended {
            return Ok(Receive::End);
        }
        let Some(packet) = self.reader.next(&|| self.control.check())? else {
            self.ended = true;
            return Ok(Receive::End);
        };
        Ok(Receive::Output(MediaBuffer::new(
            BufferHeader {
                stream_id: 0,
                generation: self.generation,
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
    fn drain(&mut self) -> Result<()> {
        self.ended = true;
        Ok(())
    }
    fn reset(&mut self, generation: u64, target: i64) -> Result<()> {
        let sample =
            (u128::try_from(target.max(0))? * u128::from(self.reader.rate) / 1_000_000) as u64;
        self.reader.seek(sample, &|| self.control.check())?;
        self.generation = generation;
        self.ended = false;
        Ok(())
    }
    fn command(&mut self, command: &serde_json::Value) -> Result<Option<MediaBuffer>> {
        self.control.check()?;
        if command["op"] == "flac.stats" {
            return Ok(Some(self.stats()));
        }
        if command["op"] != "artwork" {
            return Ok(None);
        }
        Ok(
            super::metadata::flac_artwork(&self.source, 8 * 1024 * 1024)?.map(|bytes| {
                MediaBuffer::new(
                    BufferHeader {
                        stream_id: 0,
                        generation: self.generation,
                        info: FrameInfo::Artwork {
                            codec: "system-image".into(),
                        },
                    },
                    vec![bytes],
                )
            }),
        )
    }
}
