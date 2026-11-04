//! UI surfaces: status bar, grid view, palette, which-key, help and info panel.
//!
//! Each frame the UI turns state into a [`Scene`]; the renderer only draws it.

use std::path::Path;
use std::time::Instant;

use crate::config::theme::{Rgba, Theme};
use crate::render::atlas::ThumbDraw;
use crate::render::image::AlphaStyle;
use crate::render::prims::Prim;
use crate::render::text::{Align, FontRole, TextItem};
use crate::render::{ImageDraw, Scene};
use crate::state::{AlphaBg, AppState, BAR_HEIGHT, GRID_NAME_SIZE, Mode, MsgKind, ThumbState};

/// Space Mono label tracking.
const LABEL_TRACKING: f32 = 0.08;

pub fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into(),
    )
}

fn label(text: &str, color: Rgba, sf: f32, x: f32, y: f32, align: Align) -> TextItem {
    TextItem {
        spans: vec![(text.to_uppercase(), color)],
        role: FontRole::Mono,
        size: (11.0 * sf).round(),
        weight: 400,
        tracking: LABEL_TRACKING,
        x,
        y,
        align,
        max_width: None,
        clip: None,
    }
}

/// Longest message shown before truncation, in characters.
const MAX_MESSAGE: usize = 72;

/// `measure` returns the laid-out width of a text item, for layouts that depend on it.
pub fn scene<'a>(
    state: &'a AppState,
    theme: &Theme,
    now: Instant,
    measure: &mut dyn FnMut(&TextItem) -> f32,
) -> Scene<'a> {
    let sf = state.ui_scale();
    let [w, h] = state.window();
    let bar_h = state.bar_height();
    let mut scene = Scene {
        clear: theme.background,
        image_clip: [0.0, 0.0, w, h - bar_h],
        alpha: alpha_style(state.alpha_bg, theme, sf),
        images: Vec::new(),
        thumbs: Vec::new(),
        under: Vec::new(),
        prims: Vec::new(),
        texts: Vec::new(),
    };

    if state.in_grid() {
        grid(&mut scene, state, theme, now);
    } else {
        image(&mut scene, state, now);
    }
    if state.queue.is_empty() {
        empty_state(&mut scene, theme, sf, [w, h - bar_h]);
    }
    if bar_h > 0.0 {
        status_bar(&mut scene, state, theme, now, [w, h], bar_h, measure);
    } else if state.prompt.is_some() {
        // A question can't hide with the bar: it overlays the image until answered.
        let bar_h = (BAR_HEIGHT * sf).round();
        status_bar(&mut scene, state, theme, now, [w, h], bar_h, measure);
    }
    scene
}

/// The current image, and the previous one fading out under it.
fn image<'a>(scene: &mut Scene<'a>, state: &'a AppState, now: Instant) {
    let nearest = state.effective_nearest(now);
    if let Some(fade) = &state.fade {
        scene.images.push(ImageDraw {
            path: &fade.path,
            affine: fade.affine,
            alpha: 1.0,
            nearest,
        });
    }
    if let Some(shown) = &state.shown {
        let alpha = state.fade.as_ref().map_or(1.0, |f| f.alpha.value(now));
        let affine = state.view.transform(now);
        scene.images.push(ImageDraw {
            path: &shown.path,
            affine,
            alpha,
            nearest,
        });
    }
}

/// `r` grown by `d` on every side.
fn expand(r: [f32; 4], d: f32) -> [f32; 4] {
    [r[0] - d, r[1] - d, r[2] + 2.0 * d, r[3] + 2.0 * d]
}

/// Thumbnails, the cursor, marks and the visual range.
fn grid<'a>(scene: &mut Scene<'a>, state: &'a AppState, theme: &Theme, now: Instant) {
    let sf = state.ui_scale();
    let l = state.grid_layout();
    // Whole pixels keep 1:1 thumbnails crisp while scrolling.
    let scroll = state.grid.scroll(&l, now).round();
    let cursor = state.queue.index();
    let range = state.visual_range();
    let line = sf.round().max(1.0);
    let area = [0.0, 0.0, l.viewport[0], l.viewport[1]];
    for i in l.visible(scroll) {
        let Some(path) = state.queue.get(i) else {
            continue;
        };
        let [x, y] = l.origin(i);
        let cell = [x, y - scroll, l.cell, l.cell];
        let thumb = state.thumbs.get(path);
        let rect = match thumb {
            Some(ThumbState { size, .. }) => {
                let k = (l.cell / size[0].max(size[1]) as f32).min(1.0);
                let (tw, th) = ((size[0] as f32 * k).round(), (size[1] as f32 * k).round());
                [
                    cell[0] + ((l.cell - tw) * 0.5).floor(),
                    cell[1] + ((l.cell - th) * 0.5).floor(),
                    tw,
                    th,
                ]
            }
            _ => cell,
        };
        if range.as_ref().is_some_and(|r| r.contains(&i)) {
            scene.under.push(Prim::rounded(
                expand(cell, 6.0 * sf),
                theme.surface_raised.linear(),
                8.0 * sf,
            ));
        }
        let (cx, cy) = (cell[0] + l.cell * 0.5, cell[1] + l.cell * 0.5);
        match thumb {
            Some(t) => scene.thumbs.push(ThumbDraw {
                path,
                rect,
                alpha: state.thumb_alpha(t.at, now),
            }),
            // Pending: one dim dot, no skeleton.
            None => scene.prims.push(Prim::dot(
                [cx, cy],
                2.0 * sf,
                theme.text_disabled.linear(),
                None,
            )),
        }
        if state.queue.is_marked(i) {
            let r = 4.0 * sf;
            scene.prims.push(Prim::dot(
                [rect[0] + rect[2] - 2.0 * r, rect[1] + 2.0 * r],
                r,
                theme.accent.linear(),
                Some((theme.background.linear(), line)),
            ));
        }
        if i == cursor {
            scene.prims.push(Prim::outline(
                expand(rect, 4.0 * sf),
                theme.text_display.linear(),
                4.0 * sf,
                line,
            ));
        }
        if l.label > 0.0 {
            let color = if i == cursor {
                theme.text_primary
            } else {
                theme.text_secondary
            };
            scene.texts.push(TextItem {
                spans: vec![(file_name(path), color)],
                role: FontRole::Ui,
                size: (GRID_NAME_SIZE * sf).round(),
                weight: 400,
                tracking: 0.0,
                x: cx,
                y: cell[1] + l.cell + l.label * 0.5,
                align: Align::Center,
                max_width: Some(l.cell),
                clip: Some(area),
            });
        }
    }
}

fn alpha_style(bg: AlphaBg, theme: &Theme, sf: f32) -> AlphaStyle {
    let (mode, dot) = match bg {
        AlphaBg::Solid => (0, theme.image_background),
        AlphaBg::Dots => (1, theme.border_visible),
        AlphaBg::Checker => (2, theme.surface_raised),
    };
    let cell = if mode == 2 { 8.0 } else { 16.0 };
    AlphaStyle {
        mode,
        bg: theme.image_background.linear(),
        dot: dot.linear(),
        cell: (cell * sf).round(),
        radius: sf.max(1.0),
    }
}

/// Doto `NO IMAGES` and one hint line.
fn empty_state(scene: &mut Scene, theme: &Theme, sf: f32, area: [f32; 2]) {
    let cx = area[0] * 0.5;
    let cy = area[1] * 0.5;
    scene.texts.push(TextItem {
        spans: vec![("NO IMAGES".into(), theme.text_secondary)],
        role: FontRole::Display,
        size: (48.0 * sf).round(),
        weight: 700,
        tracking: -0.02,
        x: cx,
        y: cy - 12.0 * sf,
        align: Align::Center,
        max_width: None,
        clip: None,
    });
    let hint = "remage <file|dir>…";
    scene.texts.push(label(
        hint,
        theme.text_disabled,
        sf,
        cx,
        cy + 32.0 * sf,
        Align::Center,
    ));
}

fn status_bar(
    scene: &mut Scene,
    state: &AppState,
    theme: &Theme,
    now: Instant,
    [w, h]: [f32; 2],
    bar_h: f32,
    measure: &mut dyn FnMut(&TextItem) -> f32,
) {
    let sf = state.ui_scale();
    let pad = 16.0 * sf;
    let y = h - bar_h * 0.5;
    scene.prims.push(Prim::rect(
        [0.0, h - bar_h, w, bar_h],
        theme.background.linear(),
    ));

    let mut spans: Vec<(String, Rgba)> = Vec::new();
    if !state.pending.is_empty() {
        spans.push((state.pending.clone(), theme.text_disabled));
    }
    if let Some((_, name)) = state.running.last() {
        let more = match state.running.len() {
            1 => String::new(),
            n => format!(" +{}", n - 1),
        };
        spans.push((
            format!("[RUNNING {}{more}]", name.to_uppercase()),
            theme.text_secondary,
        ));
    }
    if let Some(m) = &state.message {
        let color = if m.kind == MsgKind::Error {
            theme.accent
        } else {
            theme.text_primary
        };
        let mut text = m.text.to_uppercase();
        if let Some((cut, _)) = text.char_indices().nth(MAX_MESSAGE) {
            text.truncate(cut);
            text.push('…');
        }
        spans.push((format!("[{text}]"), color));
    }
    if state.loading_visible(now) {
        spans.push(("[LOADING]".into(), theme.text_secondary));
    }
    if state.skipped > 0 {
        spans.push((format!("[SKIPPED {}]", state.skipped), theme.text_secondary));
    }
    if let Some(r) = state.visual_range() {
        spans.push((format!("[VISUAL {}]", r.len()), theme.text_secondary));
    }
    let mut info = Vec::new();
    if let Some(s) = state
        .shown
        .as_ref()
        .filter(|s| state.mode == Mode::Image && Some(s.path.as_path()) == state.queue.current())
    {
        info.push(s.format.to_string());
        info.push(format!("{}×{}", s.size[0], s.size[1]));
        info.push(format!("{}%", (state.view.scale() * 100.0).round()));
    }
    if !state.queue.is_empty() {
        let count = state.queue.len();
        let digits = count.to_string().len().max(3);
        info.push(format!("{:0digits$}/{count}", state.queue.index() + 1));
    }
    if !info.is_empty() {
        spans.push((info.join("  ").to_uppercase(), theme.text_secondary));
    }
    // Two spaces between segments, attached to the preceding span so colours stay put.
    let n = spans.len();
    for (s, _) in spans.iter_mut().take(n.saturating_sub(1)) {
        s.push_str("  ");
    }
    let mut right_w = 0.0;
    if !spans.is_empty() {
        let mut item = label("", theme.text_secondary, sf, w - pad, y, Align::Right);
        // Already in caps, except pending keys: `g` and `G` are different keys.
        item.spans = spans;
        right_w = measure(&item);
        scene.texts.push(item);
    }

    // Marks: an accent dot and the count, filled when the current file is marked.
    let marks = state.queue.marked_count();
    if marks > 0 {
        let gap = if right_w > 0.0 { 16.0 * sf } else { 0.0 };
        let item = label(
            &marks.to_string(),
            theme.text_secondary,
            sf,
            w - pad - right_w - gap,
            y,
            Align::Right,
        );
        let count_w = measure(&item);
        scene.texts.push(item);
        let r = 3.0 * sf;
        let cx = w - pad - right_w - gap - count_w - 6.0 * sf - r;
        let accent = theme.accent.linear();
        let dot = if state.queue.is_marked(state.queue.index()) {
            Prim::dot([cx, y], r, accent, None)
        } else {
            Prim::outline(
                [cx - r, y - r, 2.0 * r, 2.0 * r],
                accent,
                r,
                sf.round().max(1.0),
            )
        };
        scene.prims.push(dot);
        right_w += gap + count_w + 6.0 * sf + 2.0 * r;
    }

    // A pending question takes the filename's place: `DELETE 3 FILES?  Y / N`.
    if let Some(q) = &state.prompt {
        let mut item = label("", theme.text_display, sf, pad, y, Align::Left);
        item.spans = vec![
            (format!("{}  ", q.to_uppercase()), theme.text_display),
            ("Y / N".into(), theme.text_secondary),
        ];
        item.max_width = Some((w - 2.0 * pad - right_w - 32.0 * sf).max(0.0));
        scene.texts.push(item);
        return;
    }

    // The filename takes whatever the right side leaves, ellipsised in the middle.
    if let Some(cur) = state.queue.current() {
        let room = (w - 2.0 * pad - right_w - 32.0 * sf).max(0.0);
        scene.texts.push(TextItem {
            spans: vec![(file_name(cur), theme.text_primary)],
            role: FontRole::Ui,
            size: (14.0 * sf).round(),
            weight: 400,
            tracking: 0.0,
            x: pad,
            y,
            align: Align::Left,
            max_width: Some(room),
            clip: None,
        });
    }
}
