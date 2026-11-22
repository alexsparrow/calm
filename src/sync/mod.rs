//! Read-only sync: fetch → parse → validate → expand → commit, per source,
//! on background threads. The UI only ever reads the store. See
//! `docs/storage-sync.md` for the design.

mod config;
pub mod google;
pub mod ics;
pub mod store;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use chrono::{DateTime, Utc};

pub use config::{Location, Source, config_dir, load as load_sources, parse_duration, parse_refresh, wanted_calendars};
use store::{Run, SourceState, Store};

/// FNV-1a: stable across builds, unlike std's hasher, so it can be stored.
pub fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

pub fn db_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("calm/calendar.sqlite3")
}

/// Recurrences are expanded this far either side of now.
pub fn window(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    (now - chrono::Duration::days(400), now + chrono::Duration::days(400))
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Applied; `skipped` malformed events were left out.
    Ok { events: usize, skipped: usize },
    Unchanged,
    /// Fetched fine but held back: the feed shrank suspiciously.
    Rejected(String),
    Error(String),
}

impl Outcome {
    fn label(&self) -> &'static str {
        match self {
            Outcome::Ok { .. } => "ok",
            Outcome::Unchanged => "unchanged",
            Outcome::Rejected(_) => "rejected",
            Outcome::Error(_) => "error",
        }
    }
}

struct Fetched {
    status: Option<u16>,
    body: Option<String>,
    etag: Option<String>,
    last_modified: Option<String>,
}

/// Fetches a source, conditionally where the server allows. Error messages never contain the URL.
fn fetch(source: &Source, state: &SourceState) -> Result<Fetched, String> {
    match &source.location {
        Location::Google(_) => Err("Google accounts sync through the API".into()),
        Location::File(p) => std::fs::read_to_string(p)
            .map(|body| Fetched { status: None, body: Some(body), etag: None, last_modified: None })
            .map_err(|e| format!("can't read file: {}", e.kind())),
        Location::Url(url) => {
            let url = url.strip_prefix("webcal://").map_or_else(|| url.clone(), |rest| format!("https://{rest}"));
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_connect(Some(Duration::from_secs(15)))
                .timeout_global(Some(Duration::from_secs(60)))
                .http_status_as_error(false)
                .build()
                .into();
            let mut req = agent.get(&url).header("User-Agent", "calm/0.1 (read-only calendar viewer)");
            if let Some(e) = &state.etag {
                req = req.header("If-None-Match", e);
            }
            if let Some(m) = &state.last_modified {
                req = req.header("If-Modified-Since", m);
            }
            let mut resp = req.call().map_err(|e| e.to_string().replace(&url, "<url>"))?;
            let status = resp.status().as_u16();
            let header = |name: &str| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
            let (etag, last_modified) = (header("etag"), header("last-modified"));
            let body = match status {
                304 => None,
                200..=299 => Some(
                    resp.body_mut()
                        .with_config()
                        .limit(64 << 20)
                        .read_to_string()
                        .map_err(|e| e.to_string().replace(&url, "<url>"))?,
                ),
                s => return Err(format!("HTTP {s}")),
            };
            Ok(Fetched { status: Some(status), body, etag, last_modified })
        }
    }
}

/// Retry delay after `failures` consecutive failures: 1, 2, 4… minutes, capped at an hour, jittered.
fn backoff(failures: i64, name: &str) -> i64 {
    let base = (60i64 << (failures - 1).clamp(0, 6)).min(3600);
    base + (hash(&format!("{name}{failures}")) % 30) as i64
}

/// One sync of one source against the store.
pub fn sync_source(store: &mut Store, source: &Source, now: DateTime<Utc>) -> Outcome {
    let mut state = store.state(&source.name).unwrap_or_default();
    state.name = source.name.clone();
    if state.config_hash.as_deref() != Some(&source.config_hash) {
        // A redefined source starts over: no conditional fetch, no suspect memory.
        state = SourceState { name: source.name.clone(), config_hash: Some(source.config_hash.clone()), ..Default::default() };
    }
    let started = now.timestamp();
    state.last_attempt_at = Some(started);
    let refresh = source.refresh.as_secs() as i64;

    let (outcome, status, bytes, events) = if let Location::Google(account) = &source.location {
        let outcome = google::sync_account(store, &source.name, account, now);
        let events = if let Outcome::Ok { events, .. } = &outcome { Some(*events) } else { None };
        (outcome, None, None, events)
    } else {
        (|| {
        let fetched = match fetch(source, &state) {
            Ok(f) => f,
            Err(e) => return (Outcome::Error(e), None, None, None),
        };
        let Some(body) = fetched.body else { return (Outcome::Unchanged, fetched.status, None, None) };
        let bytes = Some(body.len());
        let content = format!("{:016x}", hash(&body));
        if state.content_hash.as_deref() == Some(&content) {
            return (Outcome::Unchanged, fetched.status, bytes, None);
        }
        let feed = match ics::parse(&body) {
            Ok(f) => f,
            Err(e) => return (Outcome::Error(e.to_string()), fetched.status, bytes, None),
        };
        let count = feed.components.len();
        // Two-strike rule: a feed that suddenly loses most of its events is held once.
        let shrank = state.event_count >= 20 && (count as i64) * 2 < state.event_count;
        if shrank && state.suspect_hash.as_deref() != Some(&content) {
            state.suspect_hash = Some(content);
            let msg = format!("feed shrank from {} to {count} events; keeping the cached copy unless it repeats", state.event_count);
            return (Outcome::Rejected(msg), fetched.status, bytes, Some(count));
        }
        let (occurrences, _bad_rules) = ics::expand(&feed.components, window(now));
        state.content_hash = Some(content);
        state.suspect_hash = None;
        state.etag = fetched.etag;
        state.last_modified = fetched.last_modified;
        state.event_count = count as i64;
        state.last_ok_at = Some(started);
        state.last_error = None;
        state.failures = 0;
        state.next_attempt_at = Some(started + refresh);
        if let Err(e) = store.replace(&state, &feed.components, &occurrences) {
            return (Outcome::Error(format!("cache write failed: {e}")), fetched.status, bytes, Some(count));
        }
        (Outcome::Ok { events: count, skipped: feed.skipped }, fetched.status, bytes, Some(count))
    })()
    };

    let google = matches!(source.location, Location::Google(_));
    match &outcome {
        // A Google account keeps its own per-calendar state; this is the account's.
        Outcome::Ok { events, .. } if google => {
            state.last_ok_at = Some(started);
            state.last_error = None;
            state.failures = 0;
            state.event_count = *events as i64;
            state.next_attempt_at = Some(started + refresh);
        }
        Outcome::Ok { .. } => {}
        Outcome::Unchanged => {
            state.last_ok_at = Some(started);
            state.last_error = None;
            state.failures = 0;
            state.next_attempt_at = Some(started + refresh);
        }
        Outcome::Rejected(msg) => {
            state.last_error = Some(msg.clone());
            state.next_attempt_at = Some(started + refresh.min(300));
        }
        Outcome::Error(msg) => {
            state.failures += 1;
            state.last_error = Some(msg.clone());
            state.next_attempt_at = Some(started + backoff(state.failures, &source.name));
        }
    }
    if google || !matches!(outcome, Outcome::Ok { .. }) {
        let _ = store.save_state(&state);
    }
    let message = match &outcome {
        Outcome::Rejected(m) | Outcome::Error(m) => Some(m.clone()),
        Outcome::Ok { skipped, .. } if *skipped > 0 => Some(format!("{skipped} malformed events skipped")),
        _ => None,
    };
    let _ = store.record_run(&Run {
        source: source.name.clone(),
        started_at: started,
        finished_at: Utc::now().timestamp(),
        outcome: outcome.label(),
        http_status: status,
        bytes,
        events,
        message,
    });
    outcome
}

/// What the engine tells the UI.
pub enum Update {
    /// A source's data changed: reload.
    Changed,
    /// Only bookkeeping changed (a check, an error): refresh the status display.
    Status,
}

pub enum Command {
    /// Sync every source now, ignoring schedules.
    Refresh,
}

/// Starts the background scheduler. Each due source syncs on its own thread
/// with its own database connection, so a hanging feed delays nothing else.
pub fn start(sources: Vec<Source>) -> (Receiver<Update>, Sender<Command>) {
    let (tx, rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
    std::thread::Builder::new()
        .name("calm-sync".into())
        .spawn(move || {
            let path = db_path();
            if let Ok(mut store) = Store::open(&path) {
                let names: Vec<String> = sources.iter().map(|s| s.name.clone()).collect();
                let _ = store.retain_sources(&names);
            }
            let mut force = true;
            loop {
                let now = Utc::now();
                let due: Vec<&Source> = sources
                    .iter()
                    .filter(|s| {
                        force
                            || Store::open(&path)
                                .and_then(|st| st.state(&s.name))
                                .map(|st| st.config_hash.as_deref() != Some(&s.config_hash) || st.next_attempt_at.is_none_or(|t| t <= now.timestamp()))
                                .unwrap_or(true)
                    })
                    .collect();
                force = false;
                std::thread::scope(|scope| {
                    for s in due {
                        let (tx, path) = (tx.clone(), path.clone());
                        scope.spawn(move || {
                            let Ok(mut store) = Store::open(&path) else { return };
                            let outcome = sync_source(&mut store, s, Utc::now());
                            let _ = tx.send(if matches!(outcome, Outcome::Ok { .. }) { Update::Changed } else { Update::Status });
                        });
                    }
                });
                // Sleep until a command arrives or it's worth checking schedules again.
                match cmd_rx.recv_timeout(Duration::from_secs(30)) {
                    Ok(Command::Refresh) => force = true,
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .expect("spawn sync thread");
    (rx, cmd_tx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn feed(n: usize) -> String {
        let mut s = String::from("BEGIN:VCALENDAR\nVERSION:2.0\n");
        for i in 0..n {
            s += &format!("BEGIN:VEVENT\nUID:e{i}\nSUMMARY:Event {i}\nDTSTART:20260310T{:02}0000Z\nDURATION:PT30M\nEND:VEVENT\n", 8 + i % 10);
        }
        s + "END:VCALENDAR\n"
    }

    fn source(path: &std::path::Path) -> Source {
        Source {
            name: "test".into(),
            location: Location::File(path.to_path_buf()),
            calendar: 0,
            refresh: Duration::from_secs(900),
            config_hash: "t".into(),
        }
    }

    fn write(path: &std::path::Path, body: &str) {
        std::fs::File::create(path).unwrap().write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn syncs_validates_and_never_loses_good_data() {
        let dir = std::env::temp_dir().join(format!("calm-sync-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("feed.ics");
        let mut store = Store::memory().unwrap();
        let src = source(&file);
        let now = Utc::now();
        let count = |store: &Store| store.occurrences(now - chrono::Duration::days(500), now + chrono::Duration::days(500)).unwrap().len();

        write(&file, &feed(30));
        assert_eq!(sync_source(&mut store, &src, now), Outcome::Ok { events: 30, skipped: 0 });
        assert_eq!(count(&store), 30);

        // Same content: nothing rewritten.
        assert_eq!(sync_source(&mut store, &src, now), Outcome::Unchanged);

        // An HTML error page with 200 OK: error, cache untouched.
        write(&file, "<html>oops</html>");
        assert!(matches!(sync_source(&mut store, &src, now), Outcome::Error(_)));
        assert_eq!(count(&store), 30);
        assert_eq!(store.state("test").unwrap().failures, 1);

        // Suddenly a third of the events: held once, accepted when it repeats.
        write(&file, &feed(10));
        assert!(matches!(sync_source(&mut store, &src, now), Outcome::Rejected(_)));
        assert_eq!(count(&store), 30);
        assert_eq!(sync_source(&mut store, &src, now), Outcome::Ok { events: 10, skipped: 0 });
        assert_eq!(count(&store), 10);
        std::fs::remove_dir_all(&dir).ok();
    }
}
