//! Keyboard and mouse input: vim key notation and per-mode keymaps.
//!
//! Until the configurable keymap lands (M2), [`Input`] holds the default bindings in code.

pub mod keymap;
pub mod keys;

use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::action::{Action, Zoom};
use crate::state::view::{Axis, Dir, FitMode};

#[derive(Debug, Default)]
pub struct Input {
    /// A `g` is waiting for its second key.
    pending_g: bool,
    /// A count prefix (`5n`, `12G`).
    count: Option<usize>,
}

impl Input {
    /// The incomplete sequence typed so far, for the status bar.
    pub fn pending(&self) -> String {
        let mut s = self.count.map(|c| c.to_string()).unwrap_or_default();
        if self.pending_g {
            s.push('g');
        }
        s
    }

    pub fn cancel(&mut self) {
        self.pending_g = false;
        self.count = None;
    }

    /// Map a key press to an action using the default bindings.
    pub fn key(&mut self, key: &Key, mods: ModifiersState) -> Option<Action> {
        if mods.control_key() || mods.alt_key() || mods.super_key() {
            return None;
        }
        if let Key::Character(s) = key {
            // `0` is a motion unless a count is already pending.
            if let Some(d) = s.chars().next().and_then(|c| c.to_digit(10))
                && s.len() == 1
                && !self.pending_g
                && (d != 0 || self.count.is_some())
            {
                self.count = Some(
                    self.count
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(d as usize)
                        .min(9_999_999),
                );
                return None;
            }
        }
        let was_g = std::mem::take(&mut self.pending_g);
        if !was_g && matches!(key, Key::Character(s) if s == "g") {
            self.pending_g = true;
            return None;
        }
        let count = self.count.take();
        let n = count.unwrap_or(1);
        let action = match key {
            Key::Character(s) => match s.as_str() {
                "g" => count.map_or(Action::First, Action::Goto),
                "q" => Action::Quit,
                "n" => Action::Next(n),
                "p" => Action::Prev(n),
                "]" => Action::Next(10 * n),
                "[" => Action::Prev(10 * n),
                "G" => count.map_or(Action::Last, Action::Goto),
                "h" => Action::Pan(Dir::Left),
                "j" => Action::Pan(Dir::Down),
                "k" => Action::Pan(Dir::Up),
                "l" => Action::Pan(Dir::Right),
                "H" => Action::PanEdge(Dir::Left),
                "J" => Action::PanEdge(Dir::Down),
                "K" => Action::PanEdge(Dir::Up),
                "L" => Action::PanEdge(Dir::Right),
                "+" => Action::Zoom(Zoom::In),
                "-" => Action::Zoom(Zoom::Out),
                "=" => Action::Zoom(Zoom::Percent(100.0)),
                "w" => Action::Fit(FitMode::Fit),
                "W" => Action::Fit(FitMode::Fill),
                "e" => Action::Fit(FitMode::Width),
                "E" => Action::Fit(FitMode::Height),
                "z" => Action::Center,
                "<" => Action::Rotate(-1),
                ">" => Action::Rotate(1),
                "|" => Action::Flip(Axis::Horizontal),
                "_" => Action::Flip(Axis::Vertical),
                "a" => Action::FilterToggle,
                "A" => Action::AlphaBgCycle,
                "f" => Action::Fullscreen,
                "b" => Action::BarToggle,
                "r" => Action::Reload,
                "D" => Action::Remove,
                _ => return None,
            },
            Key::Named(named) => match named {
                NamedKey::Backspace => Action::Prev(n),
                NamedKey::Escape => Action::Cancel,
                NamedKey::ArrowLeft => Action::Pan(Dir::Left),
                NamedKey::ArrowDown => Action::Pan(Dir::Down),
                NamedKey::ArrowUp => Action::Pan(Dir::Up),
                NamedKey::ArrowRight => Action::Pan(Dir::Right),
                NamedKey::Home => Action::First,
                NamedKey::End => Action::Last,
                _ => return None,
            },
            _ => return None,
        };
        Some(action)
    }
}

/// Whether holding the key should repeat the action.
pub fn repeats(action: &Action) -> bool {
    matches!(
        action,
        Action::Next(_) | Action::Prev(_) | Action::Zoom(Zoom::In | Zoom::Out)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }

    #[test]
    fn gg_is_a_sequence() {
        let mut input = Input::default();
        let none = ModifiersState::empty();
        assert_eq!(input.key(&ch("g"), none), None);
        assert_eq!(input.pending(), "g");
        assert_eq!(input.key(&ch("g"), none), Some(Action::First));
        assert_eq!(input.key(&ch("g"), none), None);
        assert_eq!(input.key(&ch("n"), none), Some(Action::Next(1)));
        assert_eq!(input.pending(), "");
    }

    #[test]
    fn counts() {
        let mut input = Input::default();
        let none = ModifiersState::empty();
        for k in ["1", "2"] {
            assert_eq!(input.key(&ch(k), none), None);
        }
        assert_eq!(input.pending(), "12");
        assert_eq!(input.key(&ch("G"), none), Some(Action::Goto(12)));
        assert_eq!(input.key(&ch("3"), none), None);
        assert_eq!(input.key(&ch("n"), none), Some(Action::Next(3)));
        assert_eq!(input.key(&ch("G"), none), Some(Action::Last));
        // `0` alone is not a count.
        assert_eq!(input.key(&ch("0"), none), None);
        assert_eq!(input.pending(), "");
        input.key(&ch("5"), none);
        input.key(&ch("g"), none);
        assert_eq!(input.pending(), "5g");
        assert_eq!(input.key(&ch("g"), none), Some(Action::Goto(5)));
    }

    #[test]
    fn modifiers_block_plain_bindings() {
        let mut input = Input::default();
        assert_eq!(input.key(&ch("n"), ModifiersState::CONTROL), None);
    }
}
