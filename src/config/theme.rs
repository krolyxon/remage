//! Colour tokens, presets and palette derivation.

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
}

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub background: Rgba,
    pub foreground: Rgba,
    pub surface: Rgba,
    pub surface_raised: Rgba,
    pub border: Rgba,
    pub border_visible: Rgba,
    pub text_disabled: Rgba,
    pub text_secondary: Rgba,
    pub text_primary: Rgba,
    pub text_display: Rgba,
    pub accent: Rgba,
    pub success: Rgba,
    pub warning: Rgba,
    pub image_background: Rgba,
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
        Self::derive(
            Rgba::rgb(0x000000),
            Rgba::rgb(0xFFFFFF),
            Rgba::rgb(0xD71921),
        )
    }
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
}
