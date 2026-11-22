//! Which calendars to sync: `source` nodes in calm.kdl, or the older `~/.config/calm/sources`:
//!
//! ```toml
//! [[source]]
//! name = "Personal"
//! url_file = "~/.config/calm/secrets/personal.url"   # or url = "…", or path = "….ics"
//! calendar = "Personal"
//! refresh = "15m"
//! ```

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

#[derive(Deserialize)]
struct File {
    #[serde(default, rename = "source")]
    sources: Vec<Raw>,
}

#[derive(Deserialize)]
struct Raw {
    name: String,
    url: Option<String>,
    url_file: Option<String>,
    path: Option<String>,
    calendar: Option<String>,
    refresh: Option<String>,
}

#[derive(Clone)]
pub enum Location {
    Url(String),
    File(PathBuf),
}

#[derive(Clone)]
pub struct Source {
    pub name: String,
    pub location: Location,
    /// Index into the app's calendars.
    pub calendar: usize,
    pub refresh: Duration,
    /// Changes when the source's definition changes, forcing a full re-fetch.
    pub config_hash: String,
}

pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("calm"))
}

fn expand_tilde(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(p),
    }
}

/// "15m", "1h", "90s"; at least a minute.
pub fn parse_refresh(s: &str) -> Option<Duration> {
    parse_duration(s).map(|d| d.max(Duration::from_secs(60)))
}

/// "45m", "1h", "90s".
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: u64 = num.parse().ok()?;
    let secs = match unit.trim() {
        "s" => n,
        "m" | "min" => n * 60,
        "h" => n * 3600,
        _ => return None,
    };
    Some(Duration::from_secs(secs))
}

/// Sources from calm.kdl when it declares any, else from the older `sources` TOML file.
fn raws(declared: Option<&[crate::viewconf::SourceDecl]>, problems: &mut Vec<String>) -> Vec<Raw> {
    match declared {
        Some(decls) => decls
            .iter()
            .map(|d| Raw {
                name: d.name.clone(),
                url: d.url.clone(),
                url_file: d.url_file.clone(),
                path: d.path.clone(),
                calendar: d.calendar.clone(),
                refresh: d.refresh.clone(),
            })
            .collect(),
        None => {
            let Some(path) = config_dir().map(|d| d.join("sources")) else { return Vec::new() };
            let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
            match toml::from_str::<File>(&text) {
                Ok(f) => f.sources,
                Err(e) => {
                    problems.push(format!("{}: {}", path.display(), e.message()));
                    Vec::new()
                }
            }
        }
    }
}

/// The calendar each source asks for: its `calendar=`, else its own name.
pub fn wanted_calendars(declared: Option<&[crate::viewconf::SourceDecl]>) -> Vec<String> {
    raws(declared, &mut Vec::new()).into_iter().map(|r| r.calendar.unwrap_or(r.name)).collect()
}

/// Configured sources, plus human-readable problems (never containing secrets).
/// `calendars` are the resolved calendar names; each source maps to one by name.
pub fn load(declared: Option<&[crate::viewconf::SourceDecl]>, calendars: &[String]) -> (Vec<Source>, Vec<String>) {
    let mut problems = Vec::new();
    let raws = raws(declared, &mut problems);
    let mut out = Vec::new();
    for raw in raws {
        let location = match (&raw.url, &raw.url_file, &raw.path) {
            (Some(u), None, None) => Location::Url(u.clone()),
            (None, Some(f), None) => {
                let file = expand_tilde(f);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = std::fs::metadata(&file)
                        && meta.permissions().mode() & 0o077 != 0
                    {
                        problems.push(format!("{}: secret file is readable by others; chmod 600 it", raw.name));
                    }
                }
                match std::fs::read_to_string(&file) {
                    Ok(u) => Location::Url(u.trim().to_string()),
                    Err(e) => {
                        problems.push(format!("{}: can't read url_file ({e})", raw.name));
                        continue;
                    }
                }
            }
            (None, None, Some(p)) => Location::File(expand_tilde(p)),
            _ => {
                problems.push(format!("{}: give exactly one of url, url_file or path", raw.name));
                continue;
            }
        };
        if let Location::Url(u) = &location
            && !(u.starts_with("https://") || u.starts_with("http://") || u.starts_with("webcal://"))
        {
            problems.push(format!("{}: url must be http(s) or webcal", raw.name));
            continue;
        }
        let want = raw.calendar.as_deref().unwrap_or(&raw.name);
        let calendar = match calendars.iter().position(|n| n.eq_ignore_ascii_case(want)) {
            Some(c) => c,
            None => {
                problems.push(format!("{}: no calendar named `{want}`; using `{}`", raw.name, calendars.first().map_or("", String::as_str)));
                0
            }
        };
        let refresh = raw.refresh.as_deref().and_then(parse_refresh).unwrap_or(Duration::from_secs(15 * 60));
        let key = match &location {
            Location::Url(u) => format!("url:{:016x}", super::hash(u)),
            Location::File(p) => format!("path:{}", p.display()),
        };
        out.push(Source { config_hash: format!("{key}|{calendar}"), name: raw.name, location, calendar, refresh });
    }
    (out, problems)
}
