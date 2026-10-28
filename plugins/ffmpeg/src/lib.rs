//! FFmpeg compatibility capability pack. Only this crate links libav*.
mod mp3;
mod session;
use anyhow::{Result, ensure};
use ffmpeg_next as ffmpeg;
use remdia_plugin_abi::*;
use remdia_types::*;
use serde_json::json;
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
};

struct Owned {
    json: Vec<u8>,
    bytes: Vec<u8>,
    video: Option<ffmpeg::frame::Video>,
}
impl Owned {
    fn output(self, out: &mut Output) {
        let owner = Box::new(self);
        let mut planes = [View::EMPTY; 4];
        if let Some(v) = &owner.video {
            for (i, plane) in planes.iter_mut().enumerate().take(v.planes().min(4)) {
                *plane = View::new(v.data(i));
            }
        } else {
            planes[0] = View::new(&owner.bytes);
        }
        *out = Output {
            header: Header::of::<Output>(),
            json: View::new(&owner.json),
            planes,
            owner: Box::into_raw(owner).cast(),
            release: Some(release),
        };
    }
}
unsafe extern "C" fn release(ptr: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        drop(Box::from_raw(ptr.cast::<Owned>()))
    }));
}
fn reply(value: &impl serde::Serialize) -> Result<Owned> {
    Ok(Owned {
        json: serde_json::to_vec(value)?,
        bytes: Vec::new(),
        video: None,
    })
}
fn boundary(out: *mut Output, f: impl FnOnce() -> Result<Option<Owned>>) -> i32 {
    if out.is_null() {
        return ERROR;
    }
    unsafe {
        *out = Output::default();
    }
    let result = catch_unwind(AssertUnwindSafe(f));
    match result {
        Ok(Ok(Some(value))) => {
            value.output(unsafe { &mut *out });
            OK
        }
        Ok(Ok(None)) => EOF,
        other => {
            let (status, message) = match other {
                Ok(Err(e)) => (ERROR, format!("{e:#}")),
                _ => (PANIC, "plugin panic contained".into()),
            };
            if let Ok(value) = reply(&json!({"error":message})) {
                value.output(unsafe { &mut *out });
            }
            status
        }
    }
}
unsafe extern "C" fn open(
    request: View,
    control: Control,
    handle: *mut *mut c_void,
    out: *mut Output,
) -> i32 {
    boundary(out, || {
        ensure!(!handle.is_null(), "null session destination");
        unsafe {
            *handle = std::ptr::null_mut();
        }
        let request: Request = serde_json::from_slice(unsafe { request.as_slice() })?;
        request.validate()?;
        let session = session::Session::open(request, control)?;
        let info = reply(&session.info)?;
        unsafe {
            *handle = Box::into_raw(Box::new(session)).cast();
        }
        Ok(Some(info))
    })
}
unsafe extern "C" fn next(handle: *mut c_void, out: *mut Output) -> i32 {
    boundary(out, || unsafe {
        (&mut *handle.cast::<session::Session>()).next()
    })
}
unsafe extern "C" fn seek(handle: *mut c_void, target: i64, out: *mut Output) -> i32 {
    boundary(out, || {
        unsafe {
            (&mut *handle.cast::<session::Session>()).seek(target)?;
        }
        Ok(Some(reply(&json!({"seek_us":target}))?))
    })
}
unsafe extern "C" fn artwork(handle: *mut c_void, out: *mut Output) -> i32 {
    boundary(out, || unsafe {
        (&mut *handle.cast::<session::Session>()).artwork()
    })
}
unsafe extern "C" fn encode_mp3(pcm: View, out: *mut Output) -> i32 {
    boundary(out, || {
        let bytes = mp3::encode(unsafe { pcm.as_slice() })?;
        let mut value = reply(&FrameInfo::Blob {
            format: "mp3".into(),
        })?;
        value.bytes = bytes;
        Ok(Some(value))
    })
}
unsafe extern "C" fn close(handle: *mut c_void) {
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        drop(Box::from_raw(handle.cast::<session::Session>()))
    }));
}
/// # Safety
/// `out` must point to writable storage for PluginApi, with the host header size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn remdia_plugin_query_v1(host: Header, out: *mut PluginApi) -> i32 {
    if out.is_null()
        || host.major != ABI_MAJOR
        || host.struct_size < std::mem::size_of::<PluginApi>() as u32
    {
        return ERROR;
    }
    unsafe {
        *out = PluginApi {
            header: Header::of::<PluginApi>(),
            open,
            next,
            seek,
            artwork,
            encode_mp3,
            close,
        };
    }
    OK
}
