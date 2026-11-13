//! JPEG XL through `jxl-oxide`, rendered to sRGB (moxcms handles ICC-encoded files), with
//! animation.

use std::path::Path;
use std::time::Duration;

use anyhow::anyhow;
use jxl_oxide::{EnumColourEncoding, JxlImage, Moxcms, RenderingIntent};

use super::color;
use super::{DecodedImage, Frame, MAX_FRAMES_BYTES, Options};

pub fn decode(path: &Path, opts: &Options) -> anyhow::Result<DecodedImage> {
    let mut image = JxlImage::builder().open(path).map_err(|e| anyhow!("{e}"))?;
    if opts.color {
        image.set_cms(Moxcms);
        image.request_color_encoding(EnumColourEncoding::srgb(RenderingIntent::Relative));
    }
    // Seconds per tick of a frame's duration.
    let tick = image
        .image_header()
        .metadata
        .animation
        .as_ref()
        .filter(|a| a.tps_numerator > 0)
        .map(|a| a.tps_denominator as f64 / a.tps_numerator as f64);
    let count = image.num_loaded_keyframes();
    let mut frames = Vec::new();
    let mut size = [0, 0];
    let mut bytes = 0;
    for i in 0..count {
        let render = image.render_frame(i).map_err(|e| anyhow!("{e}"))?;
        let mut stream = render.stream();
        let channels = stream.channels() as usize;
        size = [stream.width(), stream.height()];
        let mut buf = vec![0u8; size[0] as usize * size[1] as usize * channels];
        stream.write_to_buffer(&mut buf);
        // Extra channels (CMYK black, spot colours) beyond RGBA are dropped.
        let pixels = if channels > 4 {
            buf.chunks_exact(channels)
                .flat_map(|p| [p[0], p[1], p[2], p[3]])
                .collect()
        } else {
            color::expand(&buf, channels)
        };
        let delay = tick.map_or(Duration::ZERO, |t| {
            Duration::from_secs_f64(render.duration() as f64 * t)
        });
        bytes += pixels.len();
        frames.push(Frame { pixels, delay });
        if bytes > MAX_FRAMES_BYTES {
            log::warn!("animation truncated at {} frames", frames.len());
            break;
        }
    }
    if frames.is_empty() {
        return Err(anyhow!("no frames"));
    }
    Ok(DecodedImage {
        size,
        animated: tick.is_some() && frames.len() > 1,
        frames,
        format: "JXL",
        source_size: size,
        orientation: image::metadata::Orientation::NoTransforms,
        refine: None,
    })
}
