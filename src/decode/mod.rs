//! Format sniffing and dispatch to the decoder backends.

pub mod color;

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;

use anyhow::{Context, anyhow};
use fast_image_resize as fir;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, metadata::Orientation};

/// Refuse single images that would need more than this to decode.
const MAX_ALLOC: u64 = 4 << 30;

/// Straight-alpha RGBA8 in sRGB, oriented upright, ready to upload.
pub struct DecodedImage {
    pub size: [u32; 2],
    pub pixels: Vec<u8>,
    pub format: &'static str,
    /// Size before any downscale to fit the GPU's texture limit.
    pub source_size: [u32; 2],
}

impl DecodedImage {
    pub fn bytes(&self) -> usize {
        self.pixels.len()
    }
}

impl std::fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedImage")
            .field("size", &self.size)
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

static EXTENSIONS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    ImageFormat::all()
        .filter(ImageFormat::reading_enabled)
        .flat_map(|f| f.extensions_str().iter().copied())
        .chain(["apng", "jfif"])
        .collect()
});

/// Whether the extension alone says we can decode this file.
pub fn is_known_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.iter().any(|k| k.eq_ignore_ascii_case(e)))
}

/// Check magic bytes for files whose extension is unknown.
pub fn sniff(path: &Path) -> bool {
    let mut buf = [0u8; 64];
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let Ok(n) = f.read(&mut buf) else {
        return false;
    };
    image::guess_format(&buf[..n]).is_ok_and(|f| f.reading_enabled())
}

fn label(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "PNG",
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Gif => "GIF",
        ImageFormat::WebP => "WEBP",
        ImageFormat::Pnm => "PNM",
        ImageFormat::Tiff => "TIFF",
        ImageFormat::Tga => "TGA",
        ImageFormat::Dds => "DDS",
        ImageFormat::Bmp => "BMP",
        ImageFormat::Ico => "ICO",
        ImageFormat::Hdr => "HDR",
        ImageFormat::OpenExr => "EXR",
        ImageFormat::Farbfeld => "FARBFELD",
        ImageFormat::Avif => "AVIF",
        ImageFormat::Qoi => "QOI",
        _ => "IMAGE",
    }
}

/// Decode `path`, downscaling if either side exceeds `max_dim` (the GPU texture limit).
pub fn decode(path: &Path, max_dim: u32) -> anyhow::Result<DecodedImage> {
    // `open` guesses from the extension; magic bytes take precedence when recognised.
    let mut reader = ImageReader::open(path)?.with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    let format = reader.format().ok_or_else(|| anyhow!("unknown format"))?;
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    let rgba = img.into_rgba8();
    let source_size = [rgba.width(), rgba.height()];
    let mut out = DecodedImage {
        size: source_size,
        pixels: rgba.into_raw(),
        format: label(format),
        source_size,
    };
    if source_size[0] > max_dim || source_size[1] > max_dim {
        out = downscale(out, max_dim)?;
    }
    Ok(out)
}

/// Shrink so neither side exceeds `max_dim`. A stopgap until M5 tiles huge images.
pub fn downscale(img: DecodedImage, max_dim: u32) -> anyhow::Result<DecodedImage> {
    let [w, h] = img.size;
    let k = max_dim as f64 / w.max(h) as f64;
    let (nw, nh) = (
        ((w as f64 * k) as u32).max(1),
        ((h as f64 * k) as u32).max(1),
    );
    let src = fir::images::Image::from_vec_u8(w, h, img.pixels, fir::PixelType::U8x4)
        .context("resize source")?;
    let mut dst = fir::images::Image::new(nw, nh, fir::PixelType::U8x4);
    fir::Resizer::new()
        .resize(&src, &mut dst, None)
        .context("resize")?;
    log::debug!("downscaled {w}×{h} to {nw}×{nh} for the GPU");
    Ok(DecodedImage {
        size: [nw, nh],
        pixels: dst.into_vec(),
        ..img
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_extensions() {
        assert!(is_known_extension(Path::new("a/b.JPG")));
        assert!(is_known_extension(Path::new("b.png")));
        assert!(!is_known_extension(Path::new("b.txt")));
        assert!(!is_known_extension(Path::new("png")));
    }

    #[test]
    fn decodes_and_downscales_png() {
        let dir = std::env::temp_dir().join(format!("remage-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("noext");
        image::RgbaImage::from_pixel(64, 32, image::Rgba([255, 0, 0, 128]))
            .save_with_format(&path, ImageFormat::Png)
            .unwrap();
        assert!(sniff(&path));
        let img = decode(&path, 16).unwrap();
        assert_eq!(img.format, "PNG");
        assert_eq!(img.source_size, [64, 32]);
        assert_eq!(img.size, [16, 8]);
        assert_eq!(img.pixels.len(), 16 * 8 * 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
