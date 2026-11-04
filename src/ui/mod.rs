//! UI surfaces: status bar, grid view, palette, which-key, help and info panel.
//!
//! Each frame the UI turns state into a [`Scene`]; the renderer only draws it.

use std::path::Path;
use std::time::Instant;

use crate::config::theme::{Rgba, Theme};
use crate::render::image::AlphaStyle;
use crate::render::prims::Prim;
use crate::render::text::{Align, FontRole, TextItem};
use crate::render::{ImageDraw, Scene};
use crate::state::{AlphaBg, AppState, MsgKind};

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
    let sf = state.scale_factor();
    let [w, h] = state.window();
    let bar_h = state.bar_height();
    let mut scene = Scene {
        clear: theme.background,
        image_clip: [0.0, 0.0, w, h - bar_h],
        alpha: alpha_style(state.alpha_bg, theme, sf),
        images: Vec::new(),
        prims: Vec::new(),
        texts: Vec::new(),
    };

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

    if state.queue.is_empty() {
        empty_state(&mut scene, theme, sf, [w, h - bar_h]);
    }
    if bar_h > 0.0 {
        status_bar(&mut scene, state, theme, now, [w, h], bar_h, measure);
    }
    scene
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
    let sf = state.scale_factor();
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
    let mut info = Vec::new();
    if let Some(s) = state
        .shown
        .as_ref()
        .filter(|s| Some(s.path.as_path()) == state.queue.current())
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
        spans.push((info.join("  "), theme.text_secondary));
    }
    // Two spaces between segments, attached to the preceding span so colours stay put.
    let n = spans.len();
    for (s, _) in spans.iter_mut().take(n.saturating_sub(1)) {
        s.push_str("  ");
    }
    let mut right_w = 0.0;
    if !spans.is_empty() {
        let mut item = label("", theme.text_secondary, sf, w - pad, y, Align::Right);
        item.spans = spans
            .into_iter()
            .map(|(s, c)| (s.to_uppercase(), c))
            .collect();
        right_w = measure(&item);
        scene.texts.push(item);
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
        });
    }
}
