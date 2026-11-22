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
//!
//! Zoom is not a channel: it is how many cells are visible. Everything else
//! is derived, so zooming a lens out from a week to a year is continuous.

use crate::color::{ColorEnc, Compose, Measure};

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
    /// Annular sectors (polar blocks).
    Arc,
    /// One line per event, stacked in start order: a month-view cell.
    List,
    /// The cell itself, coloured by the colour channel.
    Heat,
}

/// Which extent of a cell drives level of detail.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Extent {
    Height,
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

#[derive(Clone, Copy, Debug)]
pub struct Stop {
    pub name: &'static str,
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

#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub name: &'static str,
    pub facet: Facet,
    pub coord: Coord,
    pub x: Axis,
    pub y: Axis,
    pub collide: Collide,
    pub mark: Mark,
    pub lod: &'static [Lod],
    pub lod_extent: Extent,
    pub stops: &'static [Stop],
    /// Default colour encoding; the view keeps per-lens overrides.
    pub color: ColorEnc,
    pub detail: Detail,
}

const DAY: Stop = Stop { name: "Day", cells: 1, align: Align::Cell };
const WEEK: Stop = Stop { name: "Week", cells: 7, align: Align::Line };
const MONTH: Stop = Stop { name: "Month", cells: 35, align: Align::Month };
/// A week of day-rows, Monday first.
const WEEK_ROWS: Stop = Stop { name: "Week", cells: 7, align: Align::Group };
const QUARTER: Stop = Stop { name: "Quarter", cells: 91, align: Align::Group };
const YEAR: Stop = Stop { name: "Year", cells: 364, align: Align::Group };

const BLEND: ColorEnc = ColorEnc { measure: Measure::Hours, by_calendar: true, compose: Compose::Blend };

pub const PRESETS: [Spec; 5] = [
    Spec {
        name: "Week",
        facet: Facet { per_line: 7, dir: Dir::Rows, band: 13 },
        coord: Coord::Cartesian,
        x: Axis::Lane,
        y: Axis::TimeOfDay,
        collide: Collide::Split,
        mark: Mark::Block,
        lod: &[Lod { below: 240., mark: Mark::List }, Lod { below: 60., mark: Mark::Heat }],
        lod_extent: Extent::Height,
        stops: &[DAY, WEEK, MONTH, QUARTER, YEAR],
        color: BLEND,
        detail: Detail::None,
    },
    Spec {
        name: "Lanes",
        facet: Facet { per_line: 1, dir: Dir::Rows, band: 7 },
        coord: Coord::Cartesian,
        x: Axis::TimeOfDay,
        y: Axis::Calendar,
        collide: Collide::HighlightConflict,
        mark: Mark::Pill,
        lod: &[Lod { below: 40., mark: Mark::Heat }],
        lod_extent: Extent::Height,
        stops: &[DAY, WEEK_ROWS, Stop { name: "Month", cells: 28, align: Align::Group }],
        color: BLEND,
        detail: Detail::None,
    },
    Spec {
        name: "Energy",
        facet: Facet { per_line: 1, dir: Dir::Rows, band: 7 },
        coord: Coord::Cartesian,
        x: Axis::TimeOfDay,
        y: Axis::Demand,
        collide: Collide::Overlay,
        mark: Mark::Pill,
        lod: &[Lod { below: 40., mark: Mark::Heat }],
        lod_extent: Extent::Height,
        stops: &[DAY, WEEK_ROWS],
        color: BLEND,
        detail: Detail::None,
    },
    Spec {
        name: "Dial",
        facet: Facet { per_line: 7, dir: Dir::Rows, band: 13 },
        coord: Coord::Polar,
        x: Axis::TimeOfDay,
        y: Axis::Calendar,
        collide: Collide::Split,
        mark: Mark::Arc,
        lod: &[Lod { below: 72., mark: Mark::Heat }],
        lod_extent: Extent::Min,
        stops: &[DAY, WEEK, MONTH],
        color: BLEND,
        detail: Detail::None,
    },
    Spec {
        name: "Quarter",
        facet: Facet { per_line: 7, dir: Dir::Columns, band: 26 },
        coord: Coord::Cartesian,
        x: Axis::None,
        y: Axis::None,
        collide: Collide::Overlay,
        mark: Mark::Heat,
        lod: &[],
        lod_extent: Extent::Height,
        stops: &[
            Stop { name: "Quarter", cells: 84, align: Align::Line },
            Stop { name: "Half", cells: 182, align: Align::Group },
            YEAR,
        ],
        color: ColorEnc { measure: Measure::Hours, by_calendar: false, compose: Compose::Stripes },
        detail: Detail::None,
    },
];

impl Spec {
    /// The mark to draw at a given cell extent, with hysteresis around the
    /// thresholds so a cell hovering near one doesn't flicker between marks.
    pub fn lod_mark(&self, extent: f32, current: Mark) -> Mark {
        let hyst = 1.08;
        let mut mark = self.mark;
        for rule in self.lod {
            let threshold = if current == mark { rule.below / hyst } else { rule.below * hyst };
            if extent < threshold {
                mark = rule.mark;
            } else {
                break;
            }
        }
        mark
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
            Mark::Block => "blocks",
            Mark::Pill => "pills",
            Mark::Arc => "arcs",
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
