use anyhow::{Context, Result, ensure};
use mp3lame_encoder::{Bitrate, Builder, Encoder, FlushGap, InterleavedPcm, MonoPcm, Quality};
use remdia_graph::*;
fn info(input: DataKind, format: &str) -> NodeInfo {
    let mut output = PortCaps::new(DataKind::Bytes);
    output.format = Some(format.into());
    NodeInfo {
        input: Some(PortCaps::new(input)),
        output,
        max_output_bytes: 16 * 1024 * 1024,
        media: None,
    }
}
fn bytes(format: &str, offset: Option<u64>, data: Vec<u8>) -> MediaBuffer {
    MediaBuffer::new(
        BufferHeader {
            stream_id: 0,
            generation: 1,
            info: FrameInfo::Bytes {
                format: format.into(),
                offset,
            },
        },
        vec![data],
    )
}
fn format(c: &NodeConfig) -> Result<(u32, u16)> {
    let rate = c
        .settings
        .get("rate")
        .and_then(|v| v.as_u64())
        .unwrap_or(16000) as u32;
    let channels = c
        .settings
        .get("channels")
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as u16;
    ensure!(
        (8000..=192000).contains(&rate) && (1..=2).contains(&channels),
        "invalid audio mux format"
    );
    Ok((rate, channels))
}
pub struct Wav {
    info: NodeInfo,
    rate: u32,
    channels: u16,
    header_sent: bool,
    pending: Option<MediaBuffer>,
    written: u32,
    draining: bool,
    finalized: bool,
}
impl Wav {
    pub fn new(c: &NodeConfig) -> Result<Self> {
        let (rate, channels) = format(c)?;
        let mut info = info(DataKind::Audio, "wav");
        info.input.as_mut().unwrap().rate = Some(rate);
        info.input.as_mut().unwrap().channels = Some(channels);
        Ok(Self {
            info,
            rate,
            channels,
            header_sent: false,
            pending: None,
            written: 0,
            draining: false,
            finalized: false,
        })
    }
    fn header(&self) -> Vec<u8> {
        let mut h = Vec::with_capacity(44);
        h.extend_from_slice(b"RIFF");
        h.extend_from_slice(&(36 + self.written).to_le_bytes());
        h.extend_from_slice(b"WAVEfmt ");
        h.extend_from_slice(&16u32.to_le_bytes());
        h.extend_from_slice(&1u16.to_le_bytes());
        h.extend_from_slice(&self.channels.to_le_bytes());
        h.extend_from_slice(&self.rate.to_le_bytes());
        let align = self.channels * 2;
        h.extend_from_slice(&(self.rate * align as u32).to_le_bytes());
        h.extend_from_slice(&align.to_le_bytes());
        h.extend_from_slice(&16u16.to_le_bytes());
        h.extend_from_slice(b"data");
        h.extend_from_slice(&self.written.to_le_bytes());
        h
    }
}
impl Component for Wav {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, b: &MediaBuffer) -> Result<Submit> {
        if self.pending.is_some() {
            return Ok(Submit::WouldBlock);
        }
        ensure!(!self.draining, "submit after WAV drain");
        b.validate()?;
        ensure!(
            matches!(b.header.info,FrameInfo::Audio {rate,channels,..} if rate==self.rate && channels==self.channels),
            "WAV input format mismatch"
        );
        self.pending = Some(b.clone());
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> Result<Receive> {
        if !self.header_sent {
            self.header_sent = true;
            return Ok(Receive::Output(bytes("wav", None, self.header())));
        }
        if let Some(input) = self.pending.take() {
            let data: Vec<_> = input
                .bytes()
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|v| {
                    ((f32::from_le_bytes(*v).clamp(-1., 1.) * 32767.).round() as i16).to_le_bytes()
                })
                .collect();
            self.written = self
                .written
                .checked_add(u32::try_from(data.len())?)
                .filter(|v| *v <= u32::MAX - 36)
                .context("WAV exceeds RIFF size limit")?;
            return Ok(Receive::Output(bytes("wav", None, data)));
        }
        if self.draining {
            if !self.finalized {
                self.finalized = true;
                return Ok(Receive::Output(bytes("wav", Some(0), self.header())));
            }
            return Ok(Receive::End);
        }
        Ok(Receive::NeedInput)
    }
    fn drain(&mut self) -> Result<()> {
        self.draining = true;
        Ok(())
    }
    fn reset(&mut self, _g: u64, _t: i64) -> Result<()> {
        ensure!(
            !self.header_sent,
            "reset an active mux by creating a new output graph"
        );
        Ok(())
    }
}
pub struct Mp3 {
    info: NodeInfo,
    encoder: Encoder,
    rate: u32,
    channels: u16,
    pending: Option<MediaBuffer>,
    draining: bool,
    flushed: bool,
    tagged: bool,
}
impl Mp3 {
    pub fn new(c: &NodeConfig) -> Result<Self> {
        let (rate, channels) = format(c)?;
        ensure!(rate <= 48000, "MP3 sample rate exceeds 48kHz");
        let encoder = Builder::new()
            .context("LAME allocation failed")?
            .with_num_channels(channels as u8)?
            .with_sample_rate(rate)?
            .with_brate(Bitrate::Kbps64)?
            .with_quality(Quality::Best)?
            .with_to_write_vbr_tag(true)?
            .build()?;
        let mut info = info(DataKind::Audio, "mp3");
        info.input.as_mut().unwrap().rate = Some(rate);
        info.input.as_mut().unwrap().channels = Some(channels);
        Ok(Self {
            info,
            encoder,
            rate,
            channels,
            pending: None,
            draining: false,
            flushed: false,
            tagged: false,
        })
    }
}
impl Component for Mp3 {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, b: &MediaBuffer) -> Result<Submit> {
        if self.pending.is_some() {
            return Ok(Submit::WouldBlock);
        }
        ensure!(!self.draining, "submit after MP3 drain");
        b.validate()?;
        ensure!(
            matches!(b.header.info,FrameInfo::Audio {rate,channels,..} if rate==self.rate && channels==self.channels),
            "MP3 input format mismatch"
        );
        self.pending = Some(b.clone());
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> Result<Receive> {
        if let Some(input) = self.pending.take() {
            let samples: Vec<_> = input
                .bytes()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|v| f32::from_le_bytes(*v))
                .collect();
            ensure!(samples.iter().all(|s| s.is_finite()), "non-finite PCM");
            let mut encoded =
                Vec::with_capacity(mp3lame_encoder::max_required_buffer_size(samples.len()));
            if self.channels == 1 {
                self.encoder
                    .encode_to_vec(MonoPcm(&samples), &mut encoded)?;
            } else {
                self.encoder
                    .encode_to_vec(InterleavedPcm(&samples), &mut encoded)?;
            }
            if !encoded.is_empty() {
                return Ok(Receive::Output(bytes("mp3", None, encoded)));
            }
        }
        if self.draining {
            if !self.flushed {
                self.flushed = true;
                let mut encoded = Vec::with_capacity(7200);
                self.encoder.flush_to_vec::<FlushGap>(&mut encoded)?;
                return Ok(Receive::Output(bytes("mp3", None, encoded)));
            }
            if !self.tagged {
                self.tagged = true;
                let mut tag = Vec::with_capacity(self.encoder.lame_tag_size());
                if self.encoder.lame_tag_encode_to_vec(&mut tag).is_some() {
                    return Ok(Receive::Output(bytes(
                        "mp3",
                        Some(self.encoder.id3v2_tag_size() as u64),
                        tag,
                    )));
                }
            }
            return Ok(Receive::End);
        }
        Ok(Receive::NeedInput)
    }
    fn drain(&mut self) -> Result<()> {
        self.draining = true;
        Ok(())
    }
    fn reset(&mut self, _g: u64, _t: i64) -> Result<()> {
        anyhow::bail!("reset an active MP3 encoder by creating a new output graph")
    }
}

pub struct PacketMux {
    info: NodeInfo,
    pending: Option<MediaBuffer>,
    draining: bool,
    stream: u32,
}
impl PacketMux {
    pub fn new(c: &NodeConfig) -> Result<Self> {
        let track: Track = serde_json::from_value(
            c.settings
                .get("track")
                .cloned()
                .context("MP3 mux requires track configuration")?,
        )?;
        ensure!(track.codec == "mp3", "raw MP3 mux requires MP3 packets");
        Ok(Self {
            info: info(DataKind::Packet, "mp3"),
            pending: None,
            draining: false,
            stream: track.index as u32,
        })
    }
}
impl Component for PacketMux {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, b: &MediaBuffer) -> Result<Submit> {
        if self.pending.is_some() {
            return Ok(Submit::WouldBlock);
        }
        ensure!(
            !self.draining && b.header.stream_id == self.stream,
            "invalid MP3 mux input"
        );
        ensure!(
            matches!(b.header.info, FrameInfo::Packet { .. }),
            "MP3 mux expected packet"
        );
        self.pending = Some(b.clone());
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> Result<Receive> {
        if let Some(mut b) = self.pending.take() {
            b.header.info = FrameInfo::Bytes {
                format: "mp3".into(),
                offset: None,
            };
            return Ok(Receive::Output(b));
        }
        Ok(if self.draining {
            Receive::End
        } else {
            Receive::NeedInput
        })
    }
    fn drain(&mut self) -> Result<()> {
        self.draining = true;
        Ok(())
    }
    fn reset(&mut self, _g: u64, _t: i64) -> Result<()> {
        self.pending = None;
        self.draining = false;
        Ok(())
    }
}
pub struct FileSink {
    info: NodeInfo,
    file: Option<std::fs::File>,
    temporary: std::path::PathBuf,
    destination: std::path::PathBuf,
    ended: bool,
}
impl FileSink {
    pub fn new(c: &NodeConfig) -> Result<Self> {
        let destination = std::path::PathBuf::from(
            c.settings
                .get("path")
                .and_then(|v| v.as_str())
                .context("file sink requires path")?,
        );
        ensure!(!destination.exists(), "destination already exists");
        let serial = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let temporary =
            destination.with_extension(format!("remdia-{}-{serial}.tmp", std::process::id()));
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        Ok(Self {
            info: info(DataKind::Bytes, "sink"),
            file: Some(file),
            temporary,
            destination,
            ended: false,
        })
    }
}
impl Drop for FileSink {
    fn drop(&mut self) {
        self.file = None;
        let _ = std::fs::remove_file(&self.temporary);
    }
}
impl Component for FileSink {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, b: &MediaBuffer) -> Result<Submit> {
        use std::io::{Seek, SeekFrom, Write};
        ensure!(!self.ended, "write after file finalize");
        let FrameInfo::Bytes { offset, .. } = b.header.info else {
            anyhow::bail!("file sink expected container bytes")
        };
        let file = self.file.as_mut().context("file sink closed")?;
        if let Some(offset) = offset {
            file.seek(SeekFrom::Start(offset))?;
        } else {
            file.seek(SeekFrom::End(0))?;
        }
        file.write_all(b.bytes())?;
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> Result<Receive> {
        Ok(if self.ended {
            Receive::End
        } else {
            Receive::NeedInput
        })
    }
    fn drain(&mut self) -> Result<()> {
        if !self.ended {
            self.file.as_ref().unwrap().sync_all()?;
            self.file = None;
            std::fs::hard_link(&self.temporary, &self.destination)
                .context("publish output without overwriting destination")?;
            std::fs::remove_file(&self.temporary)?;
            self.ended = true;
        }
        Ok(())
    }
    fn reset(&mut self, _g: u64, _t: i64) -> Result<()> {
        anyhow::bail!("file sink cannot seek an active export graph")
    }
}
