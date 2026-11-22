//! Turns lens state into draw commands, and paints them.
//!
//! Each mark's live target shape comes from its inner geometry mapped through
//! its day cell. During a morph the drawn shape interpolates, in screen
//! space, from where the mark was last drawn to that live target. Furniture
//! (gridlines, labels, aggregate cells) is drawn per visible cell through each
//! lens's own arrangement and fades with the lens and its level of detail.

use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, PI, TAU};

use chrono::Datelike;
use gpui::{App, BorderStyle, BoxShadow, Bounds, ContentMask, Hsla, PathBuilder, ShapedLine, SharedString, Window, point, px, quad, radians};

use super::CalmView;
use super::Hit;
use super::arrange::{Arr, Pt, Rect};
use crate::color::{self, ColorEnc, Measure};
use crate::layout::{Kind, LabelMode, Target, U0, U1, V0, V1};
use crate::model::{self, Status};
use crate::spec::{Axis, Coord, Detail, Dir, Mark};
use crate::text::{self, fit, shape};
use crate::theme::{CalColors, with_alpha};

pub const MARGIN_R: f32 = 24.;
/// Width set aside for the energy rail beside each day's time axis.
pub const RAIL_W: f32 = 10.;
/// Height of one row of all-day bars.
pub const BAND_ROW: f32 = 17.;
pub const MARGIN_T: f32 = 128.;
/// Width of each extra time-zone column of hour labels, and height of each extra row.
pub const ZONE_COL: f32 = 50.;
pub const ZONE_ROW: f32 = 16.;
pub const DAY_NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const ROW_TOP: f32 = 22.;
const ROW_H: f32 = 17.;

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0., 1.);
    t * t * (3. - 2. * t)
}

fn overlaps(a: &Rect, b: &Rect) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

pub fn hour_label(h: i32) -> String {
    match h.rem_euclid(24) {
        0 => "12 am".into(),
        12 => "noon".into(),
        h if h < 12 => format!("{h} am"),
        h => format!("{} pm", h - 12),
    }
}

pub fn fmt_time(min: f32) -> String {
    let m = min.round() as i32;
    let (h, mm) = (m / 60 % 24, m % 60);
    let (h12, ap) = (if h % 12 == 0 { 12 } else { h % 12 }, if h < 12 { "am" } else { "pm" });
    if mm == 0 { format!("{h12} {ap}") } else { format!("{h12}:{mm:02} {ap}") }
}

pub fn fmt_range(start: f32, end: f32) -> String {
    let (a, b) = (fmt_time(start), fmt_time(end));
    match (a.rsplit_once(' '), b.rsplit_once(' ')) {
        (Some((a0, ap)), Some((_, bp))) if ap == bp => format!("{a0} – {b}"),
        _ => format!("{a} – {b}"),
    }
}

/// "8:15a" / "6p": times for tight spaces.
pub fn compact_time(min: f32) -> String {
    let m = min.round() as i32;
    let (h, mm) = (m / 60 % 24, m % 60);
    let (h12, ap) = (if h % 12 == 0 { 12 } else { h % 12 }, if h < 12 { "a" } else { "p" });
    if mm == 0 { format!("{h12}{ap}") } else { format!("{h12}:{mm:02}{ap}") }
}

/// A mark's screen geometry: a rectangle, or a strip whose first half is one
/// edge and second half the other edge reversed (annular sectors, morphs).
#[derive(Clone, Debug)]
pub enum Shape {
    Rect(Rect),
    Strip(Vec<Pt>),
}

impl Shape {
    fn strip(&self, k: usize) -> Vec<Pt> {
        match self {
            Shape::Strip(pts) if pts.len() == 2 * k => pts.clone(),
            Shape::Strip(pts) => {
                // Resample each edge to k points.
                let n = pts.len() / 2;
                let edge = |e: &[Pt]| -> Vec<Pt> {
                    (0..k)
                        .map(|i| {
                            let f = i as f32 / (k - 1) as f32 * (e.len() - 1) as f32;
                            let (j, t) = (f.floor() as usize, f.fract());
                            let (a, b) = (e[j], e[(j + 1).min(e.len() - 1)]);
                            (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
                        })
                        .collect()
                };
                let mut out = edge(&pts[..n]);
                out.extend(edge(&pts[n..]));
                out
            }
            Shape::Rect(r) => {
                let mut out: Vec<Pt> = (0..k).map(|i| (r[0] + (r[2] - r[0]) * i as f32 / (k - 1) as f32, r[1])).collect();
                out.extend((0..k).rev().map(|i| (r[0] + (r[2] - r[0]) * i as f32 / (k - 1) as f32, r[3])));
                out
            }
        }
    }

    fn lerp(&self, to: &Shape, t: f32) -> Shape {
        match (self, to) {
            (Shape::Rect(a), Shape::Rect(b)) => Shape::Rect([0, 1, 2, 3].map(|i| a[i] + (b[i] - a[i]) * t)),
            _ => {
                let k = match (self, to) {
                    (Shape::Strip(a), Shape::Strip(b)) => (a.len().max(b.len()) / 2).max(2),
                    (Shape::Strip(p), _) | (_, Shape::Strip(p)) => (p.len() / 2).max(2),
                    _ => 2,
                };
                let (a, b) = (self.strip(k), to.strip(k));
                Shape::Strip(a.iter().zip(&b).map(|(p, q)| (p.0 + (q.0 - p.0) * t, p.1 + (q.1 - p.1) * t)).collect())
            }
        }
    }

    pub fn bbox(&self) -> Rect {
        match self {
            Shape::Rect(r) => *r,
            Shape::Strip(pts) => {
                let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
                for &(x, y) in pts {
                    b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
                }
                b
            }
        }
    }
}

pub enum Cmd {
    Quad { b: Rect, r: f32, color: Hsla, border: Option<(Hsla, f32, bool)> },
    /// Per-corner radii: top-left, top-right, bottom-right, bottom-left.
    Corners { b: Rect, radii: [f32; 4], color: Hsla },
    Shadow { b: Rect, r: f32, color: Hsla },
    Strip { pts: Vec<Pt>, color: Hsla },
    Stroke { pts: Vec<Pt>, closed: bool, width: f32, color: Hsla },
    Line { line: ShapedLine, at: Pt, size: f32, clip: Option<Rect> },
    ArcText { line: ShapedLine, color: Hsla, c: Pt, r: f32, mid: f32, flip: bool, size: f32 },
}

#[derive(Default)]
pub struct Group {
    pub clip: Option<Rect>,
    /// Paint as one gpui layer: a single draw order, no per-primitive BoundsTree insert.
    pub layer: bool,
    pub cmds: Vec<Cmd>,
}

pub struct Frame {
    pub groups: [Group; 7],
}
const BACK: usize = 0;
const MARKS: usize = 1;
const FRONT: usize = 2;
const LABELS: usize = 3;
const TOP: usize = 4;
const LEFT: usize = 5;
const FREE: usize = 6;

impl Frame {
    fn new(plot: [f32; 4]) -> Self {
        let [x, y, w, h] = plot;
        let clip = [x - 2., y - 2., x + w + 2., y + h + 2.];
        let g = |clip, layer| Group { clip, layer, cmds: Vec::new() };
        Self {
            groups: [
                g(Some(clip), false),
                g(Some(clip), true),
                g(Some(clip), false),
                g(Some(clip), true),
                g(Some([x - 2., y - 140., x + w + 2., y]), false),
                g(Some([0., y - 12., x - 4., y + h]), false),
                g(None, false),
            ],
        }
    }

    fn push(&mut self, group: usize, cmd: Cmd) {
        self.groups[group].cmds.push(cmd);
    }
}

pub struct Built {
    pub frame: Frame,
    pub counts: (usize, usize, usize),
    /// Label placement changed a spring target; another frame is needed.
    pub relabel: bool,
}

impl CalmView {
    /// An event's colours under the current encoding, mid-crossfade if one is running.
    fn mark_colors(&self, e: &model::Event) -> CalColors {
        let th = &self.theme;
        let base = th.cals[e.calendar];
        let to = color::event_colors(&self.enc_to, e, th).unwrap_or(base);
        let t = self.enc_t.x.clamp(0., 1.);
        if t >= 1. {
            return to;
        }
        let from = color::event_colors(&self.enc_from, e, th).unwrap_or(base);
        color::mix_cal(&from, &to, t)
    }

    /// The live screen shape of a target in lens `p`.
    fn target_shape(&self, p: usize, arr: &Arr, t: &Target) -> Shape {
        let cell = arr.cell_inset(t.day);
        let inner = self.inner(p, cell);
        let g = t.g;
        match t.kind {
            Kind::Cell => Shape::Rect(cell),
            Kind::AllDay { row, prev, next } => {
                // Joined to the neighbouring days' pieces where the event continues.
                // Only where the next day sits alongside (lines running across) do pieces join up.
                let across = self.views[p].facet.dir == Dir::Rows && self.views[p].facet.per_line > 1;
                let (prev, next) = (prev && across, next && across);
                let (x0, x1) = (if prev { cell[0] - 3. } else { cell[0] + 2. }, if next { cell[2] + 3. } else { cell[2] - 2. });
                if inner.coord == Coord::Polar {
                    // A thin ring just inside the dial's centre, one per stacked event.
                    let (r_in, _) = inner.radii();
                    let r = (r_in - 5. - row as f32 * 6.).max(3.);
                    let (cx, cy) = inner.center();
                    let n = 72;
                    let ring = |rr: f32, k: usize| {
                        let a = k as f32 / n as f32 * TAU;
                        (cx + rr * a.cos(), cy + rr * a.sin())
                    };
                    let mut pts: Vec<Pt> = (0..=n).map(|k| ring(r + 2., k)).collect();
                    pts.extend((0..=n).rev().map(|k| ring(r - 2., k)));
                    Shape::Strip(pts)
                } else if self.lenses[p].mark == Mark::Heat {
                    // A stripe along the top of the aggregate cell.
                    let y = cell[1] + 2. + row as f32 * 4.;
                    Shape::Rect([x0, y, x1, y + 3.])
                } else {
                    // A bar in the band above the time axis.
                    let top = inner.cell[1] - self.band_rows(p, t.day) as f32 * BAND_ROW - 3. + row as f32 * BAND_ROW;
                    Shape::Rect([x0, top, x1, top + BAND_ROW - 2.])
                }
            }
            Kind::Row { rank } => {
                let y0 = cell[1] + ROW_TOP + rank as f32 * ROW_H;
                Shape::Rect([cell[0] + 3., y0, cell[2] - 3., y0 + ROW_H - 2.])
            }
            Kind::Rect if inner.coord == Coord::Polar => {
                let span = (inner.nu(g[U1]) - inner.nu(g[U0])).abs();
                let n = ((span * 120.).ceil() as usize).clamp(2, 96);
                let mut pts = Vec::with_capacity(2 * n + 2);
                for k in 0..=n {
                    pts.push(inner.map(g[U0] + (g[U1] - g[U0]) * k as f32 / n as f32, g[V0]));
                }
                for k in (0..=n).rev() {
                    pts.push(inner.map(g[U0] + (g[U1] - g[U0]) * k as f32 / n as f32, g[V1]));
                }
                Shape::Strip(pts)
            }
            Kind::Rect => {
                let (x0, y0) = inner.map(g[U0], g[V0]);
                let (x1, y1) = inner.map(g[U1], g[V1]);
                let mut r = [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)];
                // Hairline gaps between stacked blocks and side-by-side lanes.
                let time_y = self.views[p].y == Axis::TimeOfDay;
                if time_y && r[3] - r[1] > 4. {
                    r[3] -= 1.5;
                }
                if r[2] - r[0] > 8. && g[U1] < 0.999 {
                    r[2] -= 2.;
                }
                if g[U0] > 0.001 {
                    r[0] += 1.;
                }
                Shape::Rect(clip_to(r, inner.cell))
            }
            Kind::Pill { row, rows } if inner.coord == Coord::Polar => {
                // A short arc at the pill's radius, as thick as its straight version would be.
                let (r_in, r_out) = inner.radii();
                let (r0, r1) = (inner.radius(g[V0]), inner.radius(g[V1]));
                let (mid, h) = if (g[V1] - g[V0]).abs() < 1e-6 {
                    (r0, ((r_out - r_in) * 0.07).clamp(3., 18.))
                } else {
                    let lane = (r1 - r0).abs();
                    let rows = rows.max(1) as f32;
                    let h = (lane / rows * 0.8).clamp(2., 20.);
                    let gap = (h * 0.25).min(6.);
                    let stack = rows * h + (rows - 1.) * gap;
                    ((r0 + r1) * 0.5 - stack * 0.5 + row as f32 * (h + gap) + h * 0.5, h)
                };
                let (cx, cy) = inner.center();
                let (a0, mut a1) = (inner.angle(g[U0]), inner.angle(g[U1]));
                // At least a few pixels long, so short events stay visible.
                a1 = a1.max(a0 + 3. / mid.max(1.));
                let n = (((a1 - a0) * mid / 4.).ceil() as usize).clamp(2, 96);
                let at = |r: f32, k: usize| {
                    let t = a0 + (a1 - a0) * k as f32 / n as f32;
                    (cx + r * t.cos(), cy + r * t.sin())
                };
                let mut pts: Vec<Pt> = (0..=n).map(|k| at(mid - h * 0.5, k)).collect();
                pts.extend((0..=n).rev().map(|k| at(mid + h * 0.5, k)));
                Shape::Strip(pts)
            }
            Kind::Pill { row, rows } => {
                let (x0, _) = inner.map(g[U0], 0.);
                let (x1, _) = inner.map(g[U1], 0.);
                let (_, vy0) = inner.map(0., g[V0]);
                let (_, vy1) = inner.map(0., g[V1]);
                let ch = cell[3] - cell[1];
                let (top, h) = if (g[V1] - g[V0]).abs() < 1e-6 {
                    let h = (ch * 0.07).clamp(3., 22.);
                    (vy0 - h * 0.5, h)
                } else {
                    let lane = vy1 - vy0;
                    let rows = rows.max(1) as f32;
                    let h = (lane / rows * 0.8).clamp(2., 24.);
                    let gap = (h * 0.25).min(6.);
                    let stack = rows * h + (rows - 1.) * gap;
                    ((vy0 + vy1) * 0.5 - stack * 0.5 + row as f32 * (h + gap), h)
                };
                // Clipped to the cell: outside the hour window, nothing is left to draw.
                let (cx0, cx1) = (x0.max(inner.cell[0]), x1.max(x0 + 2.).min(inner.cell[2]));
                Shape::Rect([cx0, top, cx1.max(cx0), top + h])
            }
        }
    }

    pub(super) fn build(&mut self, window: &Window) -> Built {
        let th = self.theme;
        let fonts = self.fonts.clone();
        let p = self.preset;
        let arr = self.arr();
        let mut frame = Frame::new(arr.plot);
        self.zone_hits.borrow_mut().clear();
        let mut hits = Vec::new();

        for li in 0..self.views.len() {
            let a = self.layers[li].x.clamp(0., 1.);
            if a > 0.01 && !skip("decor") {
                self.decorate(li, a, window, &mut frame);
            }
        }

        // Candidates: events on visible days, plus anything mid-morph.
        let mut cand: Vec<usize> = if skip("marks") { Vec::new() } else { arr.visible().flat_map(|d| self.data.on_day(d)).collect() };
        let vis = arr.visible();
        cand.extend(self.flipping.iter().copied().filter(|&i| !vis.contains(&self.marks[i].target.day)));

        let mut boxes: Vec<(usize, Rect, f32)> = Vec::with_capacity(cand.len());
        let mut shown: Vec<(usize, Shape)> = Vec::with_capacity(cand.len());
        let mut overflow: HashMap<i32, u32> = HashMap::new();
        let (mut quads, mut strips) = (0, 0);
        let energy_lens = self.views[p].y == Axis::Demand;

        for &i in &cand {
            let m = &self.marks[i];
            let alpha = m.alpha.x.clamp(0., 1.);
            // Marks fading in must be drawn (and so stepped) even while still invisible.
            if alpha < 0.01 && m.alpha.target < 0.01 {
                continue;
            }
            let target = self.target_shape(p, &arr, &m.target);
            let t = if m.from.is_some() { m.t.x } else { 1. };
            let shape = match &m.from {
                Some(from) if t < 0.999 => from.lerp(&target, t),
                _ => target,
            };
            if let (Kind::Row { .. }, None) = (m.target.kind, &m.from) {
                let cell = arr.cell_inset(m.target.day);
                if shape.bbox()[3] > cell[3] - 2. {
                    *overflow.entry(m.target.day).or_default() += 1;
                    continue;
                }
            }
            let b = shape.bbox();
            // Clipped away (wholly outside the hour window), or off screen.
            if !arr.on_screen(&b) || b[2] - b[0] < 0.5 || b[3] - b[1] < 0.5 {
                continue;
            }
            let e = &self.data.events[i];
            let c = self.mark_colors(e);
            let hover = m.hover.x.clamp(0., 1.);
            let selected = self.selected.is_some_and(|s| self.data.events[s].group == e.group);
            let tentative = e.status == Status::Tentative;
            let mut fill = c.fill.blend(with_alpha(c.edge, 0.18 * hover));
            // Stripes over aggregate cells need the strong colour to show at all.
            if matches!(m.target.kind, Kind::AllDay { .. }) && self.lenses[p].mark == Mark::Heat {
                fill = c.dot.blend(with_alpha(th.ink, 0.15 * hover));
            }
            fill.a = alpha * if tentative { 0.75 } else { 1. };
            // Queued changes: dimmed until confirmed; outlined when they need you.
            use crate::sync::outbox::State as Sync;
            let unsent = matches!(e.sync, Some(Sync::Draft | Sync::Pending | Sync::Sending));
            if unsent {
                fill.a *= 0.7;
            }
            let attention = match e.sync {
                Some(Sync::Failed) => Some(th.now),
                Some(Sync::Conflict) => Some(gpui::hsla(38. / 360., 0.85, 0.52, 1.)),
                _ => None,
            };
            let (w, h) = (b[2] - b[0], b[3] - b[1]);
            match &shape {
                Shape::Rect(r) => {
                    let radius = match m.target.kind {
                        // Square where a multi-day bar carries on, so the pieces read as one.
                        Kind::AllDay { .. } if self.lenses[p].mark == Mark::Heat => 1.5,
                        Kind::AllDay { prev, next, .. } => if (prev || next) && self.views[p].facet.dir == Dir::Rows && self.views[p].facet.per_line > 1 { 0. } else { 4f32.min(h * 0.5) },
                        Kind::Pill { .. } => (h * 0.5).min(w * 0.5),
                        Kind::Row { .. } => 4f32.min(h * 0.5),
                        _ => 6f32.min(w * 0.5).min(h * 0.5),
                    }
                    .max(0.);
                    if hover > 0.01 {
                        frame.push(MARKS, Cmd::Shadow { b: *r, r: radius, color: with_alpha(th.shadow, hover * alpha) });
                    }
                    let border = if let Some(c) = attention {
                        Some((with_alpha(c, alpha), 1.75, false))
                    } else if m.target.conflict {
                        Some((with_alpha(th.now, 0.8 * alpha), 1.25, false))
                    } else if selected {
                        Some((with_alpha(c.edge, alpha), 1.5, false))
                    } else if tentative {
                        Some((with_alpha(c.edge, 0.7 * alpha), 1., true))
                    } else {
                        None
                    };
                    frame.push(MARKS, Cmd::Quad { b: *r, r: radius, color: fill, border });
                    if unsent && w > 14. && h > 10. {
                        // A small dot: waiting to sync.
                        let d = [r[2] - 8., r[1] + 4., r[2] - 4., r[1] + 8.];
                        frame.push(FRONT, Cmd::Quad { b: d, r: 2., color: with_alpha(th.ink, 0.55 * alpha), border: None });
                    }
                    // Accent bar on blocks and list rows: the calendar's colour, quietly.
                    let accent = matches!(m.target.kind, Kind::Rect | Kind::Row { .. });
                    if accent && w > 10. && h > 6. && !tentative {
                        let bar = [r[0], r[1], r[0] + 3., r[3]];
                        frame.push(MARKS, Cmd::Quad { b: bar, r: radius.min(1.5), color: with_alpha(c.edge, alpha), border: None });
                    }
                    quads += 1;
                    if alpha > 0.3 {
                        hits.push((i, Hit::Rect(*r)));
                    }
                }
                Shape::Strip(pts) => {
                    if selected || m.target.conflict {
                        let edge = if selected { c.edge } else { th.now };
                        frame.push(MARKS, Cmd::Stroke { pts: pts.clone(), closed: true, width: 1.5, color: with_alpha(edge, alpha) });
                    }
                    if alpha > 0.3 {
                        hits.push((i, Hit::Poly(pts.clone())));
                    }
                    frame.push(MARKS, Cmd::Strip { pts: pts.clone(), color: fill });
                    strips += 1;
                }
            }

            // Strain: a demanding pill above your usual energy reaches down to the curve.
            if energy_lens && t > 0.99 && model::strain(e) >= model::STRAIN {
                let ga = self.lenses[p].grid.x.clamp(0., 1.) * self.layers[p].x.clamp(0., 1.) * alpha;
                if ga > 0.01 {
                    let inner = self.inner(p, arr.cell_inset(m.target.day));
                    let mid = (e.start_min + e.end_min()) * 0.5;
                    let (x, y) = inner.map(mid / 1440., model::energy_v(self.data.energy_at(m.target.day, mid)));
                    if y > b[3] + 2. {
                        frame.push(BACK, Cmd::Quad { b: [x - 0.75, b[3], x + 0.75, y], r: 0., color: with_alpha(th.now, 0.55 * ga), border: None });
                    }
                    frame.push(FRONT, Cmd::Quad { b: [x - 3.5, y - 3.5, x + 3.5, y + 3.5], r: 3.5, color: with_alpha(th.now, ga), border: None });
                }
            }
            // With the energy rail: strained events get a dot on the rail at their midpoint.
            if !energy_lens && t > 0.99 && model::strain(e) >= model::STRAIN && self.rail_of(p).is_some_and(|(k, _)| k == crate::spec::Rail::Energy) {
                let ga = self.lenses[p].grid.x.clamp(0., 1.) * self.layers[p].x.clamp(0., 1.) * alpha;
                if ga > 0.01 {
                    let inner = self.inner(p, arr.cell_inset(m.target.day));
                    let u = (e.start_min + e.end_min()) * 0.5 / 1440.;
                    let (x, y) = inner.map(u, u);
                    let (x, y) = if self.views[p].y == Axis::TimeOfDay { (inner.cell[0] - RAIL_W * 0.5, y) } else { (x, inner.cell[1] - RAIL_W * 0.5) };
                    frame.push(FRONT, Cmd::Quad { b: [x - 3.5, y - 3.5, x + 3.5, y + 3.5], r: 3.5, color: with_alpha(th.now, ga), border: Some((with_alpha(th.bg, ga), 1., false)) });
                }
            }
            boxes.push((i, b, t));
            shown.push((i, shape));
        }

        // Remember what was drawn: the start of the next morph.
        for &i in &self.drawn_marks {
            self.marks[i].shown = None;
        }
        self.drawn_marks.clear();
        for (i, s) in shown {
            self.marks[i].shown = Some(s);
            self.drawn_marks.push(i);
        }
        self.hits = hits;

        let (n_labels, placed) = if skip("labels") { (0, Vec::new()) } else { self.labels(p, &arr, &boxes, window, &mut frame) };

        // "+3" where a month-view cell has more events than room.
        for (day, n) in overflow {
            let cell = arr.cell_inset(day);
            if !arr.on_screen(&cell) {
                continue;
            }
            let line = shape(window, format!("+{n}").into(), &fonts.medium, 10.5, with_alpha(th.faint, self.layers[p].x));
            frame.push(LABELS, Cmd::Line { line, at: (cell[2] - 22., cell[3] - 15.), size: 10.5, clip: Some(cell) });
        }

        let mut relabel = false;
        for &(i, _, _) in &boxes {
            let t = if placed.contains(&i) { 1. } else { 0. };
            let m = &mut self.marks[i];
            if m.placed.target != t {
                m.placed.target = t;
                relabel = true;
            }
        }

        Built { frame, counts: (quads, strips, n_labels), relabel }
    }

    /// Label placement. Already-visible labels win ties so placement is stable.
    fn labels(&self, p: usize, arr: &Arr, boxes: &[(usize, Rect, f32)], window: &Window, frame: &mut Frame) -> (usize, Vec<usize>) {
        let th = self.theme;
        let fonts = &self.fonts;
        let mut cands: Vec<&(usize, Rect, f32)> =
            boxes.iter().filter(|(i, _, _)| self.marks[*i].label.x > 0.02 || self.marks[*i].label.target > 0.).collect();
        cands.sort_by(|a, b| {
            let (ma, mb) = (&self.marks[a.0], &self.marks[b.0]);
            (mb.placed.target > 0.5)
                .cmp(&(ma.placed.target > 0.5))
                .then(self.data.events[b.0].dur_min.total_cmp(&self.data.events[a.0].dur_min))
        });
        let mut placed_rects: Vec<Rect> = Vec::new();
        let mut placed = Vec::new();
        let mut n = 0;
        let lens_a = self.layers[p].x.clamp(0., 1.);

        for &&(i, b, t) in &cands {
            let m = &self.marks[i];
            let e = &self.data.events[i];
            let c = self.mark_colors(e);
            let settle = smoothstep(0.7, 1., t);
            let base = m.label.x.clamp(0., 1.) * m.placed.x.clamp(0., 1.) * m.alpha.x.clamp(0., 1.) * settle;
            let (w, h) = (b[2] - b[0], b[3] - b[1]);
            let cell = arr.cell_inset(m.target.day);
            match m.label_mode {
                LabelMode::Inside => {
                    if h < 11. || w < 30. {
                        continue;
                    }
                    placed.push(i);
                    let a = base;
                    if a < 0.02 {
                        continue;
                    }
                    let (x0, avail) = (b[0] + 9., w - 14.);
                    let clip = Some([b[0], b[1], b[2] - 3., b[3]]);
                    let size = if h >= 20. { 12. } else if h >= 14. { 11. } else { 10. };
                    let ty = if h >= 34. { b[1] + 5. } else { b[1] + (h - size * 1.3) * 0.5 - 0.5 };
                    let Some((title, tw)) = fit(window, &e.title, &fonts.semibold, size, avail) else { continue };
                    let line = shape(window, title, &fonts.semibold, size, with_alpha(c.text, a));
                    frame.push(LABELS, Cmd::Line { line, at: (x0, ty), size, clip });
                    n += 1;
                    let muted = with_alpha(c.text, a * 0.68);
                    if h >= 34. {
                        // Full range if it fits, else just the start: never a truncated time.
                        let time: SharedString = fmt_range(e.start_min, e.end_min()).into();
                        let start: SharedString = fmt_time(e.start_min).into();
                        if let Some(t) = [time, start].into_iter().find(|t| text::width(window, t, &fonts.regular, 11.) <= avail) {
                            let line = shape(window, t, &fonts.regular, 11., muted);
                            frame.push(LABELS, Cmd::Line { line, at: (x0, ty + 16.), size: 11., clip });
                        }
                        if let (true, Some(loc)) = (h >= 52., &e.location)
                            && let Some((t, _)) = fit(window, loc, &fonts.regular, 11., avail)
                        {
                            let line = shape(window, t, &fonts.regular, 11., with_alpha(c.text, a * 0.5));
                            frame.push(LABELS, Cmd::Line { line, at: (x0, ty + 31.), size: 11., clip });
                        }
                    } else if h >= 14. {
                        let start: SharedString = fmt_time(e.start_min).into();
                        let sw = text::width(window, &start, &fonts.regular, 11.);
                        if tw + 6. + sw <= avail {
                            let line = shape(window, start, &fonts.regular, 11., muted);
                            frame.push(LABELS, Cmd::Line { line, at: (x0 + tw + 6., ty + 1.), size: 11., clip });
                        }
                    }
                }
                LabelMode::Row => {
                    if h < 9. || w < 34. {
                        continue;
                    }
                    // A multi-day bar is titled once per line: where it starts, or where a line begins.
                    let per_line = self.views[p].facet.per_line.max(1);
                    let piece = e.all_day.filter(|_| matches!(m.target.kind, Kind::AllDay { .. }));
                    if piece.is_some_and(|a| a.prev && e.day.rem_euclid(per_line) != 0) {
                        continue;
                    }
                    placed.push(i);
                    if base < 0.02 {
                        continue;
                    }
                    // The title may run on across the bar's later days in this line.
                    let mut right = b[2];
                    if piece.is_some() {
                        let mut d = e.day;
                        while (d + 1).rem_euclid(per_line) != 0 && self.data.on_day(d + 1).any(|k| self.data.events[k].group == e.group && self.data.events[k].all_day.is_some()) {
                            d += 1;
                        }
                        right = arr.cell_inset(d)[2] - 2.;
                    }
                    let clip = Some([b[0], b[1], right - 2., b[3]]);
                    let time: SharedString = compact_time(e.start_min).into();
                    let tw = text::width(window, &time, &fonts.regular, 10.5);
                    let (x0, y) = (b[0] + 7., b[1] + (h - 13.5) * 0.5);
                    let show_time = w > 90. && e.all_day.is_none();
                    if show_time {
                        let line = shape(window, time, &fonts.regular, 10.5, with_alpha(c.text, base * 0.65));
                        frame.push(LABELS, Cmd::Line { line, at: (x0, y), size: 10.5, clip });
                    }
                    let tx = if show_time { x0 + tw + 5. } else { x0 };
                    if let Some((title, _)) = fit(window, &e.title, &fonts.medium, 10.5, right - tx - 4.) {
                        let line = shape(window, title, &fonts.medium, 10.5, with_alpha(c.text, base));
                        frame.push(LABELS, Cmd::Line { line, at: (tx, y), size: 10.5, clip });
                        n += 1;
                    }
                }
                LabelMode::Overflow => {
                    if h < 14. {
                        continue;
                    }
                    let a = base;
                    let size = 12.;
                    let lh = size * 1.3;
                    let yc = (b[1] + b[3]) * 0.5;
                    // Inside the pill when the title fits, otherwise trailing it on the canvas.
                    let full = text::width(window, &e.title, &fonts.medium, size);
                    let lx = if full <= w - 20. { b[0] + 10. } else { b[2] + 7. };
                    let band = [lx, yc - lh * 0.5, lx, yc + lh * 0.5];
                    // Room until the next mark or label on this line, or the cell edge.
                    let mut limit = cell[2] - 4.;
                    for &(j, ob, _) in boxes {
                        if j != i && ob[2] > lx && ob[1] < band[3] && ob[3] > band[1] {
                            limit = limit.min(ob[0] - 6.);
                        }
                    }
                    for r in &placed_rects {
                        if r[2] > lx && r[1] < band[3] && r[3] > band[1] {
                            limit = limit.min(r[0] - 8.);
                        }
                    }
                    let Some((title, tw)) = fit(window, &e.title, &fonts.medium, size, limit - lx) else { continue };
                    let rect = [lx, band[1], lx + tw, band[3]];
                    if placed_rects.iter().any(|r| overlaps(r, &rect)) {
                        continue;
                    }
                    placed_rects.push(rect);
                    placed.push(i);
                    if a < 0.02 {
                        continue;
                    }
                    if lx >= b[2] {
                        // On the open canvas: a plate keeps the label legible over gridlines and curves.
                        let plate = [lx - 4., band[1] + 1., lx + tw + 4., band[3] - 1.];
                        let mut under = th.bg;
                        let ea = if self.views[p].y == Axis::Demand { self.lenses[p].grid.x.clamp(0., 1.) * lens_a } else { 0. };
                        if ea > 0.01 {
                            // Match the energy fill when the plate sits inside the curve area.
                            let inner = self.inner(p, cell);
                            let u = inner.xr.0 + (lx + tw * 0.5 - cell[0]) / (cell[2] - cell[0]) * (inner.xr.1 - inner.xr.0);
                            let level = self.data.energy_at(e.day, u * 1440.);
                            let (_, cy) = inner.map(u, 1. - (1. - model::energy_v(level)) * ea);
                            if yc > cy {
                                under = th.bg.blend(with_alpha(th.energy, if th.dark { 0.10 } else { 0.13 } * ea));
                            }
                        }
                        frame.push(LABELS, Cmd::Quad { b: plate, r: 4., color: with_alpha(under, 0.9 * a), border: None });
                    }
                    let line = shape(window, title, &fonts.medium, size, with_alpha(c.text, a));
                    frame.push(LABELS, Cmd::Line { line, at: (lx, band[1]), size, clip: None });
                    n += 1;
                }
                LabelMode::Arc => {
                    let inner = self.inner(p, cell);
                    if inner.coord != Coord::Polar {
                        continue;
                    }
                    let g = m.target.g;
                    let (r0, r1) = (inner.radius(g[V0]), inner.radius(g[V1]));
                    let r_mid = (r0 + r1) * 0.5;
                    let span = (inner.nu(g[U1]) - inner.nu(g[U0])).abs() * TAU;
                    let size = 11.;
                    if (r1 - r0).abs() < size + 3. || span * r_mid < 36. {
                        continue;
                    }
                    placed.push(i);
                    if base < 0.02 {
                        continue;
                    }
                    let Some((title, _)) = fit(window, &e.title, &fonts.medium, size, span * r_mid - 14.) else { continue };
                    let mid = (inner.angle(g[U0]) + inner.angle(g[U1])) * 0.5;
                    let color = with_alpha(c.text, base);
                    let line = shape(window, title, &fonts.medium, size, color);
                    // Keep text upright: flip along the bottom half of the dial.
                    let flip = mid.sin() > 0.2;
                    frame.push(LABELS, Cmd::ArcText { line, color, c: inner.center(), r: r_mid, mid, flip, size });
                    n += 1;
                }
                LabelMode::Hidden => {}
            }
        }
        (n, placed)
    }

    /// Furniture for lens `li` at layer opacity `a`.
    fn decorate(&self, li: usize, a: f32, window: &Window, f: &mut Frame) {
        let th = self.theme;
        let fonts = &self.fonts;
        let spec = &self.views[li];
        let arr = self.arr_of(li, false);
        let l = &self.lenses[li];
        let [px0, py0, pw, ph] = arr.plot;
        let boxn = arr.boxness();
        let heat = l.heat.x.clamp(0., 1.);
        let grid = l.grid.x.clamp(0., 1.);
        let bandness = smoothstep(1., 1.3, arr.bands());
        let today = self.data.today;
        let now = self.now_min;
        let label = |f: &mut Frame, group, s: String, font: &gpui::Font, size: f32, color: Hsla, at: Pt, align: f32| {
            let line = shape(window, s.into(), font, size, color);
            let x = at.0 - f32::from(line.width) * align;
            f.push(group, Cmd::Line { line, at: (x, at.1), size, clip: None });
        };
        // Zone names: a little larger than axis text (flags read better), right-aligned at
        // `at`, and hoverable for the zone's long name and current time.
        let zone_label = |f: &mut Frame, s: String, font: &gpui::Font, color: Hsla, (x, y): Pt, tz: Option<chrono_tz::Tz>, a: f32| {
            let size = 12.5;
            let line = shape(window, s.into(), font, size, color);
            let x0 = x - f32::from(line.width);
            if li == self.preset && a > 0.5 {
                self.zone_hits.borrow_mut().push(([x0 - 4., y - 2., x + 4., y + size * 1.3 + 2.], tz));
            }
            f.push(FREE, Cmd::Line { line, at: (x0, y), size, clip: None });
        };
        let hair = |f: &mut Frame, group, b: Rect, alpha: f32| {
            f.push(group, Cmd::Quad { b, r: 0., color: with_alpha(th.hairline, alpha), border: None });
        };
        let vis: Vec<(i32, Rect)> = arr.visible().map(|d| (d, arr.cell(d))).filter(|(_, c)| arr.on_screen(c)).collect();
        let at_top = |c: &Rect| c[1] <= py0 + 2. && c[3] > py0 + 2.;
        let at_left = |c: &Rect| c[0] <= px0 + 2. && c[2] > px0 + 2.;
        let time_y = spec.y == Axis::TimeOfDay;
        let time_x = spec.x == Axis::TimeOfDay;
        // Extra zones labelling this lens's time axis, unless its grammar narrows to primary.
        let zones: Vec<chrono_tz::Tz> =
            if self.zones_on && spec.zones == crate::spec::ZoneAxes::All { self.zones.extra.clone() } else { Vec::new() };

        // Cell boxes once lines separate, with alternate months gently tinted.
        let box_a = a * boxn * (1. - heat);
        if box_a > 0.01 && spec.coord == Coord::Cartesian {
            for &(d, _) in &vis {
                let c = arr.cell_inset(d);
                let month = self.data.date(d).month();
                let bg = if month.is_multiple_of(2) { th.cell } else { color::mix(th.cell, th.bg, 0.5) };
                f.push(BACK, Cmd::Quad { b: c, r: 6. * boxn, color: with_alpha(bg, box_a), border: None });
            }
        }

        if heat * a > 0.01 {
            self.heat_cells(li, &arr, heat * a, window, f);
        }

        // Date labels inside boxes (aggregate cells carry their own).
        let date_a = a * boxn * (1. - heat);
        if date_a > 0.01 && spec.coord == Coord::Cartesian {
            for &(d, _) in &vis {
                let c = arr.cell_inset(d);
                if c[2] - c[0] < 24. || c[3] - c[1] < 16. {
                    continue;
                }
                let date = self.data.date(d);
                let first = date.day() == 1;
                let text = match (spec.facet.per_line, first) {
                    (1, _) => format!("{} {}", DAY_NAMES[d.rem_euclid(7) as usize], date.day()),
                    (_, true) => date.format("%-d %b").to_string(),
                    _ => date.day().to_string(),
                };
                let (font, color) = if d == today {
                    (&fonts.semibold, th.now)
                } else if first {
                    (&fonts.semibold, th.ink)
                } else {
                    (&fonts.medium, th.muted)
                };
                label(f, LABELS, text, font, 10.5, with_alpha(color, date_a), (c[0] + 7., c[1] + 4.), 0.);
                if d == today {
                    f.push(BACK, Cmd::Quad { b: c, r: 6., color: gpui::transparent_black(), border: Some((with_alpha(th.now, date_a), 1.5, false)) });
                }
            }
        }

        // Time axes, wherever time runs: hairlines on every hour, labels on the outer edge
        // (unless the arrangement already uses it), extra-zone rows, and the now marker.
        let week_header = spec.facet.per_line > 1 && spec.facet.dir == Dir::Rows;
        let step_for = |px_per_hour: f32| if px_per_hour >= 44. { 1 } else if px_per_hour >= 22. { 2 } else if px_per_hour >= 12. { 3 } else { 6 };
        let span_h = (l.hours[1].x - l.hours[0].x) * 24.;
        if spec.coord == Coord::Cartesian && time_x {
            let ga = a * grid;
            let labels = !week_header;
            for &(d, _) in &vis {
                let c = arr.cell_inset(d);
                let inner = self.inner(li, c);
                let step = step_for((c[2] - c[0]) / span_h);
                if ga > 0.01 {
                    for hr in 0..=24 {
                        let (x, _) = inner.map(hr as f32 / 24., 0.);
                        if x < c[0] - 0.5 || x > c[2] + 0.5 {
                            continue;
                        }
                        hair(f, BACK, [x, c[1], x + 1., c[3]], ga * (1. - 0.4 * boxn));
                        if labels && at_top(&c) && hr % step == 0 {
                            label(f, TOP, hour_label(hr), &fonts.regular, 11., with_alpha(th.faint, ga), (x, py0 - 26.), 0.5);
                            // Extra zones stack above, one row each.
                            let date = self.data.date(d);
                            for (j, tz) in zones.iter().enumerate() {
                                let text = crate::zones::axis_label(hr as f32 * 60., self.zones.delta(*tz, date));
                                let y = py0 - 26. - ZONE_ROW * (j + 1) as f32;
                                label(f, TOP, text, &fonts.regular, 10.5, with_alpha(th.faint, ga * 0.75), (x, y), 0.5);
                            }
                        }
                    }
                }
                if d == today && ga > 0.01 {
                    let (x, _) = inner.map(now / 1440., 0.);
                    f.push(FRONT, Cmd::Quad { b: [x - 1., c[1], x + 1., c[3]], r: 1., color: with_alpha(th.now, ga), border: None });
                    f.push(FRONT, Cmd::Quad { b: [x - 4., c[1] - 2., x + 4., c[1] + 6.], r: 4., color: with_alpha(th.now, ga), border: None });
                }
            }
            // Zone names beside their hour rows.
            if labels && !zones.is_empty() && ga > 0.01
                && let Some(&(d, _)) = vis.iter().find(|(_, c)| at_top(c))
            {
                let date = self.data.date(d);
                zone_label(f, self.zones.primary_abbrev(date), &fonts.semibold, with_alpha(th.muted, ga), (px0 - 12., py0 - 27.), None, ga);
                for (j, tz) in zones.iter().enumerate() {
                    let y = py0 - 25. - ZONE_ROW * (j + 1) as f32;
                    zone_label(f, self.zones.label(*tz, date), &fonts.medium, with_alpha(th.faint, ga), (px0 - 12., y - 2.), Some(*tz), ga);
                }
            }
        }
        if spec.coord == Coord::Cartesian && time_y {
            let ga = a * grid;
            // The gutter holds weekday names when lines run across.
            let labels = spec.facet.dir == Dir::Rows;
            for &(d, c) in &vis {
                if ga <= 0.01 {
                    break;
                }
                let inner = self.inner(li, arr.cell_inset(d));
                let ci = inner.cell;
                let step = step_for((ci[3] - ci[1]) / span_h);
                for hr in 0..=24 {
                    let (_, y) = inner.map(0., hr as f32 / 24.);
                    if y < ci[1] - 0.5 || y > ci[3] + 0.5 {
                        continue;
                    }
                    hair(f, BACK, [ci[0], y, ci[2], y + 1.], ga * (1. - 0.5 * boxn));
                    if labels && at_left(&c) && at_top(&c) && hr % step == 0 && hr > 0 && hr < 24 {
                        let la = ga * (1. - boxn);
                        label(f, LEFT, hour_label(hr), &fonts.regular, 11., with_alpha(th.faint, la), (px0 - 12., y - 7.), 1.);
                        // The same hour in each extra zone, one column further out each.
                        let date = self.data.date(d);
                        for (j, tz) in zones.iter().enumerate() {
                            let text = crate::zones::axis_label(hr as f32 * 60., self.zones.delta(*tz, date));
                            let x = px0 - 12. - ZONE_COL * (j + 1) as f32;
                            label(f, LEFT, text, &fonts.regular, 10.5, with_alpha(th.faint, la * 0.75), (x, y - 7.), 1.);
                        }
                    }
                }
                if d == today {
                    let (_, y) = inner.map(0., now / 1440.);
                    if y > ci[1] && y < ci[3] {
                        f.push(FRONT, Cmd::Quad { b: [ci[0], y - 1., ci[2], y + 1.], r: 1., color: with_alpha(th.now, ga), border: None });
                        f.push(FRONT, Cmd::Quad { b: [ci[0] - 4., y - 4., ci[0] + 4., y + 4.], r: 4., color: with_alpha(th.now, ga), border: None });
                    }
                }
            }
            // Zone names heading the hour columns.
            let za = a * grid * (1. - boxn);
            if labels && !zones.is_empty() && za > 0.01
                && let Some(&(d, _)) = vis.iter().find(|(_, c)| at_left(c) && at_top(c))
            {
                let date = self.data.date(d);
                zone_label(f, self.zones.primary_abbrev(date), &fonts.semibold, with_alpha(th.muted, za), (px0 - 12., py0 - 21.), None, za);
                for (j, tz) in zones.iter().enumerate() {
                    let x = px0 - 12. - ZONE_COL * (j + 1) as f32;
                    zone_label(f, self.zones.label(*tz, date), &fonts.medium, with_alpha(th.faint, za), (x, py0 - 21.), Some(*tz), za);
                }
            }
        }

        // The rail: a series beside each day's time axis, in 10-minute steps whose width
        // (style `rail`) and colour follow it. Energy uses the Demand colour scale, so events
        // coloured by Demand compare directly; daylight runs night → dawn → day.
        if let Some((kind, rail)) = self.rail_of(li) {
            let ra = a * grid;
            if ra > 0.01 {
                let time_y = spec.y == Axis::TimeOfDay;
                let n = crate::energy::SAMPLES - 1;
                for &(d, _) in &vis {
                    let inner = self.inner(li, arr.cell_inset(d));
                    let ci = inner.cell;
                    let date = self.data.date(d);
                    let utc = self.zones.primary_offset(date) as f32;
                    let place = self.place;
                    let value = |minute: f32| match kind {
                        crate::spec::Rail::Energy => self.data.energy_at(d, minute),
                        crate::spec::Rail::Daylight => place.map_or(0., |p| crate::sun::daylight(crate::sun::elevation(p, date, minute, utc))),
                    };
                    // Runs of near-equal energy draw as one quad: far fewer than one per step.
                    let mut k = 0;
                    while k < n {
                        let level = value((k as f32 + 0.5) / n as f32 * 1440.);
                        let mut end = k + 1;
                        while end < n && (value((end as f32 + 0.5) / n as f32 * 1440.) - level).abs() < 0.02 {
                            end += 1;
                        }
                        let (u0, u1) = (k as f32 / n as f32, end as f32 / n as f32);
                        k = end;
                        let (p0, p1) = (inner.map(u0, u0), inner.map(u1, u1));
                        let (s0, s1) = if time_y { (p0.1.max(ci[1]), p1.1.min(ci[3])) } else { (p0.0.max(ci[0]), p1.0.min(ci[2])) };
                        if s1 <= s0 {
                            continue;
                        }
                        let w = if rail { 2. + (RAIL_W - 4.) * level } else { RAIL_W - 4. };
                        let color = with_alpha(
                            match kind {
                                crate::spec::Rail::Energy => color::ramp(&th, color::Measure::Demand, 0.2 + 0.8 * level),
                                crate::spec::Rail::Daylight => color::daylight(&th, level),
                            },
                            ra,
                        );
                        let b = if time_y { [ci[0] - RAIL_W + 1., s0, ci[0] - RAIL_W + 1. + w, s1] } else { [s0, ci[1] - RAIL_W + 1., s1, ci[1] - RAIL_W + 1. + w] };
                        f.push(BACK, Cmd::Quad { b, r: 0., color, border: None });
                    }
                }
            }
        }

        match (spec.coord, spec.facet.per_line, spec.facet.dir) {
            (Coord::Polar, _, _) => self.decorate_dials(li, &arr, &vis, a, grid, heat, window, f),
            (_, _, Dir::Columns) => {
                // Week columns: month or date above the first band, weekdays in the gutter.
                for &(d, c) in &vis {
                    if d % 7 == 0 && c[1] <= py0 + 2. {
                        let date = self.data.date(d);
                        let (text, font, color) = if date.day() <= 7 {
                            (date.format("%b").to_string(), &fonts.semibold, th.muted)
                        } else {
                            (date.day().to_string(), &fonts.regular, th.faint)
                        };
                        // Above the hour rows (and their zones) when time runs across.
                        let y = if time_x { py0 - 44. - ZONE_ROW * zones.len() as f32 } else { py0 - 22. };
                        if c[2] - c[0] > 14. || date.day() <= 7 {
                            label(f, TOP, text, font, 11., with_alpha(color, a), ((c[0] + c[2]) * 0.5, y), 0.5);
                        }
                    }
                    if at_left(&c) {
                        let name = DAY_NAMES[d.rem_euclid(7) as usize];
                        label(f, LEFT, name.to_string(), &fonts.regular, 11., with_alpha(th.faint, a), (px0 - 12., (c[1] + c[3]) * 0.5 - 7.), 1.);
                    }
                }
            }
            (_, 1, _) => {
                // Day rows (Lanes, Energy).
                let ga = a * grid;
                for &(d, _) in &vis {
                    let c = arr.cell_inset(d);
                    let inner = self.inner(li, c);
                    if ga > 0.01 && spec.y == Axis::Calendar {
                        let n = (0..self.calendars.len()).filter(|&k| !self.hidden[k]).count().max(1);
                        for k in 1..n {
                            let (_, y) = inner.map(0., k as f32 / n as f32);
                            hair(f, BACK, [c[0], y, c[2], y + 1.], ga * 1.5);
                        }
                    }
                    if ga > 0.01 && spec.y == Axis::Demand {
                        // Personal energy: a soft area that rises as the lens arrives.
                        let k = 120;
                        let curve: Vec<Pt> = (0..=k)
                            .map(|j| {
                                let u = inner.xr.0 + (inner.xr.1 - inner.xr.0) * j as f32 / k as f32;
                                let level = self.data.energy_at(d, u * 1440.);
                                inner.map(u, 1. - (1. - model::energy_v(level)) * grid)
                            })
                            .collect();
                        let mut pts = curve.clone();
                        pts.extend(curve.iter().rev().map(|&(x, _)| (x, c[3])));
                        f.push(BACK, Cmd::Strip { pts, color: with_alpha(th.energy, if th.dark { 0.10 } else { 0.13 } * a) });
                        f.push(BACK, Cmd::Stroke { pts: curve, closed: false, width: if boxn > 0.5 { 1.25 } else { 2. }, color: with_alpha(th.energy, 0.7 * ga) });
                    }
                }
                // Gutter: lanes or the demand scale for a single day.
                let single = ga * (1. - boxn);
                if single > 0.01
                    && let Some(&(d, _)) = vis.iter().find(|(_, c)| at_top(c))
                {
                    let c = arr.cell_inset(d);
                    let inner = self.inner(li, c);
                    if spec.y == Axis::Calendar {
                        let visible: Vec<usize> = (0..self.calendars.len()).filter(|&k| !self.hidden[k]).collect();
                        let n = visible.len().max(1) as f32;
                        for (lane, &k) in visible.iter().enumerate() {
                            let (_, y0) = inner.map(0., lane as f32 / n);
                            let (_, y1) = inner.map(0., (lane as f32 + 1.) / n);
                            let y = (y0 + y1) * 0.5;
                            f.push(LEFT, Cmd::Quad { b: [16., y - 4., 24., y + 4.], r: 4., color: with_alpha(th.cals[k].dot, single), border: None });
                            label(f, LEFT, self.calendars[k].display().to_string(), &fonts.medium, 12., with_alpha(th.muted, single), (30., y - 8.), 0.);
                        }
                    } else {
                        label(f, LEFT, "Demanding".into(), &fonts.medium, 11.5, with_alpha(th.muted, single), (px0 - 14., c[1] + 4.), 1.);
                        label(f, LEFT, "Light".into(), &fonts.medium, 11.5, with_alpha(th.muted, single), (px0 - 14., c[3] - 20.), 1.);
                        let u = inner.xr.0;
                        let (_, y) = inner.map(u, model::energy_v(self.data.energy_at(d, u * 1440.)));
                        label(f, LEFT, "Your energy".into(), &fonts.medium, 11., with_alpha(th.energy, single), (px0 - 14., y - 8.), 1.);
                    }
                }
            }
            _ => {
                // Week grid: days along lines, time of day inside each cell.
                let single = a * (1. - boxn);
                for &(d, c) in &vis {
                    if single > 0.01 {
                        if d.rem_euclid(7) >= 5 {
                            f.push(BACK, Cmd::Quad { b: c, r: 0., color: with_alpha(th.weekend, single), border: None });
                        }
                        hair(f, BACK, [c[0], c[1], c[0] + 1., c[3]], single);
                    }
                    if at_top(&c) {
                        let xc = (c[0] + c[2]) * 0.5;
                        let date = self.data.date(d);
                        let is_today = d == today;
                        let dow = d.rem_euclid(7) as usize;
                        if single > 0.01 {
                            label(f, TOP, DAY_NAMES[dow].to_uppercase(), &fonts.medium, 10.5, with_alpha(if is_today { th.now } else { th.faint }, single), (xc, py0 - 56.), 0.5);
                            let num = date.day().to_string();
                            if is_today {
                                let r = 15.;
                                f.push(TOP, Cmd::Quad { b: [xc - r, py0 - 38., xc + r, py0 - 38. + 2. * r], r, color: with_alpha(th.now, single), border: None });
                                label(f, TOP, num, &fonts.semibold, 15., with_alpha(th.bg, single), (xc, py0 - 32.), 0.5);
                            } else {
                                label(f, TOP, num, &fonts.medium, 15., with_alpha(th.ink, single * 0.85), (xc, py0 - 32.), 0.5);
                            }
                        }
                        let boxed = a * boxn;
                        if boxed > 0.01 && c[2] - c[0] > 12. {
                            let initial = if c[2] - c[0] > 40. { DAY_NAMES[dow].to_string() } else { DAY_NAMES[dow][..1].to_string() };
                            label(f, TOP, initial, &fonts.medium, 10.5, with_alpha(th.faint, boxed), (xc, py0 - 18.), 0.5);
                        }
                    }
                    // Month names in the gutter where a month starts.
                    let gutter = a * boxn * (1. - bandness);
                    if gutter > 0.01 && d % 7 == 0
                        && let Some(first) = (d..d + 7).find(|&k| self.data.date(k).day() == 1) {
                            let date = self.data.date(first);
                            let text = if date.month() == 1 { date.format("%b %Y").to_string() } else { date.format("%b").to_string() };
                            label(f, LEFT, text, &fonts.semibold, 11.5, with_alpha(th.muted, gutter), (px0 - 12., c[1] + 4.), 1.);
                        }
                    // Band titles once quarters sit side by side.
                    let per_band = spec.facet.per_line * spec.facet.band;
                    if bandness * a > 0.01 && d % per_band == 0 {
                        // Bands start on the Monday before a quarter; name them by the days they mostly cover.
                        let (d0, d1) = (self.data.date(d + 3), self.data.date(d + per_band - 4));
                        let text = format!("{} – {}", d0.format("%b"), d1.format("%b %Y"));
                        label(f, TOP, text, &fonts.semibold, 11.5, with_alpha(th.muted, bandness * a), (c[0] + 2., py0 - 40.), 0.);
                    }
                }
                let _ = pw;
                let _ = ph;
            }
        }
    }

    /// Dials, one per visible day: rings, ticks, date, and now.
    #[allow(clippy::too_many_arguments)]
    fn decorate_dials(&self, li: usize, arr: &Arr, vis: &[(i32, Rect)], a: f32, grid: f32, heat: f32, window: &Window, f: &mut Frame) {
        let th = self.theme;
        let spec = &self.views[li];
        let fonts = &self.fonts;
        let today = self.data.today;
        let now = self.now_min;
        let ga = a * grid * (1. - heat);
        let lanes = (0..self.calendars.len()).filter(|&c| !self.hidden[c]).count().max(1);
        let label = |f: &mut Frame, s: String, font: &gpui::Font, size: f32, color: Hsla, at: Pt, align: f32| {
            let line = shape(window, s.into(), font, size, color);
            let x = at.0 - f32::from(line.width) * align;
            f.push(LABELS, Cmd::Line { line, at: (x, at.1), size, clip: None });
        };
        for &(d, _) in vis {
            let c = arr.cell_inset(d);
            let inner = self.inner(li, c);
            let (r_in, r_out) = inner.radii();
            let (cx, cy) = inner.center();
            let polar = |r: f32, t: f32| (cx + r * t.cos(), cy + r * t.sin());
            if ga > 0.01 {
                let samples = ((r_out / 3.) as usize).clamp(24, 128);
                // Lane rings only when the radius means calendar; otherwise just the band's edges.
                let rings = if spec.y == Axis::Calendar { lanes } else { 1 };
                for k in 0..=rings {
                    let r = r_in + k as f32 / rings as f32 * (r_out - r_in);
                    let pts = (0..=samples).map(|j| polar(r, j as f32 / samples as f32 * TAU)).collect();
                    f.push(BACK, Cmd::Stroke { pts, closed: true, width: 1., color: with_alpha(th.hairline, ga * 1.6) });
                }
                if r_out > 90. {
                    for hr in 0..24 {
                        let t = hr as f32 / 24. * TAU - FRAC_PI_2;
                        let major = hr % 3 == 0;
                        let (r0, r1) = (r_out + 4., r_out + if major { 10. } else { 7. });
                        f.push(BACK, Cmd::Stroke { pts: vec![polar(r0, t), polar(r1, t)], closed: false, width: 1., color: with_alpha(th.faint, ga * if major { 0.8 } else { 0.4 }) });
                        if major && r_out > 140. {
                            // Anchored by side, so labels hug the dial and stay inside the plot.
                            let (x, y) = polar(r_out + 16., t);
                            label(f, hour_label(hr), &fonts.regular, 11., with_alpha(th.faint, ga), (x, y - 7. + 7. * t.sin()), 0.5 - 0.5 * t.cos());
                        }
                    }
                }
                if spec.y == Axis::Demand {
                    // Personal energy as a radial area, from the curve out to the rim, as in the straight view.
                    let k = 144;
                    let curve: Vec<Pt> = (0..=k)
                        .map(|j| {
                            let u = j as f32 / k as f32;
                            let v = 1. - (1. - model::energy_v(self.data.energy_at(d, u * 1440.))) * grid;
                            let (r, t) = (inner.radius(v), inner.angle(u));
                            polar(r, t)
                        })
                        .collect();
                    let mut pts = curve.clone();
                    pts.extend((0..=k).rev().map(|j| polar(r_out, inner.angle(j as f32 / k as f32))));
                    f.push(BACK, Cmd::Strip { pts, color: with_alpha(th.energy, if th.dark { 0.10 } else { 0.13 } * a) });
                    f.push(BACK, Cmd::Stroke { pts: curve, closed: true, width: 1.5, color: with_alpha(th.energy, 0.7 * ga) });
                }
            }
            // Centre: weekday and date, sized to the dial.
            let ca = a * (1. - heat);
            if ca > 0.01 && r_in > 14. {
                let date = self.data.date(d);
                let big = (r_in * 0.22).clamp(10., 22.);
                let wd = DAY_NAMES[d.rem_euclid(7) as usize].to_uppercase();
                if r_in > 40. {
                    label(f, wd, &fonts.medium, 11., with_alpha(if d == today { th.now } else { th.faint }, ca), (cx, cy - big * 0.5 - 16.), 0.5);
                }
                let text = if r_in > 40. { date.format("%-d %b").to_string() } else { date.day().to_string() };
                label(f, text, &fonts.semibold, big, with_alpha(if d == today { th.now } else { th.ink }, ca), (cx, cy - big * 0.65), 0.5);
                if d == today && r_in > 40. {
                    label(f, fmt_time(now), &fonts.medium, 12., with_alpha(th.now, ca), (cx, cy + big * 0.5 + 2.), 0.5);
                }
            }
            if d == today && ga > 0.01 {
                // Time already spent today, as a quiet wedge, and the hand.
                let t1 = now / 1440. * TAU - FRAC_PI_2;
                let n = ((r_out / 4.) as usize).clamp(16, 96);
                let mut pts = Vec::with_capacity(2 * n + 2);
                for k in 0..=n {
                    pts.push(polar(r_in, -FRAC_PI_2 + (t1 + FRAC_PI_2) * k as f32 / n as f32));
                }
                for k in (0..=n).rev() {
                    pts.push(polar(r_out, -FRAC_PI_2 + (t1 + FRAC_PI_2) * k as f32 / n as f32));
                }
                f.push(BACK, Cmd::Strip { pts, color: with_alpha(th.weekend, ga) });
                f.push(FRONT, Cmd::Stroke { pts: vec![polar(r_in - 6., t1), polar(r_out + 6., t1)], closed: false, width: 2., color: with_alpha(th.now, ga) });
                let (x, y) = polar(r_out + 6., t1);
                f.push(FRONT, Cmd::Quad { b: [x - 4., y - 4., x + 4., y + 4.], r: 4., color: with_alpha(th.now, ga), border: None });
            }
        }
    }

    /// Aggregate cells: the colour channel reduced over each day, with the
    /// day-span glyph and value labels.
    fn heat_cells(&self, li: usize, arr: &Arr, a: f32, window: &Window, f: &mut Frame) {
        let th = self.theme;
        let fonts = &self.fonts;
        let hidden = &self.hidden;
        let today = self.data.today;
        let label = |f: &mut Frame, s: String, font: &gpui::Font, size: f32, color: Hsla, at: Pt, align: f32| {
            let line = shape(window, s.into(), font, size, color);
            let x = at.0 - f32::from(line.width) * align;
            f.push(LABELS, Cmd::Line { line, at: (x, at.1), size, clip: None });
        };
        let t = self.enc_t.x.clamp(0., 1.);
        let encs: Vec<(ColorEnc, f32)> = if li != self.preset {
            vec![(self.views[li].color_enc(&self.values[li], self.lenses[li].mark), 1.)]
        } else if t < 1. {
            vec![(self.enc_from, 1.), (self.enc_to, t)]
        } else {
            vec![(self.enc_to, 1.)]
        };
        let norms: Vec<f32> = encs.iter().map(|(e, _)| color::norm(&self.cells, e.measure, hidden)).collect();
        let enc = encs.last().unwrap().0;
        let span_a = if self.views[li].detail(&self.values[li], self.lenses[li].mark) == Detail::DaySpan { if li == self.preset { self.detail_t.x } else { 1. } } else { 0. };
        for d in arr.visible() {
            let b = arr.cell_inset(d);
            if !arr.on_screen(&b) {
                continue;
            }
            let Some(s) = self.cells.get(d as usize) else { continue };
            let (w, h) = (b[2] - b[0], b[3] - b[1]);
            let r = 6f32.min(w * 0.25).min(h * 0.25);
            let mut rep = th.cell;
            // Draw the outgoing encoding solid and the incoming one over it.
            for ((e, k), &norm) in encs.iter().zip(&norms) {
                let (pieces, rc) = color::cell_pieces(e, s, norm, &th, hidden);
                rep = color::mix(rep, rc, *k);
                for p in pieces {
                    let pb = [b[0] + p.r[0] * w, b[1] + p.r[1] * h, b[0] + p.r[2] * w, b[1] + p.r[3] * h];
                    if pb[2] - pb[0] < 0.25 || pb[3] - pb[1] < 0.25 {
                        continue;
                    }
                    // Round only the corners this piece shares with the cell.
                    let corner = |on: bool| if on { r } else { 0. };
                    let (l, tp, rt, bt) = (p.r[0] == 0., p.r[1] == 0., p.r[2] == 1., p.r[3] == 1.);
                    let radii = [corner(l && tp), corner(rt && tp), corner(rt && bt), corner(l && bt)];
                    f.push(BACK, Cmd::Corners { b: pb, radii, color: with_alpha(p.color, a * k) });
                }
            }
            // Day span: first start → last end on a mini 6am–midnight axis.
            let ga = span_a * a;
            if let (true, Some((first, last))) = (ga > 0.01 && h > 40., s.bounds(hidden)) {
                let ink = color::text_on(&th, rep);
                let top = b[1] + if h > 60. { 22. } else { 6. };
                let bottom = b[3] - 5.;
                let y_of = |min: f32| top + ((min - 360.) / 1080.).clamp(0., 1.) * (bottom - top);
                let roomy = w > 58. && h > 96.;
                let xc = if roomy { b[2] - 12. } else { (b[0] + b[2]) * 0.5 };
                for tick in [540., 1020.] {
                    let y = y_of(tick);
                    f.push(BACK, Cmd::Quad { b: [xc - 5., y, xc + 5., y + 1.], r: 0., color: with_alpha(ink, 0.3 * ga), border: None });
                }
                let (y0, y1) = (y_of(first), y_of(last).max(y_of(first) + 3.));
                let bw = if w > 30. { 2. } else { 1.25 };
                f.push(BACK, Cmd::Quad { b: [xc - bw, y0, xc + bw, y1], r: bw, color: with_alpha(ink, 0.8 * ga), border: None });
                if roomy {
                    label(f, compact_time(first), &fonts.medium, 10., with_alpha(ink, 0.85 * ga), (xc - 8., y0 - 6.5), 1.);
                    label(f, compact_time(last), &fonts.medium, 10., with_alpha(ink, 0.85 * ga), (xc - 8., y1 - 6.5), 1.);
                }
            }
            if self.hover_day == Some(d) && li == self.preset {
                f.push(BACK, Cmd::Quad { b, r, color: with_alpha(th.ink, 0.07 * a), border: None });
            }
            if d == today {
                f.push(BACK, Cmd::Quad { b, r, color: gpui::transparent_black(), border: Some((with_alpha(th.now, a), 1.5, false)) });
            }
            if w > 20. && h > 24. {
                let ink = color::text_on(&th, rep);
                let date = self.data.date(d);
                let small = w < 34.;
                let text = if date.day() == 1 && !small { date.format("%-d %b").to_string() } else { date.day().to_string() };
                let font = if date.day() == 1 { &fonts.semibold } else { &fonts.medium };
                label(f, text, font, if small { 9.5 } else { 11. }, with_alpha(ink, a), (b[0] + if small { 4. } else { 7. }, b[1] + 4.), 0.);
                let total = s.total(enc.measure, hidden);
                let value = match enc.measure {
                    Measure::Hours if total > 0. => format!("{total:.1}h"),
                    Measure::Count if total > 0. => format!("{}", total as u32),
                    Measure::Demand if total > 0. => format!("{:.0}%", total * 100.),
                    Measure::Strain if total > 0.05 => format!("{total:.1}"),
                    Measure::Free => format!("{total:.1}h"),
                    Measure::Starts | Measure::Ends if s.bounds(hidden).is_some() => compact_time(total),
                    Measure::Span if total > 0. => format!("{total:.1}h"),
                    _ => String::new(),
                };
                if !value.is_empty() && w > 56. {
                    label(f, value, &fonts.regular, 10.5, with_alpha(ink, a * 0.75), (b[2] - 7., b[1] + 5.5), 1.);
                }
            }
        }
    }
}

fn pt((x, y): Pt) -> gpui::Point<gpui::Pixels> {
    point(px(x), px(y))
}

fn bounds(b: Rect) -> Bounds<gpui::Pixels> {
    Bounds::from_corners(pt((b[0], b[1])), pt((b[2], b[3])))
}

pub fn paint(frame: Frame, window: &mut Window, cx: &mut App) {
    for group in frame.groups {
        let run = |window: &mut Window, cx: &mut App| {
            if group.layer {
                let b = group.clip.map(bounds).unwrap_or(Bounds::from_corners(pt((0., 0.)), pt((1e5, 1e5))));
                window.paint_layer(b, |window| paint_cmds(group.cmds, window, cx));
            } else {
                paint_cmds(group.cmds, window, cx);
            }
        };
        match group.clip {
            Some(c) => window.with_content_mask(Some(ContentMask { bounds: bounds(c) }), |window| run(window, cx)),
            None => run(window, cx),
        }
    }
}

fn paint_cmds(cmds: Vec<Cmd>, window: &mut Window, cx: &mut App) {
    for cmd in cmds {
        match cmd {
            Cmd::Quad { b, r, color, border } => {
                let (bw, bc, style) = match border {
                    Some((c, w, dashed)) => (w, c, if dashed { BorderStyle::Dashed } else { BorderStyle::Solid }),
                    None => (0., gpui::transparent_black(), BorderStyle::Solid),
                };
                window.paint_quad(quad(bounds(b), px(r), color, px(bw), bc, style));
            }
            Cmd::Corners { b, radii, color } => {
                let corners = gpui::Corners { top_left: px(radii[0]), top_right: px(radii[1]), bottom_right: px(radii[2]), bottom_left: px(radii[3]) };
                window.paint_quad(quad(bounds(b), corners, color, px(0.), gpui::transparent_black(), BorderStyle::Solid));
            }
            Cmd::Shadow { b, r, color } => {
                let shadow = BoxShadow { color, offset: point(px(0.), px(3.)), blur_radius: px(12.), spread_radius: px(0.) };
                window.paint_shadows(bounds(b), px(r).into(), &[shadow]);
            }
            Cmd::Strip { pts, color } => {
                // Triangulate the strip directly; a general tessellator per mark per frame is wasteful.
                let n = pts.len() / 2;
                if n < 2 {
                    continue;
                }
                let mut path = gpui::Path::new(pt(pts[0]));
                let st = (point(0., 1.), point(0., 1.), point(0., 1.));
                for k in 0..n - 1 {
                    let (a, b) = (pt(pts[k]), pt(pts[k + 1]));
                    let (c, d) = (pt(pts[2 * n - 1 - k]), pt(pts[2 * n - 2 - k]));
                    path.push_triangle((a, b, c), st);
                    path.push_triangle((b, d, c), st);
                }
                window.paint_path(path, color);
            }
            Cmd::Stroke { pts, closed, width, color } => {
                let mut b = PathBuilder::stroke(px(width));
                let pts: Vec<_> = pts.into_iter().map(pt).collect();
                b.add_polygon(&pts, closed);
                if let Ok(path) = b.build() {
                    window.paint_path(path, color);
                }
            }
            Cmd::Line { line, at, size, clip } => {
                let lh = text::line_height(size);
                match clip {
                    Some(c) => window.with_content_mask(Some(ContentMask { bounds: bounds(c) }), |window| {
                        line.paint(pt(at), lh, window, cx).ok();
                    }),
                    None => {
                        line.paint(pt(at), lh, window, cx).ok();
                    }
                }
            }
            Cmd::ArcText { line, color, c, r, mid, flip, size } => {
                // Baseline radius: glyphs sit centred on the arc's mid radius.
                let total = f32::from(line.width);
                let shift = size * 0.36;
                let rb = if flip { r + shift } else { r - shift };
                for run in line.runs.iter() {
                    for g in &run.glyphs {
                        let s = f32::from(g.position.x) - total * 0.5;
                        let (theta, rot) = if flip {
                            let t = mid - s / rb;
                            (t, t - FRAC_PI_2)
                        } else {
                            let t = mid + s / rb;
                            (t, t + FRAC_PI_2)
                        };
                        let origin = pt((c.0 + rb * theta.cos(), c.1 + rb * theta.sin()));
                        let rot = if rot > PI { rot - TAU } else { rot };
                        window.paint_glyph_rotated(origin, run.font_id, g.id, px(size), color, radians(rot)).ok();
                    }
                }
            }
        }
    }
}

/// Benchmark bisection: `CALM_SKIP=decor,marks,labels,chrome` turns parts off.
pub fn skip(part: &str) -> bool {
    thread_local!(static SKIP: String = std::env::var("CALM_SKIP").unwrap_or_default());
    SKIP.with(|s| s.split(',').any(|p| p == part))
}

/// `r` limited to `cell`; empty (zero-sized) when they don't overlap.
fn clip_to(r: Rect, cell: Rect) -> Rect {
    let (x0, y0) = (r[0].max(cell[0]), r[1].max(cell[1]));
    [x0, y0, r[2].min(cell[2]).max(x0), r[3].min(cell[3]).max(y0)]
}
