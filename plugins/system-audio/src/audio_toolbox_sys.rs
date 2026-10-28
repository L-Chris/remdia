//! Minimal AudioConverter FFI. No codec implementation or framework binaries.
use libloading::Library;
use std::{ffi::c_void, sync::OnceLock};
pub type AudioConverterRef = *mut c_void;
pub const K_AUDIO_FORMAT_MPEG4_AAC: u32 = u32::from_be_bytes(*b"aac ");
pub const K_AUDIO_CONVERTER_DECOMPRESSION_MAGIC_COOKIE: u32 = u32::from_be_bytes(*b"dmgc");
#[repr(C)]
pub struct AudioStreamBasicDescription {
    pub sample_rate: f64,
    pub format_id: u32,
    pub format_flags: u32,
    pub bytes_per_packet: u32,
    pub frames_per_packet: u32,
    pub bytes_per_frame: u32,
    pub channels_per_frame: u32,
    pub bits_per_channel: u32,
    pub reserved: u32,
}
impl AudioStreamBasicDescription {
    pub fn mpeg4_aac(rate: f64, channels: u32) -> Self {
        Self {
            sample_rate: rate,
            format_id: K_AUDIO_FORMAT_MPEG4_AAC,
            format_flags: 2,
            bytes_per_packet: 0,
            frames_per_packet: 1024,
            bytes_per_frame: 0,
            channels_per_frame: channels,
            bits_per_channel: 0,
            reserved: 0,
        }
    }
    pub fn pcm_float32(rate: f64, channels: u32) -> Self {
        Self {
            sample_rate: rate,
            format_id: u32::from_be_bytes(*b"lpcm"),
            format_flags: 1 | 8,
            bytes_per_packet: 4 * channels,
            frames_per_packet: 1,
            bytes_per_frame: 4 * channels,
            channels_per_frame: channels,
            bits_per_channel: 32,
            reserved: 0,
        }
    }
}
#[repr(C)]
pub struct AudioBuffer {
    pub number_channels: u32,
    pub data_byte_size: u32,
    pub data: *mut u8,
}
#[repr(C)]
pub struct AudioBufferList1 {
    pub number_buffers: u32,
    pub buffers: [AudioBuffer; 1],
}
#[repr(C)]
#[derive(Default)]
pub struct AudioStreamPacketDescription {
    pub start_offset: i64,
    pub variable_frames_in_packet: u32,
    pub data_byte_size: u32,
}
type Callback = unsafe extern "C" fn(
    AudioConverterRef,
    *mut u32,
    *mut AudioBufferList1,
    *mut *mut AudioStreamPacketDescription,
    *mut c_void,
) -> i32;
type New = unsafe extern "C" fn(
    *const AudioStreamBasicDescription,
    *const AudioStreamBasicDescription,
    *mut AudioConverterRef,
) -> i32;
type Unary = unsafe extern "C" fn(AudioConverterRef) -> i32;
type Set = unsafe extern "C" fn(AudioConverterRef, u32, u32, *const c_void) -> i32;
type Convert =
    unsafe extern "C" fn(AudioConverterRef, u32, *const c_void, *mut u32, *mut c_void) -> i32;
type Get = unsafe extern "C" fn(AudioConverterRef, u32, *mut u32, *mut c_void) -> i32;
type Fill = unsafe extern "C" fn(
    AudioConverterRef,
    Callback,
    *mut c_void,
    *mut u32,
    *mut AudioBufferList1,
    *mut AudioStreamPacketDescription,
) -> i32;
pub struct Framework {
    new: New,
    dispose: Unary,
    reset: Unary,
    set: Set,
    get: Get,
    convert: Convert,
    fill: Fill,
    _library: Library,
}
pub fn framework() -> Result<&'static Framework, &'static str> {
    static FRAMEWORK: OnceLock<Result<Framework, String>> = OnceLock::new();
    FRAMEWORK
        .get_or_init(|| unsafe {
            let library =
                Library::new("/System/Library/Frameworks/AudioToolbox.framework/AudioToolbox")
                    .map_err(|e| e.to_string())?;
            Ok(Framework {
                new: *library
                    .get::<New>(b"AudioConverterNew\0")
                    .map_err(|e| e.to_string())?,
                dispose: *library
                    .get::<Unary>(b"AudioConverterDispose\0")
                    .map_err(|e| e.to_string())?,
                reset: *library
                    .get::<Unary>(b"AudioConverterReset\0")
                    .map_err(|e| e.to_string())?,
                set: *library
                    .get::<Set>(b"AudioConverterSetProperty\0")
                    .map_err(|e| e.to_string())?,
                get: *library
                    .get::<Get>(b"AudioConverterGetProperty\0")
                    .map_err(|e| e.to_string())?,
                convert: *library
                    .get::<Convert>(b"AudioConverterConvertBuffer\0")
                    .map_err(|e| e.to_string())?,
                fill: *library
                    .get::<Fill>(b"AudioConverterFillComplexBuffer\0")
                    .map_err(|e| e.to_string())?,
                _library: library,
            })
        })
        .as_ref()
        .map_err(String::as_str)
}
pub unsafe fn audio_converter_new(
    fw: &Framework,
    source: *const AudioStreamBasicDescription,
    destination: *const AudioStreamBasicDescription,
    out: *mut AudioConverterRef,
) -> i32 {
    unsafe { (fw.new)(source, destination, out) }
}
pub unsafe fn audio_converter_dispose(fw: &Framework, converter: AudioConverterRef) -> i32 {
    unsafe { (fw.dispose)(converter) }
}
pub unsafe fn audio_converter_reset(fw: &Framework, converter: AudioConverterRef) -> i32 {
    unsafe { (fw.reset)(converter) }
}
pub unsafe fn audio_converter_set_property(
    fw: &Framework,
    converter: AudioConverterRef,
    property: u32,
    size: u32,
    data: *const c_void,
) -> i32 {
    unsafe { (fw.set)(converter, property, size, data) }
}
pub unsafe fn audio_converter_get_property(
    fw: &Framework,
    converter: AudioConverterRef,
    property: u32,
    size: *mut u32,
    data: *mut c_void,
) -> i32 {
    unsafe { (fw.get)(converter, property, size, data) }
}
pub unsafe fn audio_converter_fill_complex_buffer(
    fw: &Framework,
    converter: AudioConverterRef,
    callback: Callback,
    user: *mut c_void,
    frames: *mut u32,
    output: *mut AudioBufferList1,
    descriptions: *mut AudioStreamPacketDescription,
) -> i32 {
    unsafe { (fw.fill)(converter, callback, user, frames, output, descriptions) }
}

pub unsafe fn audio_converter_convert_buffer(
    fw: &Framework,
    converter: AudioConverterRef,
    size: u32,
    input: *const c_void,
    output_size: *mut u32,
    output: *mut c_void,
) -> i32 {
    unsafe { (fw.convert)(converter, size, input, output_size, output) }
}
