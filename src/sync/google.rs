//! Google Calendar, read-only, through its API.
//!
//! Sign-in is OAuth for installed apps: `calm google login` opens Google's consent page
//! and catches the answer on a loopback port, with PKCE. The client ID is your own
//! (`secrets/google-client.json`, downloaded from Google Cloud); the refresh token goes
//! to `secrets/google-<account>.json` (mode 600). The only scope is
//! `calendar.readonly`.
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
use super::store::{RemoteCalendar, Store};
use super::{Outcome, config_dir};

const SCOPE: &str = "https://www.googleapis.com/auth/calendar.readonly";
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
        organizer: people(&e["organizer"]),
        attendees: e["attendees"].as_array().into_iter().flatten().filter_map(people).collect(),
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
        Ok(token) => sync(store, source_name, &Http { token, agent: agent() }, now),
        Err(e) => Outcome::Error(e),
    }
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
