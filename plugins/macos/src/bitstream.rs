use anyhow::{Context, Result, ensure};
use h264_reader::nal::{
    Nal, RefNal,
    sps::{ChromaFormat, FrameMbsFlags, Profile, SeqParameterSet},
};
use remdia_graph::Track;

#[derive(Clone)]
pub struct Configuration {
    pub hevc: bool,
    pub length: usize,
    pub sets: Vec<Vec<u8>>,
    pub reorder: usize,
}
fn nal(data: &[u8], at: &mut usize) -> Result<Vec<u8>> {
    let end = at.checked_add(2).context("parameter offset overflow")?;
    let len = u16::from_be_bytes(
        data.get(*at..end)
            .context("truncated parameter length")?
            .try_into()?,
    ) as usize;
    *at = end;
    let end = at.checked_add(len).context("parameter size overflow")?;
    let bytes = data.get(*at..end).context("truncated parameter set")?;
    ensure!(!bytes.is_empty(), "empty parameter set");
    *at = end;
    Ok(bytes.to_vec())
}
impl Configuration {
    pub fn parse(track: &Track) -> Result<Self> {
        let arrays: Vec<Vec<u8>> = serde_json::from_value(
            track
                .initialization
                .get("extra_data")
                .cloned()
                .context("missing video initialization")?,
        )?;
        let data = arrays.first().context("missing codec configuration")?;
        ensure!(data.len() <= 1024 * 1024, "oversize codec configuration");
        ensure!(
            data.first() == Some(&1),
            "VideoToolbox requires avcC/hvcC configuration"
        );
        let hevc = track.codec == "hevc";
        ensure!(
            hevc || track.codec == "h264",
            "unsupported macOS video codec: {}",
            track.codec
        );
        let mut sets = Vec::new();
        let length;
        if hevc {
            ensure!(data.len() >= 23, "truncated HEVC configuration");
            length = (data[21] & 3) as usize + 1;
            let mut at = 23;
            for _ in 0..data[22] {
                let kind = *data.get(at).context("missing HEVC array type")? & 63;
                at += 1;
                let end = at.checked_add(2).context("array offset overflow")?;
                let count = u16::from_be_bytes(
                    data.get(at..end)
                        .context("missing HEVC array count")?
                        .try_into()?,
                );
                at = end;
                ensure!(count <= 64, "too many HEVC parameter sets");
                for _ in 0..count {
                    let bytes = nal(data, &mut at)?;
                    if matches!(kind, 32..=34) {
                        sets.push(bytes);
                    }
                }
            }
        } else {
            ensure!(data.len() >= 7, "truncated AVC configuration");
            length = (data[4] & 3) as usize + 1;
            let mut at = 6;
            for _ in 0..data[5] & 31 {
                sets.push(nal(data, &mut at)?);
            }
            let count = *data.get(at).context("missing AVC PPS count")?;
            at += 1;
            ensure!(count <= 64, "too many AVC picture parameters");
            for _ in 0..count {
                sets.push(nal(data, &mut at)?);
            }
        }
        ensure!(matches!(length, 1 | 2 | 4), "invalid NAL prefix size");
        ensure!(sets.len() <= 64, "too many codec parameters");
        let mut reorder = 0;
        let mut seen_sps = false;
        for bytes in &sets {
            let kind = if hevc {
                (bytes[0] >> 1) & 63
            } else {
                bytes[0] & 31
            };
            if kind == if hevc { 33 } else { 7 } {
                reorder = reorder.max(if hevc {
                    hevc_reorder(bytes)?
                } else {
                    h264_reorder(bytes)?
                });
                seen_sps = true;
            }
        }
        ensure!(
            seen_sps && reorder <= 16,
            "missing SPS or excessive reorder depth"
        );
        for kind in if hevc { &[32, 33, 34][..] } else { &[7, 8][..] } {
            ensure!(
                sets.iter()
                    .any(|b| if hevc { (b[0] >> 1) & 63 } else { b[0] & 31 } == *kind),
                "missing video parameter {kind}"
            );
        }
        Ok(Self {
            hevc,
            length,
            sets,
            reorder,
        })
    }
    pub fn validate_packet(&self, bytes: &[u8]) -> Result<()> {
        ensure!(!bytes.is_empty(), "empty compressed access unit");
        let mut at = 0;
        while at < bytes.len() {
            let end = at.checked_add(self.length).context("NAL offset overflow")?;
            let prefix = bytes.get(at..end).context("truncated NAL prefix")?;
            let len = prefix.iter().fold(0usize, |n, &b| n * 256 + b as usize);
            at = end;
            let end = at.checked_add(len).context("NAL length overflow")?;
            let data = bytes.get(at..end).context("NAL payload exceeds packet")?;
            ensure!(!data.is_empty(), "empty NAL unit");
            let kind = if self.hevc {
                (data[0] >> 1) & 63
            } else {
                data[0] & 31
            };
            if matches!((self.hevc, kind), (false, 7 | 8) | (true, 32..=34)) {
                // avc3/hev1 changes need a new format description at a random-access boundary.
                ensure!(
                    self.sets.iter().any(|s| s == data),
                    "video parameter sets changed; reopen from a keyframe"
                );
            }
            at = end;
        }
        Ok(())
    }
}
fn h264_reorder(bytes: &[u8]) -> Result<usize> {
    let n = RefNal::new(bytes, &[], true);
    let s = SeqParameterSet::from_bits(n.rbsp_bits())
        .map_err(|e| anyhow::anyhow!("invalid H.264 SPS: {e:?}"))?;
    ensure!(
        matches!(s.frame_mbs_flags, FrameMbsFlags::Frames),
        "interlaced H.264 is unsupported"
    );
    ensure!(
        s.chroma_info.chroma_format == ChromaFormat::YUV420
            && s.chroma_info.bit_depth_luma_minus8 == 0
            && s.chroma_info.bit_depth_chroma_minus8 == 0,
        "macOS first version requires 8-bit 4:2:0 SDR"
    );
    if let Some(v) = &s.vui_parameters {
        if let Some(c) = v
            .video_signal_type
            .as_ref()
            .and_then(|v| v.colour_description.as_ref())
        {
            ensure!(
                !matches!(c.transfer_characteristics, 16 | 18),
                "HDR requires tone mapping support"
            );
        }
        if let Some(r) = &v.bitstream_restrictions {
            return Ok(r.max_num_reorder_frames as usize);
        }
    }
    if matches!(
        s.profile(),
        Profile::Baseline | Profile::ConstrainedBaseline
    ) {
        return Ok(0);
    }
    // Annex E: without bitstream restrictions use the level's MaxDpbFrames.
    let limits = s.level().limits().context("unknown H.264 level")?;
    let mb = s
        .pic_width_in_mbs()
        .checked_mul(s.pic_height_in_map_units())
        .context("H.264 size overflow")?;
    ensure!(mb > 0, "empty H.264 picture");
    Ok((limits.max_dpb_mbs / mb).min(16) as usize)
}
fn hevc_reorder(bytes: &[u8]) -> Result<usize> {
    let s = scuffle_h265::SpsNALUnit::parse(bytes)?.rbsp;
    ensure!(
        s.chroma_format_idc == 1 && s.bit_depth_luma_minus8 == 0 && s.bit_depth_chroma_minus8 == 0,
        "macOS first version requires 8-bit 4:2:0 SDR"
    );
    ensure!(
        !s.profile_tier_level.general_profile.interlaced_source_flag,
        "interlaced HEVC is unsupported"
    );
    if let Some(v) = &s.vui_parameters {
        ensure!(
            !v.field_seq_flag && !matches!(v.video_signal_type.transfer_characteristics, 16 | 18),
            "interlaced/HDR HEVC is unsupported"
        );
    }
    Ok(*s
        .sub_layer_ordering_info
        .sps_max_num_reorder_pics
        .iter()
        .max()
        .context("missing HEVC reorder limit")? as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixture_parameters_supply_reorder_depth_and_fallback_dpb() {
        let file = include_bytes!("../../../tests/fixtures/av.mp4");
        let at = file.windows(4).position(|v| v == b"avcC").unwrap();
        let size = u32::from_be_bytes(file[at - 4..at].try_into().unwrap()) as usize;
        let data = &file[at + 4..at - 4 + size];
        let t: Track = serde_json::from_value(serde_json::json!({"index":0,"kind":"video","codec":"h264","time_base":{"num":1,"den":1000},"width":160,"height":90,"sample_rate":0,"channels":0,"attached_picture":false,"metadata":{},"initialization":{"extra_data":[data]}})).unwrap();
        let c = Configuration::parse(&t).unwrap();
        assert_eq!(c.reorder, 2);
        assert_eq!(c.length, 4);
        assert!(!c.hevc);
        assert_eq!(c.sets.len(), 2);
        assert!(c.validate_packet(&[0, 0, 0, 1, 0x65]).is_ok());
    }
    #[test]
    fn reject_bad_configuration_and_unbounded_nal_lengths() {
        let mut t: Track = serde_json::from_value(serde_json::json!({"index":0,"kind":"video","codec":"h264","time_base":{"num":1,"den":1000},"width":160,"height":90,"sample_rate":0,"channels":0,"attached_picture":false,"metadata":{},"initialization":{"extra_data":[[1,2,3]]}})).unwrap();
        assert!(Configuration::parse(&t).is_err());
        t.initialization["extra_data"] = serde_json::json!([[1, 100, 0, 31, 255, 225, 255, 255]]);
        assert!(Configuration::parse(&t).is_err());
        let c = Configuration {
            hevc: false,
            length: 4,
            sets: vec![],
            reorder: 0,
        };
        assert!(c.validate_packet(&[255, 255, 255, 255, 1]).is_err());
    }
}
