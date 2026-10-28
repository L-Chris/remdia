//! Component C ABI v2: factories plus independent submit/receive/drain/reset.
use std::ffi::c_void;
pub const ABI_MAJOR: u16 = 2;
pub const ABI_MINOR: u16 = 1;
pub const OK: i32 = 0;
pub const EOF: i32 = 1;
pub const NEED_INPUT: i32 = 2;
pub const WOULD_BLOCK: i32 = 3;
pub const RECONFIGURED: i32 = 4;
pub const ERROR: i32 = -1;
pub const PANIC: i32 = -2;
pub const INTERRUPTED: i32 = -3;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Header {
    pub struct_size: u32,
    pub major: u16,
    pub minor: u16,
}
impl Header {
    pub fn of<T>() -> Self {
        Self {
            struct_size: std::mem::size_of::<T>() as u32,
            major: ABI_MAJOR,
            minor: ABI_MINOR,
        }
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct View {
    pub ptr: *const u8,
    pub len: usize,
}
impl View {
    pub const EMPTY: Self = Self {
        ptr: std::ptr::null(),
        len: 0,
    };
    pub fn new(bytes: &[u8]) -> Self {
        Self {
            ptr: bytes.as_ptr(),
            len: bytes.len(),
        }
    }
    /// # Safety
    /// A nonempty view must reference readable storage for the chosen lifetime.
    pub unsafe fn as_slice<'a>(self) -> &'a [u8] {
        if self.len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
        }
    }
}
/// Poll: 0=run, 1=cancel, 2=seek/read interruption. Valid through component close.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Control {
    pub context: *mut c_void,
    pub cancelled: unsafe extern "C" fn(*mut c_void) -> u8,
}
#[repr(C)]
pub struct Output {
    pub header: Header,
    pub json: View,
    pub planes: [View; 4],
    pub owner: *mut c_void,
    pub release: Option<unsafe extern "C" fn(*mut c_void)>,
}
impl Default for Output {
    fn default() -> Self {
        Self {
            header: Header::of::<Self>(),
            json: View::EMPTY,
            planes: [View::EMPTY; 4],
            owner: std::ptr::null_mut(),
            release: None,
        }
    }
}
/// Borrowed input with explicit retain/release for asynchronous or delayed use.
#[repr(C)]
pub struct Input {
    pub header: Header,
    pub json: View,
    pub planes: [View; 4],
    pub owner: *const c_void,
    pub retain: unsafe extern "C" fn(*const c_void),
    pub release: unsafe extern "C" fn(*const c_void),
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PluginApi {
    pub header: Header,
    pub describe: unsafe extern "C" fn(*mut Output) -> i32,
    pub create: unsafe extern "C" fn(View, Control, *mut *mut c_void, *mut Output) -> i32,
    pub submit: unsafe extern "C" fn(*mut c_void, *const Input, *mut Output) -> i32,
    pub receive: unsafe extern "C" fn(*mut c_void, *mut Output) -> i32,
    pub drain: unsafe extern "C" fn(*mut c_void, *mut Output) -> i32,
    pub reset: unsafe extern "C" fn(*mut c_void, u64, i64, *mut Output) -> i32,
    pub command: unsafe extern "C" fn(*mut c_void, View, *mut Output) -> i32,
    pub close: unsafe extern "C" fn(*mut c_void),
}
pub type Query = unsafe extern "C" fn(Header, *mut PluginApi) -> i32;
