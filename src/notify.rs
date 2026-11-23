//! Reminders, calmly: one heads-up per event, timed to what it needs and worded as a
//! transition ("Standup in 5 min", "9:30 – 9:45 · then free until 2 pm"). The desktop's
//! notification daemon shows them (and applies its own Do Not Disturb); calm's Do Not Disturb
//! (`Q`, `calm dnd`) holds them too.
//!
//! The notifier reads the local cache, so it works whether the window is open or not: the app
//! runs it on a thread, and `calm agent` runs it alone. Only one notifies at a time (a lock
//! file), and what's been sent is remembered, so nothing is said twice.
//!
//! Quiet by default: all-day, tentative, cancelled and free ("show as available") events,
//! free/busy calendars, and calendars with `notify=#false`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Local, NaiveTime, Timelike, Utc};

/// Something that may need a heads-up.
#[derive(Clone, Debug)]
pub struct Item {
    /// Stable across syncs: the event and its start.
    pub key: String,
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub location: Option<String>,
    /// Joined by a link rather than a trip.
    pub video: bool,
    /// How demanding (0–1), from its calendar.
    pub demand: f32,
    /// Never said: all-day, tentative, free, or a quiet calendar. Still counts as busy time.
    pub silent: bool,
}

/// How far ahead to say things, and when not to.
#[derive(Clone, Debug, PartialEq)]
pub struct Rules {
    /// Minutes before an ordinary event.
    pub before: i64,
    /// Before a video call: time to open the link.
    pub video: i64,
    /// Before an event somewhere else: time to get there.
    pub place: i64,
    /// Before a demanding one: time to get ready.
    pub demanding: i64,
    /// Nothing said between these local times (`quiet="22:00-07:00"`).
    pub quiet: Option<(NaiveTime, NaiveTime)>,
}

impl Default for Rules {
    fn default() -> Self {
        Rules { before: 5, video: 2, place: 15, demanding: 10, quiet: None }
    }
}

/// A heads-up to show now.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub key: String,
    pub title: String,
    pub body: String,
}

/// How long before `e` to say it.
pub fn lead(e: &Item, r: &Rules) -> chrono::Duration {
    let min = if e.video {
        r.video
    } else if e.location.is_some() {
        r.place
    } else if e.demand >= 0.7 {
        r.demanding
    } else {
        r.before
    };
    chrono::Duration::minutes(min)
}

fn quiet_now(now: DateTime<Local>, quiet: Option<(NaiveTime, NaiveTime)>) -> bool {
    let Some((a, b)) = quiet else { return false };
    let t = now.time();
    if a <= b { t >= a && t < b } else { t >= a || t < b }
}

/// "2 pm", "9:30 am".
pub fn clock(t: DateTime<Local>) -> String {
    let (h, m) = (t.hour(), t.minute());
    let (h12, ap) = (if h % 12 == 0 { 12 } else { h % 12 }, if h < 12 { "am" } else { "pm" });
    if m == 0 { format!("{h12} {ap}") } else { format!("{h12}:{m:02} {ap}") }
}

/// What to say about `e`: where it is, and what comes after it today.
pub fn words(e: &Item, all: &[Item], now: DateTime<Utc>) -> Note {
    let local = |t: DateTime<Utc>| t.with_timezone(&Local);
    let mins = (e.start - now).num_minutes().max(0) + i64::from((e.start - now).num_seconds() % 60 > 0);
    let title = if mins <= 1 { format!("{} now", e.title) } else { format!("{} in {mins} min", e.title) };
    let mut parts = vec![format!("{} \u{2013} {}", clock(local(e.start)), clock(local(e.end)))];
    if let Some(l) = e.location.as_ref().filter(|_| !e.video) {
        parts.push(l.lines().next().unwrap_or(l).to_string());
    }
    // What follows, the same day: soon after, or a stretch of free time.
    let day_end = local(e.end).date_naive().and_hms_opt(23, 59, 59).and_then(|t| t.and_local_timezone(Local).single()).map(|t| t.with_timezone(&Utc));
    let next = all.iter().filter(|o| o.key != e.key && o.start >= e.end && day_end.is_none_or(|d| o.start <= d) && !o.title.is_empty()).min_by_key(|o| o.start);
    parts.push(match next {
        Some(n) if n.start - e.end <= chrono::Duration::minutes(30) => format!("then {} at {}", n.title, clock(local(n.start))),
        Some(n) => format!("then free until {}", clock(local(n.start))),
        None => "then free for the rest of the day".into(),
    });
    Note { key: e.key.clone(), title, body: parts.join(" \u{b7} ") }
}

/// The heads-ups due now: each event once, from its lead time until it starts.
pub fn due(items: &[Item], now: DateTime<Utc>, r: &Rules, sent: &HashSet<String>) -> Vec<Note> {
    if quiet_now(now.with_timezone(&Local), r.quiet) {
        return Vec::new();
    }
    let mut out: Vec<(DateTime<Utc>, Note)> = items
        .iter()
        .filter(|e| !e.silent && !sent.contains(&e.key) && now < e.start && now >= e.start - lead(e, r))
        .map(|e| (e.start, words(e, items, now)))
        .collect();
    out.sort_by_key(|(t, _)| *t);
    out.into_iter().map(|(_, n)| n).collect()
}

// --- Do Not Disturb ------------------------------------------------------------------------

/// calm's Do Not Disturb: off, on until you turn it off, or on until a time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dnd {
    Off,
    On(Option<DateTime<Utc>>),
}

fn state(name: &str) -> PathBuf {
    crate::sync::db_path().with_file_name(name)
}

/// Whether Do Not Disturb is on now (one that's run out is off).
pub fn dnd(now: DateTime<Utc>) -> Dnd {
    let Ok(text) = std::fs::read_to_string(state("dnd")) else { return Dnd::Off };
    match text.trim() {
        "on" => Dnd::On(None),
        t => match t.parse::<i64>().ok().and_then(|s| DateTime::from_timestamp(s, 0)) {
            Some(until) if until > now => Dnd::On(Some(until)),
            _ => Dnd::Off,
        },
    }
}

pub fn set_dnd(d: Dnd) -> std::io::Result<()> {
    let path = state("dnd");
    match d {
        Dnd::Off => match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
        Dnd::On(until) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, until.map_or("on".to_string(), |t| t.timestamp().to_string()))
        }
    }
}

/// `30m`, `2h`, `90m`, `1h30m`, `until 14:00`, `tomorrow` (8 am): when it'd end.
pub fn parse_until(text: &str, now: DateTime<Local>) -> Option<DateTime<Utc>> {
    let text = text.trim();
    if text == "tomorrow" {
        let t = (now.date_naive() + chrono::Duration::days(1)).and_hms_opt(8, 0, 0)?;
        return t.and_local_timezone(Local).earliest().map(|t| t.with_timezone(&Utc));
    }
    if let Some(at) = text.strip_prefix("until ") {
        let t = NaiveTime::parse_from_str(at.trim(), "%H:%M").ok()?;
        let mut day = now.date_naive();
        if t <= now.time() {
            day += chrono::Duration::days(1);
        }
        return day.and_time(t).and_local_timezone(Local).earliest().map(|t| t.with_timezone(&Utc));
    }
    let (mut total, mut num) = (0i64, String::new());
    for c in text.chars() {
        match c {
            '0'..='9' => num.push(c),
            'h' | 'm' if !num.is_empty() => {
                let n: i64 = num.parse().ok()?;
                total += if c == 'h' { n * 60 } else { n };
                num.clear();
            }
            _ => return None,
        }
    }
    (num.is_empty() && total > 0).then(|| (now + chrono::Duration::minutes(total)).with_timezone(&Utc))
}

/// "Do not disturb", "… until 3 pm", "… until tomorrow 8 am".
pub fn describe_dnd(d: Dnd, now: DateTime<Utc>) -> Option<String> {
    match d {
        Dnd::Off => None,
        Dnd::On(None) => Some("Do not disturb".into()),
        Dnd::On(Some(t)) => {
            let (t, today) = (t.with_timezone(&Local), now.with_timezone(&Local).date_naive());
            let day = if t.date_naive() == today { String::new() } else { format!("{} ", t.format("%a")) };
            Some(format!("Do not disturb until {day}{}", clock(t)))
        }
    }
}

// --- From the cache ------------------------------------------------------------------------

/// Upcoming events from the cache (the next day), with what the config says about them.
pub fn items(now: DateTime<Utc>) -> (Vec<Item>, Rules) {
    let (config, _) = crate::viewconf::load();
    let rules = config.settings.notify.clone().unwrap_or_default();
    let decls = config.settings.calendars.clone().unwrap_or_default();
    let calendars = crate::view::calendars_for(&config.settings, false);
    let names: Vec<String> = calendars.iter().map(|c| c.name.clone()).collect();
    let (sources, _) = crate::sync::load_sources(config.settings.sources.as_deref(), &names);
    let Ok(store) = crate::sync::store::Store::open(&crate::sync::db_path()) else { return (Vec::new(), rules) };
    // Each stored key's calendar, and whether you only see free/busy there.
    let mut keys: std::collections::HashMap<String, (usize, bool)> = std::collections::HashMap::new();
    for s in &sources {
        keys.insert(s.name.clone(), (s.calendar, false));
        if matches!(s.location, crate::sync::Location::Google(_)) {
            for c in store.calendars(&s.name).unwrap_or_default() {
                let i = names.iter().position(|n| n.eq_ignore_ascii_case(&c.name)).unwrap_or(0);
                keys.insert(format!("{}/{}", s.name, c.id), (i, c.access == "freeBusyReader"));
            }
        }
    }
    let stored = store.occurrences(now - chrono::Duration::hours(1), now + chrono::Duration::hours(26)).unwrap_or_default();
    let video = |s: &str| ["meet.google.com", "zoom.us", "teams.microsoft.com", "whereby.com", "webex.com"].iter().any(|v| s.contains(v));
    let items = stored
        .into_iter()
        .filter_map(|s| {
            let (cal, busy_only) = *keys.get(&s.source)?;
            let o = s.occ;
            let decl = calendars.get(cal).and_then(|c| decls.iter().find(|d| d.name.eq_ignore_ascii_case(&c.name)));
            let quiet_calendar = decl.is_some_and(|d| d.quiet);
            let status = o.status.as_deref().unwrap_or("");
            let silent = o.all_day || busy_only || quiet_calendar || status == "TENTATIVE" || status == "CANCELLED" || o.transp.as_deref() == Some("TRANSPARENT");
            let linked = o.location.as_deref().is_some_and(video) || o.description.as_deref().is_some_and(video);
            Some(Item {
                key: format!("{}@{}", o.uid, o.start.timestamp()),
                title: o.summary,
                start: o.start,
                end: o.end,
                location: o.location.filter(|l| !video(l) && !l.trim().is_empty()),
                video: linked,
                demand: calendars.get(cal).map_or(0.4, |c| c.demand),
                silent,
            })
        })
        .collect();
    (items, rules)
}

/// The next event that isn't over, for status bars (`calm next`).
pub fn next(items: &[Item], now: DateTime<Utc>) -> Option<&Item> {
    items.iter().filter(|e| e.end > now && !e.silent).min_by_key(|e| e.start)
}

// --- Sending -------------------------------------------------------------------------------

/// Shows a notification through the desktop's daemon: `notify-send` if there is one, else
/// D-Bus by `busctl`.
fn show(n: &Note) -> bool {
    let quiet = std::process::Stdio::null;
    let sent = std::process::Command::new("notify-send")
        .args(["-a", "calm", "-i", "x-office-calendar", "-u", "normal", &n.title, &n.body])
        .stdout(quiet())
        .stderr(quiet())
        .status()
        .is_ok_and(|s| s.success());
    sent || std::process::Command::new("busctl")
        .args([
            "--user",
            // The timeout (-1) would read as an option.
            "--",
            "call",
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "Notify",
            "susssasa{sv}i",
            "calm",
            "0",
            "x-office-calendar",
            &n.title,
            &n.body,
            "0",
            "1",
            "urgency",
            "y",
            "1",
            "-1",
        ])
        .stdout(quiet())
        .stderr(quiet())
        .status()
        .is_ok_and(|s| s.success())
}

/// What's been said (key, when), the last two days of it.
fn sent_log() -> HashSet<String> {
    std::fs::read_to_string(state("notified")).unwrap_or_default().lines().filter_map(|l| l.split('\t').next()).map(str::to_string).collect()
}

fn remember(keys: &[String], now: DateTime<Utc>) {
    let path = state("notified");
    let cutoff = now.timestamp() - 2 * 86400;
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.split('\t').nth(1).and_then(|t| t.parse::<i64>().ok()).is_some_and(|t| t > cutoff))
        .map(str::to_string)
        .collect();
    lines.extend(keys.iter().map(|k| format!("{k}\t{}", now.timestamp())));
    let _ = std::fs::write(path, lines.join("\n") + "\n");
}

/// One look: say what's due, unless Do Not Disturb is on (what comes due meanwhile is let
/// go once it starts, not saved up).
pub fn tick(now: DateTime<Utc>) -> Vec<Note> {
    if dnd(now) != Dnd::Off {
        return Vec::new();
    }
    let (items, rules) = items(now);
    let notes = due(&items, now, &rules, &sent_log());
    let shown: Vec<String> = notes.iter().filter(|n| show(n)).map(|n| n.key.clone()).collect();
    if !shown.is_empty() {
        remember(&shown, now);
    }
    notes
}

/// Notifies while this process holds the lock, checking every 20 seconds; otherwise waits
/// for whoever holds it (the app or another agent) to stop.
pub fn run() -> ! {
    let path = state("notify.lock");
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut held: Option<std::fs::File> = None;
    loop {
        if held.is_none() {
            held = std::fs::File::create(&path).ok().filter(|f| f.try_lock().is_ok());
        }
        if held.is_some() {
            tick(Utc::now());
        }
        std::thread::sleep(StdDuration::from_secs(20));
    }
}

/// The app's notifier: `run` on its own thread.
pub fn spawn() {
    std::thread::Builder::new().name("notify".into()).spawn(|| run()).ok();
}

/// `calm next [--json]`, `calm dnd [on|off|toggle|30m|until 14:00|tomorrow] [--json]`,
/// `calm agent`.
pub fn cli(args: &[String]) -> i32 {
    let now = Utc::now();
    let json = args.iter().any(|a| a == "--json");
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let dnd_json = |d: Dnd| match d {
        Dnd::Off => "false".to_string(),
        Dnd::On(None) => "true".to_string(),
        Dnd::On(Some(t)) => format!("\"{}\"", t.to_rfc3339()),
    };
    match args.first().map(String::as_str) {
        Some("agent") => run(),
        Some("next") => {
            let (items, _) = items(now);
            let d = dnd(now);
            let next = next(&items, now);
            // `--notify`: say it now, to check notifications reach the desktop.
            if args.iter().any(|a| a == "--notify") {
                let note = next.map(|e| words(e, &items, now)).unwrap_or(Note { key: String::new(), title: "calm".into(), body: "Nothing coming up".into() });
                if !show(&note) {
                    eprintln!("calm: couldn't reach a notification daemon (tried notify-send and busctl)");
                    return 1;
                }
            }
            if json {
                match next {
                    Some(e) => println!(
                        r#"{{"title":"{}","start":"{}","end":"{}","minutes":{},"now":{},"dnd":{}}}"#,
                        esc(&e.title),
                        e.start.to_rfc3339(),
                        e.end.to_rfc3339(),
                        (e.start - now).num_minutes().max(0),
                        e.start <= now,
                        dnd_json(d)
                    ),
                    None => println!(r#"{{"title":null,"dnd":{}}}"#, dnd_json(d)),
                }
            } else {
                match next {
                    Some(e) if e.start <= now => println!("{} \u{b7} until {}", e.title, clock(e.end.with_timezone(&Local))),
                    Some(e) => println!("{} \u{b7} {}m", e.title, (e.start - now).num_minutes()),
                    None => println!("Nothing coming up"),
                }
            }
            0
        }
        Some("dnd") => {
            let arg = args[1..].iter().filter(|a| *a != "--json").cloned().collect::<Vec<_>>().join(" ");
            let current = dnd(now);
            let want = match arg.as_str() {
                "" if json => current,
                "" | "toggle" => if current == Dnd::Off { Dnd::On(None) } else { Dnd::Off },
                "on" => Dnd::On(None),
                "off" => Dnd::Off,
                t => match parse_until(t, now.with_timezone(&Local)) {
                    Some(until) => Dnd::On(Some(until)),
                    None => {
                        eprintln!("usage: calm dnd [on|off|toggle|30m|2h|until 14:00|tomorrow] [--json]");
                        return 2;
                    }
                },
            };
            if want != current
                && let Err(e) = set_dnd(want)
            {
                eprintln!("calm: {e}");
                return 1;
            }
            if json {
                println!(r#"{{"dnd":{}}}"#, dnd_json(want));
            } else {
                println!("{}", describe_dnd(want, now).unwrap_or_else(|| "Notifications on".into()));
            }
            0
        }
        _ => {
            eprintln!("usage: calm next [--json] [--notify] | calm dnd [on|off|toggle|30m|until 14:00|tomorrow] [--json] | calm agent");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Local::now().date_naive().and_hms_opt(h, m, 0).unwrap().and_local_timezone(Local).unwrap().with_timezone(&Utc)
    }

    fn item(key: &str, s: (u32, u32), e: (u32, u32)) -> Item {
        Item { key: key.into(), title: key.into(), start: at(s.0, s.1), end: at(e.0, e.1), location: None, video: false, demand: 0.4, silent: false }
    }

    #[test]
    fn heads_up_once_at_the_right_time() {
        let standup = item("Standup", (9, 30), (9, 45));
        let lunch = Item { location: Some("Cafe".into()), ..item("Lunch", (12, 30), (13, 15)) };
        let call = Item { video: true, ..item("Call", (14, 0), (14, 30)) };
        let all = vec![standup.clone(), lunch.clone(), call.clone(), Item { silent: true, ..item("Holiday", (9, 0), (17, 0)) }];
        let r = Rules::default();
        let none = HashSet::new();
        assert!(due(&all, at(9, 20), &r, &none).is_empty());
        let n = due(&all, at(9, 26), &r, &none);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].title, "Standup in 4 min");
        assert!(n[0].body.ends_with("then free until 12:30 pm"), "{}", n[0].body);
        // Once only, and not after it's started.
        assert!(due(&all, at(9, 27), &r, &HashSet::from(["Standup".to_string()])).is_empty());
        assert!(due(&all, at(9, 31), &r, &none).is_empty());
        // Somewhere else: 15 minutes ahead, and the place said.
        let n = due(&all, at(12, 16), &r, &none);
        assert!(n[0].body.contains("Cafe") && n[0].body.contains("then Call at 2 pm") == false, "{}", n[0].body);
        // A video call: two minutes.
        assert!(due(&all, at(13, 57), &r, &none).is_empty());
        assert_eq!(due(&all, at(13, 58), &r, &none)[0].title, "Call in 2 min");
        // Quiet hours.
        let quiet = Rules { quiet: Some((NaiveTime::from_hms_opt(9, 0, 0).unwrap(), NaiveTime::from_hms_opt(10, 0, 0).unwrap())), ..Rules::default() };
        assert!(due(&all, at(9, 26), &quiet, &none).is_empty());
    }

    #[test]
    fn then_soon_after() {
        let a = item("Planning", (10, 0), (11, 0));
        let b = item("Review", (11, 15), (12, 0));
        assert!(words(&a, &[a.clone(), b], at(9, 55)).body.ends_with("then Review at 11:15 am"));
    }

    #[test]
    fn dnd_times() {
        let now = Local::now().date_naive().and_hms_opt(13, 10, 0).unwrap().and_local_timezone(Local).unwrap();
        let mins = |s| parse_until(s, now).map(|t| (t - now.with_timezone(&Utc)).num_minutes());
        assert_eq!(mins("30m"), Some(30));
        assert_eq!(mins("1h30m"), Some(90));
        assert_eq!(mins("2h"), Some(120));
        assert_eq!(mins("until 14:00"), Some(50));
        assert_eq!(mins("until 9:00"), Some(19 * 60 + 50));
        assert!(mins("soon").is_none());
        assert!(parse_until("tomorrow", now).is_some());
    }
}
