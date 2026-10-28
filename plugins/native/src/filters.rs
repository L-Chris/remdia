use anyhow::{Result, ensure};
use remdia_graph::*;

fn info(input: DataKind, output: DataKind, max: usize) -> NodeInfo {
    NodeInfo {
        input: Some(PortCaps::new(input)),
        output: PortCaps::new(output),
        max_output_bytes: max,
        media: None,
    }
}
fn floats(b: &MediaBuffer) -> Vec<f32> {
    b.bytes()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|v| f32::from_le_bytes(*v))
        .collect()
}
pub struct Copy {
    info: NodeInfo,
    pending: Option<MediaBuffer>,
    draining: bool,
}
impl Copy {
    pub fn new() -> Self {
        Self {
            info: info(DataKind::Packet, DataKind::Packet, 16 * 1024 * 1024),
            pending: None,
            draining: false,
        }
    }
}
impl Component for Copy {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, b: &MediaBuffer) -> Result<Submit> {
        if self.pending.is_some() {
            return Ok(Submit::WouldBlock);
        }
        ensure!(!self.draining, "submit after drain");
        self.pending = Some(b.clone());
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> Result<Receive> {
        Ok(match self.pending.take() {
            Some(b) => Receive::Output(b),
            None if self.draining => Receive::End,
            None => Receive::NeedInput,
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
pub struct Gain {
    info: NodeInfo,
    gain: f32,
    pending: Option<MediaBuffer>,
    draining: bool,
}
impl Gain {
    pub fn new(c: &NodeConfig) -> Result<Self> {
        let gain = c
            .settings
            .get("gain")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0) as f32;
        ensure!(
            gain.is_finite() && (0.0..=16.0).contains(&gain),
            "invalid gain"
        );
        Ok(Self {
            info: info(DataKind::Audio, DataKind::Audio, 16 * 1024 * 1024),
            gain,
            pending: None,
            draining: false,
        })
    }
}
impl Component for Gain {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, b: &MediaBuffer) -> Result<Submit> {
        if self.pending.is_some() {
            return Ok(Submit::WouldBlock);
        }
        ensure!(!self.draining, "submit after gain drain");
        b.validate()?;
        let mut header = b.header.clone();
        let FrameInfo::Audio { .. } = header.info else {
            anyhow::bail!("gain requires PCM")
        };
        let bytes = floats(b)
            .into_iter()
            .map(|v| v * self.gain)
            .flat_map(f32::to_le_bytes)
            .collect();
        header.generation = b.header.generation;
        self.pending = Some(MediaBuffer::new(header, vec![bytes]));
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> Result<Receive> {
        Ok(match self.pending.take() {
            Some(b) => Receive::Output(b),
            None if self.draining => Receive::End,
            None => Receive::NeedInput,
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
pub struct Rgba {
    info: NodeInfo,
    pending: Option<MediaBuffer>,
    draining: bool,
}
impl Rgba {
    pub fn new(_c: &NodeConfig) -> Result<Self> {
        let mut info = info(DataKind::Video, DataKind::Video, 1);
        info.output.format = Some("rgba".into());
        Ok(Self {
            info,
            pending: None,
            draining: false,
        })
    }
    fn convert(b: &MediaBuffer) -> Result<MediaBuffer> {
        b.validate()?;
        let FrameInfo::Video {
            width,
            height,
            format,
            planes,
            color_space,
            color_range,
            ..
        } = &b.header.info
        else {
            anyhow::bail!("video scaler expected video")
        };
        ensure!(
            (*width as u64) * (*height as u64) <= 33_554_432,
            "image exceeds pixel limit"
        );
        ensure!(
            planes.len() == if format == "nv12" { 2 } else { 1 },
            "invalid video plane count"
        );
        ensure!(
            planes[0].rows >= *height as usize
                && planes[0].stride >= *width as usize * if format == "nv12" { 1 } else { 4 },
            "invalid video stride"
        );
        if format == "nv12" {
            ensure!(
                planes[1].rows >= height.div_ceil(2) as usize
                    && planes[1].stride >= width.div_ceil(2) as usize * 2,
                "invalid chroma stride"
            );
        }
        let mut rgba = vec![0u8; *width as usize * *height as usize * 4];
        for y in 0..*height as usize {
            for x in 0..*width as usize {
                let p = (y * *width as usize + x) * 4;
                match format.as_str() {
                    "rgba" | "bgra" => {
                        let offset = y * planes[0].stride + x * 4;
                        rgba[p..p + 4].copy_from_slice(&b.plane(0)[offset..offset + 4]);
                        if format == "bgra" {
                            rgba.swap(p, p + 2);
                        }
                    }
                    "nv12" => {
                        let yy = b.plane(0)[y * planes[0].stride + x] as f32;
                        let offset = (y / 2) * planes[1].stride + (x / 2) * 2;
                        let u = b.plane(1)[offset] as f32 - 128.;
                        let v = b.plane(1)[offset + 1] as f32 - 128.;
                        let full = color_range == "full";
                        let yy = if full { yy } else { (yy - 16.) * 255. / 219. };
                        let c = if full { 1.0 } else { 255. / 224. };
                        let (kr, kb) = if color_space.to_lowercase().contains("709")
                            || (color_space == "unspecified" && *height > 576)
                        {
                            (0.2126, 0.0722)
                        } else {
                            (0.299, 0.114)
                        };
                        let r = yy + 2. * (1. - kr) * v * c;
                        let blue = yy + 2. * (1. - kb) * u * c;
                        let g = yy
                            - 2. * kb * (1. - kb) / (1. - kr - kb) * u * c
                            - 2. * kr * (1. - kr) / (1. - kr - kb) * v * c;
                        rgba[p..p + 4].copy_from_slice(&[
                            r.round().clamp(0., 255.) as u8,
                            g.round().clamp(0., 255.) as u8,
                            blue.round().clamp(0., 255.) as u8,
                            255,
                        ]);
                    }
                    other => anyhow::bail!("RGBA converter does not support {other}"),
                }
            }
        }
        let mut header = b.header.clone();
        if let FrameInfo::Video { format, planes, .. } = &mut header.info {
            *format = "rgba".into();
            *planes = vec![Plane {
                stride: *width as usize * 4,
                rows: *height as usize,
            }];
        }
        Ok(MediaBuffer::new(header, vec![rgba]))
    }
}
impl Component for Rgba {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, b: &MediaBuffer) -> Result<Submit> {
        ensure!(!self.draining, "submit after conversion drain");
        if self.pending.is_some() {
            return Ok(Submit::WouldBlock);
        }
        b.validate()?;
        self.pending = Some(b.clone());
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> Result<Receive> {
        if let Some(b) = &self.pending {
            let FrameInfo::Video { width, height, .. } = &b.header.info else {
                anyhow::bail!("expected video")
            };
            ensure!(
                u64::from(*width) * u64::from(*height) <= 33_554_432,
                "image exceeds pixel limit"
            );
            let bound = *width as usize * *height as usize * 4;
            if bound != self.info.max_output_bytes {
                self.info.max_output_bytes = bound;
                return Ok(Receive::Reconfigured);
            }
            let input = self.pending.take().unwrap();
            return Ok(Receive::Output(Self::convert(&input)?));
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
