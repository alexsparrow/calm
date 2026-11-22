//! Layout solver: collision and projection into *inner* cell coordinates.
//!
//! Every mark belongs to one day cell. Its geometry is expressed inside that
//! cell (time of day as a 0..1 fraction of the day, categorical axes as 0..1
//! bands), so the facet arrangement alone decides where cells sit on screen.
//! Zooming and scrolling therefore never relayout; only semantic changes do
//! (a different lens, level of detail, or hidden calendars).

use crate::model::{self, Event, MAX_CALENDARS, Status};
use crate::spec::{Axis, Collide, Mark, Spec};

const N: usize = MAX_CALENDARS;

/// Inner geometry indices into [`Target::g`].
pub const U0: usize = 0;
pub const U1: usize = 1;
pub const V0: usize = 2;
pub const V1: usize = 3;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    /// `g` is an inner rectangle (or annular sector in polar coordinates).
    Rect,
    /// `g` gives the time span and a band; the pill is `row` of `rows`
    /// stacked rows within the band, with a constant pixel height.
    Pill { row: u16, rows: u16 },
    /// A line in the cell's event list.
    Row { rank: u16 },
    /// The whole cell (aggregated away).
    Cell,
    /// An all-day piece, stacked in `row`: a band above the time axis, a ring inside a
    /// dial, or a stripe across an aggregate cell, depending on the mark and coordinates.
    AllDay { row: u16, prev: bool, next: bool },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LabelMode {
    Inside,
    Overflow,
    Arc,
    Row,
    Hidden,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Target {
    pub day: i32,
    pub g: [f32; 4],
    pub kind: Kind,
    pub alpha: f32,
    pub label: LabelMode,
    pub conflict: bool,
}

fn band(axis: Axis, e: &Event, lane_of: &[usize; N], lanes: f32) -> (f32, f32) {
    match axis {
        Axis::TimeOfDay => (e.start_min / 1440., e.end_min() / 1440.),
        Axis::Lane | Axis::None => (0., 1.),
        Axis::Calendar => {
            let l = lane_of[e.calendar] as f32;
            (l / lanes, (l + 1.) / lanes)
        }
        Axis::Demand => {
            let v = model::energy_v(e.energy);
            (v, v)
        }
    }
}

/// Lays out every event for `spec` drawn with `mark` (the current level of detail).
pub fn solve(events: &[Event], spec: &Spec, mark: Mark, hidden: &[bool; N]) -> Vec<Target> {
    let mut lane_of = [N; N];
    let mut next = 0;
    for (c, lane) in lane_of.iter_mut().enumerate() {
        if !hidden[c] {
            *lane = next;
            next += 1;
        }
    }
    let lanes = next.max(1) as f32;
    let shown = |e: &Event| !hidden[e.calendar] && (e.all_day.is_none() || spec.all_day);
    let alpha = |e: &Event| match (shown(e), e.status) {
        (false, _) => 0.,
        (true, Status::Tentative) => 0.6,
        (true, Status::Confirmed) => 1.,
    };

    // All-day pieces: their own kind at every level of detail except lists, where they lead.
    let all_day = |e: &Event| {
        e.all_day.map(|a| Target {
            day: e.day,
            g: [0., 1., 0., 1.],
            kind: Kind::AllDay { row: a.row, prev: a.prev, next: a.next },
            alpha: alpha(e),
            // Dials title their rings along the curve; aggregate stripes are too thin for text.
            label: if mark == Mark::Heat {
                LabelMode::Hidden
            } else if spec.coord == crate::spec::Coord::Polar {
                LabelMode::Arc
            } else {
                LabelMode::Row
            },
            conflict: false,
        })
    };
    match mark {
        Mark::Heat => {
            return events
                .iter()
                .map(|e| all_day(e).unwrap_or(Target { day: e.day, g: [0., 1., 0., 1.], kind: Kind::Cell, alpha: 0., label: LabelMode::Hidden, conflict: false }))
                .collect();
        }
        Mark::List => {
            // Events are sorted by day; rank visible ones by start within each day.
            let mut out: Vec<Target> = events
                .iter()
                .map(|e| Target { day: e.day, g: [0., 1., 0., 1.], kind: Kind::Row { rank: 0 }, alpha: alpha(e), label: LabelMode::Row, conflict: false })
                .collect();
            let mut i = 0;
            while i < events.len() {
                let day = events[i].day;
                let mut j = i;
                while j < events.len() && events[j].day == day {
                    j += 1;
                }
                let mut idx: Vec<usize> = (i..j).filter(|&k| shown(&events[k])).collect();
                // All day first (in stack order), then by start.
                let key = |k: usize| (events[k].all_day.map_or(u16::MAX, |a| a.row), events[k].start_min);
                idx.sort_by(|&a, &b| key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal));
                for (rank, k) in idx.into_iter().enumerate() {
                    out[k].kind = Kind::Row { rank: rank as u16 };
                }
                i = j;
            }
            return out;
        }
        _ => {}
    }

    // Blocks bend into arcs in polar coordinates, and their labels follow.
    let label = match mark {
        Mark::Block if spec.coord == crate::spec::Coord::Polar => LabelMode::Arc,
        Mark::Block => LabelMode::Inside,
        Mark::Pill => LabelMode::Overflow,
        _ => LabelMode::Arc,
    };
    let kind = if mark == Mark::Pill { Kind::Pill { row: 0, rows: 1 } } else { Kind::Rect };
    let mut out: Vec<Target> = events
        .iter()
        .map(|e| {
            let (u0, u1) = band(spec.x, e, &lane_of, lanes);
            let (v0, v1) = band(spec.y, e, &lane_of, lanes);
            all_day(e).unwrap_or(Target { day: e.day, g: [u0, u1, v0, v1], kind, alpha: alpha(e), label, conflict: false })
        })
        .collect();

    // Collision: overlaps on the time axis within one (day, category) group.
    let axes = if spec.x == Axis::TimeOfDay {
        Some((U0, U1, V0, V1))
    } else if spec.y == Axis::TimeOfDay {
        Some((V0, V1, U0, U1))
    } else {
        None
    };
    let Some(axes) = axes else { return out };
    if spec.collide == Collide::Overlay {
        return out;
    }
    let (t_lo, _, c_lo, _) = axes;
    // All-day pieces stay out of the time axis's lanes.
    let mut order: Vec<usize> = (0..out.len()).filter(|&i| shown(&events[i]) && events[i].all_day.is_none()).collect();
    order.sort_by(|&a, &b| {
        let (ta, tb) = (&out[a], &out[b]);
        ta.day.cmp(&tb.day).then(ta.g[c_lo].total_cmp(&tb.g[c_lo])).then(ta.g[t_lo].total_cmp(&tb.g[t_lo]))
    });
    let mut i = 0;
    while i < order.len() {
        let (day, key) = (out[order[i]].day, out[order[i]].g[c_lo]);
        let mut j = i;
        while j < order.len() && out[order[j]].day == day && out[order[j]].g[c_lo] == key {
            j += 1;
        }
        assign_lanes(&mut out, &order[i..j], axes, mark, spec.collide);
        i = j;
    }
    out
}

/// Greedy interval colouring within overlap clusters.
fn assign_lanes(out: &mut [Target], group: &[usize], axes: (usize, usize, usize, usize), mark: Mark, collide: Collide) {
    let (t_lo, t_hi, c_lo, c_hi) = axes;
    let mut assigned: Vec<(usize, usize, usize)> = Vec::with_capacity(group.len());
    let mut start = 0;
    while start < group.len() {
        let mut end = start + 1;
        let mut cluster_hi = out[group[start]].g[t_hi];
        while end < group.len() && out[group[end]].g[t_lo] < cluster_hi {
            cluster_hi = cluster_hi.max(out[group[end]].g[t_hi]);
            end += 1;
        }
        let mut lane_ends: Vec<f32> = Vec::new();
        let first = assigned.len();
        for &idx in &group[start..end] {
            let lo = out[idx].g[t_lo];
            let lane = lane_ends.iter().position(|&e| e <= lo).unwrap_or_else(|| {
                lane_ends.push(0.);
                lane_ends.len() - 1
            });
            lane_ends[lane] = out[idx].g[t_hi];
            assigned.push((idx, lane, 0));
        }
        for a in &mut assigned[first..] {
            a.2 = lane_ends.len();
        }
        start = end;
    }
    let rows = assigned.iter().map(|a| a.2).max().unwrap_or(1);
    for (idx, lane, n) in assigned {
        let t = &mut out[idx];
        if mark == Mark::Pill {
            t.kind = Kind::Pill { row: lane as u16, rows: rows as u16 };
        } else {
            let (lo, hi) = (t.g[c_lo], t.g[c_hi]);
            let w = (hi - lo) / n as f32;
            t.g[c_lo] = lo + w * lane as f32;
            t.g[c_hi] = lo + w * (lane as f32 + 1.);
        }
        t.conflict = collide == Collide::HighlightConflict && n > 1;
    }
}
