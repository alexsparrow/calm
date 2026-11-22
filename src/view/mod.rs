//! The calendar view: owns the dataset, per-lens zoom state, mark animation
//! and input.
//!
//! Marks are laid out in inner cell coordinates and projected through the
//! lens's facet arrangement every frame, so zooming and scrolling track input
//! exactly. Semantic changes (lens, level of detail, hidden calendars) morph
//! in *screen space*: each mark remembers the shape it was last drawn with
//! and springs from there to its live target, so a block can fold into an
//! arc or collapse into a month-view row whatever the projection does.

mod arrange;
mod chrome;
mod scene;

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use chrono::Datelike;
use gpui::{
    Context, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, Render, ScrollDelta,
    ScrollWheelEvent, Subscription, Window, WindowAppearance, canvas, div, prelude::*,
};

use crate::anim::{GENTLE, SNAPPY, Spring, SpringCfg};
use crate::color::{self, CellStats, ColorEnc, Compose, Measure};
use crate::layout::{self, LabelMode, Target};
use crate::model::{self, CALENDARS, DAYS, Dataset};
use crate::spec::{Align, Axis, Coord, Detail, Dir, Extent, Mark, PRESETS, Spec};
use crate::text::Fonts;
use crate::theme::Theme;
use arrange::{Arr, Inner, Pt};
use scene::Shape;

/// Zoom and scroll: responsive but still smoothed.
const VIEWPORT: SpringCfg = SpringCfg { stiffness: 320., damping: 36. };
/// How long input must pause before the zoom settles onto a stop.
const SNAP_AFTER: Duration = Duration::from_millis(220);
/// Left gutter when nothing is labelled beside the plot: just breathing room.
const GUTTER_MIN: f32 = 20.;
/// Bottom margin: the lens switcher and colour bar, plus a little air; and with the
/// second (compose / legend) row that aggregate views add.
const CONTROLS_ONE_ROW: f32 = 112.;
const CONTROLS_TWO_ROWS: f32 = 152.;

enum Hit {
    Rect([f32; 4]),
    Poly(Vec<Pt>),
}

impl Hit {
    fn contains(&self, (x, y): Pt) -> bool {
        match self {
            Hit::Rect(b) => x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3],
            Hit::Poly(pts) => {
                let mut inside = false;
                let mut j = pts.len() - 1;
                for i in 0..pts.len() {
                    let ((xi, yi), (xj, yj)) = (pts[i], pts[j]);
                    if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                        inside = !inside;
                    }
                    j = i;
                }
                inside
            }
        }
    }
}

struct MarkAnim {
    target: Target,
    alpha: Spring,
    /// Screen shape at the start of the current morph, if one is running.
    from: Option<Shape>,
    /// Morph progress, 0 → 1.
    t: Spring,
    delay: f32,
    /// What was drawn last frame: the starting point for the next morph.
    shown: Option<Shape>,
    /// Label-mode crossfade: fade out, swap mode, fade back in.
    label: Spring,
    label_mode: LabelMode,
    /// Whether label placement found room; springed so collisions fade.
    placed: Spring,
    hover: Spring,
}

/// Zoom, scroll and level of detail for one lens.
struct Lens {
    /// log2 of the number of day cells visible.
    zoom: Spring,
    /// First visible cell (continuous).
    start: Spring,
    /// Window on the inner time axis, as fractions of a day.
    hours: [Spring; 2],
    mark: Mark,
    /// Visibility of the aggregate (heat) layer and of the detailed-mark furniture.
    heat: Spring,
    grid: Spring,
    last_input: Option<Instant>,
}

#[derive(Default)]
struct Stats {
    intervals: Vec<f32>,
    cpu: Vec<f32>,
}

impl Stats {
    fn push(&mut self, interval: f32, cpu: f32) {
        self.intervals.push(interval);
        self.cpu.push(cpu);
        if self.intervals.len() > 4096 {
            self.intervals.drain(..2048);
            self.cpu.drain(..2048);
        }
    }
    fn pct(xs: &[f32], p: f32) -> f32 {
        if xs.is_empty() {
            return 0.;
        }
        let mut v = xs.to_vec();
        v.sort_by(f32::total_cmp);
        v[((v.len() - 1) as f32 * p) as usize]
    }
    fn recent(xs: &[f32]) -> &[f32] {
        &xs[xs.len().saturating_sub(120)..]
    }
}

struct Bench {
    /// False for `--cycle`: advance forever without printing stats.
    report: bool,
    start: Instant,
    last_step: Instant,
    step: usize,
    stats: Stats,
}

pub struct CalmView {
    focus: FocusHandle,
    fonts: Fonts,
    theme: Theme,
    theme_override: Option<bool>,
    /// Window background opacity when translucent.
    translucent: Option<f32>,
    _appearance: Subscription,
    data: Dataset,
    cells: Vec<CellStats>,
    colors: [ColorEnc; PRESETS.len()],
    details: [Detail; PRESETS.len()],
    detail_t: Spring,
    enc_from: ColorEnc,
    enc_to: ColorEnc,
    enc_t: Spring,
    marks: Vec<MarkAnim>,
    /// Marks with a morph in flight, drawn even if their day is off screen.
    flipping: Vec<usize>,
    /// Marks drawn last frame.
    drawn_marks: Vec<usize>,
    preset: usize,
    lenses: [Lens; PRESETS.len()],
    hidden: [bool; CALENDARS.len()],
    now_min: f32,
    dirty: bool,
    stagger: bool,
    viewport: (f32, f32),
    layers: [Spring; PRESETS.len()],
    /// Plot margins that fit what is actually drawn beside the plot.
    margin_l: Spring,
    margin_b: Spring,
    margin_t: Spring,
    /// Extra time zones (a global setting) and whether they are shown.
    zones: crate::zones::Zones,
    zones_on: bool,
    /// Zone labels drawn this frame, for hover: (rect, zone; `None` is primary).
    zone_hits: std::cell::RefCell<Vec<([f32; 4], Option<chrono_tz::Tz>)>>,
    hover_zone: Option<Option<chrono_tz::Tz>>,
    switcher: Spring,
    drawer: Spring,
    tooltip: Spring,
    help: Spring,
    help_on: bool,
    hud_on: bool,
    mouse: Pt,
    hits: Vec<(usize, Hit)>,
    hovered: Option<usize>,
    hover_day: Option<i32>,
    selected: Option<usize>,
    /// Last selection, kept while the drawer slides out.
    shown: Option<usize>,
    stress: bool,
    last_frame: Option<Instant>,
    cpu_last: Rc<Cell<f32>>,
    stats: Stats,
    animating: bool,
    drawn: (usize, usize, usize),
    bench: Option<Bench>,
    /// Screenshot mode: springs settle in a single frame.
    still: bool,
    /// Scripted pointer for screenshots: applied once the viewport is stable.
    pending: (Option<Pt>, Option<Pt>),
    stable_frames: u32,
    /// What the previous frame did, for explaining long intervals in benchmarks.
    last_trace: String,
}

fn stops_log(spec: &Spec) -> Vec<f32> {
    spec.stops.iter().map(|s| (s.cells as f32).log2()).collect()
}

impl CalmView {
    pub fn new(opts: Options, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let appearance = cx.observe_window_appearance(window, |this, window, cx| {
            this.theme = theme_for(this.theme_override, window.appearance(), this.translucent);
            cx.notify();
        });
        // Keep the now-line honest without waking every frame.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(20)).await;
                let alive = this.update(cx, |this, cx| {
                    this.now_min = model::now_minutes();
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();

        let data = model::generate(if opts.bench { std::env::var("CALM_MULT").ok().and_then(|m| m.parse().ok()).unwrap_or(2) } else { 1 });
        let today = data.today;
        let lenses = PRESETS.map(|spec| {
            let p = spec.facet.per_line;
            let first = spec.stops.iter().find(|s| s.cells >= p).unwrap_or(&spec.stops[0]);
            let n = first.cells as f32;
            let start = match spec.facet.dir {
                // Days run down columns: put today's week a few columns in.
                Dir::Columns => ((today.div_euclid(p) - 4).max(0) * p) as f32,
                Dir::Rows => (today.div_euclid(p) * p) as f32,
            };
            let hours = match (spec.coord, spec.y, spec.x) {
                (Coord::Polar, _, _) => (0., 1.),
                (_, Axis::TimeOfDay, _) => (7. / 24., 21.5 / 24.),
                (_, _, Axis::TimeOfDay) => (6.5 / 24., 22.5 / 24.),
                _ => (0., 1.),
            };
            Lens {
                zoom: Spring::new(n.log2()),
                start: Spring::new(start),
                hours: [Spring::new(hours.0), Spring::new(hours.1)],
                mark: spec.mark,
                heat: Spring::new(if spec.mark == Mark::Heat { 1. } else { 0. }),
                grid: Spring::new(if spec.mark == Mark::Heat { 0. } else { 1. }),
                last_input: None,
            }
        });
        let mut layers = [Spring::new(0.); PRESETS.len()];
        layers[0] = Spring::new(1.);
        let mut this = Self {
            focus,
            fonts: Fonts::new(),
            theme: theme_for(None, window.appearance(), opts.opacity),
            theme_override: None,
            translucent: opts.opacity,
            _appearance: appearance,
            cells: Vec::new(),
            colors: PRESETS.map(|p| p.color),
            details: PRESETS.map(|p| p.detail),
            detail_t: Spring::new(0.),
            enc_from: PRESETS[0].color,
            enc_to: PRESETS[0].color,
            enc_t: Spring::new(1.),
            marks: Vec::new(),
            flipping: Vec::new(),
            drawn_marks: Vec::new(),
            preset: 0,
            lenses,
            hidden: [false; CALENDARS.len()],
            now_min: model::now_minutes(),
            dirty: true,
            stagger: false,
            viewport: (0., 0.),
            layers,
            margin_l: Spring::new(GUTTER_MIN),
            margin_b: Spring::new(CONTROLS_ONE_ROW),
            margin_t: Spring::new(scene::MARGIN_T),
            zones: crate::zones::Zones::load(opts.zones.as_deref()),
            zones_on: true,
            zone_hits: Default::default(),
            hover_zone: None,
            switcher: Spring::new(0.),
            drawer: Spring::new(0.),
            tooltip: Spring::new(0.),
            help: Spring::new(0.),
            help_on: false,
            hud_on: opts.bench,
            mouse: (-1., -1.),
            hits: Vec::new(),
            hovered: None,
            hover_day: None,
            selected: None,
            shown: None,
            stress: opts.bench,
            last_frame: None,
            cpu_last: Rc::new(Cell::new(0.)),
            stats: Stats::default(),
            animating: true,
            drawn: (0, 0, 0),
            bench: (opts.bench || opts.cycle).then(|| Bench {
                report: opts.bench,
                start: Instant::now(),
                last_step: Instant::now(),
                step: 0,
                stats: Stats::default(),
            }),
            still: opts.still,
            pending: (opts.hover, opts.select),
            stable_frames: 0,
            last_trace: String::new(),
            data,
        };
        this.restat();
        if let Some(dark) = opts.dark {
            this.theme_override = Some(dark);
            this.theme = theme_for(Some(dark), window.appearance(), this.translucent);
        }
        if let Some(lens) = opts.lens {
            this.set_preset(lens.min(PRESETS.len() - 1));
        }
        if opts.offset != 0 {
            let l = &mut this.lenses[this.preset];
            let s = (l.start.target + opts.offset as f32).max(0.);
            l.start = Spring::new(s);
        }
        if let Some(stop) = &opts.zoom {
            let spec = this.spec();
            if let Some(i) = spec.stops.iter().position(|s| s.name.eq_ignore_ascii_case(stop)) {
                this.zoom_to_stop(i, None);
                let l = &mut this.lenses[this.preset];
                l.zoom = Spring::new(l.zoom.target);
                l.start = Spring::new(l.start.target);
            }
        }
        if opts.span {
            this.details[this.preset] = Detail::DaySpan;
            this.detail_t = Spring::new(1.);
        }
        if let Some(spec) = &opts.color {
            let mut enc = this.color();
            let mut parts = spec.split(':');
            let measure = parts.next().unwrap_or("");
            if measure == "calendar" {
                enc.by_calendar = true;
            } else if let Some(m) = Measure::ALL.into_iter().find(|m| m.label().to_lowercase().starts_with(measure)) {
                enc.measure = m;
                enc.by_calendar = false;
            }
            if let Some(c) = parts.next().and_then(|c| Compose::ALL.into_iter().find(|x| x.label().to_lowercase() == c)) {
                enc.compose = c;
                enc.by_calendar = true;
            }
            this.set_color(enc);
            this.enc_from = enc;
            this.enc_to = enc;
        }
        this
    }

    fn spec(&self) -> &'static Spec {
        &PRESETS[self.preset]
    }

    fn lens(&self) -> &Lens {
        &self.lenses[self.preset]
    }

    /// The arrangement for lens `p`, live or (with `target`) where it is heading.
    fn arr_of(&self, p: usize, target: bool) -> Arr {
        let l = &self.lenses[p];
        let spec = &PRESETS[p];
        let (x, y, w, h) = self.plot();
        let (z, s) = if target { (l.zoom.target, l.start.target) } else { (l.zoom.x, l.start.x) };
        Arr { plot: [x, y, w, h], facet: spec.facet, n: z.exp2(), start: s, days: DAYS, boxed: spec.x == Axis::None }
    }

    /// The plot rectangle (x, y, w, h) inside the window.
    fn plot(&self) -> (f32, f32, f32, f32) {
        let (vw, vh) = self.viewport;
        let (l, b) = (self.margin_l.x, self.margin_b.x);
        let t = self.margin_t.x;
        (l, t, (vw - l - scene::MARGIN_R).max(200.), (vh - t - b).max(200.))
    }

    /// Extra zones labelling the current lens's time axis (none if the grammar narrows to primary).
    fn shown_zones(&self) -> Vec<chrono_tz::Tz> {
        if self.zones_on && self.spec().zones == crate::spec::ZoneAxes::All {
            self.zones.extra.clone()
        } else {
            Vec::new()
        }
    }

    /// Left gutter wide enough for the labels the current lens draws there.
    fn gutter_target(&self) -> f32 {
        let spec = self.spec();
        let lines = self.arr_of(self.preset, true).lines();
        let single = lines < 1.12;
        match (spec.coord, spec.facet.per_line, spec.facet.dir) {
            (Coord::Polar, _, _) => GUTTER_MIN,
            // Weekday names beside the columns.
            (_, _, Dir::Columns) => 48.,
            // Calendar names, or the demand scale and "Your energy".
            (_, 1, _) if single => if spec.y == Axis::Calendar { 100. } else { 96. },
            (_, 1, _) => GUTTER_MIN,
            // Hour labels for a single week; month names once weeks stack; nothing for bands.
            // Plus a column per extra time zone.
            _ if single => 60. + scene::ZONE_COL * self.shown_zones().len() as f32,
            _ if self.arr_of(self.preset, true).bands() < 1.3 => 64.,
            _ => GUTTER_MIN,
        }
    }

    fn arr(&self) -> Arr {
        self.arr_of(self.preset, false)
    }

    /// Inner coordinates of a cell in lens `p`.
    fn inner(&self, p: usize, mut cell: [f32; 4]) -> Inner {
        let spec = &PRESETS[p];
        if spec.coord == Coord::Cartesian && spec.x != Axis::None {
            // Boxed cells keep a header strip for their date label.
            let b = self.arr_of(p, false).boxness();
            cell[1] = (cell[1] + 17. * b).min(cell[3] - 4.);
        }
        let l = &self.lenses[p];
        let hours = (l.hours[0].x, l.hours[1].x);
        let (xr, yr) = match (spec.x, spec.y) {
            (Axis::TimeOfDay, _) => (hours, (0., 1.)),
            (_, Axis::TimeOfDay) => ((0., 1.), hours),
            _ => ((0., 1.), (0., 1.)),
        };
        Inner { cell, coord: spec.coord, xr, yr }
    }

    /// Whether the current lens shows aggregates (so colour means a reduction over a day).
    fn aggregate(&self) -> bool {
        self.lens().mark == Mark::Heat
    }

    fn restat(&mut self) {
        self.cells = color::cell_stats(&self.data.events, &self.hidden, DAYS as usize);
    }

    fn set_data(&mut self, data: Dataset) {
        self.data = data;
        self.marks.clear();
        self.flipping.clear();
        self.drawn_marks.clear();
        self.selected = None;
        self.hovered = None;
        self.dirty = true;
        self.restat();
    }

    fn set_preset(&mut self, preset: usize) {
        if preset == self.preset {
            return;
        }
        self.preset = preset;
        for (i, l) in self.layers.iter_mut().enumerate() {
            l.target = if i == preset { 1. } else { 0. };
        }
        self.switcher.target = preset as f32;
        self.dirty = true;
        self.stagger = true;
    }

    fn color(&self) -> ColorEnc {
        self.colors[self.preset]
    }

    fn set_color(&mut self, enc: ColorEnc) {
        self.colors[self.preset] = enc;
    }

    /// Start a crossfade whenever the effective encoding changes, whether
    /// from the colour bar or from switching lens.
    fn sync_color(&mut self) {
        let enc = self.color();
        if enc != self.enc_to {
            self.enc_from = self.enc_to;
            self.enc_to = enc;
            self.enc_t = Spring { target: 1., ..Spring::new(0.) };
        }
    }

    fn cycle_measure(&mut self) {
        let mut enc = self.color();
        let aggregate = self.aggregate();
        let options: Vec<Measure> = Measure::ALL.into_iter().filter(|m| aggregate || m.per_event()).collect();
        let i = options.iter().position(|&m| m == enc.measure).map_or(0, |i| (i + 1) % options.len());
        enc.measure = options[i];
        if !aggregate {
            enc.by_calendar = false;
        }
        self.set_color(enc);
    }

    fn toggle_detail(&mut self) {
        let d = &mut self.details[self.preset];
        *d = if *d == Detail::DaySpan { Detail::None } else { Detail::DaySpan };
    }

    fn cycle_compose(&mut self) {
        let mut enc = self.color();
        let i = Compose::ALL.iter().position(|&c| c == enc.compose).unwrap_or(0);
        enc.compose = Compose::ALL[(i + 1) % Compose::ALL.len()];
        enc.by_calendar = true;
        self.set_color(enc);
    }

    fn toggle_calendar(&mut self, c: usize) {
        self.hidden[c] = !self.hidden[c];
        if self.hidden.iter().all(|&h| h) {
            self.hidden[c] = false;
        }
        self.restat();
        self.dirty = true;
    }

    // --- Zoom, scroll and snapping ------------------------------------------

    /// Index of the stop nearest the lens's target zoom.
    fn nearest_stop(&self) -> usize {
        let z = self.lens().zoom.target;
        let logs = stops_log(self.spec());
        (0..logs.len()).min_by(|&a, &b| (logs[a] - z).abs().total_cmp(&(logs[b] - z).abs())).unwrap_or(0)
    }

    /// (cells, start) for stop `i`, aligned per the stop's rule around `center`.
    fn aligned(&self, i: usize, center: f32) -> (f32, f32) {
        let spec = self.spec();
        let stop = spec.stops[i];
        let p = spec.facet.per_line as f32;
        let n = stop.cells as f32;
        let center = center.clamp(0., DAYS as f32 - 1.);
        let (n, start) = match stop.align {
            Align::Cell => (n, (center - n * 0.5 + 0.5).floor()),
            Align::Line => (n, ((center - n * 0.5 + p * 0.5) / p).floor() * p),
            Align::Group => (n, (center / n).floor() * n),
            Align::Month => {
                let date = self.data.date(center as i32);
                let first = date.with_day(1).unwrap();
                let next = if first.month() == 12 {
                    first.with_year(first.year() + 1).unwrap().with_month(1).unwrap()
                } else {
                    first.with_month(first.month() + 1).unwrap()
                };
                let d0 = (first - self.data.origin).num_days() as f32;
                let d1 = (next - self.data.origin).num_days() as f32 - 1.;
                let (l0, l1) = ((d0 / p).floor(), (d1 / p).floor());
                ((l1 - l0 + 1.) * p, l0 * p)
            }
        };
        (n, start.clamp(0., (DAYS as f32 - n).max(0.)))
    }

    fn center_target(&self) -> f32 {
        let l = self.lens();
        l.start.target + l.zoom.target.exp2() * 0.5
    }

    fn zoom_to_stop(&mut self, i: usize, center: Option<f32>) {
        let center = center.unwrap_or_else(|| self.center_target());
        let (n, start) = self.aligned(i, center);
        let l = &mut self.lenses[self.preset];
        l.zoom.target = n.log2();
        l.start.target = start;
        l.last_input = None;
    }

    /// Continuous zoom by `delta` (log2 cells), keeping the cell under `anchor` in place.
    fn zoom_by(&mut self, delta: f32, anchor: Option<Pt>) {
        let spec = self.spec();
        let logs = stops_log(spec);
        let (lo, hi) = (logs[0], *logs.last().unwrap());
        let arr = self.arr_of(self.preset, true);
        let old_n = arr.n;
        let a = anchor.and_then(|pt| arr.day_at(pt)).map(|d| d as f32 + 0.5).unwrap_or(arr.start + old_n * 0.5);
        let f = ((a - arr.start) / old_n).clamp(0., 1.);
        let l = &mut self.lenses[self.preset];
        l.zoom.target = (l.zoom.target + delta).clamp(lo, hi);
        let n = l.zoom.target.exp2();
        l.start.target = (a - f * n).clamp(0., (DAYS as f32 - n).max(0.));
        l.last_input = Some(Instant::now());
    }

    /// Step to the next stop out (`dir > 0`) or in.
    fn zoom_step(&mut self, dir: i32) {
        let i = self.nearest_stop() as i32;
        let next = (i + dir).clamp(0, self.spec().stops.len() as i32 - 1) as usize;
        self.zoom_to_stop(next, None);
    }

    /// Previous/next by the current stop's unit.
    fn pan(&mut self, dir: i32) {
        let i = self.nearest_stop();
        let stop = self.spec().stops[i];
        let shift = match stop.align {
            Align::Month => 30.5,
            _ => stop.cells as f32,
        };
        let center = self.center_target() + dir as f32 * shift;
        self.zoom_to_stop(i, Some(center));
    }

    fn go_today(&mut self) {
        let i = self.nearest_stop();
        self.zoom_to_stop(i, Some(self.data.today as f32 + 0.5));
    }

    /// Free scroll along the flow; snaps to alignment once input pauses.
    fn scroll_cells(&mut self, px: f32) {
        let arr = self.arr_of(self.preset, true);
        let l = &mut self.lenses[self.preset];
        let n = l.zoom.target.exp2();
        l.start.target = (l.start.target + px * arr.cells_per_px()).clamp(0., (DAYS as f32 - n).max(0.));
        l.last_input = Some(Instant::now());
    }

    /// Scroll (shift) or zoom (scale around `anchor` in 0..1) the inner hour window.
    fn scroll_hours(&mut self, shift: f32, scale: f32, anchor: f32) {
        let l = &mut self.lenses[self.preset];
        let (h0, h1) = (l.hours[0].target, l.hours[1].target);
        let span = ((h1 - h0) * scale).clamp(3. / 24., 1.);
        let pivot = h0 + (h1 - h0) * anchor;
        let lo = (pivot - span * anchor + shift).clamp(0., 1. - span);
        l.hours[0].target = lo;
        l.hours[1].target = lo + span;
    }

    /// Whether wheel scrolling should move the hour window rather than the days.
    fn scrolls_hours(&self) -> bool {
        let spec = self.spec();
        let l = self.lens();
        spec.coord == Coord::Cartesian
            && (spec.x == Axis::TimeOfDay || spec.y == Axis::TimeOfDay)
            && l.mark == spec.mark
            && self.arr().lines() < 1.05
            && (l.hours[1].target - l.hours[0].target) < 0.999
    }

    fn snap(&mut self) {
        let Some(at) = self.lens().last_input else { return };
        if at.elapsed() < SNAP_AFTER {
            return;
        }
        let i = self.nearest_stop();
        self.zoom_to_stop(i, None);
    }

    /// Recompute the level of detail from the live cell size.
    fn update_lod(&mut self) {
        let spec = self.spec();
        if spec.lod.is_empty() {
            return;
        }
        let arr = self.arr();
        let c = arr.cell((arr.start + arr.n * 0.5) as i32);
        let (w, h) = (c[2] - c[0], c[3] - c[1]);
        let extent = match spec.lod_extent {
            Extent::Height => h,
            Extent::Min => w.min(h),
        };
        let current = self.lens().mark;
        let mark = spec.lod_mark(extent, current);
        if mark != current {
            self.lenses[self.preset].mark = mark;
            self.dirty = true;
        }
    }

    fn relayout(&mut self) {
        let spec = *self.spec();
        let mark = self.lens().mark;
        let arr = self.arr();
        let targets = layout::solve(&self.data.events, &spec, mark, &self.hidden);
        for (i, t) in targets.into_iter().enumerate() {
            // Stagger by screen position so a lens change reads as a wave.
            let delay = if self.stagger {
                let c = arr.cell(t.day);
                0.12 * ((c[0] - arr.plot[0]) / arr.plot[2]).clamp(0., 1.)
            } else {
                0.
            };
            match self.marks.get_mut(i) {
                Some(m) => {
                    if m.target == t && !self.stagger {
                        continue;
                    }
                    let moved = m.target.kind != t.kind || m.target.g != t.g || m.target.day != t.day || self.stagger;
                    if moved
                        && let Some(shape) = m.shown.take() {
                            m.from = Some(shape);
                            m.t = Spring { target: 1., ..Spring::new(0.) };
                            m.delay = delay;
                            self.flipping.push(i);
                        }
                    if t.label == LabelMode::Hidden {
                        // Aggregating away: labels would only pile up as marks converge.
                        m.label = Spring::new(0.);
                    } else if m.label_mode != t.label {
                        m.label.target = 0.;
                    }
                    m.alpha.target = t.alpha;
                    if m.shown.is_none() && m.from.is_none() {
                        // Off screen: no one sees the transition, so skip it.
                        m.alpha = Spring::new(t.alpha);
                        m.label_mode = t.label;
                        m.label = Spring::new(1.);
                    }
                    m.target = t;
                }
                None => self.marks.push(MarkAnim {
                    target: t,
                    alpha: Spring { target: t.alpha, ..Spring::new(0.) },
                    from: None,
                    t: Spring::new(1.),
                    delay: 0.,
                    shown: None,
                    label: Spring::new(1.),
                    label_mode: t.label,
                    placed: Spring::new(0.),
                    hover: Spring::new(0.),
                }),
            }
        }
        self.flipping.sort_unstable();
        self.flipping.dedup();
        self.dirty = false;
        self.stagger = false;
    }

    fn step(&mut self, dt: f32) -> bool {
        let mut moving = false;
        // Morphs in flight.
        let mut still_flipping = Vec::with_capacity(self.flipping.len());
        for &i in &self.flipping {
            let m = &mut self.marks[i];
            if m.delay > 0. {
                m.delay -= dt;
                still_flipping.push(i);
                moving = true;
                continue;
            }
            if m.t.step(dt, GENTLE) {
                still_flipping.push(i);
                moving = true;
            } else {
                m.from = None;
            }
        }
        self.flipping = still_flipping;
        // Only marks on screen or mid-morph animate; everything else snaps when it
        // changes (see `relayout`), so cost tracks what is visible, not the dataset.
        let mut live = self.drawn_marks.clone();
        live.extend_from_slice(&self.flipping);
        live.extend(self.hovered);
        live.sort_unstable();
        live.dedup();
        for i in live {
            let m = &mut self.marks[i];
            moving |= m.alpha.step(dt, GENTLE);
            if m.label_mode != m.target.label && m.label.x < 0.05 {
                m.label_mode = m.target.label;
            }
            if m.label_mode == m.target.label {
                m.label.target = 1.;
            }
            moving |= m.label.step(dt, SNAPPY);
            moving |= m.placed.step(dt, SNAPPY);
            m.hover.target = if self.hovered == Some(i) { 1. } else { 0. };
            moving |= m.hover.step(dt, SNAPPY);
        }
        for (p, l) in self.lenses.iter_mut().enumerate() {
            let base = PRESETS[p].mark;
            l.heat.target = if l.mark == Mark::Heat { 1. } else { 0. };
            l.grid.target = if l.mark == base { 1. } else { 0. };
            let [h0, h1] = &mut l.hours;
            for s in [&mut l.zoom, &mut l.start, h0, h1] {
                moving |= s.step(dt, VIEWPORT);
            }
            moving |= l.heat.step(dt, GENTLE);
            moving |= l.grid.step(dt, GENTLE);
            // Keep frames coming until a pending snap has fired.
            moving |= p == self.preset && l.last_input.is_some();
        }
        for l in &mut self.layers {
            moving |= l.step(dt, GENTLE);
        }
        self.drawer.target = if self.selected.is_some() { 1. } else { 0. };
        let hovering = self.hovered.is_some() || self.hover_day.is_some() || self.hover_zone.is_some();
        self.tooltip.target = if hovering && self.selected.is_none() { 1. } else { 0. };
        self.help.target = if self.help_on { 1. } else { 0. };
        self.detail_t.target = if self.details[self.preset] == Detail::DaySpan { 1. } else { 0. };
        self.margin_l.target = self.gutter_target();
        // A row of hour labels per extra zone above a horizontal time axis.
        let rows = if self.spec().x == Axis::TimeOfDay { self.shown_zones().len() } else { 0 };
        self.margin_t.target = scene::MARGIN_T + scene::ZONE_ROW * rows as f32;
        self.margin_b.target = if self.aggregate() { CONTROLS_TWO_ROWS } else { CONTROLS_ONE_ROW };
        for (s, cfg) in [
            (&mut self.switcher, SNAPPY),
            (&mut self.drawer, GENTLE),
            (&mut self.tooltip, SNAPPY),
            (&mut self.help, SNAPPY),
            (&mut self.enc_t, GENTLE),
            (&mut self.detail_t, GENTLE),
            (&mut self.margin_l, GENTLE),
            (&mut self.margin_b, GENTLE),
            (&mut self.margin_t, GENTLE),
        ] {
            moving |= s.step(dt, cfg);
        }
        if self.selected.is_some() {
            self.shown = self.selected;
        }
        moving
    }

    fn hit(&self, pos: Pt) -> Option<usize> {
        self.hits.iter().rev().find(|(_, h)| h.contains(pos)).map(|(i, _)| *i)
    }

    fn plot_contains(&self, (x, y): Pt) -> bool {
        let (px, py, w, h) = self.plot();
        x >= px && x <= px + w && y >= py && y <= py + h
    }

    /// The day under the pointer when cells are overviews rather than detail.
    fn overview_day(&self, pos: Pt) -> Option<i32> {
        let arr = self.arr();
        let overview = self.lens().mark != self.spec().mark || arr.lines() > 1.3 || self.spec().x == Axis::None;
        (overview && self.plot_contains(pos)).then(|| arr.day_at(pos)).flatten()
    }

    fn over_chrome(&self, (x, y): Pt) -> bool {
        let drawer = self.selected.is_some() && x > self.viewport.0 - chrome::DRAWER_W - 24.;
        let slider = x > self.viewport.0 - scene::MARGIN_R + 4.;
        drawer || slider || y < chrome::TOPBAR_H || y > self.viewport.1 - self.margin_b.x + 6.
    }

    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        match k.key.as_str() {
            n @ ("1" | "2" | "3" | "4" | "5") => self.set_preset(n.parse::<usize>().unwrap() - 1),
            "tab" if k.modifiers.shift => self.set_preset((self.preset + PRESETS.len() - 1) % PRESETS.len()),
            "tab" => self.set_preset((self.preset + 1) % PRESETS.len()),
            "left" | "h" => self.pan(-1),
            "right" | "l" => self.pan(1),
            "up" | "k" if self.scrolls_hours() => self.scroll_hours(-1. / 24., 1., 0.5),
            "down" | "j" if self.scrolls_hours() => self.scroll_hours(1. / 24., 1., 0.5),
            "up" | "k" => self.pan(-1),
            "down" | "j" => self.pan(1),
            "-" => self.zoom_step(1),
            "=" | "+" => self.zoom_step(-1),
            "t" => self.go_today(),
            "c" => self.cycle_measure(),
            "g" => self.toggle_detail(),
            "z" => self.zones_on = !self.zones_on,
            "v" => self.cycle_compose(),
            "b" => {
                let mut enc = self.color();
                enc.by_calendar = !enc.by_calendar;
                self.set_color(enc);
            }
            "d" => {
                let dark = !self.theme.dark;
                self.theme_override = Some(dark);
                self.theme = theme_for(Some(dark), WindowAppearance::Light, self.translucent);
            }
            "?" => self.help_on = !self.help_on,
            "/" if k.modifiers.shift => self.help_on = !self.help_on,
            "`" => self.hud_on = !self.hud_on,
            "s" => {
                self.stress = !self.stress;
                self.set_data(model::generate(if self.stress { 3 } else { 1 }));
            }
            "escape" => {
                if self.help_on {
                    self.help_on = false;
                } else {
                    self.selected = None;
                }
            }
            _ => return,
        }
        cx.notify();
    }

    fn on_move(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        let pos: Pt = (ev.position.x.into(), ev.position.y.into());
        self.mouse = pos;
        let zone = self.zone_hits.borrow().iter().find(|(r, _)| pos.0 >= r[0] && pos.0 <= r[2] && pos.1 >= r[1] && pos.1 <= r[3]).map(|(_, z)| *z);
        if zone != self.hover_zone {
            self.hover_zone = zone;
            cx.notify();
        }
        let (hovered, day) = if self.over_chrome(pos) {
            (None, None)
        } else {
            let hit = self.hit(pos);
            (hit, if hit.is_none() && self.aggregate() { self.overview_day(pos) } else { None })
        };
        if hovered != self.hovered || day != self.hover_day || self.tooltip.x > 0.01 {
            self.hovered = hovered;
            self.hover_day = day;
            cx.notify();
        }
    }

    fn on_down(&mut self, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        let pos: Pt = (ev.position.x.into(), ev.position.y.into());
        if self.over_chrome(pos) {
            return;
        }
        if let Some(i) = self.hit(pos) {
            self.selected = Some(i);
        } else if let Some(day) = self.overview_day(pos) {
            // Drill down: an overview cell opens at the next level of detail.
            if self.spec().x == Axis::None {
                self.set_preset(0);
            }
            let spec = self.spec();
            let i = spec.stops.iter().position(|s| s.cells >= spec.facet.per_line).unwrap_or(0);
            self.zoom_to_stop(i, Some(day as f32 + 0.5));
        } else {
            self.selected = None;
        }
        cx.notify();
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let (dx, dy): (f32, f32) = match ev.delta {
            ScrollDelta::Pixels(p) => (p.x.into(), p.y.into()),
            ScrollDelta::Lines(l) => (l.x * 48., l.y * 48.),
        };
        if ev.modifiers.control || ev.modifiers.platform {
            // Semantic zoom around the pointer.
            self.zoom_by(-dy * 0.006, Some(self.mouse));
        } else if ev.modifiers.alt {
            let (x, y, w, h) = self.plot();
            let anchor = if self.spec().y == Axis::TimeOfDay { (self.mouse.1 - y) / h } else { (self.mouse.0 - x) / w };
            self.scroll_hours(0., (1. - dy * 0.003).clamp(0.5, 2.), anchor.clamp(0., 1.));
        } else if ev.modifiers.shift {
            self.scroll_cells(-(dx + dy));
        } else if self.scrolls_hours() && dy != 0. {
            let (_, _, w, h) = self.plot();
            let l = self.lens();
            let span = l.hours[1].target - l.hours[0].target;
            let px = if self.spec().y == Axis::TimeOfDay { h } else { w };
            self.scroll_hours(-dy / px * span, 1., 0.5);
            if dx != 0. {
                self.scroll_cells(-dx);
            }
        } else {
            self.scroll_cells(-(dy + dx));
        }
        cx.notify();
    }

    /// Benchmark script: lens changes and zoom sweeps, one step every 1.1 s.
    fn tick_bench(&mut self, interval: f32, cx: &mut Context<Self>) {
        let cpu = self.cpu_last.get();
        let Some(b) = &mut self.bench else { return };
        if b.start.elapsed() > Duration::from_millis(500) {
            b.stats.push(interval, cpu);
        }
        let advance = b.last_step.elapsed() > Duration::from_millis(1100);
        let step = b.step;
        if advance {
            b.last_step = Instant::now();
            b.step += 1;
        }
        let done = b.report && b.start.elapsed() > Duration::from_secs(22);
        if advance {
            // (lens, stop) pairs exercising morphs, level-of-detail changes and big zooms.
            const SCRIPT: [(usize, &str); 12] = [
                (0, "Month"),
                (0, "Quarter"),
                (0, "Year"),
                (0, "Week"),
                (1, "Week"),
                (1, "Day"),
                (2, "Day"),
                (3, "Day"),
                (3, "Week"),
                (4, "Quarter"),
                (4, "Year"),
                (0, "Week"),
            ];
            let (lens, stop) = SCRIPT[step % SCRIPT.len()];
            self.set_preset(lens);
            if let Some(i) = self.spec().stops.iter().position(|s| s.name == stop) {
                self.zoom_to_stop(i, None);
            }
        }
        let Some(b) = &self.bench else { return };
        if done {
            let s = &b.stats;
            println!(
                "bench: events={} frames={} interval ms p50={:.2} p95={:.2} p99={:.2} | cpu ms p50={:.2} p95={:.2} p99={:.2} max={:.2}",
                self.data.events.len(),
                s.intervals.len(),
                Stats::pct(&s.intervals, 0.5),
                Stats::pct(&s.intervals, 0.95),
                Stats::pct(&s.intervals, 0.99),
                Stats::pct(&s.cpu, 0.5),
                Stats::pct(&s.cpu, 0.95),
                Stats::pct(&s.cpu, 0.99),
                Stats::pct(&s.cpu, 1.0),
            );
            cx.quit();
        }
    }
}

fn theme_for(override_dark: Option<bool>, appearance: WindowAppearance, translucent: Option<f32>) -> Theme {
    let dark = override_dark.unwrap_or(matches!(appearance, WindowAppearance::Dark | WindowAppearance::VibrantDark));
    let theme = if dark { Theme::dark() } else { Theme::light() };
    match translucent {
        Some(opacity) => theme.translucent(opacity),
        None => theme,
    }
}

impl Render for CalmView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t0 = Instant::now();
        let size = window.viewport_size();
        let viewport = (f32::from(size.width), f32::from(size.height));
        self.stable_frames += 1;
        if viewport != self.viewport {
            self.stable_frames = 0;
            self.viewport = viewport;
        }

        // After idle, treat the first frame as a nominal 60 Hz step.
        let interval = match (self.animating, self.last_frame) {
            (true, Some(last)) => t0.duration_since(last).as_secs_f32(),
            _ => 1. / 60.,
        };
        if self.animating && self.last_frame.is_some() {
            self.stats.push(interval * 1000., self.cpu_last.get());
        }
        self.last_frame = Some(t0);
        if self.bench.as_ref().is_some_and(|b| b.report) && interval > 0.025 {
            eprintln!("long interval {:.1}ms after: {}", interval * 1000., self.last_trace);
        }
        self.tick_bench(interval * 1000., cx);

        self.snap();
        self.update_lod();
        if self.dirty {
            self.relayout();
        }
        self.sync_color();
        let dt = if self.still { 5. } else { interval.min(1. / 20.) };
        let first = self.stats.intervals.is_empty() && self.stable_frames <= 1;
        let moving = self.step(dt);
        if first {
            // Open at the right margins rather than animating into them.
            self.margin_l = Spring::new(self.margin_l.target);
            self.margin_b = Spring::new(self.margin_b.target);
            self.margin_t = Spring::new(self.margin_t.target);
        }
        let t_build = Instant::now();
        let built = self.build(window);
        let build_ms = t_build.elapsed().as_secs_f32() * 1000.;
        let trace = self.bench.as_ref().is_some_and(|b| b.report);
        let trace_info = format!(
            "lens={} n={:.0} mark={:?} flipping={} counts={:?} pre={:.1}",
            self.preset,
            self.arr().n,
            self.lens().mark,
            self.flipping.len(),
            built.counts,
            t_build.duration_since(t0).as_secs_f32() * 1000.
        );
        self.last_trace = trace_info.clone();
        self.drawn = built.counts;
        let waiting = self.pending.0.is_some() || self.pending.1.is_some();
        if waiting && self.stable_frames > 3 {
            if let Some(pos) = self.pending.0.take() {
                self.mouse = pos;
                self.hover_zone = self.zone_hits.borrow().iter().find(|(r, _)| pos.0 >= r[0] && pos.0 <= r[2] && pos.1 >= r[1] && pos.1 <= r[3]).map(|(_, z)| *z);
                self.hovered = self.hit(pos);
                self.hover_day = if self.hovered.is_none() { self.overview_day(pos) } else { None };
            }
            if let Some(pos) = self.pending.1.take() {
                self.selected = self.hit(pos);
            }
        }
        self.animating = moving || built.relabel || waiting || self.dirty || self.bench.is_some();
        if self.animating {
            window.request_animation_frame();
        }

        let frame = built.frame;
        let cpu_last = self.cpu_last.clone();
        let th = self.theme;

        div()
            .size_full()
            .bg(th.bg)
            .font_family("Inter")
            .text_color(th.ink)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev, _, cx| this.on_key(ev, cx)))
            .on_mouse_move(cx.listener(|this, ev, _, cx| this.on_move(ev, cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, ev, _, cx| this.on_down(ev, cx)))
            .on_scroll_wheel(cx.listener(|this, ev, _, cx| this.on_scroll(ev, cx)))
            .child(
                canvas(
                    |_, _, _| {},
                    move |_, _, window, cx| {
                        scene::paint(frame, window, cx);
                        let total = t0.elapsed().as_secs_f32() * 1000.;
                        cpu_last.set(total);
                        if trace && total > 10. {
                            eprintln!("slow: {trace_info} build {build_ms:.1} total {total:.1}ms");
                        }
                    },
                )
                .size_full(),
            )
            .when(!scene::skip("chrome"), |d| d.child(self.top_bar(cx)).child(self.zoom_slider(cx)).child(self.switcher(cx)))
            .children(self.tooltip_card())
            .children(self.drawer_card(cx))
            .children(self.help_card())
            .children(self.hud())
    }
}

#[derive(Default)]
pub struct Options {
    pub bench: bool,
    pub still: bool,
    pub cycle: bool,
    /// Background opacity; `None` is an opaque window.
    pub opacity: Option<f32>,
    /// Ask the compositor to blur what shows through.
    pub blur: bool,
    pub lens: Option<usize>,
    pub dark: Option<bool>,
    pub offset: i32,
    pub zoom: Option<String>,
    pub hover: Option<Pt>,
    pub select: Option<Pt>,
    /// `measure[:compose]`, e.g. `hours:stripes`; a compose implies by-calendar.
    pub color: Option<String>,
    pub span: bool,
    /// Extra time zones, overriding the config file: `America/New_York,Europe/London`.
    pub zones: Option<String>,
}

impl Options {
    pub fn from_args() -> Self {
        let mut o = Self::default();
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            match a.as_str() {
                "--bench" => o.bench = true,
                "--still" => o.still = true,
                "--cycle" => o.cycle = true,
                // Like a translucent terminal: a mostly solid background, no blur.
                "--translucent" => o.opacity = o.opacity.or(Some(0.8)),
                "--opacity" => o.opacity = args.next().and_then(|v| v.parse::<f32>().ok()).map(|v| v.clamp(0.2, 1.)),
                "--blur" => {
                    o.blur = true;
                    o.opacity = o.opacity.or(Some(0.8));
                }
                "--lens" => o.lens = args.next().and_then(|n| n.parse::<usize>().ok()).map(|n| n.saturating_sub(1)),
                "--theme" => o.dark = args.next().map(|t| t == "dark"),
                "--offset" => o.offset = args.next().and_then(|n| n.parse().ok()).unwrap_or(0),
                "--zoom" => o.zoom = args.next(),
                "--hover" => o.hover = args.next().and_then(|s| parse_pt(&s)),
                "--select" => o.select = args.next().and_then(|s| parse_pt(&s)),
                "--color" => o.color = args.next(),
                "--span" => o.span = true,
                "--zones" => o.zones = args.next(),
                _ => eprintln!("calm: ignoring unknown argument {a}"),
            }
        }
        o
    }
}

fn parse_pt(s: &str) -> Option<Pt> {
    let (x, y) = s.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}
