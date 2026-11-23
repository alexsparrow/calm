//! Google Calendar, read-only, through its API.
//!
//! Sign-in is OAuth for installed apps: `calm google login` opens Google's consent page
//! and catches the answer on a loopback port, with PKCE. The client ID is your own
//! (`secrets/google-client.json`, downloaded from Google Cloud); the refresh token goes
//! to `secrets/google-<account>.json` (mode 600). The only scope is
//! `calendar.readonly` with `calendar.events` (to write events; see `docs/editing.md`).
//!
//! Sync: the calendar list first (names, colours, access), then each calendar on its own:
//! - Calendars you can read sync incrementally. The first sync lists every event
//!   (recurring ones as rules, not instances); after that Google's sync token returns
//!   only what changed. Raw events are kept (as JSON) and re-expanded with the same
//!   recurrence code as iCal feeds, so overrides and cancellations behave identically.
//!   A token Google has expired (HTTP 410) triggers a full sync.
//! - Calendars shared with you as free/busy only come from the free/busy API, as
//!   "Busy" blocks over a shorter window.
//!
//! Each calendar is stored under `<source>/<calendar id>`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Value, json};

use super::ics::{self, Component, Moment, Occurrence};
use super::outbox::{self, Kind, Merge, Op, State, merge};
use super::store::{RemoteCalendar, Store};
use serde_json::Map;
use super::{Outcome, config_dir};

/// Read everything (calendar list, free/busy), and write events.
const SCOPE: &str = "https://www.googleapis.com/auth/calendar.readonly https://www.googleapis.com/auth/calendar.events";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const API: &str = "https://www.googleapis.com/calendar/v3";

// --- Credentials -----------------------------------------------------------------

struct Client {
    id: String,
    secret: String,
}

fn secrets() -> Result<PathBuf, String> {
    config_dir().map(|d| d.join("secrets")).ok_or_else(|| "no config directory".into())
}

/// The OAuth client you created in Google Cloud ("Desktop app"), as Google downloads it.
fn client() -> Result<Client, String> {
    let path = secrets()?.join("google-client.json");
    let text = std::fs::read_to_string(&path).map_err(|_| format!("no Google client: save it as {} (see docs/google.md)", path.display()))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let c = v.get("installed").or_else(|| v.get("web")).unwrap_or(&v);
    match (c["client_id"].as_str(), c["client_secret"].as_str()) {
        (Some(id), Some(secret)) => Ok(Client { id: id.into(), secret: secret.into() }),
        _ => Err(format!("{}: expected client_id and client_secret", path.display())),
    }
}

fn token_path(account: &str) -> Result<PathBuf, String> {
    let safe: String = account.chars().map(|c| if c.is_ascii_alphanumeric() || "@._-".contains(c) { c } else { '_' }).collect();
    Ok(secrets()?.join(format!("google-{safe}.json")))
}

fn write_private(path: &std::path::Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp).map_err(|e| e.to_string())?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .build()
        .into()
}

/// POSTs a form to Google's token endpoint. Errors carry Google's error code only.
fn token_request(form: &[(&str, &str)]) -> Result<Value, String> {
    let mut resp = agent().post(TOKEN_URL).send_form(form.iter().copied()).map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if status == 200 {
        Ok(v)
    } else {
        Err(v["error"].as_str().unwrap_or("token request failed").to_string())
    }
}

/// A current access token for `account`, refreshed (and saved) when near expiry.
fn access_token(account: &str) -> Result<String, String> {
    let path = token_path(account)?;
    let text = std::fs::read_to_string(&path).map_err(|_| format!("not signed in to Google as {account}: run `calm google login {account}`"))?;
    let mut tok: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let now = Utc::now().timestamp();
    if let (Some(t), Some(exp)) = (tok["access_token"].as_str(), tok["expires_at"].as_i64())
        && exp - 60 > now
    {
        return Ok(t.to_string());
    }
    let refresh = tok["refresh_token"].as_str().ok_or("no refresh token: run `calm google login` again")?.to_string();
    let c = client()?;
    let fresh = token_request(&[("client_id", &c.id), ("client_secret", &c.secret), ("refresh_token", &refresh), ("grant_type", "refresh_token")])
        .map_err(|e| if e == "invalid_grant" { format!("Google sign-in expired: run `calm google login {account}`") } else { e })?;
    let access = fresh["access_token"].as_str().ok_or("no access token in response")?.to_string();
    tok["access_token"] = json!(access);
    tok["expires_at"] = json!(now + fresh["expires_in"].as_i64().unwrap_or(3600));
    write_private(&path, &tok.to_string())?;
    Ok(access)
}

fn random_token(bytes: usize) -> Result<String, String> {
    use base64::Engine;
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| e.to_string())?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf))
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(b);
                    i += 2;
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `calm google login ACCOUNT`: consent in the browser, tokens saved privately.
pub fn login(account: &str) -> Result<(), String> {
    use base64::Engine;
    use sha2::Digest;
    let c = client()?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let redirect = format!("http://127.0.0.1:{}", listener.local_addr().map_err(|e| e.to_string())?.port());
    let verifier = random_token(48)?;
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
    let state = random_token(16)?;
    let url = format!(
        "{AUTH_URL}?client_id={}&redirect_uri={}&response_type=code&scope={}&code_challenge={challenge}&code_challenge_method=S256&state={state}&access_type=offline&prompt=consent&login_hint={}",
        url_encode(&c.id),
        url_encode(&redirect),
        url_encode(SCOPE),
        url_encode(account)
    );
    println!("Opening Google sign-in in your browser. If it doesn't open, visit:\n\n{url}\n");
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = std::process::Command::new(opener).arg(&url).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();

    // One request: Google redirects the browser to us with `code` and `state`.
    let (stream, _) = listener.accept().map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).map_err(|e| e.to_string())?;
    let query = line.split_whitespace().nth(1).and_then(|p| p.split_once('?')).map(|(_, q)| q.to_string()).unwrap_or_default();
    let params: HashMap<String, String> = query.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), url_decode(v))).collect();
    let reply = |mut s: &std::net::TcpStream, msg: &str| {
        let body = format!("<html><body style=\"font-family:sans-serif;padding:3em\"><h2>{msg}</h2>You can close this tab.</body></html>");
        let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    };
    if params.get("state") != Some(&state) {
        reply(&stream, "calm: sign-in didn't match; try again.");
        return Err("state mismatch: try again".into());
    }
    let Some(code) = params.get("code") else {
        reply(&stream, "calm: sign-in was cancelled.");
        return Err(params.get("error").cloned().unwrap_or_else(|| "no code returned".into()));
    };
    let tok = token_request(&[
        ("client_id", &c.id),
        ("client_secret", &c.secret),
        ("code", code),
        ("code_verifier", &verifier),
        ("redirect_uri", &redirect),
        ("grant_type", "authorization_code"),
    ]);
    let tok = match tok {
        Ok(t) => t,
        Err(e) => {
            reply(&stream, "calm: Google didn't accept the sign-in.");
            return Err(e);
        }
    };
    let saved = json!({
        "refresh_token": tok["refresh_token"],
        "access_token": tok["access_token"],
        "expires_at": Utc::now().timestamp() + tok["expires_in"].as_i64().unwrap_or(3600),
    });
    if saved["refresh_token"].is_null() {
        reply(&stream, "calm: Google gave no refresh token.");
        return Err("Google returned no refresh token; remove calm's access at myaccount.google.com/permissions and sign in again".into());
    }
    write_private(&token_path(account)?, &saved.to_string())?;
    reply(&stream, "calm is signed in to Google Calendar.");
    println!("Signed in as {account}. Add to calm.kdl:\n\n    source \"Google\" google=\"{account}\"\n");
    Ok(())
}

// --- API ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ApiError {
    /// 410: the sync token expired; start over.
    Gone,
    Other(String),
}

/// What sync needs from Google, so tests can stand in for it.
pub trait Api {
    fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value, ApiError>;
    fn post(&self, path: &str, body: &Value) -> Result<Value, ApiError>;
    /// Any request, returning the status and body whatever the status; `Err` only when no
    /// answer came at all (network, timeout). Writes go through this.
    fn send(&self, method: &str, path: &str, query: &[(&str, String)], body: Option<&Value>, if_match: Option<&str>) -> Result<(u16, Value), String>;
}

struct Http {
    token: String,
    agent: ureq::Agent,
}

impl Http {
    fn finish(mut resp: ureq::http::Response<ureq::Body>) -> Result<Value, ApiError> {
        let status = resp.status().as_u16();
        let text = resp.body_mut().with_config().limit(64 << 20).read_to_string().unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        match status {
            200..=299 => Ok(v),
            410 => Err(ApiError::Gone),
            s => Err(ApiError::Other(format!("Google {s}: {}", v["error"]["message"].as_str().unwrap_or("request failed")))),
        }
    }
}

impl Api for Http {
    fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value, ApiError> {
        let mut req = self.agent.get(format!("{API}{path}")).header("Authorization", format!("Bearer {}", self.token));
        for (k, v) in query {
            req = req.query(*k, v);
        }
        Self::finish(req.call().map_err(|e| ApiError::Other(e.to_string()))?)
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value, ApiError> {
        let req = self.agent.post(format!("{API}{path}")).header("Authorization", format!("Bearer {}", self.token));
        let req = req.header("Content-Type", "application/json");
        Self::finish(req.send(body.to_string()).map_err(|e| ApiError::Other(e.to_string()))?)
    }

    fn send(&self, method: &str, path: &str, query: &[(&str, String)], body: Option<&Value>, if_match: Option<&str>) -> Result<(u16, Value), String> {
        let url = format!("{API}{path}");
        let auth = format!("Bearer {}", self.token);
        let with = |mut q: String| {
            for (i, (k, v)) in query.iter().enumerate() {
                q += if i == 0 { "?" } else { "&" };
                q += &format!("{k}={}", url_encode(v));
            }
            q
        };
        let url = with(url);
        let mut resp = match (method, body) {
            ("GET", _) => {
                let r = self.agent.get(&url).header("Authorization", &auth);
                r.call()
            }
            ("DELETE", _) => {
                let mut r = self.agent.delete(&url).header("Authorization", &auth);
                if let Some(e) = if_match {
                    r = r.header("If-Match", e);
                }
                r.call()
            }
            (m, b) => {
                let mut r = if m == "PATCH" { self.agent.patch(&url) } else { self.agent.post(&url) }.header("Authorization", &auth).header("Content-Type", "application/json");
                if let Some(e) = if_match {
                    r = r.header("If-Match", e);
                }
                r.send(b.map_or_else(String::new, Value::to_string))
            }
        }
        .map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
    }
}

fn err(e: ApiError) -> String {
    match e {
        ApiError::Gone => "Google: sync token expired".into(),
        ApiError::Other(s) => s,
    }
}

// --- Sync ----------------------------------------------------------------------------

/// The calendars to show: the ones ticked in Google Calendar, in its order.
fn calendar_list(api: &dyn Api, source_name: &str) -> Result<Vec<RemoteCalendar>, String> {
    let mut out = Vec::new();
    let mut page: Option<String> = None;
    loop {
        let mut q = vec![("maxResults", "250".to_string())];
        if let Some(p) = &page {
            q.push(("pageToken", p.clone()));
        }
        let v = api.get("/users/me/calendarList", &q).map_err(err)?;
        for c in v["items"].as_array().into_iter().flatten() {
            if c["deleted"].as_bool() == Some(true) || c["hidden"].as_bool() == Some(true) || c["selected"].as_bool() == Some(false) {
                continue;
            }
            let Some(id) = c["id"].as_str() else { continue };
            // Your own calendar is named after the source (its Google name is your address).
            let name = c["summaryOverride"].as_str().map(str::to_string).unwrap_or_else(|| {
                if c["primary"].as_bool() == Some(true) { source_name.to_string() } else { c["summary"].as_str().unwrap_or(id).to_string() }
            });
            out.push(RemoteCalendar {
                id: id.into(),
                name,
                color: c["backgroundColor"].as_str().map(str::to_string),
                access: c["accessRole"].as_str().unwrap_or("reader").into(),
            });
        }
        match v["nextPageToken"].as_str() {
            Some(p) => page = Some(p.into()),
            None => return Ok(out),
        }
    }
}

/// `{"dateTime": "…", "timeZone": "…"}` or `{"date": "…"}`.
fn moment(v: &Value, zone: Option<&str>) -> Option<Moment> {
    if let Some(d) = v["date"].as_str() {
        return NaiveDate::parse_from_str(d, "%Y-%m-%d").ok().map(Moment::Date);
    }
    let t = DateTime::parse_from_rfc3339(v["dateTime"].as_str()?).ok()?;
    // Keep the zone, so recurrences stay at the same local time across DST changes.
    match v["timeZone"].as_str().or(zone).and_then(|z| z.parse::<chrono_tz::Tz>().ok()) {
        Some(tz) => Some(Moment::Zoned(t.with_timezone(&tz).naive_local(), tz)),
        None => Some(Moment::Utc(t.with_timezone(&Utc))),
    }
}

/// A Google event as a component. Instances that differ from their series become overrides
/// (uid = the series, recurrence-id = the original start), as in iCalendar.
pub fn component(e: &Value, calendar_zone: Option<&str>, free_busy_only: bool) -> Option<Component> {
    // A cancelled instance comes with only its original start; it still has to suppress it.
    let start = moment(&e["start"], calendar_zone).or_else(|| moment(&e["originalStartTime"], calendar_zone))?;
    let end = moment(&e["end"], calendar_zone);
    let duration = match (&start, end) {
        (Moment::Date(a), Some(Moment::Date(b))) => chrono::Duration::days((b - *a).num_days().max(1)),
        (_, Some(end)) => (end.instant() - start.instant()).max(chrono::Duration::zero()),
        (s, None) => if s.is_date() { chrono::Duration::days(1) } else { chrono::Duration::zero() },
    };
    let recurrence: Vec<String> = e["recurrence"].as_array().into_iter().flatten().filter_map(|l| l.as_str().map(str::to_string)).collect();
    let (rrule, exdates, rdates) = ics::recurrence_lines(&recurrence);
    let series = e["recurringEventId"].as_str();
    let recurrence_id = series.and_then(|_| moment(&e["originalStartTime"], calendar_zone)).map(|m| m.instant());
    let text = |k: &str| e[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let people = |v: &Value| v["displayName"].as_str().or(v["email"].as_str()).map(str::to_string);
    // A calendar organising its own events, or you organising yours, isn't someone you're with.
    let organiser = |v: &Value| (v["self"] != true && !v["email"].as_str().is_some_and(|m| m.ends_with("calendar.google.com"))).then(|| people(v)).flatten();
    Some(Component {
        uid: series.or(e["id"].as_str())?.to_string(),
        recurrence_id,
        sequence: e["sequence"].as_i64().unwrap_or(0),
        raw: e.to_string(),
        summary: text("summary").unwrap_or_else(|| if free_busy_only { "Busy".into() } else { "(no title)".into() }),
        location: text("location"),
        description: text("description"),
        status: e["status"].as_str().map(str::to_ascii_uppercase),
        transp: e["transparency"].as_str().map(str::to_ascii_uppercase),
        organizer: organiser(&e["organizer"]),
        // Rooms aren't guests.
        attendees: e["attendees"].as_array().into_iter().flatten().filter(|a| a["resource"] != true).filter_map(people).collect(),
        categories: Vec::new(),
        start,
        duration,
        rrule,
        exdates,
        rdates,
    })
}

/// Lists a calendar's events: everything (no token), or what changed since `token`.
/// Returns the changed events and the next token.
fn list_events(api: &dyn Api, calendar: &str, token: Option<&str>) -> Result<(Vec<Value>, Option<String>), ApiError> {
    let path = format!("/calendars/{}/events", url_encode(calendar));
    let (mut items, mut page) = (Vec::new(), None::<String>);
    loop {
        // Recurring events come as rules, not instances: compact, and the token stays valid.
        let mut q = vec![("maxResults", "2500".to_string()), ("singleEvents", "false".into()), ("showDeleted", "true".into())];
        if let Some(t) = token {
            q.push(("syncToken", t.to_string()));
        }
        if let Some(p) = &page {
            q.push(("pageToken", p.clone()));
        }
        let v = api.get(&path, &q)?;
        items.extend(v["items"].as_array().into_iter().flatten().cloned());
        match v["nextPageToken"].as_str() {
            Some(p) => page = Some(p.into()),
            None => return Ok((items, v["nextSyncToken"].as_str().map(str::to_string))),
        }
    }
}

/// Folds changed events into the stored set, keyed by event id. A cancelled event is
/// deleted, except a cancelled instance of a series, which stays to suppress it.
fn apply(events: &mut HashMap<String, Value>, changes: Vec<Value>) {
    for e in changes {
        let Some(id) = e["id"].as_str().map(str::to_string) else { continue };
        if e["status"].as_str() == Some("cancelled") && e["recurringEventId"].is_null() {
            events.remove(&id);
        } else {
            events.insert(id, e);
        }
    }
}

/// Syncs one readable calendar into `<source>/<id>`. `Ok(true)` if anything changed.
fn sync_events(store: &mut Store, api: &dyn Api, key: &str, cal: &RemoteCalendar, now: DateTime<Utc>) -> Result<bool, String> {
    let mut state = store.state(key).map_err(|e| e.to_string())?;
    state.name = key.to_string();
    let mut events: HashMap<String, Value> = HashMap::new();
    if state.sync_token.is_some() {
        for raw in store.raw_components(key).map_err(|e| e.to_string())? {
            if let Ok(v) = serde_json::from_str::<Value>(&raw)
                && let Some(id) = v["id"].as_str()
            {
                events.insert(id.to_string(), v);
            }
        }
    }
    let (changes, token) = match list_events(api, &cal.id, state.sync_token.as_deref()) {
        Ok(r) => r,
        Err(ApiError::Gone) => {
            events.clear();
            list_events(api, &cal.id, None).map_err(err)?
        }
        Err(e) => return Err(err(e)),
    };
    let unchanged = changes.is_empty() && state.sync_token.is_some();
    state.sync_token = token;
    state.last_ok_at = Some(now.timestamp());
    state.last_error = None;
    if unchanged {
        store.save_state(&state).map_err(|e| e.to_string())?;
        return Ok(false);
    }
    apply(&mut events, changes);
    let components: Vec<Component> = events.values().filter_map(|e| component(e, None, false)).collect();
    let (occurrences, _) = ics::expand(&components, super::window(now));
    state.event_count = components.len() as i64;
    store.replace(&state, &components, &occurrences).map_err(|e| format!("cache write failed: {e}"))?;
    Ok(true)
}

/// Free/busy calendars: busy blocks from a month back to half a year ahead, queried in
/// eight-week spans to stay inside the range Google accepts per request.
fn sync_free_busy(store: &mut Store, api: &dyn Api, key: &str, cal: &RemoteCalendar, now: DateTime<Utc>) -> Result<bool, String> {
    let mut busy = Vec::new();
    let mut from = now - chrono::Duration::days(28);
    let until = now + chrono::Duration::days(182);
    while from < until {
        let to = (from + chrono::Duration::days(56)).min(until);
        let v = api
            .post("/freeBusy", &json!({ "timeMin": from.to_rfc3339(), "timeMax": to.to_rfc3339(), "items": [{ "id": cal.id }] }))
            .map_err(err)?;
        for b in v["calendars"][&cal.id]["busy"].as_array().into_iter().flatten() {
            let t = |k: &str| b[k].as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|t| t.with_timezone(&Utc));
            if let (Some(start), Some(end)) = (t("start"), t("end")) {
                busy.push(Occurrence {
                    uid: format!("busy-{}", start.timestamp()),
                    start,
                    end,
                    all_day: false,
                    summary: "Busy".into(),
                    location: None,
                    description: None,
                    status: Some("BUSY".into()),
                    transp: None,
                    organizer: None,
                    attendees: Vec::new(),
                    categories: Vec::new(),
                    ref_id: String::new(),
                });
            }
        }
        from = to;
    }
    // Busy blocks that span two queries arrive twice.
    busy.sort_by_key(|o| (o.start, o.end));
    busy.dedup_by_key(|o| (o.start, o.end));
    let content = format!("{:016x}", super::hash(&busy.iter().map(|o| format!("{}-{}", o.start.timestamp(), o.end.timestamp())).collect::<String>()));
    let mut state = store.state(key).map_err(|e| e.to_string())?;
    state.name = key.to_string();
    state.last_ok_at = Some(now.timestamp());
    state.last_error = None;
    if state.content_hash.as_deref() == Some(&content) {
        store.save_state(&state).map_err(|e| e.to_string())?;
        return Ok(false);
    }
    state.content_hash = Some(content);
    state.event_count = busy.len() as i64;
    store.replace(&state, &[], &busy).map_err(|e| format!("cache write failed: {e}"))?;
    Ok(true)
}

/// One sync of a Google account. A failure on one calendar doesn't stop the others,
/// and never touches what's cached.
pub fn sync(store: &mut Store, source_name: &str, api: &dyn Api, now: DateTime<Utc>) -> Outcome {
    let calendars = match calendar_list(api, source_name) {
        Ok(c) => c,
        Err(e) => return Outcome::Error(e),
    };
    if let Err(e) = store.set_calendars(source_name, &calendars) {
        return Outcome::Error(format!("cache write failed: {e}"));
    }
    let (mut changed, mut events, mut errors) = (false, 0, Vec::new());
    for cal in &calendars {
        let key = format!("{source_name}/{}", cal.id);
        let result = if cal.access == "freeBusyReader" { sync_free_busy(store, api, &key, cal, now) } else { sync_events(store, api, &key, cal, now) };
        match result {
            Ok(c) => changed |= c,
            Err(e) => errors.push(format!("{}: {e}", cal.name)),
        }
        events += store.state(&key).map(|s| s.event_count as usize).unwrap_or(0);
    }
    match (errors.is_empty(), changed) {
        (false, _) => Outcome::Error(errors.join("; ")),
        (true, true) => Outcome::Ok { events, skipped: 0 },
        (true, false) => Outcome::Unchanged,
    }
}

/// Syncs `account` over the network.
pub fn sync_account(store: &mut Store, source_name: &str, account: &str, now: DateTime<Utc>) -> Outcome {
    match access_token(account) {
        Ok(token) => {
            let api = Http { token, agent: agent() };
            // Changes first, then read: one worker per source, so they never race.
            flush(store, source_name, &api, now);
            sync(store, source_name, &api, now)
        }
        Err(e) => Outcome::Error(e),
    }
}

/// Sends `source`'s due changes now (e.g. `calm commit`), with its own sign-in.
pub fn flush_account(store: &mut Store, source_name: &str, account: &str, now: DateTime<Utc>) -> Result<Flushed, String> {
    let token = access_token(account)?;
    Ok(flush(store, source_name, &Http { token, agent: agent() }, now))
}

// --- What you see: the mirror with queued changes laid over it -----------------------------

/// Replaces the occurrences of every calendar with queued changes by those calendars' events
/// with the changes applied (re-expanded, so instances of series behave), each touched
/// occurrence marked with its change's state. `preview` off shows the server's version, with
/// only the changes needing attention marked.
pub fn overlay(store: &Store, stored: &mut Vec<super::store::Stored>, now: DateTime<Utc>, preview: bool) {
    let ops = store.ops().unwrap_or_default();
    let mut by_key: HashMap<String, Vec<&Op>> = HashMap::new();
    for op in &ops {
        by_key.entry(op.key()).or_default().push(op);
    }
    for (key, ops) in by_key {
        let shown: Vec<&Op> = if preview { ops.clone() } else { Vec::new() };
        let events = outbox::overlay_events(stored_events(store, &key), &shown);
        let components: Vec<Component> = events.values().filter_map(|e| component(e, None, false)).collect();
        let (occurrences, _) = ics::expand(&components, super::window(now));
        stored.retain(|s| s.source != key);
        for occ in occurrences {
            // Marked by the change to this occurrence, or to its whole series.
            let sync = ops
                .iter()
                .rev()
                .find(|o| o.event == occ.ref_id || o.event == occ.uid)
                .map(|o| o.state)
                .filter(|s| preview || matches!(s, State::Failed | State::Conflict));
            stored.push(super::store::Stored { source: key.clone(), occ, sync });
        }
    }
}

// --- Writing -------------------------------------------------------------------------------

/// What a flush did.
#[derive(Debug, Default, PartialEq)]
pub struct Flushed {
    pub sent: usize,
    /// Still queued for a retry (offline, rate limited, Google unwell).
    pub waiting: usize,
    /// Now `failed` or `conflict`: they need you.
    pub problems: Vec<String>,
}

/// How one change went.
enum Sent {
    /// Done: the server's event now (`None`: it no longer exists).
    Done(Option<Value>),
    /// Try again later.
    Retry(String),
    /// Needs you: rejected, or can't be sent as things stand.
    Failed(String),
    /// Changed elsewhere in a way that clashes: the server's values of those fields.
    Conflict(Value),
}

fn event_path(calendar: &str, event: Option<&str>) -> String {
    match event {
        Some(e) => format!("/calendars/{}/events/{}", url_encode(calendar), url_encode(e)),
        None => format!("/calendars/{}/events", url_encode(calendar)),
    }
}

/// What an error status means for a change.
fn status_outcome(status: u16, body: &Value) -> Sent {
    let message = body["error"]["message"].as_str().unwrap_or("request failed").to_string();
    match status {
        401 => Sent::Retry("Google sign-in needs refreshing".into()),
        403 if message.to_lowercase().contains("scope") || message.to_lowercase().contains("insufficient") => {
            Sent::Failed("calm can't edit yet: run `calm google login` again to allow editing".into())
        }
        403 => Sent::Failed(format!("Google refused: {message}")),
        400 => Sent::Failed(format!("Google rejected the change: {message}")),
        429 | 500..=599 => Sent::Retry(format!("Google {status}: {message}")),
        s => Sent::Failed(format!("Google {s}: {message}")),
    }
}

/// Sends one change: creates by our own ID (so a repeat is recognised), updates and deletes
/// against the version current right now, merged with what you based your change on.
fn send_op(api: &dyn Api, op: &Op) -> Sent {
    let notify = [("sendUpdates", op.notify.clone())];
    if op.kind == Kind::Create {
        let mut body = op.patch.clone();
        body["id"] = json!(op.event);
        return match api.send("POST", &event_path(&op.calendar, None), &notify, Some(&body), None) {
            Err(e) => Sent::Retry(e),
            Ok((200..=299, v)) => Sent::Done(Some(v)),
            // That ID exists: an earlier attempt landed and its answer was lost?
            Ok((409, _)) => match api.send("GET", &event_path(&op.calendar, Some(&op.event)), &[], None, None) {
                Ok((200..=299, theirs)) if merge(&json!({}), &op.patch, &theirs) == Merge::Send(json!({})) => Sent::Done(Some(theirs)),
                Ok((200..=299, _)) => Sent::Failed("an event with this ID already exists".into()),
                Ok((s, v)) => status_outcome(s, &v),
                Err(e) => Sent::Retry(e),
            },
            Ok((s, v)) => status_outcome(s, &v),
        };
    }
    // Updates and deletes: a few rounds, in case someone else writes between our read and ours.
    for _ in 0..3 {
        let path = event_path(&op.calendar, Some(&op.event));
        let theirs = match api.send("GET", &path, &[], None, None) {
            Err(e) => return Sent::Retry(e),
            Ok((200..=299, v)) if v["status"].as_str() != Some("cancelled") => v,
            Ok((404 | 410, _)) | Ok((200..=299, _)) => {
                return match op.kind {
                    Kind::Delete => Sent::Done(None),
                    _ => Sent::Conflict(json!({ "deleted": true })),
                };
            }
            Ok((s, v)) => return status_outcome(s, &v),
        };
        let etag = theirs["etag"].as_str().map(str::to_string);
        let request = match op.kind {
            Kind::Delete => {
                // Deleting something changed since you saw it would discard their change unseen.
                match merge(&op.base, &op.base, &theirs) {
                    Merge::Conflict(c) => return Sent::Conflict(c),
                    Merge::Send(_) => ("DELETE", None),
                }
            }
            _ => match merge(&op.base, &op.patch, &theirs) {
                Merge::Conflict(c) => return Sent::Conflict(c),
                Merge::Send(p) if p.as_object().is_none_or(Map::is_empty) => return Sent::Done(Some(theirs)),
                Merge::Send(p) => ("PATCH", Some(p)),
            },
        };
        match api.send(request.0, &path, &notify, request.1.as_ref(), etag.as_deref()) {
            Err(e) => return Sent::Retry(e),
            Ok((200..=299, v)) => return Sent::Done(if op.kind == Kind::Delete { None } else { Some(v) }),
            Ok((404 | 410, _)) if op.kind == Kind::Delete => return Sent::Done(None),
            // Changed between our read and our write: read again and re-merge.
            Ok((412, _)) => continue,
            Ok((s, v)) => return status_outcome(s, &v),
        }
    }
    Sent::Retry("the event kept changing while sending; will try again".into())
}

/// The stored events of `<source>/<calendar>`, by ID.
fn stored_events(store: &Store, key: &str) -> HashMap<String, Value> {
    store
        .raw_components(key)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|raw| serde_json::from_str::<Value>(&raw).ok())
        .filter_map(|v| Some((v["id"].as_str()?.to_string(), v)))
        .collect()
}

/// Rebuilds one calendar's mirror from `events`, retiring change `done` in the same transaction.
fn commit_mirror(store: &mut Store, key: &str, events: &HashMap<String, Value>, done: &Op, after: &Value, now: DateTime<Utc>) -> rusqlite::Result<()> {
    let mut state = store.state(key)?;
    state.name = key.to_string();
    let components: Vec<Component> = events.values().filter_map(|e| component(e, None, false)).collect();
    let (occurrences, _) = ics::expand(&components, super::window(now));
    state.event_count = components.len() as i64;
    let seq = done.seq;
    store.replace_then(&state, &components, &occurrences, |tx| {
        tx.execute("DELETE FROM outbox WHERE seq = ?1", [seq])?;
        outbox::record(tx, done, after)
    })
}

/// Sends `source`'s due changes, oldest first. Successes go straight into the mirror (and
/// out of the outbox) in one transaction each; anything else stays queued with its reason.
pub fn flush(store: &mut Store, source_name: &str, api: &dyn Api, now: DateTime<Utc>) -> Flushed {
    let mut out = Flushed::default();
    let ops: Vec<Op> = store.ops().unwrap_or_default().into_iter().filter(|o| o.source == source_name && o.state == State::Pending).collect();
    let mut blocked: Vec<String> = Vec::new();
    for op in ops {
        // Changes to one event go in order: never past an earlier one still waiting.
        let target = format!("{}/{}", op.calendar, op.event);
        if blocked.contains(&target) || op.next_attempt_at.is_some_and(|t| t > now.timestamp()) {
            blocked.push(target);
            out.waiting += 1;
            continue;
        }
        if !store.claim(op.seq).unwrap_or(false) {
            continue;
        }
        match send_op(api, &op) {
            Sent::Done(event) => {
                let key = op.key();
                let mut events = stored_events(store, &key);
                match &event {
                    Some(e) => {
                        events.insert(op.event.clone(), e.clone());
                    }
                    None => {
                        // A deleted instance is a cancelled exception; anything else just goes.
                        events.remove(&op.event);
                        if let (Some(series), Some(original)) = (&op.series, &op.original) {
                            events.insert(op.event.clone(), json!({ "id": op.event, "recurringEventId": series, "originalStartTime": original, "status": "cancelled" }));
                        }
                    }
                }
                match commit_mirror(store, &key, &events, &op, event.as_ref().unwrap_or(&Value::Null), now) {
                    Ok(()) => out.sent += 1,
                    Err(e) => {
                        let _ = store.set_op_state(op.seq, State::Pending, Some(&format!("cache write failed: {e}")), Some(now.timestamp() + 60));
                        out.waiting += 1;
                    }
                }
            }
            Sent::Retry(e) => {
                let next = now.timestamp() + super::backoff(op.attempts + 1, &op.op_id);
                let _ = store.set_op_state(op.seq, State::Pending, Some(&e), Some(next));
                blocked.push(target);
                out.waiting += 1;
            }
            Sent::Failed(e) => {
                let _ = store.set_op_state(op.seq, State::Failed, Some(&e), None);
                blocked.push(target);
                out.problems.push(format!("{}: {e}", op.describe()));
            }
            Sent::Conflict(theirs) => {
                let _ = store.set_conflict(op.seq, &theirs);
                blocked.push(target);
                out.problems.push(format!("{}: changed elsewhere", op.describe()));
            }
        }
    }
    out
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Stands in for Google: answers by path, records what was asked.
    struct Fake {
        events: RefCell<Vec<Result<Value, &'static str>>>,
        asked: RefCell<Vec<String>>,
        fail_list: bool,
    }

    impl Api for Fake {
        fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value, ApiError> {
            self.asked.borrow_mut().push(format!("{path}?{}", query.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")));
            if path == "/users/me/calendarList" {
                if self.fail_list {
                    return Err(ApiError::Other("Google 503: backend error".into()));
                }
                return Ok(json!({ "items": [
                    { "id": "me@example.com", "summary": "me@example.com", "primary": true, "accessRole": "owner", "backgroundColor": "#9fc6e7" },
                    { "id": "team@group", "summary": "Team", "accessRole": "freeBusyReader", "backgroundColor": "#f691b2" },
                    { "id": "old@group", "summary": "Old", "accessRole": "reader", "selected": false },
                ]}));
            }
            match self.events.borrow_mut().remove(0) {
                Ok(v) => Ok(v),
                Err("gone") => Err(ApiError::Gone),
                Err(e) => Err(ApiError::Other(e.into())),
            }
        }

        fn post(&self, path: &str, body: &Value) -> Result<Value, ApiError> {
            assert_eq!(path, "/freeBusy");
            let from = body["timeMin"].as_str().unwrap().to_string();
            // One busy hour at the start of each span asked for.
            let start = DateTime::parse_from_rfc3339(&from).unwrap().with_timezone(&Utc) + chrono::Duration::days(1);
            Ok(json!({ "calendars": { "team@group": { "busy": [
                { "start": start.to_rfc3339(), "end": (start + chrono::Duration::hours(1)).to_rfc3339() }
            ]}}}))
        }

        fn send(&self, _: &str, _: &str, _: &[(&str, String)], _: Option<&Value>, _: Option<&str>) -> Result<(u16, Value), String> {
            Err("the read-only fake doesn't write".into())
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-22T12:00:00Z").unwrap().with_timezone(&Utc)
    }

    fn first_page() -> Value {
        json!({
            "items": [
                // Weekly on Mondays at 9:00 London time, with one week moved and one cancelled.
                { "id": "standup", "status": "confirmed", "summary": "Standup",
                  "start": { "dateTime": "2026-11-02T09:00:00Z", "timeZone": "Europe/London" },
                  "end": { "dateTime": "2026-11-02T09:15:00Z", "timeZone": "Europe/London" },
                  "recurrence": ["RRULE:FREQ=WEEKLY;COUNT=6"] },
                { "id": "standup_20261109T090000Z", "status": "confirmed", "summary": "Standup (moved)", "recurringEventId": "standup",
                  "originalStartTime": { "dateTime": "2026-11-09T09:00:00Z", "timeZone": "Europe/London" },
                  "start": { "dateTime": "2026-11-09T11:00:00Z" }, "end": { "dateTime": "2026-11-09T11:15:00Z" } },
                { "id": "standup_20261116T090000Z", "status": "cancelled", "recurringEventId": "standup",
                  "originalStartTime": { "dateTime": "2026-11-16T09:00:00Z", "timeZone": "Europe/London" } },
                { "id": "holiday", "status": "confirmed", "summary": "Holiday", "start": { "date": "2026-11-27" }, "end": { "date": "2026-11-29" } },
                { "id": "lunch", "status": "tentative", "summary": "Lunch", "location": "Cafe",
                  "start": { "dateTime": "2026-11-24T12:30:00+08:00" }, "end": { "dateTime": "2026-11-24T13:30:00+08:00" } },
            ],
            "nextSyncToken": "t1",
        })
    }

    fn stored(store: &Store, key: &str) -> Vec<Occurrence> {
        let n = now();
        store
            .occurrences(n - chrono::Duration::days(400), n + chrono::Duration::days(400))
            .unwrap()
            .into_iter()
            .filter(|s| s.source == key)
            .map(|s| s.occ)
            .collect()
    }

    #[test]
    fn full_then_incremental_sync() {
        let mut store = Store::memory().unwrap();
        let fake = Fake { events: RefCell::new(vec![Ok(first_page())]), asked: RefCell::new(Vec::new()), fail_list: false };
        assert!(matches!(sync(&mut store, "Google", &fake, now()), Outcome::Ok { .. }));

        // The ticked calendars only; your own is named after the source.
        let cals = store.calendars("Google").unwrap();
        assert_eq!(cals.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Google", "Team"]);
        assert_eq!(cals[0].color.as_deref(), Some("#9fc6e7"));

        let mine = stored(&store, "Google/me@example.com");
        let titles: Vec<&str> = mine.iter().map(|o| o.summary.as_str()).collect();
        // Six standups: one moved, one cancelled, so four as scheduled plus the moved one.
        assert_eq!(titles.iter().filter(|t| **t == "Standup").count(), 4, "{titles:?}");
        assert_eq!(titles.iter().filter(|t| **t == "Standup (moved)").count(), 1);
        assert!(mine.iter().any(|o| o.summary == "Holiday" && o.all_day));
        assert!(mine.iter().any(|o| o.summary == "Lunch" && o.status.as_deref() == Some("TENTATIVE") && o.location.as_deref() == Some("Cafe")));
        // Free/busy: one busy block per span asked for, nothing else known.
        let team = stored(&store, "Google/team@group");
        assert!(!team.is_empty() && team.iter().all(|o| o.summary == "Busy"));

        // Incremental: only changes come back, with the token from last time.
        fake.events.borrow_mut().push(Ok(json!({
            "items": [
                { "id": "lunch", "status": "cancelled" },
                { "id": "review", "status": "confirmed", "summary": "Review",
                  "start": { "dateTime": "2026-11-25T15:00:00Z" }, "end": { "dateTime": "2026-11-25T16:00:00Z" } },
            ],
            "nextSyncToken": "t2",
        })));
        assert!(matches!(sync(&mut store, "Google", &fake, now()), Outcome::Ok { .. }));
        assert!(fake.asked.borrow().iter().any(|q| q.contains("syncToken=t1")));
        let mine = stored(&store, "Google/me@example.com");
        assert!(!mine.iter().any(|o| o.summary == "Lunch"));
        assert!(mine.iter().any(|o| o.summary == "Review"));
        assert_eq!(mine.iter().filter(|o| o.summary.starts_with("Standup")).count(), 5);

        // Nothing new: unchanged, cache untouched.
        fake.events.borrow_mut().push(Ok(json!({ "items": [], "nextSyncToken": "t3" })));
        assert_eq!(sync(&mut store, "Google", &fake, now()), Outcome::Unchanged);
        assert_eq!(stored(&store, "Google/me@example.com").len(), mine.len());

        // An expired token: start over with a full listing.
        fake.events.borrow_mut().push(Err("gone"));
        fake.events.borrow_mut().push(Ok(first_page()));
        assert!(matches!(sync(&mut store, "Google", &fake, now()), Outcome::Ok { .. }));
        let mine = stored(&store, "Google/me@example.com");
        assert!(mine.iter().any(|o| o.summary == "Lunch") && !mine.iter().any(|o| o.summary == "Review"));
    }

    #[test]
    fn failures_never_touch_the_cache() {
        let mut store = Store::memory().unwrap();
        let fake = Fake { events: RefCell::new(vec![Ok(first_page())]), asked: RefCell::new(Vec::new()), fail_list: false };
        sync(&mut store, "Google", &fake, now());
        let before = stored(&store, "Google/me@example.com").len();

        // An error listing events keeps what was cached.
        fake.events.borrow_mut().push(Err("Google 500: backend error"));
        assert!(matches!(sync(&mut store, "Google", &fake, now()), Outcome::Error(_)));
        assert_eq!(stored(&store, "Google/me@example.com").len(), before);

        // So does failing to list the calendars at all.
        let down = Fake { events: RefCell::new(Vec::new()), asked: RefCell::new(Vec::new()), fail_list: true };
        assert!(matches!(sync(&mut store, "Google", &down, now()), Outcome::Error(_)));
        assert_eq!(stored(&store, "Google/me@example.com").len(), before);
        assert_eq!(store.calendars("Google").unwrap().len(), 2);
    }
}

#[cfg(test)]
mod write_tests {
    use super::*;
    use crate::sync::outbox::same;
    use std::cell::{Cell, RefCell};

    /// A small stateful Google: one calendar, events with etags, instances of series, and
    /// knobs for the faults the write path has to survive.
    #[derive(Default)]
    pub(super) struct Server {
        events: RefCell<HashMap<String, Value>>,
        version: Cell<u64>,
        /// Apply the next write, then lose its answer (as a dropped connection would).
        lose_next: Cell<bool>,
        /// Answer the next write with this status instead.
        fail_next: Cell<Option<u16>>,
        writes: Cell<usize>,
    }

    impl Server {
        fn put(&self, mut e: Value) {
            self.version.set(self.version.get() + 1);
            e["etag"] = json!(format!("\"{}\"", self.version.get()));
            self.events.borrow_mut().insert(e["id"].as_str().unwrap().to_string(), e);
        }

        /// Someone else edits an event (a field patch), as if from their phone.
        fn edit_elsewhere(&self, id: &str, patch: Value) {
            let mut e = self.get(id).unwrap();
            outbox_apply(&mut e, &patch);
            self.put(e);
        }

        /// An event, or an instance of a series (`{series}_{20261124T090000Z}`) made on the fly.
        fn get(&self, id: &str) -> Option<Value> {
            if let Some(e) = self.events.borrow().get(id) {
                return Some(e.clone());
            }
            let (series, stamp) = id.rsplit_once('_')?;
            let s = self.events.borrow().get(series)?.clone();
            let start = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%SZ").ok()?.and_utc();
            let len = DateTime::parse_from_rfc3339(s["end"]["dateTime"].as_str()?).ok()? - DateTime::parse_from_rfc3339(s["start"]["dateTime"].as_str()?).ok()?;
            Some(json!({
                "id": id, "recurringEventId": series, "summary": s["summary"], "etag": s["etag"],
                "originalStartTime": { "dateTime": start.to_rfc3339() },
                "start": { "dateTime": start.to_rfc3339() }, "end": { "dateTime": (start + len).to_rfc3339() },
            }))
        }
    }

    fn outbox_apply(e: &mut Value, patch: &Value) {
        for (k, v) in patch.as_object().unwrap() {
            e[k] = v.clone();
        }
    }

    impl Api for Server {
        fn get(&self, path: &str, _: &[(&str, String)]) -> Result<Value, ApiError> {
            if path == "/users/me/calendarList" {
                return Ok(json!({ "items": [{ "id": "me", "summary": "me", "primary": true, "accessRole": "owner" }] }));
            }
            let items: Vec<Value> = self.events.borrow().values().cloned().collect();
            Ok(json!({ "items": items, "nextSyncToken": format!("t{}", self.version.get()) }))
        }

        fn post(&self, _: &str, _: &Value) -> Result<Value, ApiError> {
            Ok(json!({}))
        }

        fn send(&self, method: &str, path: &str, _: &[(&str, String)], body: Option<&Value>, if_match: Option<&str>) -> Result<(u16, Value), String> {
            let id = path.rsplit('/').next().unwrap().to_string();
            if method == "GET" {
                return Ok(self.get(&id).map_or((404, json!({})), |e| (200, e)));
            }
            self.writes.set(self.writes.get() + 1);
            if let Some(s) = self.fail_next.take() {
                return Ok((s, json!({ "error": { "message": if s == 403 { "Request had insufficient authentication scopes." } else { "nope" } } })));
            }
            let current = self.get(&id);
            if let (Some(want), Some(cur)) = (if_match, &current)
                && cur["etag"].as_str() != Some(want)
            {
                return Ok((412, json!({})));
            }
            let reply = match method {
                "POST" => {
                    let new = body.unwrap().clone();
                    let new_id = new["id"].as_str().unwrap().to_string();
                    if self.events.borrow().contains_key(&new_id) {
                        return Ok((409, json!({})));
                    }
                    self.put(new);
                    (200, self.get(&new_id).unwrap())
                }
                "PATCH" => {
                    let Some(mut e) = current else { return Ok((404, json!({}))) };
                    outbox_apply(&mut e, body.unwrap());
                    self.put(e);
                    (200, self.get(&id).unwrap())
                }
                "DELETE" => {
                    if current.is_none() {
                        return Ok((410, json!({})));
                    }
                    self.events.borrow_mut().remove(&id);
                    (204, Value::Null)
                }
                _ => unreachable!(),
            };
            if self.lose_next.take() {
                return Err("connection reset".into());
            }
            Ok(reply)
        }
    }

    fn at(s: &str) -> Value {
        json!({ "dateTime": s, "timeZone": "Europe/London" })
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-22T12:00:00Z").unwrap().with_timezone(&Utc)
    }

    pub(super) fn setup_pub() -> (Server, Store) {
        setup()
    }

    impl Server {
        pub(super) fn edit_elsewhere_pub(&self, id: &str, patch: Value) {
            self.edit_elsewhere(id, patch)
        }
        pub(super) fn lose_next_pub(&self) {
            self.lose_next.set(true)
        }
        pub(super) fn fail_next_pub(&self, s: u16) {
            self.fail_next.set(Some(s))
        }
        pub(super) fn get_pub(&self, id: &str) -> Option<Value> {
            self.get(id)
        }
    }

    /// A server with a standup, and a store that has synced it.
    fn setup() -> (Server, Store) {
        let server = Server::default();
        server.put(json!({ "id": "standup", "status": "confirmed", "summary": "Standup", "start": at("2026-11-24T09:00:00Z"), "end": at("2026-11-24T09:15:00Z") }));
        let mut store = Store::memory().unwrap();
        sync(&mut store, "Google", &server, now());
        (server, store)
    }

    fn mv(start: &str, end: &str) -> Op {
        Op::new(
            "Google",
            "me",
            "standup",
            Kind::Update,
            json!({ "start": at("2026-11-24T09:00:00Z"), "end": at("2026-11-24T09:15:00Z") }),
            json!({ "start": at(start), "end": at(end) }),
            outbox::Origin::App,
        )
    }

    fn mirrored(store: &Store, id: &str) -> Option<Value> {
        stored_events(store, "Google/me").get(id).cloned()
    }

    #[test]
    fn a_move_lands_in_the_server_and_the_mirror() {
        let (server, mut store) = setup();
        store.queue(mv("2026-11-24T10:00:00Z", "2026-11-24T10:15:00Z")).unwrap();
        assert_eq!(flush(&mut store, "Google", &server, now()), Flushed { sent: 1, ..Default::default() });
        assert!(same(&server.get("standup").unwrap()["start"], &at("2026-11-24T10:00:00Z")));
        assert!(same(&mirrored(&store, "standup").unwrap()["start"], &at("2026-11-24T10:00:00Z")));
        assert!(store.ops().unwrap().is_empty());
    }

    #[test]
    fn a_lost_response_is_not_sent_twice() {
        let (server, mut store) = setup();
        store.queue(mv("2026-11-24T10:00:00Z", "2026-11-24T10:15:00Z")).unwrap();
        server.lose_next.set(true);
        // Applied, but we never heard: still queued.
        let first = flush(&mut store, "Google", &server, now());
        assert_eq!((first.sent, first.waiting), (0, 1));
        // Next time the server already has it: done, without writing again.
        let later = now() + chrono::Duration::hours(2);
        assert_eq!(flush(&mut store, "Google", &server, later).sent, 1);
        assert_eq!(server.writes.get(), 1);
        assert!(store.ops().unwrap().is_empty());
    }

    #[test]
    fn different_fields_merge_and_the_same_field_conflicts() {
        let (server, mut store) = setup();
        // They rename it while we move it: both stand.
        store.queue(mv("2026-11-24T10:00:00Z", "2026-11-24T10:15:00Z")).unwrap();
        server.edit_elsewhere("standup", json!({ "summary": "Daily" }));
        flush(&mut store, "Google", &server, now());
        let e = server.get("standup").unwrap();
        assert_eq!(e["summary"], "Daily");
        assert!(same(&e["start"], &at("2026-11-24T10:00:00Z")));

        // They move it too: a conflict, and nothing of ours is written.
        let mut op = mv("2026-11-24T11:00:00Z", "2026-11-24T11:15:00Z");
        op.base = json!({ "start": at("2026-11-24T10:00:00Z"), "end": at("2026-11-24T10:15:00Z") });
        let seq = store.queue(op).unwrap().unwrap();
        server.edit_elsewhere("standup", json!({ "start": at("2026-11-24T15:00:00Z"), "end": at("2026-11-24T15:15:00Z") }));
        let writes = server.writes.get();
        let r = flush(&mut store, "Google", &server, now());
        assert_eq!(r.problems.len(), 1);
        assert_eq!(server.writes.get(), writes);
        let op = store.op(seq).unwrap().unwrap();
        assert_eq!(op.state, State::Conflict);
        assert!(same(&op.theirs.unwrap()["start"], &at("2026-11-24T15:00:00Z")));
    }

    #[test]
    fn a_create_whose_answer_is_lost_happens_once() {
        let (server, mut store) = setup();
        let op = Op::new("Google", "me", &outbox::new_id(), Kind::Create, json!({}), json!({ "summary": "Focus", "start": at("2026-11-25T09:00:00Z"), "end": at("2026-11-25T11:00:00Z") }), outbox::Origin::App);
        let id = op.event.clone();
        store.queue(op).unwrap();
        server.lose_next.set(true);
        flush(&mut store, "Google", &server, now());
        assert_eq!(flush(&mut store, "Google", &server, now() + chrono::Duration::hours(2)).sent, 1);
        assert_eq!(server.events.borrow().len(), 2);
        assert_eq!(mirrored(&store, &id).unwrap()["summary"], "Focus");
    }

    #[test]
    fn deletes_and_failures() {
        let (server, mut store) = setup();
        // Deleted elsewhere already: our delete is simply done.
        server.events.borrow_mut().remove("standup");
        store.queue(Op::new("Google", "me", "standup", Kind::Delete, json!({ "summary": "Standup" }), json!({}), outbox::Origin::App)).unwrap();
        assert_eq!(flush(&mut store, "Google", &server, now()).sent, 1);
        assert!(mirrored(&store, "standup").is_none());

        // No write access yet: failed, kept, with a way forward.
        let (server, mut store) = setup();
        let seq = store.queue(mv("2026-11-24T10:00:00Z", "2026-11-24T10:15:00Z")).unwrap().unwrap();
        server.fail_next.set(Some(403));
        flush(&mut store, "Google", &server, now());
        let op = store.op(seq).unwrap().unwrap();
        assert_eq!(op.state, State::Failed);
        assert!(op.last_error.unwrap().contains("calm google login"));

        // Google unwell: queued for later, with backoff.
        let (server, mut store) = setup();
        let seq = store.queue(mv("2026-11-24T10:00:00Z", "2026-11-24T10:15:00Z")).unwrap().unwrap();
        server.fail_next.set(Some(503));
        let r = flush(&mut store, "Google", &server, now());
        assert_eq!(r.waiting, 1);
        let op = store.op(seq).unwrap().unwrap();
        assert_eq!(op.state, State::Pending);
        assert!(op.next_attempt_at.unwrap() > now().timestamp());
    }

    #[test]
    fn moving_one_instance_of_a_series() {
        let server = Server::default();
        server.put(json!({ "id": "weekly", "status": "confirmed", "summary": "Review", "start": at("2026-11-02T14:00:00Z"), "end": at("2026-11-02T15:00:00Z"), "recurrence": ["RRULE:FREQ=WEEKLY;COUNT=4"] }));
        let mut store = Store::memory().unwrap();
        sync(&mut store, "Google", &server, now());
        let instance = "weekly_20261116T140000Z";
        let mut op = Op::new(
            "Google",
            "me",
            instance,
            Kind::Update,
            json!({ "start": { "dateTime": "2026-11-16T14:00:00+00:00" }, "end": { "dateTime": "2026-11-16T15:00:00+00:00" } }),
            json!({ "start": at("2026-11-16T16:00:00Z"), "end": at("2026-11-16T17:00:00Z") }),
            outbox::Origin::App,
        );
        op.series = Some("weekly".into());
        op.original = Some(json!({ "dateTime": "2026-11-16T14:00:00Z" }));
        store.queue(op).unwrap();
        assert_eq!(flush(&mut store, "Google", &server, now()).sent, 1);
        // The mirror now holds an exception: that week at 16:00, the others untouched.
        let from = now() - chrono::Duration::days(60);
        let occ = store.occurrences(from, now() + chrono::Duration::days(60)).unwrap();
        let starts: Vec<String> = occ.iter().filter(|s| s.source == "Google/me").map(|s| s.occ.start.format("%d %H").to_string()).collect();
        assert_eq!(starts, ["02 14", "09 14", "16 16", "23 14"]);
    }
}

#[cfg(test)]
mod convergence {
    use super::*;

    /// Random interleavings of local edits, remote edits and network faults. Once the faults
    /// stop and everything flushes: the mirror matches the server, and every change has either
    /// been sent or is visibly waiting on you (a conflict or failure); none is stuck mid-send.
    /// (That conflicts and failures are kept, never dropped, is covered by `write_tests`.)
    #[test]
    fn edits_and_faults_converge() {
        for seed in 1..=40u64 {
            let mut rng = seed.wrapping_mul(0x9E3779B97F4A7C15);
            let mut next = |n: u64| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                rng % n
            };
            let (server, mut store) = write_tests_setup();
            let mut now = DateTime::parse_from_rfc3339("2026-11-22T12:00:00Z").unwrap().with_timezone(&Utc);
            for _ in 0..30 {
                match next(6) {
                    // You move or rename it, based on what you see.
                    0 | 1 => {
                        let seen = outbox::overlay_events(stored_events(&store, "Google/me"), &store.ops().unwrap().iter().collect::<Vec<_>>())["standup"].clone();
                        let op = if next(2) == 0 {
                            let h = 8 + next(10);
                            let patch = json!({ "start": { "dateTime": format!("2026-11-24T{h:02}:00:00Z") }, "end": { "dateTime": format!("2026-11-24T{h:02}:15:00Z") } });
                            Op::new("Google", "me", "standup", Kind::Update, json!({ "start": seen["start"], "end": seen["end"] }), patch, outbox::Origin::App)
                        } else {
                            Op::new("Google", "me", "standup", Kind::Update, json!({ "summary": seen["summary"] }), json!({ "summary": format!("Mine {}", next(100)) }), outbox::Origin::App)
                        };
                        store.queue(op).unwrap();
                    }
                    // Someone else edits it.
                    2 => {
                        if next(2) == 0 {
                            server.edit_elsewhere_pub("standup", json!({ "summary": format!("Theirs {}", next(100)) }));
                        } else {
                            let h = 8 + next(10);
                            server.edit_elsewhere_pub("standup", json!({ "start": { "dateTime": format!("2026-11-24T{h:02}:00:00Z") }, "end": { "dateTime": format!("2026-11-24T{h:02}:15:00Z") } }));
                        }
                    }
                    // The network misbehaves on the next write.
                    3 => server.lose_next_pub(),
                    4 => server.fail_next_pub(503),
                    // Time passes; the worker flushes and reads.
                    _ => {
                        now += chrono::Duration::minutes(next(120) as i64);
                        flush(&mut store, "Google", &server, now);
                        sync(&mut store, "Google", &server, now);
                    }
                }
            }
            // Calm returns: no more faults, and time enough for every retry.
            for _ in 0..12 {
                now += chrono::Duration::hours(2);
                flush(&mut store, "Google", &server, now);
                sync(&mut store, "Google", &server, now);
            }
            let ops = store.ops().unwrap();
            assert!(ops.iter().all(|o| matches!(o.state, State::Conflict | State::Failed)), "seed {seed}: left in flight: {ops:?}");
            // The mirror is the server's version.
            let server_event = server.get_pub("standup").unwrap();
            let mirror = stored_events(&store, "Google/me")["standup"].clone();
            assert_eq!(server_event["summary"], mirror["summary"], "seed {seed}");
            assert!(outbox::same(&server_event["start"], &mirror["start"]), "seed {seed}");
        }
    }

    fn write_tests_setup() -> (super::write_tests::Server, Store) {
        super::write_tests::setup_pub()
    }
}
