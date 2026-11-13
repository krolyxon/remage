//! freedesktop thumbnail cache: lookup, validation, generation and atomic writes.
//!
//! Entries live in `$XDG_CACHE_HOME/thumbnails/<bucket>/<md5(uri)>.png`, validated by their
//! `Thumb::URI` and `Thumb::MTime` text chunks. The cache is shared with file managers, so
//! the URI must be spelled exactly as GLib spells it.

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::RwLock;
use std::time::UNIX_EPOCH;

use anyhow::{Context, anyhow, bail};
use fast_image_resize as fir;
use image::metadata::Orientation;
use md5::{Digest, Md5};

use crate::decode::{self, DecodedImage};

/// The freedesktop size buckets, smallest first.
pub const BUCKETS: [(&str, u32); 4] = [
    ("normal", 128),
    ("large", 256),
    ("x-large", 512),
    ("xx-large", 1024),
];

/// The folder failed entries go in, under `fail/`.
const APP: &str = "remage";

/// A thumbnail ready for the atlas: straight-alpha RGBA8, at most `px` on its longer side.
pub struct Thumb {
    pub size: [u32; 2],
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for Thumb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Thumb").field("size", &self.size).finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ThumbError {
    /// A `fail/remage` entry says this file can't be thumbnailed; don't retry.
    #[error("thumbnailing failed before")]
    KnownFailure,
    #[error("{0:#}")]
    Failed(anyhow::Error),
}

/// How a job uses the shared cache.
#[derive(Clone, Debug)]
pub struct CacheOptions {
    /// `$XDG_CACHE_HOME/thumbnails`; `None` disables the cache entirely.
    pub root: Option<PathBuf>,
    /// Read but never write (`-p`).
    pub private: bool,
}

impl CacheOptions {
    pub fn new(enabled: bool, private: bool) -> Self {
        Self {
            root: enabled
                .then(|| dirs::cache_dir().map(|d| d.join("thumbnails")))
                .flatten(),
            private,
        }
    }
}

/// The smallest bucket at least `px` wide, else the largest.
pub fn bucket_for(px: u32) -> (&'static str, u32) {
    BUCKETS
        .iter()
        .copied()
        .find(|(_, s)| *s >= px)
        .unwrap_or(BUCKETS[BUCKETS.len() - 1])
}

/// Make `path` absolute and drop `.` and `..` without resolving symlinks, as GLib does.
pub fn normalize(path: &Path) -> PathBuf {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// `file://` URI of an absolute path, escaped like GLib's `g_filename_to_uri`.
pub fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        let keep = b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&b);
        if keep {
            uri.push(b as char);
        } else {
            uri.push('%');
            uri.push(HEX[(b >> 4) as usize] as char);
            uri.push(HEX[(b & 15) as usize] as char);
        }
    }
    uri
}

/// The cache file name for a URI: its MD5 in lowercase hex, plus `.png`.
pub fn cache_name(uri: &str) -> String {
    let digest = Md5::digest(uri.as_bytes());
    let mut s = String::with_capacity(36);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s.push_str(".png");
    s
}

/// Seconds since the epoch, as `Thumb::MTime` stores it.
fn mtime_secs(meta: &fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// PNG text chunks as keyword–text pairs.
type Text = Vec<(String, String)>;

/// Text chunks of a PNG, and its pixels as RGBA8 if `pixels` is set.
fn read_png(path: &Path, pixels: bool) -> anyhow::Result<(Text, Option<Thumb>)> {
    let file = File::open(path)?;
    let mut dec = png::Decoder::new(BufReader::new(file));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info()?;
    let mut thumb = None;
    if pixels {
        let mut buf = vec![0; reader.output_buffer_size().context("thumbnail too large")?];
        let out = reader.next_frame(&mut buf)?;
        buf.truncate(out.buffer_size());
        let rgba = to_rgba(buf, out.color_type).context("unsupported thumbnail colour type")?;
        thumb = Some(Thumb {
            size: [out.width, out.height],
            pixels: rgba,
        });
        // Text chunks after the image data.
        let _ = reader.finish();
    }
    let info = reader.info();
    let mut text: Text = info
        .uncompressed_latin1_text
        .iter()
        .map(|c| (c.keyword.clone(), c.text.clone()))
        .collect();
    text.extend(
        info.utf8_text
            .iter()
            .filter_map(|c| Some((c.keyword.clone(), c.get_text().ok()?))),
    );
    text.extend(
        info.compressed_latin1_text
            .iter()
            .filter_map(|c| Some((c.keyword.clone(), c.get_text().ok()?))),
    );
    Ok((text, thumb))
}

fn to_rgba(buf: Vec<u8>, color: png::ColorType) -> Option<Vec<u8>> {
    use png::ColorType::*;
    Some(match color {
        Rgba => buf,
        Rgb => buf
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        Indexed => return None,
    })
}

/// Whether the text chunks describe `uri` at `mtime`. Some writers (ffmpegthumbnailer) store
/// a bare path as the URI; that counts too.
fn is_valid(text: &[(String, String)], uri: &str, mtime: u64) -> bool {
    let get = |k: &str| text.iter().find(|(key, _)| key == k).map(|(_, v)| v.trim());
    // Some writers store fractional seconds.
    let mtime_ok = get("Thumb::MTime")
        .and_then(|m| m.parse::<f64>().ok())
        .is_some_and(|m| m.trunc() as u64 == mtime);
    let path = uri.strip_prefix("file://").map(|p| {
        let bytes = percent_decode(p);
        String::from_utf8_lossy(&bytes).into_owned()
    });
    let uri_ok = get("Thumb::URI").is_some_and(|u| u == uri || Some(u) == path.as_deref());
    uri_ok && mtime_ok
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).and_then(|&c| hex(c)),
                b.get(i + 2).and_then(|&c| hex(c)),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// Set once the process is on its way out.
static WRITES_CLOSED: RwLock<bool> = RwLock::new(false);

/// Wait for thumbnails being written and refuse new ones. Workers aren't joined on exit, so
/// this is what keeps temporary files from being left in the shared cache.
pub fn finish_writes() {
    *WRITES_CLOSED.write().unwrap_or_else(|e| e.into_inner()) = true;
}

/// Write a PNG atomically (temporary file, then rename) with mode 0600.
fn write_png(path: &Path, thumb: &Thumb, text: &[(&str, String)]) -> anyhow::Result<()> {
    let dir = path.parent().context("cache path has no parent")?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let name = path.file_name().context("cache path has no name")?;
    let tmp = dir.join(format!(
        ".{}.remage-{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    // Held to the end, so quitting never kills a write half-way (see `finish_writes`).
    let closed = WRITES_CLOSED.read().unwrap_or_else(|e| e.into_inner());
    if *closed {
        bail!("shutting down");
    }
    let result = (|| -> anyhow::Result<()> {
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        let mut w = BufWriter::new(file);
        let mut enc = png::Encoder::new(&mut w, thumb.size[0], thumb.size[1]);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        for (k, v) in text {
            enc.add_text_chunk(k.to_string(), v.clone())?;
        }
        let mut writer = enc.write_header()?;
        writer.write_image_data(&thumb.pixels)?;
        writer.finish()?;
        w.flush()?;
        drop(w);
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Shrink `src` so its longer side is at most `max` (never enlarging).
pub fn resize(src: Thumb, max: u32) -> anyhow::Result<Thumb> {
    let [w, h] = src.size;
    if w <= max && h <= max {
        return Ok(src);
    }
    let k = max as f64 / w.max(h) as f64;
    let nw = ((w as f64 * k).round() as u32).clamp(1, max);
    let nh = ((h as f64 * k).round() as u32).clamp(1, max);
    let mut img = fir::images::Image::from_vec_u8(w, h, src.pixels, fir::PixelType::U8x4)
        .context("resize source")?;
    // Resample in premultiplied alpha so transparent edges don't bleed dark fringes.
    let mul = fir::MulDiv::default();
    mul.multiply_alpha_inplace(&mut img)
        .context("premultiply")?;
    let mut dst = fir::images::Image::new(nw, nh, fir::PixelType::U8x4);
    let opts = fir::ResizeOptions::new()
        .resize_alg(fir::ResizeAlg::Convolution(fir::FilterType::Lanczos3))
        .use_alpha(false);
    fir::Resizer::new()
        .resize(&img, &mut dst, &opts)
        .context("resize")?;
    mul.divide_alpha_inplace(&mut dst)
        .context("unpremultiply")?;
    Ok(Thumb {
        size: [nw, nh],
        pixels: dst.into_vec(),
    })
}

/// Load a thumbnail of `path` at most `px` on its longer side: from the shared cache when a
/// valid entry is big enough, otherwise generated (and written back unless private).
pub fn load(path: &Path, px: u32, cache: &CacheOptions) -> Result<Thumb, ThumbError> {
    let fail = |e: anyhow::Error| ThumbError::Failed(e);
    let meta = fs::metadata(path).map_err(|e| fail(e.into()))?;
    let mtime = mtime_secs(&meta).unwrap_or(0);
    let abs = normalize(path);
    let uri = file_uri(&abs);
    let name = cache_name(&uri);
    let (bucket, bucket_px) = bucket_for(px);

    if let Some(root) = &cache.root {
        // Our bucket, or any larger one.
        for (dir, _) in BUCKETS.iter().filter(|(_, s)| *s >= bucket_px) {
            let p = root.join(dir).join(&name);
            if let Ok((text, Some(t))) = read_png(&p, true)
                && is_valid(&text, &uri, mtime)
            {
                return resize(t, px).map_err(fail);
            }
        }
        // Only trust failures this version recorded: a later one may decode more formats.
        let failed = root.join("fail").join(APP).join(&name);
        if let Ok((text, _)) = read_png(&failed, false)
            && is_valid(&text, &uri, mtime)
            && text
                .iter()
                .any(|(k, v)| k == "Software" && *v == software())
        {
            return Err(ThumbError::KnownFailure);
        }
    }

    let generated = match generate(path, bucket_px) {
        Ok(t) => t,
        Err(e) => {
            if let Some(root) = cache.root.as_ref().filter(|_| !cache.private) {
                let failed = root.join("fail").join(APP).join(&name);
                let empty = Thumb {
                    size: [1, 1],
                    pixels: vec![0; 4],
                };
                let text = [
                    ("Thumb::URI", uri.clone()),
                    ("Thumb::MTime", mtime.to_string()),
                    ("Software", software()),
                ];
                if let Err(e) = write_png(&failed, &empty, &text) {
                    log::debug!("{}: {e:#}", failed.display());
                }
            }
            return Err(fail(e));
        }
    };

    // Images no bigger than the bucket gain nothing from a cache entry.
    let worth_caching = generated.source_size.iter().any(|&s| s > bucket_px);
    let thumb = Thumb {
        size: generated.size,
        pixels: generated.pixels,
    };
    if let Some(root) = cache.root.as_ref().filter(|_| !cache.private)
        && worth_caching
        && !abs.starts_with(root)
    {
        let dest = root.join(bucket).join(&name);
        let [sw, sh] = generated.source_size;
        let text = [
            ("Thumb::URI", uri.clone()),
            ("Thumb::MTime", mtime.to_string()),
            ("Thumb::Size", meta.len().to_string()),
            ("Thumb::Image::Width", sw.to_string()),
            ("Thumb::Image::Height", sh.to_string()),
            ("Software", software()),
        ];
        if let Err(e) = write_png(&dest, &thumb, &text) {
            log::debug!("{}: {e:#}", dest.display());
        }
    }
    resize(thumb, px).map_err(fail)
}

fn software() -> String {
    format!("remage {}", env!("CARGO_PKG_VERSION"))
}

/// A freshly made thumbnail at most `max` on its longer side.
struct Generated {
    size: [u32; 2],
    pixels: Vec<u8>,
    /// The original's upright size.
    source_size: [u32; 2],
}

fn generate(path: &Path, max: u32) -> anyhow::Result<Generated> {
    // JPEG: decode at 1/2, 1/4 or 1/8 straight from the DCT.
    if let Some(g) = jpeg_scaled(path, max)? {
        return Ok(g);
    }
    // Scalable and previewed sources (SVG, RAW) only need the bucket's size.
    let opts = decode::Options {
        target: max,
        ..decode::Options::default()
    };
    let img = decode::decode(path, &opts)?;
    let source_size = img.source_size;
    let DecodedImage {
        size,
        mut frames,
        orientation,
        ..
    } = img;
    let pixels = frames.swap_remove(0).pixels;
    let t = resize(Thumb { size, pixels }, max)?;
    // Turning the thumbnail is much cheaper than turning the image.
    let (size, pixels) = decode::color::orient(t.size, t.pixels, orientation);
    Ok(Generated {
        size,
        pixels,
        source_size,
    })
}

/// DCT-scaled JPEG decode, or `None` when the file isn't a plain 8-bit RGB/grey JPEG.
fn jpeg_scaled(path: &Path, max: u32) -> anyhow::Result<Option<Generated>> {
    let mut magic = [0u8; 3];
    {
        use std::io::Read;
        let mut f = File::open(path)?;
        if f.read_exact(&mut magic).is_err() || magic != [0xFF, 0xD8, 0xFF] {
            return Ok(None);
        }
    }
    let mut dec = jpeg_decoder::Decoder::new(BufReader::new(File::open(path)?));
    dec.read_info()?;
    let info = dec.info().ok_or_else(|| anyhow!("no JPEG header"))?;
    use jpeg_decoder::PixelFormat::*;
    if !matches!(info.pixel_format, L8 | RGB24) {
        return Ok(None);
    }
    let (w, h) = (info.width as u32, info.height as u32);
    let k = (max as f64 / w.max(h) as f64).min(1.0);
    let req = |v: u32| ((v as f64 * k).ceil() as u32).clamp(1, u16::MAX as u32) as u16;
    let (sw, sh) = dec.scale(req(w), req(h))?;
    let data = dec.decode()?;
    let pixels: Vec<u8> = match info.pixel_format {
        L8 => data.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        _ => data
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
    };
    let (sw, sh) = (sw as u32, sh as u32);
    if pixels.len() != (sw * sh * 4) as usize {
        bail!("JPEG decoded to an unexpected size");
    }
    let orientation = dec
        .exif_data()
        .and_then(Orientation::from_exif_chunk)
        .unwrap_or(Orientation::NoTransforms);
    let mut img = image::DynamicImage::ImageRgba8(
        image::RgbaImage::from_raw(sw, sh, pixels).context("JPEG buffer size")?,
    );
    img.apply_orientation(orientation);
    let source_size = if img.width() == sw { [w, h] } else { [h, w] };
    let rgba = img.into_rgba8();
    let size = [rgba.width(), rgba.height()];
    let t = resize(
        Thumb {
            size,
            pixels: rgba.into_raw(),
        },
        max,
    )?;
    Ok(Some(Generated {
        size: t.size,
        pixels: t.pixels,
        source_size,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("remage-thumbs-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn uris_are_escaped_like_glib() {
        assert_eq!(
            file_uri(Path::new("/home/u/a b/ü#?%.png")),
            "file:///home/u/a%20b/%C3%BC%23%3F%25.png"
        );
        assert_eq!(
            file_uri(Path::new("/x/(1)!$&'*+,;=:@~-_.png")),
            "file:///x/(1)!$&'*+,;=:@~-_.png"
        );
    }

    #[test]
    fn names_are_md5_of_the_uri() {
        // The spec's own example.
        assert_eq!(
            cache_name("file:///home/jens/photos/me.png"),
            "c6ee772d9e49320e97ec29a7eb5b1697.png"
        );
    }

    #[test]
    fn normalize_is_lexical() {
        assert_eq!(
            normalize(Path::new("/a/./b/../c.png")),
            Path::new("/a/c.png")
        );
        assert!(normalize(Path::new("x.png")).is_absolute());
    }

    #[test]
    fn buckets() {
        assert_eq!(bucket_for(96), ("normal", 128));
        assert_eq!(bucket_for(128), ("normal", 128));
        assert_eq!(bucket_for(160), ("large", 256));
        assert_eq!(bucket_for(320), ("x-large", 512));
        assert_eq!(bucket_for(2000), ("xx-large", 1024));
    }

    #[test]
    fn generates_writes_and_rereads() {
        let dir = tmp("cache");
        let img = dir.join("pic.png");
        image::RgbaImage::from_fn(600, 300, |x, _| image::Rgba([(x % 256) as u8, 0, 0, 255]))
            .save(&img)
            .unwrap();
        let cache = CacheOptions {
            root: Some(dir.join("thumbnails")),
            private: false,
        };
        let t = load(&img, 160, &cache).unwrap();
        assert_eq!(t.size, [160, 80]);
        let entry = dir
            .join("thumbnails/large")
            .join(cache_name(&file_uri(&normalize(&img))));
        let (text, stored) = read_png(&entry, true).unwrap();
        assert_eq!(stored.unwrap().size, [256, 128]);
        assert!(
            text.iter()
                .any(|(k, v)| k == "Thumb::Image::Width" && v == "600")
        );
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&entry).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // A second load reads the entry (same result), a smaller size downscales it.
        assert_eq!(load(&img, 160, &cache).unwrap().size, [160, 80]);
        assert_eq!(load(&img, 96, &cache).unwrap().size, [96, 48]);
        // A stale entry (different mtime) is ignored and replaced.
        let text = vec![
            ("Thumb::URI".to_string(), file_uri(&normalize(&img))),
            ("Thumb::MTime".to_string(), "1".to_string()),
        ];
        let meta = fs::metadata(&img).unwrap();
        assert!(!is_valid(&text, &text[0].1, mtime_secs(&meta).unwrap()));
        assert!(is_valid(&text, &text[0].1, 1));
        // A bare path as the URI is accepted too.
        let bare = vec![
            ("Thumb::URI".to_string(), "/a b/c.png".to_string()),
            ("Thumb::MTime".to_string(), "1".to_string()),
        ];
        assert!(is_valid(&bare, "file:///a%20b/c.png", 1));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn failures_are_remembered_and_private_writes_nothing() {
        let dir = tmp("fail");
        let bad = dir.join("bad.png");
        fs::write(&bad, b"not a png").unwrap();
        let cache = CacheOptions {
            root: Some(dir.join("thumbnails")),
            private: true,
        };
        assert!(matches!(
            load(&bad, 128, &cache),
            Err(ThumbError::Failed(_))
        ));
        assert!(!dir.join("thumbnails").exists());
        let cache = CacheOptions {
            private: false,
            ..cache
        };
        assert!(matches!(
            load(&bad, 128, &cache),
            Err(ThumbError::Failed(_))
        ));
        assert!(matches!(
            load(&bad, 128, &cache),
            Err(ThumbError::KnownFailure)
        ));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn jpeg_is_scaled_and_oriented() {
        let dir = tmp("jpeg");
        let p = dir.join("a.jpg");
        image::RgbImage::from_pixel(800, 400, image::Rgb([10, 200, 30]))
            .save(&p)
            .unwrap();
        let g = jpeg_scaled(&p, 128).unwrap().unwrap();
        assert_eq!(g.source_size, [800, 400]);
        assert_eq!(g.size, [128, 64]);
        let px = &g.pixels[..4];
        assert!(px[1] > 150 && px[0] < 60, "{px:?}");
        fs::remove_dir_all(&dir).unwrap();
    }
}
