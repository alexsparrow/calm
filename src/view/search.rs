//! Finding things, vim-style:
//!
//! * `/` searches. Typing jumps to the best match and dims everything else; a list of matches
//!   opens under the field (↑ ↓ to move through it, Tab to complete a facet). Enter puts you on
//!   the match, and then `n` / `N` step through the rest. Esc puts the view back.
//! * `:` jumps to a date: `:31`, `:12-31`, `:2027-03-04`, `:dec`, `:mon`, `:+2w`, `:today`.
//!
//! A search is plain words (in the title) and facets: `with:` (people), `in:` (calendar),
//! `at:` (location), `series:` (events in the same series as one titled so), `is:` (tentative,
//! confirmed, allday). Quote values with spaces: `with:"Priya Shah"`.

use std::time::Instant;

use chrono::{Datelike, Duration, Local, NaiveDate, Weekday};
use gpui::{Context, Entity, FontWeight, ScrollHandle, Subscription, Window, div, prelude::*, px};

use super::CalmView;
use super::input::{InputEvent, InputStyle, TextInput};
use crate::keys::Command;
use crate::model::Event;
use crate::spec::{Axis, Mark};
use crate::theme::with_alpha;

/// How faint non-matching events get while a search is on.
pub(super) const DIM: f32 = 0.16;
const FACETS: [(&str, &str); 5] = [("with:", "someone on it"), ("in:", "a calendar"), ("at:", "a place"), ("series:", "events in a series"), ("is:", "tentative · confirmed · allday")];

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Kind {
    Search,
    Jump,
}

/// The `/` or `:` field while it's open.
pub(super) struct Prompt {
    pub kind: Kind,
    input: Entity<TextInput>,
    list: ScrollHandle,
    /// The match the list was last scrolled to.
    scrolled: std::cell::Cell<Option<usize>>,
    _subs: Vec<Subscription>,
}

/// A search's results, kept after the field closes (for `n` / `N` and the dimming).
pub(super) struct Found {
    pub query: String,
    /// One event index per match (the first piece of each), in time order.
    pub matches: Vec<usize>,
    /// Per event index: whether it matches.
    pub hits: Vec<bool>,
    pub current: usize,
    /// Where the view was, to go back to on Esc: (start, zoom, hours).
    before: (f32, f32, [f32; 2]),
}

/// One condition of a search.
enum Term {
    Title(String),
    With(String),
    In(String),
    At(String),
    Series(String),
    Is(String),
}

/// Splits a query into tokens, keeping `key:"quoted values"` together.
fn tokens(q: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in q.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn terms(q: &str) -> Vec<Term> {
    tokens(q)
        .into_iter()
        .filter_map(|t| {
            let low = t.to_lowercase();
            let (key, value) = low.split_once(':').unwrap_or(("", &low));
            let v = value.to_string();
            Some(match key {
                _ if v.is_empty() => return None,
                "with" => Term::With(v),
                "in" => Term::In(v),
                "at" => Term::At(v),
                "series" => Term::Series(v),
                "is" => Term::Is(v),
                _ => Term::Title(low),
            })
        })
        .collect()
}

fn has(text: &str, needle: &str) -> bool {
    text.to_lowercase().contains(needle)
}

impl CalmView {
    /// The key an event's series goes by: its recurring ID, else its title.
    fn series_key<'a>(&self, e: &'a Event) -> &'a str {
        e.edit.as_ref().map(|r| r.uid.as_str()).unwrap_or(&e.title)
    }

    /// Which events match a query (per event index), or `None` for an empty one.
    fn hits_for(&self, q: &str) -> Option<Vec<bool>> {
        let terms = terms(q);
        if terms.is_empty() {
            return None;
        }
        let events = &self.data.events;
        // `series:` names a series by a title in it.
        let series: Vec<std::collections::HashSet<&str>> = terms
            .iter()
            .filter_map(|t| if let Term::Series(v) = t { Some(v) } else { None })
            .map(|v| events.iter().filter(|e| has(&e.title, v)).map(|e| self.series_key(e)).collect())
            .collect();
        Some(
            events
                .iter()
                .map(|e| {
                    let mut k = 0;
                    terms.iter().all(|t| match t {
                        Term::Title(v) => has(&e.title, v),
                        Term::With(v) => e.people.iter().any(|p| has(p, v)),
                        Term::In(v) => self.calendars.get(e.calendar).is_some_and(|c| has(c.display(), v) || has(&c.name, v)),
                        Term::At(v) => e.location.as_ref().is_some_and(|l| has(l, v)),
                        Term::Series(_) => {
                            k += 1;
                            series[k - 1].contains(self.series_key(e))
                        }
                        Term::Is(v) => match v.as_str() {
                            "tentative" => e.status == crate::model::Status::Tentative,
                            "confirmed" => e.status == crate::model::Status::Confirmed,
                            "allday" | "all-day" => e.all_day.is_some(),
                            _ => false,
                        },
                    })
                })
                .collect(),
        )
    }

    /// Matches for a query: the first piece of each matching event, in time order.
    fn matches_of(&self, hits: &[bool]) -> Vec<usize> {
        let mut seen = std::collections::HashSet::new();
        // Events are already in time order.
        (0..hits.len()).filter(|&i| hits[i] && seen.insert(self.data.events[i].group)).collect()
    }

    /// Whether event `i` is dimmed by the search.
    pub(super) fn search_dims(&self, i: usize) -> bool {
        self.found.as_ref().is_some_and(|f| !f.hits.get(i).copied().unwrap_or(true))
    }

    /// The group of the match you're on, to outline it.
    pub(super) fn current_match(&self) -> Option<u32> {
        let f = self.found.as_ref()?;
        f.matches.get(f.current).map(|&i| self.data.events[i].group)
    }

    // --- The field ---------------------------------------------------------------------------

    /// Opens `/` or `:` (on the next frame, which has the window to focus it).
    pub(super) fn open_prompt(&mut self, kind: Kind) {
        self.pending_prompt = Some(kind);
    }

    /// Creates the field asked for by `open_prompt` and focuses it; keeps its colours with the theme.
    pub(super) fn sync_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let th = self.theme;
        if let Some(p) = &self.prompt
            && p.input.read(cx).style.text != th.ink
        {
            p.input.update(cx, |t, cx| {
                (t.style.text, t.style.placeholder, t.style.cursor, t.style.selection) = (th.ink, th.faint, th.now, with_alpha(th.now, 0.25));
                cx.notify();
            });
        }
        let Some(kind) = self.pending_prompt.take() else { return };
        let style = InputStyle { text: th.ink, placeholder: th.faint, cursor: th.now, selection: with_alpha(th.now, 0.25), size: 15. };
        let (text, placeholder) = match kind {
            Kind::Search => (self.found.as_ref().map(|f| f.query.clone()).unwrap_or_default(), "Search titles, or with: in: at: series: is:"),
            Kind::Jump => (String::new(), "31 · 12-31 · 2027-03-04 · dec · mon · +2w · today"),
        };
        let seeded = self.prompt_seed.take();
        let input = cx.new(|cx| {
            let mut t = TextInput::new(seeded.clone().unwrap_or(text), placeholder, style, cx);
            if seeded.is_none() {
                t.select_everything(cx);
            }
            t
        });
        self.prompt_query_dirty = seeded.is_some();
        let subs = vec![
            cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| {
                match ev {
                    InputEvent::Submit => this.submit_prompt(cx),
                    InputEvent::Cancel => this.cancel_prompt(),
                    InputEvent::Blur => this.prompt = None,
                }
                window.focus(&this.focus);
                cx.notify();
            }),
            cx.observe(&input, |this, _, cx| {
                this.prompt_changed();
                cx.notify();
            }),
        ];
        input.read(cx).focus(window);
        if kind == Kind::Search && self.found.is_none() {
            let l = &self.lenses[self.preset];
            self.found = Some(Found { query: String::new(), matches: Vec::new(), hits: Vec::new(), current: 0, before: (l.start.target, l.zoom.target, [l.hours[0].target, l.hours[1].target]) });
        }
        self.prompt = Some(Prompt { kind, input, list: ScrollHandle::new(), scrolled: Default::default(), _subs: subs });
    }

    fn prompt_text(&self, cx: &gpui::App) -> Option<String> {
        self.prompt.as_ref().map(|p| p.input.read(cx).text().to_string())
    }

    /// Whether you're typing in the `/` or `:` field (shortcuts then step aside).
    pub(super) fn prompting(&self, window: &Window, cx: &gpui::App) -> bool {
        self.prompt.as_ref().is_some_and(|p| p.input.read(cx).is_focused(window))
    }

    /// The query changed: search again and jump to the first match from where you are.
    fn prompt_changed(&mut self) {
        self.prompt_query_dirty = true;
    }

    /// Re-runs the search after typing (from render, which has the app to read the field).
    pub(super) fn refresh_search(&mut self, cx: &gpui::App) {
        if !std::mem::take(&mut self.prompt_query_dirty) {
            return;
        }
        let Some(p) = &self.prompt else { return };
        if p.kind != Kind::Search {
            return;
        }
        let q = p.input.read(cx).text().to_string();
        let Some(f) = &self.found else { return };
        if f.query == q {
            return;
        }
        let before = f.before;
        let hits = self.hits_for(&q);
        let matches = hits.as_ref().map(|h| self.matches_of(h)).unwrap_or_default();
        // The first match at or after the moment you're looking at.
        let from = self.focus_day() as f32 * 1440. + if self.data.today == self.focus_day() { self.now_min } else { 0. };
        let current = matches
            .iter()
            .position(|&i| {
                let e = &self.data.events[i];
                e.day as f32 * 1440. + e.start_min >= from
            })
            .unwrap_or(matches.len().saturating_sub(1));
        let found = !matches.is_empty();
        self.found = Some(Found { query: q, matches, hits: hits.unwrap_or_default(), current, before });
        self.dirty = true;
        self.restat();

        if found {
            self.reveal_current();
        }
    }

    /// Runs the search again over new data (a sync, an edit), staying on the same match.
    pub(super) fn research(&mut self) {
        let Some(f) = self.found.take() else { return };
        let on = f.matches.get(f.current).map(|&i| self.series_key(&self.data.events[i]).to_string());
        let hits = self.hits_for(&f.query).unwrap_or_default();
        let matches = self.matches_of(&hits);
        let current = on.and_then(|k| matches.iter().position(|&i| self.series_key(&self.data.events[i]) == k)).unwrap_or(0).min(matches.len().saturating_sub(1));
        self.found = Some(Found { hits, matches, current, ..f });
        self.restat();
    }

    fn submit_prompt(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.prompt.take() else { return };
        let text = p.input.read(cx).text().to_string();
        match p.kind {
            Kind::Jump => match (self.parse_jump(&text), Command::parse(&text)) {
                (Some(date), _) => self.jump_to(date),
                (None, Ok(cmd)) => {
                    self.run(&cmd, cx);
                }
                (None, Err(e)) => self.toast(format!("Not a date or a command: {e}"), None, cx),
            },
            Kind::Search => {
                self.prompt_query_dirty = true;
                self.prompt = Some(p);
                self.refresh_search(cx);
                self.prompt = None;
                match &self.found {
                    Some(f) if f.matches.is_empty() => {
                        if !f.query.is_empty() {
                            self.toast(format!("Nothing matches \u{201c}{}\u{201d}", f.query), None, cx);
                        }
                        self.clear_search(true);
                    }
                    Some(f) => self.selected = f.matches.get(f.current).copied(),
                    None => {}
                }
            }
        }
    }

    fn cancel_prompt(&mut self) {
        if let Some(p) = self.prompt.take()
            && p.kind == Kind::Search
        {
            self.clear_search(true);
        }
    }

    /// Ends the search; `restore` puts the view back where it was before it.
    pub(super) fn clear_search(&mut self, restore: bool) {
        if let Some(f) = self.found.take() {
            if restore {
                let l = &mut self.lenses[self.preset];
                (l.start.target, l.zoom.target) = (f.before.0, f.before.1);
                (l.hours[0].target, l.hours[1].target) = (f.before.2[0], f.before.2[1]);
            }
            self.dirty = true;
            self.restat();
        }
    }

    /// A command in the search field (see `keys "search"` in `assets/keys.kdl`): through
    /// the matches, or completing a facet. True if taken.
    pub(super) fn prompt_command(&mut self, cmd: &Command, cx: &mut Context<Self>) -> bool {
        match cmd {
            Command::Match(d) => self.step_match(*d),
            Command::Complete => {
                let Some(text) = self.prompt_text(cx) else { return true };
                if let Some((_, completed)) = self.suggestions(&text).into_iter().next() {
                    let input = self.prompt.as_ref().unwrap().input.clone();
                    input.update(cx, |t, cx| t.set_text(completed, cx));
                }
            }
            _ => return false,
        }
        true
    }

    /// `n` / `N` after a search: the next or previous match, selected. True if there's a search.
    pub(super) fn next_match(&mut self, dir: i32) -> bool {
        if self.found.as_ref().is_none_or(|f| f.matches.is_empty()) {
            return false;
        }
        self.step_match(dir);
        self.selected = self.found.as_ref().and_then(|f| f.matches.get(f.current).copied());
        true
    }

    fn step_match(&mut self, dir: i32) {
        let Some(f) = &mut self.found else { return };
        let n = f.matches.len() as i32;
        if n == 0 {
            return;
        }
        f.current = (f.current as i32 + dir).rem_euclid(n) as usize;
        self.reveal_current();
    }

    fn reveal_current(&mut self) {
        if let Some(i) = self.found.as_ref().and_then(|f| f.matches.get(f.current).copied()) {
            self.reveal(i);
        }
    }

    /// Brings event `i` into view: its day, and its time in the hour window.
    pub(super) fn reveal(&mut self, i: usize) {
        let e = &self.data.events[i];
        let (day, start, end, all_day) = (e.day, e.start_min, e.end_min(), e.all_day.is_some());
        let l = &self.lenses[self.preset];
        let (s, n) = (l.start.target, l.zoom.target.exp2());
        if (day as f32) < s.floor() || day as f32 >= s + n - 0.01 {
            self.zoom_to_stop(self.nearest_stop(), Some(day as f32 + 0.5));
        }
        let spec = self.spec();
        let timed = (spec.x == Axis::TimeOfDay || spec.y == Axis::TimeOfDay) && matches!(self.lens().mark, Mark::Block | Mark::Pill);
        if timed && !all_day {
            let l = &mut self.lenses[self.preset];
            let (h0, h1) = (l.hours[0].target, l.hours[1].target);
            let span = h1 - h0;
            let (u0, u1) = (start / 1440., end / 1440.);
            if u0 < h0 || u1 > h1 {
                let lo = (u0 - span * 0.15).clamp(0., 1. - span);
                l.hours[0].target = lo;
                l.hours[1].target = lo + span;
            }
        }
    }

    // --- Suggestions -------------------------------------------------------------------------

    /// What Tab would complete the last word to: (label, the whole new query).
    fn suggestions(&self, text: &str) -> Vec<(String, String)> {
        // The word being typed, and everything before it.
        let cut = text.rfind(' ').map_or(0, |k| k + 1);
        let (head, last) = (text[..cut].to_string(), text[cut..].to_string());
        let with = |value: &str, key: &str| {
            let v = if value.contains(' ') { format!("\"{value}\"") } else { value.to_string() };
            format!("{head}{key}{v} ")
        };
        let low = last.to_lowercase();
        if let Some((key, partial)) = low.split_once(':') {
            let key = format!("{key}:");
            let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
            let mut add = |v: &str| {
                if has(v, partial) {
                    *counts.entry(v.to_string()).or_default() += 1;
                }
            };
            match key.as_str() {
                "with:" => self.data.events.iter().flat_map(|e| e.people.iter()).for_each(|p| add(p)),
                "in:" => self.calendars.iter().for_each(|c| add(c.display())),
                "at:" => self.data.events.iter().filter_map(|e| e.location.as_ref()).for_each(|l| add(l)),
                "series:" => self.data.events.iter().filter(|e| e.edit.as_ref().is_none_or(|r| r.uid != r.id)).for_each(|e| add(&e.title)),
                "is:" => ["tentative", "confirmed", "allday"].iter().for_each(|v| add(v)),
                _ => {}
            }
            let mut values: Vec<(String, usize)> = counts.into_iter().collect();
            values.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            return values.into_iter().take(6).map(|(v, n)| (format!("{key}{v}  ·  {n}"), with(&v, &key))).collect();
        }
        if last.is_empty() {
            return Vec::new();
        }
        FACETS.iter().filter(|(k, _)| k.starts_with(&low)).map(|(k, what)| (format!("{k} {what}"), format!("{head}{k}"))).collect()
    }

    // --- Dates -------------------------------------------------------------------------------

    /// The day a jump is relative to: today when it's in view, else the middle of the view. So
    /// `:31` in a month view means that month, and in a year view this month (if this year is
    /// in view) or the middle month.
    fn focus_day(&self) -> i32 {
        let l = self.lens();
        let (s, n) = (l.start.target, l.zoom.target.exp2());
        let today = self.data.today;
        if (today as f32) >= s.floor() && (today as f32) < s + n { today } else { (s + n * 0.5) as i32 }
    }

    /// Reads a date: `31`, `12-31`, `2027-03-04`, `dec`, `dec 31`, `31 dec`, `mon`, `+2w`,
    /// `-3d`, `+1m`, `today`.
    pub(super) fn parse_jump(&self, text: &str) -> Option<NaiveDate> {
        parse_date(text, self.data.date(self.focus_day()), Local::now().date_naive())
    }
    /// Centres the view on a date at the current zoom, and marks the day for a moment.
    pub(super) fn jump_to(&mut self, date: NaiveDate) {
        let day = (date - self.data.origin).num_days() as i32;
        if !(0..crate::model::DAYS).contains(&day) {
            return;
        }
        self.zoom_to_stop(self.nearest_stop(), Some(day as f32 + 0.5));
        self.jump_mark = Some((day, Instant::now()));
    }

    // --- Drawing -----------------------------------------------------------------------------

    /// The field and, for a search, its suggestions and matches, under the top bar.
    pub(super) fn prompt_panel(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let p = self.prompt.as_ref()?;
        let th = self.theme;
        let text = p.input.read(cx).text().to_string();
        let (glyph, info) = match p.kind {
            Kind::Search => {
                let info = match &self.found {
                    Some(f) if !f.matches.is_empty() => format!("{} of {}", f.current + 1, f.matches.len()),
                    Some(f) if !f.query.trim().is_empty() => "no matches".into(),
                    _ => String::new(),
                };
                ("/", info)
            }
            Kind::Jump => (
                ":",
                match (self.parse_jump(&text), Command::parse(&text)) {
                    (Some(d), _) => format!("\u{2192} {}", d.format("%a %-d %b %Y")),
                    _ if text.trim().is_empty() => "\u{2192} today".into(),
                    (None, Ok(cmd)) => format!("\u{21b5} {}", cmd.describe().text),
                    (None, Err(e)) => e,
                },
            ),
        };
        let width = (self.viewport.0 - 32.).min(560.);
        let mut panel = div()
            .w(px(width))
            .flex()
            .flex_col()
            .rounded(px(14.))
            .bg(th.surface)
            .border_1()
            .border_color(th.border)
            .shadow_lg()
            .overflow_hidden()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .h(px(46.))
                    .px(px(14.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).text_color(th.now).child(glyph))
                    .child(div().flex_1().min_w_0().child(p.input.clone()))
                    .child(div().flex_none().text_size(px(12.5)).text_color(th.faint).child(info)),
            );
        if p.kind == Kind::Search {
            let row = |id: (&'static str, usize)| div().id(id).h(px(32.)).px(px(14.)).flex().items_center().gap(px(10.)).text_size(px(13.)).cursor_pointer();
            let suggestions = if text.trim().is_empty() {
                FACETS.iter().map(|(k, what)| (format!("{k} {what}"), format!("{text}{k}"))).collect()
            } else {
                self.suggestions(&text)
            };
            if !suggestions.is_empty() {
                panel = panel.child(div().h(px(1.)).bg(th.border)).children(suggestions.into_iter().enumerate().map(|(k, (label, completed))| {
                    let input = p.input.clone();
                    row(("suggest", k))
                        .text_color(th.muted)
                        .hover(move |s| s.bg(with_alpha(th.ink, 0.05)))
                        .child(div().flex_1().child(label))
                        .when(k == 0, |d| d.child(div().text_size(px(11.)).text_color(th.faint).child("Tab")))
                        .on_click(cx.listener(move |_, _, window, cx| {
                            input.update(cx, |t, cx| t.set_text(completed.clone(), cx));
                            input.read(cx).focus(window);
                        }))
                }));
            }
            if let Some(f) = self.found.as_ref().filter(|f| !f.matches.is_empty()) {
                let timeline = self.timeline(f, width, cx);
                let rows = f.matches.iter().enumerate().map(|(k, &i)| {
                    let e = &self.data.events[i];
                    let date = self.data.date(e.day).format("%a %-d %b").to_string();
                    let when = if e.all_day.is_some() { date } else { format!("{date} · {}", super::scene::fmt_time(e.start_min)) };
                    let on = k == f.current;
                    row(("match", k))
                        .when(on, |d| d.bg(with_alpha(th.now, 0.12)))
                        .hover(move |s| s.bg(with_alpha(th.ink, 0.05)))
                        .child(div().size(px(8.)).rounded_full().flex_none().bg(th.cals[e.calendar.min(th.cals.len() - 1)].dot))
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(th.ink).child(e.title.clone()))
                        .child(div().flex_none().text_size(px(12.)).text_color(th.faint).child(when))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(f) = &mut this.found {
                                f.current = k;
                            }
                            this.reveal(i);
                            this.prompt = None;
                            this.selected = Some(i);
                            window.focus(&this.focus);
                            cx.notify();
                        }))
                });
                let height = (f.matches.len() as f32 * 32.).min(7. * 32.);
                // Keep the current match in the list, a few rows down.
                if p.scrolled.get() != Some(f.current) {
                    p.scrolled.set(Some(f.current));
                    let top = (f.current as f32 - 3.).max(0.) * 32.;
                    p.list.set_offset(gpui::point(px(0.), px(-top.min((f.matches.len() as f32 * 32. - height).max(0.)))));
                }
                panel = panel
                    .child(div().h(px(1.)).bg(th.border))
                    .child(timeline)
                    .child(div().h(px(1.)).bg(th.border))
                    .child(div().id("matches").h(px(height)).overflow_y_scroll().track_scroll(&p.list).children(rows));
            }
        }
        Some(div().absolute().top(px(super::chrome::TOPBAR_H + 8.)).left_0().right_0().flex().justify_center().child(panel))
    }
}

/// A date from `text`: numbers and month names relative to `anchor`, weekdays to `today`.
fn parse_date(text: &str, anchor: NaiveDate, today: NaiveDate) -> Option<NaiveDate> {
    let t = text.trim().trim_start_matches(':').trim().to_lowercase();
    let clamp = |y: i32, m: u32, d: u32| -> Option<NaiveDate> {
        let last = (28..=31).rev().find(|&d| NaiveDate::from_ymd_opt(y, m, d).is_some())?;
        NaiveDate::from_ymd_opt(y, m, d.clamp(1, last))
    };
    if t.is_empty() || t == "today" || t == "t" {
        return Some(today);
    }
    // Relative: +2w, -3d, +1m, +1y.
    if let Some(rest) = t.strip_prefix('+').map(|r| (1, r)).or_else(|| t.strip_prefix('-').map(|r| (-1, r))) {
        let (sign, rest) = rest;
        let (num, unit) = rest.split_at(rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len()));
        let n: i64 = if num.is_empty() { 1 } else { num.parse().ok()? };
        let n = n * sign;
        return match unit {
            "" | "d" => Some(anchor + Duration::days(n)),
            "w" => Some(anchor + Duration::weeks(n)),
            "m" => {
                let months = anchor.year() as i64 * 12 + anchor.month0() as i64 + n;
                clamp(months.div_euclid(12) as i32, months.rem_euclid(12) as u32 + 1, anchor.day())
            }
            "y" => clamp(anchor.year() + n as i32, anchor.month(), anchor.day()),
            _ => None,
        };
    }
    // Numbers: d, m-d, y-m-d (also with / or .).
    let nums: Vec<&str> = t.split(['-', '/', '.']).collect();
    if nums.iter().all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) {
        let v: Vec<u32> = nums.iter().map(|n| n.parse().ok()).collect::<Option<_>>()?;
        return match v[..] {
            [y] if nums[0].len() == 4 => clamp(y as i32, 1, 1),
            [d] => clamp(anchor.year(), anchor.month(), d),
            [m, d] if (1..=12).contains(&m) => clamp(anchor.year(), m, d),
            [y, m, d] if (1..=12).contains(&m) => clamp(y as i32, m, d),
            _ => None,
        };
    }
    // Words: a weekday (the coming one), or a month with an optional day.
    let words: Vec<&str> = t.split_whitespace().collect();
    if let [w] = words[..]
        && let Some(wd) = weekday(w)
    {
        let ahead = (wd.num_days_from_monday() as i64 - today.weekday().num_days_from_monday() as i64 - 1).rem_euclid(7) + 1;
        return Some(today + Duration::days(ahead));
    }
    let month = words.iter().find_map(|w| month(w))?;
    let day = words.iter().find_map(|w| w.parse::<u32>().ok()).unwrap_or(1);
    clamp(anchor.year(), month, day)
}

impl CalmView {
    /// Every match as a dot on a thin line through the whole calendar, with today and what's in
    /// view marked. Click to go to the nearest match.
    fn timeline(&self, f: &Found, width: f32, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let th = self.theme;
        const PAD: f32 = 14.;
        let inner = width - 2. * PAD;
        let days = crate::model::DAYS as f32;
        let x = |day: f32| PAD + (day / days).clamp(0., 1.) * inner;
        let l = self.lens();
        let (s, n) = (l.start.x, l.zoom.x.exp2());
        let mut strip = div().relative().h(px(30.)).w_full();
        // What's in view, faintly.
        strip = strip.child(div().absolute().top(px(6.)).h(px(18.)).left(px(x(s))).w(px((x(s + n) - x(s)).max(2.))).rounded(px(3.)).bg(with_alpha(th.ink, 0.07)));
        // The line, and a tick at each month (taller at each year).
        strip = strip.child(div().absolute().top(px(15.)).h(px(1.)).left(px(PAD)).w(px(inner)).bg(th.border));
        let (origin, mut d) = (self.data.origin, self.data.origin.with_day(1).unwrap_or(self.data.origin));
        while ((d - origin).num_days() as f32) < days {
            let day = (d - origin).num_days() as f32;
            if day >= 0. {
                let year = d.month() == 1;
                strip = strip.child(div().absolute().top(px(if year { 9. } else { 12. })).h(px(if year { 13. } else { 7. })).left(px(x(day))).w(px(1.)).bg(if year { th.muted } else { th.border }));
                if year {
                    strip = strip.child(div().absolute().top(px(17.)).left(px(x(day) + 3.)).text_size(px(9.5)).text_color(th.faint).child(d.year().to_string()));
                }
            }
            d = d.checked_add_months(chrono::Months::new(1)).unwrap_or(d + Duration::days(31));
        }
        // Today.
        let today = x(self.data.today as f32 + 0.5);
        strip = strip.child(div().absolute().top(px(8.)).h(px(15.)).left(px(today - 0.75)).w(px(1.5)).bg(th.now));
        // The matches; the current one larger.
        for (k, &i) in f.matches.iter().enumerate() {
            let e = &self.data.events[i];
            let on = k == f.current;
            let r = if on { 4.5 } else { 2. };
            let cx_ = x(e.day as f32 + e.start_min / 1440.);
            let color = th.cals[e.calendar.min(th.cals.len() - 1)].dot;
            strip = strip.child(
                div()
                    .absolute()
                    .top(px(15.5 - r))
                    .left(px(cx_ - r))
                    .size(px(2. * r))
                    .rounded_full()
                    .bg(if on { th.ink } else { color })
                    .when(on, |d| d.border_2().border_color(color)),
            );
        }
        let left = (self.viewport.0 - width) * 0.5;
        div().id("timeline").w_full().cursor_pointer().child(strip).on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(move |this, ev: &gpui::MouseDownEvent, window, cx| {
                cx.stop_propagation();
                let at = (f32::from(ev.position.x) - left - PAD) / inner * days;
                let Some(f) = &mut this.found else { return };
                let Some(k) = (0..f.matches.len()).min_by(|&a, &b| {
                    let t = |k: usize| {
                        let e = &this.data.events[f.matches[k]];
                        (e.day as f32 + e.start_min / 1440. - at).abs()
                    };
                    t(a).total_cmp(&t(b))
                }) else {
                    return;
                };
                f.current = k;
                this.reveal_current();
                if let Some(p) = &this.prompt {
                    p.input.read(cx).focus(window);
                }
                cx.notify();
            }),
        )
    }
}

fn weekday(w: &str) -> Option<Weekday> {
    let days = [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri, Weekday::Sat, Weekday::Sun];
    let names = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];
    (w.len() >= 2).then(|| names.iter().position(|n| n.starts_with(w)).map(|k| days[k])).flatten()
}

fn month(w: &str) -> Option<u32> {
    let names = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];
    (w.len() >= 3).then(|| names.iter().position(|n| n.starts_with(w)).map(|k| k as u32 + 1)).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn dates() {
        // Viewing November 2026; today is Monday 23 November.
        let (anchor, today) = (d(2026, 11, 10), d(2026, 11, 23));
        let p = |t: &str| parse_date(t, anchor, today);
        assert_eq!(p("31"), Some(d(2026, 11, 30)), "clamped to the month");
        assert_eq!(p(":12-31"), Some(d(2026, 12, 31)));
        assert_eq!(p("2027-03-04"), Some(d(2027, 3, 4)));
        assert_eq!(p("2027"), Some(d(2027, 1, 1)));
        assert_eq!(p("dec"), Some(d(2026, 12, 1)));
        assert_eq!(p("31 dec"), Some(d(2026, 12, 31)));
        assert_eq!(p("feb 30"), Some(d(2026, 2, 28)));
        assert_eq!(p("mon"), Some(d(2026, 11, 30)), "the coming Monday, not today");
        assert_eq!(p("fri"), Some(d(2026, 11, 27)));
        assert_eq!(p("+2w"), Some(d(2026, 11, 24)));
        assert_eq!(p("-3d"), Some(d(2026, 11, 7)));
        assert_eq!(p("+3m"), Some(d(2027, 2, 10)));
        assert_eq!(p(""), Some(today));
        assert_eq!(p("13-1"), None);
        assert_eq!(p("banana"), None);
    }

    #[test]
    fn queries() {
        assert_eq!(tokens(r#"plan with:"Priya Shah" in:work"#), vec!["plan", "with:Priya Shah", "in:work"]);
        let t = terms("Plan WITH:sam is:");
        assert!(matches!(&t[..], [Term::Title(a), Term::With(b)] if a == "plan" && b == "sam"));
    }
}
