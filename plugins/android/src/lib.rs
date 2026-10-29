//! Independent Android system demux, decoder, encoder and mux factories.
use anyhow::Result;
use remdia_graph::*;
#[cfg(any(target_os = "android", test))]
mod pcm;
#[cfg(all(target_os = "android", not(feature = "builtin")))]
remdia_android_platform::jni_on_load!();

fn descriptors() -> Vec<ComponentDescriptor> {
    if !cfg!(target_os = "android") {
        return Vec::new();
    }
    [
        ("demux", "demux", None, DataKind::Packet),
        ("demux.capabilities", "source", None, DataKind::Bytes),
        ("audio.capabilities", "source", None, DataKind::Bytes),
        ("video.capabilities", "source", None, DataKind::Bytes),
        (
            "audio.decode",
            "decoder",
            Some(DataKind::Packet),
            DataKind::Audio,
        ),
        (
            "audio.convert",
            "filter",
            Some(DataKind::Audio),
            DataKind::Audio,
        ),
        (
            "video.decode",
            "decoder",
            Some(DataKind::Packet),
            DataKind::Video,
        ),
        (
            "audio.encode",
            "encoder",
            Some(DataKind::Audio),
            DataKind::Packet,
        ),
        ("mux", "mux", Some(DataKind::Packet), DataKind::Bytes),
    ]
    .into_iter()
    .map(|(name, role, input, output)| ComponentDescriptor {
        id: format!("android.{name}"),
        role: role.into(),
        input: input.map(PortCaps::new),
        output: PortCaps::new(output),
    })
    .collect()
}
fn create(config: &NodeConfig, control: Control) -> Result<Box<dyn Component>> {
    #[cfg(target_os = "android")]
    {
        control.check()?;
        if config.component == "android.audio.convert" {
            return Ok(Box::new(pcm::Convert::new(config, control)?));
        }
        let node = remdia_android_platform::Node::open(&config.component, &config.settings)?;
        let info = serde_json::from_value(node.info()?)?;
        Ok(Box::new(Android {
            node,
            info,
            control,
        }))
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (config, control);
        anyhow::bail!("Android capability pack requires Android API 28+")
    }
}
#[cfg(target_os = "android")]
struct Android {
    node: remdia_android_platform::Node,
    info: NodeInfo,
    control: Control,
}
#[cfg(target_os = "android")]
impl Component for Android {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, input: &MediaBuffer) -> Result<Submit> {
        self.control.check()?;
        input.validate()?;
        Ok(
            if self
                .node
                .submit(&serde_json::to_string(&input.header)?, input.bytes())?
            {
                Submit::Accepted
            } else {
                Submit::WouldBlock
            },
        )
    }
    fn receive(&mut self) -> Result<Receive> {
        self.control.check()?;
        // Framework format changes return an explicit reconfiguration event
        // before any pixels in the new layout are copied. Avoid JNI/JSON info
        // queries on every empty codec poll.
        let result = self.node.receive(self.info.max_output_bytes)?;
        self.control.check()?;
        match result {
            Some((header, bytes)) => {
                if header["reconfigured"] == true {
                    self.info = serde_json::from_value(self.node.info()?)?;
                    return Ok(Receive::Reconfigured);
                }
                let header: BufferHeader = serde_json::from_value(header)?;
                let lengths = match &header.info {
                    FrameInfo::Video { planes, .. } => planes
                        .iter()
                        .map(|p| {
                            p.stride
                                .checked_mul(p.rows)
                                .ok_or_else(|| anyhow::anyhow!("plane size overflow"))
                        })
                        .collect::<Result<Vec<_>>>()?,
                    _ => vec![bytes.len()],
                };
                let mut offset = 0usize;
                let mut ranges = Vec::new();
                for length in lengths {
                    let end = offset
                        .checked_add(length)
                        .ok_or_else(|| anyhow::anyhow!("plane size overflow"))?;
                    anyhow::ensure!(end <= bytes.len(), "truncated Android plane");
                    ranges.push(offset..end);
                    offset = end;
                }
                anyhow::ensure!(offset == bytes.len(), "unaccounted Android plane bytes");
                let buffer =
                    MediaBuffer::from_owner(header, Box::new(Packed { bytes, ranges }), None);
                buffer.validate()?;
                Ok(Receive::Output(buffer))
            }
            None if self.node.ended()? => Ok(Receive::End),
            None => Ok(Receive::NeedInput),
        }
    }
    fn drain(&mut self) -> Result<()> {
        self.control.check()?;
        self.node.drain()
    }
    fn reset(&mut self, generation: u64, target: i64) -> Result<()> {
        self.control.check()?;
        self.node.reset(generation, target)
    }
    fn command(&mut self, command: &serde_json::Value) -> Result<Option<MediaBuffer>> {
        self.control.check()?;
        let output = self.node.command(command, 8 * 1024 * 1024)?;
        self.control.check()?;
        output
            .map(|(header, bytes)| {
                let buffer = MediaBuffer::new(serde_json::from_value(header)?, vec![bytes]);
                buffer.validate()?;
                Ok(buffer)
            })
            .transpose()
    }
}
#[cfg(target_os = "android")]
struct Packed {
    bytes: Vec<u8>,
    ranges: Vec<std::ops::Range<usize>>,
}
#[cfg(target_os = "android")]
impl PlaneOwner for Packed {
    fn plane(&self, index: usize) -> &[u8] {
        self.ranges
            .get(index)
            .map_or(&[], |r| &self.bytes[r.clone()])
    }
}
remdia_graph::export_plugin!(descriptors, create);
