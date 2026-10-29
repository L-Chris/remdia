//! Bounded MP3 information-frame mux; no audio codec implementation.
use anyhow::{Result, ensure};
use remdia_graph::*;
pub struct Mux {
    info: NodeInfo,
    rate: u32,
    channels: u16,
    samples: u64,
    delay: u32,
    data: Vec<u8>,
    draining: bool,
    emitted: bool,
    generation: u64,
}
impl Mux {
    pub fn new(config: &NodeConfig) -> Result<Self> {
        let rate = u32::try_from(config.settings["rate"].as_u64().unwrap_or(16000))?;
        let channels = u16::try_from(config.settings["channels"].as_u64().unwrap_or(1))?;
        let samples = config.settings["valid_samples"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("missing MP3 valid sample count"))?;
        let delay = u32::try_from(
            config.settings["encoder_delay"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("missing MP3 encoder delay"))?,
        )?;
        ensure!(
            (8000..=48000).contains(&rate)
                && (1..=2).contains(&channels)
                && samples > 0
                && samples <= u64::from(rate) * 120
                && delay <= 4095,
            "invalid MP3 gapless mux configuration"
        );
        let mut caps = PortCaps::new(DataKind::Bytes);
        caps.format = Some("mp3".into());
        Ok(Self {
            info: NodeInfo {
                input: Some(caps.clone()),
                output: caps,
                max_output_bytes: 16 * 1024 * 1024,
                media: None,
            },
            rate,
            channels,
            samples,
            delay,
            data: vec![],
            draining: false,
            emitted: false,
            generation: 1,
        })
    }
}
impl Component for Mux {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, input: &MediaBuffer) -> Result<Submit> {
        ensure!(
            !self.draining
                && matches!(&input.header.info,FrameInfo::Bytes {format,offset:None} if format=="mp3"),
            "MP3 mux requires append-only encoded MP3 bytes"
        );
        ensure!(
            self.data.len() + input.bytes().len() <= 16 * 1024 * 1024 - 8192,
            "MP3 mux exceeds output budget"
        );
        self.generation = input.header.generation;
        self.data.extend_from_slice(input.bytes());
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> Result<Receive> {
        if !self.draining {
            return Ok(Receive::NeedInput);
        }
        if self.emitted {
            return Ok(Receive::End);
        }
        let bytes = remdia_bitstream::mp3::gapless_file(
            &self.data,
            self.rate,
            self.channels,
            self.samples,
            self.delay,
        )?;
        self.data.clear();
        self.emitted = true;
        Ok(Receive::Output(MediaBuffer::new(
            BufferHeader {
                stream_id: 0,
                generation: self.generation,
                info: FrameInfo::Bytes {
                    format: "mp3".into(),
                    offset: None,
                },
            },
            vec![bytes],
        )))
    }
    fn drain(&mut self) -> Result<()> {
        self.draining = true;
        Ok(())
    }
    fn reset(&mut self, generation: u64, _target: i64) -> Result<()> {
        self.data.clear();
        self.draining = false;
        self.emitted = false;
        self.generation = generation;
        Ok(())
    }
}
