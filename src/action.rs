//! Every user-facing operation, with one grammar (`name [args]`) shared by key bindings, the
//! palette, IPC and command output. `parse(display(a)) == a` for every action.

use std::fmt;
use std::str::FromStr;

use clap::ValueEnum;

use crate::config::suggest;
use crate::state::queue::SortKey;
use crate::state::view::{Axis, Dir, FitMode};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zoom {
    /// Zoom steps.
    In(u32),
    Out(u32),
    /// Absolute zoom, 100 = one image pixel per screen pixel.
    Percent(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Switch {
    Toggle,
    On,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeArg {
    Image,
    Grid,
    Toggle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adjust {
    Gamma,
    Brightness,
    Contrast,
}

/// Grid paging, like vim's `<C-f>`/`<C-b>` and `<C-d>`/`<C-u>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Down,
    Up,
    HalfDown,
    HalfUp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Up,
    Down,
    Reset,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Next(usize),
    Prev(usize),
    First,
    Last,
    /// 1-based, like the status bar.
    Goto(usize),
    Random,
    NextMark,
    PrevMark,
    NextFrame,
    PrevFrame,
    Mark(Switch),
    MarkAll,
    UnmarkAll,
    InvertMarks,
    Zoom(Zoom),
    Fit(FitMode),
    /// From a key this pans for as long as the key is held; otherwise one step.
    Pan(Dir),
    PanEdge(Dir),
    Center,
    /// Grid cursor: cells left/right, rows up/down.
    Move(Dir, usize),
    Page(Page),
    RowStart,
    RowEnd,
    /// Scroll the grid by rows (the image view pans a step).
    Scroll(Dir, usize),
    /// Quarter turns, positive = clockwise.
    Rotate(i32),
    Flip(Axis),
    FilterToggle,
    AlphaBgCycle,
    Adjust(Adjust, Step),
    Play(Switch),
    Slideshow(Switch),
    SlideshowDelay {
        secs: f32,
        relative: bool,
    },
    Mode(ModeArg),
    Visual,
    Fullscreen(Switch),
    Bar(Switch),
    Info(Switch),
    Help,
    Palette,
    Search,
    /// Steps through `grid.sizes`.
    GridSize(i32),
    Sort(SortKey),
    Reverse,
    Filter(String),
    FilterClear,
    Remove,
    Reload,
    Refresh,
    Open(String),
    Run(String),
    ReloadConfig,
    Echo(String),
    Cancel,
    Quit,
    QuitNoOutput,
}

/// One row of the action reference: palette, help and `--dump-config`.
pub struct Entry {
    pub name: &'static str,
    pub args: &'static str,
    pub desc: &'static str,
}

const fn e(name: &'static str, args: &'static str, desc: &'static str) -> Entry {
    Entry { name, args, desc }
}

pub const CATALOG: &[Entry] = &[
    e("next", "[n]", "Next image"),
    e("prev", "[n]", "Previous image"),
    e("first", "", "First image"),
    e("last", "", "Last image"),
    e("goto", "N", "Go to image N"),
    e("random", "", "Random image"),
    e("next-mark", "", "Next marked image"),
    e("prev-mark", "", "Previous marked image"),
    e("next-frame", "", "Next frame or page"),
    e("prev-frame", "", "Previous frame or page"),
    e("mark", "[toggle|on|off]", "Mark the current image"),
    e("mark-all", "", "Mark every image"),
    e("unmark-all", "", "Clear all marks"),
    e("invert-marks", "", "Invert marks"),
    e("zoom", "in [n]|out [n]|N%", "Zoom"),
    e("fit", "down|fit|fill|width|height|original", "Fit mode"),
    e("pan", "left|right|up|down", "Pan (held keys pan smoothly)"),
    e("pan-edge", "left|right|up|down", "Jump to an edge"),
    e("center", "", "Centre the image"),
    e("move", "left|right|up|down [n]", "Move the grid cursor"),
    e("page", "down|up|half-down|half-up", "Page the grid"),
    e("row-start", "", "First thumbnail in the row"),
    e("row-end", "", "Last thumbnail in the row"),
    e(
        "scroll",
        "left|right|up|down [n]",
        "Scroll the grid (pan an image)",
    ),
    e("rotate", "cw|ccw|180", "Rotate the view"),
    e("flip", "h|v", "Flip the view"),
    e("filter", "toggle", "Toggle nearest/linear filtering"),
    e("alpha-bg", "cycle", "Cycle the transparency background"),
    e("gamma", "+|-|reset", "Adjust gamma"),
    e("brightness", "+|-|reset", "Adjust brightness"),
    e("contrast", "+|-|reset", "Adjust contrast"),
    e("play", "[toggle|on|off]", "Play or pause animation"),
    e("slideshow", "[toggle|on|off]", "Slideshow"),
    e("slideshow-delay", "±N|N", "Slideshow delay in seconds"),
    e("mode", "image|grid|toggle", "Switch between image and grid"),
    e("visual", "", "Visual range selection"),
    e("fullscreen", "[toggle|on|off]", "Fullscreen"),
    e("bar", "[toggle|on|off]", "Status bar"),
    e("info", "[toggle|on|off]", "Info panel"),
    e("help", "", "Key reference"),
    e("palette", "", "Command palette"),
    e("search", "", "Search filenames"),
    e("grid-size", "+|-", "Thumbnail size"),
    e("sort", "natural|name|mtime|size|random", "Sort the queue"),
    e("reverse", "", "Reverse the queue order"),
    e("filter", "<pattern>", "Show only matching files"),
    e("filter-clear", "", "Clear the filter"),
    e("remove", "", "Remove from the queue"),
    e("reload", "", "Reload the image from disk"),
    e("refresh", "", "Re-check targets on disk"),
    e("open", "<path>", "Open a file or directory"),
    e("run", "<command>", "Run a user command"),
    e("reload-config", "", "Reload the config"),
    e("echo", "<message>", "Show a message"),
    e("cancel", "", "Cancel pending keys and messages"),
    e("quit", "", "Quit"),
    e("quit-no-output", "", "Quit without printing marks"),
];

fn names() -> impl Iterator<Item = &'static str> {
    CATALOG.iter().map(|e| e.name)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

fn err<T>(msg: impl Into<String>) -> Result<T, ParseError> {
    Err(ParseError(msg.into()))
}

/// Match one word of a fixed set, with a "did you mean" on failure.
fn one_of<T: Copy>(name: &str, arg: &str, opts: &[(&str, T)]) -> Result<T, ParseError> {
    if let Some((_, v)) = opts.iter().find(|(k, _)| k.eq_ignore_ascii_case(arg)) {
        return Ok(*v);
    }
    let expected: Vec<&str> = opts.iter().map(|(k, _)| *k).collect();
    let hint = suggest(arg, expected.iter().copied())
        .map(|s| format!(", did you mean `{s}`?"))
        .unwrap_or_default();
    if arg.is_empty() {
        err(format!("`{name}` needs one of: {}", expected.join(", ")))
    } else {
        err(format!("`{name}`: unknown argument `{arg}`{hint}"))
    }
}

fn no_args(name: &str, rest: &str) -> Result<(), ParseError> {
    if rest.is_empty() {
        Ok(())
    } else {
        err(format!("`{name}` takes no arguments"))
    }
}

fn count(name: &str, rest: &str) -> Result<usize, ParseError> {
    if rest.is_empty() {
        return Ok(1);
    }
    match rest.parse::<usize>() {
        Ok(n) if n > 0 => Ok(n),
        _ => err(format!("`{name}`: expected a positive count, got `{rest}`")),
    }
}

fn switch(name: &str, rest: &str) -> Result<Switch, ParseError> {
    if rest.is_empty() {
        return Ok(Switch::Toggle);
    }
    one_of(
        name,
        rest,
        &[
            ("toggle", Switch::Toggle),
            ("on", Switch::On),
            ("off", Switch::Off),
        ],
    )
}

fn dir(name: &str, rest: &str) -> Result<Dir, ParseError> {
    one_of(
        name,
        rest,
        &[
            ("left", Dir::Left),
            ("right", Dir::Right),
            ("up", Dir::Up),
            ("down", Dir::Down),
        ],
    )
}

fn step(name: &str, rest: &str) -> Result<Step, ParseError> {
    one_of(
        name,
        rest,
        &[("+", Step::Up), ("-", Step::Down), ("reset", Step::Reset)],
    )
}

/// A free-form argument, optionally quoted.
fn text(name: &str, rest: &str) -> Result<String, ParseError> {
    if rest.is_empty() {
        return err(format!("`{name}` needs an argument"));
    }
    for q in ['"', '\''] {
        if rest.len() >= 2 && rest.starts_with(q) && rest.ends_with(q) {
            return Ok(rest[1..rest.len() - 1].to_string());
        }
    }
    Ok(rest.to_string())
}

/// Quote a free-form argument when it would not parse back as itself.
fn quoted(s: &str, reserved: &[&str]) -> String {
    let plain =
        !s.is_empty() && s.trim() == s && !s.starts_with(['"', '\'']) && !reserved.contains(&s);
    if plain {
        s.into()
    } else if s.contains('"') {
        format!("'{s}'")
    } else {
        format!("\"{s}\"")
    }
}

fn value_enum<T: ValueEnum>(name: &str, rest: &str) -> Result<T, ParseError> {
    if let Ok(v) = T::from_str(rest, true) {
        return Ok(v);
    }
    let all: Vec<String> = T::value_variants()
        .iter()
        .filter_map(|v| v.to_possible_value().map(|p| p.get_name().to_string()))
        .collect();
    let hint = suggest(rest, all.iter().map(String::as_str))
        .map(|s| format!(", did you mean `{s}`?"))
        .unwrap_or_default();
    err(format!(
        "`{name}`: expected one of {}, got `{rest}`{hint}",
        all.join(", ")
    ))
}

fn value_name<T: ValueEnum>(v: &T) -> String {
    v.to_possible_value()
        .map(|p| p.get_name().to_string())
        .unwrap_or_default()
}

impl FromStr for Action {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, ParseError> {
        let s = s.trim();
        let (name, rest) = s
            .split_once(char::is_whitespace)
            .map_or((s, ""), |(n, r)| (n, r.trim()));
        let a = match name {
            "next" => Action::Next(count(name, rest)?),
            "prev" => Action::Prev(count(name, rest)?),
            "goto" => match rest.parse::<usize>() {
                Ok(n) if n > 0 => Action::Goto(n),
                _ => return err(format!("`goto` needs a positive index, got `{rest}`")),
            },
            "mark" => Action::Mark(switch(name, rest)?),
            "zoom" => match rest.split_once(' ').unwrap_or((rest, "")) {
                ("in", n) => Action::Zoom(Zoom::In(count(name, n.trim())? as u32)),
                ("out", n) => Action::Zoom(Zoom::Out(count(name, n.trim())? as u32)),
                _ => match rest.trim_end_matches('%').trim().parse::<f32>() {
                    Ok(p) if p.is_finite() && p > 0.0 => Action::Zoom(Zoom::Percent(p)),
                    _ => return err(format!("`zoom` needs in, out or N%, got `{rest}`")),
                },
            },
            "fit" => Action::Fit(value_enum(name, rest)?),
            "pan" => Action::Pan(dir(name, rest)?),
            "pan-edge" => Action::PanEdge(dir(name, rest)?),
            "move" | "scroll" => {
                let (d, n) = rest.split_once(' ').unwrap_or((rest, ""));
                let (d, n) = (dir(name, d)?, count(name, n.trim())?);
                if name == "move" {
                    Action::Move(d, n)
                } else {
                    Action::Scroll(d, n)
                }
            }
            "page" => Action::Page(one_of(
                name,
                rest,
                &[
                    ("down", Page::Down),
                    ("up", Page::Up),
                    ("half-down", Page::HalfDown),
                    ("half-up", Page::HalfUp),
                ],
            )?),
            "rotate" => match rest {
                "cw" => Action::Rotate(1),
                "ccw" => Action::Rotate(-1),
                _ => match rest.parse::<i32>() {
                    Ok(d) if d != 0 && d % 90 == 0 => Action::Rotate(d / 90),
                    _ => return err(format!("`rotate` needs cw, ccw or 180, got `{rest}`")),
                },
            },
            "flip" => Action::Flip(one_of(
                name,
                rest,
                &[
                    ("h", Axis::Horizontal),
                    ("v", Axis::Vertical),
                    ("horizontal", Axis::Horizontal),
                    ("vertical", Axis::Vertical),
                ],
            )?),
            "filter" => match rest {
                "toggle" => Action::FilterToggle,
                _ => Action::Filter(text(name, rest)?),
            },
            "alpha-bg" => {
                one_of(name, rest, &[("cycle", ())])?;
                Action::AlphaBgCycle
            }
            "gamma" => Action::Adjust(Adjust::Gamma, step(name, rest)?),
            "brightness" => Action::Adjust(Adjust::Brightness, step(name, rest)?),
            "contrast" => Action::Adjust(Adjust::Contrast, step(name, rest)?),
            "play" => Action::Play(switch(name, rest)?),
            "slideshow" => Action::Slideshow(switch(name, rest)?),
            "slideshow-delay" => {
                let relative = rest.starts_with(['+', '-']);
                match rest.parse::<f32>() {
                    Ok(secs) if secs.is_finite() && (relative || secs > 0.0) => {
                        Action::SlideshowDelay { secs, relative }
                    }
                    _ => return err(format!("`slideshow-delay` needs ±N or N, got `{rest}`")),
                }
            }
            "mode" => Action::Mode(one_of(
                name,
                rest,
                &[
                    ("image", ModeArg::Image),
                    ("grid", ModeArg::Grid),
                    ("toggle", ModeArg::Toggle),
                ],
            )?),
            "fullscreen" => Action::Fullscreen(switch(name, rest)?),
            "bar" => Action::Bar(switch(name, rest)?),
            "info" => Action::Info(switch(name, rest)?),
            "grid-size" => Action::GridSize(one_of(name, rest, &[("+", 1), ("-", -1)])?),
            "sort" => Action::Sort(value_enum(name, rest)?),
            "open" => Action::Open(text(name, rest)?),
            "run" => match rest {
                "" => return err("`run` needs a command name"),
                r if r.contains(char::is_whitespace) => {
                    return err(format!("`run`: command names have no spaces, got `{r}`"));
                }
                r => Action::Run(r.to_string()),
            },
            "echo" => Action::Echo(text(name, rest)?),
            _ => {
                let a = match name {
                    "first" => Action::First,
                    "last" => Action::Last,
                    "random" => Action::Random,
                    "next-mark" => Action::NextMark,
                    "prev-mark" => Action::PrevMark,
                    "next-frame" => Action::NextFrame,
                    "prev-frame" => Action::PrevFrame,
                    "mark-all" => Action::MarkAll,
                    "unmark-all" => Action::UnmarkAll,
                    "invert-marks" => Action::InvertMarks,
                    "center" | "centre" => Action::Center,
                    "row-start" => Action::RowStart,
                    "row-end" => Action::RowEnd,
                    "visual" => Action::Visual,
                    "help" => Action::Help,
                    "palette" => Action::Palette,
                    "search" => Action::Search,
                    "reverse" => Action::Reverse,
                    "filter-clear" => Action::FilterClear,
                    "remove" => Action::Remove,
                    "reload" => Action::Reload,
                    "refresh" => Action::Refresh,
                    "reload-config" => Action::ReloadConfig,
                    "cancel" => Action::Cancel,
                    "quit" => Action::Quit,
                    "quit-no-output" => Action::QuitNoOutput,
                    "" => return err("empty action"),
                    _ => {
                        let hint = suggest(name, names())
                            .map(|s| format!(", did you mean `{s}`?"))
                            .unwrap_or_default();
                        return err(format!("unknown action `{name}`{hint}"));
                    }
                };
                no_args(name, rest)?;
                a
            }
        };
        Ok(a)
    }
}

fn switch_suffix(s: Switch) -> &'static str {
    match s {
        Switch::Toggle => "",
        Switch::On => " on",
        Switch::Off => " off",
    }
}

fn dir_name(d: Dir) -> &'static str {
    match d {
        Dir::Left => "left",
        Dir::Right => "right",
        Dir::Up => "up",
        Dir::Down => "down",
    }
}

fn page_name(p: Page) -> &'static str {
    match p {
        Page::Down => "down",
        Page::Up => "up",
        Page::HalfDown => "half-down",
        Page::HalfUp => "half-up",
    }
}

fn step_name(s: Step) -> &'static str {
    match s {
        Step::Up => "+",
        Step::Down => "-",
        Step::Reset => "reset",
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.name();
        match self {
            Action::Next(1) | Action::Prev(1) => f.write_str(n),
            Action::Next(c) | Action::Prev(c) => write!(f, "{n} {c}"),
            Action::Goto(i) => write!(f, "goto {i}"),
            Action::Mark(s)
            | Action::Play(s)
            | Action::Slideshow(s)
            | Action::Fullscreen(s)
            | Action::Bar(s)
            | Action::Info(s) => write!(f, "{n}{}", switch_suffix(*s)),
            Action::Zoom(Zoom::In(1)) => f.write_str("zoom in"),
            Action::Zoom(Zoom::Out(1)) => f.write_str("zoom out"),
            Action::Zoom(Zoom::In(c)) => write!(f, "zoom in {c}"),
            Action::Zoom(Zoom::Out(c)) => write!(f, "zoom out {c}"),
            Action::Zoom(Zoom::Percent(p)) => write!(f, "zoom {p}%"),
            Action::Fit(m) => write!(f, "fit {}", value_name(m)),
            Action::Pan(d) | Action::PanEdge(d) => write!(f, "{n} {}", dir_name(*d)),
            Action::Move(d, 1) | Action::Scroll(d, 1) => write!(f, "{n} {}", dir_name(*d)),
            Action::Move(d, c) | Action::Scroll(d, c) => write!(f, "{n} {} {c}", dir_name(*d)),
            Action::Page(p) => write!(f, "page {}", page_name(*p)),
            Action::Rotate(1) => f.write_str("rotate cw"),
            Action::Rotate(-1) => f.write_str("rotate ccw"),
            Action::Rotate(q) => write!(f, "rotate {}", q * 90),
            Action::Flip(Axis::Horizontal) => f.write_str("flip h"),
            Action::Flip(Axis::Vertical) => f.write_str("flip v"),
            Action::FilterToggle => f.write_str("filter toggle"),
            Action::AlphaBgCycle => f.write_str("alpha-bg cycle"),
            Action::Adjust(_, s) => write!(f, "{n} {}", step_name(*s)),
            Action::SlideshowDelay { secs, relative } => {
                if *relative {
                    write!(f, "{n} {secs:+}")
                } else {
                    write!(f, "{n} {secs}")
                }
            }
            Action::Mode(m) => write!(
                f,
                "mode {}",
                match m {
                    ModeArg::Image => "image",
                    ModeArg::Grid => "grid",
                    ModeArg::Toggle => "toggle",
                }
            ),
            Action::GridSize(d) if *d > 0 => f.write_str("grid-size +"),
            Action::GridSize(_) => f.write_str("grid-size -"),
            Action::Sort(k) => write!(f, "sort {}", value_name(k)),
            Action::Filter(p) => write!(f, "filter {}", quoted(p, &["toggle"])),
            Action::Open(p) => write!(f, "open {}", quoted(p, &[])),
            Action::Echo(m) => write!(f, "echo {}", quoted(m, &[])),
            Action::Run(c) => write!(f, "run {c}"),
            _ => f.write_str(n),
        }
    }
}

impl Action {
    /// The grammar's head word.
    pub fn name(&self) -> &'static str {
        match self {
            Action::Next(_) => "next",
            Action::Prev(_) => "prev",
            Action::First => "first",
            Action::Last => "last",
            Action::Goto(_) => "goto",
            Action::Random => "random",
            Action::NextMark => "next-mark",
            Action::PrevMark => "prev-mark",
            Action::NextFrame => "next-frame",
            Action::PrevFrame => "prev-frame",
            Action::Mark(_) => "mark",
            Action::MarkAll => "mark-all",
            Action::UnmarkAll => "unmark-all",
            Action::InvertMarks => "invert-marks",
            Action::Zoom(_) => "zoom",
            Action::Fit(_) => "fit",
            Action::Pan(_) => "pan",
            Action::PanEdge(_) => "pan-edge",
            Action::Center => "center",
            Action::Move(..) => "move",
            Action::Page(_) => "page",
            Action::RowStart => "row-start",
            Action::RowEnd => "row-end",
            Action::Scroll(..) => "scroll",
            Action::Rotate(_) => "rotate",
            Action::Flip(_) => "flip",
            Action::FilterToggle => "filter",
            Action::AlphaBgCycle => "alpha-bg",
            Action::Adjust(Adjust::Gamma, _) => "gamma",
            Action::Adjust(Adjust::Brightness, _) => "brightness",
            Action::Adjust(Adjust::Contrast, _) => "contrast",
            Action::Play(_) => "play",
            Action::Slideshow(_) => "slideshow",
            Action::SlideshowDelay { .. } => "slideshow-delay",
            Action::Mode(_) => "mode",
            Action::Visual => "visual",
            Action::Fullscreen(_) => "fullscreen",
            Action::Bar(_) => "bar",
            Action::Info(_) => "info",
            Action::Help => "help",
            Action::Palette => "palette",
            Action::Search => "search",
            Action::GridSize(_) => "grid-size",
            Action::Sort(_) => "sort",
            Action::Reverse => "reverse",
            Action::Filter(_) => "filter",
            Action::FilterClear => "filter-clear",
            Action::Remove => "remove",
            Action::Reload => "reload",
            Action::Refresh => "refresh",
            Action::Open(_) => "open",
            Action::Run(_) => "run",
            Action::ReloadConfig => "reload-config",
            Action::Echo(_) => "echo",
            Action::Cancel => "cancel",
            Action::Quit => "quit",
            Action::QuitNoOutput => "quit-no-output",
        }
    }

    /// The help section this action is listed under.
    pub fn group(&self) -> &'static str {
        match self {
            Action::Next(_)
            | Action::Prev(_)
            | Action::First
            | Action::Last
            | Action::Goto(_)
            | Action::Random
            | Action::NextMark
            | Action::PrevMark => "Navigation",
            Action::Mark(_) | Action::MarkAll | Action::UnmarkAll | Action::InvertMarks => "Marks",
            Action::Move(..)
            | Action::Page(_)
            | Action::RowStart
            | Action::RowEnd
            | Action::Scroll(..)
            | Action::GridSize(_)
            | Action::Visual => "Grid",
            Action::Zoom(_)
            | Action::Fit(_)
            | Action::Pan(_)
            | Action::PanEdge(_)
            | Action::Center
            | Action::Rotate(_)
            | Action::Flip(_)
            | Action::FilterToggle
            | Action::AlphaBgCycle
            | Action::Adjust(..) => "View",
            Action::Play(_)
            | Action::NextFrame
            | Action::PrevFrame
            | Action::Slideshow(_)
            | Action::SlideshowDelay { .. } => "Playback",
            Action::Mode(_)
            | Action::Fullscreen(_)
            | Action::Bar(_)
            | Action::Info(_)
            | Action::Help
            | Action::Palette
            | Action::Search => "Interface",
            Action::Sort(_)
            | Action::Reverse
            | Action::Filter(_)
            | Action::FilterClear
            | Action::Remove
            | Action::Reload
            | Action::Refresh
            | Action::Open(_) => "Queue",
            Action::Run(_)
            | Action::ReloadConfig
            | Action::Echo(_)
            | Action::Cancel
            | Action::Quit
            | Action::QuitNoOutput => "App",
        }
    }

    /// A one-line description of this exact action, for which-key, help and the palette.
    pub fn description(&self) -> String {
        let toggle = |s: &Switch, what: &str| match s {
            Switch::Toggle => format!("Toggle {what}"),
            Switch::On => format!("Show {what}"),
            Switch::Off => format!("Hide {what}"),
        };
        match self {
            Action::Next(1) => "Next image".into(),
            Action::Prev(1) => "Previous image".into(),
            Action::Next(n) => format!("Forward {n} images"),
            Action::Prev(n) => format!("Back {n} images"),
            Action::Goto(n) => format!("Go to image {n}"),
            Action::Mark(Switch::Toggle) => "Toggle mark".into(),
            Action::Mark(Switch::On) => "Mark".into(),
            Action::Mark(Switch::Off) => "Unmark".into(),
            Action::Zoom(Zoom::In(_)) => "Zoom in".into(),
            Action::Zoom(Zoom::Out(_)) => "Zoom out".into(),
            Action::Zoom(Zoom::Percent(p)) => format!("Zoom to {p}%"),
            Action::Fit(m) => match m {
                FitMode::Down => "Fit, never enlarge".into(),
                FitMode::Fit => "Fit to window".into(),
                FitMode::Fill => "Fill window".into(),
                FitMode::Width => "Fit width".into(),
                FitMode::Height => "Fit height".into(),
                FitMode::Original => "Original size".into(),
            },
            Action::Pan(d) => format!("Pan {}", dir_name(*d)),
            Action::PanEdge(d) => format!(
                "Jump to {} edge",
                match d {
                    Dir::Up => "top",
                    Dir::Down => "bottom",
                    other => dir_name(*other),
                }
            ),
            Action::Move(d, _) => format!("Move {}", dir_name(*d)),
            Action::Scroll(d, _) => format!("Scroll {}", dir_name(*d)),
            Action::Page(p) => match p {
                Page::Down => "Page down".into(),
                Page::Up => "Page up".into(),
                Page::HalfDown => "Half a page down".into(),
                Page::HalfUp => "Half a page up".into(),
            },
            Action::Rotate(1) => "Rotate clockwise".into(),
            Action::Rotate(-1) => "Rotate counter-clockwise".into(),
            Action::Rotate(q) => format!("Rotate {}°", q * 90),
            Action::Flip(Axis::Horizontal) => "Flip horizontally".into(),
            Action::Flip(Axis::Vertical) => "Flip vertically".into(),
            Action::Adjust(a, s) => {
                let what = match a {
                    Adjust::Gamma => "gamma",
                    Adjust::Brightness => "brightness",
                    Adjust::Contrast => "contrast",
                };
                match s {
                    Step::Up => format!("Increase {what}"),
                    Step::Down => format!("Decrease {what}"),
                    Step::Reset => format!("Reset {what}"),
                }
            }
            Action::Play(s) => match s {
                Switch::Toggle => "Play / pause".into(),
                Switch::On => "Play".into(),
                Switch::Off => "Pause".into(),
            },
            Action::Slideshow(s) => toggle(s, "slideshow"),
            Action::SlideshowDelay { secs, relative } if *relative => {
                format!("Slideshow delay {secs:+}s")
            }
            Action::SlideshowDelay { secs, .. } => format!("Slideshow delay {secs}s"),
            Action::Mode(ModeArg::Image) => "Image mode".into(),
            Action::Mode(ModeArg::Grid) => "Grid mode".into(),
            Action::Mode(ModeArg::Toggle) => "Toggle image / grid".into(),
            Action::Fullscreen(s) => toggle(s, "fullscreen"),
            Action::Bar(s) => toggle(s, "status bar"),
            Action::Info(s) => toggle(s, "info panel"),
            Action::GridSize(d) if *d > 0 => "Larger thumbnails".into(),
            Action::GridSize(_) => "Smaller thumbnails".into(),
            Action::Sort(k) => format!("Sort by {}", value_name(k)),
            Action::Filter(p) => format!("Filter: {p}"),
            Action::Open(p) => format!("Open {p}"),
            Action::Run(c) => format!("Run {c}"),
            Action::Echo(m) => format!("Echo: {m}"),
            other => CATALOG
                .iter()
                .find(|e| e.name == other.name())
                .map_or_else(|| other.to_string(), |e| e.desc.to_string()),
        }
    }

    /// Apply a count prefix: `5n` moves five images, `12G` goes to image 12, `3+` zooms three
    /// steps. Actions without a meaningful count ignore it.
    pub fn with_count(self, n: usize) -> Action {
        let n = n.max(1);
        let n32 = n.min(i32::MAX as usize) as i32;
        match self {
            Action::Next(k) => Action::Next(k.saturating_mul(n)),
            Action::Prev(k) => Action::Prev(k.saturating_mul(n)),
            Action::First | Action::Last => Action::Goto(n),
            Action::Zoom(Zoom::In(k)) => Action::Zoom(Zoom::In(k.saturating_mul(n32 as u32))),
            Action::Zoom(Zoom::Out(k)) => Action::Zoom(Zoom::Out(k.saturating_mul(n32 as u32))),
            Action::Rotate(q) => {
                // Keep at most one full turn so the animation stays sane.
                let total = q.saturating_mul(n32);
                Action::Rotate(total.signum() * ((total.abs() - 1) % 4 + 1))
            }
            Action::GridSize(d) => Action::GridSize(d.saturating_mul(n32)),
            Action::Move(d, k) => Action::Move(d, k.saturating_mul(n)),
            Action::Scroll(d, k) => Action::Scroll(d, k.saturating_mul(n)),
            other => other,
        }
    }

    /// Whether holding the key should repeat the action.
    pub fn repeats(&self) -> bool {
        matches!(
            self,
            Action::Next(_)
                | Action::Prev(_)
                | Action::NextMark
                | Action::PrevMark
                | Action::NextFrame
                | Action::PrevFrame
                | Action::Zoom(Zoom::In(_) | Zoom::Out(_))
                | Action::GridSize(_)
                | Action::Move(..)
                | Action::Scroll(..)
                | Action::Page(_)
                | Action::Adjust(_, Step::Up | Step::Down)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Action {
        s.parse().unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn parses_the_grammar() {
        assert_eq!(p("next"), Action::Next(1));
        assert_eq!(p("  next   5 "), Action::Next(5));
        assert_eq!(p("goto 12"), Action::Goto(12));
        assert_eq!(p("zoom in"), Action::Zoom(Zoom::In(1)));
        assert_eq!(p("zoom 150%"), Action::Zoom(Zoom::Percent(150.0)));
        assert_eq!(p("zoom 50"), Action::Zoom(Zoom::Percent(50.0)));
        assert_eq!(p("fit width"), Action::Fit(FitMode::Width));
        assert_eq!(p("rotate 180"), Action::Rotate(2));
        assert_eq!(p("rotate ccw"), Action::Rotate(-1));
        assert_eq!(p("flip v"), Action::Flip(Axis::Vertical));
        assert_eq!(p("filter toggle"), Action::FilterToggle);
        assert_eq!(p("filter *.png"), Action::Filter("*.png".into()));
        assert_eq!(p("filter \"toggle\""), Action::Filter("toggle".into()));
        assert_eq!(p("bar toggle"), Action::Bar(Switch::Toggle));
        assert_eq!(p("bar off"), Action::Bar(Switch::Off));
        assert_eq!(p("mark"), Action::Mark(Switch::Toggle));
        assert_eq!(p("gamma +"), Action::Adjust(Adjust::Gamma, Step::Up));
        assert_eq!(
            p("slideshow-delay -1.5"),
            Action::SlideshowDelay {
                secs: -1.5,
                relative: true
            }
        );
        assert_eq!(p("sort mtime"), Action::Sort(SortKey::Mtime));
        assert_eq!(p("run wallpaper"), Action::Run("wallpaper".into()));
        assert_eq!(p("echo hello  world"), Action::Echo("hello  world".into()));
        assert_eq!(p("quit-no-output"), Action::QuitNoOutput);
    }

    #[test]
    fn errors_are_helpful() {
        let e = |s: &str| s.parse::<Action>().unwrap_err().0;
        assert_eq!(e("nxt"), "unknown action `nxt`, did you mean `next`?");
        assert!(
            e("fit wdth").contains("did you mean `width`"),
            "{}",
            e("fit wdth")
        );
        assert!(e("quit now").contains("takes no arguments"));
        assert!(e("goto 0").contains("positive"));
        assert!(e("next -1").contains("positive"));
        assert!(e("rotate 45").contains("cw, ccw"));
        assert!(e("run two words").contains("no spaces"));
        assert!(e("").contains("empty"));
    }

    #[test]
    fn display_round_trips() {
        let all = [
            "next",
            "next 5",
            "prev",
            "prev 3",
            "first",
            "last",
            "goto 7",
            "random",
            "next-mark",
            "prev-mark",
            "next-frame",
            "prev-frame",
            "mark",
            "mark on",
            "mark off",
            "mark-all",
            "unmark-all",
            "invert-marks",
            "zoom in",
            "zoom out 3",
            "zoom 100%",
            "zoom 12.5%",
            "fit down",
            "fit original",
            "pan left",
            "pan-edge down",
            "center",
            "move left",
            "move down 3",
            "page half-down",
            "page up",
            "row-start",
            "row-end",
            "scroll up",
            "scroll down 2",
            "rotate cw",
            "rotate ccw",
            "rotate 180",
            "flip h",
            "filter toggle",
            "alpha-bg cycle",
            "gamma +",
            "brightness -",
            "contrast reset",
            "play",
            "slideshow on",
            "slideshow-delay +1",
            "slideshow-delay 5",
            "mode toggle",
            "mode grid",
            "visual",
            "fullscreen",
            "bar",
            "info off",
            "help",
            "palette",
            "search",
            "grid-size +",
            "grid-size -",
            "sort natural",
            "reverse",
            "filter cat",
            "filter \"toggle\"",
            "filter-clear",
            "remove",
            "reload",
            "refresh",
            "open ~/Pictures/a b.png",
            "run copy",
            "reload-config",
            "echo hi there",
            "echo \" padded \"",
            "cancel",
            "quit",
            "quit-no-output",
        ];
        for s in all {
            let a = p(s);
            assert_eq!(a.to_string(), s, "display of {a:?}");
            assert_eq!(p(&a.to_string()), a);
            assert!(!a.description().is_empty());
        }
        // Every catalog entry is reachable from the list above.
        for entry in CATALOG {
            assert!(
                all.iter().any(|s| p(s).name() == entry.name),
                "{} untested",
                entry.name
            );
        }
    }

    #[test]
    fn counts() {
        assert_eq!(Action::Next(10).with_count(3), Action::Next(30));
        assert_eq!(Action::First.with_count(12), Action::Goto(12));
        assert_eq!(Action::Last.with_count(5), Action::Goto(5));
        assert_eq!(
            Action::Zoom(Zoom::In(1)).with_count(3),
            Action::Zoom(Zoom::In(3))
        );
        assert_eq!(Action::GridSize(-1).with_count(2), Action::GridSize(-2));
        assert_eq!(
            Action::Move(Dir::Down, 1).with_count(5),
            Action::Move(Dir::Down, 5)
        );
        assert_eq!(Action::Quit.with_count(4), Action::Quit);
    }
}
