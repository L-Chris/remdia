//! Source Reader configured for native compressed media types only.
use anyhow::{Context, Result, ensure};
use remdia_graph::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};
use windows::{
    Win32::{
        Media::MediaFoundation::*,
        System::{
            Com::{
                COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize,
                StructuredStorage::{PROPVARIANT, PropVariantClear, PropVariantToInt64},
            },
            Variant::VT_I8,
        },
    },
    core::{GUID, PCWSTR},
};
use windows::{
    Win32::{
        Storage::EnhancedStorage::{
            PKEY_Music_AlbumTitle, PKEY_Music_Artist, PKEY_ThumbnailStream, PKEY_Title,
        },
        System::{
            Com::{CoTaskMemFree, IStream, StructuredStorage::PropVariantToStringAlloc},
            Variant::{VT_STREAM, VT_UNKNOWN},
        },
        UI::Shell::PropertiesSystem::{
            GPS_DEFAULT, IPropertyStore, SHGetPropertyStoreFromParsingName,
        },
    },
    core::Interface,
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
        unsafe { CoUninitialize() }
    }
}
fn startup() -> Result<()> {
    static INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|e| e.to_string()))
        .clone()
        .map_err(anyhow::Error::msg)
}
unsafe fn blob(media: &IMFMediaType, key: &GUID) -> Result<Vec<u8>> {
    unsafe {
        let size = media.GetBlobSize(key).unwrap_or(0) as usize;
        ensure!(size <= 65536, "oversize system codec configuration");
        let mut out = vec![0; size];
        if size > 0 {
            media.GetBlob(key, &mut out, None)?;
        }
        Ok(out)
    }
}
unsafe fn track(media: &IMFMediaType, index: u32) -> Result<Option<Track>> {
    unsafe {
        let major = media.GetGUID(&MF_MT_MAJOR_TYPE)?;
        let subtype = media.GetGUID(&MF_MT_SUBTYPE)?;
        if major != MFMediaType_Audio && major != MFMediaType_Video {
            return Ok(None);
        }
        let audio = major == MFMediaType_Audio;
        let mut width = 0;
        let mut height = 0;
        let mut rate = 0;
        let mut channels = 0;
        let codec;
        if audio {
            rate = media.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?;
            channels = u16::try_from(media.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)?)?;
            codec = if subtype == MFAudioFormat_AAC {
                "aac".into()
            } else if subtype == MFAudioFormat_MP3 {
                "mp3".into()
            } else if subtype == MFAudioFormat_FLAC {
                "flac".into()
            } else if subtype == MFAudioFormat_ALAC {
                "alac".into()
            } else if subtype == MFAudioFormat_Opus {
                "opus".into()
            } else if subtype == MFAudioFormat_Vorbis {
                "vorbis".into()
            } else if subtype == MFAudioFormat_Dolby_AC3 {
                "ac3".into()
            } else if subtype == MFAudioFormat_Dolby_DDPlus {
                "eac3".into()
            } else if subtype == MFAudioFormat_PCM || subtype == MFAudioFormat_Float {
                let bits = media.GetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE)?;
                format!(
                    "pcm_{}{bits}{}",
                    if subtype == MFAudioFormat_Float {
                        "f"
                    } else if bits == 8 {
                        "u"
                    } else {
                        "s"
                    },
                    if bits == 8 { "" } else { "le" }
                )
            } else {
                format!("system_{subtype:?}")
            };
        } else {
            let size = media.GetUINT64(&MF_MT_FRAME_SIZE)?;
            width = (size >> 32) as u32;
            height = size as u32;
            codec = if subtype == MFVideoFormat_H264 {
                "h264"
            } else if subtype == MFVideoFormat_HEVC {
                "hevc"
            } else if subtype == MFVideoFormat_VP90 {
                "vp9"
            } else if subtype == MFVideoFormat_VP80 {
                "vp8"
            } else {
                "unsupported_video"
            }
            .into();
        }
        let extra = if audio {
            serde_json::json!(blob(media, &MF_MT_USER_DATA)?)
        } else {
            serde_json::json!([blob(media, &MF_MT_MPEG_SEQUENCE_HEADER)?])
        };
        Ok(Some(Track {
            index: index as usize,
            kind: if audio { "audio" } else { "video" }.into(),
            codec,
            time_base: Rational {
                num: 1,
                den: 10_000_000,
            },
            width,
            height,
            sample_rate: rate,
            channels,
            attached_picture: false,
            metadata: BTreeMap::new(),
            initialization: serde_json::json!({"extra_data":extra,"system_user_data":true,"container":"mediafoundation","bits_per_sample":media.GetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE).ok(),"channel_mask":media.GetUINT32(&MF_MT_AUDIO_CHANNEL_MASK).unwrap_or(0)}),
        }))
    }
}
pub struct Demux {
    reader: IMFSourceReader,
    info: NodeInfo,
    control: Control,
    generation: u64,
    ended: BTreeSet<u32>,
    selected: BTreeSet<u32>,
    source: std::path::PathBuf,
    properties: Option<IPropertyStore>,
    _apartment: Apartment,
}
impl Demux {
    pub fn open(config: &NodeConfig, control: Control) -> Result<Self> {
        let request: Request = serde_json::from_value(config.settings.clone())?;
        request.validate()?;
        control.check()?;
        ensure!(
            !request.source.contains("://"),
            "system demux currently requires a local file source"
        );
        let source = std::path::Path::new(&request.source).canonicalize()?;
        let file_path = source.clone();
        let source = source.to_str().context("invalid media file path")?;
        let source = if let Some(unc) = source.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{unc}")
        } else {
            source.strip_prefix(r"\\?\").unwrap_or(source).to_owned()
        };
        let source = source.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let apartment = Apartment::new()?;
        startup()?;
        unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 4)?;
            let attributes = attributes.context("missing source attributes")?;
            attributes.SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, 1)?;
            attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 0)?;
            let reader = MFCreateSourceReaderFromURL(PCWSTR(source.as_ptr()), &attributes)?;
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            let mut tracks = Vec::new();
            let mut selected = BTreeSet::new();
            for index in 0..64 {
                let media = match reader.GetNativeMediaType(index, 0) {
                    Ok(media) => media,
                    Err(error) if error.code() == MF_E_INVALIDSTREAMNUMBER => break,
                    Err(error) => return Err(error).context("query native stream type"),
                };
                if let Some(track) = track(&media, index)? {
                    reader.SetCurrentMediaType(index, None, &media)?;
                    reader.SetStreamSelection(index, true)?;
                    selected.insert(index);
                    tracks.push(track);
                }
            }
            ensure!(!tracks.is_empty(), "no system-readable media tracks");
            let duration = if let Ok(mut value) = reader
                .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
            {
                let status = PropVariantToInt64(&value);
                let _ = PropVariantClear(&mut value);
                status.ok().map(|duration| duration / 10)
            } else {
                None
            };
            let properties: Option<IPropertyStore> =
                SHGetPropertyStoreFromParsingName(PCWSTR(source.as_ptr()), None, GPS_DEFAULT).ok();
            let mut metadata = BTreeMap::new();
            if let Some(store) = &properties {
                for (key, name) in [
                    (PKEY_Title, "title"),
                    (PKEY_Music_Artist, "artist"),
                    (PKEY_Music_AlbumTitle, "album"),
                ] {
                    if let Ok(mut value) = store.GetValue(&key) {
                        if let Ok(text) = PropVariantToStringAlloc(&value) {
                            let string = text.to_string();
                            CoTaskMemFree(Some(text.0.cast()));
                            if let Ok(string) = string
                                && !string.is_empty()
                                && string.len() <= 8192
                            {
                                metadata.insert(name.into(), string);
                            }
                        }
                        let _ = PropVariantClear(&mut value);
                    }
                }
            }
            Ok(Self {
                reader,
                info: NodeInfo {
                    input: None,
                    output: PortCaps::new(DataKind::Packet),
                    max_output_bytes: request.max_packet_bytes,
                    media: Some(MediaInfo {
                        container: "mediafoundation".into(),
                        duration_us: duration,
                        origin_us: 0,
                        metadata,
                        tracks,
                    }),
                },
                control,
                generation: 1,
                ended: BTreeSet::new(),
                selected,
                source: file_path,
                properties,
                _apartment: apartment,
            })
        }
    }
}
impl Component for Demux {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn receive(&mut self) -> Result<Receive> {
        self.control.check()?;
        if self.ended == self.selected {
            return Ok(Receive::End);
        }
        unsafe {
            let mut stream = 0;
            let mut flags = 0;
            let mut pts = 0;
            let mut sample = None;
            self.reader.ReadSample(
                MF_SOURCE_READER_ANY_STREAM.0 as u32,
                0,
                Some(&mut stream),
                Some(&mut flags),
                Some(&mut pts),
                Some(&mut sample),
            )?;
            self.control.check()?;
            ensure!(
                flags & (MF_SOURCE_READERF_ERROR.0 as u32) == 0,
                "system demux stream error"
            );
            ensure!(
                flags
                    & ((MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0
                        | MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED.0)
                        as u32)
                    == 0,
                "system stream format changed; reopen graph"
            );
            if flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
                self.ended.insert(stream);
            }
            let Some(sample) = sample else {
                return Ok(if self.ended == self.selected {
                    Receive::End
                } else {
                    Receive::NeedInput
                });
            };
            let buffer = sample.ConvertToContiguousBuffer()?;
            let length = buffer.GetCurrentLength()? as usize;
            ensure!(
                length <= self.info.max_output_bytes,
                "compressed sample exceeds packet budget"
            );
            if length == 0 {
                return Ok(Receive::NeedInput);
            }
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            let bytes = std::slice::from_raw_parts(data, length).to_vec();
            buffer.Unlock()?;
            let dts = sample
                .GetUINT64(&MFSampleExtension_DecodeTimestamp)
                .map(|n| n as i64)
                .unwrap_or(pts);
            let duration = u64::try_from(sample.GetSampleDuration().unwrap_or(0))?;
            let keyframe = sample
                .GetUINT32(&MFSampleExtension_CleanPoint)
                .ok()
                .map(|n| n != 0);
            Ok(Receive::Output(MediaBuffer::new(
                BufferHeader {
                    stream_id: stream,
                    generation: self.generation,
                    info: FrameInfo::Packet {
                        pts,
                        dts,
                        duration,
                        time_base: Rational {
                            num: 1,
                            den: 10_000_000,
                        },
                        keyframe,
                        trim_start: 0,
                        trim_end: 0,
                    },
                },
                vec![bytes],
            )))
        }
    }
    fn command(&mut self, command: &serde_json::Value) -> Result<Option<MediaBuffer>> {
        if command["op"] != "artwork" {
            return Ok(None);
        }
        self.control.check()?;
        let image =
            if let Some(bytes) = super::metadata::flac_artwork(&self.source, 8 * 1024 * 1024)? {
                Some(bytes)
            } else if let Some(store) = &self.properties {
                unsafe {
                    let stream = if let Ok(mut value) = store.GetValue(&PKEY_ThumbnailStream) {
                        let inner = &value.Anonymous.Anonymous;
                        let stream: Option<IStream> = if inner.vt == VT_STREAM {
                            (*inner.Anonymous.pStream).clone()
                        } else if inner.vt == VT_UNKNOWN {
                            inner
                                .Anonymous
                                .punkVal
                                .as_ref()
                                .and_then(|unknown| unknown.cast().ok())
                        } else {
                            None
                        };
                        let _ = PropVariantClear(&mut value);
                        stream
                    } else {
                        None
                    };
                    if let Some(stream) = stream {
                        let mut bytes = Vec::new();
                        let mut chunk = [0u8; 65536];
                        loop {
                            self.control.check()?;
                            let mut read = 0;
                            let result = stream.Read(
                                chunk.as_mut_ptr().cast(),
                                chunk.len() as u32,
                                Some(&mut read),
                            );
                            result.ok()?;
                            ensure!(
                                bytes.len() + read as usize <= 8 * 1024 * 1024,
                                "system artwork exceeds budget"
                            );
                            bytes.extend_from_slice(&chunk[..read as usize]);
                            if read == 0 || result == windows::core::HRESULT(1) {
                                break;
                            }
                        }
                        (!bytes.is_empty()).then_some(bytes)
                    } else {
                        None
                    }
                }
            } else {
                None
            };
        Ok(image.map(|bytes| {
            MediaBuffer::new(
                BufferHeader {
                    stream_id: 0,
                    generation: self.generation,
                    info: FrameInfo::Artwork {
                        codec: "system-image".into(),
                    },
                },
                vec![bytes],
            )
        }))
    }
    fn drain(&mut self) -> Result<()> {
        self.ended = self.selected.clone();
        Ok(())
    }
    fn reset(&mut self, generation: u64, target: i64) -> Result<()> {
        self.control.check()?;
        let target = target.checked_mul(10).context("seek timestamp overflow")?;
        unsafe {
            let mut value = PROPVARIANT::default();
            (*value.Anonymous.Anonymous).vt = VT_I8;
            (*value.Anonymous.Anonymous).Anonymous.hVal = target;
            self.reader.Flush(MF_SOURCE_READER_ALL_STREAMS.0 as u32)?;
            self.reader.SetCurrentPosition(&GUID::zeroed(), &value)?;
        }
        self.generation = generation;
        self.ended.clear();
        Ok(())
    }
}

pub fn supports(kind: &str) -> Result<bool> {
    let _apartment = Apartment::new()?;
    startup()?;
    let wanted = match kind {
        "mp4" => &["video/mp4", "audio/mp4"][..],
        "mp3" => &["audio/mpeg"][..],
        "wav" => &["audio/wav", "audio/x-wav"][..],
        "flac" => &["audio/flac", "audio/x-flac"][..],
        "ogg" => &["audio/ogg", "application/ogg"][..],
        "aac" => &["audio/aac", "audio/aacp"][..],
        "mkv" => &["video/x-matroska"][..],
        _ => &[][..],
    };
    unsafe {
        use windows::Win32::System::Com::{
            CoTaskMemFree,
            StructuredStorage::{PropVariantGetElementCount, PropVariantGetStringElem},
        };
        let mut types = MFGetSupportedMimeTypes()?;
        let count = PropVariantGetElementCount(&types);
        let mut found = false;
        for index in 0..count.min(1024) {
            if let Ok(value) = PropVariantGetStringElem(&types, index) {
                let name = value.to_string();
                CoTaskMemFree(Some(value.0.cast()));
                if name.as_deref().is_ok_and(|name| wanted.contains(&name)) {
                    found = true;
                    break;
                }
            }
        }
        PropVariantClear(&mut types)?;
        Ok(found)
    }
}
