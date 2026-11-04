//! UI surfaces: status bar, grid view, palette, which-key, help and info panel.
//!
//! Each frame the UI turns state into a [`Scene`]; the renderer only draws it.

pub mod template;

use std::path::Path;
use std::time::Instant;

use crate::config::theme::{Rgba, Theme};
use crate::render::atlas::ThumbDraw;
use crate::render::image::AlphaStyle;
use crate::render::prims::Prim;
use crate::render::text::{Align, FontRole, TextItem};
use crate::render::{ImageDraw, Scene};
use crate::state::overlay::{HelpRow, InputKind, LIST_ROWS};
use crate::state::{
    AlphaBg, AppState, BAR_HEIGHT, GRID_NAME_SIZE, HELP_HEAD, Mode, MsgKind, PANEL_MARGIN,
    PANEL_PAD, PANEL_ROW, ThumbState,
};

/// Space Mono label tracking.
const LABEL_TRACKING: f32 = 0.08;

pub use crate::state::file_name;

/// Key notation for display: `<lt>`, `<Bar>` and `<Bslash>` as the characters themselves.
pub fn pretty_keys(s: &str) -> String {
    s.replace("<lt>", "<")
        .replace("<Bar>", "|")
        .replace("<Bslash>", "\\")
}

/// Space Mono caps: labels and data.
fn label(text: &str, color: Rgba, sf: f32, x: f32, y: f32, align: Align) -> TextItem {
    mono(&text.to_uppercase(), color, sf, x, y, align)
}

/// Space Mono as written: key names, where `g` and `G` differ, and action grammar.
fn mono(text: &str, color: Rgba, sf: f32, x: f32, y: f32, align: Align) -> TextItem {
    TextItem {
        spans: vec![(text.to_string(), color)],
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

/// Space Grotesk 14: names and descriptions.
fn body(text: &str, color: Rgba, sf: f32, x: f32, y: f32, align: Align) -> TextItem {
    TextItem {
        spans: vec![(text.to_string(), color)],
        role: FontRole::Ui,
        size: (14.0 * sf).round(),
        weight: 400,
        tracking: 0.0,
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
        alpha: AlphaStyle {
            adjust: [
                state.adjust.gamma,
                state.adjust.brightness,
                state.adjust.contrast,
            ],
            ..alpha_style(state.alpha_bg, theme, sf)
        },
        images: Vec::new(),
        thumbs: Vec::new(),
        under: Vec::new(),
        prims: Vec::new(),
        texts: Vec::new(),
        overlay_prims: Vec::new(),
        overlay_texts: Vec::new(),
    };

    if state.in_grid() {
        grid(&mut scene, state, theme, now);
    } else {
        image(&mut scene, state, now);
    }
    if state.queue.is_empty() {
        let hint = if state.queue.all_files().is_empty() {
            None
        } else if state.input.is_some() {
            Some("<Esc> brings everything back")
        } else {
            Some("filter-clear shows everything")
        };
        empty_state(&mut scene, theme, sf, [w, h - bar_h], hint);
    }
    if bar_h > 0.0 {
        status_bar(&mut scene, state, theme, now, [w, h], bar_h, measure);
    } else if state.prompt.is_some() {
        // A question can't hide with the bar: it overlays the image until answered.
        let bar_h = (BAR_HEIGHT * sf).round();
        status_bar(&mut scene, state, theme, now, [w, h], bar_h, measure);
    }

    // Overlays, anchored to the bottom (above the bar) or the right edge.
    let o = Overlay {
        sf,
        m: (PANEL_MARGIN * sf).round(),
        pad: (PANEL_PAD * sf).round(),
        row: (PANEL_ROW * sf).round(),
        w,
        bottom: h - bar_h,
    };
    // Panels share one text layer, so they never overlap: help takes the right edge alone,
    // and the info panel stays in the upper part, clear of the bottom panels.
    if state.help.is_some() {
        help_panel(&mut scene, state, theme, &o);
    } else if state.info_open {
        info_panel(&mut scene, state, theme, &o);
    }
    if state.input.is_some() {
        prompt_panel(&mut scene, state, theme, &o, measure);
    } else if state.which_key.is_some() && state.help.is_none() {
        which_key_panel(&mut scene, state, theme, &o);
    } else {
        count_numerals(&mut scene, state, theme, &o);
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
        adjust: [1.0, 0.0, 1.0],
        mode,
        bg: theme.image_background.linear(),
        dot: dot.linear(),
        cell: (cell * sf).round(),
        radius: sf.max(1.0),
    }
}

/// Doto `NO IMAGES` (or `NO MATCHES` behind a filter) and one hint line.
fn empty_state(scene: &mut Scene, theme: &Theme, sf: f32, area: [f32; 2], hint: Option<&str>) {
    let cx = area[0] * 0.5;
    let cy = area[1] * 0.5;
    let title = if hint.is_some() {
        "NO MATCHES"
    } else {
        "NO IMAGES"
    };
    scene.texts.push(TextItem {
        spans: vec![(title.into(), theme.text_secondary)],
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
    let hint = hint.unwrap_or("remage <file|dir>…");
    scene.texts.push(label(
        hint,
        theme.text_disabled,
        sf,
        cx,
        cy + 32.0 * sf,
        Align::Center,
    ));
}

/// A template placeholder's value for the current state: `Some("")` when it has none right
/// now (the segment drops out), `None` for unknown names.
pub fn var(state: &AppState, name: &str) -> Option<String> {
    let q = &state.queue;
    let cur = q.current();
    let lossy = |s: &std::ffi::OsStr| s.to_string_lossy().into_owned();
    let shown = state
        .shown
        .as_ref()
        .filter(|s| state.mode == Mode::Image && Some(s.path.as_path()) == cur);
    let v = match name {
        "name" => cur.map(file_name),
        "path" => cur.map(|p| p.display().to_string()),
        "dir" => cur.and_then(Path::parent).map(|d| d.display().to_string()),
        "stem" => cur.and_then(Path::file_stem).map(lossy),
        "ext" => cur.and_then(Path::extension).map(lossy),
        "format" => shown.map(|s| s.format.to_string()),
        "width" => shown.map(|s| s.size[0].to_string()),
        "height" => shown.map(|s| s.size[1].to_string()),
        "zoom" => shown.map(|_| format!("{}", (state.view.scale() * 100.0).round())),
        "index" => (!q.is_empty()).then(|| {
            let digits = q.len().to_string().len().max(3);
            format!("{:0digits$}", q.index() + 1)
        }),
        "count" => (!q.is_empty()).then(|| q.len().to_string()),
        "marked" => (q.marked_count() > 0).then(|| q.marked_count().to_string()),
        "frame" => shown
            .and(state.playback.as_ref())
            .map(|pb| format!("{}/{}", pb.index + 1, pb.len())),
        "mode" => Some(state.mode.name().to_string()),
        "filter" => q.filter().map(str::to_string),
        "info" => state
            .info_line
            .as_ref()
            .filter(|(p, _)| Some(p.as_path()) == cur)
            .map(|(_, l)| l.clone()),
        _ => return None,
    };
    Some(v.unwrap_or_default())
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
    let var = |n: &str| var(state, n);

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
    if let Some(f) = state.queue.filter() {
        info.push(format!("FILTER {f}"));
    }
    if state.slideshow.is_some() {
        info.push(format!(
            "SLIDESHOW {}S",
            (state.slideshow_delay.as_secs_f32() * 10.0).round() / 10.0
        ));
    }
    if let Some(pb) = state
        .playback
        .as_ref()
        .filter(|_| state.mode == Mode::Image)
    {
        let at = format!("{}/{}", pb.index + 1, pb.len());
        if !pb.animated {
            info.push(format!("PAGE {at}"));
        } else if !pb.playing {
            info.push(format!("FRAME {at}"));
        }
    }
    let right = template::expand(&state.settings.bar_right, &var);
    if !right.is_empty() {
        info.push(right);
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

    // The left side (the filename by default) takes whatever the right leaves, ellipsised in
    // the middle. An `info_command` line replaces it unless a template places `{info}`.
    let s = &state.settings;
    let placed = s.bar_left.contains("{info}") || s.bar_right.contains("{info}");
    let left = match var("info") {
        Some(line) if !placed && !line.is_empty() => line,
        _ => template::expand(&s.bar_left, &var),
    };
    if !left.is_empty() {
        let room = (w - 2.0 * pad - right_w - 32.0 * sf).max(0.0);
        let mut item = body(&left, theme.text_primary, sf, pad, y, Align::Left);
        item.max_width = Some(room);
        scene.texts.push(item);
    }
}

/// Overlay geometry for this frame, in physical pixels.
struct Overlay {
    sf: f32,
    m: f32,
    pad: f32,
    row: f32,
    w: f32,
    /// The bottom of the area above the status bar.
    bottom: f32,
}

impl Overlay {
    /// A panel: `surface`, a 1 px `border_visible` border, 12 px radius.
    fn panel(&self, scene: &mut Scene, theme: &Theme, r: [f32; 4]) {
        let line = self.sf.round().max(1.0);
        let radius = 12.0 * self.sf;
        scene
            .overlay_prims
            .push(Prim::rounded(r, theme.surface.linear(), radius));
        scene.overlay_prims.push(Prim::outline(
            r,
            theme.border_visible.linear(),
            radius,
            line,
        ));
    }

    /// The selected row: `surface_raised` with a 2 px accent bar on the left.
    fn selection(&self, scene: &mut Scene, theme: &Theme, r: [f32; 4]) {
        let sf = self.sf;
        scene
            .overlay_prims
            .push(Prim::rounded(r, theme.surface_raised.linear(), 6.0 * sf));
        let bar = (2.0 * sf).round().max(1.0);
        scene.overlay_prims.push(Prim::rect(
            [r[0], r[1] + 6.0 * sf, bar, r[3] - 12.0 * sf],
            theme.accent.linear(),
        ));
    }
}

fn clipped(mut t: TextItem, max: f32) -> TextItem {
    t.max_width = Some(max.max(0.0));
    t
}

/// The `:` palette or `/` search: the line at the bottom, matches above it, best nearest.
fn prompt_panel(
    scene: &mut Scene,
    state: &AppState,
    theme: &Theme,
    o: &Overlay,
    measure: &mut dyn FnMut(&TextItem) -> f32,
) {
    let Some(input) = &state.input else { return };
    let sf = o.sf;
    let pw = (o.w - 2.0 * o.m).min(720.0 * sf).max(0.0);
    let x = o.m;
    let line_h = (40.0 * sf).round();
    let image_search = input.kind == InputKind::Search && state.mode == Mode::Image;
    let listed = input.kind == InputKind::Palette || image_search;
    let start = input.window_start();
    let shown = if listed {
        input.matches.len().saturating_sub(start).min(LIST_ROWS)
    } else {
        0
    };
    let gap = 8.0 * sf;
    let list_h = if shown > 0 {
        shown as f32 * o.row + gap
    } else {
        0.0
    };
    let ph = line_h + list_h;
    let y0 = o.bottom - o.m - ph;
    o.panel(scene, theme, [x, y0, pw, ph]);

    // The line: prompt character, text, caret; the match count on the right.
    let ly = y0 + list_h + line_h * 0.5;
    let tx = x + o.pad;
    scene.overlay_texts.push(mono(
        input.prefix(),
        theme.text_secondary,
        sf,
        tx,
        ly,
        Align::Left,
    ));
    let text_x = tx + 16.0 * sf;
    let count = if listed {
        format!("{}", input.matches.len())
    } else {
        format!("{}/{}", state.queue.len(), state.queue.all_files().len())
    };
    let count_item = label(
        &count,
        theme.text_disabled,
        sf,
        x + pw - o.pad,
        ly,
        Align::Right,
    );
    let count_w = measure(&count_item);
    scene.overlay_texts.push(count_item);
    let room = pw - (text_x - x) - o.pad - count_w - 16.0 * sf;
    let text = body(&input.text, theme.text_display, sf, text_x, ly, Align::Left);
    let text_w = if input.text.is_empty() {
        0.0
    } else {
        measure(&text).min(room)
    };
    if !input.text.is_empty() {
        scene.overlay_texts.push(clipped(text, room));
    }
    let caret_h = (16.0 * sf).round();
    scene.overlay_prims.push(Prim::rect(
        [
            text_x + text_w + sf,
            ly - caret_h * 0.5,
            sf.round().max(1.0),
            caret_h,
        ],
        theme.text_display.linear(),
    ));

    // Matches, bottom-up from the line.
    let inner = pw - 2.0 * 8.0 * sf;
    for k in 0..shown {
        let idx = start + k;
        let Some(&m) = input.matches.get(idx) else {
            break;
        };
        let ry = y0 + list_h - gap - (k + 1) as f32 * o.row;
        let rx = x + 8.0 * sf;
        if idx == input.selected {
            o.selection(scene, theme, [rx, ry, inner, o.row]);
        }
        let cy = ry + o.row * 0.5;
        let lx = rx + 16.0 * sf;
        let right = rx + inner - 8.0 * sf;
        match input.kind {
            InputKind::Palette => {
                let Some(item) = input.items.get(m) else {
                    continue;
                };
                let keys_w = inner * 0.2;
                let action_x = rx + inner * 0.55;
                scene.overlay_texts.push(clipped(
                    body(&item.desc, theme.text_primary, sf, lx, cy, Align::Left),
                    action_x - lx - 16.0 * sf,
                ));
                scene.overlay_texts.push(clipped(
                    mono(
                        &item.action,
                        theme.text_disabled,
                        sf,
                        action_x,
                        cy,
                        Align::Left,
                    ),
                    right - keys_w - action_x - 16.0 * sf,
                ));
                if !item.keys.is_empty() {
                    scene.overlay_texts.push(clipped(
                        mono(
                            &item.keys,
                            theme.text_secondary,
                            sf,
                            right,
                            cy,
                            Align::Right,
                        ),
                        keys_w,
                    ));
                }
            }
            InputKind::Search => {
                let Some(p) = state.queue.get(m) else {
                    continue;
                };
                let digits = state.queue.len().to_string().len().max(3);
                scene.overlay_texts.push(clipped(
                    body(&file_name(p), theme.text_primary, sf, lx, cy, Align::Left),
                    inner * 0.8,
                ));
                scene.overlay_texts.push(label(
                    &format!("{:0digits$}", m + 1),
                    theme.text_secondary,
                    sf,
                    right,
                    cy,
                    Align::Right,
                ));
            }
        }
    }
}

/// Keys that continue the pending sequence, in columns above the bar.
fn which_key_panel(scene: &mut Scene, state: &AppState, theme: &Theme, o: &Overlay) {
    let Some(rows) = &state.which_key else { return };
    if rows.is_empty() {
        return;
    }
    let sf = o.sf;
    let room = (o.w - 2.0 * o.m).max(0.0);
    let col_w = 280.0 * sf;
    let cols = (((room - 2.0 * o.pad) / col_w).floor() as usize).clamp(1, rows.len());
    let pw = (cols as f32 * col_w + 2.0 * o.pad).min(room);
    let per_col = rows.len().div_ceil(cols).min(12);
    let head = 32.0 * sf;
    let ph = 2.0 * o.pad + head + per_col as f32 * o.row - 8.0 * sf;
    let (x0, y0) = (o.m, o.bottom - o.m - ph);
    o.panel(scene, theme, [x0, y0, pw, ph]);
    scene.overlay_texts.push(mono(
        &state.pending,
        theme.text_secondary,
        sf,
        x0 + o.pad,
        y0 + o.pad + 8.0 * sf,
        Align::Left,
    ));
    let key_w = 72.0 * sf;
    for (i, r) in rows.iter().take(per_col * cols).enumerate() {
        let (c, k) = (i / per_col, i % per_col);
        let x = x0 + o.pad + c as f32 * col_w;
        let cy = y0 + o.pad + head + k as f32 * o.row + o.row * 0.5 - 4.0 * sf;
        scene.overlay_texts.push(clipped(
            mono(&r.key, theme.text_display, sf, x, cy, Align::Left),
            key_w - 8.0 * sf,
        ));
        let color = if r.prefix {
            theme.text_disabled
        } else {
            theme.text_secondary
        };
        scene.overlay_texts.push(clipped(
            body(&r.desc, color, sf, x + key_w, cy, Align::Left),
            col_w - key_w - 16.0 * sf,
        ));
    }
}

/// The key reference, on the right edge.
fn help_panel(scene: &mut Scene, state: &AppState, theme: &Theme, o: &Overlay) {
    let Some(help) = &state.help else { return };
    let sf = o.sf;
    let pw = (o.w - 2.0 * o.m).min(520.0 * sf).max(0.0);
    let ph = (o.bottom - 2.0 * o.m).max(0.0);
    let (x0, y0) = (o.w - o.m - pw, o.m);
    o.panel(scene, theme, [x0, y0, pw, ph]);
    let page = state.help_page();
    let title = format!("Keys · {}", state.mode.name());
    let head_y = y0 + o.pad + 8.0 * sf;
    scene.overlay_texts.push(label(
        &title,
        theme.text_secondary,
        sf,
        x0 + o.pad,
        head_y,
        Align::Left,
    ));
    if help.rows.len() > page {
        let last = (help.scroll + page).min(help.rows.len());
        scene.overlay_texts.push(label(
            &format!("{}–{} / {}", help.scroll + 1, last, help.rows.len()),
            theme.text_disabled,
            sf,
            x0 + pw - o.pad,
            head_y,
            Align::Right,
        ));
    }
    let key_w = 136.0 * sf;
    let top = y0 + o.pad + HELP_HEAD * sf;
    for (k, row) in help.rows.iter().skip(help.scroll).take(page).enumerate() {
        let cy = top + k as f32 * o.row + o.row * 0.5;
        let x = x0 + o.pad;
        match row {
            HelpRow::Header(h) => {
                scene.overlay_texts.push(label(
                    h,
                    theme.text_disabled,
                    sf,
                    x,
                    cy + 4.0 * sf,
                    Align::Left,
                ));
            }
            HelpRow::Binding { keys, desc } => {
                scene.overlay_texts.push(clipped(
                    mono(keys, theme.text_secondary, sf, x, cy, Align::Left),
                    key_w - 12.0 * sf,
                ));
                scene.overlay_texts.push(clipped(
                    body(desc, theme.text_primary, sf, x + key_w, cy, Align::Left),
                    pw - 2.0 * o.pad - key_w,
                ));
            }
        }
    }
}

/// File and image metadata, top right.
fn info_panel(scene: &mut Scene, state: &AppState, theme: &Theme, o: &Overlay) {
    let sf = o.sf;
    let cur = state.queue.current();
    let mut rows: Vec<(String, String)> = match (&state.info, cur) {
        (Some(info), Some(c)) if info.path == c => info.rows.clone(),
        (_, Some(_)) => vec![("".into(), "[LOADING]".into())],
        (_, None) => return,
    };
    // What the decoder found, after the file rows.
    if let Some(s) = state
        .shown
        .as_ref()
        .filter(|s| Some(s.path.as_path()) == cur && state.info.is_some())
    {
        let at = rows
            .iter()
            .position(|(l, _)| l == "Modified")
            .map_or(rows.len(), |i| i + 1);
        let [w, h] = s.size;
        let mp = w as f64 * h as f64 / 1e6;
        let mut add = vec![
            ("Format".to_string(), s.format.to_string()),
            ("Pixels".to_string(), format!("{w} × {h}  ({mp:.1} MP)")),
        ];
        if let Some(pb) = &state.playback {
            let what = if pb.animated { "Frames" } else { "Pages" };
            add.push((what.to_string(), pb.len().to_string()));
        }
        rows.splice(at..at, add);
    }
    let pw = (o.w - 2.0 * o.m).min(400.0 * sf).max(0.0);
    let max_rows = (((o.bottom * 0.55 - o.m - 2.0 * o.pad) / o.row).floor() as usize).max(1);
    let n = rows.len().min(max_rows);
    let ph = 2.0 * o.pad + n as f32 * o.row - 8.0 * sf;
    let (x0, y0) = (o.w - o.m - pw, o.m);
    o.panel(scene, theme, [x0, y0, pw, ph]);
    let label_w = 112.0 * sf;
    for (k, (l, v)) in rows.iter().take(n).enumerate() {
        let cy = y0 + o.pad + k as f32 * o.row + o.row * 0.5 - 4.0 * sf;
        scene.overlay_texts.push(label(
            l,
            theme.text_secondary,
            sf,
            x0 + o.pad,
            cy,
            Align::Left,
        ));
        scene.overlay_texts.push(clipped(
            body(v, theme.text_primary, sf, x0 + pw - o.pad, cy, Align::Right),
            pw - 2.0 * o.pad - label_w,
        ));
    }
}

/// A count being typed shows as large Doto numerals in the corner.
fn count_numerals(scene: &mut Scene, state: &AppState, theme: &Theme, o: &Overlay) {
    let digits: String = state
        .pending
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        return;
    }
    let size = (72.0 * o.sf).round();
    scene.overlay_texts.push(TextItem {
        spans: vec![(digits, theme.text_display)],
        role: FontRole::Display,
        size,
        weight: 700,
        tracking: 0.0,
        x: o.w - o.m - 8.0 * o.sf,
        y: o.bottom - o.m - size * 0.5,
        align: Align::Right,
        max_width: None,
        clip: None,
    });
}
