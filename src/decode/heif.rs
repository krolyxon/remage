//! HEIC/HEIF through the system libheif. libheif applies the file's rotation and mirroring.

use std::path::Path;

use anyhow::anyhow;
use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};

use super::color::ToSrgb;
use super::{DecodedImage, Options};

pub fn decode(path: &Path, opts: &Options) -> anyhow::Result<DecodedImage> {
    let bytes = std::fs::read(path)?;
    let ctx = HeifContext::read_from_bytes(&bytes)?;
    let handle = ctx.primary_image_handle()?;
    let lib = LibHeif::new();
    let image = lib.decode(&handle, ColorSpace::Rgb(RgbChroma::Rgba), None)?;
    let planes = image.planes();
    let plane = planes
        .interleaved
        .ok_or_else(|| anyhow!("no interleaved plane"))?;
    let (w, h) = (plane.width, plane.height);
    let row = w as usize * 4;
    let mut pixels = Vec::with_capacity(row * h as usize);
    for y in 0..h as usize {
        pixels.extend_from_slice(&plane.data[y * plane.stride..][..row]);
    }
    if opts.color {
        let transform = match handle.color_profile_raw().filter(|p| !p.data.is_empty()) {
            Some(icc) => ToSrgb::from_icc(&icc.data),
            None => handle.color_profile_nclx().and_then(|n| {
                ToSrgb::from_cicp(
                    n.color_primaries() as u8,
                    n.transfer_characteristics() as u8,
                )
            }),
        };
        if let Some(t) = transform {
            t.apply(&mut pixels);
        }
    }
    Ok(DecodedImage::still([w, h], pixels, "HEIF"))
}
