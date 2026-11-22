//! The outbox: changes you've asked for that the server hasn't confirmed. See
//! `docs/editing.md`.
//!
//! The mirror (`components`, `occurrences`) only ever holds what the server said; this holds
//! intent. What you see is the mirror with the outbox laid over it (`overlay`), so reads can
//! replace the mirror freely and pending changes replay against whatever is current.
//!
//! Changes are field patches in Google's event shape: `summary`, `location`, `description`,
//! and `start`/`end` (always together: times are one field for merging).

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value, json};

use super::store::Store;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Create,
    Update,
    Delete,
}

/// Where a change is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Staged for review: sent only when committed.
    Draft,
    /// Will be sent when due.
    Pending,
    /// Being sent now (claimed by one sender).
    Sending,
    /// Rejected, or can't be sent as things stand: needs you.
    Failed,
    /// Changed elsewhere in a way that clashes: needs you.
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    App,
    /// The command line: an agent or a script. Always staged.
    Cli,
}

macro_rules! names {
    ($t:ty { $($v:ident = $s:literal),* }) => {
        impl $t {
            pub fn name(self) -> &'static str {
                match self { $(Self::$v => $s),* }
            }
            pub fn parse(s: &str) -> Option<Self> {
                match s { $($s => Some(Self::$v),)* _ => None }
            }
        }
    };
}
names!(Kind { Create = "create", Update = "update", Delete = "delete" });
names!(State { Draft = "draft", Pending = "pending", Sending = "sending", Failed = "failed", Conflict = "conflict" });
names!(Origin { App = "app", Cli = "cli" });

/// One queued change.
#[derive(Clone, Debug, PartialEq)]
pub struct Op {
    pub seq: i64,
    pub op_id: String,
    pub source: String,
    pub calendar: String,
    /// The event's ID (an instance ID for one occurrence of a series; ours for a create).
    pub event: String,
    pub kind: Kind,
    /// For an instance: its series, and original start (as Google's `originalStartTime`).
    pub series: Option<String>,
    pub original: Option<Value>,
    /// The changed fields as they were when you edited (for merging). Empty for creates.
    pub base: Value,
    /// The changed fields as you want them; the whole event for a create.
    pub patch: Value,
    /// `all`, `none`: whether guests hear about it.
    pub notify: String,
    pub state: State,
    pub origin: Origin,
    pub attempts: i64,
    pub next_attempt_at: Option<i64>,
    pub last_error: Option<String>,
    /// For a conflict: the server's version of the clashing fields.
    pub theirs: Option<Value>,
    pub created_at: i64,
}

impl Op {
    /// A new change, not yet queued.
    pub fn new(source: &str, calendar: &str, event: &str, kind: Kind, base: Value, patch: Value, origin: Origin) -> Self {
        Op {
            seq: 0,
            op_id: new_id(),
            source: source.into(),
            calendar: calendar.into(),
            event: event.into(),
            kind,
            series: None,
            original: None,
            base,
            patch,
            notify: "none".into(),
            state: State::Pending,
            origin,
            attempts: 0,
            next_attempt_at: None,
            last_error: None,
            theirs: None,
            created_at: Utc::now().timestamp(),
        }
    }

    /// Where its calendar lives in the store.
    pub fn key(&self) -> String {
        format!("{}/{}", self.source, self.calendar)
    }

    /// "Move Standup Tue 9:00 → 10:00"-style summary, for lists.
    pub fn describe(&self) -> String {
        let title = self.patch["summary"].as_str().or(self.base["summary"].as_str()).unwrap_or(&self.event).to_string();
        let when = |v: &Value| v["dateTime"].as_str().or(v["date"].as_str()).unwrap_or("").to_string();
        match self.kind {
            Kind::Create => format!("Create  {title}  {}", when(&self.patch["start"])),
            Kind::Delete => format!("Delete  {title}"),
            Kind::Update => {
                let mut parts = Vec::new();
                if self.patch.get("start").is_some() {
                    parts.push(format!("{} → {}", when(&self.base["start"]), when(&self.patch["start"])));
                }
                for (k, label) in [("summary", "title"), ("location", "location"), ("description", "notes")] {
                    if self.patch.get(k).is_some() {
                        parts.push(format!("{label}: {}", self.patch[k].as_str().unwrap_or("")));
                    }
                }
                format!("Update  {title}  {}", parts.join(" · "))
            }
        }
    }
}

/// A Google-compatible event ID: base32hex (`0-9a-v`), 26 characters from 128 random bits.
pub fn new_id() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    let n = u128::from_le_bytes(bytes);
    (0..26).map(|i| b"0123456789abcdefghijklmnopqrstuv"[((n >> (i * 5)) & 31) as usize] as char).collect()
}

// --- Merging -------------------------------------------------------------------------------

/// Fields compared as one unit: start and end move together.
fn groups(patch: &Value) -> Vec<Vec<&'static str>> {
    let mut out = Vec::new();
    if patch.get("start").is_some() || patch.get("end").is_some() {
        out.push(vec!["start", "end"]);
    }
    for k in ["summary", "location", "description"] {
        if patch.get(k).is_some() {
            out.push(vec![k]);
        }
    }
    out
}

/// Whether two values of a field mean the same: times compare as instants (or dates),
/// whatever their offset or zone; text compares trimmed; absent equals empty.
pub fn same(a: &Value, b: &Value) -> bool {
    let instant = |v: &Value| v["dateTime"].as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|t| t.with_timezone(&Utc));
    if a.is_object() || b.is_object() {
        return match (instant(a), instant(b)) {
            (Some(x), Some(y)) => x == y,
            (None, None) => a["date"] == b["date"],
            _ => false,
        };
    }
    let text = |v: &Value| v.as_str().map(str::trim).unwrap_or("").to_string();
    text(a) == text(b)
}

/// What merging your change with the server's current version gives.
#[derive(Debug, PartialEq)]
pub enum Merge {
    /// Send this (only the fields still to change). Empty: the server already has it all.
    Send(Value),
    /// The same field changed both ways: the server's values of the clashing fields.
    Conflict(Value),
}

/// Three-way merge of one change: `base` (as you saw it), `ours` (your patch) and `theirs`
/// (the server's whole event now). Fields only you changed go; fields already as you want
/// are dropped (that is also how a lost response resolves); a field changed both ways clashes.
pub fn merge(base: &Value, ours: &Value, theirs: &Value) -> Merge {
    let mut send = Map::new();
    let mut clash = Map::new();
    for group in groups(ours) {
        let already = group.iter().all(|k| same(&ours[*k], &theirs[*k]));
        let untouched = group.iter().all(|k| same(&base[*k], &theirs[*k]));
        if already {
            continue;
        }
        if untouched {
            for k in &group {
                if let Some(v) = ours.get(*k) {
                    send.insert(k.to_string(), v.clone());
                }
            }
        } else {
            for k in &group {
                clash.insert(k.to_string(), theirs[*k].clone());
            }
        }
    }
    if clash.is_empty() { Merge::Send(Value::Object(send)) } else { Merge::Conflict(Value::Object(clash)) }
}

/// Lays `patch`'s fields over `onto`.
fn apply_patch(onto: &mut Value, patch: &Value) {
    if let (Some(o), Some(p)) = (onto.as_object_mut(), patch.as_object()) {
        for (k, v) in p {
            o.insert(k.clone(), v.clone());
        }
    }
}

// --- Storage -------------------------------------------------------------------------------

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Op> {
    let json = |i: usize| -> rusqlite::Result<Value> { Ok(r.get::<_, Option<String>>(i)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)) };
    Ok(Op {
        seq: r.get(0)?,
        op_id: r.get(1)?,
        source: r.get(2)?,
        calendar: r.get(3)?,
        event: r.get(4)?,
        kind: Kind::parse(&r.get::<_, String>(5)?).unwrap_or(Kind::Update),
        series: r.get(6)?,
        original: r.get::<_, Option<String>>(7)?.and_then(|s| serde_json::from_str(&s).ok()),
        base: json(8)?,
        patch: json(9)?,
        notify: r.get(10)?,
        state: State::parse(&r.get::<_, String>(11)?).unwrap_or(State::Failed),
        origin: Origin::parse(&r.get::<_, String>(12)?).unwrap_or(Origin::Cli),
        attempts: r.get(13)?,
        next_attempt_at: r.get(14)?,
        last_error: r.get(15)?,
        theirs: r.get::<_, Option<String>>(16)?.and_then(|s| serde_json::from_str(&s).ok()),
        created_at: r.get(17)?,
    })
}

const COLUMNS: &str = "seq, op_id, source, calendar, event, kind, series, original, base, patch, notify, state, origin,
    attempts, next_attempt_at, last_error, theirs, created_at";

impl Store {
    /// Every queued change, oldest first.
    pub fn ops(&self) -> rusqlite::Result<Vec<Op>> {
        self.conn.prepare(&format!("SELECT {COLUMNS} FROM outbox ORDER BY seq"))?.query_map([], row)?.collect()
    }

    #[cfg(test)]
    pub fn op(&self, seq: i64) -> rusqlite::Result<Option<Op>> {
        self.conn.query_row(&format!("SELECT {COLUMNS} FROM outbox WHERE seq = ?1"), [seq], row).optional()
    }

    /// Queues a change, folding it into an unsent change to the same event where it can:
    /// edits merge, an edit to an unsent create becomes part of the create, and deleting an
    /// unsent create leaves nothing at all. Returns the change's `seq`, or `None` if it
    /// cancelled out.
    pub fn queue(&mut self, mut op: Op) -> rusqlite::Result<Option<i64>> {
        let tx = self.conn.transaction()?;
        let open: Option<Op> = tx
            .query_row(
                &format!("SELECT {COLUMNS} FROM outbox WHERE source = ?1 AND calendar = ?2 AND event = ?3 AND state IN ('draft', 'pending') ORDER BY seq DESC LIMIT 1"),
                params![op.source, op.calendar, op.event],
                row,
            )
            .optional()?;
        let result = match open {
            Some(prev) if prev.kind == Kind::Create && op.kind == Kind::Delete => {
                tx.execute("DELETE FROM outbox WHERE seq = ?1", [prev.seq])?;
                None
            }
            Some(mut prev) if prev.kind != Kind::Delete && op.kind == Kind::Update => {
                // Keep the earliest base for each field, and the latest value.
                for (k, v) in op.base.as_object().cloned().unwrap_or_default() {
                    if prev.base.get(&k).is_none() && prev.kind == Kind::Update {
                        prev.base[&k] = v;
                    }
                }
                apply_patch(&mut prev.patch, &op.patch);
                // An agent's change folded into yours stays staged.
                let state = if op.state == State::Draft { State::Draft } else { prev.state };
                tx.execute(
                    "UPDATE outbox SET base = ?2, patch = ?3, state = ?4, next_attempt_at = ?5 WHERE seq = ?1",
                    params![prev.seq, prev.base.to_string(), prev.patch.to_string(), state.name(), op.next_attempt_at],
                )?;
                Some(prev.seq)
            }
            Some(prev) if prev.kind == Kind::Update && op.kind == Kind::Delete => {
                op.base = prev.base;
                tx.execute("DELETE FROM outbox WHERE seq = ?1", [prev.seq])?;
                Some(insert(&tx, &op)?)
            }
            _ => Some(insert(&tx, &op)?),
        };
        tx.commit()?;
        Ok(result)
    }

    pub fn set_op_state(&self, seq: i64, state: State, error: Option<&str>, next_attempt_at: Option<i64>) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE outbox SET state = ?2, last_error = ?3, next_attempt_at = ?4 WHERE seq = ?1",
            params![seq, state.name(), error, next_attempt_at],
        )?;
        Ok(())
    }

    /// Claims a change for sending; false if another sender got there first.
    pub fn claim(&self, seq: i64) -> rusqlite::Result<bool> {
        Ok(self.conn.execute("UPDATE outbox SET state = 'sending', attempts = attempts + 1 WHERE seq = ?1 AND state = 'pending'", [seq])? == 1)
    }

    /// A cheap fingerprint of the outbox, to notice changes made elsewhere (`calm event …`).
    pub fn outbox_fingerprint(&self) -> String {
        self.conn
            .query_row("SELECT COALESCE(group_concat(seq || state, ','), '') FROM outbox", [], |r| r.get(0))
            .unwrap_or_default()
    }

    /// Changes left mid-send by a quit or crash go back to pending (sends are idempotent).
    pub fn recover_sending(&self) -> rusqlite::Result<()> {
        self.conn.execute("UPDATE outbox SET state = 'pending' WHERE state = 'sending'", [])?;
        Ok(())
    }

    pub fn set_conflict(&self, seq: i64, theirs: &Value) -> rusqlite::Result<()> {
        self.conn.execute("UPDATE outbox SET state = 'conflict', theirs = ?2, last_error = NULL WHERE seq = ?1", params![seq, theirs.to_string()])?;
        Ok(())
    }

    /// Resolves a conflict your way: the clashing fields' base becomes their values, so your
    /// change goes against their latest version.
    pub fn keep_mine(&self, seq: i64) -> rusqlite::Result<()> {
        let Some((base, theirs)) = self
            .conn
            .query_row("SELECT base, theirs FROM outbox WHERE seq = ?1", [seq], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))
            .optional()?
        else {
            return Ok(());
        };
        let mut base: Value = serde_json::from_str(&base).unwrap_or(json!({}));
        if let Some(theirs) = theirs.and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
            apply_patch(&mut base, &theirs);
        }
        self.conn.execute("UPDATE outbox SET base = ?2, theirs = NULL, state = 'pending', last_error = NULL WHERE seq = ?1", params![seq, base.to_string()])?;
        Ok(())
    }

    /// Commits staged changes: they become pending, to send now.
    pub fn commit(&self, seqs: &[i64]) -> rusqlite::Result<usize> {
        let mut n = 0;
        for s in seqs {
            n += self.conn.execute("UPDATE outbox SET state = 'pending', next_attempt_at = NULL, last_error = NULL, theirs = NULL WHERE seq = ?1 AND state IN ('draft', 'failed', 'conflict')", [s])?;
        }
        Ok(n)
    }

    /// Drops changes (never ones being sent).
    pub fn discard(&self, seqs: &[i64]) -> rusqlite::Result<usize> {
        let mut n = 0;
        for s in seqs {
            n += self.conn.execute("DELETE FROM outbox WHERE seq = ?1 AND state != 'sending'", [s])?;
        }
        Ok(n)
    }
}

fn insert(tx: &rusqlite::Transaction<'_>, op: &Op) -> rusqlite::Result<i64> {
    tx.execute(
        "INSERT INTO outbox (op_id, source, calendar, event, kind, series, original, base, patch, notify, state, origin,
             attempts, next_attempt_at, last_error, theirs, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0, ?13, NULL, NULL, ?14)",
        params![
            op.op_id,
            op.source,
            op.calendar,
            op.event,
            op.kind.name(),
            op.series,
            op.original.as_ref().map(Value::to_string),
            op.base.to_string(),
            op.patch.to_string(),
            op.notify,
            op.state.name(),
            op.origin.name(),
            op.next_attempt_at,
            op.created_at
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

/// Records a sent change, for the changes panel's history and for undo.
pub fn record(tx: &rusqlite::Transaction<'_>, op: &Op, after: &Value) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO history (at, source, calendar, event, kind, before, after, origin) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![Utc::now().timestamp(), op.source, op.calendar, op.event, op.kind.name(), op.base.to_string(), after.to_string(), op.origin.name()],
    )?;
    tx.execute("DELETE FROM history WHERE id <= (SELECT MAX(id) - 1000 FROM history)", [])?;
    Ok(())
}

// --- Overlay -------------------------------------------------------------------------------

/// The events of one calendar with its queued changes applied, keyed by event ID. Updates to
/// one instance of a series become an exception to it, as Google would store them.
pub fn overlay_events(mut events: HashMap<String, Value>, ops: &[&Op]) -> HashMap<String, Value> {
    for op in ops {
        match op.kind {
            Kind::Create => {
                let mut e = op.patch.clone();
                e["id"] = json!(op.event);
                events.insert(op.event.clone(), e);
            }
            Kind::Update | Kind::Delete => {
                let mut e = match events.get(&op.event) {
                    Some(e) => e.clone(),
                    // An instance with no exception yet: start one from its series.
                    None => match (&op.series, &op.original) {
                        (Some(series), Some(original)) => {
                            let Some(s) = events.get(series) else { continue };
                            let mut e = s.clone();
                            if let Some(o) = e.as_object_mut() {
                                o.remove("recurrence");
                            }
                            e["id"] = json!(op.event);
                            e["recurringEventId"] = json!(series);
                            e["originalStartTime"] = original.clone();
                            // Its own times, until the patch says otherwise.
                            if let (Some(s), Some(e2)) = (op.base.get("start"), op.base.get("end")) {
                                e["start"] = s.clone();
                                e["end"] = e2.clone();
                            }
                            e
                        }
                        _ => continue,
                    },
                };
                if op.kind == Kind::Delete {
                    if e["recurringEventId"].is_null() {
                        events.remove(&op.event);
                        continue;
                    }
                    e["status"] = json!("cancelled");
                } else {
                    apply_patch(&mut e, &op.patch);
                }
                events.insert(op.event.clone(), e);
            }
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Value {
        json!({ "dateTime": s, "timeZone": "Europe/London" })
    }

    #[test]
    fn merge_table() {
        let base = json!({ "start": t("2026-11-24T09:00:00Z"), "end": t("2026-11-24T10:00:00Z") });
        let ours = json!({ "start": t("2026-11-24T10:00:00Z"), "end": t("2026-11-24T11:00:00Z") });
        let theirs_untouched = json!({ "summary": "Standup", "start": t("2026-11-24T09:00:00Z"), "end": t("2026-11-24T10:00:00Z") });
        // Only we changed the time: send it.
        assert_eq!(merge(&base, &ours, &theirs_untouched), Merge::Send(ours.clone()));
        // They renamed it, we moved it: still just our move (their title stands).
        let renamed = json!({ "summary": "Daily", "start": t("2026-11-24T09:00:00Z"), "end": t("2026-11-24T10:00:00Z") });
        assert_eq!(merge(&base, &ours, &renamed), Merge::Send(ours.clone()));
        // Already as we want, however the offset is written: nothing to send (a lost response).
        let done = json!({ "start": { "dateTime": "2026-11-24T18:00:00+08:00" }, "end": { "dateTime": "2026-11-24T19:00:00+08:00" } });
        assert_eq!(merge(&base, &ours, &done), Merge::Send(json!({})));
        // They moved it elsewhere: a conflict, carrying their times.
        let moved = json!({ "start": t("2026-11-24T15:00:00Z"), "end": t("2026-11-24T16:00:00Z") });
        assert!(matches!(merge(&base, &ours, &moved), Merge::Conflict(c) if c["start"] == moved["start"]));
        // Text fields compare trimmed; absent counts as empty.
        assert_eq!(merge(&json!({ "location": "" }), &json!({ "location": "Cafe" }), &json!({})), Merge::Send(json!({ "location": "Cafe" })));
    }

    #[test]
    fn queue_coalesces() {
        let mut store = Store::memory().unwrap();
        let ev = |kind, base: Value, patch: Value| Op::new("Google", "me", "e1", kind, base, patch, Origin::App);
        let a = store.queue(ev(Kind::Update, json!({ "summary": "A" }), json!({ "summary": "B" }))).unwrap().unwrap();
        let b = store.queue(ev(Kind::Update, json!({ "summary": "B", "location": "" }), json!({ "summary": "C", "location": "Here" }))).unwrap().unwrap();
        // One change: the first base, the latest values.
        assert_eq!(a, b);
        let op = store.op(a).unwrap().unwrap();
        assert_eq!(op.base, json!({ "summary": "A", "location": "" }));
        assert_eq!(op.patch, json!({ "summary": "C", "location": "Here" }));
        // Create then delete, unsent: nothing at all.
        let c = Op::new("Google", "me", "new1", Kind::Create, json!({}), json!({ "summary": "X" }), Origin::App);
        store.queue(c).unwrap();
        assert_eq!(store.queue(Op::new("Google", "me", "new1", Kind::Delete, json!({}), json!({}), Origin::App)).unwrap(), None);
        assert_eq!(store.ops().unwrap().len(), 1);
        // Once claimed for sending, a new edit queues behind it.
        assert!(store.claim(a).unwrap());
        let d = store.queue(ev(Kind::Update, json!({ "summary": "C" }), json!({ "summary": "D" }))).unwrap().unwrap();
        assert_ne!(a, d);
    }

    #[test]
    fn ids_are_base32hex() {
        let id = new_id();
        assert_eq!(id.len(), 26);
        assert!(id.chars().all(|c| c.is_ascii_digit() || ('a'..='v').contains(&c)));
        assert_ne!(id, new_id());
    }
}
