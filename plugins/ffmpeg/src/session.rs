use super::{Owned, reply};
use anyhow::{Context, Result, bail, ensure};
use ffmpeg_next::{self as f, ChannelLayout, codec, format, frame, media, software};
use remdia_plugin_abi::Control;
use remdia_types::*;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

fn again(e: f::Error) -> bool {
    matches!(e,f::Error::Other {errno} if errno==f::error::EAGAIN)
}
fn micros(ts: i64, tb: f::Rational) -> Result<i64> {
    Rational {
        num: tb.numerator(),
        den: tb.denominator(),
    }
    .rescale(ts, Rational::MICROS)
}
fn tags(d: f::DictionaryRef<'_>) -> BTreeMap<String, String> {
    d.iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}
struct Video {
    index: usize,
    tb: f::Rational,
    dec: f::decoder::Video,
    drained: bool,
}
struct Audio {
    index: usize,
    tb: f::Rational,
    dec: f::decoder::Audio,
    drained: bool,
    resampler: Option<software::resampling::Context>,
    next_sample: Option<i64>,
    flushed: bool,
}
pub struct Session {
    input: format::context::Input,
    request: Request,
    pub info: MediaInfo,
    video: Option<Video>,
    audio: Option<Audio>,
    active: Option<bool>,
    eof: bool,
    target: i64,
    base: Instant,
    deadline: Arc<AtomicU64>,
    control: Control,
}
impl Session {
    pub fn open(request: Request, control: Control) -> Result<Self> {
        f::init()?;
        let base = Instant::now();
        let deadline = Arc::new(AtomicU64::new(request.timeout_ms));
        let limit = deadline.clone();
        // Only addresses cross this closure; Control remains valid until close.
        let context = control.context as usize;
        let cancelled = control.cancelled;
        let mut options = f::Dictionary::new();
        options.set("probesize", "2097152");
        options.set("analyzeduration", "2000000");
        options.set(
            "rw_timeout",
            &(request.timeout_ms.saturating_mul(1000)).to_string(),
        );
        let input = format::input_with_interrupt_and_dictionary(
            &request.source,
            move || unsafe {
                cancelled(context as *mut _) != 0
                    || base.elapsed().as_millis() as u64 > limit.load(Ordering::Relaxed)
            },
            options,
        )
        .context("open media")?;
        let start = unsafe { (*input.as_ptr()).start_time };
        let origin_us = if start == f::ffi::AV_NOPTS_VALUE {
            0
        } else {
            start
        };
        let mut tracks = Vec::new();
        for s in input.streams() {
            let p = s.parameters();
            let raw = unsafe { &*p.as_ptr() };
            tracks.push(Track {
                index: s.index(),
                kind: format!("{:?}", p.medium()).to_lowercase(),
                codec: p.id().name().into(),
                time_base: Rational {
                    num: s.time_base().numerator(),
                    den: s.time_base().denominator(),
                },
                width: raw.width.max(0) as u32,
                height: raw.height.max(0) as u32,
                sample_rate: raw.sample_rate.max(0) as u32,
                channels: raw.ch_layout.nb_channels.max(0) as u16,
                attached_picture: s
                    .disposition()
                    .contains(format::stream::Disposition::ATTACHED_PIC),
                metadata: tags(s.metadata()),
            });
        }
        let info = MediaInfo {
            container: input.format().name().into(),
            duration_us: (input.duration() > 0).then(|| input.duration()),
            origin_us,
            metadata: tags(input.metadata()),
            tracks,
        };
        let mut video = None;
        let mut audio = None;
        if matches!(request.task, Task::Playback | Task::Thumbnail) {
            let stream = input
                .streams()
                .filter(|s| {
                    s.parameters().medium() == media::Type::Video
                        && !s
                            .disposition()
                            .contains(format::stream::Disposition::ATTACHED_PIC)
                })
                .max_by_key(|s| {
                    s.disposition()
                        .contains(format::stream::Disposition::DEFAULT)
                });
            if let Some(s) = stream {
                let dec = open_video(s.parameters(), &request)?;
                ensure!(
                    u64::from(dec.width()) * u64::from(dec.height()) <= 33_554_432,
                    "video dimensions exceed v1 limit"
                );
                video = Some(Video {
                    index: s.index(),
                    tb: s.time_base(),
                    dec,
                    drained: false,
                });
            }
        }
        if matches!(request.task, Task::Playback | Task::Audio)
            && let Some(s) = input.streams().best(media::Type::Audio)
        {
            let mut ctx = codec::context::Context::from_parameters(s.parameters())?;
            ctx.set_threading(codec::threading::Config::count(request.threads as usize));
            let mut dec = ctx.decoder().audio()?;
            dec.set_packet_time_base(s.time_base());
            audio = Some(Audio {
                index: s.index(),
                tb: s.time_base(),
                dec,
                drained: false,
                resampler: None,
                next_sample: None,
                flushed: false,
            });
        }
        if request.task != Task::Probe {
            ensure!(
                video.is_some() || audio.is_some(),
                "no matching playable track"
            );
        }
        Ok(Self {
            input,
            request,
            info,
            video,
            audio,
            active: None,
            eof: false,
            target: 0,
            base,
            deadline,
            control,
        })
    }
    fn check(&self) -> Result<()> {
        ensure!(
            unsafe { (self.control.cancelled)(self.control.context) } == 0,
            "cancelled"
        );
        ensure!(
            (self.base.elapsed().as_millis() as u64) <= self.deadline.load(Ordering::Relaxed),
            "operation timed out"
        );
        Ok(())
    }
    fn begin(&self) {
        self.deadline.store(
            self.base.elapsed().as_millis() as u64 + self.request.timeout_ms,
            Ordering::Relaxed,
        );
    }
    pub fn seek(&mut self, target: i64) -> Result<()> {
        ensure!(
            self.request.task != Task::Probe && target >= 0,
            "invalid seek"
        );
        self.begin();
        self.check()?;
        let ts = target
            .checked_add(self.info.origin_us)
            .context("seek overflow")?;
        unsafe {
            let io = (*self.input.as_mut_ptr()).pb;
            if !io.is_null() {
                (*io).error = 0;
                (*io).eof_reached = 0;
            }
        }
        self.input.seek(ts, ..ts)?;
        if let Some(v) = &mut self.video {
            v.dec.flush();
            v.drained = false;
        }
        if let Some(a) = &mut self.audio {
            a.dec.flush();
            a.drained = false;
            a.resampler = None;
            a.next_sample = None;
            a.flushed = false;
        }
        self.eof = false;
        self.active = None;
        self.target = target;
        Ok(())
    }
    pub fn artwork(&mut self) -> Result<Option<Owned>> {
        for s in self.input.streams() {
            if !s
                .disposition()
                .contains(format::stream::Disposition::ATTACHED_PIC)
            {
                continue;
            }
            let packet = unsafe { &(*s.as_ptr()).attached_pic };
            if packet.size <= 0 || packet.data.is_null() {
                continue;
            }
            ensure!(
                packet.size as usize <= 8 * 1024 * 1024,
                "artwork exceeds 8 MiB limit"
            );
            let mut out = reply(&FrameInfo::Artwork {
                codec: s.parameters().id().name().into(),
            })?;
            out.bytes =
                unsafe { std::slice::from_raw_parts(packet.data, packet.size as usize) }.to_vec();
            return Ok(Some(out));
        }
        Ok(None)
    }
    pub fn next(&mut self) -> Result<Option<Owned>> {
        ensure!(
            self.request.task != Task::Probe,
            "probe sessions do not decode"
        );
        self.begin();
        loop {
            self.check()?;
            if let Some(video) = self.active {
                if video {
                    let v = self.video.as_mut().unwrap();
                    let mut frame = frame::Video::empty();
                    match v.dec.receive_frame(&mut frame) {
                        Ok(()) => {
                            let pts = frame
                                .timestamp()
                                .or(frame.pts())
                                .map(|p| micros(p, v.tb))
                                .transpose()?
                                .map(|p| p - self.info.origin_us);
                            if pts.is_some_and(|p| p < self.target) {
                                continue;
                            }
                            return Ok(Some(video_output(frame, pts, v.tb, &self.request)?));
                        }
                        Err(e) if again(e) || e == f::Error::Eof => self.active = None,
                        Err(e) => return Err(e).context("receive video"),
                    }
                } else {
                    let a = self.audio.as_mut().unwrap();
                    let mut frame = frame::Audio::empty();
                    match a.dec.receive_frame(&mut frame) {
                        Ok(()) => {
                            if let Some(output) = audio_output(
                                a,
                                frame,
                                &self.request,
                                self.info.origin_us,
                                self.target,
                            )? {
                                return Ok(Some(output));
                            }
                        }
                        Err(e) if again(e) || e == f::Error::Eof => self.active = None,
                        Err(e) => return Err(e).context("receive audio"),
                    }
                }
                continue;
            }
            if self.eof {
                if let Some(v) = &mut self.video
                    && !v.drained
                {
                    v.dec.send_eof()?;
                    v.drained = true;
                    self.active = Some(true);
                    continue;
                }
                if let Some(a) = &mut self.audio {
                    if !a.drained {
                        a.dec.send_eof()?;
                        a.drained = true;
                        self.active = Some(false);
                        continue;
                    }
                    if !a.flushed {
                        if let Some(resampler) = &mut a.resampler {
                            let layout = layout(self.request.audio_channels);
                            let mut output = frame::Audio::new(
                                format::Sample::F32(format::sample::Type::Packed),
                                4096,
                                layout,
                            );
                            resampler.flush(&mut output)?;
                            if output.samples() > 0 {
                                if let Some(out) =
                                    pcm_output(a, &output, &self.request, self.target)?
                                {
                                    return Ok(Some(out));
                                }
                                continue;
                            }
                        }
                        a.flushed = true;
                    }
                }
                return Ok(None);
            }
            let mut packet = f::Packet::empty();
            match packet.read(&mut self.input) {
                Ok(()) => {}
                Err(f::Error::Eof) => {
                    self.eof = true;
                    continue;
                }
                Err(f::Error::InvalidData) => continue,
                Err(e) => return Err(e).context("read packet"),
            }
            ensure!(
                packet.size() <= self.request.max_packet_bytes,
                "packet exceeds budget"
            );
            if let Some(v) = &mut self.video
                && packet.stream() == v.index
            {
                match v.dec.send_packet(&packet) {
                    Ok(()) => self.active = Some(true),
                    Err(f::Error::InvalidData) => {}
                    Err(e) => return Err(e).context("submit video"),
                };
                continue;
            }
            if let Some(a) = &mut self.audio
                && packet.stream() == a.index
            {
                match a.dec.send_packet(&packet) {
                    Ok(()) => self.active = Some(false),
                    Err(f::Error::InvalidData) => {}
                    Err(e) => return Err(e).context("submit audio"),
                };
            }
        }
    }
}
fn layout(channels: u16) -> ChannelLayout {
    if channels == 1 {
        ChannelLayout::MONO
    } else {
        ChannelLayout::STEREO
    }
}
fn audio_output(
    a: &mut Audio,
    mut input: frame::Audio,
    r: &Request,
    origin: i64,
    target: i64,
) -> Result<Option<Owned>> {
    if input.channel_layout().is_empty() {
        input.set_channel_layout(ChannelLayout::default(input.channels() as i32));
    }
    let changed = a.resampler.as_ref().is_none_or(|s| {
        s.input().format != input.format()
            || s.input().rate != input.rate()
            || s.input().channel_layout != input.channel_layout()
    });
    if changed {
        a.resampler = Some(software::resampling::Context::get(
            input.format(),
            input.channel_layout(),
            input.rate(),
            format::Sample::F32(format::sample::Type::Packed),
            layout(r.audio_channels),
            r.audio_rate,
        )?);
    }
    let pts = input
        .timestamp()
        .or(input.pts())
        .map(|p| micros(p, a.tb))
        .transpose()?
        .map(|p| p - origin);
    let position = pts
        .map(|p| {
            Rational::MICROS.rescale(
                p,
                Rational {
                    num: 1,
                    den: r.audio_rate as i32,
                },
            )
        })
        .transpose()?;
    if a.next_sample.is_none()
        || position
            .zip(a.next_sample)
            .is_some_and(|(a, b)| (a - b).abs() > r.audio_rate as i64 / 10)
    {
        a.next_sample = Some(position.unwrap_or(0));
    }
    let s = a.resampler.as_mut().unwrap();
    let count = (input.samples() as u64 * r.audio_rate as u64).div_ceil(input.rate().max(1) as u64)
        as usize
        + s.delay().map_or(0, |d| d.output.max(0) as usize)
        + 256;
    ensure!(
        count.saturating_mul(r.audio_channels as usize * 4) <= r.max_frame_bytes,
        "audio frame exceeds budget"
    );
    let mut output = frame::Audio::new(
        format::Sample::F32(format::sample::Type::Packed),
        count,
        layout(r.audio_channels),
    );
    s.run(&input, &mut output)?;
    pcm_output(a, &output, r, target)
}
fn pcm_output(
    a: &mut Audio,
    output: &frame::Audio,
    r: &Request,
    target: i64,
) -> Result<Option<Owned>> {
    let start = a.next_sample.unwrap_or(0);
    let n = output.samples();
    a.next_sample = Some(start + n as i64);
    let target_sample = (i128::from(target) * i128::from(r.audio_rate) + 999_999) / 1_000_000;
    let skip = (target_sample - i128::from(start)).max(0).min(n as i128) as usize;
    if skip >= n {
        return Ok(None);
    }
    let stride = r.audio_channels as usize * 4;
    ensure!(
        n.saturating_mul(stride) <= r.max_frame_bytes,
        "audio frame exceeds budget"
    );
    let pts = Rational {
        num: 1,
        den: r.audio_rate as i32,
    }
    .rescale(start + skip as i64, Rational::MICROS)?;
    let mut out = reply(&FrameInfo::Audio {
        pts_us: pts,
        rate: r.audio_rate,
        channels: r.audio_channels,
        samples: n - skip,
        format: "f32le".into(),
    })?;
    out.bytes = output.data(0)[skip * stride..n * stride].to_vec();
    Ok(Some(out))
}
fn video_output(
    mut video: frame::Video,
    pts: Option<i64>,
    tb: f::Rational,
    r: &Request,
) -> Result<Owned> {
    let hardware = unsafe { !(*video.as_ptr()).hw_frames_ctx.is_null() };
    ensure!(
        u64::from(video.width()) * u64::from(video.height()) * 8 <= r.max_frame_bytes as u64,
        "video frame exceeds conservative budget"
    );
    if hardware {
        let mut host = frame::Video::empty();
        let result =
            unsafe { f::ffi::av_hwframe_transfer_data(host.as_mut_ptr(), video.as_ptr(), 0) };
        if result < 0 {
            bail!("hardware download failed: {}", f::Error::from(result));
        }
        let result = unsafe { f::ffi::av_frame_copy_props(host.as_mut_ptr(), video.as_ptr()) };
        if result < 0 {
            bail!("hardware metadata copy failed");
        }
        video = host;
    }
    // ffmpeg-next's data() assumes positive, ordinary planar strides. Keep
    // supported YUV layouts native and normalize palettes/negative strides/RGB.
    let native = matches!(
        video.format(),
        format::Pixel::YUV420P
            | format::Pixel::YUVJ420P
            | format::Pixel::YUV420P10LE
            | format::Pixel::YUV420P12LE
            | format::Pixel::YUV420P16LE
            | format::Pixel::NV12
            | format::Pixel::P010LE
            | format::Pixel::P016LE
    ) && (0..video.planes()).all(|i| unsafe { (*video.as_ptr()).linesize[i] > 0 });
    if r.task == Task::Thumbnail || !native {
        let bound = if r.task == Task::Thumbnail {
            600.0
        } else {
            f64::MAX
        };
        let scale = (bound / video.width().max(1) as f64)
            .min(bound / video.height().max(1) as f64)
            .min(1.0);
        let w = (video.width() as f64 * scale).round().max(1.0) as u32;
        let h = (video.height() as f64 * scale).round().max(1.0) as u32;
        let mut scaler = software::scaling::Context::get(
            video.format(),
            video.width(),
            video.height(),
            format::Pixel::RGBA,
            w,
            h,
            software::scaling::flag::Flags::BILINEAR,
        )?;
        let mut rgba = frame::Video::empty();
        scaler.run(&video, &mut rgba)?;
        let result = unsafe { f::ffi::av_frame_copy_props(rgba.as_mut_ptr(), video.as_ptr()) };
        ensure!(result >= 0, "copy converted video metadata failed");
        video = rgba;
    }
    let mut bytes = 0usize;
    ensure!(video.planes() <= 4, "unsupported plane count");
    for i in 0..video.planes() {
        ensure!(
            unsafe { (*video.as_ptr()).linesize[i] > 0 && !(*video.as_ptr()).data[i].is_null() },
            "invalid video plane"
        );
        bytes = bytes
            .checked_add(
                video
                    .stride(i)
                    .checked_mul(video.plane_height(i) as usize)
                    .context("plane overflow")?,
            )
            .context("frame overflow")?;
    }
    ensure!(
        video.planes() <= 4 && bytes <= r.max_frame_bytes,
        "native video planes exceed budget"
    );
    let sar = video.aspect_ratio();
    let info = FrameInfo::Video {
        pts_us: pts,
        duration_us: micros(video.packet().duration, tb)?,
        width: video.width(),
        height: video.height(),
        format: format!("{:?}", video.format()).to_lowercase(),
        planes: (0..video.planes())
            .map(|i| Plane {
                stride: video.stride(i),
                rows: video.plane_height(i) as usize,
            })
            .collect(),
        sample_aspect_ratio: Rational {
            num: sar.numerator(),
            den: sar.denominator(),
        },
        color_space: format!("{:?}", video.color_space()),
        color_range: format!("{:?}", video.color_range()),
        color_primaries: format!("{:?}", video.color_primaries()),
        color_transfer: format!("{:?}", video.color_transfer_characteristic()),
        hardware_decoded: hardware,
    };
    let mut out = reply(&info)?;
    out.video = Some(video);
    Ok(out)
}
fn open_video(params: codec::Parameters, r: &Request) -> Result<f::decoder::Video> {
    #[cfg(windows)]
    if r.hardware
        && r.task != Task::Thumbnail
        && let Ok(dec) = open_d3d11(params.clone(), r.threads)
    {
        return Ok(dec);
    }
    let mut context = codec::context::Context::from_parameters(params)?;
    context.set_threading(codec::threading::Config::count(r.threads as usize));
    Ok(context.decoder().video()?)
}
#[cfg(windows)]
fn open_d3d11(params: codec::Parameters, threads: u32) -> Result<f::decoder::Video> {
    use f::ffi::*;
    unsafe extern "C" fn choose(
        ctx: *mut AVCodecContext,
        formats: *const AVPixelFormat,
    ) -> AVPixelFormat {
        unsafe {
            let mut p = formats;
            while *p != AVPixelFormat::AV_PIX_FMT_NONE {
                if *p == AVPixelFormat::AV_PIX_FMT_D3D11 {
                    return *p;
                }
                p = p.add(1);
            }
            avcodec_default_get_format(ctx, formats)
        }
    }
    let mut context = codec::context::Context::from_parameters(params)?;
    context.set_threading(codec::threading::Config::count(threads as usize));
    let codec = f::decoder::find(context.id()).context("decoder unavailable")?;
    unsafe {
        let mut supported = false;
        for i in 0..64 {
            let config = avcodec_get_hw_config(codec.as_ptr(), i);
            if config.is_null() {
                break;
            }
            if (*config).device_type == AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA
                && (*config).methods & AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 != 0
            {
                supported = true;
                break;
            }
        }
        ensure!(supported, "D3D11VA unsupported");
        let mut device = std::ptr::null_mut();
        let result = av_hwdevice_ctx_create(
            &mut device,
            AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
        );
        if result < 0 {
            return Err(f::Error::from(result).into());
        }
        (*context.as_mut_ptr()).hw_device_ctx = device;
        (*context.as_mut_ptr()).get_format = Some(choose);
        (*context.as_mut_ptr()).extra_hw_frames = 4;
    }
    Ok(context.decoder().open_as(codec)?.video()?)
}
