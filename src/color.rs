//! The colour channel of the grammar: what drives a mark's colour, how a set
//! of events is reduced to that value (an event mark is a set of one, an
//! aggregate cell is a day), and how faceted values are composed into one
//! cell (blended, striped, stacked, or the dominant facet).

use gpui::{Hsla, Rgba, hsla};

use crate::model::{self, Event, MAX_CALENDARS};
use crate::theme::{CalColors, Theme};

const N: usize = MAX_CALENDARS;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Measure {
    /// Hours booked.
    Hours,
    /// Number of events: fragmentation.
    Count,
    /// Duration-weighted mean demand.
    Demand,
    /// Demand above personal energy, signed and duration-weighted.
    Strain,
    /// Unbooked hours in the 9–5 core.
    Free,
    /// Earliest start: when the day begins.
    Starts,
    /// Latest end: when the day lets go.
    Ends,
    /// First start to last end, in hours.
    Span,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Compose {
    /// Facet colours mixed by share.
    Blend,
    /// Side-by-side stripes, width by share.
    Stripes,
    /// A stacked bar: height is the total, segments are shares.
    Stack,
    /// The largest facet's colour, stronger the more it dominates.
    Dominant,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ColorEnc {
    pub measure: Measure,
    /// Facet the measure by calendar and compose the parts.
    pub by_calendar: bool,
    pub compose: Compose,
    pub palette: Palette,
}

/// Where a mark's hue comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Palette {
    /// The measure's own ramp.
    Scale,
    /// Its calendar's colour; the measure sets how strong.
    Calendar,
}

impl Measure {
    /// Meaningful for a single event, not only for aggregates.
    pub fn per_event(self) -> bool {
        matches!(self, Measure::Hours | Measure::Demand | Measure::Strain)
    }

    pub fn facetable(self) -> bool {
        !matches!(self, Measure::Free | Measure::Starts | Measure::Ends | Measure::Span)
    }

    /// Per event, strain is signed (above or below the curve); aggregates sum the positive part.
    pub fn diverging_per_event(self) -> bool {
        self == Measure::Strain
    }

    fn hue(self) -> f32 {
        match self {
            Measure::Hours => 150.,
            Measure::Count => 215.,
            Measure::Demand => 38.,
            Measure::Strain => 10.,
            Measure::Free => 190.,
            Measure::Starts => 200.,
            Measure::Ends => 265.,
            Measure::Span => 28.,
        }
    }

    pub fn legend(self) -> (&'static str, &'static str) {
        match self {
            Measure::Hours => ("free", "booked"),
            Measure::Count => ("few", "many"),
            Measure::Demand => ("light", "heavy"),
            Measure::Strain => ("easy", "strained"),
            Measure::Free => ("none", "8h free"),
            Measure::Starts => ("late", "early"),
            Measure::Ends => ("early", "late"),
            Measure::Span => ("short", "long"),
        }
    }
}

impl Compose {
    pub const ALL: [Compose; 4] = [Compose::Blend, Compose::Stripes, Compose::Stack, Compose::Dominant];

    pub fn label(self) -> &'static str {
        match self {
            Compose::Blend => "Blend",
            Compose::Stripes => "Stripes",
            Compose::Stack => "Stack",
            Compose::Dominant => "Dominant",
        }
    }
}

impl ColorEnc {
    pub fn faceted(&self) -> bool {
        (self.by_calendar || self.palette == Palette::Calendar) && self.measure.facetable()
    }
}

/// Per-day reduction of the events, kept per calendar so any measure and
/// composition can be derived without touching events again.
#[derive(Clone, Copy, Default)]
pub struct CellStats {
    pub hours: [f32; N],
    pub count: [u32; N],
    /// demand × hours
    pub load: [f32; N],
    /// strain × hours, signed
    pub strain: [f32; N],
    pub free: f32,
    /// Earliest start and latest end per calendar, in minutes.
    pub first: [f32; N],
    pub last: [f32; N],
}

impl CellStats {
    pub fn part(&self, m: Measure, c: usize) -> f32 {
        match m {
            Measure::Hours => self.hours[c],
            Measure::Count => self.count[c] as f32,
            Measure::Demand => self.load[c],
            Measure::Strain => self.strain[c],
            Measure::Free | Measure::Starts | Measure::Ends | Measure::Span => 0.,
        }
    }

    /// First start and last end across visible calendars, if anything is booked.
    pub fn bounds(&self, hidden: &[bool; N]) -> Option<(f32, f32)> {
        let vis = (0..N).filter(|&c| !hidden[c]);
        let first = vis.clone().map(|c| self.first[c]).fold(f32::INFINITY, f32::min);
        let last = vis.map(|c| self.last[c]).fold(f32::NEG_INFINITY, f32::max);
        (first <= last).then_some((first, last))
    }

    pub fn total(&self, m: Measure, hidden: &[bool; N]) -> f32 {
        let sum = |f: &dyn Fn(usize) -> f32| (0..N).filter(|&c| !hidden[c]).map(f).sum::<f32>();
        match m {
            Measure::Demand => {
                let h = sum(&|c| self.hours[c]);
                if h > 0. { sum(&|c| self.load[c]) / h } else { 0. }
            }
            Measure::Free => self.free,
            Measure::Starts => self.bounds(hidden).map_or(0., |b| b.0),
            Measure::Ends => self.bounds(hidden).map_or(0., |b| b.1),
            Measure::Span => self.bounds(hidden).map_or(0., |b| (b.1 - b.0) / 60.),
            _ => sum(&|c| self.part(m, c)),
        }
    }
}

pub fn cell_stats(events: &[Event], hidden: &[bool; N], days: usize) -> Vec<CellStats> {
    let empty = CellStats { first: [f32::INFINITY; N], last: [f32::NEG_INFINITY; N], ..Default::default() };
    let mut cells = vec![empty; days];
    let mut core: Vec<Vec<(f32, f32)>> = vec![Vec::new(); days];
    for e in events {
        // All day isn't time spent: it has its own stripe, not a share of the measures.
        if e.all_day.is_some() {
            continue;
        }
        let Some(s) = cells.get_mut(e.day as usize) else { continue };
        let (c, h) = (e.calendar, e.dur_min / 60.);
        s.hours[c] += h;
        s.count[c] += 1;
        s.load[c] += e.energy * h;
        // Only demand above the curve counts: light events must not cancel strained ones.
        s.strain[c] += model::strain(e).max(0.) * h;
        s.first[c] = s.first[c].min(e.start_min);
        s.last[c] = s.last[c].max(e.end_min());
        if !hidden[c] {
            let (a, b) = (e.start_min.max(540.), e.end_min().min(1020.));
            if b > a {
                core[e.day as usize].push((a, b));
            }
        }
    }
    for (s, mut iv) in cells.iter_mut().zip(core) {
        iv.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (mut busy, mut end) = (0., 0f32);
        for (a, b) in iv {
            let a = a.max(end);
            if b > a {
                busy += b - a;
                end = b;
            }
        }
        s.free = (480. - busy) / 60.;
    }
    cells
}

/// Normaliser for a measure across all cells: intensities become `total / norm`.
pub fn norm(cells: &[CellStats], m: Measure, hidden: &[bool; N]) -> f32 {
    match m {
        Measure::Demand => 1.,
        Measure::Free => 8.,
        Measure::Starts | Measure::Ends => 1.,
        _ => cells.iter().map(|c| c.total(m, hidden).abs()).fold(1e-3, f32::max),
    }
}

pub fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b): (Rgba, Rgba) = (a.into(), b.into());
    let t = t.clamp(0., 1.);
    Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
    .into()
}

/// Weighted blend in Oklab, keeping the inputs' average chroma so mixing
/// several hues stays vivid instead of collapsing toward grey.
fn mix_weighted(colors: &[(Hsla, f32)]) -> Hsla {
    let total: f32 = colors.iter().map(|c| c.1).sum::<f32>().max(1e-6);
    let (mut l, mut a, mut b, mut chroma, mut alpha) = (0., 0., 0., 0., 0.);
    for &(c, w) in colors {
        let w = w / total;
        let rgba: Rgba = c.into();
        let (cl, ca, cb) = to_oklab(rgba);
        l += cl * w;
        a += ca * w;
        b += cb * w;
        chroma += (ca * ca + cb * cb).sqrt() * w;
        alpha += rgba.a * w;
    }
    let c = (a * a + b * b).sqrt();
    if c > 1e-4 {
        a *= chroma / c;
        b *= chroma / c;
    }
    let mut out = from_oklab(l, a, b);
    out.a = alpha;
    out.into()
}

#[allow(clippy::excessive_precision)] // Reference Oklab constants, kept verbatim.
fn to_oklab(c: Rgba) -> (f32, f32, f32) {
    let lin = |x: f32| if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) };
    let (r, g, b) = (lin(c.r), lin(c.g), lin(c.b));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    (
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    )
}

#[allow(clippy::excessive_precision)]
fn from_oklab(l: f32, a: f32, b: f32) -> Rgba {
    let l_ = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m_ = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s_ = (l - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    let gamma = |x: f32| {
        let x = x.clamp(0., 1.);
        if x <= 0.0031308 { 12.92 * x } else { 1.055 * x.powf(1. / 2.4) - 0.055 }
    };
    Rgba {
        r: gamma(4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_),
        g: gamma(-1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_),
        b: gamma(-0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_),
        a: 1.,
    }
}

/// Sequential ramp from the empty-cell colour to the measure's hue.
pub fn ramp(th: &Theme, m: Measure, t: f32) -> Hsla {
    let full = if th.dark { hsla(m.hue() / 360., 0.42, 0.52, 1.) } else { hsla(m.hue() / 360., 0.45, 0.46, 1.) };
    mix(th.cell, full, t)
}

/// How far toward sage the "easy" side of the per-event strain scale goes.
pub const EASY: f32 = 0.35;

/// Diverging ramp: comfortable (sage) ← neutral → strained (clay).
pub fn diverging(th: &Theme, t: f32) -> Hsla {
    let (l, s) = if th.dark { (0.52, 0.40) } else { (0.48, 0.45) };
    let end = if t < 0. { hsla(150. / 360., s, l, 1.) } else { hsla(10. / 360., s + 0.1, l, 1.) };
    mix(th.cell, end, t.abs())
}

/// A rectangle in unit cell space (x0, y0, x1, y1) with its colour.
pub struct Piece {
    pub r: [f32; 4],
    pub color: Hsla,
}

/// Colour pieces for one aggregate cell, plus a representative colour for
/// choosing legible text on top.
pub fn cell_pieces(enc: &ColorEnc, s: &CellStats, norm: f32, th: &Theme, hidden: &[bool; N]) -> (Vec<Piece>, Hsla) {
    let m = enc.measure;
    let total = s.total(m, hidden);
    let t = intensity(m, total, norm);
    let full = |color| (vec![Piece { r: [0., 0., 1., 1.], color }], color);
    if matches!(m, Measure::Starts | Measure::Ends | Measure::Span) && s.bounds(hidden).is_none() {
        return full(th.cell);
    }

    if !enc.faceted() {
        return full(ramp(th, m, t.clamp(0., 1.)));
    }

    let parts: Vec<(usize, f32)> = (0..N).filter(|&c| !hidden[c]).map(|c| (c, s.part(m, c).abs())).filter(|p| p.1 > 0.).collect();
    let sum: f32 = parts.iter().map(|p| p.1).sum();
    if sum <= 0. {
        return full(th.cell);
    }
    let t = t.abs().clamp(0., 1.);
    let dot = |c: usize| th.cals[c].dot;
    match enc.compose {
        Compose::Blend => {
            let blended = mix_weighted(&parts.iter().map(|&(c, v)| (dot(c), v)).collect::<Vec<_>>());
            full(mix(th.cell, blended, 0.15 + 0.85 * t))
        }
        Compose::Dominant => {
            let &(c, v) = parts.iter().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
            let share = v / sum;
            full(mix(th.cell, dot(c), (0.15 + 0.85 * t) * (0.35 + 0.65 * share)))
        }
        Compose::Stripes => {
            let mut pieces = vec![Piece { r: [0., 0., 1., 1.], color: th.cell }];
            let mut x = 0.;
            let k = 0.25 + 0.75 * t;
            for &(c, v) in &parts {
                let w = v / sum;
                pieces.push(Piece { r: [x, 0., x + w, 1.], color: mix(th.cell, dot(c), k) });
                x += w;
            }
            let rep = mix(th.cell, mix_weighted(&parts.iter().map(|&(c, v)| (dot(c), v)).collect::<Vec<_>>()), k);
            (pieces, rep)
        }
        Compose::Stack => {
            let mut pieces = vec![Piece { r: [0., 0., 1., 1.], color: th.cell }];
            let mut y = 1.;
            for &(c, v) in &parts {
                let h = v / sum * t;
                pieces.push(Piece { r: [0., y - h, 1., y], color: mix(th.cell, dot(c), 0.85) });
                y -= h;
            }
            (pieces, th.cell)
        }
    }
}

/// Colours for an individual event mark, or `None` to use its calendar's.
pub fn event_colors(enc: &ColorEnc, e: &Event, th: &Theme) -> Option<CalColors> {
    if enc.by_calendar || !enc.measure.per_event() {
        return None;
    }
    if enc.palette == Palette::Calendar {
        // The calendar's hue at the measure's strength. The edge keeps full calendar
        // colour so which calendar stays legible however pale the fill.
        let t = match enc.measure {
            Measure::Hours => 0.25 + 0.75 * (e.dur_min / 180.).min(1.),
            Measure::Demand => 0.15 + 0.85 * e.energy,
            // Easy and neutral alike stay pale; only strain deepens.
            _ => 0.15 + 0.85 * (model::strain(e) / 0.4).clamp(0., 1.),
        };
        let dot = th.cals[e.calendar].dot;
        let fill = mix(th.cell, dot, 0.1 + 0.9 * t);
        return Some(CalColors { fill, edge: dot, text: text_on(th, fill), dot });
    }
    let color = match enc.measure {
        Measure::Hours => ramp(th, Measure::Hours, 0.25 + 0.75 * (e.dur_min / 180.).min(1.)),
        Measure::Demand => ramp(th, Measure::Demand, 0.2 + 0.8 * e.energy),
        // Quiet when easy, loud when strained: comfort is the default, not news.
        _ => {
            let s = (model::strain(e) / 0.4).clamp(-1., 1.);
            diverging(th, if s < 0. { s * EASY } else { s })
        }
    };
    Some(CalColors {
        fill: color,
        edge: mix(color, th.ink, 0.35),
        text: if th.dark { th.ink } else { mix(th.ink, color, 0.2) },
        dot: color,
    })
}

pub fn mix_cal(a: &CalColors, b: &CalColors, t: f32) -> CalColors {
    CalColors { fill: mix(a.fill, b.fill, t), edge: mix(a.edge, b.edge, t), text: mix(a.text, b.text, t), dot: mix(a.dot, b.dot, t) }
}

/// Readable text over a background colour.
pub fn text_on(th: &Theme, bg: Hsla) -> Hsla {
    if bg.l > 0.6 { mix(th.ink, bg, 0.25) } else if th.dark { th.ink } else { th.bg }
}

/// Where a cell total sits on its measure's 0..1 ramp.
pub fn intensity(m: Measure, total: f32, norm: f32) -> f32 {
    match m {
        Measure::Demand => total,
        // 6am is as early as it gets, 11am is a late start.
        Measure::Starts => (660. - total) / 300.,
        // 3pm is an early finish, 11pm a late one.
        Measure::Ends => (total - 900.) / 480.,
        _ => total / norm,
    }
}

impl Measure {
    /// Config name → measure. `duration` is the per-event name for hours.
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "hours" | "duration" => Measure::Hours,
            "events" | "count" => Measure::Count,
            "demand" => Measure::Demand,
            "strain" => Measure::Strain,
            "free" => Measure::Free,
            "starts" => Measure::Starts,
            "ends" => Measure::Ends,
            "span" => Measure::Span,
            _ => return None,
        })
    }

    pub const NAMES: &[&str] = &["calendar", "hours", "duration", "events", "demand", "strain", "free", "starts", "ends", "span"];
}

impl Compose {
    pub fn from_name(s: &str) -> Option<Self> {
        Compose::ALL.into_iter().find(|c| c.label().eq_ignore_ascii_case(s))
    }

    pub const NAMES: &[&str] = &["blend", "stripes", "stack", "dominant"];
}

/// Daylight from 0 (night) to 1 (full day): dusky blue through a warm dawn to pale sun.
pub fn daylight(th: &Theme, t: f32) -> Hsla {
    let (night, dawn, day) = if th.dark {
        (hsla(225. / 360., 0.30, 0.30, 1.), hsla(22. / 360., 0.60, 0.52, 1.), hsla(48. / 360., 0.70, 0.66, 1.))
    } else {
        (hsla(225. / 360., 0.28, 0.42, 1.), hsla(22. / 360., 0.65, 0.62, 1.), hsla(48. / 360., 0.80, 0.70, 1.))
    };
    if t < 0.25 { mix(night, dawn, t / 0.25) } else { mix(dawn, day, (t - 0.25) / 0.75) }
}
