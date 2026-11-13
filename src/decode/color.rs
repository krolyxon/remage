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
    #[cfg_attr(not(feature = "heif"), allow(dead_code))]
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
            Ok(t) => {
                let t = Self(t);
                // Most tagged files carry some flavour of the sRGB profile.
                (!t.is_identity()).then_some(t)
            }
            Err(e) => {
                log::debug!("colour transform unavailable: {e:?}");
                None
            }
        }
    }

    /// Whether the transform leaves colours alone, to within rounding (an sRGB source).
    fn is_identity(&self) -> bool {
        const STEPS: [u8; 9] = [0, 32, 64, 96, 128, 160, 192, 224, 255];
        let mut src = Vec::with_capacity((STEPS.len().pow(3) + 256) * 4);
        for r in STEPS {
            for g in STEPS {
                for b in STEPS {
                    src.extend([r, g, b, 255]);
                }
            }
        }
        for v in 0..=255 {
            src.extend([v, v, v, 255]);
        }
        let mut dst = vec![0; src.len()];
        self.0.transform(&src, &mut dst).is_ok()
            && src.iter().zip(&dst).all(|(a, b)| a.abs_diff(*b) <= 1)
    }

    /// Convert RGBA8 pixels in place; alpha is untouched. Large images are split across a
    /// few threads.
    pub fn apply(&self, pixels: &mut [u8]) {
        /// Pixels are converted through a scratch buffer of this many bytes.
        const CHUNK: usize = 64 << 10;
        /// Bytes that make another thread worth starting.
        const PER_THREAD: usize = 4 << 20;
        let band = |band: &mut [u8]| {
            let mut scratch = [0u8; CHUNK];
            for chunk in band.chunks_mut(CHUNK) {
                let src = &mut scratch[..chunk.len()];
                src.copy_from_slice(chunk);
                if let Err(e) = self.0.transform(src, chunk) {
                    log::debug!("colour transform failed: {e:?}");
                    chunk.copy_from_slice(src);
                }
            }
        };
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        let threads = (pixels.len() / PER_THREAD).clamp(1, cores.min(4));
        if threads == 1 {
            return band(pixels);
        }
        let len = (pixels.len() / 4).div_ceil(threads) * 4;
        std::thread::scope(|s| {
            for part in pixels.chunks_mut(len) {
                s.spawn(|| band(part));
            }
        });
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
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| &p[..3])
        .copied()
        .filter(|v| v.is_finite())
        .fold(0.0f32, f32::max);
    let hdr = peak > 1.0;
    let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let mut out = Vec::with_capacity(rgba.len());
    for p in rgba.as_chunks::<4>().0.iter() {
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
    match img {
        DynamicImage::ImageRgb8(rgb) => (size, expand(rgb.as_raw(), 3)),
        img if is_float(&img) => (size, tonemap(img.into_rgba32f().as_raw())),
        img => (size, img.into_rgba8().into_raw()),
    }
}

/// Whether an orientation turns the image on its side, swapping width and height.
pub fn swaps_axes(o: Orientation) -> bool {
    use Orientation::*;
    matches!(o, Rotate90 | Rotate270 | Rotate90FlipH | Rotate270FlipH)
}

/// Where stored pixels land in the upright image, as a map between unit squares:
/// `upright = m * stored + t`. The renderer folds this into an image's placement, so
/// rotated photos are never turned on the CPU.
pub fn orient_uv(o: Orientation) -> ([f32; 4], [f32; 2]) {
    use Orientation::*;
    match o {
        NoTransforms => ([1.0, 0.0, 0.0, 1.0], [0.0, 0.0]),
        Rotate90 => ([0.0, -1.0, 1.0, 0.0], [1.0, 0.0]),
        Rotate180 => ([-1.0, 0.0, 0.0, -1.0], [1.0, 1.0]),
        Rotate270 => ([0.0, 1.0, -1.0, 0.0], [0.0, 1.0]),
        FlipHorizontal => ([-1.0, 0.0, 0.0, 1.0], [1.0, 0.0]),
        FlipVertical => ([1.0, 0.0, 0.0, -1.0], [0.0, 1.0]),
        Rotate90FlipH => ([0.0, 1.0, 1.0, 0.0], [0.0, 0.0]),
        Rotate270FlipH => ([0.0, -1.0, -1.0, 0.0], [1.0, 1.0]),
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
        3 => {
            let mut out = vec![255; samples.len() / 3 * 4];
            for (o, p) in out
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(samples.as_chunks::<3>().0)
            {
                o[..3].copy_from_slice(p);
            }
            out
        }
        2 => samples
            .as_chunks::<2>()
            .0
            .iter()
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
    fn gpu_orientation_matches_the_cpu() {
        use Orientation::*;
        // 3×2, every pixel distinct.
        let (w, h) = (3u32, 2u32);
        let px: Vec<u8> = (0..w * h).flat_map(|i| [i as u8, 0, 0, 255]).collect();
        for o in [
            NoTransforms,
            Rotate90,
            Rotate180,
            Rotate270,
            FlipHorizontal,
            FlipVertical,
            Rotate90FlipH,
            Rotate270FlipH,
        ] {
            let ([uw, uh], upright) = orient([w, h], px.clone(), o);
            assert_eq!(swaps_axes(o), uw == h, "{o:?}");
            let (m, t) = orient_uv(o);
            for y in 0..h {
                for x in 0..w {
                    // The stored pixel's centre, mapped into the upright image.
                    let (u, v) = ((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
                    let ux = ((m[0] * u + m[1] * v + t[0]) * uw as f32) as u32;
                    let uy = ((m[2] * u + m[3] * v + t[1]) * uh as f32) as u32;
                    let got = upright[((uy * uw + ux) * 4) as usize];
                    assert_eq!(got, (y * w + x) as u8, "{o:?} at {x},{y}");
                }
            }
        }
    }

    #[test]
    fn srgb_profiles_need_no_transform() {
        let srgb = ColorProfile::new_srgb().encode().unwrap();
        assert!(ToSrgb::from_icc(&srgb).is_none());
        let p3 = ColorProfile::new_display_p3().encode().unwrap();
        let t = ToSrgb::from_icc(&p3).expect("P3 transform");
        // Threaded and single-threaded conversion agree, whatever the length.
        let px: Vec<u8> = (0..(9 << 20) + 4).map(|i| (i * 7 % 256) as u8).collect();
        let mut big = px.clone();
        t.apply(&mut big);
        for at in [0, 4 << 20, px.len() - 8] {
            let mut small = px[at..at + 8].to_vec();
            t.apply(&mut small);
            assert_eq!(small, big[at..at + 8], "at {at}");
        }
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
        assert_eq!(
            expand(&[1, 2, 3, 4, 5, 6, 7], 3),
            [1, 2, 3, 255, 4, 5, 6, 255]
        );
    }
}
