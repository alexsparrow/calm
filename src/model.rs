//! Normalized event model and a deterministic synthetic dataset anchored on
//! the real current date. Day indices count from `Dataset::origin`, a Monday.

use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone, Timelike};
use gpui::SharedString;

/// From the Monday on or before 1 January, through the first quarter of next year.
pub const WEEKS: i32 = 65;
pub const DAYS: i32 = WEEKS * 7;
/// Calendars are configured (see `Calendar`); per-calendar arrays are sized for at most this many.
pub const MAX_CALENDARS: usize = 12;

/// A calendar: how its events look, and how draining they are by default.
#[derive(Clone, Debug, PartialEq)]
pub struct Calendar {
    pub name: String,
    /// Hue in degrees and saturation; the theme derives fills, edges and text from these.
    pub hue: f32,
    pub sat: f32,
    /// Default demand (0..1) of its events, for Energy and strain.
    pub demand: f32,
}

/// Named colours, muted to sit with the theme. Calendars without a colour take these in order.
pub const PALETTE: [(&str, f32, f32); 12] = [
    ("sage", 150., 0.22),
    ("clay", 18., 0.48),
    ("slate", 215., 0.30),
    ("ochre", 40., 0.55),
    ("plum", 320., 0.24),
    ("teal", 180., 0.30),
    ("rose", 350., 0.36),
    ("moss", 85., 0.28),
    ("sky", 200., 0.42),
    ("lavender", 262., 0.26),
    ("sand", 32., 0.30),
    ("stone", 30., 0.07),
];

/// The demo data's calendars (and their old defaults, which configs naming them inherit).
const DEMO: [(&str, usize, f32); 4] = [("Work", 0, 0.55), ("Personal", 1, 0.35), ("Family", 2, 0.25), ("Focus", 3, 0.85)];

pub fn demo_calendars() -> Vec<Calendar> {
    DEMO.iter().map(|&(name, p, demand)| Calendar { name: name.into(), hue: PALETTE[p].1, sat: PALETTE[p].2, demand }).collect()
}

/// A palette name or `#rrggbb` as (hue, saturation). Hex colours keep their hue; their
/// saturation is tempered so they sit with the palette in both themes.
pub fn parse_color(s: &str) -> Option<(f32, f32)> {
    if let Some(&(_, h, sat)) = PALETTE.iter().find(|p| p.0.eq_ignore_ascii_case(s)) {
        return Some((h, sat));
    }
    let hex = s.strip_prefix('#').filter(|h| h.len() == 6)?;
    let v = u32::from_str_radix(hex, 16).ok()?;
    let rgb = gpui::Rgba { r: ((v >> 16) & 255) as f32 / 255., g: ((v >> 8) & 255) as f32 / 255., b: (v & 255) as f32 / 255., a: 1. };
    let hsl: gpui::Hsla = rgb.into();
    Some((hsl.h * 360., hsl.s.clamp(0.06, 0.55)))
}

/// The calendar list: as declared, else one per distinct name the sources ask for
/// (`calendar=`, or the source's own name). Unset colours take the palette in order, and
/// names the demo used keep its colours and demand.
pub fn resolve_calendars(declared: Option<&[crate::viewconf::CalendarDecl]>, wanted: &[String]) -> Vec<Calendar> {
    let decls: Vec<crate::viewconf::CalendarDecl> = match declared {
        Some(d) => d.to_vec(),
        None => {
            let mut names: Vec<String> = Vec::new();
            for n in wanted {
                if !names.iter().any(|x| x.eq_ignore_ascii_case(n)) {
                    names.push(n.clone());
                }
            }
            names.into_iter().map(|n| (n, None, None)).collect()
        }
    };
    let mut out: Vec<Calendar> = Vec::new();
    for (name, color, demand) in decls.into_iter().take(MAX_CALENDARS) {
        let demo = DEMO.iter().find(|d| d.0.eq_ignore_ascii_case(&name));
        let (hue, sat) = match color.as_deref().and_then(parse_color) {
            Some(c) => c,
            None => {
                let p = match demo {
                    Some(d) => d.1,
                    // The first palette colour no calendar is using yet (cycling if all are).
                    None => (0..PALETTE.len()).find(|&i| !out.iter().any(|c| c.hue == PALETTE[i].1 && c.sat == PALETTE[i].2)).unwrap_or(out.len() % PALETTE.len()),
                };
                (PALETTE[p].1, PALETTE[p].2)
            }
        };
        let demand = demand.or(demo.map(|d| d.2)).unwrap_or(0.4);
        out.push(Calendar { name, hue, sat, demand });
    }
    if out.is_empty() { demo_calendars() } else { out }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Confirmed,
    Tentative,
}

#[derive(Clone)]
pub struct Event {
    pub title: SharedString,
    pub calendar: usize,
    pub day: i32,
    pub start_min: f32,
    pub dur_min: f32,
    pub energy: f32,
    pub status: Status,
    pub location: Option<SharedString>,
    pub notes: Option<SharedString>,
    /// Shared by the per-day pieces of one occurrence (an event crossing midnight).
    pub group: u32,
    /// Demand above your energy at its midpoint (see `Dataset::set_energy`).
    pub strain: f32,
}

impl Event {
    pub fn end_min(&self) -> f32 {
        (self.start_min + self.dur_min).min(1440.0)
    }

    pub fn overlaps(&self, other: &Event) -> bool {
        self.day == other.day && self.start_min < other.end_min() && other.start_min < self.end_min()
    }
}

pub struct Dataset {
    pub origin: NaiveDate,
    pub today: i32,
    pub events: Vec<Event>,
    /// `events[day_start[d]..day_start[d + 1]]` are the events on day `d`.
    pub day_start: Vec<usize>,
    /// All-day events per day: kept out of the time grid until there is a horizon track.
    pub all_day: Vec<u16>,
    /// Each day's energy curve, `energy::SAMPLES` per day, back to back.
    energy: Vec<f32>,
}

impl Dataset {
    pub fn date(&self, day: i32) -> NaiveDate {
        self.origin + Duration::days(day as i64)
    }
}

pub fn now_minutes() -> f32 {
    let t = Local::now().time();
    t.hour() as f32 * 60. + t.minute() as f32 + t.second() as f32 / 60.
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn chance(&mut self, p: f32) -> bool {
        self.f() < p
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[(self.next() % xs.len() as u64) as usize]
    }
}

const MEETINGS: &[(&str, Option<&str>)] = &[
    ("Design review", Some("Studio")),
    ("Planning", Some("Room 4")),
    ("Customer call", None),
    ("Architecture sync", Some("Room 2")),
    ("Hiring loop", Some("Room 1")),
    ("Budget review", None),
    ("Demo prep", None),
    ("Roadmap workshop", Some("Studio")),
    ("Partner check-in", None),
    ("Interview: frontend", Some("Room 1")),
];
const ONE_ON_ONES: &[&str] = &["1:1 with Sam", "1:1 with Priya", "1:1 with Jonas"];
const PERSONAL: &[(&str, Option<&str>)] = &[
    ("Dentist", Some("Harbour Dental")),
    ("Reading", None),
    ("Call Mum", None),
    ("Groceries", Some("Market Hall")),
    ("Haircut", None),
    ("Pottery class", Some("Clay Works")),
];
const FAMILY: &[(&str, Option<&str>)] = &[
    ("Swim lessons", Some("Leisure centre")),
    ("Movie night", None),
    ("Park with the kids", Some("Victoria Park")),
    ("Parents' evening", Some("School")),
];
const FOCUS: &[&str] = &["Deep work", "Writing", "Prototype", "Research", "Strategy doc"];

/// `multiplier` > 1 densifies the dataset for stress testing.
pub fn generate(multiplier: usize) -> Dataset {
    let (origin, today) = range();

    let mut rng = Rng(0x9E3779B97F4A7C15);
    let mut events = Vec::new();
    let mut push = |title: &str, calendar, day, start_h: f32, dur_min: f32, energy: f32, status, location: Option<&str>| {
        events.push(Event {
            title: SharedString::from(title.to_string()),
            calendar,
            day,
            start_min: (start_h * 4.).round() * 15.,
            dur_min,
            energy,
            status,
            location: location.map(|l| SharedString::from(l.to_string())),
            notes: None,
            group: events.len() as u32,
            strain: 0.,
        });
    };

    for rep in 0..multiplier {
        let jitter = |rng: &mut Rng| if rep == 0 { 0. } else { (rng.f() - 0.5) * 3. };
        for day in 0..DAYS {
            let dow = day % 7;
            let weekday = dow < 5;
            let ok = Status::Confirmed;
            if weekday {
                push("Standup", 0, day, 9.5 + jitter(&mut rng), 15., 0.3, ok, None);
                if dow == 1 || dow == 3 {
                    push("Gym", 1, day, 7., 60., 0.5, ok, Some("Gym"));
                }
                if rng.chance(0.85) {
                    let len = *rng.pick(&[90., 120., 150.]);
                    push(rng.pick(FOCUS), 3, day, 10. + jitter(&mut rng) * 0.3, len, 0.85 + 0.15 * rng.f(), ok, None);
                }
                push("Lunch", 1, day, 12.5, 45., 0.1, ok, None);
                if dow == 2 {
                    push(rng.pick(ONE_ON_ONES), 0, day, 14., 30., 0.45, ok, None);
                }
                if dow == 4 {
                    push("Retro", 0, day, 15.5, 60., 0.5, ok, Some("Room 2"));
                }
                for _ in 0..(1 + rng.next() % 3) {
                    let (title, loc) = *rng.pick(MEETINGS);
                    let status = if rng.chance(0.18) { Status::Tentative } else { ok };
                    let start = 13. + rng.f() * 4.5 + jitter(&mut rng);
                    push(title, 0, day, start, *rng.pick(&[30., 45., 60., 60., 90.]), 0.35 + 0.4 * rng.f(), status, loc);
                }
                if rng.chance(0.25) {
                    push("Inbox zero", 0, day, 17.5, 30., 0.2, ok, None);
                }
                push("School pickup", 2, day, 15.25, 30., 0.2, ok, Some("School"));
                push("Dinner", 2, day, 18.5, 60., 0.1, ok, None);
            } else {
                if rng.chance(0.8) {
                    push("Long run", 1, day, 8. + rng.f(), *rng.pick(&[60., 90.]), 0.6, ok, Some("River path"));
                }
                let (title, loc) = *rng.pick(FAMILY);
                push(title, 2, day, 10.5 + rng.f() * 4., *rng.pick(&[90., 120., 180.]), 0.3, ok, loc);
                push("Family dinner", 2, day, 18.5, 90., 0.1, ok, None);
            }
            if rng.chance(0.3) {
                let (title, loc) = *rng.pick(PERSONAL);
                let status = if rng.chance(0.3) { Status::Tentative } else { ok };
                push(title, 1, day, 17. + rng.f() * 3.5, *rng.pick(&[30., 60., 90.]), 0.25, status, loc);
            }
            if weekday && rng.chance(0.2) {
                push("Writing", 3, day, 20.5, 60., 0.75, ok, None);
            }
        }
    }

    for e in &mut events {
        e.notes = match e.calendar {
            0 if e.title.starts_with("1:1") => Some("Career goals, blockers, feedback both ways.".into()),
            0 if e.status == Status::Tentative => Some("Awaiting confirmation from the organiser.".into()),
            3 => Some("Notifications off. Protected time.".into()),
            _ => None,
        };
    }

    Dataset::new(origin, today, events, vec![0; DAYS as usize])
}

/// How far an event's demand exceeds your energy at its midpoint (precomputed per day).
pub fn strain(e: &Event) -> f32 {
    e.strain
}

/// Demand above typical energy by this much is worth pointing out.
pub const STRAIN: f32 = 0.15;

/// Vertical position for a 0..1 energy/demand level in the Energy view.
pub fn energy_v(level: f32) -> f32 {
    0.06 + (1. - level) * 0.88
}

impl Dataset {
    pub fn on_day(&self, day: i32) -> std::ops::Range<usize> {
        if !(0..DAYS).contains(&day) {
            return 0..0;
        }
        self.day_start[day as usize]..self.day_start[day as usize + 1]
    }
}

/// The data range: from the Monday on or before 1 January of this year. Returns (origin, today).
fn range() -> (NaiveDate, i32) {
    let today_date = Local::now().date_naive();
    let jan1 = NaiveDate::from_ymd_opt(today_date.year(), 1, 1).unwrap();
    let origin = jan1 - Duration::days(jan1.weekday().num_days_from_monday() as i64);
    (origin, (today_date - origin).num_days() as i32)
}

impl Dataset {
    fn new(origin: NaiveDate, today: i32, mut events: Vec<Event>, all_day: Vec<u16>) -> Self {
        // Day order lets views touch only the days they can see.
        events.sort_by(|a, b| a.day.cmp(&b.day).then(a.start_min.total_cmp(&b.start_min)));
        let mut day_start = vec![0; DAYS as usize + 1];
        for e in &events {
            day_start[e.day as usize + 1] += 1;
        }
        for d in 0..DAYS as usize {
            day_start[d + 1] += day_start[d];
        }
        let mut data = Dataset { origin, today, events, day_start, all_day, energy: Vec::new() };
        data.set_energy(&crate::energy::EnergyModel::default());
        data
    }

    /// Recomputes each day's energy curve, and each event's strain against it.
    pub fn set_energy(&mut self, model: &crate::energy::EnergyModel) {
        use crate::energy::SAMPLES;
        self.energy = Vec::with_capacity(DAYS as usize * SAMPLES);
        for d in 0..DAYS {
            let r = self.on_day(d);
            let curve = model.day(self.events[r].iter());
            self.energy.extend(curve);
        }
        for i in 0..self.events.len() {
            let e = &self.events[i];
            let s = e.energy - self.energy_at(e.day, (e.start_min + e.end_min()) * 0.5);
            self.events[i].strain = s;
        }
    }

    /// Your energy (0..1) at `minute` of `day`.
    pub fn energy_at(&self, day: i32, minute: f32) -> f32 {
        use crate::energy::{PER_HOUR, SAMPLES};
        if !(0..DAYS).contains(&day) || self.energy.is_empty() {
            return 0.5;
        }
        let x = (minute / 60. * PER_HOUR as f32).clamp(0., (SAMPLES - 1) as f32);
        let (k, t) = (x.floor() as usize, x.fract());
        let base = day as usize * SAMPLES;
        let a = self.energy[base + k];
        let b = self.energy[base + (k + 1).min(SAMPLES - 1)];
        a + (b - a) * t
    }

    /// The instants this dataset covers, for querying the store.
    pub fn span(&self) -> (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) {
        let local = |d: NaiveDate| {
            Local.from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap()).earliest().map_or_else(Default::default, |t| t.with_timezone(&chrono::Utc))
        };
        (local(self.origin), local(self.date(DAYS)))
    }
}

/// Default demand by calendar, until rules can set it per event.
/// Builds the view's dataset from synced occurrences, in the local time zone.
/// Timed events crossing midnight are split into per-day segments.
pub fn from_occurrences(occurrences: &[crate::sync::store::Stored], calendar_of: impl Fn(&str) -> usize, calendars: &[Calendar]) -> Dataset {
    let (origin, today) = range();
    let mut events = Vec::new();
    let mut all_day = vec![0u16; DAYS as usize];
    let day_of = |d: NaiveDate| (d - origin).num_days() as i32;
    for (group, s) in occurrences.iter().enumerate() {
        let o = &s.occ;
        let calendar = calendar_of(&s.source);
        if o.all_day {
            let mut d = o.start.date_naive();
            // End is exclusive; a zero-length all-day event still covers its date.
            let last = o.end.date_naive().max(d + Duration::days(1));
            while d < last {
                if let Some(c) = all_day.get_mut(day_of(d) as usize) {
                    *c = c.saturating_add(1);
                }
                d += Duration::days(1);
            }
            continue;
        }
        let (start, end) = (o.start.with_timezone(&Local), o.end.with_timezone(&Local).max(o.start.with_timezone(&Local)));
        let mut seg = start;
        loop {
            let date = seg.date_naive();
            let midnight = Local.from_local_datetime(&(date + Duration::days(1)).and_hms_opt(0, 0, 0).unwrap()).earliest();
            let seg_end = midnight.map_or(end, |m| m.min(end));
            let day = day_of(date);
            if (0..DAYS).contains(&day) {
                let start_min = seg.time().num_seconds_from_midnight() as f32 / 60.;
                // Zero-length events still need to be visible.
                let dur_min = ((seg_end - seg).num_seconds() as f32 / 60.).max(15.);
                events.push(Event {
                    title: SharedString::from(o.summary.clone()),
                    calendar,
                    day,
                    start_min,
                    dur_min,
                    energy: calendars.get(calendar).map_or(0.4, |c| c.demand),
                    status: if o.status.as_deref() == Some("TENTATIVE") { Status::Tentative } else { Status::Confirmed },
                    location: o.location.clone().map(SharedString::from),
                    notes: o.description.as_ref().map(|d| {
                        let d: String = d.chars().take(280).collect();
                        SharedString::from(d)
                    }),
                    group: group as u32,
                    strain: 0.,
                });
            }
            match midnight {
                Some(m) if m < end => seg = m,
                _ => break,
            }
        }
    }
    Dataset::new(origin, today, events, all_day)
}

/// The instants the app's data range covers.
pub fn generate_span() -> (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) {
    let (origin, _) = range();
    Dataset::new(origin, 0, Vec::new(), Vec::new()).span()
}
