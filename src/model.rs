//! Normalized event model and a deterministic synthetic dataset anchored on
//! the real current date. Day indices count from `Dataset::origin`, a Monday.

use chrono::{Datelike, Duration, Local, NaiveDate, Timelike};
use gpui::SharedString;

/// From the Monday on or before 1 January, through the first quarter of next year.
pub const WEEKS: i32 = 65;
pub const DAYS: i32 = WEEKS * 7;
pub const CALENDARS: [&str; 4] = ["Work", "Personal", "Family", "Focus"];

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
    let today_date = Local::now().date_naive();
    let jan1 = NaiveDate::from_ymd_opt(today_date.year(), 1, 1).unwrap();
    let origin = jan1 - Duration::days(jan1.weekday().num_days_from_monday() as i64);
    let today = (today_date - origin).num_days() as i32;

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

    // Day order lets views touch only the days they can see.
    events.sort_by_key(|e| e.day);
    let mut day_start = vec![0; DAYS as usize + 1];
    for e in &events {
        day_start[e.day as usize + 1] += 1;
    }
    for d in 0..DAYS as usize {
        day_start[d + 1] += day_start[d];
    }
    Dataset { origin, today, events, day_start }
}

/// A default chronotype: personal energy (0..1) through the day. This is a
/// property of the person and the hour, not of any event. Key points are
/// (hour, level); the curve is a periodic Catmull–Rom spline through them.
const CHRONOTYPE: [(f32, f32); 10] = [
    (0., 0.12),
    (3.5, 0.06),
    (6.5, 0.25),
    (8.5, 0.66),
    (10.5, 0.88),
    (12.5, 0.72),
    (14.5, 0.42),
    (17., 0.68),
    (19.5, 0.5),
    (22., 0.24),
];

pub fn personal_energy(minute: f32) -> f32 {
    let h = (minute / 60.).rem_euclid(24.);
    let n = CHRONOTYPE.len();
    let i = CHRONOTYPE.iter().rposition(|&(kh, _)| kh <= h).unwrap_or(0);
    let at = |k: isize| {
        let j = k.rem_euclid(n as isize) as usize;
        let wraps = k.div_euclid(n as isize) as f32;
        (CHRONOTYPE[j].0 + 24. * wraps, CHRONOTYPE[j].1)
    };
    let i = i as isize;
    let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
    let t = (h - p1.0) / (p2.0 - p1.0);
    let (t2, t3) = (t * t, t * t * t);
    let v = 0.5
        * (2. * p1.1
            + (-p0.1 + p2.1) * t
            + (2. * p0.1 - 5. * p1.1 + 4. * p2.1 - p3.1) * t2
            + (-p0.1 + 3. * p1.1 - 3. * p2.1 + p3.1) * t3);
    v.clamp(0., 1.)
}

/// How far an event's demand exceeds the energy you typically have at its midpoint.
pub fn strain(e: &Event) -> f32 {
    e.energy - personal_energy((e.start_min + e.end_min()) * 0.5)
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
