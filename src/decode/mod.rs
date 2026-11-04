//! Format sniffing and dispatch to the decoder backends.
//!
//! Every backend returns straight-alpha RGBA8 in sRGB, upright: ICC/CICP colour is converted,
//! float images are tone-mapped and EXIF orientation is applied here, so nothing downstream
//! cares where pixels came from.

pub mod color;
mod external;
#[cfg(feature = "heif")]
mod heif;
mod image_rs;
#[cfg(feature = "jxl")]
mod jxl;
pub mod meta;
#[cfg(feature = "raw")]
mod raw;
#[cfg(feature = "svg")]
mod svg;

use std::fs::File;
use std::io::Read;
use std::path::Path;
#[cfg(feature = "svg")]
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::anyhow;
use image::ImageFormat;

pub use external::set_decoders;

/// Refuse single images that would need more than this to decode.
const MAX_ALLOC: u64 = 4 << 30;
/// Animations and multi-page files stop adding frames past this many bytes.
const MAX_FRAMES_BYTES: usize = 1 << 30;

/// One frame of an animation or one page of a document, `size` RGBA8 pixels.
pub struct Frame {
    pub pixels: Vec<u8>,
    /// How long an animation frame shows; zero for pages.
    pub delay: Duration,
}

/// Straight-alpha RGBA8 in sRGB, oriented upright, ready to upload.
pub struct DecodedImage {
    /// Pixel size of every frame.
    pub size: [u32; 2],
    /// At least one.
    pub frames: Vec<Frame>,
    /// Frames are timed animation rather than pages.
    pub animated: bool,
    pub format: &'static str,
    /// The size the view works in: the image's own pixels. Scalable or previewed sources
    /// (SVG, RAW) may hold fewer or more pixels than this.
    pub source_size: [u32; 2],
    /// A sharper version can be made on demand (SVG re-raster, full RAW develop).
    pub refine: Option<Refine>,
}

/// How to make a sharper version of an image.
#[derive(Clone)]
pub enum Refine {
    #[cfg(feature = "svg")]
    Svg(Arc<svg::Svg>),
    /// The embedded preview is showing; develop the raw data in full.
    #[cfg(feature = "raw")]
    Raw,
}

impl Refine {
    /// Make a version whose longer side is about `long` pixels.
    pub fn run(&self, path: &Path, long: u32, opts: &Options) -> anyhow::Result<DecodedImage> {
        let _ = (path, long, opts);
        match self {
            #[cfg(feature = "svg")]
            Refine::Svg(s) => Ok(s.rasterize(long)),
            #[cfg(feature = "raw")]
            Refine::Raw => raw::develop(path, opts),
            #[allow(unreachable_patterns)]
            _ => unreachable!(),
        }
    }

    /// Whether a texture of `have` pixels on its longer side should be remade for `want`.
    pub fn wants(&self, have: u32, want: u32) -> Option<u32> {
        let _ = (have, want);
        match self {
            #[cfg(feature = "svg")]
            Refine::Svg(_) => {
                let want = want.clamp(16, svg::MAX_RASTER);
                // Sharper when zoomed in; smaller again when zoomed far out.
                (want > have + have / 8 || want < have / 3).then_some(want)
            }
            #[cfg(feature = "raw")]
            Refine::Raw => (want > have).then_some(u32::MAX),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }
}

impl DecodedImage {
    pub fn still(size: [u32; 2], pixels: Vec<u8>, format: &'static str) -> Self {
        Self {
            size,
            frames: vec![Frame {
                pixels,
                delay: Duration::ZERO,
            }],
            animated: false,
            format,
            source_size: size,
            refine: None,
        }
    }

    pub fn bytes(&self) -> usize {
        self.frames.iter().map(|f| f.pixels.len()).sum()
    }

    /// The first frame's pixels.
    #[cfg(test)]
    pub fn pixels(&self) -> &[u8] {
        &self.frames[0].pixels
    }

    pub fn frame(&self, i: usize) -> &[u8] {
        &self.frames[i.min(self.frames.len() - 1)].pixels
    }

    /// Frame delays when there is more than one frame.
    pub fn delays(&self) -> Vec<Duration> {
        self.frames.iter().map(|f| f.delay).collect()
    }
}

impl std::fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedImage")
            .field("size", &self.size)
            .field("frames", &self.frames.len())
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

/// How to decode.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// About how many pixels the longer side needs, for sources that can be rendered at any
    /// size (SVG) or offer a preview (RAW).
    #[cfg_attr(not(any(feature = "svg", feature = "raw")), allow(dead_code))]
    pub target: u32,
    pub auto_orient: bool,
    pub color: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            target: 2048,
            auto_orient: true,
            color: true,
        }
    }
}

#[cfg(feature = "raw")]
const RAW_EXTENSIONS: &[&str] = &[
    "3fr", "ari", "arw", "cr2", "cr3", "crw", "dcr", "dcs", "dng", "erf", "iiq", "kdc", "mef",
    "mos", "mrw", "nef", "nrw", "orf", "ori", "pef", "raf", "raw", "rw2", "rwl", "sr2", "srf",
    "srw", "x3f",
];
#[cfg(not(feature = "raw"))]
const RAW_EXTENSIONS: &[&str] = &[];

static EXTENSIONS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let mut v: Vec<&'static str> = ImageFormat::all()
        .filter(ImageFormat::reading_enabled)
        .flat_map(|f| f.extensions_str().iter().copied())
        .chain(["apng", "jfif"])
        .collect();
    if cfg!(feature = "avif") {
        v.push("avif");
    }
    if cfg!(feature = "jxl") {
        v.push("jxl");
    }
    if cfg!(feature = "heif") {
        v.extend(["heic", "heif", "hif"]);
    }
    if cfg!(feature = "svg") {
        v.extend(["svg", "svgz"]);
    }
    v.extend(RAW_EXTENSIONS);
    v
});

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// Whether the extension alone (or an external decoder) says we can decode this file.
pub fn is_known_extension(path: &Path) -> bool {
    extension(path).is_some_and(|e| EXTENSIONS.contains(&e.as_str()))
        || external::find(path).is_some()
}

/// Which backend decodes a file.
#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Image(ImageFormat),
    Jxl,
    Heif,
    Svg,
    Raw,
    External(crate::config::Decoder),
}

/// The ISO-BMFF `ftyp` brands, major first.
fn ftyp_brands(head: &[u8]) -> Vec<&[u8]> {
    if head.len() < 16 || &head[4..8] != b"ftyp" {
        return Vec::new();
    }
    let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
    let end = len.clamp(16, head.len());
    let mut brands = vec![&head[8..12]];
    brands.extend(head[16..end].chunks_exact(4));
    brands
}

fn looks_like_svg(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(head);
    let t = text.trim_start_matches('\u{feff}').trim_start();
    (t.starts_with("<?xml") || t.starts_with("<svg") || t.starts_with("<!--"))
        && text.contains("<svg")
}

/// Magic bytes first, the extension second. External decoders configured for a file win.
fn detect(path: &Path, head: &[u8]) -> Option<Kind> {
    if let Some(d) = external::find(path) {
        return Some(Kind::External(d));
    }
    let ext = extension(path).unwrap_or_default();
    let jxl = head.starts_with(&[0xFF, 0x0A]) || head.starts_with(b"\0\0\0\x0cJXL \r\n\x87\n");
    if jxl && cfg!(feature = "jxl") {
        return Some(Kind::Jxl);
    }
    let brands = ftyp_brands(head);
    if !brands.is_empty() {
        let has = |b: &[u8]| brands.contains(&b);
        if has(b"crx ") && cfg!(feature = "raw") {
            return Some(Kind::Raw);
        }
        if has(b"avif") || has(b"avis") {
            return Some(Kind::Image(ImageFormat::Avif));
        }
        let heif = [
            b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx", b"mif1", b"msf1",
        ];
        if heif.iter().any(|b| has(*b)) && cfg!(feature = "heif") {
            return Some(Kind::Heif);
        }
    }
    // Most raw formats are TIFF inside, so the extension decides before the magic does.
    if RAW_EXTENSIONS.contains(&ext.as_str()) {
        return Some(Kind::Raw);
    }
    if cfg!(feature = "svg")
        && (looks_like_svg(head) || (ext == "svgz" && head.starts_with(&[0x1F, 0x8B])))
    {
        return Some(Kind::Svg);
    }
    if let Ok(f) = image::guess_format(head)
        && f.reading_enabled()
    {
        return Some(Kind::Image(f));
    }
    match ext.as_str() {
        "jxl" if cfg!(feature = "jxl") => Some(Kind::Jxl),
        "heic" | "heif" | "hif" if cfg!(feature = "heif") => Some(Kind::Heif),
        "svg" | "svgz" if cfg!(feature = "svg") => Some(Kind::Svg),
        _ => ImageFormat::from_extension(&ext)
            .filter(ImageFormat::reading_enabled)
            .map(Kind::Image),
    }
}

fn head(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(1024);
    File::open(path)?.take(1024).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Whether a file with an unknown extension is an image we can decode.
pub fn sniff(path: &Path) -> bool {
    let Ok(head) = head(path) else { return false };
    // An extension guess alone is not proof; require the content to say so.
    match detect(path, &head) {
        Some(Kind::Image(_)) => image::guess_format(&head).is_ok(),
        Some(Kind::Raw) => false,
        Some(_) => true,
        None => false,
    }
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

/// Decode `path` in full: every frame, colour-converted and upright.
pub fn decode(path: &Path, opts: &Options) -> anyhow::Result<DecodedImage> {
    let head = head(path)?;
    let kind = detect(path, &head).ok_or_else(|| anyhow!("unknown format"))?;
    match kind {
        Kind::Image(f) => image_rs::decode_file(path, f, opts),
        #[cfg(feature = "jxl")]
        Kind::Jxl => jxl::decode(path, opts),
        #[cfg(feature = "heif")]
        Kind::Heif => heif::decode(path, opts),
        #[cfg(feature = "svg")]
        Kind::Svg => svg::decode(path, opts),
        #[cfg(feature = "raw")]
        Kind::Raw => raw::decode(path, opts),
        Kind::External(d) => external::decode(path, &d, opts),
        #[allow(unreachable_patterns)]
        _ => Err(anyhow!("support for this format is not compiled in")),
    }
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
        #[cfg(feature = "jxl")]
        assert!(is_known_extension(Path::new("a.jxl")));
        #[cfg(feature = "raw")]
        assert!(is_known_extension(Path::new("a.NEF")));
    }

    #[test]
    fn detection_prefers_magic() {
        let p = Path::new("x.png");
        assert_eq!(
            detect(p, b"\xFF\xD8\xFF\xE0"),
            Some(Kind::Image(ImageFormat::Jpeg))
        );
        let avif = b"\0\0\0\x1cftypavif\0\0\0\0avifmif1miaf";
        assert_eq!(detect(p, avif), Some(Kind::Image(ImageFormat::Avif)));
        #[cfg(feature = "heif")]
        assert_eq!(
            detect(p, b"\0\0\0\x18ftypheic\0\0\0\0mif1heic"),
            Some(Kind::Heif)
        );
        #[cfg(feature = "jxl")]
        assert_eq!(detect(p, b"\xFF\x0A\x00"), Some(Kind::Jxl));
        #[cfg(feature = "svg")]
        assert_eq!(
            detect(p, b"<?xml version=\"1.0\"?>\n<svg xmlns=\"x\">"),
            Some(Kind::Svg)
        );
        #[cfg(feature = "raw")]
        assert_eq!(detect(Path::new("a.nef"), b"MM\0*"), Some(Kind::Raw));
        assert_eq!(
            detect(Path::new("a.tif"), b"MM\0*"),
            Some(Kind::Image(ImageFormat::Tiff))
        );
        assert_eq!(detect(Path::new("a.txt"), b"hello"), None);
    }

    #[test]
    fn decodes_png_without_extension() {
        let dir = std::env::temp_dir().join(format!("remage-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("noext");
        image::RgbaImage::from_pixel(64, 32, image::Rgba([255, 0, 0, 128]))
            .save_with_format(&path, ImageFormat::Png)
            .unwrap();
        assert!(sniff(&path));
        let img = decode(&path, &Options::default()).unwrap();
        assert_eq!(img.format, "PNG");
        assert_eq!(img.source_size, [64, 32]);
        assert_eq!(img.size, [64, 32]);
        assert_eq!(img.pixels().len(), 64 * 32 * 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Decoder tests against `tests/fixtures` (see `make.sh` there).
    mod fixtures {
        use super::*;

        fn fixture(name: &str) -> std::path::PathBuf {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name)
        }

        fn load(name: &str) -> DecodedImage {
            decode(&fixture(name), &Options::default()).unwrap_or_else(|e| panic!("{name}: {e:#}"))
        }

        fn close(a: &[u8], b: [u8; 3], name: &str, what: &str) {
            let ok = a[..3].iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 40);
            assert!(ok, "{name}: {what} is {:?}, expected {b:?}", &a[..4]);
        }

        /// Red, lime, blue and white quadrants, upright.
        fn check_quadrants(img: &DecodedImage, frame: usize, name: &str) {
            assert_eq!(img.size, [16, 12], "{name}");
            let px = img.frame(frame);
            let at = |x: usize, y: usize| &px[(y * 16 + x) * 4..][..4];
            close(at(3, 3), [255, 0, 0], name, "top left");
            close(at(12, 3), [0, 255, 0], name, "top right");
            close(at(3, 9), [0, 0, 255], name, "bottom left");
            close(at(12, 9), [255, 255, 255], name, "bottom right");
        }

        #[test]
        fn every_format_decodes_upright() {
            let mut names = vec![
                "quadrants.png",
                "quadrants.jpg",
                "quadrants.bmp",
                "quadrants.tga",
                "quadrants.qoi",
                "quadrants.ppm",
                "quadrants.webp",
                "quadrants.exr",
                "quadrants.avif",
            ];
            if cfg!(feature = "jxl") {
                names.push("quadrants.jxl");
            }
            if cfg!(feature = "heif") {
                names.push("quadrants.heic");
            }
            if cfg!(feature = "svg") {
                names.extend(["drawing.svg", "drawing.svgz"]);
            }
            for name in names {
                assert!(is_known_extension(&fixture(name)), "{name}");
                check_quadrants(&load(name), 0, name);
            }
        }

        #[test]
        fn exif_orientations() {
            for i in 1..=8 {
                let name = format!("orient-{i}.jpg");
                check_quadrants(&load(&name), 0, &name);
            }
            // With auto_orient off, a rotated file stays as stored (12×16).
            let opts = Options {
                auto_orient: false,
                ..Options::default()
            };
            let raw = decode(&fixture("orient-6.jpg"), &opts).unwrap();
            assert_eq!(raw.size, [12, 16]);
        }

        #[test]
        fn animations() {
            let mut names = vec!["anim.gif", "anim.webp", "anim.png"];
            if cfg!(feature = "jxl") {
                names.push("anim.jxl");
            }
            for name in names {
                let img = load(name);
                assert!(img.animated, "{name}");
                assert_eq!(img.frames.len(), 3, "{name}");
                let ms: Vec<u128> = img.delays().iter().map(Duration::as_millis).collect();
                // ImageMagick writes APNG at a constant frame rate.
                let want = if name == "anim.png" {
                    [50, 50, 50]
                } else {
                    [50, 100, 150]
                };
                assert_eq!(ms, want, "{name}");
                for (f, c) in [[255, 0, 0], [0, 255, 0], [0, 0, 255]]
                    .into_iter()
                    .enumerate()
                {
                    close(img.frame(f), c, name, &format!("frame {f}"));
                }
            }
        }

        #[test]
        fn pages() {
            let img = load("pages.tif");
            assert!(!img.animated);
            assert_eq!(img.frames.len(), 3);
            close(img.frame(2), [0, 0, 255], "pages.tif", "page 3");
        }

        #[test]
        fn icc_profiles_convert_to_srgb() {
            use image::ImageEncoder;
            let icc = moxcms::ColorProfile::new_display_p3().encode().unwrap();
            let dir = std::env::temp_dir().join(format!("remage-icc-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("p3.png");
            let px = [40u8, 160, 90, 255].repeat(4);
            {
                let mut enc =
                    image::codecs::png::PngEncoder::new(std::fs::File::create(&path).unwrap());
                enc.set_icc_profile(icc).unwrap();
                enc.write_image(&px, 2, 2, image::ExtendedColorType::Rgba8)
                    .unwrap();
            }
            let raw = decode(
                &path,
                &Options {
                    color: false,
                    ..Options::default()
                },
            )
            .unwrap();
            assert_eq!(&raw.frame(0)[..4], [40, 160, 90, 255]);
            let srgb = decode(&path, &Options::default()).unwrap();
            let p = &srgb.frame(0)[..4];
            // P3 green is more saturated than sRGB can say: red drops, green rises.
            assert!(p[0] < 40 && p[1] > 160 && p[3] == 255, "{p:?}");
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}
