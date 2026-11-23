//! Travel: where you are over time, from events whose start
//! and end are in different time zones (a journey: a flight, a train, a drive). Google lets an event have separate start and
//! end zones; calendar feeds say the same with `DTSTART;TZID=…` and `DTEND;TZID=…`.
//!
//! Reads the cached events' raw form, so nothing else in calm needs to know about it. From the
//! legs it answers "which zone am I in at this instant?", for the zone rail and the drawer.

use chrono::{DateTime, FixedOffset, NaiveDateTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::Value;

/// A zone as events give it: an IANA name, or a fixed offset (Google's `GMT+03:00`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zone {
    Named(Tz),
    Fixed(i32),
}

impl Zone {
    pub fn parse(s: &str) -> Option<Zone> {
        let s = s.trim();
        if let Ok(tz) = s.parse::<Tz>() {
            return Some(Zone::Named(tz));
        }
        // GMT+03:00, UTC-4, GMT+5:30
        let rest = s.strip_prefix("GMT").or_else(|| s.strip_prefix("UTC"))?;
        if rest.is_empty() {
            return Some(Zone::Fixed(0));
        }
        let sign = match rest.as_bytes()[0] {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        let (h, m) = rest[1..].split_once(':').unwrap_or((&rest[1..], "0"));
        let secs = h.parse::<i32>().ok()? * 3600 + m.parse::<i32>().ok()? * 60;
        (secs <= 14 * 3600).then_some(Zone::Fixed(sign * secs))
    }

    /// Seconds east of UTC at `t`.
    pub fn offset(&self, t: DateTime<Utc>) -> i32 {
        match self {
            Zone::Named(tz) => tz.offset_from_utc_datetime(&t.naive_utc()).fix().local_minus_utc(),
            Zone::Fixed(s) => *s,
        }
    }

    /// A short name at `t`: the zone's abbreviation, else `GMT+8`.
    pub fn label(&self, t: DateTime<Utc>) -> String {
        if let Zone::Named(tz) = self {
            let abbr = t.with_timezone(tz).format("%Z").to_string();
            if abbr.chars().any(|c| c.is_ascii_alphabetic()) {
                return abbr;
            }
        }
        let s = self.offset(t);
        let (h, m) = (s.abs() / 3600, s.abs() % 3600 / 60);
        let sign = if s < 0 { '-' } else { '+' };
        if m == 0 { format!("GMT{sign}{h}") } else { format!("GMT{sign}{h}:{m:02}") }
    }

    /// As a time-zone database zone: named ones as they are, whole-hour offsets as `Etc/GMT∓h`.
    pub fn tz(&self) -> Option<Tz> {
        match self {
            Zone::Named(tz) => Some(*tz),
            Zone::Fixed(s) if s % 3600 == 0 => format!("Etc/GMT{:+}", -s / 3600).replace("GMT+0", "GMT").parse().ok(),
            Zone::Fixed(_) => None,
        }
    }

    pub fn local(&self, t: DateTime<Utc>) -> NaiveDateTime {
        t.naive_utc() + chrono::Duration::seconds(self.offset(t) as i64)
    }
}

/// A stretch of time: staying put (`ahead` of home, in hours), or on the way, on leg `air`.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub air: Option<usize>,
    pub ahead: f32,
}

/// One move between zones.
#[derive(Clone, Debug)]
pub struct Leg {
    pub title: String,
    pub depart: DateTime<Utc>,
    pub arrive: DateTime<Utc>,
    pub from: Zone,
    pub to: Zone,
}

impl Leg {
    /// Hours the clock moves (positive flying east).
    pub fn shift_hours(&self) -> f32 {
        (self.to.offset(self.arrive) - self.from.offset(self.depart)) as f32 / 3600.
    }
}

/// Where you are: `home` until the first leg, then wherever each leg ends.
#[derive(Clone, Debug)]
pub struct Travel {
    pub home: Zone,
    pub legs: Vec<Leg>,
}

impl Travel {
    /// Legs from cached events in their raw form (Google's JSON or an iCalendar `VEVENT`).
    pub fn from_raw<'a>(raws: impl IntoIterator<Item = &'a str>, home: Zone) -> Travel {
        let mut legs: Vec<Leg> = raws.into_iter().filter_map(|r| if r.trim_start().starts_with('{') { google_leg(r) } else { ics_leg(r) }).collect();
        legs.sort_by_key(|l| l.depart);
        legs.dedup_by(|a, b| a.depart == b.depart && a.title == b.title);
        Travel { home, legs }
    }

    /// The zone you're in at `t`, and whether you're on the way between zones.
    pub fn zone_at(&self, t: DateTime<Utc>) -> (Zone, bool) {
        let Some(first) = self.legs.first() else { return (self.home, false) };
        if t < first.depart {
            return (first.from, false);
        }
        let k = self.legs.partition_point(|l| l.depart <= t) - 1;
        let leg = &self.legs[k];
        if t < leg.arrive { (leg.to, true) } else { (leg.to, false) }
    }

    /// Hours your clock is ahead of home at `t` (negative: behind).
    pub fn ahead_of_home(&self, t: DateTime<Utc>) -> f32 {
        (self.zone_at(t).0.offset(t) - self.home.offset(t)) as f32 / 3600.
    }

    /// The stretches of `from..to`: staying in a zone, or on the way on a leg.
    pub fn segments(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Segment> {
        let mut out = Vec::new();
        let mut t = from;
        while t < to {
            let next = self.legs.iter().enumerate().find(|(_, l)| l.arrive > t).filter(|(_, l)| l.depart < to);
            match next {
                Some((k, l)) if l.depart <= t => {
                    let end = l.arrive.min(to);
                    out.push(Segment { from: t, to: end, air: Some(k), ahead: 0. });
                    t = end;
                }
                Some((_, l)) => {
                    out.push(Segment { from: t, to: l.depart, air: None, ahead: self.ahead_of_home(t) });
                    t = l.depart;
                }
                None => {
                    out.push(Segment { from: t, to, air: None, ahead: self.ahead_of_home(t) });
                    t = to;
                }
            }
        }
        out
    }

    /// The zones moves go to and from during `from..to` (for extra hour columns).
    pub fn zones_between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Tz> {
        let mut out: Vec<Tz> = Vec::new();
        for l in self.legs.iter().filter(|l| l.arrive >= from && l.depart <= to) {
            for z in [l.from, l.to] {
                if let Some(tz) = z.tz()
                    && !out.contains(&tz)
                {
                    out.push(tz);
                }
            }
        }
        out
    }

    /// The leg an event is, by its start.
    pub fn leg_at(&self, depart: DateTime<Utc>) -> Option<&Leg> {
        self.legs.iter().find(|l| l.depart == depart)
    }
}

/// Home: your system's zone.
pub fn home() -> Zone {
    match crate::zones::Zones::load(None, None).primary {
        Some(tz) => Zone::Named(tz),
        None => Zone::Fixed(chrono::Local::now().offset().local_minus_utc()),
    }
}

/// Travel from the cache, if any events move between zones.
pub fn load() -> Option<Travel> {
    let store = crate::sync::store::Store::open(&crate::sync::db_path()).ok()?;
    let raws = store.all_raw().ok()?;
    let t = Travel::from_raw(raws.iter().map(String::as_str), home());
    (!t.legs.is_empty()).then_some(t)
}

/// `calm travel`: each move, and where you are now.
pub fn cli() -> i32 {
    let Some(t) = load() else {
        println!("No events move between time zones. Give one different start and end zones (Google's");
        println!("\"separate start and end time zones\", or DTSTART;TZID / DTEND;TZID in a feed).");
        return 0;
    };
    for l in &t.legs {
        let (d, a) = (l.from.local(l.depart), l.to.local(l.arrive));
        println!(
            "{}  {} {} \u{2192} {} {}  ({:+} h)  {}",
            d.format("%a %-d %b %Y"),
            d.format("%H:%M"),
            l.from.label(l.depart),
            a.format("%H:%M"),
            l.to.label(l.arrive),
            l.shift_hours(),
            l.title
        );
    }
    let now = Utc::now();
    let (zone, flying) = t.zone_at(now);
    println!("\nNow: {}{}, {:+} h from home ({}).", zone.label(now), if flying { " (on the way)" } else { "" }, t.ahead_of_home(now), t.home.label(now));
    println!("In calm: click a move for its local times, or pick Rail \u{2192} Zone in the view editor (E).");
    0
}

/// A Google event whose start and end zones differ (by offset, not just by name).
fn google_leg(raw: &str) -> Option<Leg> {
    let v: Value = serde_json::from_str(raw).ok()?;
    if v["status"] == "cancelled" || v.get("recurrence").is_some() {
        return None;
    }
    let at = |k: &str| -> Option<(DateTime<Utc>, Zone)> {
        let t = DateTime::parse_from_rfc3339(v[k]["dateTime"].as_str()?).ok()?.with_timezone(&Utc);
        Some((t, Zone::parse(v[k]["timeZone"].as_str()?)?))
    };
    let ((depart, from), (arrive, to)) = (at("start")?, at("end")?);
    (from.offset(depart) != to.offset(arrive)).then(|| Leg { title: v["summary"].as_str().unwrap_or("").to_string(), depart, arrive, from, to })
}

/// An iCalendar event with `DTSTART;TZID=A` and `DTEND;TZID=B` in different zones.
fn ics_leg(raw: &str) -> Option<Leg> {
    if raw.contains("\nRRULE") {
        return None;
    }
    let line = |name: &str| raw.lines().find(|l| l.starts_with(name)).map(str::trim);
    let at = |name: &str| -> Option<(DateTime<Utc>, Zone)> {
        let l = line(name)?;
        let (params, value) = l.split_once(':')?;
        let tzid = params.split(';').find_map(|p| p.strip_prefix("TZID="))?.trim_matches('"');
        let zone = Zone::parse(tzid)?;
        let local = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").ok()?;
        let utc = match zone {
            Zone::Named(tz) => tz.from_local_datetime(&local).earliest()?.with_timezone(&Utc),
            Zone::Fixed(s) => FixedOffset::east_opt(s)?.from_local_datetime(&local).earliest()?.with_timezone(&Utc),
        };
        Some((utc, zone))
    };
    let ((depart, from), (arrive, to)) = (at("DTSTART")?, at("DTEND")?);
    let title = line("SUMMARY").and_then(|l| l.split_once(':')).map_or(String::new(), |(_, s)| s.to_string());
    (from.offset(depart) != to.offset(arrive)).then_some(Leg { title, depart, arrive, from, to })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn legs_and_where_you_are() {
        let raws = [
            // Two legs, as Google keeps them (fixed-offset zones).
            r#"{"summary":"QR 106 LHR-DOH","start":{"dateTime":"2025-10-09T18:55:00+01:00","timeZone":"GMT+01:00"},"end":{"dateTime":"2025-10-10T01:45:00+01:00","timeZone":"GMT+03:00"}}"#,
            r#"{"summary":"QR 928 DOH-MNL","start":{"dateTime":"2025-10-10T05:50:00+01:00","timeZone":"GMT+03:00"},"end":{"dateTime":"2025-10-10T15:20:00+01:00","timeZone":"GMT+08:00"}}"#,
            // Same zone both ends: not a move.
            r#"{"summary":"Flight to London","start":{"dateTime":"2024-11-30T14:55:00Z","timeZone":"Europe/London"},"end":{"dateTime":"2024-12-01T05:30:00Z","timeZone":"Europe/London"}}"#,
            // Home again, from a calendar feed.
            "BEGIN:VEVENT\nSUMMARY:PR 720 MNL-LHR\nDTSTART;TZID=Asia/Manila:20251020T232000\nDTEND;TZID=Europe/London:20251021T053000\nEND:VEVENT",
        ];
        let home = Zone::Named(chrono_tz::Europe::London);
        let t = Travel::from_raw(raws, home);
        assert_eq!(t.legs.len(), 3);
        assert_eq!(t.legs[1].shift_hours(), 5.);
        assert_eq!(t.zone_at(utc("2025-10-01T12:00:00Z")).0, Zone::Fixed(3600), "before the first leg: where it leaves from");
        assert_eq!(t.ahead_of_home(utc("2025-10-12T12:00:00Z")), 7., "in Manila, BST + 7");
        assert!(t.zone_at(utc("2025-10-10T08:00:00Z")).1, "on the way");
        assert_eq!(t.ahead_of_home(utc("2025-10-22T12:00:00Z")), 0., "home");
        assert_eq!(Zone::parse("GMT+05:30"), Some(Zone::Fixed(19800)));
        assert_eq!(Zone::Fixed(3 * 3600).tz(), Some(chrono_tz::Etc::GMTMinus3));
        // Ground, air, ground across the Doha–Manila leg.
        let seg = t.segments(utc("2025-10-10T00:00:00Z"), utc("2025-10-11T00:00:00Z"));
        assert_eq!(seg.iter().map(|s| s.air.is_some()).collect::<Vec<_>>(), [true, false, true, false]);
        assert_eq!(seg.last().unwrap().ahead, 7.);
        assert_eq!(t.zones_between(utc("2025-10-09T00:00:00Z"), utc("2025-10-11T00:00:00Z")).len(), 3);
        assert_eq!(Zone::Fixed(28800).label(utc("2025-10-12T12:00:00Z")), "GMT+8");
    }
}
