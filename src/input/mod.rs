//! Keyboard and mouse input: vim key notation, per-mode keymaps, and the bridge from winit.

pub mod keymap;
pub mod keys;

use winit::event::MouseButton;
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

use keys::{Button, Code, Key, Mods, Named, Scroll};

pub fn mods(m: ModifiersState) -> Mods {
    Mods {
        ctrl: m.control_key(),
        alt: m.alt_key(),
        shift: m.shift_key(),
        sup: m.super_key(),
    }
}

/// A key press in vim terms. Modifier keys on their own, dead keys and unnamed keys give `None`.
pub fn key(key: &WinitKey, m: ModifiersState) -> Option<Key> {
    let code = match key {
        WinitKey::Character(s) => {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Code::Char(c),
                _ => return None,
            }
        }
        WinitKey::Named(n) => Code::Named(match n {
            NamedKey::Space => Named::Space,
            NamedKey::Enter => Named::Enter,
            NamedKey::Escape => Named::Esc,
            NamedKey::Tab => Named::Tab,
            NamedKey::Backspace => Named::Backspace,
            NamedKey::Delete => Named::Delete,
            NamedKey::Insert => Named::Insert,
            NamedKey::Home => Named::Home,
            NamedKey::End => Named::End,
            NamedKey::PageUp => Named::PageUp,
            NamedKey::PageDown => Named::PageDown,
            NamedKey::ArrowUp => Named::Up,
            NamedKey::ArrowDown => Named::Down,
            NamedKey::ArrowLeft => Named::Left,
            NamedKey::ArrowRight => Named::Right,
            other => {
                return function_key(*other)
                    .map(Named::F)
                    .map(|f| Key::new(mods(m), Code::Named(f)));
            }
        }),
        _ => return None,
    };
    Some(Key::new(mods(m), code))
}

fn function_key(n: NamedKey) -> Option<u8> {
    use NamedKey::*;
    const F: [NamedKey; 35] = [
        F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, F13, F14, F15, F16, F17, F18, F19, F20,
        F21, F22, F23, F24, F25, F26, F27, F28, F29, F30, F31, F32, F33, F34, F35,
    ];
    F.iter().position(|f| *f == n).map(|i| i as u8 + 1)
}

pub fn button(b: MouseButton, m: ModifiersState) -> Option<Key> {
    let b = match b {
        MouseButton::Left => Button::Left,
        MouseButton::Right => Button::Right,
        MouseButton::Middle => Button::Middle,
        MouseButton::Back => Button::Back,
        MouseButton::Forward => Button::Forward,
        MouseButton::Other(_) => return None,
    };
    Some(Key::new(mods(m), Code::Mouse(b)))
}

pub fn scroll(dir: Scroll, m: ModifiersState) -> Key {
    Key::new(mods(m), Code::Scroll(dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use keys::KeySeq;

    fn k(s: &str) -> Key {
        s.parse::<KeySeq>().unwrap().0[0]
    }

    #[test]
    fn winit_keys_match_notation() {
        let none = ModifiersState::empty();
        let ch = |s: &str| WinitKey::Character(s.into());
        assert_eq!(key(&ch("n"), none), Some(k("n")));
        // Shift is already in the character.
        assert_eq!(key(&ch("G"), ModifiersState::SHIFT), Some(k("G")));
        assert_eq!(key(&ch("?"), ModifiersState::SHIFT), Some(k("?")));
        assert_eq!(key(&ch("x"), ModifiersState::CONTROL), Some(k("<C-x>")));
        assert_eq!(key(&ch(" "), none), Some(k("<Space>")));
        assert_eq!(
            key(&WinitKey::Named(NamedKey::Space), ModifiersState::CONTROL),
            Some(k("<C-Space>"))
        );
        assert_eq!(
            key(&WinitKey::Named(NamedKey::Tab), ModifiersState::SHIFT),
            Some(k("<S-Tab>"))
        );
        assert_eq!(key(&WinitKey::Named(NamedKey::F5), none), Some(k("<F5>")));
        assert_eq!(
            key(&WinitKey::Named(NamedKey::Shift), ModifiersState::SHIFT),
            None
        );
        assert_eq!(button(MouseButton::Right, none), Some(k("<RightMouse>")));
        assert_eq!(
            scroll(Scroll::Up, ModifiersState::CONTROL),
            k("<C-ScrollUp>")
        );
    }
}
