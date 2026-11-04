//! SVG and SVGZ through resvg. The parsed tree is kept so the image can be re-rasterised at
//! the current zoom and stay sharp.

use std::path::Path;
use std::sync::{Arc, LazyLock};

use resvg::{tiny_skia, usvg};

use super::{DecodedImage, Options, Refine};

/// Longest side of any raster; zooming further shows this one magnified.
pub const MAX_RASTER: u32 = 8192;

/// System fonts for `<text>`, loaded once on first use.
static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
    let mut db = usvg::fontdb::Database::new();
    db.load_system_fonts();
    Arc::new(db)
});

pub struct Svg {
    tree: usvg::Tree,
    /// Intrinsic size, rounded up to whole pixels.
    size: [u32; 2],
}

pub fn decode(path: &Path, opts: &Options) -> anyhow::Result<DecodedImage> {
    let data = std::fs::read(path)?;
    let options = usvg::Options {
        resources_dir: path.parent().map(Path::to_path_buf),
        fontdb: FONTS.clone(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(&data, &options)?;
    let s = tree.size();
    let size = [
        (s.width().ceil() as u32).max(1),
        (s.height().ceil() as u32).max(1),
    ];
    let svg = Arc::new(Svg { tree, size });
    // Never more pixels than the window needs to start with; zooming in re-rasterises.
    let long = size[0].max(size[1]).min(opts.target.max(16));
    Ok(svg.rasterize(long))
}

impl Svg {
    /// Render with the longer side `long` pixels.
    pub fn rasterize(self: &Arc<Self>, long: u32) -> DecodedImage {
        let [w, h] = self.size;
        let k = long.clamp(1, MAX_RASTER) as f32 / w.max(h) as f32;
        let pw = ((w as f32 * k).round() as u32).max(1);
        let ph = ((h as f32 * k).round() as u32).max(1);
        let pixels = match tiny_skia::Pixmap::new(pw, ph) {
            Some(mut pixmap) => {
                let t =
                    tiny_skia::Transform::from_scale(pw as f32 / w as f32, ph as f32 / h as f32);
                resvg::render(&self.tree, t, &mut pixmap.as_mut());
                pixmap.take_demultiplied()
            }
            None => vec![0; pw as usize * ph as usize * 4],
        };
        let mut img = DecodedImage::still([pw, ph], pixels, "SVG");
        img.source_size = self.size;
        img.refine = Some(Refine::Svg(self.clone()));
        img
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rasterises_at_any_size() {
        let dir = std::env::temp_dir().join(format!("remage-svg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.svg");
        std::fs::write(
            &path,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20">
                 <rect width="20" height="20" fill="red"/></svg>"#,
        )
        .unwrap();
        let img = decode(&path, &Options::default()).unwrap();
        assert_eq!((img.size, img.source_size), ([40, 20], [40, 20]));
        assert_eq!(&img.pixels()[..4], [255, 0, 0, 255]);
        // Transparent right half.
        assert_eq!(img.pixels()[(30 * 4) + 3], 0);
        let Some(Refine::Svg(svg)) = &img.refine else {
            panic!("refinable")
        };
        let big = svg.rasterize(400);
        assert_eq!((big.size, big.source_size), ([400, 200], [40, 20]));
        let small = decode(
            &path,
            &Options {
                target: 10,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(small.size, [16, 8]);
    }
}
