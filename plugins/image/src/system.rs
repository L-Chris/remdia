#![cfg_attr(not(any(windows, target_os = "macos")), allow(unused_mut))]
use anyhow::{Result, bail, ensure};
use remdia_graph::PlaneOwner;
use std::{ffi::CString, ptr};

#[repr(C)]
pub struct Probe {
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    pub orientation: u32,
    pub format: [u8; 64],
}
#[repr(C)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub size: usize,
    pub data: *mut u8,
}
impl Default for Pixels {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            stride: 0,
            size: 0,
            data: ptr::null_mut(),
        }
    }
}
#[cfg(any(windows, target_os = "macos"))]
unsafe extern "C" {
    fn rm_image_probe(data: *const u8, size: usize, out: *mut Probe) -> i32;
    fn rm_image_decode(
        data: *const u8,
        size: usize,
        maxw: u32,
        maxh: u32,
        maxpixels: usize,
        limit: usize,
        out: *mut Pixels,
    ) -> i32;
    fn rm_image_scale(
        data: *const u8,
        w: u32,
        h: u32,
        stride: usize,
        dw: u32,
        dh: u32,
        limit: usize,
        out: *mut Pixels,
    ) -> i32;
    fn rm_image_encode(
        data: *const u8,
        w: u32,
        h: u32,
        stride: usize,
        jpeg: i32,
        quality: f64,
        limit: usize,
        out: *mut Pixels,
    ) -> i32;
    fn rm_image_supports(format: *const std::ffi::c_char, encode: i32) -> i32;
    fn rm_image_free(out: *mut Pixels);
}
fn status(code: i32) -> Result<()> {
    match code {
        0 => Ok(()),
        1 => bail!("system image capability unavailable"),
        2 => bail!("invalid image data"),
        3 => bail!("image resource limit exceeded"),
        _ => bail!("system image operation failed"),
    }
}
pub fn probe(bytes: &[u8]) -> Result<Probe> {
    let mut out = Probe {
        width: 0,
        height: 0,
        frames: 0,
        orientation: 0,
        format: [0; 64],
    };
    #[cfg(any(windows, target_os = "macos"))]
    status(unsafe { rm_image_probe(bytes.as_ptr(), bytes.len(), &mut out) })?;
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = bytes;
        status(1)?;
    }
    ensure!(
        out.width > 0 && out.height > 0 && (1..=8).contains(&out.orientation),
        "invalid image metadata"
    );
    Ok(out)
}
impl Pixels {
    pub fn decode(
        bytes: &[u8],
        maxw: u32,
        maxh: u32,
        maxpixels: usize,
        limit: usize,
    ) -> Result<Self> {
        let mut out = Self::default();
        #[cfg(any(windows, target_os = "macos"))]
        status(unsafe {
            rm_image_decode(
                bytes.as_ptr(),
                bytes.len(),
                maxw,
                maxh,
                maxpixels,
                limit,
                &mut out,
            )
        })?;
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = (bytes, maxw, maxh, maxpixels, limit);
            status(1)?;
        }
        out.check(limit)?;
        Ok(out)
    }
    pub fn scale(
        bytes: &[u8],
        info: &remdia_graph::ImageInfo,
        w: u32,
        h: u32,
        limit: usize,
    ) -> Result<Self> {
        info.validate(bytes)?;
        let mut out = Self::default();
        #[cfg(any(windows, target_os = "macos"))]
        status(unsafe {
            rm_image_scale(
                bytes.as_ptr(),
                info.width,
                info.height,
                info.stride,
                w,
                h,
                limit,
                &mut out,
            )
        })?;
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = (w, h, limit);
            status(1)?;
        }
        out.check(limit)?;
        Ok(out)
    }
    pub fn encode(
        bytes: &[u8],
        info: &remdia_graph::ImageInfo,
        jpeg: bool,
        quality: f64,
        limit: usize,
    ) -> Result<Self> {
        info.validate(bytes)?;
        let mut out = Self::default();
        #[cfg(any(windows, target_os = "macos"))]
        status(unsafe {
            rm_image_encode(
                bytes.as_ptr(),
                info.width,
                info.height,
                info.stride,
                i32::from(jpeg),
                quality,
                limit,
                &mut out,
            )
        })?;
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = (jpeg, quality, limit);
            status(1)?;
        }
        out.check(limit)?;
        Ok(out)
    }
    fn check(&self, limit: usize) -> Result<()> {
        ensure!(
            !self.data.is_null() && self.size > 0 && self.size <= limit,
            "invalid system image output"
        );
        Ok(())
    }
}
// SAFETY: outputs own immutable malloc storage, no platform/COM handles survive.
unsafe impl Send for Pixels {}
unsafe impl Sync for Pixels {}
impl PlaneOwner for Pixels {
    fn plane(&self, index: usize) -> &[u8] {
        if index == 0 && !self.data.is_null() {
            unsafe { std::slice::from_raw_parts(self.data, self.size) }
        } else {
            &[]
        }
    }
}
impl Drop for Pixels {
    fn drop(&mut self) {
        #[cfg(any(windows, target_os = "macos"))]
        unsafe {
            rm_image_free(self)
        }
    }
}
pub fn supports(format: &str, encode: bool) -> bool {
    let Ok(format) = CString::new(format) else {
        return false;
    };
    #[cfg(any(windows, target_os = "macos"))]
    return unsafe { rm_image_supports(format.as_ptr(), i32::from(encode)) != 0 };
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (format, encode);
        false
    }
}
