//! Vim key notation: `n`, `G`, `<C-x>`, `<S-Tab>`, `<Space>w`, `gg`, mouse buttons and wheel.
//!
//! Keys are normalised so that the config and the keyboard agree: a character already carries
//! its shift state (`G`, `?`), so shift is dropped from character keys and `<S-g>` means `G`.

use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// The logo key, `<D-…>` as in vim.
    pub sup: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        ctrl: false,
        alt: false,
        shift: false,
        sup: false,
    };

    pub fn is_empty(self) -> bool {
        self == Mods::NONE
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Named {
    Space,
    Enter,
    Esc,
    Tab,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    F(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Button {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scroll {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Code {
    Char(char),
    Named(Named),
    Mouse(Button),
    Scroll(Scroll),
}

/// One key press (or click, or wheel notch) with its modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    pub mods: Mods,
    pub code: Code,
}

impl Key {
    pub fn new(mods: Mods, code: Code) -> Self {
        Self { mods, code }.normalized()
    }

    pub fn plain(c: char) -> Self {
        Self::new(Mods::NONE, Code::Char(c))
    }

    /// Characters carry their own shift state; a space is `<Space>`.
    fn normalized(mut self) -> Self {
        if let Code::Char(c) = self.code {
            if c == ' ' {
                self.code = Code::Named(Named::Space);
            } else if self.mods.shift {
                self.code = Code::Char(c.to_uppercase().next().unwrap_or(c));
                self.mods.shift = false;
            }
        }
        self
    }

    /// A digit typed without modifiers, which may start or extend a count.
    pub fn digit(self) -> Option<u32> {
        match self.code {
            Code::Char(c) if self.mods.is_empty() => c.to_digit(10),
            _ => None,
        }
    }
}

const NAMES: &[(&str, Code)] = &[
    ("Space", Code::Named(Named::Space)),
    ("CR", Code::Named(Named::Enter)),
    ("Enter", Code::Named(Named::Enter)),
    ("Return", Code::Named(Named::Enter)),
    ("Esc", Code::Named(Named::Esc)),
    ("Escape", Code::Named(Named::Esc)),
    ("Tab", Code::Named(Named::Tab)),
    ("BS", Code::Named(Named::Backspace)),
    ("Backspace", Code::Named(Named::Backspace)),
    ("Del", Code::Named(Named::Delete)),
    ("Delete", Code::Named(Named::Delete)),
    ("Insert", Code::Named(Named::Insert)),
    ("Home", Code::Named(Named::Home)),
    ("End", Code::Named(Named::End)),
    ("PageUp", Code::Named(Named::PageUp)),
    ("PageDown", Code::Named(Named::PageDown)),
    ("Up", Code::Named(Named::Up)),
    ("Down", Code::Named(Named::Down)),
    ("Left", Code::Named(Named::Left)),
    ("Right", Code::Named(Named::Right)),
    ("lt", Code::Char('<')),
    ("gt", Code::Char('>')),
    ("Bar", Code::Char('|')),
    ("Bslash", Code::Char('\\')),
    ("LeftMouse", Code::Mouse(Button::Left)),
    ("RightMouse", Code::Mouse(Button::Right)),
    ("MiddleMouse", Code::Mouse(Button::Middle)),
    ("X1Mouse", Code::Mouse(Button::Back)),
    ("X2Mouse", Code::Mouse(Button::Forward)),
    ("ScrollUp", Code::Scroll(Scroll::Up)),
    ("ScrollDown", Code::Scroll(Scroll::Down)),
    ("ScrollLeft", Code::Scroll(Scroll::Left)),
    ("ScrollRight", Code::Scroll(Scroll::Right)),
    ("ScrollWheelUp", Code::Scroll(Scroll::Up)),
    ("ScrollWheelDown", Code::Scroll(Scroll::Down)),
    ("ScrollWheelLeft", Code::Scroll(Scroll::Left)),
    ("ScrollWheelRight", Code::Scroll(Scroll::Right)),
];

fn lookup(name: &str) -> Option<Code> {
    if let Some(n) = name.strip_prefix(['F', 'f'])
        && let Ok(n) = n.parse::<u8>()
        && (1..=35).contains(&n)
    {
        return Some(Code::Named(Named::F(n)));
    }
    NAMES
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, c)| *c)
}

fn code_name(code: Code) -> Option<String> {
    let s = match code {
        Code::Char('<') => "lt",
        Code::Char(_) => return None,
        Code::Named(Named::F(n)) => return Some(format!("F{n}")),
        Code::Named(Named::Space) => "Space",
        Code::Named(Named::Enter) => "CR",
        Code::Named(Named::Esc) => "Esc",
        Code::Named(Named::Tab) => "Tab",
        Code::Named(Named::Backspace) => "BS",
        Code::Named(Named::Delete) => "Del",
        Code::Named(Named::Insert) => "Insert",
        Code::Named(Named::Home) => "Home",
        Code::Named(Named::End) => "End",
        Code::Named(Named::PageUp) => "PageUp",
        Code::Named(Named::PageDown) => "PageDown",
        Code::Named(Named::Up) => "Up",
        Code::Named(Named::Down) => "Down",
        Code::Named(Named::Left) => "Left",
        Code::Named(Named::Right) => "Right",
        Code::Mouse(Button::Left) => "LeftMouse",
        Code::Mouse(Button::Right) => "RightMouse",
        Code::Mouse(Button::Middle) => "MiddleMouse",
        Code::Mouse(Button::Back) => "X1Mouse",
        Code::Mouse(Button::Forward) => "X2Mouse",
        Code::Scroll(Scroll::Up) => "ScrollUp",
        Code::Scroll(Scroll::Down) => "ScrollDown",
        Code::Scroll(Scroll::Left) => "ScrollLeft",
        Code::Scroll(Scroll::Right) => "ScrollRight",
    };
    Some(s.into())
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.mods;
        let name = code_name(self.code);
        if m.is_empty()
            && let Code::Char(c) = self.code
            && name.is_none()
        {
            return write!(f, "{c}");
        }
        f.write_str("<")?;
        for (on, p) in [
            (m.ctrl, "C-"),
            (m.alt, "A-"),
            (m.shift, "S-"),
            (m.sup, "D-"),
        ] {
            if on {
                f.write_str(p)?;
            }
        }
        match (name, self.code) {
            (Some(n), _) => f.write_str(&n)?,
            (None, Code::Char(c)) => write!(f, "{c}")?,
            (None, _) => unreachable!("every non-char code has a name"),
        }
        f.write_str(">")
    }
}

/// Parse a `<…>` token at the start of `s`. Returns the key and the bytes consumed, or `None`
/// when `s` doesn't start with a well-formed token (a lone `<` is then a literal).
fn parse_bracket(s: &str) -> Result<Option<(Key, usize)>, String> {
    let Some(inner) = s.strip_prefix('<') else {
        return Ok(None);
    };
    let mut mods = Mods::NONE;
    let mut r = inner;
    loop {
        let mut it = r.chars();
        let (Some(m), Some('-'), Some(_)) = (it.next(), it.next(), it.next()) else {
            break;
        };
        match m.to_ascii_uppercase() {
            'C' => mods.ctrl = true,
            'A' | 'M' => mods.alt = true,
            'S' => mods.shift = true,
            'D' | 'T' => mods.sup = true,
            _ => break,
        }
        r = &r[2..];
    }
    let mut it = r.chars();
    let Some(first) = it.next() else {
        return Ok(None);
    };
    // A single character: `<C-x>`, `<C-->`.
    if it.next() == Some('>') {
        if mods.shift && !first.is_alphabetic() {
            return Err(format!(
                "`<S-{first}>`: shift only combines with letters and named keys; \
                 write the shifted character instead"
            ));
        }
        let used = 1 + (inner.len() - r.len()) + first.len_utf8() + 1;
        return Ok(Some((Key::new(mods, Code::Char(first)), used)));
    }
    let Some(end) = r.find('>') else {
        return Ok(None);
    };
    let name = &r[..end];
    if !name.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Ok(None);
    }
    let used = 1 + (inner.len() - r.len()) + end + 1;
    match lookup(name) {
        Some(code) => {
            if mods.shift && matches!(code, Code::Char(c) if !c.is_alphabetic()) {
                return Err(format!(
                    "`<S-{name}>`: shift only combines with letters and named keys"
                ));
            }
            Ok(Some((Key::new(mods, code), used)))
        }
        None if mods.is_empty() && !inner.starts_with(char::is_alphabetic) => Ok(None),
        None => Err(format!("unknown key `<{}>`", &inner[..used - 2])),
    }
}

/// A key sequence such as `gg` or `<Space>w`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeySeq(pub Vec<Key>);

impl FromStr for KeySeq {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let mut keys = Vec::new();
        let mut rest = s;
        while let Some(c) = rest.chars().next() {
            if c == '<'
                && let Some((key, used)) = parse_bracket(rest)?
            {
                keys.push(key);
                rest = &rest[used..];
                continue;
            }
            if c.is_control() {
                return Err(format!("control character in key `{}`", s.escape_debug()));
            }
            keys.push(Key::plain(c));
            rest = &rest[c.len_utf8()..];
        }
        if keys.is_empty() {
            return Err("empty key".into());
        }
        Ok(KeySeq(keys))
    }
}

impl fmt::Display for KeySeq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|k| write!(f, "{k}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq(s: &str) -> Vec<Key> {
        s.parse::<KeySeq>().unwrap().0
    }

    fn ctrl() -> Mods {
        Mods {
            ctrl: true,
            ..Mods::NONE
        }
    }

    #[test]
    fn plain_characters_and_sequences() {
        assert_eq!(seq("n"), [Key::plain('n')]);
        assert_eq!(seq("gg"), [Key::plain('g'), Key::plain('g')]);
        assert_eq!(seq("G"), [Key::plain('G')]);
        assert_eq!(seq("<"), [Key::plain('<')]);
        assert_eq!(seq("<>"), [Key::plain('<'), Key::plain('>')]);
        assert_eq!(seq("|"), [Key::plain('|')]);
        assert_eq!(seq(" "), [Key::new(Mods::NONE, Code::Named(Named::Space))]);
    }

    #[test]
    fn bracket_notation() {
        assert_eq!(seq("<C-x>"), [Key::new(ctrl(), Code::Char('x'))]);
        assert_eq!(seq("<c-X>"), [Key::new(ctrl(), Code::Char('X'))]);
        assert_eq!(seq("<C-->"), [Key::new(ctrl(), Code::Char('-'))]);
        let shift = Mods {
            shift: true,
            ..Mods::NONE
        };
        assert_eq!(seq("<S-Tab>"), [Key::new(shift, Code::Named(Named::Tab))]);
        assert_eq!(seq("<S-g>"), [Key::plain('G')]);
        assert_eq!(seq("<cr>"), seq("<Enter>"));
        assert_eq!(seq("<lt>"), [Key::plain('<')]);
        assert_eq!(
            seq("<F5>"),
            [Key::new(Mods::NONE, Code::Named(Named::F(5)))]
        );
        assert_eq!(
            seq("<Space>w"),
            [
                Key::new(Mods::NONE, Code::Named(Named::Space)),
                Key::plain('w')
            ]
        );
        assert_eq!(
            seq("<C-ScrollUp>"),
            [Key::new(ctrl(), Code::Scroll(Scroll::Up))]
        );
        assert_eq!(
            seq("<LeftMouse>"),
            [Key::new(Mods::NONE, Code::Mouse(Button::Left))]
        );
        assert_eq!(
            seq("<C-S-A-Up>"),
            [Key::new(
                Mods {
                    ctrl: true,
                    alt: true,
                    shift: true,
                    sup: false
                },
                Code::Named(Named::Up)
            )]
        );
    }

    #[test]
    fn errors() {
        assert!("".parse::<KeySeq>().is_err());
        assert!("<C-Bogus>".parse::<KeySeq>().is_err());
        assert!("<Bogus>".parse::<KeySeq>().is_err());
        assert!("<S-1>".parse::<KeySeq>().is_err());
    }

    #[test]
    fn display_round_trips() {
        for s in [
            "n",
            "gg",
            "<C-x>",
            "<S-Tab>",
            "<Space>w",
            "<lt>",
            "<C-Space>",
            "<C-ScrollUp>",
            "<F12>",
            "<CR>",
            "|",
            "<C-->",
            "<A-D-x>",
        ] {
            let k: KeySeq = s.parse().unwrap();
            assert_eq!(k.to_string(), s);
            assert_eq!(k.to_string().parse::<KeySeq>().unwrap(), k);
        }
        assert_eq!("<S-a>".parse::<KeySeq>().unwrap().to_string(), "A");
        assert_eq!("<escape>".parse::<KeySeq>().unwrap().to_string(), "<Esc>");
    }

    #[test]
    fn digits() {
        assert_eq!(Key::plain('5').digit(), Some(5));
        assert_eq!(Key::new(ctrl(), Code::Char('5')).digit(), None);
        assert_eq!(Key::plain('x').digit(), None);
    }
}
