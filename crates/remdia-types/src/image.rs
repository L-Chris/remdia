//! Portable CPU image metadata. Pixel storage uses the ordinary MediaBuffer.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub format: String,
    pub alpha: String,
    pub color_space: String,
}
impl ImageInfo {
    pub fn rgba(width: u32, height: u32) -> Result<Self> {
        let stride = (width as usize)
            .checked_mul(4)
            .ok_or_else(|| anyhow::anyhow!("image stride overflow"))?;
        ensure!(
            width > 0 && height > 0 && stride.checked_mul(height as usize).is_some(),
            "invalid image dimensions"
        );
        Ok(Self {
            width,
            height,
            stride,
            format: "rgba8".into(),
            alpha: "straight".into(),
            color_space: "srgb".into(),
        })
    }
    pub fn byte_len(&self) -> Result<usize> {
        self.stride
            .checked_mul(self.height as usize)
            .ok_or_else(|| anyhow::anyhow!("image size overflow"))
    }
    pub fn validate(&self, bytes: &[u8]) -> Result<()> {
        ensure!(
            self.width > 0
                && self.height > 0
                && self.format == "rgba8"
                && self.alpha == "straight"
                && self.color_space == "srgb",
            "unsupported image layout"
        );
        ensure!(
            (self.width as usize)
                .checked_mul(4)
                .is_some_and(|row| self.stride >= row)
                && self.byte_len()? == bytes.len(),
            "invalid image plane length"
        );
        Ok(())
    }
}
