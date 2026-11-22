//! The grammar. A view is a declarative composition of orthogonal channels:
//!
//! * `facet` — how day cells are arranged on the plane: days wrap into
//!   *lines* of `per_line` cells; lines stack (rows or columns) and wrap into
//!   *bands* of `band` lines laid side by side. This is the nesting operator:
//!   `y = week / timeOfDay` reads "time of day, nested within weeks".
//! * `coord`, `x`, `y` — the coordinate system and axes *inside* a cell.
//! * `collide`, `mark` — how overlaps resolve and what geometry is drawn.
//! * `lod` — semantic zoom: which mark to draw as cells shrink.
//! * `stops` — snap points for the zoom, with how each aligns to the calendar.
//! * `color`, `detail` — what drives fill, and glyphs nested in aggregate cells.
//! * `params` — named values a view exposes for choosing in the UI. Channels
//!   refer to them as `$name`; how they are offered (segmented, toggle, a key)
//!   is a hint, and where they appear is up to the app.
//!
//! Zoom is not a channel: it is how many cells are visible. Views are data,
//! parsed from KDL (see `viewconf` and `docs/config.md`).

use std::collections::HashMap;

use crate::color::{ColorEnc, Compose, Measure, Palette};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Coord {
    Cartesian,
    Polar,
}

/// Axes inside one day cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    None,
    TimeOfDay,
    /// Sub-lanes created by collision; spans the cell when there is no overlap.
    Lane,
    Calendar,
    Demand,
}

/// Direction lines run in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    /// Cells left to right; lines stack top to bottom; bands side by side.
    Rows,
    /// Cells top to bottom; lines stack left to right; bands stack downwards.
    Columns,
}

#[derive(Clone, Copy, Debug)]
pub struct Facet {
    pub per_line: i32,
    pub dir: Dir,
    pub band: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Collide {
    Split,
    HighlightConflict,
    Overlay,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    /// Time-proportional blocks.
    Block,
    /// Constant-height pills on a time axis.
    Pill,
    /// One line per event, stacked in start order: a month-view cell.
    List,
    /// The cell itself, coloured by the colour channel.
    Heat,
}

/// Which extent of a cell drives level of detail.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Extent {
    /// Along the time axis: width when time runs across, else height. Thresholds then
    /// mean the same thing however the view is turned.
    Time,
    Height,
    Width,
    Min,
}

/// Below `below` pixels of cell extent, draw `mark`.
#[derive(Clone, Copy, Debug)]
pub struct Lod {
    pub below: f32,
    pub mark: Mark,
}

/// How a zoom stop aligns its first visible cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align {
    Cell,
    Line,
    /// Whole lines spanning one calendar month.
    Month,
    /// Multiples of the stop's own size, counted from the data origin.
    Group,
}

#[derive(Clone, Debug)]
pub struct Stop {
    pub name: String,
    pub cells: i32,
    pub align: Align,
}

/// Secondary encoding nested inside an aggregate cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Detail {
    None,
    /// A bar from the day's first start to its last end on a mini 6am–midnight axis.
    DaySpan,
}

/// A time-of-day series drawn along each day's time axis.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rail {
    /// Your energy (see `energy`), on the Demand colour scale.
    Energy,
    /// The sun's elevation where you are.
    Daylight,
}

/// Time-zone labelling of the time axis.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ZoneAxes {
    /// Every configured zone gets its own row or column of hour labels.
    All,
    /// Only the primary zone, e.g. where extra label rings would be clutter.
    Primary,
}

/// A channel value: written literally, or `$name` to take a param's value.
#[derive(Clone, Debug, PartialEq)]
pub enum Ref {
    Lit(String),
    Param(String),
}

impl Ref {
    pub fn parse(s: &str) -> Self {
        match s.strip_prefix('$') {
            Some(name) => Ref::Param(name.to_string()),
            None => Ref::Lit(s.to_string()),
        }
    }

}

#[derive(Clone, Debug)]
pub struct ColorRef {
    /// `calendar`, or a measure (`hours`, `events`, `demand`, `strain`, `free`,
    /// `starts`, `ends`, `span`; `duration` is hours per event).
    pub measure: Ref,
    /// Facet the measure by calendar (aggregates).
    pub split: Ref,
    /// `blend`, `stripes`, `stack` or `dominant`.
    pub compose: Ref,
    /// Where hue comes from: `scale` (the measure's own ramp) or `calendar` (each
    /// calendar's colour, with the measure setting its strength). A toggle param
    /// reads `true` as `calendar`.
    pub palette: Ref,
}

/// How a param is offered in the UI.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bind {
    Segmented,
    Toggle,
}

/// When a param or option applies.
#[derive(Clone, Debug, PartialEq)]
pub enum Cond {
    Mark(Mark),
    NotMark(Mark),
    /// `name=value`
    Param(String, String),
    /// `name!=value`, written `when-param-not="name=value"`
    NotParam(String, String),
}

#[derive(Clone, Debug)]
pub struct ParamOption {
    pub label: String,
    pub value: String,
    pub when: Vec<Cond>,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub label: Option<String>,
    pub bind: Bind,
    pub key: Option<String>,
    /// Control row, counted up from the lens switcher (1 is nearest).
    pub row: u8,
    pub default: String,
    /// Segmented choices; toggles are `true`/`false`.
    pub options: Vec<ParamOption>,
    pub when: Vec<Cond>,
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub name: String,
    pub facet: Facet,
    pub coord: Coord,
    pub x: Axis,
    pub y: Axis,
    pub collide: Collide,
    pub mark: Mark,
    pub lod: Vec<Lod>,
    pub lod_extent: Extent,
    pub stops: Vec<Stop>,
    /// The inner time window the view opens with, as fractions of a day.
    pub hours: (f32, f32),
    pub zones: ZoneAxes,
    pub params: Vec<Param>,
    pub color: ColorRef,
    /// Whether aggregate cells carry the day-span glyph.
    pub day_span: Ref,
    /// A series drawn beside each day's time axis (`off`, `energy`, `daylight`), and how
    /// (`rail`: width and colour follow it; `strip`: colour only).
    pub rail: Ref,
    pub rail_style: Ref,
    /// Whether all-day events show: as bars above the time axis, rings inside dials,
    /// leading rows in lists, or stripes across aggregate cells.
    pub all_day: bool,
}

/// Current param values for one view, by name.
pub type Values = HashMap<String, String>;

impl Spec {
    /// The mark to draw at a given cell extent, with hysteresis around the
    /// thresholds so a cell hovering near one doesn't flicker between marks.
    pub fn lod_mark(&self, extent: f32, current: Mark) -> Mark {
        let hyst = 1.08;
        let mut mark = self.mark;
        for rule in &self.lod {
            let threshold = if current == mark { rule.below / hyst } else { rule.below * hyst };
            if extent < threshold {
                mark = rule.mark;
            } else {
                break;
            }
        }
        mark
    }

    pub fn defaults(&self) -> Values {
        self.params.iter().map(|p| (p.name.clone(), p.default.clone())).collect()
    }

    pub fn param(&self, name: &str) -> Option<&Param> {
        self.params.iter().find(|p| p.name == name)
    }

    fn holds(&self, conds: &[Cond], values: &Values, mark: Mark) -> bool {
        conds.iter().all(|c| match c {
            Cond::Mark(m) => *m == mark,
            Cond::NotMark(m) => *m != mark,
            Cond::Param(name, want) => self.value(name, values, mark) == *want,
            Cond::NotParam(name, want) => self.value(name, values, mark) != *want,
        })
    }

    /// Option conditions test raw values, which can't cycle back into `value`.
    fn holds_raw(&self, conds: &[Cond], values: &Values, mark: Mark) -> bool {
        conds.iter().all(|c| match c {
            Cond::Mark(m) => *m == mark,
            Cond::NotMark(m) => *m != mark,
            Cond::Param(name, want) => values.get(name).is_some_and(|v| v == want),
            Cond::NotParam(name, want) => values.get(name).is_none_or(|v| v != want),
        })
    }

    /// Whether a param is offered at all right now.
    pub fn active(&self, p: &Param, values: &Values, mark: Mark) -> bool {
        self.holds(&p.when, values, mark)
    }

    /// A param's options that apply right now.
    pub fn options<'a>(&'a self, p: &'a Param, values: &'a Values, mark: Mark) -> impl Iterator<Item = &'a ParamOption> + 'a {
        p.options.iter().filter(move |o| self.holds_raw(&o.when, values, mark))
    }

    /// A param's effective value: off (toggles) or its default (choices) when
    /// inactive, and for choices
    /// that don't apply right now (say, after zooming to aggregates) the first
    /// that does. The stored value survives, so zooming back restores it.
    pub fn value(&self, name: &str, values: &Values, mark: Mark) -> String {
        let Some(p) = self.param(name) else { return String::new() };
        // Param conditions may reference other params; guard against self-reference.
        let deps_ok = p.when.iter().all(|c| match c {
            Cond::Param(n, want) if n != name => self.value(n, values, mark) == *want,
            Cond::NotParam(n, want) if n != name => self.value(n, values, mark) != *want,
            Cond::Param(..) | Cond::NotParam(..) => true,
            Cond::Mark(m) => *m == mark,
            Cond::NotMark(m) => *m != mark,
        });
        if !deps_ok {
            // A toggle that isn't offered right now is off; a choice falls back to its default.
            return if p.bind == Bind::Toggle { "false".into() } else { p.default.clone() };
        }
        let v = values.get(name).cloned().unwrap_or_else(|| p.default.clone());
        if p.bind == Bind::Toggle || p.options.is_empty() {
            return v;
        }
        let mut opts = p.options.iter().filter(|o| self.holds_raw(&o.when, values, mark));
        let first = opts.clone().next().map(|o| o.value.clone());
        if opts.any(|o| o.value == v) { v } else { first.unwrap_or(v) }
    }

    fn resolve(&self, r: &Ref, values: &Values, mark: Mark) -> String {
        match r {
            Ref::Lit(s) => s.clone(),
            Ref::Param(name) => self.value(name, values, mark),
        }
    }

    /// The colour encoding these values select.
    pub fn color_enc(&self, values: &Values, mark: Mark) -> ColorEnc {
        let measure = self.resolve(&self.color.measure, values, mark);
        let split = self.resolve(&self.color.split, values, mark) == "true";
        let compose = Compose::from_name(&self.resolve(&self.color.compose, values, mark)).unwrap_or(Compose::Blend);
        let palette = match self.resolve(&self.color.palette, values, mark).as_str() {
            "calendar" | "true" => Palette::Calendar,
            _ => Palette::Scale,
        };
        match measure.as_str() {
            "calendar" => ColorEnc { measure: Measure::Hours, by_calendar: true, compose, palette: Palette::Calendar },
            // Splitting is for aggregates; events take calendar colour through `palette`.
            m => ColorEnc { measure: Measure::from_name(m).unwrap_or(Measure::Hours), by_calendar: split && mark == Mark::Heat, compose, palette },
        }
    }

    /// What the rail shows, if anything, and whether its width follows the value (`rail`)
    /// or only its colour (`strip`).
    pub fn rail(&self, values: &Values, mark: Mark) -> Option<(Rail, bool)> {
        let kind = match self.resolve(&self.rail, values, mark).as_str() {
            "energy" => Rail::Energy,
            "daylight" => Rail::Daylight,
            _ => return None,
        };
        Some((kind, self.resolve(&self.rail_style, values, mark) != "strip"))
    }

    pub fn detail(&self, values: &Values, mark: Mark) -> Detail {
        if self.resolve(&self.day_span, values, mark) == "true" { Detail::DaySpan } else { Detail::None }
    }

    /// Plain-language description of the grammar, for the caption.
    pub fn describe(&self, mark: Mark) -> String {
        let nest = match (self.facet.per_line, self.facet.dir) {
            (1, _) => "day",
            (_, Dir::Rows) => "week / day",
            (_, Dir::Columns) => "week | day",
        };
        let axis = |a: Axis| match a {
            Axis::None => "",
            Axis::TimeOfDay => "time",
            Axis::Lane => "lane",
            Axis::Calendar => "calendar",
            Axis::Demand => "demand",
        };
        let coord = if self.coord == Coord::Polar { "polar " } else { "" };
        let mark = match mark {
            Mark::Block if self.coord == Coord::Polar => "arcs",
            Mark::Block => "blocks",
            Mark::Pill => "pills",
            Mark::List => "list",
            Mark::Heat => "heat",
        };
        if self.x == Axis::None {
            format!("{nest} · {mark}")
        } else {
            format!("{nest} ⟶ {coord}{} × {} · {mark}", axis(self.x), axis(self.y))
        }
    }
}
