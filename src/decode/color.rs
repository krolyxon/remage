//! Colour to sRGB (ICC and CICP through moxcms), HDR tone-mapping and EXIF orientation.

use image::DynamicImage;
use image::metadata::Orientation;
use moxcms::{
    CicpColorPrimaries, CicpProfile, ColorProfile, DataColorSpace, Layout, MatrixCoefficients,
    TransferCharacteristics, TransformOptions,
};

/// An sRGB-bound transform for RGBA8 pixels, or `None` when the source already is sRGB (or
/// the profile can't be used, in which case the pixels are shown as they are).
pub struct ToSrgb(std::sync::Arc<moxcms::Transform8BitExecutor>);

impl ToSrgb {
    pub fn from_icc(icc: &[u8]) -> Option<Self> {
        let src = match ColorProfile::new_from_slice(icc) {
            Ok(p) => p,
            Err(e) => {
                log::debug!("ICC profile ignored: {e:?}");
                return None;
            }
        };
        if src.color_space != DataColorSpace::Rgb {
            return None;
        }
        Self::from_profile(&src)
    }

    /// From H.273 code points (AVIF/HEIF `nclx`). sRGB/BT.709 and unspecified need nothing.
    pub fn from_cicp(primaries: u8, transfer: u8) -> Option<Self> {
        let primaries = CicpColorPrimaries::try_from(primaries).ok()?;
        if matches!(
            primaries,
            CicpColorPrimaries::Bt709 | CicpColorPrimaries::Unspecified
        ) {
            return None;
        }
        let transfer = TransferCharacteristics::try_from(transfer)
            .ok()
            .filter(|t| !matches!(t, TransferCharacteristics::Unspecified))
            .unwrap_or(TransferCharacteristics::Srgb);
        let profile = ColorProfile::new_from_cicp(CicpProfile {
            color_primaries: primaries,
            transfer_characteristics: transfer,
            matrix_coefficients: MatrixCoefficients::Identity,
            full_range: true,
        });
        Self::from_profile(&profile)
    }

    fn from_profile(src: &ColorProfile) -> Option<Self> {
        let dst = ColorProfile::new_srgb();
        match src.create_transform_8bit(
            Layout::Rgba,
            &dst,
            Layout::Rgba,
            TransformOptions::default(),
        ) {
            Ok(t) => Some(Self(t)),
            Err(e) => {
                log::debug!("colour transform unavailable: {e:?}");
                None
            }
        }
    }

    /// Convert RGBA8 pixels in place; alpha is untouched.
    pub fn apply(&self, pixels: &mut [u8]) {
        let src = pixels.to_vec();
        if let Err(e) = self.0.transform(&src, pixels) {
            log::debug!("colour transform failed: {e:?}");
            pixels.copy_from_slice(&src);
        }
    }
}

fn srgb_encode(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Narkowicz's ACES fit: filmic highlights, unchanged blacks.
fn aces(x: f32) -> f32 {
    ((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14)).clamp(0.0, 1.0)
}

/// Linear float RGBA (EXR, Radiance HDR, float TIFF) to sRGB RGBA8. Images whose values stay
/// within 0..1 are only encoded; brighter ones are tone-mapped.
pub fn tonemap(rgba: &[f32]) -> Vec<u8> {
    let peak = rgba
        .chunks_exact(4)
        .flat_map(|p| &p[..3])
        .copied()
        .filter(|v| v.is_finite())
        .fold(0.0f32, f32::max);
    let hdr = peak > 1.0;
    let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let mut out = Vec::with_capacity(rgba.len());
    for p in rgba.chunks_exact(4) {
        for &c in &p[..3] {
            let c = if c.is_finite() { c.max(0.0) } else { 0.0 };
            let c = if hdr { aces(c) } else { c };
            out.push(to8(srgb_encode(c)));
        }
        out.push(to8(if p[3].is_finite() { p[3] } else { 1.0 }));
    }
    out
}

/// Whether the image holds float samples that need tone-mapping rather than a plain cast.
pub fn is_float(img: &DynamicImage) -> bool {
    matches!(
        img,
        DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_)
    )
}

/// Straight RGBA8 from any decoded image, tone-mapping float data.
pub fn to_rgba8(img: DynamicImage) -> ([u32; 2], Vec<u8>) {
    let size = [img.width(), img.height()];
    if is_float(&img) {
        let f = img.into_rgba32f();
        (size, tonemap(f.as_raw()))
    } else {
        (size, img.into_rgba8().into_raw())
    }
}

/// Rotate/flip RGBA8 pixels upright.
pub fn orient(size: [u32; 2], pixels: Vec<u8>, o: Orientation) -> ([u32; 2], Vec<u8>) {
    if o == Orientation::NoTransforms {
        return (size, pixels);
    }
    let Some(buf) = image::RgbaImage::from_raw(size[0], size[1], pixels) else {
        return (size, Vec::new());
    };
    let mut img = DynamicImage::ImageRgba8(buf);
    img.apply_orientation(o);
    let out = img.into_rgba8();
    ([out.width(), out.height()], out.into_raw())
}

/// RGBA8 from interleaved 8-bit samples with 1–4 channels (grey, grey+alpha, RGB, RGBA).
pub fn expand(samples: &[u8], channels: usize) -> Vec<u8> {
    match channels {
        4 => samples.to_vec(),
        3 => samples
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        2 => samples
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        _ => samples.iter().flat_map(|&g| [g, g, g, 255]).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tonemapping() {
        // In-range linear data is only sRGB-encoded.
        let px = tonemap(&[0.0, 0.2158, 1.0, 0.5]);
        assert_eq!(px, [0, 128, 255, 128]);
        // HDR highlights roll off instead of clipping, and stay ordered.
        let px = tonemap(&[1.0, 4.0, 40.0, 1.0, f32::NAN, -1.0, 0.0, f32::INFINITY]);
        assert!(px[0] < px[1] && px[1] <= px[2] && px[2] == 255, "{px:?}");
        assert_eq!(&px[4..8], [0, 0, 0, 255]);
    }

    #[test]
    fn orientation_turns_pixels() {
        // 2×1: red, green. Rotate 90° clockwise → 1×2: red on top.
        let px = vec![255, 0, 0, 255, 0, 255, 0, 255];
        let (size, out) = orient([2, 1], px, Orientation::Rotate90);
        assert_eq!(size, [1, 2]);
        assert_eq!(&out[..4], [255, 0, 0, 255]);
        let (size, out) = orient([2, 1], out.clone(), Orientation::NoTransforms);
        assert_eq!((size, out.len()), ([2, 1], 8));
    }

    #[test]
    fn cicp_and_icc() {
        assert!(ToSrgb::from_cicp(1, 13).is_none(), "BT.709 is already sRGB");
        // Display P3 primaries with the sRGB curve: pure P3 red is outside sRGB.
        let t = ToSrgb::from_cicp(12, 13).expect("P3 transform");
        let mut px = [255, 0, 0, 200];
        t.apply(&mut px);
        assert_eq!(px[0], 255);
        assert!(px[3] == 200);
        assert!(ToSrgb::from_icc(b"not a profile").is_none());
        assert_eq!(expand(&[7, 9], 2), [7, 7, 7, 9]);
    }
}
