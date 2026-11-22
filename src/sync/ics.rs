//! iCalendar parsing, normalisation and recurrence expansion.
//!
//! Feeds are split into `VEVENT` blocks and each block is parsed on its own,
//! so one malformed event is skipped and counted rather than failing the feed.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use icalendar::parser::{Property, read_components, unfold};

/// A point in time as the feed expressed it.
#[derive(Clone, Debug, PartialEq)]
pub enum Moment {
    /// All-day: a calendar date, the same wherever you are.
    Date(NaiveDate),
    Utc(DateTime<Utc>),
    Zoned(NaiveDateTime, Tz),
    /// No zone given: the viewer's local time.
    Floating(NaiveDateTime),
}

impl Moment {
    /// The instant this denotes; dates and floating times are read in the local zone.
    pub fn instant(&self) -> DateTime<Utc> {
        let local = |n: NaiveDateTime| {
            chrono::Local.from_local_datetime(&n).earliest().map_or_else(|| n.and_utc(), |t| t.with_timezone(&Utc))
        };
        match self {
            Moment::Date(d) => local(d.and_hms_opt(0, 0, 0).unwrap()),
            Moment::Utc(t) => *t,
            Moment::Zoned(n, tz) => tz.from_local_datetime(n).earliest().map_or_else(|| n.and_utc(), |t| t.with_timezone(&Utc)),
            Moment::Floating(n) => local(*n),
        }
    }

    pub fn is_date(&self) -> bool {
        matches!(self, Moment::Date(_))
    }
}

/// One `VEVENT`, normalised.
#[derive(Clone, Debug)]
pub struct Component {
    pub uid: String,
    /// For overrides of a recurring event: the original start of the instance replaced.
    pub recurrence_id: Option<DateTime<Utc>>,
    pub sequence: i64,
    pub raw: String,
    pub summary: String,
    pub location: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
    pub transp: Option<String>,
    pub organizer: Option<String>,
    pub attendees: Vec<String>,
    pub categories: Vec<String>,
    pub start: Moment,
    /// Length; all-day events default to one day, timed events to zero.
    pub duration: Duration,
    pub rrule: Option<String>,
    pub exdates: Vec<Moment>,
    pub rdates: Vec<Moment>,
}

impl Component {
    pub fn cancelled(&self) -> bool {
        self.status.as_deref() == Some("CANCELLED")
    }
}

#[derive(Debug)]
pub struct Feed {
    pub components: Vec<Component>,
    /// `VEVENT`s seen, and how many were unusable.
    #[allow(dead_code)] // Reported in tests and kept for diagnostics.
    pub total: usize,
    pub skipped: usize,
}

#[derive(Debug, PartialEq)]
pub enum FeedError {
    /// Not iCalendar at all: an HTML error page, a captive portal, an empty body.
    NotCalendar,
    /// Too many events failed to parse to trust the rest.
    TooBroken { skipped: usize, total: usize },
}

impl std::fmt::Display for FeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeedError::NotCalendar => write!(f, "response is not an iCalendar feed"),
            FeedError::TooBroken { skipped, total } => write!(f, "{skipped} of {total} events could not be read"),
        }
    }
}

/// Windows zone names some servers (Exchange, Outlook) send instead of IANA names.
const WINDOWS_ZONES: &[(&str, &str)] = &[
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("Mountain Standard Time", "America/Denver"),
    ("Central Standard Time", "America/Chicago"),
    ("Eastern Standard Time", "America/New_York"),
    ("GMT Standard Time", "Europe/London"),
    ("Greenwich Standard Time", "Etc/GMT"),
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("Central Europe Standard Time", "Europe/Budapest"),
    ("Romance Standard Time", "Europe/Paris"),
    ("India Standard Time", "Asia/Kolkata"),
    ("China Standard Time", "Asia/Shanghai"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("Taipei Standard Time", "Asia/Taipei"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("AUS Eastern Standard Time", "Australia/Sydney"),
    ("UTC", "UTC"),
];

/// Resolves a `TZID`: IANA names, then Windows names, then the feed's own
/// `VTIMEZONE` (as a fixed standard offset; good enough for unknown zones).
struct Zones {
    fixed: HashMap<String, i32>,
}

enum Zone {
    Named(Tz),
    Fixed(i32),
}

impl Zones {
    fn resolve(&self, tzid: &str) -> Option<Zone> {
        let id = tzid.trim_matches('"');
        if let Ok(tz) = id.parse::<Tz>() {
            return Some(Zone::Named(tz));
        }
        if let Some((_, iana)) = WINDOWS_ZONES.iter().find(|(w, _)| *w == id) {
            return iana.parse().ok().map(Zone::Named);
        }
        // Some feeds prefix IANA names with a path, e.g. "/mozilla.org/…/Europe/London".
        if let Some(tz) = id.rsplit('/').collect::<Vec<_>>().windows(2).find_map(|w| format!("{}/{}", w[1], w[0]).parse().ok()) {
            return Some(Zone::Named(tz));
        }
        self.fixed.get(id).map(|&m| Zone::Fixed(m))
    }
}

fn param<'a>(p: &'a Property<'_>, key: &str) -> Option<&'a str> {
    p.params.iter().find(|x| x.key.as_str().eq_ignore_ascii_case(key)).and_then(|x| x.val.as_ref()).map(|v| v.as_str())
}

fn parse_offset(s: &str) -> Option<i32> {
    let (sign, rest) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => (1, s),
    };
    let h: i32 = rest.get(0..2)?.parse().ok()?;
    let m: i32 = rest.get(2..4)?.parse().ok()?;
    Some(sign * (h * 60 + m))
}

fn parse_moment(value: &str, tzid: Option<&str>, date_only: bool, zones: &Zones) -> Option<Moment> {
    let v = value.trim();
    if date_only || (v.len() == 8 && !v.contains('T')) {
        return NaiveDate::parse_from_str(v, "%Y%m%d").ok().map(Moment::Date);
    }
    if let Some(utc) = v.strip_suffix('Z') {
        return NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S").ok().map(|n| Moment::Utc(n.and_utc()));
    }
    let n = NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok()?;
    match tzid.and_then(|id| zones.resolve(id)) {
        Some(Zone::Named(tz)) => Some(Moment::Zoned(n, tz)),
        Some(Zone::Fixed(m)) => Some(Moment::Utc((n - Duration::minutes(m as i64)).and_utc())),
        None => Some(Moment::Floating(n)),
    }
}

fn prop_moments(p: &Property<'_>, zones: &Zones) -> Vec<Moment> {
    let date_only = param(p, "VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE"));
    let tzid = param(p, "TZID");
    p.val.as_str().split(',').filter_map(|v| parse_moment(v, tzid, date_only, zones)).collect()
}

/// ISO 8601 duration as used by iCalendar (`PT1H30M`, `P1D`, `-PT15M`).
fn parse_duration(s: &str) -> Option<Duration> {
    let (neg, s) = s.strip_prefix('-').map_or((false, s.trim_start_matches('+')), |r| (true, r));
    let s = s.strip_prefix('P')?;
    let (mut total, mut num, mut in_time) = (0i64, String::new(), false);
    for c in s.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            unit => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += n * match (unit, in_time) {
                    ('W', _) => 7 * 86400,
                    ('D', _) => 86400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
            }
        }
    }
    Some(Duration::seconds(if neg { -total } else { total }))
}

fn text(p: &Property<'_>) -> String {
    p.val.clone().unescape_text().as_str().trim().to_string()
}

/// Splits unfolded text into top-level `BEGIN:NAME … END:NAME` blocks.
fn blocks<'a>(lines: &[&'a str], name: &str) -> Vec<Vec<&'a str>> {
    let (begin, end) = (format!("BEGIN:{name}"), format!("END:{name}"));
    let mut out = Vec::new();
    let mut cur: Option<Vec<&str>> = None;
    for &line in lines {
        let l = line.trim_end();
        if l.eq_ignore_ascii_case(&begin) && cur.is_none() {
            cur = Some(vec![l]);
        } else if let Some(c) = cur.as_mut() {
            c.push(l);
            if l.eq_ignore_ascii_case(&end) {
                out.push(cur.take().unwrap());
            }
        }
    }
    out
}

/// Parses a whole feed. Fails only if it isn't a calendar or is mostly broken.
pub fn parse(input: &str) -> Result<Feed, FeedError> {
    if !input.contains("BEGIN:VCALENDAR") {
        return Err(FeedError::NotCalendar);
    }
    let unfolded = unfold(input);
    let lines: Vec<&str> = unfolded.lines().collect();

    // Standard offsets of the feed's own zones, for TZIDs nothing else recognises.
    let mut fixed = HashMap::new();
    for block in blocks(&lines, "VTIMEZONE") {
        let id = block.iter().find_map(|l| l.strip_prefix("TZID:")).map(str::trim);
        let std = blocks(&block[1..], "STANDARD").into_iter().next();
        let offset = std.as_ref().and_then(|b| b.iter().find_map(|l| l.strip_prefix("TZOFFSETTO:"))).and_then(|o| parse_offset(o.trim()));
        if let (Some(id), Some(off)) = (id, offset) {
            fixed.insert(id.to_string(), off);
        }
    }
    let zones = Zones { fixed };

    let events = blocks(&lines, "VEVENT");
    let total = events.len();
    let mut components = Vec::with_capacity(total);
    let mut skipped = 0;
    for block in events {
        let raw = block.join("\n");
        let text = raw.clone() + "\n";
        let parsed = read_components(&text).ok().and_then(|mut cs| (!cs.is_empty()).then(|| cs.swap_remove(0)));
        match parsed.and_then(|c| normalise(&c, raw, &zones)) {
            Some(c) => components.push(c),
            None => skipped += 1,
        }
    }
    if total > 0 && skipped * 20 > total {
        return Err(FeedError::TooBroken { skipped, total });
    }
    Ok(Feed { components, total, skipped })
}

fn normalise(c: &icalendar::parser::Component<'_>, raw: String, zones: &Zones) -> Option<Component> {
    let find = |name: &str| c.properties.iter().find(|p| p.name.as_str().eq_ignore_ascii_case(name));
    let all = |name: &str| -> Vec<&Property<'_>> { c.properties.iter().filter(|p| p.name.as_str().eq_ignore_ascii_case(name)).collect() };

    let start = find("DTSTART").and_then(|p| prop_moments(p, zones).into_iter().next())?;
    let end = find("DTEND").and_then(|p| prop_moments(p, zones).into_iter().next());
    let duration = match (&start, end) {
        (Moment::Date(a), Some(Moment::Date(b))) => Duration::days((b - *a).num_days().max(1)),
        (_, Some(e)) => (e.instant() - start.instant()).max(Duration::zero()),
        (s, None) => find("DURATION").and_then(|p| parse_duration(p.val.as_str())).unwrap_or(if s.is_date() { Duration::days(1) } else { Duration::zero() }),
    };
    let uid = find("UID").map(text).filter(|u| !u.is_empty()).unwrap_or_else(|| format!("nouid-{:016x}", super::hash(&raw)));
    let recurrence_id = find("RECURRENCE-ID").and_then(|p| prop_moments(p, zones).into_iter().next()).map(|m| m.instant());
    let upper = |p: &Property<'_>| p.val.as_str().trim().to_ascii_uppercase();

    Some(Component {
        uid,
        recurrence_id,
        sequence: find("SEQUENCE").and_then(|p| p.val.as_str().trim().parse().ok()).unwrap_or(0),
        summary: find("SUMMARY").map(text).unwrap_or_else(|| "(no title)".into()),
        location: find("LOCATION").map(text).filter(|s| !s.is_empty()),
        description: find("DESCRIPTION").map(text).filter(|s| !s.is_empty()),
        status: find("STATUS").map(upper),
        transp: find("TRANSP").map(upper),
        organizer: find("ORGANIZER").map(|p| param(p, "CN").map(str::to_string).unwrap_or_else(|| text(p))),
        attendees: all("ATTENDEE").into_iter().map(|p| param(p, "CN").map(str::to_string).unwrap_or_else(|| text(p))).collect(),
        categories: all("CATEGORIES").into_iter().flat_map(|p| text(p).split(',').map(|s| s.trim().to_string()).collect::<Vec<_>>()).collect(),
        rrule: find("RRULE").map(|p| p.val.as_str().trim().to_string()),
        exdates: all("EXDATE").into_iter().flat_map(|p| prop_moments(p, zones)).collect(),
        rdates: all("RDATE").into_iter().flat_map(|p| prop_moments(p, zones)).collect(),
        start,
        duration,
        raw,
    })
}

/// One concrete occurrence of an event.
#[derive(Clone, Debug, PartialEq)]
pub struct Occurrence {
    pub uid: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// All-day: `start`'s *date* is what matters (stored at UTC midnight of the date).
    pub all_day: bool,
    pub summary: String,
    pub location: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
    pub transp: Option<String>,
    pub organizer: Option<String>,
    pub attendees: Vec<String>,
    pub categories: Vec<String>,
    /// The provider's ID for this occurrence: the event's own, or for an instance of a
    /// series `{series}_{original start}` (Google's instance IDs; what an edit targets).
    pub ref_id: String,
}

/// `{series}_{20261124T090000Z}`, or `{series}_{20261124}` for all-day: an instance's ID.
pub fn instance_id(series: &str, original: DateTime<Utc>, all_day: Option<NaiveDate>) -> String {
    match all_day {
        Some(d) => format!("{series}_{}", d.format("%Y%m%d")),
        None => format!("{series}_{}", original.format("%Y%m%dT%H%M%SZ")),
    }
}

fn occurrence(c: &Component, start: DateTime<Utc>, all_day_date: Option<NaiveDate>) -> Occurrence {
    let ref_id = match (c.recurrence_id, &c.rrule) {
        (Some(original), _) => instance_id(&c.uid, original, if let Moment::Date(_) = c.start { Some(original.date_naive()) } else { None }),
        (None, Some(_)) => instance_id(&c.uid, start, all_day_date),
        (None, None) => c.uid.clone(),
    };
    let (start, end) = match all_day_date {
        Some(d) => {
            let s = d.and_hms_opt(0, 0, 0).unwrap().and_utc();
            (s, s + c.duration)
        }
        None => (start, start + c.duration),
    };
    Occurrence {
        uid: c.uid.clone(),
        start,
        end,
        all_day: all_day_date.is_some(),
        summary: c.summary.clone(),
        location: c.location.clone(),
        description: c.description.clone(),
        status: c.status.clone(),
        transp: c.transp.clone(),
        organizer: c.organizer.clone(),
        attendees: c.attendees.clone(),
        categories: c.categories.clone(),
        ref_id,
    }
}

/// The DTSTART line rrule needs, and how to read its instances back.
fn rrule_dtstart(start: &Moment) -> String {
    match start {
        Moment::Utc(t) => format!("DTSTART:{}", t.format("%Y%m%dT%H%M%SZ")),
        Moment::Zoned(n, tz) => format!("DTSTART;TZID={}:{}", tz.name(), n.format("%Y%m%dT%H%M%S")),
        Moment::Floating(n) => format!("DTSTART:{}", n.format("%Y%m%dT%H%M%S")),
        // All-day series expand as local midnights; their dates are what we keep.
        Moment::Date(d) => format!("DTSTART:{}T000000", d.format("%Y%m%d")),
    }
}

/// rrule insists UNTIL is UTC when DTSTART has a zone; feeds often send it local or as a date.
fn normalise_rrule(rule: &str, start: &Moment) -> String {
    rule.split(';')
        .map(|part| {
            let Some(until) = part.strip_prefix("UNTIL=") else { return part.to_string() };
            if until.ends_with('Z') {
                return part.to_string();
            }
            let local = if until.len() == 8 {
                NaiveDate::parse_from_str(until, "%Y%m%d").ok().map(|d| d.and_hms_opt(23, 59, 59).unwrap())
            } else {
                NaiveDateTime::parse_from_str(until, "%Y%m%dT%H%M%S").ok()
            };
            let Some(local) = local else { return part.to_string() };
            let utc = match start {
                Moment::Zoned(_, tz) => Moment::Zoned(local, *tz).instant(),
                Moment::Utc(_) => local.and_utc(),
                _ => return format!("UNTIL={}", local.format("%Y%m%dT%H%M%S")),
            };
            format!("UNTIL={}", utc.format("%Y%m%dT%H%M%SZ"))
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// Expands every component into occurrences overlapping `window`, applying
/// exclusions, overrides and cancellations.
pub fn expand(components: &[Component], window: (DateTime<Utc>, DateTime<Utc>)) -> (Vec<Occurrence>, usize) {
    let mut out = Vec::new();
    let mut failed = 0;
    // Overrides by (uid, original start); cancelled ones still suppress their instance.
    let overridden: HashSet<(&str, DateTime<Utc>)> =
        components.iter().filter_map(|c| c.recurrence_id.map(|r| (c.uid.as_str(), r))).collect();
    let in_window = |o: &Occurrence| o.end >= window.0 && o.start <= window.1;

    for c in components {
        if c.cancelled() {
            continue;
        }
        if c.recurrence_id.is_some() {
            let o = occurrence(c, c.start.instant(), if let Moment::Date(d) = c.start { Some(d) } else { None });
            if in_window(&o) {
                out.push(o);
            }
            continue;
        }
        let excluded: HashSet<DateTime<Utc>> = c.exdates.iter().map(Moment::instant).collect();
        let excluded_dates: HashSet<NaiveDate> = c.exdates.iter().filter_map(|m| if let Moment::Date(d) = m { Some(*d) } else { None }).collect();

        let mut starts: Vec<DateTime<Utc>> = Vec::new();
        match &c.rrule {
            Some(rule) => {
                let spec = format!("{}\nRRULE:{}", rrule_dtstart(&c.start), normalise_rrule(rule, &c.start));
                match spec.parse::<rrule::RRuleSet>() {
                    Ok(set) => {
                        // Widen the lower bound by the event's length so ones already under way count.
                        let lo = (window.0 - c.duration - Duration::days(1)).with_timezone(&rrule::Tz::UTC);
                        let hi = window.1.with_timezone(&rrule::Tz::UTC);
                        starts.extend(set.after(lo).before(hi).all(10_000).dates.iter().map(|t| t.with_timezone(&Utc)));
                    }
                    Err(_) => {
                        failed += 1;
                        starts.push(c.start.instant());
                    }
                }
            }
            None => starts.push(c.start.instant()),
        }
        starts.extend(c.rdates.iter().map(Moment::instant));

        for s in starts {
            let date = if c.start.is_date() { Some(s.with_timezone(&chrono::Local).date_naive()) } else { None };
            if excluded.contains(&s) || date.is_some_and(|d| excluded_dates.contains(&d)) || overridden.contains(&(c.uid.as_str(), s)) {
                continue;
            }
            let o = occurrence(c, s, date);
            if in_window(&o) {
                out.push(o);
            }
        }
    }
    (out, failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
BEGIN:VEVENT\r\nUID:standup\r\nSUMMARY:Standup\r\nDTSTART;TZID=Europe/London:20260105T093000\r\n\
DTEND;TZID=Europe/London:20260105T094500\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE;COUNT=6\r\n\
EXDATE;TZID=Europe/London:20260107T093000\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:standup\r\nRECURRENCE-ID;TZID=Europe/London:20260112T093000\r\n\
SUMMARY:Standup (moved)\r\nDTSTART;TZID=Europe/London:20260112T110000\r\nDTEND;TZID=Europe/London:20260112T111500\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:standup\r\nRECURRENCE-ID;TZID=Europe/London:20260114T093000\r\nSTATUS:CANCELLED\r\n\
DTSTART;TZID=Europe/London:20260114T093000\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:holiday\r\nSUMMARY:Bank holiday\r\nDTSTART;VALUE=DATE:20260105\r\nDTEND;VALUE=DATE:20260106\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:long\r\nSUMMARY:A very long\r\n  title folded\r\nDTSTART:20260106T150000Z\r\nDURATION:PT1H30M\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";

    fn window() -> (DateTime<Utc>, DateTime<Utc>) {
        (Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(), Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap())
    }

    #[test]
    fn parses_and_unfolds() {
        let feed = parse(FEED).unwrap();
        assert_eq!(feed.total, 5);
        assert_eq!(feed.skipped, 0);
        let long = feed.components.iter().find(|c| c.uid == "long").unwrap();
        assert_eq!(long.summary, "A very long title folded");
        assert_eq!(long.duration, Duration::minutes(90));
    }

    #[test]
    fn expands_recurrence_with_exdate_override_and_cancellation() {
        let feed = parse(FEED).unwrap();
        let (occ, failed) = expand(&feed.components, window());
        assert_eq!(failed, 0);
        let standups: Vec<_> = occ.iter().filter(|o| o.uid == "standup").collect();
        // Six instances: 5th (kept), 7th (excluded), 12th (moved), 14th (cancelled), 19th, 21st.
        assert_eq!(standups.len(), 4);
        let moved = standups.iter().find(|o| o.summary == "Standup (moved)").unwrap();
        assert_eq!(moved.start, Utc.with_ymd_and_hms(2026, 1, 12, 11, 0, 0).unwrap());
        assert!(standups.iter().all(|o| o.end - o.start == Duration::minutes(15)));
    }

    #[test]
    fn all_day_events_keep_their_date() {
        let feed = parse(FEED).unwrap();
        let (occ, _) = expand(&feed.components, window());
        let h = occ.iter().find(|o| o.uid == "holiday").unwrap();
        assert!(h.all_day);
        assert_eq!(h.start.date_naive(), NaiveDate::from_ymd_opt(2026, 1, 5).unwrap());
        assert_eq!(h.end - h.start, Duration::days(1));
    }

    #[test]
    fn rejects_non_calendars_and_broken_feeds() {
        assert_eq!(parse("<html>Service unavailable</html>").unwrap_err(), FeedError::NotCalendar);
        let broken = format!("BEGIN:VCALENDAR\n{}END:VCALENDAR\n", "BEGIN:VEVENT\nUID:x\nEND:VEVENT\n".repeat(10));
        assert!(matches!(parse(&broken), Err(FeedError::TooBroken { .. })));
    }

    #[test]
    fn windows_zone_names_resolve() {
        let feed = parse("BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:w\nDTSTART;TZID=Pacific Standard Time:20260701T090000\nEND:VEVENT\nEND:VCALENDAR\n").unwrap();
        assert_eq!(feed.components[0].start.instant(), Utc.with_ymd_and_hms(2026, 7, 1, 16, 0, 0).unwrap());
    }
}

/// Recurrence lines as Google gives them (`RRULE:…`, `EXDATE;TZID=Europe/London:…`,
/// `RDATE;VALUE=DATE:…`), parsed with the same rules as a feed: the rule, then
/// excluded and added starts.
pub fn recurrence_lines(lines: &[String]) -> (Option<String>, Vec<Moment>, Vec<Moment>) {
    let zones = Zones { fixed: HashMap::new() };
    let (mut rrule, mut ex, mut add) = (None, Vec::new(), Vec::new());
    for line in lines {
        let Some((head, value)) = line.split_once(':') else { continue };
        let mut parts = head.split(';');
        let name = parts.next().unwrap_or("").trim().to_ascii_uppercase();
        let params: Vec<(String, &str)> = parts.filter_map(|p| p.split_once('=')).map(|(k, v)| (k.trim().to_ascii_uppercase(), v.trim())).collect();
        let get = |k: &str| params.iter().find(|(pk, _)| pk == k).map(|(_, v)| *v);
        let date_only = get("VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE"));
        let moments = || value.split(',').filter_map(|v| parse_moment(v, get("TZID"), date_only, &zones)).collect::<Vec<_>>();
        match name.as_str() {
            "RRULE" => rrule = Some(value.trim().to_string()),
            "EXDATE" => ex.extend(moments()),
            "RDATE" => add.extend(moments()),
            _ => {}
        }
    }
    (rrule, ex, add)
}
