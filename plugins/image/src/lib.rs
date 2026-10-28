//! System image components using the same ABI, scheduler and buffers as audio/video.
mod system;
use anyhow::{Context, Result, bail, ensure};
use remdia_graph::*;
use serde_json::{Value, json};
use std::collections::VecDeque;

const OPERATIONS: &[(&str, &str)] = &[
    ("decode", "decoder"),
    ("probe", "probe"),
    ("scale", "filter"),
    ("from_video", "filter"),
    ("crop", "filter"),
    ("alpha", "filter"),
    ("orient", "filter"),
    ("tile", "filter"),
    ("compose", "filter"),
    ("encode", "encoder"),
    ("capabilities", "source"),
];
fn prefix() -> &'static str {
    if cfg!(windows) { "windows" } else { "macos" }
}
fn descriptors() -> Vec<ComponentDescriptor> {
    if !cfg!(any(windows, target_os = "macos")) {
        return Vec::new();
    }
    OPERATIONS
        .iter()
        .map(|(op, role)| ComponentDescriptor {
            id: format!("{}.image.{op}", prefix()),
            role: (*role).into(),
            input: if *op == "capabilities" {
                None
            } else {
                Some(PortCaps::new(if matches!(*op, "decode" | "probe") {
                    DataKind::Bytes
                } else if *op == "from_video" {
                    DataKind::Video
                } else {
                    DataKind::Image
                }))
            },
            output: PortCaps::new(if matches!(*op, "encode" | "probe" | "capabilities") {
                DataKind::Bytes
            } else {
                DataKind::Image
            }),
        })
        .collect()
}
fn create(config: &NodeConfig, control: Control) -> Result<Box<dyn Component>> {
    let descriptor = descriptors()
        .into_iter()
        .find(|d| d.id == config.component)
        .context("unknown system image component")?;
    let op = config.component.rsplit('.').next().unwrap().to_owned();
    let max = if op == "encode" {
        setting(&config.settings, "max_output_bytes", 40 * 1024 * 1024)? as usize
    } else if matches!(op.as_str(), "probe" | "capabilities") {
        16 * 1024
    } else {
        1
    };
    ensure!(
        max > 0 && max <= 256 * 1024 * 1024,
        "invalid image output limit"
    );
    Ok(Box::new(Node {
        op,
        settings: config.settings.clone(),
        info: NodeInfo {
            input: descriptor.input,
            output: descriptor.output,
            max_output_bytes: max,
            media: None,
        },
        control,
        input: None,
        outputs: VecDeque::new(),
        base: None,
        draining: false,
        emitted: false,
    }))
}
fn setting(settings: &Value, name: &str, default: u64) -> Result<u64> {
    match settings.get(name) {
        None => Ok(default),
        Some(value) => value.as_u64().with_context(|| format!("invalid {name}")),
    }
}
fn dimension(settings: &Value, name: &str, default: u32) -> Result<u32> {
    let n = u32::try_from(setting(settings, name, u64::from(default))?)?;
    ensure!(n > 0, "invalid {name}");
    Ok(n)
}
fn image(buffer: &MediaBuffer) -> Result<&ImageInfo> {
    let FrameInfo::Image { image, .. } = &buffer.header.info else {
        bail!("expected image buffer")
    };
    image.validate(buffer.bytes())?;
    Ok(image)
}
fn fitted(w: u32, h: u32, maxw: u32, maxh: u32, maxpixels: usize) -> Result<(u32, u32)> {
    ensure!(w > 0 && h > 0 && maxpixels > 0, "invalid image dimensions");
    let factor = 1.0_f64
        .min(f64::from(maxw) / f64::from(w))
        .min(f64::from(maxh) / f64::from(h))
        .min((maxpixels as f64 / (f64::from(w) * f64::from(h))).sqrt());
    Ok((
        (f64::from(w) * factor).floor().max(1.0) as u32,
        (f64::from(h) * factor).floor().max(1.0) as u32,
    ))
}
struct Node {
    op: String,
    settings: Value,
    info: NodeInfo,
    control: Control,
    input: Option<MediaBuffer>,
    outputs: VecDeque<MediaBuffer>,
    base: Option<MediaBuffer>,
    draining: bool,
    emitted: bool,
}
impl Node {
    fn dimensions(&self, input: &MediaBuffer) -> Result<(u32, u32)> {
        let s = &self.settings;
        if self.op == "decode" {
            ensure!(
                input.len() <= setting(s, "max_input_bytes", 40 * 1024 * 1024)? as usize,
                "encoded image exceeds limit"
            );
            let p = system::probe(input.bytes())?;
            ensure!(
                p.width <= dimension(s, "max_source_width", 16384)?
                    && p.height <= dimension(s, "max_source_height", 65536)?
                    && p.frames <= 1024
                    && u64::from(p.width) * u64::from(p.height)
                        <= setting(s, "max_source_pixels", 64 * 1024 * 1024)?,
                "image source dimensions/frame count exceed limit"
            );
            let (w, h) = if p.orientation >= 5 {
                (p.height, p.width)
            } else {
                (p.width, p.height)
            };
            let (w, h) = fitted(
                w,
                h,
                dimension(s, "max_width", 16384)?,
                dimension(s, "max_height", 65536)?,
                setting(s, "max_pixels", 64 * 1024 * 1024)? as usize,
            )?;
            // ImageIO rounds thumbnail aspect ratios; reserve a one-pixel margin.
            return Ok((w.saturating_add(1), h.saturating_add(1)));
        }
        if self.op == "from_video" {
            let FrameInfo::Video {
                width,
                height,
                format,
                planes,
                ..
            } = &input.header.info
            else {
                bail!("expected video frame")
            };
            ensure!(
                format == "rgba"
                    && planes.len() == 1
                    && planes[0].stride == *width as usize * 4
                    && planes[0].rows == *height as usize,
                "expected packed RGBA video"
            );
            return Ok((*width, *height));
        }
        let p = image(input)?;
        match self.op.as_str() {
            "scale" => {
                if s["fit"].as_bool().unwrap_or(false) {
                    fitted(
                        p.width,
                        p.height,
                        dimension(s, "width", p.width)?,
                        dimension(s, "height", p.height)?,
                        setting(s, "max_pixels", 64 * 1024 * 1024)? as usize,
                    )
                } else {
                    Ok((
                        dimension(s, "width", p.width)?,
                        dimension(s, "height", p.height)?,
                    ))
                }
            }
            "crop" => {
                let x = setting(s, "x", 0)?;
                let y = setting(s, "y", 0)?;
                let w = dimension(s, "width", p.width)?;
                let h = dimension(s, "height", p.height)?;
                ensure!(
                    x <= u64::from(p.width)
                        && u64::from(w) <= u64::from(p.width) - x
                        && y <= u64::from(p.height)
                        && u64::from(h) <= u64::from(p.height) - y,
                    "crop outside image"
                );
                Ok((w, h))
            }
            "tile" => Ok((p.width, p.height.min(dimension(s, "height", 2048)?))),
            "compose" => {
                let p = if let Some(base) = &self.base {
                    image(base)?
                } else {
                    p
                };
                Ok((p.width, p.height))
            }
            "orient" => {
                if matches!(setting(s, "orientation", 1)?, 5..=8) {
                    Ok((p.height, p.width))
                } else {
                    Ok((p.width, p.height))
                }
            }
            _ => Ok((p.width, p.height)),
        }
    }
    fn process(&mut self, input: MediaBuffer) -> Result<()> {
        let settings = &self.settings;
        let output = match self.op.as_str() {
            "probe" => {
                ensure!(
                    input.len() <= setting(settings, "max_input_bytes", 40 * 1024 * 1024)? as usize,
                    "encoded image exceeds limit"
                );
                let p = system::probe(input.bytes())?;
                let end = p
                    .format
                    .iter()
                    .position(|b| *b == 0)
                    .unwrap_or(p.format.len());
                let value = json!({"width":p.width,"height":p.height,"frames":p.frames,"orientation":p.orientation,"format":String::from_utf8_lossy(&p.format[..end]),"provider":prefix()});
                bytes(&input, json_to_bytes(&value)?, "json")
            }
            "from_video" => {
                let (w, h) = self.dimensions(&input)?;
                let mut output = input;
                output.header.info = FrameInfo::Image {
                    image: ImageInfo::rgba(w, h)?,
                    pts_us: output.header.pts_us(),
                };
                output
            }
            "decode" => {
                let p = system::Pixels::decode(
                    input.bytes(),
                    dimension(settings, "max_width", 16384)?,
                    dimension(settings, "max_height", 65536)?,
                    setting(settings, "max_pixels", 64 * 1024 * 1024)? as usize,
                    self.info.max_output_bytes,
                )?;
                let info = ImageInfo::rgba(p.width, p.height)?;
                ensure!(
                    p.stride == info.stride && p.size == info.byte_len()?,
                    "invalid system image layout"
                );
                let mut header = input.header.clone();
                header.info = FrameInfo::Image {
                    image: info,
                    pts_us: input.header.pts_us(),
                };
                MediaBuffer::from_owner(header, Box::new(p), None)
            }
            "scale" => {
                let (w, h) = self.dimensions(&input)?;
                let p = image(&input)?;
                if w == p.width && h == p.height {
                    input
                } else {
                    let pixels =
                        system::Pixels::scale(input.bytes(), p, w, h, self.info.max_output_bytes)?;
                    let mut header = input.header.clone();
                    header.info = FrameInfo::Image {
                        image: ImageInfo::rgba(w, h)?,
                        pts_us: input.header.pts_us(),
                    };
                    MediaBuffer::from_owner(header, Box::new(pixels), None)
                }
            }
            "crop" => {
                let (w, h) = self.dimensions(&input)?;
                let info = image(&input)?;
                let x = setting(settings, "x", 0)? as usize;
                let y = setting(settings, "y", 0)? as usize;
                let mut pixels = Vec::with_capacity(w as usize * h as usize * 4);
                for row in y..y + h as usize {
                    self.control.check()?;
                    let start = row * info.stride + x * 4;
                    pixels.extend_from_slice(&input.bytes()[start..start + w as usize * 4]);
                }
                rgba(&input, w, h, pixels)?
            }
            "alpha" => {
                let p = image(&input)?;
                let mut pixels = input.bytes().to_vec();
                let bg = settings
                    .get("background")
                    .and_then(Value::as_array)
                    .context("alpha background must contain three bytes")?;
                ensure!(bg.len() == 3, "invalid alpha background");
                let mut background = [0u16; 3];
                for i in 0..3 {
                    background[i] =
                        u8::try_from(bg[i].as_u64().context("invalid background")?)? as u16;
                }
                for row in pixels.chunks_mut(p.stride) {
                    self.control.check()?;
                    for pixel in row[..p.width as usize * 4]
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                    {
                        let a = u16::from(pixel[3]);
                        for c in 0..3 {
                            pixel[c] = ((u16::from(pixel[c]) * a + background[c] * (255 - a) + 127)
                                / 255) as u8;
                        }
                        pixel[3] = 255;
                    }
                }
                rgba(&input, p.width, p.height, pixels)?
            }
            "orient" => {
                let p = image(&input)?;
                let o = setting(settings, "orientation", 1)?;
                ensure!((1..=8).contains(&o), "invalid orientation");
                let (w, h) = self.dimensions(&input)?;
                let mut pixels = vec![0; w as usize * h as usize * 4];
                for y in 0..h {
                    self.control.check()?;
                    for x in 0..w {
                        let (sx, sy) = match o {
                            1 => (x, y),
                            2 => (p.width - 1 - x, y),
                            3 => (p.width - 1 - x, p.height - 1 - y),
                            4 => (x, p.height - 1 - y),
                            5 => (y, x),
                            6 => (y, p.height - 1 - x),
                            7 => (p.width - 1 - y, p.height - 1 - x),
                            8 => (p.width - 1 - y, x),
                            _ => unreachable!(),
                        };
                        let a = sy as usize * p.stride + sx as usize * 4;
                        let b = (y as usize * w as usize + x as usize) * 4;
                        pixels[b..b + 4].copy_from_slice(&input.bytes()[a..a + 4]);
                    }
                }
                rgba(&input, w, h, pixels)?
            }
            "tile" => {
                let p = image(&input)?;
                let step = dimension(settings, "height", 2048)?;
                for y in (0..p.height).step_by(step as usize) {
                    let h = (p.height - y).min(step);
                    let mut info = p.clone();
                    info.height = h;
                    let mut header = input.header.clone();
                    header.info = FrameInfo::Image {
                        image: info,
                        pts_us: input.header.pts_us(),
                    };
                    self.outputs.push_back(MediaBuffer::from_owner(
                        header,
                        Box::new(Slice {
                            image: input.clone(),
                            offset: y as usize * p.stride,
                            len: h as usize * p.stride,
                        }),
                        None,
                    ));
                }
                return Ok(());
            }
            "compose" => {
                let Some(base) = self.base.take() else {
                    self.base = Some(input);
                    return Ok(());
                };
                let b = image(&base)?;
                let patch = image(&input)?;
                let x = setting(settings, "x", 0)?;
                let y = setting(settings, "y", 0)?;
                ensure!(
                    x <= u64::from(b.width)
                        && u64::from(patch.width) <= u64::from(b.width) - x
                        && y <= u64::from(b.height)
                        && u64::from(patch.height) <= u64::from(b.height) - y,
                    "patch outside canvas"
                );
                let mut pixels = base.bytes().to_vec();
                for row in 0..patch.height as usize {
                    self.control.check()?;
                    let start = (y as usize + row) * b.stride + x as usize * 4;
                    pixels[start..start + patch.width as usize * 4].copy_from_slice(
                        &input.bytes()
                            [row * patch.stride..row * patch.stride + patch.width as usize * 4],
                    );
                }
                rgba(&base, b.width, b.height, pixels)?
            }
            "encode" => {
                let p = image(&input)?;
                let format = settings["format"].as_str().unwrap_or("png");
                ensure!(
                    matches!(format, "jpeg" | "png"),
                    "unsupported image encoding"
                );
                let quality = settings["quality"].as_f64().unwrap_or(0.95);
                ensure!(
                    quality.is_finite() && (0.0..=1.0).contains(&quality),
                    "invalid image quality"
                );
                let encoded = system::Pixels::encode(
                    input.bytes(),
                    p,
                    format == "jpeg",
                    quality,
                    self.info.max_output_bytes,
                )?;
                let mut header = input.header.clone();
                header.info = FrameInfo::Bytes {
                    format: format.into(),
                    offset: None,
                };
                MediaBuffer::from_owner(header, Box::new(encoded), None)
            }
            other => bail!("unknown image operation {other}"),
        };
        output.validate()?;
        self.outputs.push_back(output);
        Ok(())
    }
}
fn json_to_bytes(value: &Value) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(value)?)
}
fn bytes(input: &MediaBuffer, data: Vec<u8>, format: &str) -> MediaBuffer {
    let mut header = input.header.clone();
    header.info = FrameInfo::Bytes {
        format: format.into(),
        offset: None,
    };
    MediaBuffer::new(header, vec![data])
}
fn rgba(input: &MediaBuffer, w: u32, h: u32, data: Vec<u8>) -> Result<MediaBuffer> {
    let mut header = input.header.clone();
    header.info = FrameInfo::Image {
        image: ImageInfo::rgba(w, h)?,
        pts_us: input.header.pts_us(),
    };
    let out = MediaBuffer::new(header, vec![data]);
    out.validate()?;
    Ok(out)
}
struct Slice {
    image: MediaBuffer,
    offset: usize,
    len: usize,
}
impl PlaneOwner for Slice {
    fn plane(&self, i: usize) -> &[u8] {
        if i == 0 {
            &self.image.bytes()[self.offset..self.offset + self.len]
        } else {
            &[]
        }
    }
}
impl Component for Node {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, input: &MediaBuffer) -> Result<Submit> {
        self.control.check()?;
        ensure!(!self.draining, "image input after drain");
        if self.input.is_some() || !self.outputs.is_empty() {
            return Ok(Submit::WouldBlock);
        }
        input.validate()?;
        self.input = Some(input.clone());
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> Result<Receive> {
        self.control.check()?;
        if let Some(out) = self.outputs.pop_front() {
            return Ok(Receive::Output(out));
        }
        if self.op == "capabilities" && !self.emitted {
            self.emitted = true;
            let formats = [
                "public.jpeg",
                "public.png",
                "org.webmproject.webp",
                "com.compuserve.gif",
                "public.tiff",
                "com.microsoft.bmp",
            ];
            let data = json_to_bytes(
                &json!({"provider":prefix(),"decode":formats.iter().filter(|s|system::supports(s,false)).collect::<Vec<_>>(),"encode":formats.iter().filter(|s|system::supports(s,true)).collect::<Vec<_>>() }),
            )?;
            return Ok(Receive::Output(MediaBuffer::new(
                BufferHeader {
                    stream_id: 0,
                    generation: 0,
                    info: FrameInfo::Bytes {
                        format: "json".into(),
                        offset: None,
                    },
                },
                vec![data],
            )));
        }
        if let Some(input) = &self.input {
            if !matches!(self.op.as_str(), "encode" | "probe") {
                let (w, h) = self.dimensions(input)?;
                let bound = ImageInfo::rgba(w, h)?.byte_len()?;
                ensure!(bound <= 256 * 1024 * 1024, "image output exceeds limit");
                if bound != self.info.max_output_bytes {
                    self.info.max_output_bytes = bound;
                    return Ok(Receive::Reconfigured);
                }
            }
            let input = self.input.take().unwrap();
            self.process(input)?;
            if let Some(out) = self.outputs.pop_front() {
                return Ok(Receive::Output(out));
            }
        }
        if self.draining || self.op == "capabilities" {
            ensure!(self.base.is_none(), "compose requires a canvas and a patch");
            Ok(Receive::End)
        } else {
            Ok(Receive::NeedInput)
        }
    }
    fn drain(&mut self) -> Result<()> {
        self.draining = true;
        Ok(())
    }
    fn reset(&mut self, _generation: u64, _target_us: i64) -> Result<()> {
        self.input = None;
        self.base = None;
        self.outputs.clear();
        self.draining = false;
        self.emitted = false;
        Ok(())
    }
}
remdia_graph::export_plugin!(descriptors, create);
