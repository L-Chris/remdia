//! Stable platform C ABI. See docs/plugin-abi.md and include/remdia.h.
use std::ffi::c_void;
pub const ABI_MAJOR: u16 = 1;
pub const ABI_MINOR: u16 = 0;
pub const OK: i32 = 0;
pub const EOF: i32 = 1;
pub const ERROR: i32 = -1;
pub const PANIC: i32 = -2;
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
    /// Nonempty pointers must reference readable bytes for the returned lifetime.
    pub unsafe fn as_slice<'a>(self) -> &'a [u8] {
        if self.len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
        }
    }
}
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
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PluginApi {
    pub header: Header,
    pub open: unsafe extern "C" fn(View, Control, *mut *mut c_void, *mut Output) -> i32,
    pub next: unsafe extern "C" fn(*mut c_void, *mut Output) -> i32,
    pub seek: unsafe extern "C" fn(*mut c_void, i64, *mut Output) -> i32,
    pub artwork: unsafe extern "C" fn(*mut c_void, *mut Output) -> i32,
    pub encode_mp3: unsafe extern "C" fn(View, *mut Output) -> i32,
    pub close: unsafe extern "C" fn(*mut c_void),
}
pub type Query = unsafe extern "C" fn(Header, *mut PluginApi) -> i32;
