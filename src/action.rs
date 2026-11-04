//! Every user-facing operation. Keys produce these today; the palette, IPC and command
//! output will share the same grammar (parser and `Display` arrive with the keymap in M2).

use crate::state::view::{Axis, Dir, FitMode};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zoom {
    In,
    Out,
    /// Absolute zoom, 100 = one image pixel per screen pixel.
    Percent(f32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Next(usize),
    Prev(usize),
    First,
    Last,
    /// 1-based, like the status bar.
    Goto(usize),
    Zoom(Zoom),
    Fit(FitMode),
    /// From a key this pans for as long as the key is held; otherwise one step.
    Pan(Dir),
    PanEdge(Dir),
    Center,
    /// Quarter turns, positive = clockwise.
    Rotate(i32),
    Flip(Axis),
    FilterToggle,
    AlphaBgCycle,
    Fullscreen,
    BarToggle,
    Reload,
    Remove,
    Cancel,
    Quit,
}
