use anyhow::{Context, Result, ensure};
use remdia_graph::*;
use std::{mem::ManuallyDrop, sync::OnceLock};
use windows::{
    Win32::{
        Media::MediaFoundation::*,
        System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
    },
    core::GUID,
};
struct Apartment;
impl Apartment {
    fn new() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
struct Activations {
    ptr: *mut Option<IMFActivate>,
    len: u32,
}
impl Drop for Activations {
    fn drop(&mut self) {
        unsafe {
            if !self.ptr.is_null() {
                for a in std::slice::from_raw_parts_mut(self.ptr, self.len as usize) {
                    drop(a.take());
                }
                CoTaskMemFree(Some(self.ptr.cast()));
            }
        }
    }
}
fn startup() -> Result<()> {
    static START: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    START
        .get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|e| e.to_string()))
        .clone()
        .map_err(anyhow::Error::msg)
}
pub struct Decoder {
    transform: IMFTransform,
    info: NodeInfo,
    track: Track,
    control: Control,
    width: u32,
    height: u32,
    stride: usize,
    display: DisplayArea,
    nal_length: usize,
    headers: Vec<u8>,
    inject: bool,
    draining: bool,
    generation: u64,
    _apartment: Apartment,
}
fn subtype(codec: &str) -> Result<GUID> {
    Ok(match codec {
        "h264" => MFVideoFormat_H264,
        "hevc" => MFVideoFormat_HEVC,
        "vp9" => MFVideoFormat_VP90,
        "vp8" => MFVideoFormat_VP80,
        "mpeg4" => MFVideoFormat_MP4V,
        "mpeg2" => MFVideoFormat_MPEG2,
        "vc1" => MFVideoFormat_WVC1,
        _ => anyhow::bail!("no Windows decoder mapping for {codec}"),
    })
}
fn read_u16(data: &[u8], at: &mut usize) -> Result<usize> {
    let bytes = data
        .get(*at..at.checked_add(2).context("codec config offset overflow")?)
        .context("truncated codec configuration")?;
    *at += 2;
    Ok(u16::from_be_bytes(bytes.try_into()?) as usize)
}
fn append_nal(data: &[u8], at: &mut usize, out: &mut Vec<u8>) -> Result<()> {
    let len = read_u16(data, at)?;
    let end = at.checked_add(len).context("NAL size overflow")?;
    let nal = data.get(*at..end).context("truncated NAL header")?;
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(nal);
    *at = end;
    Ok(())
}
fn configuration(track: &Track) -> Result<(usize, Vec<u8>)> {
    let arrays: Vec<Vec<u8>> = serde_json::from_value(
        track
            .initialization
            .get("extra_data")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )?;
    let Some(data) = arrays.first() else {
        return Ok((0, Vec::new()));
    };
    if track.codec == "h264" && data.first() == Some(&1) {
        ensure!(data.len() >= 7, "truncated AVC configuration");
        let length = (data[4] & 3) as usize + 1;
        let count = data[5] & 31;
        let mut at = 6;
        let mut headers = Vec::new();
        for _ in 0..count {
            append_nal(data, &mut at, &mut headers)?;
        }
        let count = *data.get(at).context("missing PPS count")?;
        at += 1;
        for _ in 0..count {
            append_nal(data, &mut at, &mut headers)?;
        }
        Ok((length, headers))
    } else if track.codec == "hevc" && data.first() == Some(&1) {
        ensure!(data.len() >= 23, "truncated HEVC configuration");
        let length = (data[21] & 3) as usize + 1;
        let count = data[22];
        let mut at = 23;
        let mut headers = Vec::new();
        for _ in 0..count {
            ensure!(at < data.len(), "truncated HEVC array");
            at += 1;
            let count = read_u16(data, &mut at)?;
            for _ in 0..count {
                append_nal(data, &mut at, &mut headers)?;
            }
        }
        Ok((length, headers))
    } else {
        Ok((0, data.clone()))
    }
}
/// Surface allocation dimensions can include codec alignment outside the image.
#[derive(Clone, Copy, Default)]
struct DisplayArea {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}
fn display_area(media: &IMFMediaType, width: u32, height: u32) -> Result<DisplayArea> {
    let mut area = DisplayArea {
        x: 0,
        y: 0,
        width: width as usize,
        height: height as usize,
    };
    for key in [MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_GEOMETRIC_APERTURE] {
        let mut bytes = [0u8; std::mem::size_of::<MFVideoArea>()];
        let mut len = 0;
        if unsafe { media.GetBlob(&key, &mut bytes, Some(&mut len)) }.is_ok()
            && len as usize == bytes.len()
        {
            // MFVideoArea has only integer fields; the blob need not be aligned.
            let aperture =
                unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<MFVideoArea>()) };
            ensure!(
                aperture.OffsetX.fract == 0 && aperture.OffsetY.fract == 0,
                "fractional Windows display aperture is unsupported"
            );
            ensure!(
                aperture.OffsetX.value >= 0
                    && aperture.OffsetY.value >= 0
                    && aperture.Area.cx > 0
                    && aperture.Area.cy > 0,
                "invalid Windows display aperture"
            );
            area = DisplayArea {
                x: aperture.OffsetX.value as usize,
                y: aperture.OffsetY.value as usize,
                width: aperture.Area.cx as usize,
                height: aperture.Area.cy as usize,
            };
            break;
        }
    }
    ensure!(
        area.x.is_multiple_of(2) && area.y.is_multiple_of(2),
        "unaligned NV12 display aperture is unsupported"
    );
    ensure!(
        area.x + area.width <= width as usize && area.y + area.height <= height as usize,
        "display aperture exceeds Windows surface"
    );
    Ok(area)
}
fn copy_nv12(
    bytes: &[u8],
    stride: usize,
    surface_height: usize,
    area: DisplayArea,
) -> Result<Vec<Vec<u8>>> {
    let uv_width = area.width.div_ceil(2) * 2;
    let uv_base = stride
        .checked_mul(surface_height)
        .context("NV12 surface overflow")?;
    let mut y = Vec::with_capacity(area.width * area.height);
    let mut uv = Vec::with_capacity(uv_width * area.height.div_ceil(2));
    for row in 0..area.height {
        let start = (area.y + row) * stride + area.x;
        y.extend_from_slice(
            bytes
                .get(start..start + area.width)
                .context("truncated NV12 luma")?,
        );
    }
    for row in 0..area.height.div_ceil(2) {
        let start = uv_base + (area.y / 2 + row) * stride + area.x;
        uv.extend_from_slice(
            bytes
                .get(start..start + uv_width)
                .context("truncated NV12 chroma")?,
        );
    }
    Ok(vec![y, uv])
}

impl Decoder {
    pub fn new(config: &NodeConfig, control: Control) -> Result<Self> {
        control.check()?;
        let apartment = Apartment::new()?;
        startup()?;
        let track: Track = serde_json::from_value(
            config
                .settings
                .get("track")
                .cloned()
                .context("Windows decoder requires a stream configuration")?,
        )?;
        ensure!(
            track.kind == "video" && track.width > 0 && track.height > 0,
            "invalid video stream configuration"
        );
        let maximum = config
            .settings
            .get("max_frame_bytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(128 * 1024 * 1024) as usize;
        let sub = subtype(&track.codec)?;
        let (nal_length, headers) = configuration(&track)?;
        unsafe {
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &sub)?;
            input.SetUINT64(
                &MF_MT_FRAME_SIZE,
                ((track.width as u64) << 32) | track.height as u64,
            )?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            if !headers.is_empty() {
                input.SetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &headers)?;
            }
            let filter = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: sub,
            };
            let mut activations = Activations {
                ptr: std::ptr::null_mut(),
                len: 0,
            };
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_DECODER,
                MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&filter),
                None,
                &mut activations.ptr,
                &mut activations.len,
            )?;
            ensure!(
                activations.len > 0,
                "Windows has no installed synchronous decoder for {}; install the required codec extension",
                track.codec
            );
            let mut selected = None;
            let mut failures = Vec::new();
            for i in 0..activations.len as usize {
                if let Some(activate) = &*activations.ptr.add(i) {
                    let result = (|| -> Result<IMFTransform> {
                        let transform: IMFTransform = activate.ActivateObject()?;
                        transform.SetInputType(0, &input, 0)?;
                        select_output(&transform)?;
                        Ok(transform)
                    })();
                    match result {
                        Ok(t) => {
                            selected = Some(t);
                            break;
                        }
                        Err(e) => failures.push(e.to_string()),
                    }
                }
            }
            let transform = selected.with_context(|| {
                format!(
                    "unable to negotiate a Windows {} decoder: {}",
                    track.codec,
                    failures.join("; ")
                )
            })?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            let mut output = PortCaps::new(DataKind::Video);
            output.format = Some("nv12".into());
            let mut decoder = Self {
                transform,
                info: NodeInfo {
                    input: Some(PortCaps::new(DataKind::Packet)),
                    output,
                    max_output_bytes: maximum,
                    media: None,
                },
                track,
                control,
                width: 0,
                height: 0,
                stride: 0,
                display: DisplayArea::default(),
                nal_length,
                headers,
                inject: true,
                draining: false,
                generation: 1,
                _apartment: apartment,
            };
            decoder.refresh_format()?;
            Ok(decoder)
        }
    }
    fn refresh_format(&mut self) -> Result<()> {
        unsafe {
            let media = self.transform.GetOutputCurrentType(0)?;
            ensure!(
                media.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12,
                "Windows decoder selected an unsupported output type"
            );
            let size = media.GetUINT64(&MF_MT_FRAME_SIZE)?;
            self.width = (size >> 32) as u32;
            self.height = size as u32;
            let stride = media
                .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                .map(|v| v as i32)
                .or_else(|_| {
                    MFGetStrideForBitmapInfoHeader(MFVideoFormat_NV12.data1, self.width)
                })?;
            ensure!(
                self.width > 0 && self.height > 0 && stride > 0,
                "unsupported Windows video dimensions/stride"
            );
            self.stride = stride as usize;
            self.display = display_area(&media, self.width, self.height)?;
            let size = self
                .stride
                .checked_mul(self.height as usize + self.height.div_ceil(2) as usize)
                .context("video allocation overflow")?;
            ensure!(
                size <= self.info.max_output_bytes,
                "video frame exceeds budget"
            );
            Ok(())
        }
    }
    fn annex_b(&self, data: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        if self.inject {
            out.extend_from_slice(&self.headers);
        }
        if self.nal_length == 0 {
            out.extend_from_slice(data);
            return Ok(out);
        }
        let mut at = 0;
        while at < data.len() {
            let end = at
                .checked_add(self.nal_length)
                .context("NAL prefix overflow")?;
            let prefix = data.get(at..end).context("truncated NAL length prefix")?;
            let len = prefix.iter().fold(0usize, |n, &b| (n << 8) | b as usize);
            at = end;
            let end = at.checked_add(len).context("NAL payload overflow")?;
            let nal = data.get(at..end).context("NAL length exceeds packet")?;
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
            at = end;
        }
        ensure!(
            out.len() <= 32 * 1024 * 1024,
            "expanded bitstream exceeds limit"
        );
        Ok(out)
    }
}
unsafe fn select_output(transform: &IMFTransform) -> Result<()> {
    unsafe {
        for i in 0..128 {
            let media = match transform.GetOutputAvailableType(0, i) {
                Ok(m) => m,
                Err(e) if e.code() == MF_E_NO_MORE_TYPES => break,
                Err(e) => return Err(e.into()),
            };
            if media.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                transform.SetOutputType(0, &media, 0)?;
                return Ok(());
            }
        }
    }
    anyhow::bail!("decoder cannot output CPU NV12")
}
impl Component for Decoder {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, input: &MediaBuffer) -> Result<Submit> {
        self.control.check()?;
        ensure!(!self.draining, "submit after video decoder drain");
        ensure!(
            input.header.stream_id == self.track.index as u32,
            "wrong video track"
        );
        let FrameInfo::Packet {
            pts,
            dts,
            duration,
            time_base,
            ..
        } = input.header.info
        else {
            anyhow::bail!("Windows decoder expected a compressed packet")
        };
        let data = self.annex_b(input.bytes())?;
        unsafe {
            let buffer = MFCreateMemoryBuffer(u32::try_from(data.len())?)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            let tb = Rational {
                num: 1,
                den: 10_000_000,
            };
            sample.SetSampleTime(time_base.rescale(pts, tb)?)?;
            sample.SetSampleDuration(time_base.rescale(i64::try_from(duration)?, tb)?)?;
            sample.SetUINT64(
                &MFSampleExtension_DecodeTimestamp,
                time_base.rescale(dts, tb)? as u64,
            )?;
            if self.inject {
                sample.SetUINT32(&MFSampleExtension_Discontinuity, 1)?;
            }
            match self.transform.ProcessInput(0, &sample, 0) {
                Ok(()) => {
                    self.inject = false;
                    self.generation = input.header.generation;
                    Ok(Submit::Accepted)
                }
                Err(e) if e.code() == MF_E_NOTACCEPTING => Ok(Submit::WouldBlock),
                Err(e) => Err(e).context("Windows video ProcessInput"),
            }
        }
    }
    fn receive(&mut self) -> Result<Receive> {
        self.control.check()?;
        for _ in 0..4 {
            unsafe {
                let requirements = self.transform.GetOutputStreamInfo(0)?;
                ensure!(
                    requirements.cbSize as usize <= self.info.max_output_bytes,
                    "MFT allocation exceeds budget"
                );
                let sample = if requirements.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32
                    != 0
                {
                    None
                } else {
                    let n = requirements.cbSize.max(
                        (self.stride * (self.height as usize + self.height.div_ceil(2) as usize))
                            as u32,
                    );
                    let buffer =
                        MFCreateAlignedMemoryBuffer(n, requirements.cbAlignment.saturating_sub(1))?;
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&buffer)?;
                    Some(sample)
                };
                let mut output = MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(sample),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                };
                let mut status = 0;
                let result =
                    self.transform
                        .ProcessOutput(0, std::slice::from_mut(&mut output), &mut status);
                let sample = ManuallyDrop::take(&mut output.pSample);
                drop(ManuallyDrop::take(&mut output.pEvents));
                match result {
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                        return Ok(if self.draining {
                            Receive::End
                        } else {
                            Receive::NeedInput
                        });
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        drop(sample);
                        select_output(&self.transform)?;
                        self.refresh_format()?;
                        continue;
                    }
                    Err(e) => return Err(e).context("Windows video ProcessOutput"),
                    Ok(()) => {}
                }
                let Some(sample) = sample else { continue };
                let pts = sample.GetSampleTime().ok().map(|v| v / 10);
                let duration = sample.GetSampleDuration().map(|v| v / 10).unwrap_or(0);
                let buffer = sample.ConvertToContiguousBuffer()?;
                let len = buffer.GetCurrentLength()? as usize;
                let ysize = self.stride * self.height as usize;
                let uvsize = self.stride * self.height.div_ceil(2) as usize;
                ensure!(
                    len >= ysize + uvsize && len <= self.info.max_output_bytes,
                    "invalid NV12 surface length"
                );
                let mut ptr = std::ptr::null_mut();
                buffer.Lock(&mut ptr, None, None)?;
                let bytes = std::slice::from_raw_parts(ptr, len);
                let planes = copy_nv12(bytes, self.stride, self.height as usize, self.display);
                buffer.Unlock()?;
                let planes = planes?;
                let media = self.transform.GetOutputCurrentType(0)?;
                let sar = media
                    .GetUINT64(&MF_MT_PIXEL_ASPECT_RATIO)
                    .unwrap_or((1u64 << 32) | 1);
                let color = match media.GetUINT32(&MF_MT_YUV_MATRIX).unwrap_or(0) {
                    1 => "bt709",
                    2 => "bt601",
                    4 => "bt2020",
                    _ => "unspecified",
                };
                let range = if media.GetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE).unwrap_or(2) == 1 {
                    "full"
                } else {
                    "limited"
                };
                let info = FrameInfo::Video {
                    pts_us: pts,
                    duration_us: duration,
                    width: self.display.width as u32,
                    height: self.display.height as u32,
                    format: "nv12".into(),
                    planes: vec![
                        Plane {
                            stride: self.display.width,
                            rows: self.display.height,
                        },
                        Plane {
                            stride: self.display.width.div_ceil(2) * 2,
                            rows: self.display.height.div_ceil(2),
                        },
                    ],
                    sample_aspect_ratio: Rational {
                        num: (sar >> 32) as i32,
                        den: sar as i32,
                    },
                    color_space: color.into(),
                    color_range: range.into(),
                    color_primaries: media
                        .GetUINT32(&MF_MT_VIDEO_PRIMARIES)
                        .unwrap_or(0)
                        .to_string(),
                    color_transfer: media
                        .GetUINT32(&MF_MT_TRANSFER_FUNCTION)
                        .unwrap_or(0)
                        .to_string(),
                    hardware_decoded: false,
                };
                return Ok(Receive::Output(MediaBuffer::new(
                    BufferHeader {
                        stream_id: self.track.index as u32,
                        generation: self.generation,
                        info,
                    },
                    planes,
                )));
            }
        }
        anyhow::bail!("MFT made no progress after output format changes")
    }
    fn drain(&mut self) -> Result<()> {
        if !self.draining {
            unsafe {
                self.transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
                self.transform
                    .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
            }
            self.draining = true;
        }
        Ok(())
    }
    fn reset(&mut self, g: u64, _target: i64) -> Result<()> {
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        self.draining = false;
        self.inject = true;
        self.generation = g;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nv12_copy_omits_alignment_padding_and_respects_aperture() {
        let bytes: Vec<_> = (0..72).collect(); // stride 8, six luma rows, three UV rows.
        let planes = copy_nv12(
            &bytes,
            8,
            6,
            DisplayArea {
                x: 2,
                y: 2,
                width: 4,
                height: 2,
            },
        )
        .unwrap();
        assert_eq!(planes[0], [18, 19, 20, 21, 26, 27, 28, 29]);
        assert_eq!(planes[1], [58, 59, 60, 61]);
        assert!(
            copy_nv12(
                &bytes[..60],
                8,
                6,
                DisplayArea {
                    x: 2,
                    y: 2,
                    width: 4,
                    height: 2
                }
            )
            .is_err()
        );
    }
}
