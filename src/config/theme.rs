//! Colour tokens, presets, palette derivation and pywal import.

use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An sRGB colour with straight alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    pub const fn rgb(hex: u32) -> Self {
        Self([(hex >> 16) as u8, (hex >> 8) as u8, hex as u8, 255])
    }

    /// Mix `self` (share `t`) over `other` in sRGB space.
    pub fn mix_over(self, other: Rgba, t: f32) -> Rgba {
        let mut out = [0u8; 4];
        for (i, o) in out.iter_mut().enumerate() {
            let v = other.0[i] as f32 + (self.0[i] as f32 - other.0[i] as f32) * t;
            *o = v.round().clamp(0.0, 255.0) as u8;
        }
        Rgba(out)
    }

    /// Linear-light RGBA for the GPU.
    pub fn linear(self) -> [f32; 4] {
        let f = |c: u8| {
            let c = c as f32 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        [
            f(self.0[0]),
            f(self.0[1]),
            f(self.0[2]),
            self.0[3] as f32 / 255.0,
        ]
    }

    /// `#RGB`, `#RRGGBB` or `#RRGGBBAA`; the `#` is optional.
    pub fn parse(s: &str) -> Result<Rgba, String> {
        let hex = s.trim().strip_prefix('#').unwrap_or(s.trim());
        let bad = || format!("expected a colour like \"#D71921\", got \"{s}\"");
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(bad());
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| bad());
        match hex.len() {
            3 => {
                let n = |i: usize| u8::from_str_radix(&hex[i..=i], 16).map(|v| v * 17);
                Ok(Rgba([
                    n(0).map_err(|_| bad())?,
                    n(1).map_err(|_| bad())?,
                    n(2).map_err(|_| bad())?,
                    255,
                ]))
            }
            6 => Ok(Rgba([byte(0)?, byte(2)?, byte(4)?, 255])),
            8 => Ok(Rgba([byte(0)?, byte(2)?, byte(4)?, byte(6)?])),
            _ => Err(bad()),
        }
    }
}

impl std::fmt::Display for Rgba {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [r, g, b, a] = self.0;
        if a == 255 {
            write!(f, "#{r:02X}{g:02X}{b:02X}")
        } else {
            write!(f, "#{r:02X}{g:02X}{b:02X}{a:02X}")
        }
    }
}

impl<'de> Deserialize<'de> for Rgba {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Rgba::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl Serialize for Rgba {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

/// Generates the token struct, the optional overrides in `[theme]`, and per-token helpers.
macro_rules! tokens {
    ($($name:ident),* $(,)?) => {
        #[derive(Clone, Debug, PartialEq)]
        pub struct Theme {
            $(pub $name: Rgba,)*
        }

        #[derive(Clone, Debug, Default, PartialEq, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        pub struct ThemeConfig {
            pub preset: Preset,
            /// pywal's `colors.json`.
            pub pywal: Option<String>,
            $(pub $name: Option<Rgba>,)*
        }

        impl Theme {
            /// Every token mixed from `self` (t = 0) to `other` (t = 1).
            pub fn mix(&self, other: &Theme, t: f32) -> Theme {
                Theme {
                    $($name: other.$name.mix_over(self.$name, t),)*
                }
            }

            fn apply(&mut self, cfg: &ThemeConfig) {
                $(if let Some(c) = cfg.$name {
                    self.$name = c;
                })*
            }
        }
    };
}

tokens!(
    background,
    foreground,
    surface,
    surface_raised,
    border,
    border_visible,
    text_disabled,
    text_secondary,
    text_primary,
    text_display,
    accent,
    success,
    warning,
    image_background,
);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Preset {
    #[default]
    NothingDark,
    NothingLight,
}

impl Preset {
    /// Background, foreground, accent.
    fn colors(self) -> [Rgba; 3] {
        match self {
            Preset::NothingDark => [
                Rgba::rgb(0x000000),
                Rgba::rgb(0xFFFFFF),
                Rgba::rgb(0xD71921),
            ],
            Preset::NothingLight => [
                Rgba::rgb(0xF5F5F5),
                Rgba::rgb(0x000000),
                Rgba::rgb(0xD71921),
            ],
        }
    }
}

impl Theme {
    /// Derive every token from background, foreground and accent. The fg shares reproduce the
    /// Nothing dark palette exactly from black/white.
    pub fn derive(background: Rgba, foreground: Rgba, accent: Rgba) -> Self {
        let mix = |t: f32| foreground.mix_over(background, t);
        Self {
            background,
            foreground,
            surface: mix(0.067),
            surface_raised: mix(0.10),
            border: mix(0.133),
            border_visible: mix(0.20),
            text_disabled: mix(0.40),
            text_secondary: mix(0.60),
            text_primary: mix(0.91),
            text_display: foreground,
            accent,
            success: Rgba::rgb(0x4A9E5C),
            warning: Rgba::rgb(0xD4A843),
            image_background: background,
        }
    }

    pub fn nothing_dark() -> Self {
        let [bg, fg, accent] = Preset::NothingDark.colors();
        Self::derive(bg, fg, accent)
    }
}

/// Preset, then pywal, then explicit base colours; derive the rest, then explicit tokens win.
/// `image_background` follows the final background unless set.
pub fn resolve(cfg: &ThemeConfig, pywal: Option<[Rgba; 3]>) -> Theme {
    let [mut bg, mut fg, mut accent] = pywal.unwrap_or_else(|| cfg.preset.colors());
    bg = cfg.background.unwrap_or(bg);
    fg = cfg.foreground.unwrap_or(fg);
    accent = cfg.accent.unwrap_or(accent);
    let mut theme = Theme::derive(bg, fg, accent);
    theme.apply(cfg);
    theme
}

/// Background, foreground and accent from pywal's `colors.json`.
pub fn read_pywal(path: &Path) -> Result<[Rgba; 3], String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let json: serde_json::Value =
        serde_json::from_str(&src).map_err(|e| format!("{}: {e}", path.display()))?;
    let get = |a: &str, b: &str| -> Result<Rgba, String> {
        let s = json[a][b]
            .as_str()
            .ok_or_else(|| format!("{}: missing {a}.{b}", path.display()))?;
        Rgba::parse(s).map_err(|e| format!("{}: {a}.{b}: {e}", path.display()))
    };
    Ok([
        get("special", "background")?,
        get("special", "foreground")?,
        get("colors", "color1")?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_reproduces_nothing_dark() {
        let t = Theme::nothing_dark();
        assert_eq!(t.surface, Rgba::rgb(0x111111));
        assert_eq!(t.surface_raised, Rgba::rgb(0x1A1A1A));
        assert_eq!(t.border, Rgba::rgb(0x222222));
        assert_eq!(t.border_visible, Rgba::rgb(0x333333));
        assert_eq!(t.text_disabled, Rgba::rgb(0x666666));
        assert_eq!(t.text_secondary, Rgba::rgb(0x999999));
        assert_eq!(t.text_primary, Rgba::rgb(0xE8E8E8));
        assert_eq!(t.text_display, Rgba::rgb(0xFFFFFF));
    }

    #[test]
    fn linear_conversion() {
        assert_eq!(Rgba::rgb(0x000000).linear(), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(Rgba::rgb(0xFFFFFF).linear(), [1.0, 1.0, 1.0, 1.0]);
        let mid = Rgba::rgb(0x808080).linear()[0];
        assert!((mid - 0.2158).abs() < 1e-3);
    }

    #[test]
    fn hex_parsing() {
        assert_eq!(Rgba::parse("#D71921"), Ok(Rgba::rgb(0xD71921)));
        assert_eq!(Rgba::parse("d71921"), Ok(Rgba::rgb(0xD71921)));
        assert_eq!(Rgba::parse("#fff"), Ok(Rgba::rgb(0xFFFFFF)));
        assert_eq!(Rgba::parse("#00000080"), Ok(Rgba([0, 0, 0, 0x80])));
        assert!(Rgba::parse("#12345").is_err());
        assert!(Rgba::parse("#gggggg").is_err());
        assert!(Rgba::parse("#ÿÿÿ").is_err());
        assert_eq!(Rgba::rgb(0xD71921).to_string(), "#D71921");
    }

    #[test]
    fn resolution_order() {
        let mut cfg = ThemeConfig::default();
        assert_eq!(resolve(&cfg, None), Theme::nothing_dark());
        cfg.preset = Preset::NothingLight;
        let light = resolve(&cfg, None);
        assert_eq!(light.background, Rgba::rgb(0xF5F5F5));
        assert_eq!(light.image_background, Rgba::rgb(0xF5F5F5));
        assert_eq!(light.text_display, Rgba::rgb(0x000000));
        // pywal replaces the preset; explicit colours win over pywal.
        let wal = [
            Rgba::rgb(0x102030),
            Rgba::rgb(0xF0E0D0),
            Rgba::rgb(0x00FF00),
        ];
        cfg.accent = Some(Rgba::rgb(0x0000FF));
        cfg.border = Some(Rgba::rgb(0x123456));
        let t = resolve(&cfg, Some(wal));
        assert_eq!(t.background, Rgba::rgb(0x102030));
        assert_eq!(t.accent, Rgba::rgb(0x0000FF));
        assert_eq!(t.border, Rgba::rgb(0x123456));
        assert_eq!(
            t.surface,
            Rgba::rgb(0xF0E0D0).mix_over(Rgba::rgb(0x102030), 0.067)
        );
    }

    #[test]
    fn mixing_themes() {
        let a = Theme::nothing_dark();
        let b = resolve(
            &ThemeConfig {
                preset: Preset::NothingLight,
                ..Default::default()
            },
            None,
        );
        assert_eq!(a.mix(&b, 0.0), a);
        assert_eq!(a.mix(&b, 1.0), b);
        assert_eq!(a.mix(&b, 0.5).background, Rgba([123, 123, 123, 255]));
    }

    #[test]
    fn pywal_import() {
        let dir = std::env::temp_dir().join(format!("remage-wal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("colors.json");
        std::fs::write(
            &p,
            r##"{"special":{"background":"#0b0c10","foreground":"#c5c6c7","cursor":"#c5c6c7"},
                "colors":{"color0":"#0b0c10","color1":"#66fcf1"}}"##,
        )
        .unwrap();
        assert_eq!(
            read_pywal(&p),
            Ok([
                Rgba::rgb(0x0B0C10),
                Rgba::rgb(0xC5C6C7),
                Rgba::rgb(0x66FCF1)
            ])
        );
        std::fs::write(&p, "{}").unwrap();
        assert!(read_pywal(&p).unwrap_err().contains("special.background"));
    }
}
