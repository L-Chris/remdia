//! A demand-driven media SDK with a backend-independent, dynamically loaded core.
pub mod plan;
pub mod registry;
use anyhow::{Context, Result, ensure};
use registry::{Module, Registry};
pub use remdia_buffer::Budget;
use remdia_buffer::Reservation;
use remdia_plugin_abi::{self as abi, Output, View};
pub use remdia_types::*;
use std::{
    ffi::c_void,
    marker::PhantomData,
    path::Path,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
unsafe extern "C" fn cancelled(context: *mut c_void) -> u8 {
    unsafe { (&*context.cast::<Cancellation>()).is_cancelled() as u8 }
}
struct RawOutput {
    raw: Output,
    _module: Arc<Module>,
}
impl Drop for RawOutput {
    fn drop(&mut self) {
        if let Some(release) = self.raw.release {
            unsafe {
                release(self.raw.owner);
            }
        }
    }
}
impl RawOutput {
    fn new(module: Arc<Module>) -> Self {
        Self {
            raw: Output::default(),
            _module: module,
        }
    }
    fn json(&self) -> Result<&[u8]> {
        ensure!(
            self.raw.header.major == abi::ABI_MAJOR
                && self.raw.header.struct_size >= std::mem::size_of::<Output>() as u32,
            "invalid output header"
        );
        ensure!(
            self.raw.json.len <= 1024 * 1024
                && (self.raw.json.len == 0 || !self.raw.json.ptr.is_null()),
            "invalid output metadata"
        );
        Ok(unsafe { self.raw.json.as_slice() })
    }
    fn status(&self, status: i32) -> Result<bool> {
        if status == abi::OK {
            return Ok(true);
        }
        if status == abi::EOF {
            return Ok(false);
        }
        let value: serde_json::Value = serde_json::from_slice(self.json()?).unwrap_or_default();
        anyhow::bail!(
            "{}",
            value
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("plugin failed")
        )
    }
}
/// Read-only plugin-owned planes. Clones share a single reservation and release callback.
/// Frames are immutable and can cross threads. Sessions stay on the media worker.
#[derive(Clone)]
pub struct Frame(Arc<FrameInner>);
struct FrameInner {
    info: FrameInfo,
    generation: u64,
    output: RawOutput,
    _reservation: Reservation,
    _local: PhantomData<Rc<()>>,
}
// SAFETY: The v1 contract requires immutable output planes and a release callback
// callable on any thread. Native modules are pinned for the process lifetime;
// the reservation uses atomic counters. Loading a plugin is an unsafe operation.
unsafe impl Send for FrameInner {}
unsafe impl Sync for FrameInner {}
impl Frame {
    pub fn info(&self) -> &FrameInfo {
        &self.0.info
    }
    pub fn generation(&self) -> u64 {
        self.0.generation
    }
    pub fn plane(&self, index: usize) -> Option<&[u8]> {
        self.0
            .output
            .raw
            .planes
            .get(index)
            .map(|v| unsafe { v.as_slice() })
    }
    pub fn bytes(&self) -> &[u8] {
        self.plane(0).unwrap()
    }
}
pub struct Engine {
    registry: Registry,
    budget: Budget,
}
impl Engine {
    pub fn discover(directory: impl AsRef<Path>, managed_budget_bytes: usize) -> Result<Self> {
        Ok(Self {
            registry: Registry::discover(directory)?,
            budget: Budget::new(managed_budget_bytes),
        })
    }
    pub fn registry(&self) -> &Registry {
        &self.registry
    }
    pub fn budget(&self) -> &Budget {
        &self.budget
    }
    pub fn plan(&self, request: &Request) -> Result<plan::Plan> {
        plan::Plan::build(&self.registry, request, self.budget_limit())
    }
    fn budget_limit(&self) -> usize {
        self.budget.limit()
    }
    /// # Safety
    /// The selected native capability pack must be trusted and obey remdia's ABI.
    pub unsafe fn open(&self, request: Request, cancellation: Cancellation) -> Result<Session> {
        let plan = self.plan(&request)?;
        let candidate = self
            .registry
            .candidates
            .iter()
            .find(|c| c.manifest.id == plan.plugin)
            .context("planned plugin disappeared")?;
        let module = unsafe { candidate.load() }?;
        let control = Box::new(cancellation);
        let _reservation = self.budget.reserve(plan.output_reservation_bytes)?;
        let json = serde_json::to_vec(&request)?;
        let mut output = RawOutput::new(module.clone());
        let mut handle = std::ptr::null_mut();
        let status = unsafe {
            (module.api.open)(
                View::new(&json),
                abi::Control {
                    context: (&*control as *const Cancellation).cast_mut().cast(),
                    cancelled,
                },
                &mut handle,
                &mut output.raw,
            )
        };
        // Close even if a plugin returns malformed header JSON after allocating a session.
        let mut session = Session {
            handle,
            module,
            control,
            budget: self.budget.clone(),
            request,
            info: None,
            generation: 1,
            _local: PhantomData,
        };
        ensure!(
            output.status(status)? && !handle.is_null(),
            "plugin did not create a session"
        );
        session.info = Some(serde_json::from_slice(output.json()?)?);
        Ok(session)
    }
}
pub struct Session {
    handle: *mut c_void,
    module: Arc<Module>,
    control: Box<Cancellation>,
    budget: Budget,
    request: Request,
    info: Option<MediaInfo>,
    generation: u64,
    _local: PhantomData<Rc<()>>,
}
impl Drop for Session {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                (self.module.api.close)(self.handle);
            }
        }
    }
}
impl Session {
    pub fn info(&self) -> &MediaInfo {
        self.info.as_ref().unwrap()
    }
    pub fn cancellation(&self) -> Cancellation {
        (*self.control).clone()
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn read(&mut self) -> Result<Option<Frame>> {
        ensure!(!self.control.is_cancelled(), "cancelled");
        let reservation = self.budget.reserve(self.request.max_frame_bytes)?;
        let mut output = RawOutput::new(self.module.clone());
        let status = unsafe { (self.module.api.next)(self.handle, &mut output.raw) };
        self.frame(status, output, reservation, self.request.max_frame_bytes)
    }
    fn frame(
        &self,
        status: i32,
        output: RawOutput,
        reservation: Reservation,
        limit: usize,
    ) -> Result<Option<Frame>> {
        if !output.status(status)? {
            return Ok(None);
        }
        let info = serde_json::from_slice(output.json()?)?;
        let mut size = 0usize;
        for v in &output.raw.planes {
            ensure!(v.len == 0 || !v.ptr.is_null(), "null output plane");
            size = size.checked_add(v.len).context("plane size overflow")?;
        }
        ensure!(size <= limit, "plugin output exceeds reserved budget");
        Ok(Some(Frame(Arc::new(FrameInner {
            info,
            generation: self.generation,
            output,
            _reservation: reservation,
            _local: PhantomData,
        }))))
    }
    pub fn seek(&mut self, target_us: i64) -> Result<u64> {
        ensure!(target_us >= 0, "negative seek");
        ensure!(!self.control.is_cancelled(), "cancelled");
        let generation = self
            .generation
            .checked_add(1)
            .context("generation overflow")?;
        let mut output = RawOutput::new(self.module.clone());
        let status = unsafe { (self.module.api.seek)(self.handle, target_us, &mut output.raw) };
        ensure!(output.status(status)?, "seek returned EOF");
        self.generation = generation;
        Ok(generation)
    }
    pub fn artwork(&mut self) -> Result<Option<Frame>> {
        let limit = 8 * 1024 * 1024;
        let reservation = self.budget.reserve(limit)?;
        let mut out = RawOutput::new(self.module.clone());
        let status = unsafe { (self.module.api.artwork)(self.handle, &mut out.raw) };
        self.frame(status, out, reservation, limit)
    }
    /// Extract a bounded 16 kHz mono f32 chunk. Use a dedicated Task::Audio session.
    /// Missing PTS gaps become silence; frames beyond the requested end are clipped.
    pub fn audio_chunk(&mut self, start_us: i64, duration_us: i64) -> Result<AudioChunk> {
        ensure!(
            self.request.task == Task::Audio
                && self.request.audio_rate == 16000
                && self.request.audio_channels == 1,
            "audio chunks require 16kHz mono Audio session"
        );
        ensure!(
            (1..=120_000_000).contains(&duration_us),
            "chunk duration must be <=120 seconds"
        );
        let end = start_us
            .checked_add(duration_us)
            .context("range overflow")?;
        self.seek(start_us)?;
        let capacity = usize::try_from((i128::from(duration_us) * 16000 + 999_999) / 1_000_000)?;
        let reservation = self.budget.reserve(capacity * 4)?;
        let mut samples = Vec::new();
        while let Some(frame) = self.read()? {
            let FrameInfo::Audio { pts_us, .. } = *frame.info() else {
                continue;
            };
            if pts_us >= end {
                break;
            }
            let offset = (((i128::from(pts_us) - i128::from(start_us)) * 16000 + 500_000)
                / 1_000_000)
                .max(0) as usize;
            if offset > samples.len() {
                samples.resize(offset.min(capacity), 0.0);
            }
            let skip = samples.len().saturating_sub(offset);
            for bytes in frame
                .bytes()
                .as_chunks::<4>()
                .0
                .iter()
                .skip(skip)
                .take(capacity - samples.len())
            {
                samples.push(f32::from_le_bytes(*bytes));
            }
            if samples.len() == capacity {
                break;
            }
        }
        Ok(AudioChunk {
            start_us,
            samples,
            _reservation: reservation,
        })
    }
    pub fn encode_mp3(&self, chunk: &AudioChunk) -> Result<Frame> {
        let limit = 16 * 1024 * 1024;
        let reservation = self.budget.reserve(limit)?;
        // Supported platforms are little endian. Avoid an extra PCM copy.
        let pcm = unsafe {
            std::slice::from_raw_parts(chunk.samples.as_ptr().cast(), chunk.samples.len() * 4)
        };
        let mut out = RawOutput::new(self.module.clone());
        let status = unsafe { (self.module.api.encode_mp3)(View::new(pcm), &mut out.raw) };
        self.frame(status, out, reservation, limit)?
            .context("MP3 encoder returned EOF")
    }
}
pub struct AudioChunk {
    pub start_us: i64,
    pub samples: Vec<f32>,
    _reservation: Reservation,
}
impl AudioChunk {
    pub fn wav(&self) -> Vec<u8> {
        let bytes = (self.samples.len() * 2) as u32;
        let mut out = Vec::with_capacity(bytes as usize + 44);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + bytes).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&16000u32.to_le_bytes());
        out.extend_from_slice(&32000u32.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&bytes.to_le_bytes());
        for s in &self.samples {
            out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
        }
        out
    }
}
