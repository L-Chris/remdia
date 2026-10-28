use super::bitstream::Configuration;
use anyhow::{Context, Result, ensure};
use remdia_graph::*;
use std::{
    ffi::c_void,
    ptr,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
struct PixelInfo {
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    matrix: u32,
    range: u32,
    primaries: u32,
    transfer: u32,
    sar_num: i32,
    sar_den: i32,
    surface_bytes: u64,
}
type Callback = unsafe extern "C" fn(*mut c_void, *const c_void, i32, *mut c_void, i64, i64, i32);
unsafe extern "C" {
    fn rm_vt_create(
        hevc: i32,
        sets: *const *const u8,
        sizes: *const usize,
        count: usize,
        length: i32,
        hardware: i32,
        callback: Callback,
        context: *mut c_void,
        out: *mut *mut c_void,
    ) -> i32;
    fn rm_vt_decode(
        v: *mut c_void,
        data: *const u8,
        size: usize,
        pts: i64,
        dts: i64,
        duration: i64,
        keyframe: i32,
        ticket: *const c_void,
    ) -> i32;
    fn rm_vt_drain(v: *mut c_void) -> i32;
    fn rm_vt_close(v: *mut c_void);
    fn rm_vt_hardware(v: *mut c_void) -> i32;
    fn rm_pixel_info(v: *mut c_void, image: *mut c_void, out: *mut PixelInfo) -> i32;
    fn rm_pixel_copy(
        image: *mut c_void,
        info: *const PixelInfo,
        y: *mut u8,
        ysize: usize,
        uv: *mut u8,
        uvsize: usize,
    ) -> i32;
    fn rm_pixel_release(image: *mut c_void);
    fn rm_pixel_info_size() -> usize;
}
fn check(status: i32, operation: &str) -> Result<()> {
    let detail = match status {
        -100001 => "callback escaped synchronous decode",
        -100002 => "allocation failed",
        -100003 => "unsupported pixel buffer format",
        -100004 => "invalid or unsupported clean aperture/plane layout",
        -100005 => "HDR requires tone mapping support",
        _ => "VideoToolbox OSStatus",
    };
    ensure!(status == 0, "{operation}: {detail} ({status})");
    Ok(())
}
struct Image(*mut c_void);
// SAFETY: The callback owns a retained, immutable CVPixelBuffer; only its owner
// maps it for reading. CoreVideo reference destruction is thread-safe.
unsafe impl Send for Image {}
impl Drop for Image {
    fn drop(&mut self) {
        unsafe {
            rm_pixel_release(self.0);
        }
    }
}
#[derive(Clone, Copy)]
struct Ticket {
    generation: u64,
    sequence: u64,
    pts: i64,
}
struct Completion {
    ticket: Ticket,
    status: i32,
    image: Option<Image>,
    pts: i64,
    duration: i64,
}
#[derive(Default)]
struct CallbackState {
    events: Mutex<Vec<Completion>>,
    failed: AtomicBool,
}
unsafe extern "C" fn output(
    context: *mut c_void,
    ticket: *const c_void,
    status: i32,
    image: *mut c_void,
    pts: i64,
    duration: i64,
    valid_pts: i32,
) {
    // The bridge transfers the image reference; even panic paths must release it.
    let image = (!image.is_null()).then_some(Image(image));
    let state = unsafe { &*context.cast::<CallbackState>() };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let ticket = unsafe { *ticket.cast::<Ticket>() };
        let mut events = state.events.lock().unwrap();
        if events.len() >= 2 {
            state.failed.store(true, Ordering::Release);
            return;
        }
        events.push(Completion {
            ticket,
            status,
            image,
            pts: if valid_pts != 0 { pts } else { ticket.pts },
            duration,
        });
    }));
    if result.is_err() {
        state.failed.store(true, Ordering::Release);
    }
}
struct Session(*mut c_void);
impl Session {
    fn close(&mut self) {
        unsafe {
            rm_vt_close(self.0);
        }
        self.0 = ptr::null_mut();
    }
    fn create(config: &Configuration, mode: i32, state: &CallbackState) -> Result<Self> {
        let pointers: Vec<_> = config.sets.iter().map(|b| b.as_ptr()).collect();
        let sizes: Vec<_> = config.sets.iter().map(Vec::len).collect();
        let mut session = ptr::null_mut();
        let create = |mode, session: &mut *mut c_void| unsafe {
            rm_vt_create(
                config.hevc as i32,
                pointers.as_ptr(),
                sizes.as_ptr(),
                pointers.len(),
                config.length as i32,
                mode,
                output,
                (state as *const CallbackState).cast_mut().cast(),
                session,
            )
        };
        let mut status = create(mode, &mut session);
        if status != 0 && mode == 1 {
            status = create(0, &mut session);
        }
        check(status, "create macOS decoder for this codec/profile")?;
        Ok(Self(session))
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}
struct Decoded {
    completion: Completion,
    info: PixelInfo,
}
pub struct Decoder {
    // Close VT before dropping its callback context, including during unwinding.
    session: Session,
    state: Box<CallbackState>,
    config: Configuration,
    info: NodeInfo,
    track: Track,
    control: Control,
    pending: Option<MediaBuffer>,
    images: Vec<Decoded>,
    generation: u64,
    sequence: u64,
    last_pts: Option<i64>,
    draining: bool,
    hardware_mode: i32,
    frame_limit: usize,
    private_limit: usize,
}
fn plane_sizes(width: u32, height: u32) -> Result<(usize, usize)> {
    let w = width as usize;
    let h = height as usize;
    ensure!(w > 0 && h > 0, "empty decoded picture");
    Ok((
        w.checked_mul(h).context("luma allocation overflow")?,
        w.div_ceil(2)
            .checked_mul(2)
            .and_then(|w| w.checked_mul(h.div_ceil(2)))
            .context("chroma allocation overflow")?,
    ))
}
impl Decoder {
    pub fn new(c: &NodeConfig, control: Control) -> Result<Self> {
        control.check()?;
        ensure!(
            unsafe { rm_pixel_info_size() } == std::mem::size_of::<PixelInfo>(),
            "private C bridge layout mismatch"
        );
        let track: Track = serde_json::from_value(
            c.settings
                .get("track")
                .cloned()
                .context("missing macOS stream configuration")?,
        )?;
        ensure!(
            track.kind == "video",
            "macOS decoder requires a video track"
        );
        let config = Configuration::parse(&track)?;
        let frame_limit = c
            .settings
            .get("max_frame_bytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(128 * 1024 * 1024) as usize;
        let private_limit = c
            .settings
            .get("max_decoder_buffer_bytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(96 * 1024 * 1024) as usize;
        let (y, uv) = plane_sizes(track.width, track.height)?;
        let maximum = y.checked_add(uv).context("frame allocation overflow")?;
        ensure!(maximum <= frame_limit, "video frame exceeds safety ceiling");
        ensure!(
            maximum
                .checked_mul(config.reorder + 1)
                .is_some_and(|n| n <= private_limit),
            "reorder surfaces exceed decoder budget"
        );
        let hardware_mode = match c.settings.get("hardware_mode").and_then(|v| v.as_str()) {
            Some("require") => 2,
            Some("software") => 0,
            Some("auto") | None => i32::from(
                c.settings
                    .get("hardware")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
            ),
            Some(_) => anyhow::bail!("invalid hardware mode"),
        };
        let state = Box::<CallbackState>::default();
        let session = Session::create(&config, hardware_mode, &state)?;
        let mut output = PortCaps::new(DataKind::Video);
        output.format = Some("nv12".into());
        Ok(Self {
            session,
            state,
            config,
            info: NodeInfo {
                input: Some(PortCaps::new(DataKind::Packet)),
                output,
                max_output_bytes: maximum,
                media: None,
            },
            track,
            control,
            pending: None,
            images: Vec::new(),
            generation: 1,
            sequence: 0,
            last_pts: None,
            draining: false,
            hardware_mode,
            frame_limit,
            private_limit,
        })
    }
    fn decode(&mut self, b: MediaBuffer) -> Result<()> {
        let FrameInfo::Packet {
            pts,
            dts,
            duration,
            time_base,
            keyframe,
            ..
        } = b.header.info
        else {
            anyhow::bail!("macOS decoder expected packet");
        };
        self.config.validate_packet(b.bytes())?;
        let nanos = Rational {
            num: 1,
            den: 1_000_000_000,
        };
        let ticket = Ticket {
            generation: b.header.generation,
            sequence: self.sequence,
            pts: time_base.rescale(pts, Rational::MICROS)?,
        };
        self.sequence = self
            .sequence
            .checked_add(1)
            .context("decode sequence exhausted")?;
        let status = unsafe {
            rm_vt_decode(
                self.session.0,
                b.bytes().as_ptr(),
                b.bytes().len(),
                time_base.rescale(pts, nanos)?,
                time_base.rescale(dts, nanos)?,
                time_base.rescale(i64::try_from(duration)?, nanos)?,
                i32::from(keyframe == Some(true)),
                (&ticket as *const Ticket).cast(),
            )
        };
        check(status, "decode macOS video packet")?;
        ensure!(
            !self.state.failed.load(Ordering::Acquire),
            "macOS output callback failed"
        );
        let events: Vec<_> = self.state.events.lock().unwrap().drain(..).collect();
        for event in events {
            check(event.status, "macOS video output")?;
            if event.ticket.generation != self.generation {
                continue;
            }
            if let Some(image) = &event.image {
                let mut info = PixelInfo::default();
                check(
                    unsafe { rm_pixel_info(self.session.0, image.0, &mut info) },
                    "describe macOS pixel buffer",
                )?;
                let (y, uv) = plane_sizes(info.width, info.height)?;
                let size = y.checked_add(uv).context("frame size overflow")?;
                ensure!(
                    size <= self.frame_limit,
                    "decoded frame exceeds safety ceiling"
                );
                ensure!(
                    size.checked_mul(self.config.reorder + 1)
                        .is_some_and(|n| n <= self.private_limit),
                    "reorder surfaces exceed decoder budget"
                );
                let retained = self
                    .images
                    .iter()
                    .try_fold(info.surface_bytes, |n, f| {
                        n.checked_add(f.info.surface_bytes)
                    })
                    .context("surface byte count overflow")?;
                ensure!(
                    retained <= self.private_limit as u64,
                    "retained VideoToolbox surfaces exceed decoder budget"
                );
                self.images.push(Decoded {
                    completion: event,
                    info,
                });
            }
        }
        ensure!(
            self.images.len() <= self.config.reorder + 1,
            "decoder exceeded bounded reorder queue"
        );
        self.images
            .sort_by_key(|f| (f.completion.pts, f.completion.ticket.sequence));
        Ok(())
    }
}
impl Component for Decoder {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, b: &MediaBuffer) -> Result<Submit> {
        self.control.check()?;
        ensure!(!self.draining, "submit after macOS decoder drain");
        ensure!(
            b.header.stream_id == self.track.index as u32,
            "wrong video stream"
        );
        ensure!(
            b.len() <= 16 * 1024 * 1024,
            "oversize compressed video packet"
        );
        if self.pending.is_some() || self.images.len() > self.config.reorder {
            return Ok(Submit::WouldBlock);
        }
        self.pending = Some(b.clone());
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> Result<Receive> {
        self.control.check()?;
        if let Some(packet) = self.pending.take() {
            self.decode(packet)?;
        }
        if self.images.len() > self.config.reorder || (self.draining && !self.images.is_empty()) {
            let f = &self.images[0];
            let (ylen, uvlen) = plane_sizes(f.info.width, f.info.height)?;
            let size = ylen + uvlen;
            if size != self.info.max_output_bytes {
                self.info.max_output_bytes = size;
                return Ok(Receive::Reconfigured);
            }
            let f = self.images.remove(0);
            ensure!(
                self.last_pts.is_none_or(|v| f.completion.pts >= v),
                "stream violated its declared video reorder bound"
            );
            self.last_pts = Some(f.completion.pts);
            let mut y = vec![0; ylen];
            let mut uv = vec![0; uvlen];
            check(
                unsafe {
                    rm_pixel_copy(
                        f.completion.image.as_ref().unwrap().0,
                        &f.info,
                        y.as_mut_ptr(),
                        ylen,
                        uv.as_mut_ptr(),
                        uvlen,
                    )
                },
                "copy macOS video planes",
            )?;
            let i = f.info;
            let info = FrameInfo::Video {
                pts_us: Some(f.completion.pts),
                duration_us: f.completion.duration,
                width: i.width,
                height: i.height,
                format: "nv12".into(),
                planes: vec![
                    Plane {
                        stride: i.width as usize,
                        rows: i.height as usize,
                    },
                    Plane {
                        stride: i.width.div_ceil(2) as usize * 2,
                        rows: i.height.div_ceil(2) as usize,
                    },
                ],
                sample_aspect_ratio: Rational {
                    num: i.sar_num,
                    den: i.sar_den,
                },
                color_space: match i.matrix {
                    1 => "bt709",
                    2 => "bt601",
                    9 => "bt2020",
                    _ => "unspecified",
                }
                .into(),
                color_range: if i.range != 0 { "full" } else { "limited" }.into(),
                color_primaries: i.primaries.to_string(),
                color_transfer: i.transfer.to_string(),
                hardware_decoded: unsafe { rm_vt_hardware(self.session.0) } != 0,
            };
            return Ok(Receive::Output(MediaBuffer::new(
                BufferHeader {
                    stream_id: self.track.index as u32,
                    generation: f.completion.ticket.generation,
                    info,
                },
                vec![y, uv],
            )));
        }
        Ok(if self.draining {
            Receive::End
        } else {
            Receive::NeedInput
        })
    }
    fn drain(&mut self) -> Result<()> {
        self.control.check()?;
        ensure!(
            self.pending.is_none(),
            "drain before accepted packet was processed"
        );
        check(
            unsafe { rm_vt_drain(self.session.0) },
            "drain macOS decoder",
        )?;
        self.draining = true;
        Ok(())
    }
    fn reset(&mut self, generation: u64, _target: i64) -> Result<()> {
        self.control.check()?;
        self.session.close();
        self.state.events.lock().unwrap().clear();
        self.state.failed.store(false, Ordering::Release);
        self.images.clear();
        self.pending = None;
        self.last_pts = None;
        self.generation = generation;
        self.sequence = 0;
        self.draining = false;
        self.session = Session::create(&self.config, self.hardware_mode, &self.state)?;
        Ok(())
    }
}
