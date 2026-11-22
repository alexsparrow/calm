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
    pub cals: [CalColors; 4],
}

const HUES: [(f32, f32); 4] = [(150., 0.22), (18., 0.48), (215., 0.30), (40., 0.55)];

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
            cell: h(40., 0.18, 0.915, 1.),
            cals: HUES.map(|(hue, s)| CalColors {
                fill: h(hue, s, 0.87, 1.),
                edge: h(hue, s + 0.05, 0.52, 1.),
                text: h(hue, s * 0.9, 0.22, 1.),
                dot: h(hue, s + 0.1, 0.55, 1.),
            }),
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
            cell: h(30., 0.07, 0.155, 1.),
            cals: HUES.map(|(hue, s)| CalColors {
                fill: h(hue, s * 0.7, 0.29, 1.),
                edge: h(hue, s + 0.05, 0.60, 1.),
                text: h(hue, s * 0.6, 0.88, 1.),
                dot: h(hue, s + 0.1, 0.62, 1.),
            }),
        }
    }
}

pub fn with_alpha(c: Hsla, a: f32) -> Hsla {
    Hsla { a: c.a * a, ..c }
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
            ..self
        }
    }
}
