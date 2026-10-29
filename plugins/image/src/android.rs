use super::{Pixels, Probe};
use anyhow::{Result, ensure};
use jni::objects::{JByteArray, JValue};
use remdia_android_platform::{text, with_env};

fn array(
    env: &mut jni::JNIEnv<'_>,
    object: jni::objects::JObject<'_>,
    limit: usize,
) -> Result<Vec<u8>> {
    let array = JByteArray::from(object);
    ensure!(
        env.get_array_length(&array)? as usize <= limit,
        "Android image exceeded output bound"
    );
    Ok(env.convert_byte_array(array)?)
}
fn assign(out: &mut Pixels, mut bytes: Vec<u8>, encoded: bool, limit: usize) -> Result<()> {
    let (width, height, stride) = if encoded {
        (0, 0, 0)
    } else {
        ensure!(bytes.len() >= 8, "missing image dimensions");
        let width = u32::from_le_bytes(bytes[..4].try_into()?);
        let height = u32::from_le_bytes(bytes[4..8].try_into()?);
        bytes.drain(..8);
        ensure!(
            width > 0
                && height > 0
                && (width as usize)
                    .checked_mul(height as usize)
                    .and_then(|n| n.checked_mul(4))
                    == Some(bytes.len()),
            "invalid image pixels"
        );
        (width, height, width as usize * 4)
    };
    ensure!(
        !bytes.is_empty() && bytes.len() <= limit,
        "image output exceeds limit"
    );
    let data = bytes.into_boxed_slice();
    *out = Pixels {
        width,
        height,
        stride,
        size: data.len(),
        data: Box::into_raw(data).cast(),
    };
    Ok(())
}
pub unsafe fn rm_image_probe(data: *const u8, size: usize, out: *mut Probe) -> i32 {
    let result = with_env(|env, class| {
        let input = env.byte_array_from_slice(unsafe { std::slice::from_raw_parts(data, size) })?;
        let value = env
            .call_static_method(
                class,
                "imageProbe",
                "([B)Ljava/lang/String;",
                &[JValue::Object(&input)],
            )?
            .l()?;
        let value: serde_json::Value = serde_json::from_str(&text(env, value)?)?;
        let out = unsafe { &mut *out };
        out.width = u32::try_from(value["width"].as_u64().unwrap_or(0))?;
        out.height = u32::try_from(value["height"].as_u64().unwrap_or(0))?;
        out.frames = u32::try_from(value["frames"].as_u64().unwrap_or(0))?;
        out.orientation = u32::try_from(value["orientation"].as_u64().unwrap_or(0))?;
        let format = value["format"].as_str().unwrap_or("").as_bytes();
        ensure!(format.len() < out.format.len(), "invalid image format");
        out.format[..format.len()].copy_from_slice(format);
        Ok(())
    });
    if result.is_ok() { 0 } else { 2 }
}
pub unsafe fn rm_image_decode(
    data: *const u8,
    size: usize,
    w: u32,
    h: u32,
    pixels: usize,
    limit: usize,
    out: *mut Pixels,
) -> i32 {
    let result = with_env(|env, class| {
        let input = env.byte_array_from_slice(unsafe { std::slice::from_raw_parts(data, size) })?;
        let value = env
            .call_static_method(
                class,
                "imageDecode",
                "([BIIJJ)[B",
                &[
                    JValue::Object(&input),
                    JValue::Int(w as i32),
                    JValue::Int(h as i32),
                    JValue::Long(pixels as i64),
                    JValue::Long(limit as i64),
                ],
            )?
            .l()?;
        assign(
            unsafe { &mut *out },
            array(
                env,
                value,
                limit
                    .checked_add(8)
                    .ok_or_else(|| anyhow::anyhow!("image size overflow"))?,
            )?,
            false,
            limit,
        )
    });
    if result.is_ok() { 0 } else { 4 }
}
#[allow(clippy::too_many_arguments)]
pub unsafe fn rm_image_scale(
    data: *const u8,
    w: u32,
    h: u32,
    stride: usize,
    dw: u32,
    dh: u32,
    limit: usize,
    out: *mut Pixels,
) -> i32 {
    let result = with_env(|env, class| {
        let length = stride
            .checked_mul(h as usize)
            .ok_or_else(|| anyhow::anyhow!("image size overflow"))?;
        let input = unsafe { env.new_direct_byte_buffer(data.cast_mut(), length) }?;
        let value = env
            .call_static_method(
                class,
                "imageScale",
                "(Ljava/nio/ByteBuffer;IIIIJ)[B",
                &[
                    JValue::Object(&input),
                    JValue::Int(w as i32),
                    JValue::Int(h as i32),
                    JValue::Int(dw as i32),
                    JValue::Int(dh as i32),
                    JValue::Long(limit as i64),
                ],
            )?
            .l()?;
        assign(
            unsafe { &mut *out },
            array(env, value, limit + 8)?,
            false,
            limit,
        )
    });
    if result.is_ok() { 0 } else { 4 }
}
#[allow(clippy::too_many_arguments)]
pub unsafe fn rm_image_encode(
    data: *const u8,
    w: u32,
    h: u32,
    stride: usize,
    jpeg: i32,
    quality: f64,
    limit: usize,
    out: *mut Pixels,
) -> i32 {
    let result = with_env(|env, class| {
        let length = stride
            .checked_mul(h as usize)
            .ok_or_else(|| anyhow::anyhow!("image size overflow"))?;
        let input = unsafe { env.new_direct_byte_buffer(data.cast_mut(), length) }?;
        let value = env
            .call_static_method(
                class,
                "imageEncode",
                "(Ljava/nio/ByteBuffer;IIZDJ)[B",
                &[
                    JValue::Object(&input),
                    JValue::Int(w as i32),
                    JValue::Int(h as i32),
                    JValue::Bool(u8::from(jpeg != 0)),
                    JValue::Double(quality),
                    JValue::Long(limit as i64),
                ],
            )?
            .l()?;
        assign(unsafe { &mut *out }, array(env, value, limit)?, true, limit)
    });
    if result.is_ok() { 0 } else { 4 }
}
pub unsafe fn rm_image_supports(format: *const std::ffi::c_char, encode: i32) -> i32 {
    let name = unsafe { std::ffi::CStr::from_ptr(format) }.to_string_lossy();
    let name = match name.as_ref() {
        "public.jpeg" => "jpeg",
        "public.png" => "png",
        "org.webmproject.webp" => "webp",
        "com.compuserve.gif" => "gif",
        "public.tiff" => "tiff",
        "com.microsoft.bmp" => "bmp",
        "public.heif" => "heif",
        "public.avif" => "avif",
        other => other,
    };
    if encode != 0 {
        return i32::from(matches!(name, "jpeg" | "png"));
    }
    let mime = format!("image/{}", if name == "jpg" { "jpeg" } else { name });
    with_env(|env, class| {
        let mime = env.new_string(mime)?;
        Ok(i32::from(
            env.call_static_method(
                class,
                "imageSupports",
                "(Ljava/lang/String;)Z",
                &[JValue::Object(&mime)],
            )?
            .z()?,
        ))
    })
    .unwrap_or(0)
}
pub unsafe fn rm_image_free(out: *mut Pixels) {
    let out = unsafe { &mut *out };
    if !out.data.is_null() {
        let data = std::mem::replace(&mut out.data, std::ptr::null_mut());
        let size = std::mem::take(&mut out.size);
        unsafe {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                data, size,
            )));
        }
        out.width = 0;
        out.height = 0;
        out.stride = 0;
    }
}
