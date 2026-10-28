use anyhow::{Context, Result, ensure};
use image_webp::WebPDecoder;
use remdia_graph::*;
use serde_json::json;
use std::io::Cursor;

pub struct Decoder {
    info: NodeInfo,
    control: Control,
    input: Option<MediaBuffer>,
    draining: bool,
    probe: bool,
    max_input: usize,
    max_width: u32,
    max_height: u32,
}
impl Decoder {
    pub fn new(config: &NodeConfig, control: Control) -> Result<Self> {
        let probe = config.component == "fallback.webp.probe";
        ensure!(
            probe || config.component == "fallback.webp.decode",
            "unknown WebP operation"
        );
        let max_input = config.settings["max_input_bytes"]
            .as_u64()
            .unwrap_or(40 * 1024 * 1024) as usize;
        ensure!(
            max_input > 0 && max_input <= 256 * 1024 * 1024,
            "invalid WebP input limit"
        );
        let max_width = u32::try_from(
            config.settings["max_source_width"]
                .as_u64()
                .unwrap_or(16384),
        )?;
        let max_height = u32::try_from(
            config.settings["max_source_height"]
                .as_u64()
                .unwrap_or(65536),
        )?;
        Ok(Self {
            info: NodeInfo {
                input: Some(PortCaps::new(DataKind::Bytes)),
                output: PortCaps::new(if probe {
                    DataKind::Bytes
                } else {
                    DataKind::Image
                }),
                max_output_bytes: if probe { 16384 } else { 1 },
                media: None,
            },
            control,
            input: None,
            draining: false,
            probe,
            max_input,
            max_width,
            max_height,
        })
    }
}
impl Component for Decoder {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, input: &MediaBuffer) -> Result<Submit> {
        self.control.check()?;
        ensure!(!self.draining, "WebP input after drain");
        if self.input.is_some() {
            return Ok(Submit::WouldBlock);
        }
        ensure!(
            !input.is_empty() && input.len() <= self.max_input,
            "WebP input exceeds limit"
        );
        self.input = Some(input.clone());
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> Result<Receive> {
        self.control.check()?;
        let Some(input) = &self.input else {
            return Ok(if self.draining {
                Receive::End
            } else {
                Receive::NeedInput
            });
        };
        let mut decoder =
            WebPDecoder::new(Cursor::new(input.bytes())).context("invalid WebP input")?;
        decoder.set_memory_limit(1024 * 1024);
        if !self.probe {
            ensure!(
                decoder.icc_profile()?.is_none() && decoder.exif_metadata()?.is_none(),
                "WebP ICC/EXIF metadata requires a system WebP codec"
            );
        }
        let (w, h) = decoder.dimensions();
        let info = ImageInfo::rgba(w, h)?;
        ensure!(
            w <= self.max_width && h <= self.max_height && info.byte_len()? <= 128 * 1024 * 1024,
            "WebP image exceeds decode budget"
        );
        if !self.probe && self.info.max_output_bytes != info.byte_len()? {
            self.info.max_output_bytes = info.byte_len()?;
            return Ok(Receive::Reconfigured);
        }
        let mut header = input.header.clone();
        let data = if self.probe {
            header.info = FrameInfo::Bytes {
                format: "json".into(),
                offset: None,
            };
            serde_json::to_vec(
                &json!({"width":w,"height":h,"frames":decoder.num_frames().max(1),"orientation":1,"format":"org.webmproject.webp","provider":"fallback.image-webp"}),
            )?
        } else {
            // Match the input channel count to avoid an extra RGB->RGBA buffer.
            // The codec's own workspace is separately measured from host buffers.
            let mut data = vec![
                0;
                decoder
                    .output_buffer_size()
                    .context("WebP allocation overflow")?
            ];
            decoder.read_image(&mut data)?;
            self.control.check()?;
            if !decoder.has_alpha() {
                let count = w as usize * h as usize;
                data.resize(info.byte_len()?, 0);
                for p in (0..count).rev() {
                    let rgb = [data[p * 3], data[p * 3 + 1], data[p * 3 + 2]];
                    data[p * 4..p * 4 + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
                }
            }
            header.info = FrameInfo::Image {
                image: info,
                pts_us: input.header.pts_us(),
            };
            data
        };
        self.input = None;
        Ok(Receive::Output(MediaBuffer::new(header, vec![data])))
    }
    fn drain(&mut self) -> Result<()> {
        self.draining = true;
        Ok(())
    }
    fn reset(&mut self, _generation: u64, _target_us: i64) -> Result<()> {
        self.input = None;
        self.draining = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    unsafe extern "C" fn running(_: *mut std::ffi::c_void) -> u8 {
        0
    }
    #[test]
    fn fallback_is_bounded_and_expands_rgb_in_place_without_channel_corruption() {
        let config = NodeConfig {
            component: "fallback.webp.decode".into(),
            settings: json!({}),
        };
        let mut node = Decoder::new(
            &config,
            Control(remdia_plugin_abi::Control {
                context: std::ptr::null_mut(),
                cancelled: running,
            }),
        )
        .unwrap();
        let bytes = include_bytes!("../../../tests/fixtures/legacy-white.webp").to_vec();
        let input = MediaBuffer::new(
            BufferHeader {
                stream_id: 0,
                generation: 0,
                info: FrameInfo::Bytes {
                    format: "webp".into(),
                    offset: None,
                },
            },
            vec![bytes],
        );
        assert!(matches!(node.submit(&input).unwrap(), Submit::Accepted));
        assert!(matches!(node.receive().unwrap(), Receive::Reconfigured));
        assert_eq!(node.info().max_output_bytes, 32 * 64 * 4);
        let Receive::Output(output) = node.receive().unwrap() else {
            panic!("expected pixels")
        };
        output.validate().unwrap();
        assert_eq!(output.len(), 32 * 64 * 4);
        assert!(
            output
                .bytes()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p == &[250, 250, 250, 255])
        );
        node.drain().unwrap();
        assert!(matches!(node.receive().unwrap(), Receive::End));
    }
}
