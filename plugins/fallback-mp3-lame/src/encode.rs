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
