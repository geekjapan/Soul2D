#[derive(Clone, Copy)]
enum Color {
    Normal,
    Add,
    Glow,
    Darken,
    Multiply,
    Burn,
    LinearBurn,
    Lighten,
    Screen,
    Dodge,
    Overlay,
    SoftLight,
    HardLight,
    LinearLight,
    Hue,
    HslColor,
    LegacyAdd,
}

#[derive(Clone, Copy)]
enum Alpha {
    Over,
    Atop,
    Out,
    Conjoint,
    Disjoint,
}

#[derive(Clone, Copy)]
pub(crate) struct Mode {
    color: Color,
    alpha: Alpha,
}

impl Mode {
    /// The four modes an INX node names by itself. An INX node has no alpha rule, so each composites as Over.
    pub(crate) const LINEAR_BURN: Self = Self { color: Color::LinearBurn, alpha: Alpha::Over };
    pub(crate) const LINEAR_LIGHT: Self = Self { color: Color::LinearLight, alpha: Alpha::Over };
    pub(crate) const HUE: Self = Self { color: Color::Hue, alpha: Alpha::Over };
    pub(crate) const COLOR: Self = Self { color: Color::HslColor, alpha: Alpha::Over };

    pub(crate) fn parse(color: &str, alpha: &str) -> Result<Self, String> {
        let color = match color {
            "NORMAL" => Color::Normal,
            "ADD" => Color::LegacyAdd,
            "ADD_R2" => Color::Add,
            "ADD_GLOW" => Color::Glow,
            "DARKEN" => Color::Darken,
            "MULTIPLY_R2" => Color::Multiply,
            "COLOR_BURN" => Color::Burn,
            "LINEAR_BURN" => Color::LinearBurn,
            "LIGHTEN" => Color::Lighten,
            "SCREEN" => Color::Screen,
            "COLOR_DODGE" => Color::Dodge,
            "OVERLAY" => Color::Overlay,
            "SOFTLIGHT" => Color::SoftLight,
            "HARDLIGHT" => Color::HardLight,
            "LINEAR_LIGHT" => Color::LinearLight,
            "HSL_HUE" => Color::Hue,
            "HSL_COLOR" => Color::HslColor,
            other => return Err(format!("unsupported native color composition {other:?}")),
        };
        let alpha = match alpha {
            "OVER" => Alpha::Over,
            "ATOP" => Alpha::Atop,
            "OUT" => Alpha::Out,
            "CONJOINT" => Alpha::Conjoint,
            "DISJOINT" => Alpha::Disjoint,
            other => return Err(format!("unsupported native alpha composition {other:?}")),
        };
        Ok(Self { color, alpha })
    }

    pub(crate) fn blend(self, destination: [f32; 4], source: [f32; 4]) -> [f32; 4] {
        if matches!(self.color, Color::LegacyAdd) {
            return [
                (destination[0] + source[0]).clamp(0.0, 1.0),
                (destination[1] + source[1]).clamp(0.0, 1.0),
                (destination[2] + source[2]).clamp(0.0, 1.0),
                destination[3],
            ];
        }
        let straight = |rgba: [f32; 4]| -> [f32; 3] {
            if rgba[3].abs() < 1e-5 {
                [0.0; 3]
            } else {
                std::array::from_fn(|i| rgba[i] / rgba[3])
            }
        };
        let s = straight(source);
        let d = straight(destination);
        let color = match self.color {
            Color::Hue => {
                let low = s.into_iter().fold(f32::INFINITY, f32::min);
                let span = s.into_iter().fold(f32::NEG_INFINITY, f32::max) - low;
                let saturation = d.into_iter().fold(f32::NEG_INFINITY, f32::max)
                    - d.into_iter().fold(f32::INFINITY, f32::min);
                set_luma(
                    if span > 0.0 {
                        s.map(|v| (v - low) * saturation / span)
                    } else {
                        [0.0; 3]
                    },
                    luma(d),
                )
            }
            Color::HslColor => set_luma(s, luma(d)),
            _ => std::array::from_fn(|i| self.channel(s[i], d[i])),
        };
        let a = source[3];
        let b = destination[3];
        let (overlap, foreground, background) = match self.alpha {
            Alpha::Over => (a * b, a * (1.0 - b), b * (1.0 - a)),
            Alpha::Atop => (a * b, 0.0, b * (1.0 - a)),
            Alpha::Out => (0.0, 0.0, b * (1.0 - a)),
            Alpha::Conjoint => (a.min(b), (a - b).max(0.0), (b - a).max(0.0)),
            Alpha::Disjoint => ((a + b - 1.0).max(0.0), a.min(1.0 - b), b.min(1.0 - a)),
        };
        let rgb: [f32; 3] = std::array::from_fn(|i| {
            (color[i] * overlap + s[i] * foreground + d[i] * background).clamp(0.0, 1.0)
        });
        [
            rgb[0],
            rgb[1],
            rgb[2],
            (overlap + foreground + background).clamp(0.0, 1.0),
        ]
    }

    fn channel(self, s: f32, d: f32) -> f32 {
        match self.color {
            Color::Normal => s,
            Color::Add => (s + d).min(1.0),
            Color::Glow => s + d,
            Color::Darken => s.min(d),
            Color::Multiply => s * d,
            Color::Burn if (d - 1.0).abs() < 1e-6 => 1.0,
            Color::Burn if s.abs() < 1e-6 => 0.0,
            Color::Burn => 1.0 - ((1.0 - d) / s).min(1.0),
            Color::LinearBurn => (s + d - 1.0).max(0.0),
            Color::Lighten => s.max(d),
            Color::Screen => s + d - s * d,
            Color::Dodge if d <= 0.0 => 0.0,
            Color::Dodge if s == 1.0 => 1.0,
            Color::Dodge => (d / (1.0 - s)).min(1.0),
            Color::Overlay if d < 0.5 => 2.0 * s * d,
            Color::Overlay => 1.0 - 2.0 * (1.0 - s) * (1.0 - d),
            Color::HardLight if s < 0.5 => 2.0 * s * d,
            Color::HardLight => 1.0 - 2.0 * (1.0 - s) * (1.0 - d),
            Color::SoftLight if s <= 0.5 => d - (1.0 - 2.0 * s) * d * (1.0 - d),
            Color::SoftLight if d <= 0.25 => {
                d + (2.0 * s - 1.0) * d * ((16.0 * d - 12.0) * d + 3.0)
            }
            Color::SoftLight => d + (2.0 * s - 1.0) * (d.sqrt() - d),
            Color::LinearLight if s < 0.5 => (2.0 * s + d - 1.0).max(0.0),
            Color::LinearLight => (2.0 * (s - 0.5) + d).min(1.0),
            Color::Hue | Color::HslColor | Color::LegacyAdd => unreachable!(),
        }
    }
}

fn luma(c: [f32; 3]) -> f32 {
    0.30 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn set_luma(c: [f32; 3], target: f32) -> [f32; 3] {
    let shift = target - luma(c);
    let mut out = c.map(|v| v + shift);
    let low = out.into_iter().fold(f32::INFINITY, f32::min);
    let high = out.into_iter().fold(f32::NEG_INFINITY, f32::max);
    let level = luma(out);
    if low < 0.0 {
        out = out.map(|v| level + (v - level) * level / (level - low));
    }
    if high > 1.0 {
        out = out.map(|v| level + (v - level) * (1.0 - level) / (high - level));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: [f32; 4], expected: [f32; 4]) {
        for i in 0..4 {
            assert!(
                (actual[i] - expected[i]).abs() < 1e-6,
                "{actual:?} != {expected:?}"
            );
        }
    }

    #[test]
    fn multiply_with_each_alpha_rule_uses_straight_colors_and_overlap_weights() {
        for (alpha, expected) in [
            ("OVER", [0.195, 0.305, 0.345, 0.625]),
            ("ATOP", [0.095, 0.255, 0.32, 0.5]),
            ("OUT", [0.075, 0.225, 0.3, 0.375]),
            ("CONJOINT", [0.09, 0.21, 0.24, 0.5]),
            ("DISJOINT", [0.3, 0.4, 0.45, 0.75]),
        ] {
            close(
                Mode::parse("MULTIPLY_R2", alpha)
                    .unwrap()
                    .blend([0.1, 0.3, 0.4, 0.5], [0.2, 0.1, 0.05, 0.25]),
                expected,
            );
        }
    }

    #[test]
    fn hue_and_color_preserve_destination_luma_and_handle_gray() {
        close(
            Mode::parse("HSL_COLOR", "OVER")
                .unwrap()
                .blend([0.5, 0.5, 0.5, 1.0], [1.0, 0.0, 0.0, 1.0]),
            [1.0, 2.0 / 7.0, 2.0 / 7.0, 1.0],
        );
        close(
            Mode::parse("HSL_HUE", "OVER")
                .unwrap()
                .blend([0.5, 0.5, 0.5, 1.0], [0.2, 0.2, 0.2, 1.0]),
            [0.5, 0.5, 0.5, 1.0],
        );
    }

    #[test]
    fn legacy_add_retains_destination_alpha_and_unknown_tokens_fail() {
        // The Add of before 5.3 does not read the alpha rule: every rule gives the same pixel.
        for alpha in ["OVER", "ATOP", "OUT", "CONJOINT", "DISJOINT"] {
            close(
                Mode::parse("ADD", alpha)
                    .unwrap()
                    .blend([0.1, 0.3, 0.4, 0.5], [0.2, 0.1, 0.05, 0.25]),
                [0.3, 0.4, 0.45, 0.5],
            );
        }
        assert!(Mode::parse("ADD", "UNKNOWN").is_err());
        assert!(Mode::parse("UNKNOWN", "OVER").is_err());
        assert!(Mode::parse("NORMAL", "UNKNOWN").is_err());
    }

    #[test]
    fn color_modes_match_literal_opaque_and_translucent_cases() {
        for (color, expected) in [
            ("NORMAL", 0.75),
            ("ADD_R2", 1.0),
            ("ADD_GLOW", 1.0),
            ("DARKEN", 0.25),
            ("MULTIPLY_R2", 0.1875),
            ("COLOR_BURN", 0.0),
            ("LINEAR_BURN", 0.0),
            ("LIGHTEN", 0.75),
            ("SCREEN", 0.8125),
            ("COLOR_DODGE", 1.0),
            ("OVERLAY", 0.375),
            ("SOFTLIGHT", 0.375),
            ("HARDLIGHT", 0.625),
            ("LINEAR_LIGHT", 0.75),
            ("HSL_HUE", 0.25),
            ("HSL_COLOR", 0.25),
        ] {
            close(
                Mode::parse(color, "OVER")
                    .unwrap()
                    .blend([0.25, 0.25, 0.25, 1.0], [0.75, 0.75, 0.75, 1.0]),
                [expected, expected, expected, 1.0],
            );
        }
        for (color, expected) in [("ADD_R2", 0.65), ("ADD_GLOW", 0.8)] {
            close(
                Mode::parse(color, "OVER")
                    .unwrap()
                    .blend([0.4, 0.4, 0.4, 0.5], [0.4, 0.4, 0.4, 0.5]),
                [expected, expected, expected, 0.75],
            );
        }
    }

    #[test]
    fn linear_light_preserves_glow_destination_range() {
        let glowing = Mode::parse("ADD_GLOW", "OVER")
            .unwrap()
            .blend([0.5; 4], [0.5; 4]);
        close(glowing, [1.0, 1.0, 1.0, 0.75]);
        close(
            Mode::parse("LINEAR_LIGHT", "OVER")
                .unwrap()
                .blend(glowing, [0.2, 0.2, 0.2, 0.5]),
            [0.975, 0.975, 0.975, 0.875],
        );
    }

    #[test]
    fn zero_alpha_does_not_expose_stale_rgb() {
        for alpha in ["OVER", "ATOP", "OUT", "CONJOINT", "DISJOINT"] {
            let mode = Mode::parse("MULTIPLY_R2", alpha).unwrap();
            close(
                mode.blend([0.1, 0.3, 0.4, 0.5], [1.0, 1.0, 1.0, 0.0]),
                [0.1, 0.3, 0.4, 0.5],
            );
            let expected = if matches!(alpha, "ATOP" | "OUT") {
                [0.0; 4]
            } else {
                [0.2, 0.1, 0.05, 0.25]
            };
            close(
                mode.blend([1.0, 1.0, 1.0, 0.0], [0.2, 0.1, 0.05, 0.25]),
                expected,
            );
        }
    }
}
