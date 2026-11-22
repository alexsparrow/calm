//! The offline cache: one SQLite database in WAL mode, so the UI can read
//! while a sync writes. Each source's data is replaced in one transaction.

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use super::ics::{Component, Occurrence};

/// Numbered migrations, applied in order; `meta.schema_version` records progress.
const MIGRATIONS: &[&str] = &[r#"
CREATE TABLE sources (
    name TEXT PRIMARY KEY,
    config_hash TEXT,
    etag TEXT,
    last_modified TEXT,
    content_hash TEXT,
    suspect_hash TEXT,
    last_ok_at INTEGER,
    last_attempt_at INTEGER,
    last_error TEXT,
    event_count INTEGER NOT NULL DEFAULT 0,
    failures INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER
);
CREATE TABLE sync_runs (
    id INTEGER PRIMARY KEY,
    source TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    finished_at INTEGER NOT NULL,
    outcome TEXT NOT NULL,
    http_status INTEGER,
    bytes INTEGER,
    events INTEGER,
    message TEXT
);
CREATE TABLE components (
    source TEXT NOT NULL,
    uid TEXT NOT NULL,
    recurrence_id INTEGER NOT NULL DEFAULT 0,
    sequence INTEGER NOT NULL,
    summary TEXT,
    rrule TEXT,
    status TEXT,
    raw TEXT NOT NULL,
    PRIMARY KEY (source, uid, recurrence_id)
);
CREATE TABLE occurrences (
    source TEXT NOT NULL,
    uid TEXT NOT NULL,
    start INTEGER NOT NULL,
    end INTEGER NOT NULL,
    all_day INTEGER NOT NULL,
    summary TEXT NOT NULL,
    location TEXT,
    description TEXT,
    status TEXT,
    transp TEXT,
    organizer TEXT,
    attendees TEXT,
    categories TEXT,
    PRIMARY KEY (source, uid, start)
);
CREATE INDEX occurrences_start ON occurrences (start);
CREATE TABLE derived (
    source TEXT NOT NULL,
    uid TEXT NOT NULL,
    start INTEGER NOT NULL,
    key TEXT NOT NULL,
    value TEXT,
    PRIMARY KEY (source, uid, start, key)
);
"#, r#"
-- Google: a sync token per calendar, and each account's calendar list.
ALTER TABLE sources ADD COLUMN sync_token TEXT;
CREATE TABLE remote_calendars (
    source TEXT NOT NULL,
    id TEXT NOT NULL,
    name TEXT NOT NULL,
    color TEXT,
    access TEXT,
    position INTEGER NOT NULL,
    PRIMARY KEY (source, id)
);
"#];

/// Per-source sync bookkeeping.
#[derive(Clone, Debug, Default)]
pub struct SourceState {
    pub name: String,
    pub config_hash: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_hash: Option<String>,
    /// A shrunken feed held back once; accepted if the same content arrives again.
    pub suspect_hash: Option<String>,
    pub last_ok_at: Option<i64>,
    pub last_attempt_at: Option<i64>,
    pub last_error: Option<String>,
    pub event_count: i64,
    pub failures: i64,
    pub next_attempt_at: Option<i64>,
    /// Google: where the next incremental sync picks up.
    pub sync_token: Option<String>,
}

/// A calendar an account offers (Google's calendar list).
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteCalendar {
    pub id: String,
    pub name: String,
    /// `#rrggbb`.
    pub color: Option<String>,
    /// `owner`, `writer`, `reader` or `freeBusyReader`.
    pub access: String,
}

#[derive(Clone, Debug)]
pub struct Run {
    pub source: String,
    pub started_at: i64,
    pub finished_at: i64,
    pub outcome: &'static str,
    pub http_status: Option<u16>,
    pub bytes: Option<usize>,
    pub events: Option<usize>,
    pub message: Option<String>,
}

/// An occurrence as the UI reads it, with its source.
#[derive(Clone, Debug)]
pub struct Stored {
    pub source: String,
    pub occ: Occurrence,
}

pub struct Store {
    conn: Connection,
}

fn list(v: &[String]) -> String {
    v.join("\u{1f}")
}

fn unlist(s: Option<String>) -> Vec<String> {
    s.filter(|s| !s.is_empty()).map(|s| s.split('\u{1f}').map(str::to_string).collect()).unwrap_or_default()
}

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)", [])?;
        let version: usize = conn
            .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get::<_, String>(0))
            .optional()?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)", [(i + 1).to_string()])?;
            tx.commit()?;
        }
        Ok(Self { conn })
    }

    pub fn state(&self, name: &str) -> rusqlite::Result<SourceState> {
        self.conn
            .query_row(
                "SELECT config_hash, etag, last_modified, content_hash, suspect_hash, last_ok_at, last_attempt_at,
                        last_error, event_count, failures, next_attempt_at, sync_token FROM sources WHERE name = ?1",
                [name],
                |r| {
                    Ok(SourceState {
                        name: name.to_string(),
                        config_hash: r.get(0)?,
                        etag: r.get(1)?,
                        last_modified: r.get(2)?,
                        content_hash: r.get(3)?,
                        suspect_hash: r.get(4)?,
                        last_ok_at: r.get(5)?,
                        last_attempt_at: r.get(6)?,
                        last_error: r.get(7)?,
                        event_count: r.get(8)?,
                        failures: r.get(9)?,
                        next_attempt_at: r.get(10)?,
                        sync_token: r.get(11)?,
                    })
                },
            )
            .optional()
            .map(|s| s.unwrap_or_else(|| SourceState { name: name.to_string(), ..Default::default() }))
    }

    pub fn states(&self) -> rusqlite::Result<Vec<SourceState>> {
        let names: Vec<String> =
            self.conn.prepare("SELECT name FROM sources ORDER BY name")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        names.iter().map(|n| self.state(n)).collect()
    }

    pub fn save_state(&self, s: &SourceState) -> rusqlite::Result<()> {
        Self::write_state(&self.conn, s)
    }

    fn write_state(conn: &Connection, s: &SourceState) -> rusqlite::Result<()> {
        conn.execute(
            "INSERT OR REPLACE INTO sources (name, config_hash, etag, last_modified, content_hash, suspect_hash,
                 last_ok_at, last_attempt_at, last_error, event_count, failures, next_attempt_at, sync_token)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                s.name,
                s.config_hash,
                s.etag,
                s.last_modified,
                s.content_hash,
                s.suspect_hash,
                s.last_ok_at,
                s.last_attempt_at,
                s.last_error,
                s.event_count,
                s.failures,
                s.next_attempt_at,
                s.sync_token
            ],
        )?;
        Ok(())
    }

    pub fn record_run(&self, run: &Run) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO sync_runs (source, started_at, finished_at, outcome, http_status, bytes, events, message)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                run.source,
                run.started_at,
                run.finished_at,
                run.outcome,
                run.http_status,
                run.bytes.map(|b| b as i64),
                run.events.map(|e| e as i64),
                run.message
            ],
        )?;
        // Keep the log bounded.
        self.conn.execute("DELETE FROM sync_runs WHERE id <= (SELECT MAX(id) - 500 FROM sync_runs)", [])?;
        Ok(())
    }

    /// Replaces everything for one source, and its state, in one transaction.
    pub fn replace(&mut self, state: &SourceState, components: &[Component], occurrences: &[Occurrence]) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM components WHERE source = ?1", [&state.name])?;
        tx.execute("DELETE FROM occurrences WHERE source = ?1", [&state.name])?;
        {
            let mut comp = tx.prepare(
                "INSERT OR REPLACE INTO components (source, uid, recurrence_id, sequence, summary, rrule, status, raw)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for c in components {
                comp.execute(params![
                    state.name,
                    c.uid,
                    c.recurrence_id.map_or(0, |r| r.timestamp()),
                    c.sequence,
                    c.summary,
                    c.rrule,
                    c.status,
                    c.raw
                ])?;
            }
            let mut occ = tx.prepare(
                "INSERT OR REPLACE INTO occurrences (source, uid, start, end, all_day, summary, location, description,
                     status, transp, organizer, attendees, categories)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            )?;
            for o in occurrences {
                occ.execute(params![
                    state.name,
                    o.uid,
                    o.start.timestamp(),
                    o.end.timestamp(),
                    o.all_day,
                    o.summary,
                    o.location,
                    o.description,
                    o.status,
                    o.transp,
                    o.organizer,
                    list(&o.attendees),
                    list(&o.categories)
                ])?;
            }
        }
        Self::write_state(&tx, state)?;
        tx.commit()
    }

    /// Drops data for sources no longer configured.
    pub fn retain_sources(&mut self, names: &[String]) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        let stale: Vec<String> = tx
            .prepare("SELECT name FROM sources")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<Vec<String>, _>>()?
            .into_iter()
            // `Account/calendar-id` keys belong to their account's source.
            .filter(|n| !names.iter().any(|k| n == k || n.starts_with(&format!("{k}/"))))
            .collect();
        for n in stale {
            for table in ["sources WHERE name", "components WHERE source", "occurrences WHERE source", "derived WHERE source", "remote_calendars WHERE source"] {
                tx.execute(&format!("DELETE FROM {table} = ?1"), [&n])?;
            }
        }
        tx.commit()
    }

    /// Replaces an account's calendar list, and drops data for calendars no longer on it.
    pub fn set_calendars(&mut self, source: &str, calendars: &[RemoteCalendar]) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM remote_calendars WHERE source = ?1", [source])?;
        for (i, c) in calendars.iter().enumerate() {
            tx.execute(
                "INSERT INTO remote_calendars (source, id, name, color, access, position) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![source, c.id, c.name, c.color, c.access, i as i64],
            )?;
        }
        let keep: Vec<String> = calendars.iter().map(|c| format!("{source}/{}", c.id)).collect();
        let subs: Vec<String> = tx
            .prepare("SELECT name FROM sources WHERE substr(name, 1, length(?1)) = ?1")?
            .query_map([format!("{source}/")], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for n in subs.into_iter().filter(|n| !keep.contains(n)) {
            for table in ["sources WHERE name", "components WHERE source", "occurrences WHERE source", "derived WHERE source"] {
                tx.execute(&format!("DELETE FROM {table} = ?1"), [&n])?;
            }
        }
        tx.commit()
    }

    /// An account's calendars, in its order.
    pub fn calendars(&self, source: &str) -> rusqlite::Result<Vec<RemoteCalendar>> {
        self.conn
            .prepare("SELECT id, name, color, access FROM remote_calendars WHERE source = ?1 ORDER BY position")?
            .query_map([source], |r| Ok(RemoteCalendar { id: r.get(0)?, name: r.get(1)?, color: r.get(2)?, access: r.get::<_, Option<String>>(3)?.unwrap_or_default() }))?
            .collect()
    }

    /// The raw text of each stored component of a source (Google keeps each event's JSON).
    pub fn raw_components(&self, source: &str) -> rusqlite::Result<Vec<String>> {
        self.conn.prepare("SELECT raw FROM components WHERE source = ?1")?.query_map([source], |r| r.get(0))?.collect()
    }

    /// Occurrences overlapping `[from, to]`, every source.
    pub fn occurrences(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> rusqlite::Result<Vec<Stored>> {
        let mut stmt = self.conn.prepare(
            "SELECT source, uid, start, end, all_day, summary, location, description, status, transp, organizer,
                    attendees, categories
             FROM occurrences WHERE end >= ?1 AND start <= ?2 ORDER BY start",
        )?;
        let ts = |s: i64| DateTime::from_timestamp(s, 0).unwrap_or_default();
        stmt.query_map(params![from.timestamp(), to.timestamp()], |r| {
            Ok(Stored {
                source: r.get(0)?,
                occ: Occurrence {
                    uid: r.get(1)?,
                    start: ts(r.get(2)?),
                    end: ts(r.get(3)?),
                    all_day: r.get(4)?,
                    summary: r.get(5)?,
                    location: r.get(6)?,
                    description: r.get(7)?,
                    status: r.get(8)?,
                    transp: r.get(9)?,
                    organizer: r.get(10)?,
                    attendees: unlist(r.get(11)?),
                    categories: unlist(r.get(12)?),
                },
            })
        })?
        .collect()
    }
}
