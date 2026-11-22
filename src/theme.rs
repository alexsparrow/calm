//! Muted, organic palette. Each calendar gets a hue; fills, edges and text are
//! derived per theme so labels always sit on a tint of their own colour.

use gpui::{Hsla, hsla};

#[derive(Clone, Copy)]
pub struct CalColors {
    pub fill: Hsla,
    pub edge: Hsla,
    pub text: Hsla,
    pub dot: Hsla,
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub dark: bool,
    pub bg: Hsla,
    pub weekend: Hsla,
    pub surface: Hsla,
    pub border: Hsla,
    pub ink: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    pub hairline: Hsla,
    pub now: Hsla,
    pub shadow: Hsla,
    /// Empty aggregate cell; the start of every sequential ramp.
    pub cell: Hsla,
    /// Personal energy curve.
    pub energy: Hsla,
    /// The top and bottom bars; transparent (just the background) unless a theme sets it.
    pub chrome: Hsla,
    /// Per calendar slot; see `with_calendars`.
    pub cals: [CalColors; MAX_CALENDARS],
}

use crate::model::{Calendar, MAX_CALENDARS, PALETTE};

/// A calendar's fill, edge, text and dot from its hue, tuned per theme.
fn cal_colors(hue: f32, s: f32, dark: bool) -> CalColors {
    if dark {
        CalColors { fill: h(hue, s * 0.7, 0.29, 1.), edge: h(hue, s + 0.05, 0.60, 1.), text: h(hue, s * 0.6, 0.88, 1.), dot: h(hue, s + 0.1, 0.62, 1.) }
    } else {
        CalColors { fill: h(hue, s, 0.87, 1.), edge: h(hue, s + 0.05, 0.52, 1.), text: h(hue, s * 0.9, 0.22, 1.), dot: h(hue, s + 0.1, 0.55, 1.) }
    }
}

fn palette_cals(dark: bool) -> [CalColors; MAX_CALENDARS] {
    std::array::from_fn(|i| cal_colors(PALETTE[i % PALETTE.len()].1, PALETTE[i % PALETTE.len()].2, dark))
}

fn h(deg: f32, s: f32, l: f32, a: f32) -> Hsla {
    hsla(deg / 360., s, l, a)
}

impl Theme {
    pub fn light() -> Self {
        Self {
            dark: false,
            bg: h(40., 0.30, 0.965, 1.),
            weekend: h(35., 0.20, 0.93, 0.55),
            surface: h(40., 0.35, 0.99, 1.),
            border: h(30., 0.15, 0.20, 0.09),
            ink: h(30., 0.12, 0.17, 1.),
            muted: h(30., 0.08, 0.40, 1.),
            faint: h(30., 0.08, 0.55, 1.),
            hairline: h(30., 0.15, 0.25, 0.07),
            now: h(8., 0.62, 0.55, 1.),
            shadow: h(30., 0.30, 0.15, 0.10),
            energy: h(36., 0.62, 0.52, 1.),
            chrome: h(0., 0., 0., 0.),
            cell: h(40., 0.18, 0.915, 1.),
            cals: palette_cals(false),
        }
    }

    pub fn dark() -> Self {
        Self {
            dark: true,
            bg: h(30., 0.08, 0.095, 1.),
            weekend: h(30., 0.08, 0.13, 0.6),
            surface: h(30., 0.08, 0.14, 1.),
            border: h(30., 0.10, 0.90, 0.09),
            ink: h(35., 0.20, 0.90, 1.),
            muted: h(35., 0.10, 0.65, 1.),
            faint: h(35., 0.08, 0.48, 1.),
            hairline: h(35., 0.10, 0.90, 0.06),
            now: h(10., 0.70, 0.62, 1.),
            shadow: h(0., 0., 0., 0.45),
            energy: h(38., 0.62, 0.62, 1.),
            chrome: h(0., 0., 0., 0.),
            cell: h(30., 0.07, 0.155, 1.),
            cals: palette_cals(true),
        }
    }
}

pub fn with_alpha(c: Hsla, a: f32) -> Hsla {
    Hsla { a: c.a * a, ..c }
}

impl Theme {
    /// Colours the calendar slots from the configured calendars.
    pub fn with_calendars(mut self, cals: &[Calendar]) -> Self {
        for (slot, c) in self.cals.iter_mut().zip(cals) {
            *slot = cal_colors(c.hue, c.sat, self.dark);
        }
        self
    }
}

impl Theme {
    /// Lets the desktop show through the canvas; panels stay nearly opaque
    /// so text on them keeps its contrast.
    pub fn translucent(self, opacity: f32) -> Self {
        Self {
            bg: with_alpha(self.bg, opacity),
            weekend: with_alpha(self.weekend, opacity),
            // Day boxes let a little through too, staying a touch more solid than the canvas.
            cell: with_alpha(self.cell, (opacity + 0.08).min(1.)),
            // Bars stay a touch more solid than the canvas, like the day boxes.
            chrome: with_alpha(self.chrome, (opacity + 0.08).min(1.)),
            ..self
        }
    }
}

/// Theme colours a `colors` block can set, by name.
pub const TOKENS: &[&str] = &["bg", "chrome", "surface", "border", "ink", "muted", "faint", "hairline", "now", "weekend", "cell", "energy", "shadow"];

impl Theme {
    fn token(&mut self, name: &str) -> Option<&mut Hsla> {
        Some(match name {
            "bg" => &mut self.bg,
            "chrome" => &mut self.chrome,
            "surface" => &mut self.surface,
            "border" => &mut self.border,
            "ink" => &mut self.ink,
            "muted" => &mut self.muted,
            "faint" => &mut self.faint,
            "hairline" => &mut self.hairline,
            "now" => &mut self.now,
            "weekend" => &mut self.weekend,
            "cell" => &mut self.cell,
            "energy" => &mut self.energy,
            "shadow" => &mut self.shadow,
            _ => return None,
        })
    }
}

/// `#rrggbb` or `#rrggbbaa`.
pub fn parse_hex(s: &str) -> Option<Hsla> {
    let hex = s.strip_prefix('#')?;
    if !matches!(hex.len(), 6 | 8) {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    let (rgb, a) = if hex.len() == 8 { (v >> 8, (v & 255) as f32 / 255.) } else { (v, 1.) };
    let c = gpui::Rgba { r: ((rgb >> 16) & 255) as f32 / 255., g: ((rgb >> 8) & 255) as f32 / 255., b: (rgb & 255) as f32 / 255., a };
    Some(c.into())
}

/// A named set of colour overrides on top of the light or dark theme.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeDecl {
    pub name: String,
    pub dark: bool,
    pub colors: Vec<(String, Hsla)>,
}

/// Which themes to use, from calm.kdl's `theme` and `colors`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Looks {
    /// `system` (default), `light`, `dark`, or a declared theme's name.
    pub choice: Option<String>,
    /// The themes `system` (and `D`) switch between; default `light` and `dark`.
    pub light: Option<String>,
    pub dark: Option<String>,
    pub themes: Vec<ThemeDecl>,
}

impl Looks {
    fn decl(&self, name: &str) -> Option<&ThemeDecl> {
        self.themes.iter().find(|t| t.name.eq_ignore_ascii_case(name))
    }

    /// Light or dark, when the choice settles it rather than the system.
    pub fn forced(&self) -> Option<bool> {
        match self.choice.as_deref() {
            None | Some("system") => None,
            Some("light") => Some(false),
            Some("dark") => Some(true),
            Some(name) => self.decl(name).map(|t| t.dark),
        }
    }

    /// The theme for a light or dark look.
    pub fn build(&self, dark: bool) -> Theme {
        let chosen = self.choice.as_deref().filter(|c| !["system", "light", "dark"].contains(c)).filter(|c| self.decl(c).is_some_and(|t| t.dark == dark));
        let name = chosen.or(if dark { self.dark.as_deref() } else { self.light.as_deref() }).unwrap_or(if dark { "dark" } else { "light" });
        let mut theme = if dark { Theme::dark() } else { Theme::light() };
        // A custom theme starts from its base, including any tweaks to that base.
        if !["light", "dark"].contains(&name)
            && let Some(base) = self.decl(if dark { "dark" } else { "light" })
        {
            theme.apply(base);
        }
        if let Some(t) = self.decl(name) {
            theme.apply(t);
        }
        theme
    }
}

impl Theme {
    fn apply(&mut self, t: &ThemeDecl) {
        for (token, color) in &t.colors {
            if let Some(slot) = self.token(token) {
                *slot = *color;
            }
        }
    }
}
