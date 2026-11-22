//! Turning an edit to an event you see into a change for the outbox: what the app (and later
//! anything else) uses so that every edit is built the same way.

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, Utc};
use serde_json::{Value, json};

use super::outbox::{Kind, Op, Origin, State, new_id};
use crate::model::EditRef;

/// Google's `{dateTime, timeZone}` for an instant, in your zone (kept, so a recurring event
/// stays at its local time across clock changes).
pub fn time(t: DateTime<Utc>) -> Value {
    let mut v = json!({ "dateTime": t.with_timezone(&Local).to_rfc3339() });
    if let Some(tz) = crate::zones::Zones::load(None, None).primary {
        v["timeZone"] = json!(tz.name());
    }
    v
}

pub fn date(d: NaiveDate) -> Value {
    json!({ "date": d.format("%Y-%m-%d").to_string() })
}

/// The event's editable fields as they are now, in Google's shape.
pub fn fields(r: &EditRef, title: &str, location: Option<&str>) -> Value {
    let (start, end) = if r.all_day { (date(r.start.date_naive()), date(r.end.date_naive())) } else { (time(r.start), time(r.end)) };
    json!({
        "summary": title, "location": location.unwrap_or(""), "description": r.description.clone().unwrap_or_default(),
        "start": start, "end": end,
    })
}

/// `(source, calendar)` from a store key.
fn split(key: &str) -> (&str, &str) {
    key.split_once('/').unwrap_or((key, ""))
}

/// For one occurrence of a series: its series, and its original start (from its instance ID).
fn instance(r: &EditRef) -> (Option<String>, Option<Value>) {
    if r.id == r.uid {
        return (None, None);
    }
    let stamp = r.id.rsplit('_').next().unwrap_or("");
    let original = NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%SZ")
        .map(|t| json!({ "dateTime": t.and_utc().to_rfc3339() }))
        .or_else(|_| NaiveDate::parse_from_str(stamp, "%Y%m%d").map(date))
        .ok();
    (Some(r.uid.clone()), original)
}

/// A change to some of an event's fields: `now` is how they are, `patch` how you want them.
pub fn update(r: &EditRef, now: &Value, patch: Value, origin: Origin, state: State) -> Op {
    let (source, calendar) = split(&r.key);
    let mut base = json!({ "summary": now["summary"] });
    for k in patch.as_object().map(|p| p.keys().cloned().collect::<Vec<_>>()).unwrap_or_default() {
        base[&k] = now[&k].clone();
    }
    let mut op = Op::new(source, calendar, &r.id, Kind::Update, base, patch, origin);
    (op.series, op.original) = instance(r);
    op.state = state;
    op
}

/// Deleting an event; its fields ride along, so an undo can bring it back.
pub fn delete(r: &EditRef, now: &Value, origin: Origin, state: State) -> Op {
    let (source, calendar) = split(&r.key);
    let mut op = Op::new(source, calendar, &r.id, Kind::Delete, now.clone(), json!({}), origin);
    (op.series, op.original) = instance(r);
    op.state = state;
    op
}

/// A new event in `key`'s calendar with these fields.
pub fn create(key: &str, event: Value, origin: Origin, state: State) -> Op {
    let (source, calendar) = split(key);
    let mut op = Op::new(source, calendar, &new_id(), Kind::Create, json!({}), event, origin);
    op.state = state;
    op
}
