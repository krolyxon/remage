//! Text through glyphon, with the embedded Nothing fonts.

use std::collections::HashMap;

use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight, Wrap,
    cosmic_text::{Ellipsize, EllipsizeHeightLimit},
    fontdb,
};

use crate::config::theme::Rgba;

const FONTS: [&[u8]; 6] = [
    include_bytes!("../../assets/fonts/SpaceGrotesk-Light.otf"),
    include_bytes!("../../assets/fonts/SpaceGrotesk-Regular.otf"),
    include_bytes!("../../assets/fonts/SpaceGrotesk-Medium.otf"),
    include_bytes!("../../assets/fonts/SpaceMono-Regular.ttf"),
    include_bytes!("../../assets/fonts/SpaceMono-Bold.ttf"),
    include_bytes!("../../assets/fonts/Doto-Variable.ttf"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontRole {
    /// Space Grotesk.
    Ui,
    /// Space Mono, for labels and data.
    Mono,
    /// Doto, dot-matrix, 36 px and up.
    Display,
}

impl FontRole {
    fn index(self) -> usize {
        match self {
            FontRole::Ui => 0,
            FontRole::Mono => 1,
            FontRole::Display => 2,
        }
    }
}

/// The embedded families: UI, mono, display.
pub const DEFAULT_FAMILIES: [&str; 3] = ["Space Grotesk", "Space Mono", "Doto"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// A single line of text. `x` is the left edge, centre or right edge depending on `align`;
/// `y` is the vertical centre of the line. All units are physical pixels.
#[derive(Clone, Debug)]
pub struct TextItem {
    pub spans: Vec<(String, Rgba)>,
    pub role: FontRole,
    pub size: f32,
    pub weight: u16,
    /// Tracking in em.
    pub tracking: f32,
    pub x: f32,
    pub y: f32,
    pub align: Align,
    pub max_width: Option<f32>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    spans: Vec<(String, [u8; 4])>,
    role: FontRole,
    size: u32,
    weight: u16,
    tracking: u32,
    max_width: Option<u32>,
}

impl Key {
    fn of(t: &TextItem) -> Self {
        Self {
            spans: t.spans.iter().map(|(s, c)| (s.clone(), c.0)).collect(),
            role: t.role,
            size: t.size.to_bits(),
            weight: t.weight,
            tracking: t.tracking.to_bits(),
            max_width: t.max_width.map(|m| m.max(0.0).round() as u32),
        }
    }
}

struct Shaped {
    buffer: Buffer,
    width: f32,
    line_height: f32,
    used: bool,
}

pub struct TextSystem {
    fonts: FontSystem,
    swash: SwashCache,
    cache: Cache,
    atlas: TextAtlas,
    renderer: TextRenderer,
    viewport: Viewport,
    format: wgpu::TextureFormat,
    shaped: HashMap<Key, Shaped>,
    /// Family names per [`FontRole`], from `ui.font_*`.
    families: [String; 3],
}

/// Locale for shaping, from the usual environment variables.
fn locale() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.is_empty() && v != "C" && v != "POSIX")
        .and_then(|v| v.split('.').next().map(|s| s.replace('_', "-")))
        .unwrap_or_else(|| "en-US".into())
}

/// The embedded fonts only: fast to build, enough for the UI itself.
pub fn embedded_fonts() -> fontdb::Database {
    let mut db = fontdb::Database::new();
    for data in FONTS {
        db.load_font_data(data.to_vec());
    }
    db.set_sans_serif_family("Space Grotesk");
    db.set_monospace_family("Space Mono");
    db
}

/// Embedded plus system fonts, for filenames in any script. Slow; build it off-thread.
pub fn all_fonts() -> fontdb::Database {
    let mut db = embedded_fonts();
    db.load_system_fonts();
    db
}

impl TextSystem {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let fonts = FontSystem::new_with_locale_and_db(locale(), embedded_fonts());
        let cache = Cache::new(device);
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let renderer =
            TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);
        let viewport = Viewport::new(device, &cache);
        Self {
            fonts,
            swash: SwashCache::new(),
            cache,
            atlas,
            renderer,
            viewport,
            format,
            shaped: HashMap::new(),
            families: DEFAULT_FAMILIES.map(String::from),
        }
    }

    pub fn set_families(&mut self, families: [String; 3]) {
        if families != self.families {
            self.families = families;
            self.shaped.clear();
        }
    }

    /// Swap in a larger font database (glyph caches are keyed by font id, so start fresh).
    pub fn set_fonts(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, db: fontdb::Database) {
        self.fonts = FontSystem::new_with_locale_and_db(locale(), db);
        self.swash = SwashCache::new();
        self.atlas = TextAtlas::new(device, queue, &self.cache, self.format);
        self.renderer = TextRenderer::new(
            &mut self.atlas,
            device,
            wgpu::MultisampleState::default(),
            None,
        );
        self.shaped.clear();
    }

    fn shape(&mut self, item: &TextItem) -> Key {
        let key = Key::of(item);
        if let Some(s) = self.shaped.get_mut(&key) {
            s.used = true;
            return key;
        }
        let line_height = (item.size * 1.25).ceil();
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(item.size, line_height));
        buffer.set_size(None, None);
        let family = self.families[item.role.index()].clone();
        let base = Attrs::new()
            .family(Family::Name(&family))
            .weight(Weight(item.weight))
            .letter_spacing(item.tracking);
        let spans = item.spans.iter().map(|(s, c)| {
            let [r, g, b, a] = c.0;
            (s.as_str(), base.clone().color(Color::rgba(r, g, b, a)))
        });
        buffer.set_rich_text(spans, &base, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fonts, false);
        let mut width = buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max);
        if let Some(max) = key.max_width.map(|m| m as f32)
            && width > max
        {
            // Middle ellipsis keeps both the start and the extension of a filename.
            buffer.set_size(Some(max), None);
            buffer.set_wrap(Wrap::None);
            buffer.set_ellipsize(Ellipsize::Middle(EllipsizeHeightLimit::Lines(1)));
            buffer.shape_until_scroll(&mut self.fonts, false);
            width = buffer
                .layout_runs()
                .map(|r| r.line_w)
                .fold(0.0, f32::max)
                .min(max);
        }
        self.shaped.insert(
            key.clone(),
            Shaped {
                buffer,
                width,
                line_height,
                used: true,
            },
        );
        key
    }

    /// Width of `item` once laid out, in physical pixels.
    pub fn measure(&mut self, item: &TextItem) -> f32 {
        let key = self.shape(item);
        self.shaped.get(&key).map_or(0.0, |s| s.width)
    }

    /// Shape and upload this frame's text.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        items: &[TextItem],
        size: [u32; 2],
    ) -> anyhow::Result<()> {
        let keys: Vec<Key> = items.iter().map(|i| self.shape(i)).collect();
        // Keep what this frame (including any `measure` calls) used, and reset for the next.
        self.shaped.retain(|_, s| std::mem::take(&mut s.used));
        self.viewport.update(
            queue,
            Resolution {
                width: size[0],
                height: size[1],
            },
        );

        let areas = items.iter().zip(&keys).filter_map(|(item, key)| {
            let s = self.shaped.get(key)?;
            let width = s.width;
            let left = match item.align {
                Align::Left => item.x,
                Align::Center => item.x - width * 0.5,
                Align::Right => item.x - width,
            };
            let top = item.y - s.line_height * 0.5;
            let color = item.spans.first().map_or(Rgba([255; 4]), |(_, c)| *c).0;
            Some(TextArea {
                buffer: &s.buffer,
                left,
                top,
                scale: 1.0,
                bounds: TextBounds {
                    left: left.floor() as i32,
                    top: top.floor() as i32,
                    right: (left + width).ceil() as i32,
                    bottom: (top + s.line_height).ceil() as i32,
                },
                default_color: Color::rgba(color[0], color[1], color[2], color[3]),
                custom_glyphs: &[],
            })
        });
        self.renderer.prepare(
            device,
            queue,
            &mut self.fonts,
            &mut self.atlas,
            &self.viewport,
            areas,
            &mut self.swash,
        )?;
        Ok(())
    }

    pub fn render(&self, pass: &mut wgpu::RenderPass<'_>) -> anyhow::Result<()> {
        self.renderer.render(&self.atlas, &self.viewport, pass)?;
        Ok(())
    }

    pub fn trim(&mut self) {
        self.atlas.trim();
    }
}
