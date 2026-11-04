//! Camera RAW through `rawler`: the embedded preview first (fast), a full develop on demand
//! once the view zooms past the preview's resolution.

use std::path::Path;

use anyhow::anyhow;
use image::metadata::Orientation;
use rawler::decoders::RawDecodeParams;
use rawler::imgop::develop::RawDevelop;
use rawler::rawsource::RawSource;

use super::color;
use super::{DecodedImage, Options, Refine};

fn orientation(o: rawler::decoders::Orientation, opts: &Options) -> Orientation {
    if !opts.auto_orient {
        return Orientation::NoTransforms;
    }
    Orientation::from_exif(o.to_u16() as u8).unwrap_or(Orientation::NoTransforms)
}

pub fn decode(path: &Path, opts: &Options) -> anyhow::Result<DecodedImage> {
    let src = RawSource::new(path)?;
    let decoder = rawler::get_decoder(&src)?;
    let params = RawDecodeParams::default();
    let preview = decoder
        .preview_image(&src, &params)
        .ok()
        .flatten()
        .or_else(|| decoder.full_image(&src, &params).ok().flatten());
    let Some(preview) = preview else {
        return develop(path, opts);
    };
    // The raw data's size (cheap: no pixels decoded) is the size the view works in.
    let raw = decoder.raw_image(&src, &params, true)?;
    let o = orientation(raw.orientation, opts);
    let (size, pixels) = color::to_rgba8(preview);
    let (size, pixels) = color::orient(size, pixels, o);
    let (rw, rh) = match raw.crop_area {
        Some(r) => (r.d.w, r.d.h),
        None => (raw.width, raw.height),
    };
    let raw_long = rw.max(rh) as u32;
    let long = size[0].max(size[1]);
    let mut img = DecodedImage::still(size, pixels, "RAW");
    if raw_long > long {
        // Same shape as the preview, scaled up to the sensor's resolution.
        let k = raw_long as f64 / long as f64;
        img.source_size = [
            (size[0] as f64 * k).round() as u32,
            (size[1] as f64 * k).round() as u32,
        ];
        img.refine = Some(Refine::Raw);
    }
    Ok(img)
}

/// Demosaic and develop the raw data in full.
pub fn develop(path: &Path, opts: &Options) -> anyhow::Result<DecodedImage> {
    let raw = rawler::decode_file(path)?;
    let o = orientation(raw.orientation, opts);
    let dynamic = RawDevelop::default()
        .develop_intermediate(&raw)?
        .to_dynamic_image()
        .ok_or_else(|| anyhow!("develop produced no image"))?;
    let (size, pixels) = color::to_rgba8(dynamic);
    let (size, pixels) = color::orient(size, pixels, o);
    Ok(DecodedImage::still(size, pixels, "RAW"))
}
