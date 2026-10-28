use anyhow::{Result, ensure};
use remdia_graph::*;
use std::ffi::{CStr, CString, c_char, c_void};
#[repr(C)]
#[derive(Default)]
struct Packet {
    data: *mut u8,
    size: usize,
    stream: u32,
    time_scale: i32,
    pts: i64,
    dts: i64,
    duration: i64,
    trim_start: u64,
    trim_end: u64,
    keyframe: i32,
}
unsafe extern "C" {
    fn rm_demux_supports(kind: *const c_char) -> i32;
    fn rm_demux_open(
        path: *const c_char,
        mime: *const c_char,
        reader: *mut *mut c_void,
        metadata: *mut *mut c_char,
    ) -> i32;
    fn rm_demux_read(reader: *mut c_void, limit: usize, out: *mut Packet) -> i32;
    fn rm_demux_seek(reader: *mut c_void, target: i64) -> i32;
    fn rm_demux_artwork(
        reader: *mut c_void,
        limit: usize,
        data: *mut *mut u8,
        size: *mut usize,
    ) -> i32;
    fn rm_demux_close(reader: *mut c_void);
    fn rm_demux_free(data: *mut c_void);
    fn rm_demux_error() -> *const c_char;
}
fn status(code: i32) -> Result<i32> {
    ensure!(
        code >= 0,
        "AVFoundation demux: {}",
        unsafe { CStr::from_ptr(rm_demux_error()) }.to_string_lossy()
    );
    Ok(code)
}
struct Bytes {
    data: *mut u8,
    size: usize,
}
// SAFETY: malloc-owned immutable bytes; the release function only calls free.
unsafe impl Send for Bytes {}
unsafe impl Sync for Bytes {}
impl Drop for Bytes {
    fn drop(&mut self) {
        unsafe { rm_demux_free(self.data.cast()) }
    }
}
impl PlaneOwner for Bytes {
    fn plane(&self, index: usize) -> &[u8] {
        if index == 0 && self.size > 0 {
            unsafe { std::slice::from_raw_parts(self.data, self.size) }
        } else {
            &[]
        }
    }
}
pub struct Demux {
    reader: *mut c_void,
    info: NodeInfo,
    control: Control,
    generation: u64,
    ended: bool,
    source: std::path::PathBuf,
}
impl Drop for Demux {
    fn drop(&mut self) {
        unsafe { rm_demux_close(self.reader) }
    }
}
impl Demux {
    pub fn open(config: &NodeConfig, control: Control) -> Result<Self> {
        let request: Request = serde_json::from_value(config.settings.clone())?;
        request.validate()?;
        control.check()?;
        ensure!(
            !request.source.contains("://"),
            "system demux currently requires a local file source"
        );
        let path = std::path::Path::new(&request.source).canonicalize()?;
        let mime = CString::new(super::capabilities::mime(&request.source)?)?;
        let source = path.clone();
        let path = CString::new(
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid file path"))?,
        )?;
        let mut reader = std::ptr::null_mut();
        let mut metadata = std::ptr::null_mut();
        status(unsafe { rm_demux_open(path.as_ptr(), mime.as_ptr(), &mut reader, &mut metadata) })?;
        ensure!(
            !reader.is_null() && !metadata.is_null(),
            "missing system demux result"
        );
        let mut demux = Self {
            reader,
            info: NodeInfo {
                input: None,
                output: PortCaps::new(DataKind::Packet),
                max_output_bytes: request.max_packet_bytes,
                media: None,
            },
            control,
            generation: 1,
            ended: false,
            source,
        };
        let bytes = unsafe { CStr::from_ptr(metadata) }.to_bytes();
        let media = serde_json::from_slice(bytes);
        unsafe { rm_demux_free(metadata.cast()) };
        demux.info.media = Some(media?);
        control.check()?;
        Ok(demux)
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
        let mut packet = Packet::default();
        if status(unsafe { rm_demux_read(self.reader, self.info.max_output_bytes, &mut packet) })?
            == 0
        {
            self.ended = true;
            return Ok(Receive::End);
        }
        let owner = Bytes {
            data: packet.data,
            size: packet.size,
        };
        self.control.check()?;
        ensure!(
            !packet.data.is_null()
                && packet.size <= self.info.max_output_bytes
                && packet.duration >= 0,
            "invalid system packet"
        );
        Ok(Receive::Output(MediaBuffer::from_owner(
            BufferHeader {
                stream_id: packet.stream,
                generation: self.generation,
                info: FrameInfo::Packet {
                    pts: packet.pts,
                    dts: packet.dts,
                    duration: packet.duration as u64,
                    time_base: Rational {
                        num: 1,
                        den: packet.time_scale,
                    },
                    keyframe: Some(packet.keyframe != 0),
                    trim_start: packet.trim_start,
                    trim_end: packet.trim_end,
                },
            },
            Box::new(owner),
            None,
        )))
    }
    fn drain(&mut self) -> Result<()> {
        self.ended = true;
        Ok(())
    }
    fn reset(&mut self, generation: u64, target: i64) -> Result<()> {
        self.control.check()?;
        status(unsafe { rm_demux_seek(self.reader, target) })?;
        self.generation = generation;
        self.ended = false;
        Ok(())
    }
    fn command(&mut self, command: &serde_json::Value) -> Result<Option<MediaBuffer>> {
        self.control.check()?;
        if command["op"] != "artwork" {
            return Ok(None);
        }
        let mut data = std::ptr::null_mut();
        let mut size = 0;
        if status(unsafe { rm_demux_artwork(self.reader, 8 * 1024 * 1024, &mut data, &mut size) })?
            == 0
        {
            return Ok(
                super::metadata::flac_artwork(&self.source, 8 * 1024 * 1024)?.map(|data| {
                    MediaBuffer::new(
                        BufferHeader {
                            stream_id: 0,
                            generation: self.generation,
                            info: FrameInfo::Artwork {
                                codec: "system-image".into(),
                            },
                        },
                        vec![data],
                    )
                }),
            );
        }
        let bytes = Bytes { data, size };
        ensure!(
            !data.is_null() && size <= 8 * 1024 * 1024,
            "invalid system artwork"
        );
        Ok(Some(MediaBuffer::from_owner(
            BufferHeader {
                stream_id: 0,
                generation: self.generation,
                info: FrameInfo::Artwork {
                    codec: "system-image".into(),
                },
            },
            Box::new(bytes),
            None,
        )))
    }
}

pub fn supports(kind: &str) -> Result<bool> {
    let kind = CString::new(kind)?;
    Ok(unsafe { rm_demux_supports(kind.as_ptr()) } != 0)
}
