//! Time zones: a global setting, not part of a view's grammar. The primary
//! zone is the system's (`TZ`, else `/etc/localtime`), matching how the
//! current time is read; extra zones come from `~/.config/calm/config`
//! (`zones = America/New_York, Europe/London`) or `--zones`.
//!
//! Event times are in the primary zone. Other zones are shown by labelling
//! the same time-of-day axis again, offset for the date in view so daylight
//! saving and half-hour zones come out right.

use chrono::{NaiveDate, NaiveTime, Offset, TimeZone};
use chrono_tz::{OffsetName, Tz};

#[derive(Clone, Default)]
pub struct Zones {
    /// `None` when the system zone couldn't be named; offsets then use chrono's `Local`.
    pub primary: Option<Tz>,
    pub extra: Vec<Tz>,
    /// Short names for axes (`short.Asia/Manila = 🇵🇭`; `label.` also works) and
    /// long names for hover (`long.Asia/Manila = Manila`).
    pub short: std::collections::HashMap<Tz, String>,
    pub long: std::collections::HashMap<Tz, String>,
}

fn system_zone() -> Option<Tz> {
    if let Ok(tz) = std::env::var("TZ") {
        let name = tz.trim_start_matches(':');
        if let Ok(tz) = name.parse() {
            return Some(tz);
        }
    }
    let path = std::fs::read_link("/etc/localtime").ok()?;
    let path = path.to_string_lossy();
    let name = path.split("zoneinfo/").nth(1)?;
    name.parse().ok()
}

fn parse_list(s: &str) -> Vec<Tz> {
    s.split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .filter_map(|n| match n.parse::<Tz>() {
            Ok(tz) => Some(tz),
            Err(_) => {
                eprintln!("calm: unknown time zone {n:?}");
                None
            }
        })
        .collect()
}

impl Zones {
    /// System zone plus extras from the config file, overridden by `cli` when given.
    pub fn load(cli: Option<&str>) -> Self {
        // Trust what the system resolves (as `date` does) over the zone's *name*: a
        // `/etc/localtime` link can be named for one zone while holding another's rules.
        let today = chrono::Local::now().date_naive();
        let config = read_config();
        let setting = |key: &str| config.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        let configured = cli.map(str::to_string).or_else(|| setting("zones"));
        let names = |prefixes: &[&str]| -> std::collections::HashMap<Tz, String> {
            config
            .iter()
            .filter_map(|(k, v)| {
                let name = prefixes.iter().find_map(|p| k.strip_prefix(p))?;
                match name.parse::<Tz>() {
                    Ok(tz) => Some((tz, v.clone())),
                    Err(_) => {
                        eprintln!("calm: unknown time zone in {k:?}");
                        None
                    }
                }
            })
            .collect()
        };
        let (short, long) = (names(&["short.", "label."]), names(&["long."]));
        let local = Self::default().primary_offset(today);
        let primary = system_zone().filter(|tz| offset(*tz, today) == local);
        let mut extra = configured.as_deref().map(parse_list).unwrap_or_default();
        // Drop whichever configured zone you are currently in (by name, or by
        // offset when the system zone has no trustworthy name).
        extra.retain(|z| Some(*z) != primary && offset(*z, today) != local);
        Self { primary, extra, short, long }
    }

    /// UTC offset of the primary zone at midday on `date`, in minutes.
    fn primary_offset(&self, date: NaiveDate) -> i32 {
        let noon = date.and_time(NaiveTime::from_hms_opt(12, 0, 0).unwrap());
        match self.primary {
            Some(tz) => offset(tz, date),
            None => chrono::Local
                .from_local_datetime(&noon)
                .earliest()
                .map_or(0, |t| t.offset().fix().local_minus_utc() / 60),
        }
    }

    /// Minutes to add to a primary-zone time to get the time in `tz` on `date`.
    pub fn delta(&self, tz: Tz, date: NaiveDate) -> i32 {
        offset(tz, date) - self.primary_offset(date)
    }

    /// The configured label for `tz`, else its abbreviation on `date`.
    pub fn label(&self, tz: Tz, date: NaiveDate) -> String {
        self.short.get(&tz).cloned().unwrap_or_else(|| abbrev(tz, date))
    }

    pub fn primary_abbrev(&self, date: NaiveDate) -> String {
        match self.primary {
            Some(tz) => self.label(tz, date),
            None => local_abbrev(date),
        }
    }
}

/// `key = value` lines from `~/.config/calm/config`; `#` starts a comment.
fn read_config() -> Vec<(String, String)> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")));
    let Some(text) = dir.and_then(|d| std::fs::read_to_string(d.join("calm/config")).ok()) else {
        return Vec::new();
    };
    text.lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// UTC offset of `tz` at midday on `date`, in minutes.
fn offset(tz: Tz, date: NaiveDate) -> i32 {
    let noon = date.and_time(NaiveTime::from_hms_opt(12, 0, 0).unwrap());
    tz.from_local_datetime(&noon).earliest().map_or(0, |t| t.offset().fix().local_minus_utc() / 60)
}

/// Short name for `tz` on `date`: "PST", "GMT", or "+0530" where the database has none.
pub fn abbrev(tz: Tz, date: NaiveDate) -> String {
    let noon = date.and_time(NaiveTime::from_hms_opt(12, 0, 0).unwrap());
    let Some(t) = tz.from_local_datetime(&noon).earliest() else { return tz.name().into() };
    match t.offset().abbreviation() {
        Some(a) if !a.starts_with(['+', '-']) => a.to_string(),
        _ => {
            let m = t.offset().fix().local_minus_utc() / 60;
            format!("{}{:02}{:02}", if m < 0 { '-' } else { '+' }, m.abs() / 60, m.abs() % 60)
        }
    }
}

/// A primary-zone minute shifted by `delta`, wrapped to the day and labelled
/// compactly for an axis ("9 am", "12:30 pm"), with a day marker if it crosses midnight.
pub fn axis_label(primary_min: f32, delta: i32) -> String {
    let m = primary_min.round() as i32 + delta;
    let day_shift = m.div_euclid(1440);
    let m = m.rem_euclid(1440);
    let (h, mm) = (m / 60, m % 60);
    let (h12, ap) = (if h % 12 == 0 { 12 } else { h % 12 }, if h < 12 { "am" } else { "pm" });
    let base = if mm == 0 { format!("{h12} {ap}") } else { format!("{h12}:{mm:02}{}", &ap[..1]) };
    match day_shift {
        0 => base,
        d if d > 0 => format!("{base}+"),
        _ => format!("{base}−"),
    }
}

/// The system's own abbreviation for local time at midday on `date`, via libc
/// (the same source as `date`), for when the zone has no trustworthy name.
fn local_abbrev(date: NaiveDate) -> String {
    use chrono::Datelike;
    // SAFETY: plain libc time calls on stack-owned values; `tm_zone` points into
    // libc's static zone table, copied out before returning.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        tm.tm_year = date.year() - 1900;
        tm.tm_mon = date.month0() as i32;
        tm.tm_mday = date.day() as i32;
        tm.tm_hour = 12;
        tm.tm_isdst = -1;
        tzset();
        let t = libc::mktime(&mut tm);
        let mut out: libc::tm = std::mem::zeroed();
        if t == -1 || libc::localtime_r(&t, &mut out).is_null() || out.tm_zone.is_null() {
            return "Local".into();
        }
        std::ffi::CStr::from_ptr(out.tm_zone).to_string_lossy().into_owned()
    }
}

unsafe extern "C" {
    /// POSIX; not bound by the libc crate.
    fn tzset();
}

/// What hovering a zone label reveals: its long name, the time there now,
/// and its offset and abbreviation.
pub struct ZoneInfo {
    pub name: String,
    pub now: String,
    pub detail: String,
}

impl Zones {
    /// `None` is the primary zone.
    pub fn info(&self, tz: Option<Tz>) -> ZoneInfo {
        let now_utc = chrono::Utc::now();
        let (naive, minutes, short) = match tz.or(self.primary) {
            Some(tz) => {
                let t = now_utc.with_timezone(&tz);
                (t.naive_local(), t.offset().fix().local_minus_utc() / 60, abbrev(tz, t.date_naive()))
            }
            None => {
                let t = now_utc.with_timezone(&chrono::Local);
                (t.naive_local(), t.offset().local_minus_utc() / 60, local_abbrev(t.date_naive()))
            }
        };
        let zone = tz.or(self.primary);
        let name = zone
            .and_then(|z| self.long.get(&z).cloned())
            .or_else(|| zone.map(|z| z.name().replace('_', " ")))
            .unwrap_or_else(|| "Local time".into());
        let name = if tz.is_none() { format!("{name} · here") } else { name };
        let sign = if minutes < 0 { '−' } else { '+' };
        let (h, m) = (minutes.abs() / 60, minutes.abs() % 60);
        let utc = if m == 0 { format!("UTC{sign}{h}") } else { format!("UTC{sign}{h}:{m:02}") };
        ZoneInfo {
            name,
            now: naive.format("%-I:%M %P · %A %-d %b").to_string(),
            detail: format!("{utc} · {short}"),
        }
    }
}
