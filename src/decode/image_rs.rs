//! The `image` crate: stills (with ICC and EXIF orientation), GIF/APNG/WebP animation, and
//! multi-page TIFF.

use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Seek};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, anyhow};
use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::metadata::Orientation;
use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageFormat, ImageReader};

use super::color::{self, ToSrgb};
use super::{DecodedImage, Frame, MAX_ALLOC, MAX_FRAMES_BYTES, Options, label};

/// Browsers show GIF frames with tiny delays at this pace, and so do we.
const MIN_DELAY: Duration = Duration::from_millis(20);
const DEFAULT_DELAY: Duration = Duration::from_millis(100);

fn limits() -> image::Limits {
    let mut l = image::Limits::default();
    l.max_alloc = Some(MAX_ALLOC);
    l
}

pub fn decode_file(
    path: &Path,
    format: ImageFormat,
    opts: &Options,
) -> anyhow::Result<DecodedImage> {
    let open = || -> anyhow::Result<BufReader<File>> { Ok(BufReader::new(File::open(path)?)) };
    let icc = |d: &mut dyn ImageDecoder| {
        if opts.color {
            d.icc_profile().ok().flatten()
        } else {
            None
        }
    };
    let animated = match format {
        ImageFormat::Gif => {
            let mut d = GifDecoder::new(open()?)?;
            let icc = icc(&mut d);
            animation(d, icc, format)?
        }
        ImageFormat::Png => {
            let mut d = PngDecoder::new(open()?)?;
            if d.is_apng()? {
                let icc = icc(&mut d);
                animation(d.apng()?, icc, format)?
            } else {
                None
            }
        }
        ImageFormat::WebP => {
            let mut d = WebPDecoder::new(open()?)?;
            if d.has_animation() {
                let icc = icc(&mut d);
                animation(d, icc, format)?
            } else {
                None
            }
        }
        _ => None,
    };
    if let Some(img) = animated {
        return Ok(img);
    }
    let mut img = decode_reader(open()?, format, opts)?;
    if format == ImageFormat::Tiff {
        add_tiff_pages(path, &mut img, opts);
    }
    Ok(img)
}

/// A still image from any reader: ICC to sRGB, float tone-mapped, oriented upright.
pub fn decode_reader<R: BufRead + Seek>(
    reader: R,
    format: ImageFormat,
    opts: &Options,
) -> anyhow::Result<DecodedImage> {
    let mut reader = ImageReader::with_format(reader, format);
    reader.limits(limits());
    let mut decoder = reader.into_decoder()?;
    let orientation = if opts.auto_orient {
        decoder.orientation().unwrap_or(Orientation::NoTransforms)
    } else {
        Orientation::NoTransforms
    };
    let icc = if opts.color {
        decoder.icc_profile().ok().flatten()
    } else {
        None
    };
    let img = DynamicImage::from_decoder(decoder)?;
    let float = color::is_float(&img);
    let (size, mut pixels) = color::to_rgba8(img);
    // Float data is linear scene light; an embedded profile doesn't describe the 8-bit result.
    if !float && let Some(t) = icc.as_deref().and_then(ToSrgb::from_icc) {
        t.apply(&mut pixels);
    }
    let (size, pixels) = color::orient(size, pixels, orientation);
    Ok(DecodedImage::still(size, pixels, label(format)))
}

/// Decode from memory, guessing the format from the bytes (external decoders' output).
pub fn decode_bytes(bytes: &[u8], opts: &Options) -> anyhow::Result<DecodedImage> {
    let format = image::guess_format(bytes).context("unrecognised output")?;
    decode_reader(Cursor::new(bytes), format, opts)
}

/// All frames of an animation, or `None` when it has only one (decode it as a still, which
/// keeps its orientation).
fn animation<'a>(
    decoder: impl AnimationDecoder<'a>,
    icc: Option<Vec<u8>>,
    format: ImageFormat,
) -> anyhow::Result<Option<DecodedImage>> {
    let transform = icc.as_deref().and_then(ToSrgb::from_icc);
    let mut frames = Vec::new();
    let mut size = [0, 0];
    let mut bytes = 0;
    for frame in decoder.into_frames() {
        let frame = frame?;
        let (num, den) = frame.delay().numer_denom_ms();
        let ms = num.checked_div(den).unwrap_or(0);
        let delay = match Duration::from_millis(ms as u64) {
            d if d < MIN_DELAY => DEFAULT_DELAY,
            d => d,
        };
        let buf = frame.into_buffer();
        size = [buf.width(), buf.height()];
        let mut pixels = buf.into_raw();
        if let Some(t) = &transform {
            t.apply(&mut pixels);
        }
        bytes += pixels.len();
        frames.push(Frame { pixels, delay });
        if bytes > MAX_FRAMES_BYTES {
            log::warn!("animation truncated at {} frames", frames.len());
            break;
        }
    }
    match frames.len() {
        0 => Err(anyhow!("no frames")),
        1 => Ok(None),
        n => {
            log::debug!("{n} frames");
            Ok(Some(DecodedImage {
                size,
                frames,
                animated: true,
                format: label(format),
                source_size: size,
                refine: None,
            }))
        }
    }
}

/// Pages after the first, as extra (untimed) frames. Pages of another size are skipped.
fn add_tiff_pages(path: &Path, img: &mut DecodedImage, opts: &Options) {
    let Ok(file) = File::open(path) else { return };
    let Ok(mut d) = tiff::decoder::Decoder::new(BufReader::new(file)) else {
        return;
    };
    let mut bytes = img.bytes();
    while d.more_images() {
        if d.next_image().is_err() {
            break;
        }
        match tiff_page(&mut d) {
            Ok((size, pixels)) => {
                // `image` rotated the first page; rotate the rest the same way.
                let o = orientation_of(&mut d, opts);
                let (size, pixels) = color::orient(size, pixels, o);
                if size != img.size {
                    log::debug!("TIFF page of another size skipped");
                    continue;
                }
                bytes += pixels.len();
                img.frames.push(Frame {
                    pixels,
                    delay: Duration::ZERO,
                });
            }
            Err(e) => log::debug!("TIFF page skipped: {e:#}"),
        }
        if bytes > MAX_FRAMES_BYTES {
            log::warn!("multi-page TIFF truncated at {} pages", img.frames.len());
            break;
        }
    }
}

fn orientation_of<R: std::io::Read + Seek>(
    d: &mut tiff::decoder::Decoder<R>,
    opts: &Options,
) -> Orientation {
    if !opts.auto_orient {
        return Orientation::NoTransforms;
    }
    d.find_tag_unsigned::<u8>(tiff::tags::Tag::Orientation)
        .ok()
        .flatten()
        .and_then(Orientation::from_exif)
        .unwrap_or(Orientation::NoTransforms)
}

fn tiff_page<R: std::io::Read + Seek>(
    d: &mut tiff::decoder::Decoder<R>,
) -> anyhow::Result<([u32; 2], Vec<u8>)> {
    use tiff::ColorType as C;
    use tiff::decoder::DecodingResult as D;
    let (w, h) = d.dimensions()?;
    let channels = match d.colortype()? {
        C::Gray(8 | 16) => 1,
        C::GrayA(8 | 16) => 2,
        C::RGB(8 | 16) => 3,
        C::RGBA(8 | 16) => 4,
        other => return Err(anyhow!("unsupported colour type {other:?}")),
    };
    let samples: Vec<u8> = match d.read_image()? {
        D::U8(v) => v,
        D::U16(v) => v.into_iter().map(|s| (s >> 8) as u8).collect(),
        _ => return Err(anyhow!("unsupported sample format")),
    };
    if samples.len() < (w * h) as usize * channels {
        return Err(anyhow!("short page"));
    }
    Ok(([w, h], color::expand(&samples, channels)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::gif::GifEncoder;
    use image::{Delay, Rgba, RgbaImage};

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("remage-imgrs-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn gif_animation_keeps_frames_and_delays() {
        let path = dir().join("a.gif");
        {
            let mut enc = GifEncoder::new(File::create(&path).unwrap());
            for (c, ms) in [(255, 50), (0, 0)] {
                let buf = RgbaImage::from_pixel(4, 3, Rgba([c, 0, 0, 255]));
                let f = image::Frame::from_parts(buf, 0, 0, Delay::from_numer_denom_ms(ms, 1));
                enc.encode_frame(f).unwrap();
            }
        }
        let img = decode_file(&path, ImageFormat::Gif, &Options::default()).unwrap();
        assert!(img.animated);
        assert_eq!(img.size, [4, 3]);
        assert_eq!(
            img.delays(),
            [Duration::from_millis(50), DEFAULT_DELAY],
            "a zero delay plays at the browser pace"
        );
        assert_eq!(img.frame(1)[0], 0);
    }

    #[test]
    fn multi_page_tiff() {
        use tiff::encoder::{TiffEncoder, colortype};
        let path = dir().join("pages.tif");
        {
            let mut enc = TiffEncoder::new(File::create(&path).unwrap()).unwrap();
            enc.write_image::<colortype::RGB8>(2, 2, &[10; 12]).unwrap();
            enc.write_image::<colortype::Gray8>(2, 2, &[200; 4])
                .unwrap();
            enc.write_image::<colortype::Gray8>(3, 1, &[0; 3]).unwrap();
        }
        let img = decode_file(&path, ImageFormat::Tiff, &Options::default()).unwrap();
        assert!(!img.animated);
        assert_eq!(img.frames.len(), 2, "the odd-sized page is skipped");
        assert_eq!(&img.frame(1)[..4], [200, 200, 200, 255]);
    }

    #[test]
    fn float_images_are_tonemapped() {
        let path = dir().join("hdr.exr");
        let buf = image::Rgb32FImage::from_pixel(2, 1, image::Rgb([4.0, 0.5, 0.0]));
        DynamicImage::ImageRgb32F(buf).save(&path).unwrap();
        let img = decode_file(&path, ImageFormat::OpenExr, &Options::default()).unwrap();
        assert_eq!(img.format, "EXR");
        let px = img.pixels();
        assert!(px[0] > px[1] && px[0] < 255 && px[2] == 0, "{px:?}");
    }
}
