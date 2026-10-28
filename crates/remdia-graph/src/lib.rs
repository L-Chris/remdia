//! Media component contracts, shared buffers and a bounded DAG scheduler.
pub mod ffi;
pub mod runtime;
use anyhow::{Result, ensure};
use remdia_buffer::Reservation;
use remdia_plugin_abi as abi;
pub use remdia_types::*;
pub use runtime::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

#[derive(Clone, Default)]
pub struct Cancellation(Arc<CancelState>);
#[derive(Default)]
struct CancelState {
    cancel: AtomicBool,
    serial: AtomicU64,
    observed: AtomicU64,
    external: Option<Arc<AtomicBool>>,
    requested: Option<Arc<AtomicU64>>,
    active_generation: AtomicU64,
}
impl Cancellation {
    pub fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self(Arc::new(CancelState {
            external: Some(flag),
            ..Default::default()
        }))
    }
    pub fn with_requested_generation(requested: Arc<AtomicU64>) -> Self {
        Self(Arc::new(CancelState {
            requested: Some(requested),
            ..Default::default()
        }))
    }
    pub fn acknowledge_generation(&self, generation: u64) {
        self.0
            .active_generation
            .store(generation, Ordering::Release);
    }
    pub fn cancel(&self) {
        self.0.cancel.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.cancel.load(Ordering::Acquire)
            || self
                .0
                .external
                .as_ref()
                .is_some_and(|f| f.load(Ordering::Acquire))
    }
    pub fn interrupt(&self) {
        self.0.serial.fetch_add(1, Ordering::AcqRel);
    }
    pub fn begin(&self) {
        self.0
            .observed
            .store(self.0.serial.load(Ordering::Acquire), Ordering::Release);
    }
    pub fn poll(&self) -> u8 {
        if self.is_cancelled() {
            1
        } else if self.0.requested.as_ref().is_some_and(|requested| {
            let active = self.0.active_generation.load(Ordering::Acquire);
            active > 0 && requested.load(Ordering::Acquire) > active
        }) || self.0.serial.load(Ordering::Acquire)
            != self.0.observed.load(Ordering::Acquire)
        {
            2
        } else {
            0
        }
    }
}
/// Plugin-side view of host cancellation. The host keeps its context alive.
#[derive(Clone, Copy)]
pub struct Control(pub abi::Control);
// SAFETY: The ABI requires the callback to be thread-safe and its context stable.
unsafe impl Send for Control {}
unsafe impl Sync for Control {}
#[derive(Debug)]
pub struct Interrupted;
impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("media operation interrupted")
    }
}
impl std::error::Error for Interrupted {}
impl Control {
    pub fn check(&self) -> Result<()> {
        match unsafe { (self.0.cancelled)(self.0.context) } {
            0 => Ok(()),
            2 => Err(Interrupted.into()),
            _ => anyhow::bail!("cancelled"),
        }
    }
}
/// # Safety
/// The context must reference a live Cancellation through component close.
pub unsafe extern "C" fn poll_cancellation(context: *mut c_void) -> u8 {
    unsafe { (&*context.cast::<Cancellation>()).poll() }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataKind {
    Image,
    Packet,
    Audio,
    Video,
    Bytes,
    Any,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortCaps {
    pub kind: DataKind,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub rate: Option<u32>,
    #[serde(default)]
    pub channels: Option<u16>,
}
impl PortCaps {
    pub fn new(kind: DataKind) -> Self {
        Self {
            kind,
            format: None,
            rate: None,
            channels: None,
        }
    }
    pub fn accepts(&self, from: &Self) -> bool {
        (self.kind == DataKind::Any || from.kind == DataKind::Any || self.kind == from.kind)
            && self
                .format
                .as_ref()
                .is_none_or(|v| from.format.as_ref().is_none_or(|x| x == v))
            && self.rate.is_none_or(|v| from.rate.is_none_or(|x| x == v))
            && self
                .channels
                .is_none_or(|v| from.channels.is_none_or(|x| x == v))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComponentDescriptor {
    pub id: String,
    pub role: String,
    pub input: Option<PortCaps>,
    pub output: PortCaps,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeInfo {
    pub input: Option<PortCaps>,
    pub output: PortCaps,
    pub max_output_bytes: usize,
    #[serde(default)]
    pub media: Option<MediaInfo>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeConfig {
    pub component: String,
    #[serde(default)]
    pub settings: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BufferHeader {
    pub stream_id: u32,
    pub generation: u64,
    pub info: FrameInfo,
}
impl BufferHeader {
    pub fn pts_us(&self) -> Option<i64> {
        match self.info {
            FrameInfo::Packet { pts, time_base, .. } => {
                time_base.rescale(pts, Rational::MICROS).ok()
            }
            _ => self.info.pts_us(),
        }
    }
    pub fn duration_us(&self) -> u64 {
        match self.info {
            FrameInfo::Packet {
                duration,
                time_base,
                ..
            } => i64::try_from(duration)
                .ok()
                .and_then(|v| time_base.rescale(v, Rational::MICROS).ok())
                .unwrap_or(0)
                .max(0) as u64,
            FrameInfo::Audio { samples, rate, .. } => {
                samples as u64 * 1_000_000 / rate.max(1) as u64
            }
            FrameInfo::Video { duration_us, .. } => duration_us.max(0) as u64,
            _ => 0,
        }
    }
}
/// Implementors guarantee immutable plane storage and thread-safe destruction.
pub trait PlaneOwner: Send + Sync {
    fn plane(&self, index: usize) -> &[u8];
}
struct VecPlanes(Vec<Vec<u8>>);
impl PlaneOwner for VecPlanes {
    fn plane(&self, i: usize) -> &[u8] {
        self.0.get(i).map_or(&[], Vec::as_slice)
    }
}
struct BufferInner {
    owner: Box<dyn PlaneOwner>,
    _reservation: Option<Reservation>,
}
#[derive(Clone)]
pub struct MediaBuffer {
    pub header: BufferHeader,
    inner: Arc<BufferInner>,
}
impl MediaBuffer {
    pub fn new(header: BufferHeader, planes: Vec<Vec<u8>>) -> Self {
        Self {
            header,
            inner: Arc::new(BufferInner {
                owner: Box::new(VecPlanes(planes)),
                _reservation: None,
            }),
        }
    }
    pub fn from_owner(
        header: BufferHeader,
        owner: Box<dyn PlaneOwner>,
        reservation: Option<Reservation>,
    ) -> Self {
        Self {
            header,
            inner: Arc::new(BufferInner {
                owner,
                _reservation: reservation,
            }),
        }
    }
    pub fn plane(&self, i: usize) -> &[u8] {
        self.inner.owner.plane(i)
    }
    pub fn bytes(&self) -> &[u8] {
        self.plane(0)
    }
    pub fn len(&self) -> usize {
        (0..4).map(|i| self.plane(i).len()).sum()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn with_budget(self, reservation: Reservation) -> Self {
        if self
            .inner
            ._reservation
            .as_ref()
            .is_some_and(|existing| existing.shares_budget_with(&reservation))
        {
            return self;
        }
        let header = self.header.clone();
        Self::from_owner(header, Box::new(self), Some(reservation))
    }
    pub fn validate(&self) -> Result<()> {
        match &self.header.info {
            FrameInfo::Image { image, .. } => image.validate(self.bytes())?,
            FrameInfo::Audio {
                samples,
                channels,
                rate,
                format,
                ..
            } => ensure!(
                *rate > 0
                    && *channels > 0
                    && *channels <= 32
                    && format == "f32le"
                    && samples.checked_mul(*channels as usize * 4) == Some(self.bytes().len()),
                "invalid PCM frame"
            ),
            FrameInfo::Video {
                planes,
                width,
                height,
                ..
            } => {
                ensure!(
                    *width > 0 && *height > 0 && planes.len() <= 4,
                    "invalid video dimensions"
                );
                for (i, p) in planes.iter().enumerate() {
                    ensure!(
                        p.stride
                            .checked_mul(p.rows)
                            .is_some_and(|n| n <= self.plane(i).len()),
                        "invalid video plane layout"
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }
}
impl PlaneOwner for MediaBuffer {
    fn plane(&self, i: usize) -> &[u8] {
        MediaBuffer::plane(self, i)
    }
}
// Keep the small state variants inline; avoid an allocation for each output.
#[allow(clippy::large_enum_variant)]
pub enum Receive {
    Output(MediaBuffer),
    NeedInput,
    /// info() changed; no media buffer was returned. Reserve the new bound next.
    Reconfigured,
    End,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submit {
    Accepted,
    WouldBlock,
}
/// One component instance, called serially on its owning worker.
pub trait Component {
    fn info(&self) -> &NodeInfo;
    fn submit(&mut self, _input: &MediaBuffer) -> Result<Submit> {
        anyhow::bail!("component has no input")
    }
    fn receive(&mut self) -> Result<Receive>;
    fn drain(&mut self) -> Result<()>;
    fn reset(&mut self, generation: u64, target_us: i64) -> Result<()>;
    fn command(&mut self, _command: &Value) -> Result<Option<MediaBuffer>> {
        Ok(None)
    }
}
pub trait Factory {
    fn create(&self, config: &NodeConfig, cancel: Cancellation) -> Result<Box<dyn Component>>;
}
