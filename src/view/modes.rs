//! Deliberate editing. Clicks and drags only ever select, until you enter a mode:
//!
//! * **New** (S for schedule, or + at the top): click where the event starts, then where it ends.
//! * **Adjust** (A, or ↕ at the top): drag an event to move it, or one of its ends to resize it.
//!
//! Both snap to a step you choose and can keep to your free time. Each is also driven from
//! the keyboard: arrows (or hjkl) move the cursor or the selected event, Enter places,
//! [ and ] change the step, F toggles free time only, Esc steps back.
//!
//! Times here are "abs" minutes: minutes since local midnight on `Dataset::origin`.

use chrono::{DateTime, Local, NaiveDate, TimeZone, Timelike, Utc};
use gpui::{Context, CursorStyle, FontWeight, div, prelude::*, px};
use serde_json::json;

use super::arrange::Pt;
use super::editing::Undo;
use super::{CalmView, Hit, chrome};
use crate::model::{EditRef, Status};
use crate::spec::{Axis, Coord, Dir, Mark};
use crate::sync::edit;
use crate::sync::outbox::{Origin, State};
use crate::theme::with_alpha;

/// The steps you can snap to, in minutes.
pub(super) const SNAPS: [i64; 5] = [5, 10, 15, 30, 60];
const DAY: i64 = 1440;
/// Seconds a reschedule waits before sending: as long as its undo is on screen.
const HOLD: i64 = 8;

/// A point in time you can place: a snapped time, or a whole day (`timed` false).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Slot {
    pub abs: i64,
    pub timed: bool,
}

pub(super) enum Mode {
    Create { start: Option<Slot> },
    Adjust { drag: Option<Drag> },
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Grip {
    Body,
    Start,
    End,
}

/// An event being dragged in Adjust mode.
pub(super) struct Drag {
    key: String,
    id: String,
    grip: Grip,
    all_day: bool,
    press: Pt,
    /// Where the pointer was pressed, unsnapped.
    anchor: f64,
    orig: (i64, i64),
    now: (i64, i64),
    /// Past the few pixels that tell a drag from a click.
    live: bool,
}

/// What to draw for the mode: a span on the calendar and its label.
pub(super) struct Ghost {
    pub span: (i64, i64),
    pub timed: bool,
    /// Just the cursor (nothing placed yet): drawn lighter.
    pub cursor: bool,
    /// It would land on busy time while you keep to free time.
    pub blocked: bool,
    /// The events it would overlap (by index), when you allow that: outlined, and named in the label.
    pub overlaps: Vec<usize>,
    pub label: String,
}

impl CalmView {
    pub(super) fn creating(&self) -> bool {
        matches!(self.mode, Some(Mode::Create { .. }))
    }

    pub(super) fn adjusting(&self) -> bool {
        matches!(self.mode, Some(Mode::Adjust { .. }))
    }

    /// Enters a mode, or leaves it if it's the one you're in.
    pub(super) fn toggle_mode(&mut self, create: bool) {
        let same = if create { self.creating() } else { self.adjusting() };
        self.mode = if same {
            None
        } else if create {
            Some(Mode::Create { start: None })
        } else {
            Some(Mode::Adjust { drag: None })
        };
        self.cursor = None;
        self.hover_grip = None;
        if create {
            self.selected = None;
        }
    }

    /// Whether the view places events at times of day (rather than on whole days).
    pub(super) fn timed_view(&self) -> bool {
        let spec = self.spec();
        (spec.x == Axis::TimeOfDay || spec.y == Axis::TimeOfDay) && matches!(self.lens().mark, Mark::Block | Mark::Pill)
    }

    /// Whether the time of day runs across the screen (so left and right move in time).
    fn time_across(&self) -> bool {
        let spec = self.spec();
        spec.coord == Coord::Cartesian && spec.x == Axis::TimeOfDay
    }

    fn snap_step(&self, timed: bool) -> i64 {
        if timed { self.snap } else { DAY }
    }

    /// The time under the pointer, unsnapped; above a day's time axis (its all-day band) is the day.
    fn point_at(&self, pos: Pt) -> Option<(f64, bool)> {
        let arr = self.arr();
        let day = arr.day_at(pos)?;
        let at_day = (day as i64 * DAY) as f64;
        if !self.timed_view() {
            return Some((at_day, false));
        }
        let inner = self.inner(self.preset, arr.cell_inset(day));
        let spec = self.spec();
        if spec.coord == Coord::Cartesian && spec.y == Axis::TimeOfDay && pos.1 < inner.cell[1] {
            return Some((at_day, false));
        }
        let (u, v) = inner.unmap(pos);
        let f = if spec.x == Axis::TimeOfDay { u } else { v };
        Some((at_day + f.clamp(0., 1.) as f64 * DAY as f64, true))
    }

    pub(super) fn slot_at(&self, pos: Pt) -> Option<Slot> {
        let (abs, timed) = self.point_at(pos)?;
        let step = self.snap_step(timed) as f64;
        let abs = if timed { (abs / step).round() * step } else { (abs / step).floor() * step };
        Some(Slot { abs: abs as i64, timed })
    }

    // --- Time conversions --------------------------------------------------------------------

    pub(super) fn abs_of(&self, t: DateTime<Utc>) -> i64 {
        let l = t.with_timezone(&Local).naive_local();
        (l.date() - self.data.origin).num_days() * DAY + (l.time().num_seconds_from_midnight() / 60) as i64
    }

    fn time_of(&self, abs: i64) -> DateTime<Utc> {
        let naive = self.data.origin.and_hms_opt(0, 0, 0).unwrap() + chrono::Duration::minutes(abs);
        Local.from_local_datetime(&naive).earliest().map(|t| t.with_timezone(&Utc)).unwrap_or_else(|| naive.and_utc())
    }

    fn date_of(&self, abs: i64) -> NaiveDate {
        self.data.date(abs.div_euclid(DAY) as i32)
    }

    /// An event's span; all-day events count whole days (their ends are dates).
    fn span_of(&self, r: &EditRef) -> (i64, i64) {
        if r.all_day {
            let d = |t: DateTime<Utc>| (t.date_naive() - self.data.origin).num_days() * DAY;
            (d(r.start), d(r.end))
        } else {
            (self.abs_of(r.start), self.abs_of(r.end))
        }
    }

    fn describe(&self, (a, b): (i64, i64), timed: bool) -> String {
        let date = |abs: i64| self.date_of(abs).format("%a %-d %b").to_string();
        if !timed {
            let days = (b - a) / DAY;
            return if days <= 1 { format!("{} · all day", date(a)) } else { format!("{} – {} · {days} days", date(a), date(b - 1)) };
        }
        let min = |abs: i64| abs.rem_euclid(DAY) as f32;
        let len = b - a;
        let dur = match (len / 60, len % 60) {
            (0, m) => format!("{m}m"),
            (h, 0) => format!("{h}h"),
            (h, m) => format!("{h}h {m}m"),
        };
        if a.div_euclid(DAY) == (b - 1).div_euclid(DAY) {
            format!("{} · {} · {dur}", date(a), super::scene::fmt_range(min(a), min(b)))
        } else {
            format!("{} {} – {} {} · {dur}", date(a), super::scene::fmt_time(min(a)), date(b), super::scene::fmt_time(min(b)))
        }
    }

    // --- Free time ---------------------------------------------------------------------------

    /// Busy time: confirmed timed events in the calendars you're showing (not secondary ones).
    fn busy(&self, except: Option<&str>) -> Vec<(i64, i64, usize)> {
        self.data
            .events
            .iter()
            .enumerate()
            .filter(|(_, e)| e.all_day.is_none() && e.status == Status::Confirmed && !self.hidden[e.calendar])
            .filter(|(_, e)| !self.calendars.get(e.calendar).is_some_and(|c| c.secondary))
            .filter(|(_, e)| except.is_none_or(|id| e.edit.as_ref().is_none_or(|r| r.id != id)))
            .map(|(i, e)| {
                let a = e.day as i64 * DAY + e.start_min.round() as i64;
                (a, a + e.dur_min.round() as i64, i)
            })
            .collect()
    }

    /// The busy events overlapping `a..b`, one index per event (not per day piece).
    fn overlapping(&self, (a, b): (i64, i64), except: Option<&str>) -> Vec<usize> {
        let mut seen = std::collections::HashSet::new();
        self.busy(except)
            .into_iter()
            .filter(|&(s, e, _)| e > a && s < b)
            .map(|(_, _, i)| i)
            .filter(|&i| seen.insert(self.data.events[i].group))
            .collect()
    }

    fn is_free(&self, span: (i64, i64), except: Option<&str>) -> bool {
        self.overlapping(span, except).is_empty()
    }

    /// "Lunch" or "Lunch +2": what a span would overlap.
    fn overlap_note(&self, overlaps: &[usize]) -> Option<String> {
        let first = self.data.events.get(*overlaps.first()?)?.title.to_string();
        Some(match overlaps.len() {
            1 => format!("overlaps {first}"),
            n => format!("overlaps {first} +{}", n - 1),
        })
    }

    /// The span from where you started to the cursor, kept clear of busy time if you asked.
    fn span_between(&self, s: Slot, c: Slot) -> (i64, i64) {
        if !s.timed {
            let (a, b) = (s.abs.min(c.abs), s.abs.max(c.abs));
            return (a.div_euclid(DAY) * DAY, b.div_euclid(DAY) * DAY + DAY);
        }
        let busy = if self.free_only { self.busy(None) } else { Vec::new() };
        if c.abs >= s.abs {
            let end = if c.abs == s.abs { s.abs + 60 } else { c.abs };
            let wall = busy.iter().filter(|&&(b0, _, _)| b0 >= s.abs).map(|&(b0, _, _)| b0).min().unwrap_or(i64::MAX);
            (s.abs, end.min(wall).max(s.abs + self.snap))
        } else {
            let wall = busy.iter().filter(|&&(_, b1, _)| b1 <= s.abs).map(|&(_, b1, _)| b1).max().unwrap_or(i64::MIN);
            (c.abs.max(wall).min(s.abs - self.snap), s.abs)
        }
    }

    /// What the mode shows on the calendar right now.
    pub(super) fn ghost(&self) -> Option<Ghost> {
        match self.mode.as_ref()? {
            Mode::Create { start } => {
                let c = self.cursor?;
                let (span, timed, cursor) = match start {
                    None => ((c.abs, c.abs + self.snap_step(c.timed)), c.timed, true),
                    Some(s) => (self.span_between(*s, c), s.timed, false),
                };
                let overlaps = if timed { self.overlapping(if cursor { (span.0, span.0 + 1) } else { span }, None) } else { Vec::new() };
                let label = if cursor && timed { format!("{} · {}", self.date_of(span.0).format("%a %-d %b"), super::scene::fmt_time(span.0.rem_euclid(DAY) as f32)) } else { self.describe(span, timed) };
                Some(self.ghost_of(span, timed, cursor, overlaps, label))
            }
            Mode::Adjust { drag: Some(d) } if d.live => {
                let overlaps = if d.all_day { Vec::new() } else { self.overlapping(d.now, Some(&d.id)) };
                Some(self.ghost_of(d.now, !d.all_day, false, overlaps, self.describe(d.now, !d.all_day)))
            }
            _ => None,
        }
    }

    /// Busy time under the ghost is refused with free time only, and otherwise flagged: the
    /// overlapped events are outlined and named.
    fn ghost_of(&self, span: (i64, i64), timed: bool, cursor: bool, overlaps: Vec<usize>, label: String) -> Ghost {
        let blocked = self.free_only && !overlaps.is_empty();
        // Red with free time only (it won't place), amber otherwise: the colour says which.
        let label = match self.overlap_note(&overlaps) {
            Some(note) => format!("{label} · {note}"),
            None => label,
        };
        Ghost { span, timed, cursor, blocked, overlaps, label }
    }

    // --- Pointer -----------------------------------------------------------------------------

    /// The pointer moved: the cursor follows it, or the dragged event does. True if anything changed.
    pub(super) fn mode_move(&mut self, pos: Pt) -> bool {
        let slot = if self.over_chrome(pos) { None } else { self.slot_at(pos) };
        if self.creating() {
            let changed = slot.is_some() && slot != self.cursor;
            if slot.is_some() {
                self.cursor = slot;
            }
            return changed;
        }
        let point = self.point_at(pos);
        let snap = self.snap;
        let Some(Mode::Adjust { drag }) = &mut self.mode else { return false };
        let Some(d) = drag else {
            let grip = self.grip_at(pos);
            let changed = grip != self.hover_grip;
            self.hover_grip = grip;
            return changed;
        };
        if !d.live && (pos.0 - d.press.0).hypot(pos.1 - d.press.1) < 4. {
            return false;
        }
        d.live = true;
        let Some((at, _)) = point else { return true };
        let at = if d.all_day { (at / DAY as f64).floor() * DAY as f64 } else { at };
        let step = if d.all_day { DAY } else { snap };
        let round = |x: f64| ((x / step as f64).round() as i64) * step;
        let (a, b) = d.orig;
        d.now = match d.grip {
            Grip::Body => {
                let start = if d.all_day { a + round(at - d.anchor) } else { round(a as f64 + at - d.anchor) };
                (start, start + b - a)
            }
            Grip::Start => (round(at).min(b - step), b),
            Grip::End => (a, round(at).max(a + step)),
        };
        true
    }

    /// Which part of an editable event is under the pointer, in Adjust mode.
    fn grip_at(&self, pos: Pt) -> Option<Grip> {
        let i = self.hit(pos)?;
        let e = &self.data.events[i];
        let r = e.edit.as_ref().filter(|r| r.editable)?;
        if r.all_day || !self.timed_view() || self.spec().coord != Coord::Cartesian {
            return Some(Grip::Body);
        }
        let Some((_, Hit::Rect(b))) = self.hits.iter().rev().find(|(j, h)| *j == i && h.contains(pos)) else { return Some(Grip::Body) };
        let (lo, hi, p) = if self.time_across() { (b[0], b[2], pos.0) } else { (b[1], b[3], pos.1) };
        let g = ((hi - lo) / 4.).min(7.);
        let (a, z) = self.span_of(r);
        let piece = e.day as i64 * DAY + e.start_min.round() as i64;
        let piece_end = piece + e.dur_min.round() as i64;
        Some(if p - lo < g && piece == a {
            Grip::Start
        } else if hi - p < g && piece_end == z {
            Grip::End
        } else {
            Grip::Body
        })
    }

    /// A press in a mode. True if the mode took it.
    pub(super) fn mode_down(&mut self, pos: Pt, cx: &mut Context<Self>) -> bool {
        if self.creating() {
            if let Some(s) = self.slot_at(pos) {
                self.cursor = Some(s);
                self.place(cx);
            }
            return true;
        }
        if !self.adjusting() {
            return false;
        }
        let Some(i) = self.hit(pos) else {
            self.selected = None;
            return true;
        };
        self.selected = Some(i);
        let e = &self.data.events[i];
        let Some(r) = e.edit.clone().filter(|r| r.editable) else {
            let title = e.title.to_string();
            self.toast(format!("\u{201c}{title}\u{201d} is read-only"), None, cx);
            return true;
        };
        let Some((at, _)) = self.point_at(pos) else { return true };
        let span = self.span_of(&r);
        let anchor = if r.all_day { (at / DAY as f64).floor() * DAY as f64 } else { at };
        let grip = self.grip_at(pos).unwrap_or(Grip::Body);
        let drag = Drag { key: r.key.clone(), id: r.id.clone(), grip, all_day: r.all_day, press: pos, anchor, orig: span, now: span, live: false };
        self.mode = Some(Mode::Adjust { drag: Some(drag) });
        true
    }

    /// A release: a drag lands.
    pub(super) fn mode_up(&mut self, cx: &mut Context<Self>) {
        let Some(Mode::Adjust { drag }) = &mut self.mode else { return };
        let Some(d) = drag.take() else { return };
        if !d.live || d.now == d.orig {
            return;
        }
        if self.free_only && !d.all_day && !self.is_free(d.now, Some(&d.id)) {
            self.toast("That's busy: turn off \u{201c}Free time only\u{201d} to overlap".into(), None, cx);
            return;
        }
        self.reschedule(&d.key, &d.id, d.now, true, cx);
    }

    // --- Keyboard ----------------------------------------------------------------------------

    /// Keys while in a mode. True if the mode took the key.
    pub(super) fn mode_key(&mut self, key: &str, shift: bool, cx: &mut Context<Self>) -> bool {
        if self.mode.is_none() {
            return false;
        }
        let dir = match key {
            "left" | "h" => Some((-1, 0)),
            "right" | "l" => Some((1, 0)),
            "up" | "k" => Some((0, -1)),
            "down" | "j" => Some((0, 1)),
            _ => None,
        };
        if let Some((dx, dy)) = dir {
            if self.creating() {
                self.move_cursor(dx, dy);
            } else {
                self.nudge(dx, dy, shift, cx);
            }
            return true;
        }
        match key {
            "enter" | "space" if self.creating() => {
                if self.cursor.is_none() {
                    self.cursor = Some(self.cursor_home());
                }
                self.place(cx);
            }
            "enter" => self.mode = None,
            "[" => self.snap = SNAPS.iter().rev().copied().find(|&s| s < self.snap).unwrap_or(SNAPS[0]),
            "]" => self.snap = SNAPS.iter().copied().find(|&s| s > self.snap).unwrap_or(SNAPS[SNAPS.len() - 1]),
            "f" => self.free_only = !self.free_only,
            "escape" => match &mut self.mode {
                Some(Mode::Create { start }) if start.is_some() => *start = None,
                Some(Mode::Adjust { drag }) if drag.is_some() => *drag = None,
                _ => {
                    self.mode = None;
                    self.cursor = None;
                }
            },
            _ => return false,
        }
        true
    }

    /// Where the keyboard cursor starts: the next step from now, today.
    fn cursor_home(&self) -> Slot {
        let timed = self.timed_view();
        let today = self.data.today as i64 * DAY;
        if !timed {
            return Slot { abs: today, timed };
        }
        let now = Local::now();
        let min = (now.hour() * 60 + now.minute()) as i64;
        Slot { abs: today + (min / self.snap + 1) * self.snap, timed }
    }

    /// Moves the keyboard cursor: along time by a step, across days by one (a week vertically
    /// in day-only views).
    fn move_cursor(&mut self, dx: i32, dy: i32) {
        let mut c = self.cursor.unwrap_or_else(|| self.cursor_home());
        let (along, across) = self.axes(dx, dy);
        c.abs += if c.timed { along * self.snap + across * DAY } else { along * DAY + across * DAY * self.spec().facet.per_line as i64 };
        self.cursor = Some(c);
        self.keep_in_view(c.abs);
    }

    /// Splits an arrow into (steps along time, steps across days) for this view.
    fn axes(&self, dx: i32, dy: i32) -> (i64, i64) {
        let timed = self.timed_view();
        let (dx, dy) = (dx as i64, dy as i64);
        if timed {
            if self.time_across() { (dx, dy) } else { (dy, dx) }
        } else if self.spec().facet.dir == Dir::Rows {
            (dx, dy)
        } else {
            (dy, dx)
        }
    }

    fn keep_in_view(&mut self, abs: i64) {
        let arr = self.arr();
        let day = abs.div_euclid(DAY) as i32;
        let cell = arr.cell(day);
        if !arr.on_screen(&cell) {
            let first = arr.visible().find(|&d| arr.on_screen(&arr.cell(d))).unwrap_or(day);
            self.pan(if day < first { -1 } else { 1 });
        }
    }

    /// Moves (or with shift, resizes) the selected event from the keyboard.
    fn nudge(&mut self, dx: i32, dy: i32, resize: bool, cx: &mut Context<Self>) {
        let Some(i) = self.selected else {
            self.toast("Select an event to move it".into(), None, cx);
            return;
        };
        let Some(r) = self.data.events.get(i).and_then(|e| e.edit.clone()).filter(|r| r.editable) else { return };
        let (along, across) = self.axes(dx, dy);
        let timed = !r.all_day && self.timed_view();
        let delta = if r.all_day {
            (along + across) * DAY
        } else if timed {
            along * self.snap + across * DAY
        } else {
            along * DAY + across * DAY * self.spec().facet.per_line as i64
        };
        let (a, b) = self.span_of(&r);
        let span = if resize { (a, (b + delta).max(a + self.snap_step(!r.all_day))) } else { (a + delta, b + delta) };
        self.reschedule(&r.key, &r.id, span, true, cx);
        self.keep_in_view(span.0);
    }

    // --- Changes -----------------------------------------------------------------------------

    /// Creates the event once both ends are placed (or places the start).
    fn place(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.cursor else { return };
        let Some(Mode::Create { start }) = &mut self.mode else { return };
        match *start {
            None => {
                *start = Some(c);
                if self.free_only && c.timed && !self.is_free((c.abs, c.abs + 1), None) {
                    self.mode = Some(Mode::Create { start: None });
                    self.toast("That's busy: start in free time, or turn off \u{201c}Free time only\u{201d}".into(), None, cx);
                }
            }
            Some(s) => {
                let span = self.span_between(s, c);
                self.mode = None;
                self.cursor = None;
                let (start, end) = if s.timed {
                    (edit::time(self.time_of(span.0)), edit::time(self.time_of(span.1)))
                } else {
                    (edit::date(self.date_of(span.0)), edit::date(self.date_of(span.1)))
                };
                self.add_event(start, end, cx);
            }
        }
    }

    /// Moves an event to `span` (abs minutes; whole days for all-day events). With `undo`, says
    /// so with an undo, and holds the change back while the undo is offered.
    pub(super) fn reschedule(&mut self, key: &str, id: &str, span: (i64, i64), undo: bool, cx: &mut Context<Self>) {
        let Some(e) = self.data.events.iter().find(|e| e.edit.as_ref().is_some_and(|r| r.key == key && r.id == id)) else { return };
        let r = e.edit.clone().unwrap();
        let before = self.span_of(&r);
        if before == span {
            return;
        }
        let title = e.title.to_string();
        let now = edit::fields(&r, &e.title, e.location.as_ref().map(|l| l.as_ref()));
        let patch = if r.all_day {
            json!({ "start": edit::date(self.date_of(span.0)), "end": edit::date(self.date_of(span.1)) })
        } else {
            json!({ "start": edit::time(self.time_of(span.0)), "end": edit::time(self.time_of(span.1)) })
        };
        let mut op = edit::update(&r, &now, patch, Origin::App, State::Pending);
        op.notify = self.notify_choice(&r);
        self.queue_change_held(op, if undo { HOLD } else { 2 }, cx);
        if undo {
            // Undo goes back to where the event was before this run of moves.
            let from = match self.toast.as_ref().and_then(|t| t.undo.as_ref()) {
                Some(Undo::Reschedule { key: k, id: i, from }) if k == key && i == id => *from,
                _ => before,
            };
            let text = format!("Moved \u{201c}{title}\u{201d} to {}", self.describe(span, !r.all_day));
            self.toast(text, Some(Undo::Reschedule { key: key.into(), id: id.into(), from }), cx);
        }
    }

    // --- Chrome ------------------------------------------------------------------------------

    /// The pointer for the mode: a crosshair to place, hands and resize arrows to adjust.
    pub(super) fn mode_cursor(&self) -> Option<CursorStyle> {
        match self.mode.as_ref()? {
            Mode::Create { .. } => Some(CursorStyle::Crosshair),
            Mode::Adjust { drag } => {
                let grip = drag.as_ref().filter(|d| d.live).map(|d| d.grip).or(self.hover_grip)?;
                Some(match grip {
                    Grip::Body if drag.is_some() => CursorStyle::ClosedHand,
                    Grip::Body => CursorStyle::OpenHand,
                    _ if self.time_across() => CursorStyle::ResizeLeftRight,
                    _ => CursorStyle::ResizeUpDown,
                })
            }
        }
    }

    /// The bar under the top bar while you're in a mode: what to do next, the snap step,
    /// free time only, and Done.
    pub(super) fn mode_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let mode = self.mode.as_ref()?;
        let th = self.theme;
        let timed = self.timed_view();
        let (title, hint) = match mode {
            Mode::Create { start: None } => ("New event", if timed { "Click where it starts" } else { "Click its first day" }),
            Mode::Create { start: Some(_) } => ("New event", if timed { "Click where it ends · Esc to restart" } else { "Click its last day · Esc to restart" }),
            Mode::Adjust { drag: Some(d) } if d.live => ("Adjust", if d.grip == Grip::Body { "Moving" } else { "Resizing" }),
            Mode::Adjust { .. } => ("Adjust", if timed { "Drag an event to move it, or its ends to resize" } else { "Drag an event to another day" }),
        };
        let sep = || div().w(px(1.)).h(px(16.)).mx(px(6.)).bg(th.border);
        let snaps = SNAPS.iter().map(|&s| {
            let label = if s == 60 { "1h".to_string() } else { format!("{s}m") };
            self.chip(("snap", s as usize), label, s == self.snap).on_click(cx.listener(move |this, _, _, cx| {
                this.snap = s;
                cx.notify();
            }))
        });
        Some(
            div().absolute().top(px(chrome::TOPBAR_H + 6.)).left_0().right_0().flex().justify_center().child(
                self.bar()
                    .id("mode-bar")
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .pl(px(12.))
                            .pr(px(4.))
                            .text_size(px(12.5))
                            .child(div().size(px(7.)).rounded_full().bg(th.now))
                            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(th.ink).child(title))
                            // Narrow windows keep the controls and drop the hint.
                            .when(self.viewport.0 >= 900., |d| d.child(div().text_color(th.faint).child(hint))),
                    )
                    .when(timed, |d| d.child(sep()).children(snaps))
                    .child(sep())
                    .child(self.chip("free-only", "Free time only", self.free_only).on_click(cx.listener(|this, _, _, cx| {
                        this.free_only = !this.free_only;
                        cx.notify();
                    })))
                    .child(sep())
                    .child(self.chip("mode-done", "Done", false).text_color(with_alpha(th.ink, 0.8)).on_click(cx.listener(|this, _, _, cx| {
                        this.mode = None;
                        this.cursor = None;
                        cx.notify();
                    }))),
            ),
        )
    }
}
