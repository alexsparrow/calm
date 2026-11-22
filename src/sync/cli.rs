//! `calm event …`, `calm outbox`, `calm commit`, `calm discard`: changes from the command line.
//!
//! Everything here goes through the outbox (see `docs/editing.md`). Changes made here are
//! always staged: they wait for you to commit them in the app, or with `calm commit`, which
//! asks for confirmation at a terminal. That's a guard rail for agents, not a security boundary.

use std::collections::HashMap;
use std::io::{IsTerminal, Write};

use chrono::{DateTime, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use serde_json::{Value, json};

use super::google;
use super::outbox::{Kind, Op, Origin, State, new_id};
use super::store::{Store, Stored};

const USAGE: &str = "usage:
  calm event list [--day YYYY-MM-DD|today|tomorrow] [--days N] [--json]
  calm event add TITLE --start WHEN [--end WHEN | --duration 30m] [--all-day [--days N]]
                       [--calendar NAME] [--location TEXT] [--notes TEXT]
  calm event edit ID [--title T] [--start WHEN] [--end WHEN] [--location TEXT] [--notes TEXT] [--all]
  calm event move ID --start WHEN [--all]
  calm event delete ID [--all]
  calm outbox [--json]
  calm commit [--all | SEQ…] [--yes]
  calm discard [--all | SEQ…]

WHEN is \"YYYY-MM-DD HH:MM\", \"today 14:00\", \"tomorrow 9:30\", or just \"HH:MM\" (for --end:
the start's day). Changes from here are staged until committed. --all on a recurring
event changes the whole series (default: just that occurrence).";

/// Runs a subcommand; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    let result = match args.first().map(String::as_str) {
        Some("event") => event(&args[1..]),
        Some("outbox") => outbox(args.iter().any(|a| a == "--json")),
        Some("commit") => commit(&args[1..]),
        Some("discard") => discard(&args[1..]),
        _ => Err(USAGE.into()),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// `--name value` options and the rest, in order.
fn options(args: &[String]) -> (HashMap<String, String>, Vec<String>, Vec<String>) {
    let (mut opts, mut flags, mut rest) = (HashMap::new(), Vec::new(), Vec::new());
    let mut i = 0;
    while i < args.len() {
        match args[i].strip_prefix("--") {
            Some(k @ ("all" | "all-day" | "json" | "yes")) => flags.push(k.to_string()),
            Some(k) => {
                opts.insert(k.to_string(), args.get(i + 1).cloned().unwrap_or_default());
                i += 1;
            }
            None => rest.push(args[i].clone()),
        }
        i += 1;
    }
    (opts, flags, rest)
}

fn store() -> Result<Store, String> {
    Store::open(&super::db_path()).map_err(|e| format!("can't open the cache: {e}"))
}

/// Google sources from calm.kdl: (source name, account).
fn google_sources() -> Vec<(String, String)> {
    let (config, _) = crate::viewconf::load();
    config.settings.sources.unwrap_or_default().into_iter().filter_map(|s| Some((s.name, s.google?))).collect()
}

fn zone() -> Option<chrono_tz::Tz> {
    crate::zones::Zones::load(None, None).primary
}

// --- Times -------------------------------------------------------------------------------------

fn day_word(s: &str) -> Option<NaiveDate> {
    let today = Local::now().date_naive();
    match s {
        "today" => Some(today),
        "tomorrow" => Some(today + Duration::days(1)),
        "yesterday" => Some(today - Duration::days(1)),
        d => NaiveDate::parse_from_str(d, "%Y-%m-%d").ok(),
    }
}

/// "2026-11-24 09:00", "tomorrow 9:30", "14:00" (on `day`), "2026-11-24" (a date).
fn when(s: &str, day: Option<NaiveDate>) -> Result<(NaiveDate, Option<NaiveTime>), String> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    let time = |t: &str| NaiveTime::parse_from_str(t, "%H:%M").map_err(|_| format!("can't read the time `{t}` (want HH:MM)"));
    match parts.as_slice() {
        [d, t] => Ok((day_word(d).ok_or(format!("can't read the date `{d}`"))?, Some(time(t)?))),
        [x] if x.contains(':') => Ok((day.unwrap_or_else(|| Local::now().date_naive()), Some(time(x)?))),
        [d] => Ok((day_word(d).ok_or(format!("can't read `{d}`"))?, None)),
        _ => Err(format!("can't read `{s}`")),
    }
}

/// Google's `{dateTime, timeZone}` for a local wall-clock time.
fn google_time(date: NaiveDate, time: NaiveTime) -> Value {
    let local = Local.from_local_datetime(&date.and_time(time)).earliest().unwrap_or_else(Local::now);
    let mut v = json!({ "dateTime": local.to_rfc3339() });
    if let Some(tz) = zone() {
        v["timeZone"] = json!(tz.name());
    }
    v
}

fn google_date(date: NaiveDate) -> Value {
    json!({ "date": date.format("%Y-%m-%d").to_string() })
}

fn duration(s: &str) -> Option<Duration> {
    super::parse_duration(s).map(|d| Duration::seconds(d.as_secs() as i64))
}

// --- Finding events ------------------------------------------------------------------------------

/// What you see now (the mirror with queued changes), every Google calendar, around `from`.
fn view(store: &Store, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Stored> {
    let mut stored = store.occurrences(from, to).unwrap_or_default();
    google::overlay(store, &mut stored, Utc::now(), true);
    stored.retain(|s| s.occ.end >= from && s.occ.start <= to);
    stored.sort_by_key(|s| s.occ.start);
    stored
}

/// An event's handle: `<source>/<calendar>/<id>` (the last part an instance ID for one
/// occurrence of a series).
fn handle(s: &Stored) -> String {
    format!("{}/{}", s.source, if s.occ.ref_id.is_empty() { &s.occ.uid } else { &s.occ.ref_id })
}

fn split_handle(h: &str) -> Result<(String, String, String), String> {
    let mut parts = h.rsplitn(3, '/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(e), Some(c), Some(s)) => Ok((s.to_string(), c.to_string(), e.to_string())),
        _ => Err(format!("`{h}` isn't an event ID (see `calm event list`)")),
    }
}

/// The occurrence a handle names, as you see it now.
fn find(store: &Store, h: &str) -> Result<Stored, String> {
    let now = Utc::now();
    let (from, to) = super::window(now);
    view(store, from, to)
        .into_iter()
        .find(|s| handle(s) == h)
        .ok_or_else(|| format!("no event `{h}` (see `calm event list`)"))
}

/// Google's version of an occurrence's editable fields.
fn fields(s: &Stored) -> Value {
    let o = &s.occ;
    let (start, end) = if o.all_day {
        (google_date(o.start.date_naive()), google_date(o.end.date_naive()))
    } else {
        let t = |t: DateTime<Utc>| {
            let mut v = json!({ "dateTime": t.with_timezone(&Local).to_rfc3339() });
            if let Some(tz) = zone() {
                v["timeZone"] = json!(tz.name());
            }
            v
        };
        (t(o.start), t(o.end))
    };
    json!({
        "summary": o.summary, "location": o.location.clone().unwrap_or_default(),
        "description": o.description.clone().unwrap_or_default(), "start": start, "end": end,
    })
}

/// For one occurrence of a series: its series and original start, from its instance ID.
fn instance(s: &Stored) -> (Option<String>, Option<Value>) {
    if s.occ.ref_id.is_empty() || s.occ.ref_id == s.occ.uid {
        return (None, None);
    }
    let stamp = s.occ.ref_id.rsplit('_').next().unwrap_or("");
    let original = NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%SZ")
        .map(|t| json!({ "dateTime": t.and_utc().to_rfc3339() }))
        .or_else(|_| NaiveDate::parse_from_str(stamp, "%Y%m%d").map(google_date))
        .ok();
    (Some(s.occ.uid.clone()), original)
}

fn writable(store: &Store, source: &str, calendar: &str) -> Result<(), String> {
    let cal = store.calendars(source).unwrap_or_default().into_iter().find(|c| c.id == calendar);
    match cal {
        Some(c) if c.access == "owner" || c.access == "writer" => Ok(()),
        Some(c) => Err(format!("{} is read-only for you ({})", c.name, c.access)),
        None if google_sources().iter().any(|(n, _)| n == source) => Err(format!("unknown calendar `{calendar}`")),
        None => Err(format!("`{source}` isn't a Google account; only Google calendars can be edited")),
    }
}

// --- Commands --------------------------------------------------------------------------------------

fn event(args: &[String]) -> Result<(), String> {
    let (opts, flags, rest) = options(args.get(1..).unwrap_or(&[]));
    match args.first().map(String::as_str) {
        Some("list") => list(&opts, flags.iter().any(|f| f == "json")),
        Some("add") => add(&rest, &opts, flags.iter().any(|f| f == "all-day")),
        Some("edit" | "move") => edit(&rest, &opts, flags.iter().any(|f| f == "all")),
        Some("delete") => delete(&rest, flags.iter().any(|f| f == "all")),
        _ => Err(USAGE.into()),
    }
}

fn list(opts: &HashMap<String, String>, as_json: bool) -> Result<(), String> {
    let store = store()?;
    let day = match opts.get("day") {
        Some(d) => day_word(d).ok_or(format!("can't read the date `{d}`"))?,
        None => Local::now().date_naive(),
    };
    let days: i64 = opts.get("days").and_then(|d| d.parse().ok()).unwrap_or(1);
    let from = Local.from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap()).earliest().unwrap().with_timezone(&Utc);
    let to = from + Duration::days(days) - Duration::seconds(1);
    let names: HashMap<String, String> = google_sources()
        .iter()
        .flat_map(|(src, _)| store.calendars(src).unwrap_or_default().into_iter().map(move |c| (format!("{src}/{}", c.id), c.name)))
        .collect();
    let items = view(&store, from, to);
    if as_json {
        let out: Vec<Value> = items
            .iter()
            .map(|s| {
                json!({
                    "id": handle(s), "title": s.occ.summary, "start": s.occ.start.to_rfc3339(), "end": s.occ.end.to_rfc3339(),
                    "all_day": s.occ.all_day, "calendar": names.get(&s.source).cloned().unwrap_or_else(|| s.source.clone()),
                    "location": s.occ.location, "sync": s.sync.map(State::name),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return Ok(());
    }
    for s in &items {
        let (a, b) = (s.occ.start.with_timezone(&Local), s.occ.end.with_timezone(&Local));
        let time = if s.occ.all_day { "all day    ".to_string() } else { format!("{}–{}", a.format("%H:%M"), b.format("%H:%M")) };
        let cal = names.get(&s.source).cloned().unwrap_or_else(|| s.source.clone());
        let sync = s.sync.map(|st| format!("  [{}]", st.name())).unwrap_or_default();
        println!("{}  {time}  {}  · {cal}{sync}\n    {}", a.format("%a %d"), s.occ.summary, handle(s));
    }
    if items.is_empty() {
        println!("nothing on {}", day.format("%a %-d %b"));
    }
    Ok(())
}

fn add(rest: &[String], opts: &HashMap<String, String>, all_day: bool) -> Result<(), String> {
    let title = rest.join(" ");
    if title.trim().is_empty() {
        return Err("give the event a title: calm event add \"Focus\" --start \"tomorrow 9:00\"".into());
    }
    let mut store = store()?;
    let start = opts.get("start").ok_or("--start is needed")?;
    let (date, time) = when(start, None)?;
    // Which calendar: --calendar by name, else your primary (named after the source).
    let sources = google_sources();
    let (source, _) = sources.first().ok_or("no Google account in calm.kdl to add to")?.clone();
    let cals = store.calendars(&source).unwrap_or_default();
    let want = opts.get("calendar").cloned().unwrap_or_else(|| source.clone());
    let cal = cals
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(&want) || c.id.eq_ignore_ascii_case(&want))
        .ok_or_else(|| format!("no calendar `{want}`; one of: {}", cals.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")))?;
    writable(&store, &source, &cal.id)?;
    let (s, e) = match time.filter(|_| !all_day) {
        None => {
            let days: i64 = opts.get("days").and_then(|d| d.parse().ok()).unwrap_or(1);
            (google_date(date), google_date(date + Duration::days(days.max(1))))
        }
        Some(t) => {
            let end = match (opts.get("end"), opts.get("duration")) {
                (Some(e), _) => {
                    let (d, t) = when(e, Some(date))?;
                    d.and_time(t.ok_or("--end needs a time")?)
                }
                (None, Some(d)) => date.and_time(t) + duration(d).ok_or(format!("can't read the duration `{d}`"))?,
                (None, None) => date.and_time(t) + Duration::minutes(30),
            };
            (google_time(date, t), google_time(end.date(), end.time()))
        }
    };
    let mut patch = json!({ "summary": title, "start": s, "end": e });
    for (k, field) in [("location", "location"), ("notes", "description")] {
        if let Some(v) = opts.get(k) {
            patch[field] = json!(v);
        }
    }
    let mut op = Op::new(&source, &cal.id, &new_id(), Kind::Create, json!({}), patch, Origin::Cli);
    op.state = State::Draft;
    let seq = store.queue(op).map_err(|e| e.to_string())?;
    println!("staged #{} · create \"{title}\" · commit with `calm commit` or in calm", seq.unwrap_or_default());
    Ok(())
}

fn edit(rest: &[String], opts: &HashMap<String, String>, whole_series: bool) -> Result<(), String> {
    let h = rest.first().ok_or("which event? (an ID from `calm event list`)")?;
    let mut store = store()?;
    let s = find(&store, h)?;
    let (source, calendar, _) = split_handle(h)?;
    writable(&store, &source, &calendar)?;
    let (series, original) = instance(&s);
    let now_fields = fields(&s);
    // The title rides along for display (merging only looks at fields being changed).
    let mut base = json!({ "summary": now_fields["summary"] });
    let mut patch = json!({});
    for (k, field) in [("title", "summary"), ("location", "location"), ("notes", "description")] {
        if let Some(v) = opts.get(k) {
            base[field] = now_fields[field].clone();
            patch[field] = json!(v);
        }
    }
    if let Some(start) = opts.get("start") {
        if s.occ.all_day {
            return Err("moving all-day events comes with the app's editing; for now, edit them in Google".into());
        }
        let (d, t) = when(start, Some(s.occ.start.with_timezone(&Local).date_naive()))?;
        let t = t.ok_or("--start needs a time")?;
        let new_start = Local.from_local_datetime(&d.and_time(t)).earliest().ok_or("that time doesn't exist here")?.with_timezone(&Utc);
        let new_end = match opts.get("end") {
            Some(e) => {
                let (d, t) = when(e, Some(d))?;
                Local.from_local_datetime(&d.and_time(t.ok_or("--end needs a time")?)).earliest().ok_or("that time doesn't exist here")?.with_timezone(&Utc)
            }
            None => new_start + (s.occ.end - s.occ.start),
        };
        let at = |t: DateTime<Utc>| google_time(t.with_timezone(&Local).date_naive(), t.with_timezone(&Local).time());
        base["start"] = now_fields["start"].clone();
        base["end"] = now_fields["end"].clone();
        patch["start"] = at(new_start);
        patch["end"] = at(new_end);
    }
    if patch.as_object().is_none_or(|p| p.is_empty()) {
        return Err("nothing to change: give --title, --start, --end, --location or --notes".into());
    }
    let mut op = if whole_series && series.is_some() {
        // The whole series: shift its own times by as much as this occurrence moved.
        let master = store.raw_components(&s.source).unwrap_or_default().into_iter().filter_map(|r| serde_json::from_str::<Value>(&r).ok()).find(|v| v["id"].as_str() == Some(&s.occ.uid)).ok_or("can't find the series")?;
        if patch.get("start").is_some() {
            let shift = DateTime::parse_from_rfc3339(patch["start"]["dateTime"].as_str().unwrap_or("")).map(|t| t.with_timezone(&Utc) - s.occ.start).unwrap_or_default();
            for k in ["start", "end"] {
                let t = DateTime::parse_from_rfc3339(master[k]["dateTime"].as_str().unwrap_or("")).map_err(|_| "the series has no times to move")?;
                let mut v = master[k].clone();
                v["dateTime"] = json!((t + shift).to_rfc3339());
                base[k] = master[k].clone();
                patch[k] = v;
            }
        }
        Op::new(&source, &calendar, &s.occ.uid, Kind::Update, base, patch, Origin::Cli)
    } else {
        let mut op = Op::new(&source, &calendar, &s.occ.ref_id, Kind::Update, base, patch, Origin::Cli);
        (op.series, op.original) = (series, original);
        op
    };
    op.state = State::Draft;
    let seq = store.queue(op).map_err(|e| e.to_string())?;
    println!("staged #{} · edit \"{}\" · commit with `calm commit` or in calm", seq.unwrap_or_default(), s.occ.summary);
    Ok(())
}

fn delete(rest: &[String], whole_series: bool) -> Result<(), String> {
    let h = rest.first().ok_or("which event? (an ID from `calm event list`)")?;
    let mut store = store()?;
    let s = find(&store, h)?;
    let (source, calendar, _) = split_handle(h)?;
    writable(&store, &source, &calendar)?;
    let (series, original) = instance(&s);
    let base = json!({ "summary": s.occ.summary });
    let mut op = if whole_series && series.is_some() {
        Op::new(&source, &calendar, &s.occ.uid, Kind::Delete, base, json!({}), Origin::Cli)
    } else {
        let mut op = Op::new(&source, &calendar, &s.occ.ref_id, Kind::Delete, base, json!({}), Origin::Cli);
        (op.series, op.original) = (series, original);
        op
    };
    op.state = State::Draft;
    match store.queue(op).map_err(|e| e.to_string())? {
        Some(seq) => println!("staged #{seq} · delete \"{}\" · commit with `calm commit` or in calm", s.occ.summary),
        None => println!("removed the staged create of \"{}\" (it was never sent)", s.occ.summary),
    }
    Ok(())
}

fn outbox(as_json: bool) -> Result<(), String> {
    let ops = store()?.ops().map_err(|e| e.to_string())?;
    if as_json {
        let out: Vec<Value> = ops
            .iter()
            .map(|o| {
                json!({
                    "seq": o.seq, "state": o.state.name(), "origin": o.origin.name(), "kind": o.kind.name(),
                    "event": format!("{}/{}/{}", o.source, o.calendar, o.event), "summary": o.describe(),
                    "before": o.base, "after": o.patch, "theirs": o.theirs, "error": o.last_error,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return Ok(());
    }
    if ops.is_empty() {
        println!("nothing queued");
    }
    for o in &ops {
        let from = if o.origin == Origin::Cli { " (from the command line)" } else { "" };
        println!("#{:<4} {:<9} {}{from}", o.seq, o.state.name(), o.describe());
        if let Some(e) = &o.last_error {
            println!("       {e}");
        }
        if let Some(t) = &o.theirs {
            println!("       changed elsewhere: {t}");
        }
    }
    Ok(())
}

/// `--all`, or the listed `#seq`s.
fn selected(store: &Store, rest: &[String], all: bool, states: &[State]) -> Result<Vec<Op>, String> {
    let ops = store.ops().map_err(|e| e.to_string())?;
    if all {
        return Ok(ops.into_iter().filter(|o| states.contains(&o.state)).collect());
    }
    if rest.is_empty() {
        return Err("which changes? give their numbers (see `calm outbox`) or --all".into());
    }
    rest.iter()
        .map(|r| {
            let seq: i64 = r.trim_start_matches('#').parse().map_err(|_| format!("`{r}` isn't a change number"))?;
            ops.iter().find(|o| o.seq == seq).cloned().ok_or(format!("no change #{seq}"))
        })
        .collect()
}

fn commit(args: &[String]) -> Result<(), String> {
    let (_, flags, rest) = options(args);
    let mut store = store()?;
    let ops = selected(&store, &rest, flags.iter().any(|f| f == "all"), &[State::Draft, State::Failed, State::Conflict])?;
    if ops.is_empty() {
        println!("nothing to commit");
        return Ok(());
    }
    // The guard rail: staged changes go out only with someone's say-so.
    if !flags.iter().any(|f| f == "yes") {
        if !std::io::stdin().is_terminal() {
            return Err("not committing without a person to confirm: run this at a terminal, or pass --yes".into());
        }
        for o in &ops {
            println!("  #{} {}", o.seq, o.describe());
        }
        print!("Commit {} change{}? [y/N] ", ops.len(), if ops.len() == 1 { "" } else { "s" });
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().read_line(&mut answer);
        if !answer.trim().eq_ignore_ascii_case("y") {
            println!("nothing committed");
            return Ok(());
        }
    }
    store.commit(&ops.iter().map(|o| o.seq).collect::<Vec<_>>()).map_err(|e| e.to_string())?;
    // Send now, rather than waiting for the app.
    let now = Utc::now();
    let mut problems = Vec::new();
    let (mut sent, mut waiting) = (0, 0);
    for (source, account) in google_sources() {
        if !ops.iter().any(|o| o.source == source) {
            continue;
        }
        match google::flush_account(&mut store, &source, &account, now) {
            Ok(f) => {
                sent += f.sent;
                waiting += f.waiting;
                problems.extend(f.problems);
            }
            Err(e) => problems.push(e),
        }
    }
    println!("committed {} · sent {sent}{}", ops.len(), if waiting > 0 { format!(" · {waiting} waiting to retry") } else { String::new() });
    for p in &problems {
        eprintln!("  {p}");
    }
    if problems.is_empty() { Ok(()) } else { Err(format!("{} need attention (see `calm outbox`)", problems.len())) }
}

fn discard(args: &[String]) -> Result<(), String> {
    let (_, flags, rest) = options(args);
    let store = store()?;
    let all = flags.iter().any(|f| f == "all");
    let ops = selected(&store, &rest, all, &[State::Draft, State::Pending, State::Failed, State::Conflict])?;
    let n = store.discard(&ops.iter().map(|o| o.seq).collect::<Vec<_>>()).map_err(|e| e.to_string())?;
    println!("discarded {n}");
    Ok(())
}
