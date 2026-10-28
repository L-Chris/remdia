use anyhow::{Context, Result, ensure};
use libloading::Library;
use remdia_graph::*;
use remdia_plugin_abi as abi;
use serde::{Deserialize, Serialize};
use std::{
    ffi::c_void,
    io::Read,
    marker::PhantomData,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex, OnceLock},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub abi_major: u16,
    pub os: String,
    pub arch: String,
    pub library: String,
    pub capabilities: Vec<String>,
}
#[derive(Clone, Debug)]
pub struct Candidate {
    pub manifest: Manifest,
    pub path: PathBuf,
}
pub struct Registry {
    pub candidates: Vec<Candidate>,
}
impl Registry {
    pub fn discover(directory: impl AsRef<Path>) -> Result<Self> {
        let mut files = std::fs::read_dir(directory)?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        files.sort();
        let mut candidates = Vec::new();
        for path in files
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
        {
            let mut bytes = Vec::new();
            std::fs::File::open(&path)?
                .take(65537)
                .read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 65536, "oversized plugin manifest");
            let manifest: Manifest = serde_json::from_slice(&bytes)
                .with_context(|| format!("manifest {}", path.display()))?;
            if manifest.abi_major != abi::ABI_MAJOR
                || manifest.os != std::env::consts::OS
                || manifest.arch != std::env::consts::ARCH
            {
                continue;
            }
            let relative = Path::new(&manifest.library);
            ensure!(
                relative.components().count() == 1
                    && matches!(
                        relative.components().next(),
                        Some(std::path::Component::Normal(_))
                    ),
                "plugin library must be a filename"
            );
            ensure!(
                !candidates
                    .iter()
                    .any(|c: &Candidate| c.manifest.id == manifest.id),
                "duplicate plugin id"
            );
            candidates.push(Candidate {
                path: path.parent().unwrap().join(relative),
                manifest,
            });
        }
        Ok(Self { candidates })
    }
    pub fn select(&self, component: &str) -> Result<&Candidate> {
        self.candidates
            .iter()
            .find(|c| c.manifest.capabilities.iter().any(|s| s == component))
            .with_context(|| format!("no compatible component installed: {component}"))
    }
}
struct Module {
    api: abi::PluginApi,
    descriptors: Vec<ComponentDescriptor>,
}
type ModuleCache = Mutex<Vec<(PathBuf, Arc<Module>)>>;
static MODULES: OnceLock<ModuleCache> = OnceLock::new();
struct RawOutput {
    raw: abi::Output,
    _module: Arc<Module>,
}
// SAFETY: ABI v2 output planes are immutable and may be released on any thread;
// native libraries are pinned for process lifetime. Loading them requires trust.
unsafe impl Send for RawOutput {}
unsafe impl Sync for RawOutput {}
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
            raw: abi::Output::default(),
            _module: module,
        }
    }
    fn json(&self) -> Result<&[u8]> {
        ensure!(
            self.raw.header.major == abi::ABI_MAJOR
                && self.raw.header.struct_size >= std::mem::size_of::<abi::Output>() as u32,
            "invalid output ABI"
        );
        ensure!(
            self.raw.json.len <= 1024 * 1024
                && (self.raw.json.len == 0 || !self.raw.json.ptr.is_null()),
            "invalid output metadata"
        );
        Ok(unsafe { self.raw.json.as_slice() })
    }
    fn status(&self, status: i32) -> Result<i32> {
        if status >= 0 {
            return Ok(status);
        }
        if status == abi::INTERRUPTED {
            return Err(Interrupted.into());
        }
        let value: serde_json::Value = serde_json::from_slice(self.json()?).unwrap_or_default();
        anyhow::bail!(
            "{}",
            value
                .get("error")
                .and_then(|s| s.as_str())
                .unwrap_or("native component failed")
        )
    }
    fn buffer(self) -> Result<MediaBuffer> {
        let header: BufferHeader = serde_json::from_slice(self.json()?)?;
        for v in &self.raw.planes {
            ensure!(v.len == 0 || !v.ptr.is_null(), "null output plane");
            ensure!(v.len <= 1024 * 1024 * 1024, "oversized output plane");
        }
        let b = MediaBuffer::from_owner(header, Box::new(self), None);
        b.validate()?;
        Ok(b)
    }
}
impl PlaneOwner for RawOutput {
    fn plane(&self, i: usize) -> &[u8] {
        self.raw
            .planes
            .get(i)
            .map_or(&[], |v| unsafe { v.as_slice() })
    }
}
impl Candidate {
    unsafe fn load(&self) -> Result<Arc<Module>> {
        let path = self.path.canonicalize()?;
        let mut cache = MODULES
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .map_err(|_| anyhow::anyhow!("plugin registry poisoned"))?;
        if let Some((_, m)) = cache.iter().find(|(p, _)| p == &path) {
            return Ok(m.clone());
        }
        let library =
            unsafe { Library::new(&path) }.with_context(|| format!("load {}", path.display()))?;
        let query: libloading::Symbol<abi::Query> =
            unsafe { library.get(b"remdia_plugin_query_v2\0") }?;
        let mut table = std::mem::MaybeUninit::<abi::PluginApi>::uninit();
        ensure!(
            unsafe { query(abi::Header::of::<abi::PluginApi>(), table.as_mut_ptr()) } == abi::OK,
            "plugin rejected component ABI"
        );
        let api = unsafe { table.assume_init() };
        ensure!(
            api.header.major == abi::ABI_MAJOR
                && api.header.struct_size >= std::mem::size_of::<abi::PluginApi>() as u32,
            "incompatible function table"
        );
        std::mem::forget(library);
        let mut module = Arc::new(Module {
            api,
            descriptors: Vec::new(),
        });
        let mut out = RawOutput::new(module.clone());
        let status = unsafe { (api.describe)(&mut out.raw) };
        ensure!(
            out.status(status)? == abi::OK,
            "component descriptions missing"
        );
        let descriptors = serde_json::from_slice(out.json()?)?;
        drop(out);
        Arc::get_mut(&mut module).unwrap().descriptors = descriptors;
        cache.push((path, module.clone()));
        Ok(module)
    }
}
/// Only constructed by explicitly unsafe Engine entry points after selecting a
/// trusted capability-pack directory. Every node can select a different DLL.
pub(crate) struct TrustedFactory<'a>(pub &'a Registry);
impl Factory for TrustedFactory<'_> {
    fn create(&self, config: &NodeConfig, cancel: Cancellation) -> Result<Box<dyn Component>> {
        let candidate = self.0.select(&config.component)?;
        let module = unsafe { candidate.load() }?;
        ensure!(
            module.descriptors.iter().any(|d| d.id == config.component),
            "manifest advertises a component absent from the DLL"
        );
        let control = Box::new(cancel);
        let mut handle = std::ptr::null_mut();
        let mut output = RawOutput::new(module.clone());
        let json = serde_json::to_vec(config)?;
        let status = unsafe {
            (module.api.create)(
                abi::View::new(&json),
                abi::Control {
                    context: (&*control as *const Cancellation).cast_mut().cast(),
                    cancelled: poll_cancellation,
                },
                &mut handle,
                &mut output.raw,
            )
        };
        let mut component = DynamicComponent {
            handle,
            module,
            control,
            info: None,
            _local: PhantomData,
        };
        ensure!(
            output.status(status)? == abi::OK && !handle.is_null(),
            "component creation failed"
        );
        component.info = Some(serde_json::from_slice(output.json()?)?);
        Ok(Box::new(component))
    }
}
struct DynamicComponent {
    handle: *mut c_void,
    module: Arc<Module>,
    control: Box<Cancellation>,
    info: Option<NodeInfo>,
    _local: PhantomData<Rc<()>>,
}
impl Drop for DynamicComponent {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                (self.module.api.close)(self.handle);
            }
        }
    }
}
unsafe extern "C" fn retain_input(p: *const c_void) {
    unsafe {
        Arc::increment_strong_count(p.cast::<MediaBuffer>());
    }
}
unsafe extern "C" fn release_input(p: *const c_void) {
    unsafe {
        Arc::decrement_strong_count(p.cast::<MediaBuffer>());
    }
}
impl Component for DynamicComponent {
    fn info(&self) -> &NodeInfo {
        self.info.as_ref().unwrap()
    }
    fn submit(&mut self, b: &MediaBuffer) -> Result<Submit> {
        let input = Arc::new(b.clone());
        let json = serde_json::to_vec(&b.header)?;
        let raw = abi::Input {
            header: abi::Header::of::<abi::Input>(),
            json: abi::View::new(&json),
            planes: std::array::from_fn(|i| abi::View::new(b.plane(i))),
            owner: Arc::as_ptr(&input).cast(),
            retain: retain_input,
            release: release_input,
        };
        let mut out = RawOutput::new(self.module.clone());
        let status = unsafe { (self.module.api.submit)(self.handle, &raw, &mut out.raw) };
        match out.status(status)? {
            abi::OK => Ok(Submit::Accepted),
            abi::WOULD_BLOCK => Ok(Submit::WouldBlock),
            _ => anyhow::bail!("invalid submit status"),
        }
    }
    fn receive(&mut self) -> Result<Receive> {
        let mut out = RawOutput::new(self.module.clone());
        let status = unsafe { (self.module.api.receive)(self.handle, &mut out.raw) };
        match out.status(status)? {
            abi::OK => Ok(Receive::Output(out.buffer()?)),
            abi::EOF => Ok(Receive::End),
            abi::NEED_INPUT => Ok(Receive::NeedInput),
            _ => anyhow::bail!("invalid receive status"),
        }
    }
    fn drain(&mut self) -> Result<()> {
        let mut out = RawOutput::new(self.module.clone());
        let status = unsafe { (self.module.api.drain)(self.handle, &mut out.raw) };
        ensure!(out.status(status)? == abi::OK, "drain failed");
        Ok(())
    }
    fn reset(&mut self, g: u64, t: i64) -> Result<()> {
        let mut out = RawOutput::new(self.module.clone());
        let status = unsafe { (self.module.api.reset)(self.handle, g, t, &mut out.raw) };
        ensure!(out.status(status)? == abi::OK, "reset failed");
        Ok(())
    }
    fn command(&mut self, v: &serde_json::Value) -> Result<Option<MediaBuffer>> {
        let _ = &self.control;
        let json = serde_json::to_vec(v)?;
        let mut out = RawOutput::new(self.module.clone());
        let status =
            unsafe { (self.module.api.command)(self.handle, abi::View::new(&json), &mut out.raw) };
        match out.status(status)? {
            abi::OK => Ok(Some(out.buffer()?)),
            abi::EOF => Ok(None),
            _ => anyhow::bail!("invalid command status"),
        }
    }
}
