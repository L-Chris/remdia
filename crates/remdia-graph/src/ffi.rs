//! Shared implementation of the component ABI. No Rust object crosses a DLL boundary.
use super::*;
use abi::{Header, Input, Output, PluginApi, View};
use std::panic::{AssertUnwindSafe, catch_unwind};

struct Owned {
    json: Vec<u8>,
    buffer: Option<MediaBuffer>,
}
impl Owned {
    fn json(v: &impl Serialize) -> Result<Self> {
        Ok(Self {
            json: serde_json::to_vec(v)?,
            buffer: None,
        })
    }
    fn buffer(b: MediaBuffer) -> Result<Self> {
        Ok(Self {
            json: serde_json::to_vec(&b.header)?,
            buffer: Some(b),
        })
    }
    fn write(self, out: &mut Output) {
        let owner = Box::new(self);
        let planes = std::array::from_fn(|i| {
            owner
                .buffer
                .as_ref()
                .map_or(View::EMPTY, |b| View::new(b.plane(i)))
        });
        *out = Output {
            header: Header::of::<Output>(),
            json: View::new(&owner.json),
            planes,
            owner: Box::into_raw(owner).cast(),
            release: Some(release),
        };
    }
}
unsafe extern "C" fn release(p: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        drop(Box::from_raw(p.cast::<Owned>()))
    }));
}
fn boundary(out: *mut Output, f: impl FnOnce() -> Result<(i32, Option<Owned>)>) -> i32 {
    if out.is_null() {
        return abi::ERROR;
    }
    unsafe {
        *out = Output::default();
    }
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok((status, reply))) => {
            if let Some(reply) = reply {
                reply.write(unsafe { &mut *out });
            }
            status
        }
        result => {
            let (status, error) = match result {
                Ok(Err(e)) => {
                    let status = if e.downcast_ref::<Interrupted>().is_some() {
                        abi::INTERRUPTED
                    } else {
                        abi::ERROR
                    };
                    (status, format!("{e:#}"))
                }
                _ => (abi::PANIC, "component panic contained".into()),
            };
            if let Ok(reply) = Owned::json(&serde_json::json!({"error":error})) {
                reply.write(unsafe { &mut *out });
            }
            status
        }
    }
}
struct Handle {
    component: Box<dyn Component>,
    control: Control,
}
struct RetainedInput {
    planes: [View; 4],
    owner: *const c_void,
    release: unsafe extern "C" fn(*const c_void),
}
// SAFETY: The input ABI requires read-only planes and thread-safe lifetime callbacks.
unsafe impl Send for RetainedInput {}
unsafe impl Sync for RetainedInput {}
impl PlaneOwner for RetainedInput {
    fn plane(&self, i: usize) -> &[u8] {
        self.planes.get(i).map_or(&[], |v| unsafe { v.as_slice() })
    }
}
impl Drop for RetainedInput {
    fn drop(&mut self) {
        unsafe {
            (self.release)(self.owner);
        }
    }
}
unsafe fn input_buffer(raw: &Input) -> Result<MediaBuffer> {
    ensure!(raw.header.major == abi::ABI_MAJOR, "invalid input ABI");
    ensure!(raw.json.len <= 1024 * 1024, "oversized input metadata");
    let header = serde_json::from_slice(unsafe { raw.json.as_slice() })?;
    for view in &raw.planes {
        ensure!(view.len == 0 || !view.ptr.is_null(), "null input plane");
    }
    unsafe {
        (raw.retain)(raw.owner);
    }
    let buffer = MediaBuffer::from_owner(
        header,
        Box::new(RetainedInput {
            planes: raw.planes,
            owner: raw.owner,
            release: raw.release,
        }),
        None,
    );
    buffer.validate()?;
    Ok(buffer)
}
pub type CreateFn = fn(&NodeConfig, Control) -> Result<Box<dyn Component>>;
pub type DescribeFn = fn() -> Vec<ComponentDescriptor>;
/// A statically linked pack uses the same factories and component contracts.
#[derive(Clone, Copy, Debug)]
pub struct BuiltinPlugin {
    pub describe: DescribeFn,
    pub create: CreateFn,
}

/// Reply ownership and panic containment shared by exported SDK operations.
pub enum Reply {
    Empty,
    Json(Value),
    Buffer(MediaBuffer),
}
/// # Safety
/// `out` must be writable for an ABI Output. Output release may run on any thread.
pub unsafe fn respond(out: *mut Output, operation: impl FnOnce() -> Result<(i32, Reply)>) -> i32 {
    boundary(out, || {
        let (status, reply) = operation()?;
        let owned = match reply {
            Reply::Empty => None,
            Reply::Json(value) => Some(Owned::json(&value)?),
            Reply::Buffer(value) => Some(Owned::buffer(value)?),
        };
        Ok((status, owned))
    })
}
/// # Safety
/// `config` and `out` obey the component ABI; `factory` returns a serial component.
pub unsafe fn create(
    config: View,
    control: abi::Control,
    handle: *mut *mut c_void,
    out: *mut Output,
    factory: CreateFn,
) -> i32 {
    boundary(out, || {
        ensure!(!handle.is_null(), "null component handle output");
        unsafe {
            *handle = std::ptr::null_mut();
        }
        ensure!(
            config.len <= 1024 * 1024,
            "oversized component configuration"
        );
        let config: NodeConfig = serde_json::from_slice(unsafe { config.as_slice() })?;
        let control = Control(control);
        control.check()?;
        let component = factory(&config, control)?;
        let info = Owned::json(component.info())?;
        unsafe {
            *handle = Box::into_raw(Box::new(Handle { component, control })).cast();
        }
        Ok((abi::OK, Some(info)))
    })
}
/// # Safety
/// `out` must be a valid Output destination.
pub unsafe fn describe(out: *mut Output, describe: DescribeFn) -> i32 {
    boundary(out, || Ok((abi::OK, Some(Owned::json(&describe())?))))
}
unsafe extern "C" fn submit(handle: *mut c_void, input: *const Input, out: *mut Output) -> i32 {
    boundary(out, || {
        let h = unsafe { &mut *handle.cast::<Handle>() };
        h.control.check()?;
        ensure!(!input.is_null(), "null component input");
        let buffer = unsafe { input_buffer(&*input) }?;
        let status = match h.component.submit(&buffer)? {
            Submit::Accepted => abi::OK,
            Submit::WouldBlock => abi::WOULD_BLOCK,
        };
        Ok((status, None))
    })
}
unsafe extern "C" fn receive(handle: *mut c_void, out: *mut Output) -> i32 {
    boundary(out, || {
        let h = unsafe { &mut *handle.cast::<Handle>() };
        h.control.check()?;
        match h.component.receive()? {
            Receive::Output(b) => Ok((abi::OK, Some(Owned::buffer(b)?))),
            Receive::NeedInput => Ok((abi::NEED_INPUT, None)),
            Receive::Reconfigured => {
                Ok((abi::RECONFIGURED, Some(Owned::json(h.component.info())?)))
            }
            Receive::End => Ok((abi::EOF, None)),
        }
    })
}
unsafe extern "C" fn drain(handle: *mut c_void, out: *mut Output) -> i32 {
    boundary(out, || {
        let h = unsafe { &mut *handle.cast::<Handle>() };
        h.control.check()?;
        h.component.drain()?;
        Ok((abi::OK, None))
    })
}
unsafe extern "C" fn reset(
    handle: *mut c_void,
    generation: u64,
    target: i64,
    out: *mut Output,
) -> i32 {
    boundary(out, || {
        let h = unsafe { &mut *handle.cast::<Handle>() };
        h.control.check()?;
        h.component.reset(generation, target)?;
        Ok((abi::OK, None))
    })
}
unsafe extern "C" fn command(handle: *mut c_void, request: View, out: *mut Output) -> i32 {
    boundary(out, || {
        let h = unsafe { &mut *handle.cast::<Handle>() };
        h.control.check()?;
        let request: Value = serde_json::from_slice(unsafe { request.as_slice() })?;
        match h.component.command(&request)? {
            Some(b) => Ok((abi::OK, Some(Owned::buffer(b)?))),
            None => Ok((abi::EOF, None)),
        }
    })
}
unsafe extern "C" fn close(handle: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        drop(Box::from_raw(handle.cast::<Handle>()))
    }));
}
/// # Safety
/// `out` is writable for the host-advertised size; the query never writes on rejection.
pub unsafe fn query(
    host: Header,
    out: *mut PluginApi,
    describe: unsafe extern "C" fn(*mut Output) -> i32,
    create: unsafe extern "C" fn(View, abi::Control, *mut *mut c_void, *mut Output) -> i32,
) -> i32 {
    if out.is_null()
        || host.major != abi::ABI_MAJOR
        || host.minor < abi::ABI_MINOR
        || host.struct_size < std::mem::size_of::<PluginApi>() as u32
    {
        return abi::ERROR;
    }
    unsafe {
        *out = PluginApi {
            header: Header::of::<PluginApi>(),
            describe,
            create,
            submit,
            receive,
            drain,
            reset,
            command,
            close,
        };
    }
    abi::OK
}
#[macro_export]
macro_rules! export_plugin {
    ($describe:path,$factory:path) => {
        pub fn builtin_plugin() -> $crate::ffi::BuiltinPlugin {
            $crate::ffi::BuiltinPlugin { describe: $describe, create: $factory }
        }
        #[cfg(not(feature = "builtin"))]
        mod remdia_dynamic_exports {
            use super::*;
            $crate::export_plugin!(@dynamic $describe, $factory);
        }
    };
    (@dynamic $describe:path,$factory:path) => {
        unsafe extern "C" fn component_describe(out: *mut remdia_plugin_abi::Output) -> i32 {
            unsafe { $crate::ffi::describe(out, $describe) }
        }
        unsafe extern "C" fn component_create(
            request: remdia_plugin_abi::View,
            control: remdia_plugin_abi::Control,
            handle: *mut *mut std::ffi::c_void,
            out: *mut remdia_plugin_abi::Output,
        ) -> i32 {
            unsafe { $crate::ffi::create(request, control, handle, out, $factory) }
        }
        /// # Safety
        /// The output pointer and advertised table size must obey the remdia v2 ABI.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn remdia_plugin_query_v2(
            host: remdia_plugin_abi::Header,
            out: *mut remdia_plugin_abi::PluginApi,
        ) -> i32 {
            unsafe { $crate::ffi::query(host, out, component_describe, component_create) }
        }
    };
}
