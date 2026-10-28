use anyhow::{Context, Result, ensure};
use libloading::Library;
use remdia_plugin_abi::*;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
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
    /// Discovery reads manifests only; loading is deferred until open.
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
            let bytes = std::fs::read(&path)?;
            ensure!(bytes.len() <= 65536, "oversized plugin manifest");
            let manifest: Manifest = serde_json::from_slice(&bytes)
                .with_context(|| format!("manifest {}", path.display()))?;
            if manifest.abi_major != ABI_MAJOR
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
            candidates.push(Candidate {
                path: path.parent().unwrap().join(relative),
                manifest,
            });
        }
        Ok(Self { candidates })
    }
    pub fn select(&self, capability: &str) -> Result<&Candidate> {
        self.candidates
            .iter()
            .find(|c| c.manifest.capabilities.iter().any(|s| s == capability))
            .context("no compatible capability pack installed")
    }
}
#[derive(Clone, Copy)]
pub(crate) struct Module {
    pub api: PluginApi,
}
// The ABI mandates serialized session calls. Function tables are immutable.
type ModuleCache = Mutex<Vec<(PathBuf, Arc<Module>)>>;
static MODULES: OnceLock<ModuleCache> = OnceLock::new();
impl Candidate {
    /// # Safety
    /// Native plugins are executable code and must be trusted. Their ABI functions
    /// and output pointers must comply with the lifetime and size contracts.
    pub(crate) unsafe fn load(&self) -> Result<Arc<Module>> {
        let path = self.path.canonicalize()?;
        let mut modules = MODULES
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .map_err(|_| anyhow::anyhow!("plugin registry poisoned"))?;
        if let Some((_, m)) = modules.iter().find(|(p, _)| p == &path) {
            return Ok(m.clone());
        }
        let library =
            unsafe { Library::new(&path) }.with_context(|| format!("load {}", path.display()))?;
        let query: libloading::Symbol<Query> = unsafe { library.get(b"remdia_plugin_query_v1\0") }?;
        let mut table = std::mem::MaybeUninit::<PluginApi>::uninit();
        let status = unsafe { query(Header::of::<PluginApi>(), table.as_mut_ptr()) };
        ensure!(status == OK, "plugin rejected ABI query");
        let api = unsafe { table.assume_init() };
        ensure!(
            api.header.major == ABI_MAJOR
                && api.header.struct_size >= std::mem::size_of::<PluginApi>() as u32,
            "incompatible plugin function table"
        );
        // V1 deliberately pins native code until process exit, including frame destructors.
        std::mem::forget(library);
        let module = Arc::new(Module { api });
        modules.push((path, module.clone()));
        Ok(module)
    }
}
