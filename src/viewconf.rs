//! View configuration: KDL in, `Spec`s out.
//!
//! The built-in views (`assets/views.kdl`) load first; `~/.config/calm/calm.kdl`
//! and anything it includes merge on top by view name. Parsing is strict —
//! unknown nodes and properties are errors with a location and a suggestion —
//! so mistakes (by people or agents) surface immediately rather than being
//! silently ignored. A config with errors is not applied; the last good one stays.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};

use crate::color::{Compose, Measure};
use crate::keys::{Binding, Chord, Command, Ctx, Keymap};
use crate::spec::*;

pub const BUILTIN: &str = include_str!("../assets/views.kdl");
pub const BUILTIN_KEYS: &str = include_str!("../assets/keys.kdl");

#[derive(Clone, Debug)]
pub struct Diag {
    pub file: String,
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl fmt::Display for Diag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}: {}", self.file, self.line, self.col, self.message)
    }
}

impl Diag {
    pub fn json(&self) -> String {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
        format!(r#"{{"file":"{}","line":{},"column":{},"message":"{}"}}"#, esc(&self.file), self.line, self.col, esc(&self.message))
    }
}

/// `overlay "Travel" key="w" { rail "travel"; zones "visible" }`: settings that go on together,
/// and off again, over whatever view you're in.
#[derive(Clone, Debug)]
pub struct Overlay {
    pub name: String,
    pub set: Vec<(String, String)>,
}

/// The effective configuration.
#[derive(Clone)]
pub struct Config {
    pub views: Vec<Spec>,
    pub settings: Settings,
    /// Each view's KDL, for `calm config dump`.
    pub view_text: Vec<String>,
    templates: Vec<String>,
    /// Every file read, for hot reload.
    pub files: Vec<PathBuf>,
    /// Views defined before the app's own edits (built-ins and calm.kdl), and views the app has edited.
    pub base_names: Vec<String>,
    pub generated_names: Vec<String>,
    /// Keys outside any view: the built-ins with yours on top.
    pub keys: Keymap,
    /// Named bundles of settings, toggled on whichever view you're in.
    pub overlays: Vec<Overlay>,
    /// Worth knowing, but not wrong: the config still applies.
    pub warnings: Vec<Diag>,
}

pub fn user_path() -> Option<PathBuf> {
    crate::sync::config_dir().map(|d| d.join("calm.kdl"))
}

/// Views edited in the app. calm owns this file; it loads after calm.kdl.
pub fn generated_path() -> Option<PathBuf> {
    crate::sync::config_dir().map(|d| d.join("views.generated.kdl"))
}

fn generated_view_names(path: &Path) -> Vec<String> {
    let Ok(doc) = std::fs::read_to_string(path).unwrap_or_default().parse::<KdlDocument>() else { return Vec::new() };
    doc.nodes()
        .iter()
        .filter(|n| n.name().value() == "view")
        .filter_map(|n| n.entries().first()?.value().as_string().map(str::to_string))
        .collect()
}

/// Built-ins plus the user's config. On errors, the built-ins alone (and the diagnostics).
pub fn load() -> (Config, Vec<Diag>) {
    let mut loader = Loader::default();
    loader.file("<built-in views.kdl>", BUILTIN, None);
    loader.file("<built-in keys.kdl>", BUILTIN_KEYS, None);
    assert!(loader.diags.is_empty(), "built-in views are invalid: {:?}", loader.diags);
    let builtin = loader.finish();
    let user = user_path().filter(|p| p.exists());
    let generated = generated_path().filter(|p| p.exists());
    if user.is_none() && generated.is_none() {
        return (builtin, Vec::new());
    }

    let mut loader = Loader::default();
    loader.file("<built-in views.kdl>", BUILTIN, None);
    loader.file("<built-in keys.kdl>", BUILTIN_KEYS, None);
    if let Some(p) = &user {
        loader.path(p, 0);
    }
    // Views edited in the app: loaded last, so they win; your calm.kdl is never rewritten.
    let base_names: Vec<String> = loader.views.iter().map(|(v, _)| v.name.clone()).collect();
    if let Some(p) = &generated {
        loader.path(p, 0);
    }
    loader.check_refs();
    let diags = std::mem::take(&mut loader.diags);
    let files = loader.files.clone();
    if diags.is_empty() {
        let mut config = loader.finish();
        config.base_names = base_names;
        config.generated_names = generated.as_deref().map(generated_view_names).unwrap_or_default();
        (config, diags)
    } else {
        (Config { files, ..builtin }, diags)
    }
}

impl Config {
    /// The effective config as KDL: templates, then views in order.
    pub fn dump(&self) -> String {
        let mut out = String::from("// Effective calm configuration (built-ins merged with your calm.kdl).\n\n");
        for t in self.settings.text.iter().chain(&self.templates).chain(&self.view_text) {
            out += t.trim();
            out += "\n\n";
        }
        out
    }
}

/// Non-view settings from calm.kdl. `None` means "not set here" (calm falls back to the
/// older `config` and `sources` files, then to defaults).
#[derive(Clone, Default, PartialEq)]
pub struct Settings {
    pub zones: Option<ZoneDecl>,
    pub sources: Option<Vec<SourceDecl>>,
    pub calendars: Option<Vec<CalendarDecl>>,
    pub energy: Option<crate::energy::EnergyModel>,
    /// Where you are (latitude, longitude), for daylight.
    pub location: Option<(f32, f32)>,
    /// `auto` (send changes after a short pause) or `review` (stage them until committed).
    pub writes: Option<String>,
    /// The calendar new events go to, by name.
    pub default_calendar: Option<String>,
    pub opacity: Option<f32>,
    pub blur: bool,
    /// `light`, `dark` or `system`.
    pub theme: Option<String>,
    /// The themes `system` switches between, and `colors` blocks.
    pub theme_light: Option<String>,
    pub theme_dark: Option<String>,
    pub themes: Vec<crate::theme::ThemeDecl>,
    /// `notify …`: reminder lead times and quiet hours.
    pub notify: Option<crate::notify::Rules>,
    /// Each settings node's KDL, for `calm config dump`.
    text: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct CalendarDecl {
    /// How sources refer to it (for Google, the calendar's name there).
    pub name: String,
    /// What the chip says, when the name is long or an address.
    pub label: Option<String>,
    pub color: Option<String>,
    pub demand: Option<f32>,
    /// `notify=#false`: no reminders for its events.
    pub quiet: bool,
    /// Listed in the top-right menu rather than as a chip of its own.
    pub secondary: bool,
}

/// (zone, short label, long name)
pub type ZoneLabel = (String, Option<String>, Option<String>);

#[derive(Clone, Default, PartialEq, Debug)]
pub struct ZoneDecl {
    pub zones: Vec<String>,
    pub labels: Vec<ZoneLabel>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct SourceDecl {
    pub name: String,
    pub url: Option<String>,
    pub url_file: Option<String>,
    pub path: Option<String>,
    /// A Google account, synced through the API (see `sync::google`).
    pub google: Option<String>,
    pub calendar: Option<String>,
    pub refresh: Option<String>,
    /// Overrides `sync writes=` for this source.
    pub writes: Option<String>,
}

#[derive(Default)]
struct Loader {
    diags: Vec<Diag>,
    templates: HashMap<String, (Vec<Param>, String)>,
    template_order: Vec<String>,
    views: Vec<(Spec, String)>,
    hidden: Vec<String>,
    order: Option<Vec<String>>,
    files: Vec<PathBuf>,
    settings: Settings,
    /// Sources declared in this load (a user list replaces nothing built in).
    sources: Vec<SourceDecl>,
    calendars: Vec<CalendarDecl>,
    /// Theme names used by `theme` (file, line, col, name), checked in `check_refs`.
    theme_refs: Vec<(String, usize, usize, String)>,
    /// `defaults "View" param=value …` nodes (file, line, col, node), applied in `check_refs`.
    param_defaults: Vec<(String, usize, usize, KdlNode)>,
    /// Each source's calendar (file, line, col, name, given explicitly), checked in `check_refs`.
    cal_refs: Vec<(String, usize, usize, String, bool)>,
    keys: Keymap,
    /// Each `keys` node's KDL, for `calm config dump`.
    keys_text: Vec<String>,
    /// Views' own chords (file, line, col, view, chord), checked against the global keys.
    view_chords: Vec<(String, usize, usize, String, Chord)>,
    warnings: Vec<Diag>,
    overlays: Vec<Overlay>,
    /// Overlays named by `overlay NAME` commands (file, line, col, name), checked in `check_refs`.
    overlay_refs: Vec<(String, usize, usize, String)>,
    /// Views named by `split` (file, line, col, name), checked in `check_refs`.
    split_refs: Vec<(String, usize, usize, String)>,
}

/// Where we are, for diagnostics.
struct At<'a> {
    file: &'a str,
    src: &'a str,
}

impl At<'_> {
    fn pos(&self, offset: usize) -> (usize, usize) {
        let before = &self.src[..offset.min(self.src.len())];
        let line = before.matches('\n').count() + 1;
        let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        (line, col)
    }
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + (ca != *cb) as usize).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

fn suggest(got: &str, allowed: &[&str]) -> String {
    let best = allowed.iter().min_by_key(|a| levenshtein(got, a));
    match best {
        Some(b) if levenshtein(got, b) <= 3 => format!("; did you mean `{b}`?"),
        _ => format!(" (expected one of: {})", allowed.join(", ")),
    }
}

impl Loader {
    fn err(&mut self, at: &At, offset: usize, message: impl Into<String>) {
        let (line, col) = at.pos(offset);
        self.diags.push(Diag { file: at.file.to_string(), line, col, message: message.into() });
    }

    fn path(&mut self, path: &Path, depth: usize) {
        let name = path.display().to_string();
        if depth > 8 || self.files.iter().any(|f| f == path) {
            self.diags.push(Diag { file: name, line: 1, col: 1, message: "include cycle or nesting too deep".into() });
            return;
        }
        self.files.push(path.to_path_buf());
        match std::fs::read_to_string(path) {
            Ok(src) => self.file(&name, &src, Some((path.parent().unwrap_or(Path::new(".")), depth))),
            Err(e) => self.diags.push(Diag { file: name, line: 1, col: 1, message: format!("can't read: {e}") }),
        }
    }

    fn file(&mut self, file: &str, src: &str, dir: Option<(&Path, usize)>) {
        let at = At { file, src };
        let doc: KdlDocument = match KdlDocument::parse(src) {
            Ok(d) => d,
            Err(e) => {
                for d in &e.diagnostics {
                    let msg = [d.message.as_deref(), d.label.as_deref(), d.help.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(": ");
                    self.err(&at, d.span.offset(), format!("syntax: {}", if msg.is_empty() { "invalid KDL".into() } else { msg }));
                }
                return;
            }
        };
        for node in doc.nodes() {
            let name = node.name().value();
            match name {
                "view" => {
                    if let Some(spec) = self.view(&at, node) {
                        let text = node.to_string();
                        match self.views.iter_mut().find(|(v, _)| v.name == spec.name) {
                            Some(slot) => *slot = (spec, text),
                            None => self.views.push((spec, text)),
                        }
                    }
                }
                "params" => {
                    let Some(tname) = self.arg_str(&at, node, 0, "a template name") else { continue };
                    let params = self.params_block(&at, node);
                    if !self.template_order.contains(&tname) {
                        self.template_order.push(tname.clone());
                    }
                    self.templates.insert(tname, (params, node.to_string()));
                }
                "include" => {
                    let Some(pattern) = self.arg_str(&at, node, 0, "a path or glob") else { continue };
                    let Some((dir, depth)) = dir else { continue };
                    for path in expand_glob(dir, &pattern) {
                        self.path(&path, depth + 1);
                    }
                }
                "hide" => {
                    for e in node.entries().iter().filter(|e| e.name().is_none()) {
                        if let Some(s) = e.value().as_string() {
                            self.hidden.push(s.to_string());
                        }
                    }
                }
                "zones" => self.zones_node(&at, node),
                "source" => self.source_node(&at, node),
                "calendar" => self.calendar_node(&at, node),
                "energy" => self.energy_node(&at, node),
                "location" => {
                    self.check_props(&at, node, &["lat", "lon"]);
                    let (lat, lon) = (self.num_prop(&at, node, "lat"), self.num_prop(&at, node, "lon"));
                    match (lat, lon) {
                        (Some(lat), Some(lon)) if lat.abs() <= 90. && lon.abs() <= 180. => {
                            self.settings.location = Some((lat as f32, lon as f32));
                            self.settings.text.push(node.to_string());
                        }
                        _ => self.err(&at, node.span().offset(), "`location` needs lat=DEGREES (−90…90) and lon=DEGREES (−180…180), north and east positive"),
                    }
                }
                "defaults" => {
                    // Applied once every view has loaded (see `check_refs`).
                    let (line, col) = at.pos(node.span().offset());
                    self.param_defaults.push((at.file.to_string(), line, col, node.clone()));
                    self.settings.text.push(node.to_string());
                }
                "window" => {
                    self.check_props(&at, node, &["opacity", "blur"]);
                    if let Some(o) = self.num_prop(&at, node, "opacity") {
                        if (0.2..=1.).contains(&o) {
                            self.settings.opacity = Some(o as f32);
                        } else {
                            self.err(&at, node.span().offset(), "`opacity` should be between 0.2 and 1");
                        }
                    }
                    match Self::prop(node, "blur").map(|e| e.value()) {
                        Some(KdlValue::Bool(b)) => self.settings.blur = *b,
                        Some(v) => self.err(&at, node.span().offset(), format!("`blur` should be #true or #false, not {v}")),
                        None => {}
                    }
                    self.settings.text.push(node.to_string());
                }
                "theme" => {
                    // `theme "system"`, `theme "Paper"`, `theme light="Paper" dark="Night"`.
                    self.check_props(&at, node, &["light", "dark"]);
                    let (line, col) = at.pos(node.span().offset());
                    let name = |this: &mut Self, v: Option<String>| {
                        if let Some(v) = &v {
                            this.theme_refs.push((at.file.to_string(), line, col, v.clone()));
                        }
                        v
                    };
                    let arg = Self::arg(node, 0).and_then(|e| e.value().as_string()).map(str::to_string);
                    if let Some(t) = name(self, arg) {
                        self.settings.theme = Some(t);
                    }
                    let light = self.str_prop(&at, node, "light");
                    if let Some(t) = name(self, light) {
                        self.settings.theme_light = Some(t);
                    }
                    let dark = self.str_prop(&at, node, "dark");
                    if let Some(t) = name(self, dark) {
                        self.settings.theme_dark = Some(t);
                    }
                    self.settings.text.push(node.to_string());
                }
                "colors" => self.colors_node(&at, node),
                "sync" => {
                    // `sync writes="auto"|"review"`: whether your changes send by themselves.
                    self.check_props(&at, node, &["writes"]);
                    if let Some(w) = self.str_prop(&at, node, "writes")
                        && self.choice(&at, node.span().offset(), "writes", &w, &[("auto", ()), ("review", ())]).is_some()
                    {
                        self.settings.writes = Some(w);
                        self.settings.text.push(node.to_string());
                    }
                }
                // Travel needs no switch any more (the `zone` rail is the opt-in); still accepted.
                "travel" => {}
                "default-calendar" => {
                    self.check_props(&at, node, &[]);
                    if let Some(c) = self.arg_str(&at, node, 0, "a calendar name") {
                        self.settings.default_calendar = Some(c);
                        self.settings.text.push(node.to_string());
                    }
                }
                "keys" => self.keys_node(&at, node),
                "notify" => self.notify_node(&at, node),
                "overlay" => self.overlay_node(&at, node),
                "order" => {
                    self.order = Some(node.entries().iter().filter_map(|e| e.value().as_string().map(str::to_string)).collect());
                }
                other => {
                    let hint = suggest(other, &["view", "params", "include", "hide", "order", "keys", "overlay", "notify", "defaults", "zones", "source", "calendar", "energy", "location", "window", "theme", "colors", "sync", "default-calendar"]);
                    self.err(&at, node.span().offset(), format!("unknown top-level node `{other}`{hint}"));
                }
            }
        }
    }

    /// `keys "create" { bind "h" "left" "step left"; unbind "f" }`: the context is optional
    /// (normal keys).
    fn keys_node(&mut self, at: &At, node: &KdlNode) {
        self.check_props(at, node, &[]);
        let ctx = match Self::arg(node, 0).map(|e| e.value()) {
            None => Ctx::Normal,
            Some(KdlValue::String(c)) => {
                let Some(ctx) = self.choice(at, node.span().offset(), "keys context", c, Ctx::NAMES) else { return };
                ctx
            }
            Some(v) => return self.err(at, node.span().offset(), format!("`keys` takes a context name, not {v}")),
        };
        self.keys_text.push(node.to_string());
        for child in Self::children(node) {
            match child.name().value() {
                "bind" => {
                    for (chord, command, help) in self.bind_node(at, child) {
                        self.keys.bind(ctx, chord, command, help);
                    }
                }
                "unbind" => {
                    self.check_props(at, child, &[]);
                    for e in child.entries() {
                        let Some(k) = e.value().as_string() else { continue };
                        match Chord::parse(k) {
                            Ok(c) => {
                                if !self.keys.unbind(ctx, &c) {
                                    self.err(at, e.span().offset(), format!("`{k}` isn't bound in {} keys", ctx.name()));
                                }
                            }
                            Err(m) => self.err(at, e.span().offset(), m),
                        }
                    }
                }
                other => self.err(at, child.span().offset(), format!("unknown node `{other}` in keys{}", suggest(other, &["bind", "unbind"]))),
            }
        }
    }

    /// `notify before="5m" place="15m" video="2m" demanding="10m" quiet="22:00-07:00"`
    fn notify_node(&mut self, at: &At, node: &KdlNode) {
        self.check_props(at, node, &["before", "place", "video", "demanding", "quiet"]);
        let off = node.span().offset();
        let mut rules = crate::notify::Rules::default();
        for (key, slot) in [("before", &mut rules.before), ("place", &mut rules.place), ("video", &mut rules.video), ("demanding", &mut rules.demanding)] {
            let Some(text) = self.str_prop(at, node, key) else { continue };
            match text.strip_suffix('m').and_then(|n| n.parse::<i64>().ok()).or_else(|| text.strip_suffix('h').and_then(|n| n.parse::<i64>().ok()).map(|h| h * 60)) {
                Some(m) if (0..=24 * 60).contains(&m) => *slot = m,
                _ => self.err(at, off, format!("`{key}` should be minutes or hours, like \"5m\" or \"1h\", not `{text}`")),
            }
        }
        if let Some(q) = self.str_prop(at, node, "quiet") {
            let t = |s: &str| chrono::NaiveTime::parse_from_str(s.trim(), "%H:%M").ok();
            match q.split_once('-').and_then(|(a, b)| Some((t(a)?, t(b)?))) {
                Some(span) => rules.quiet = Some(span),
                None => self.err(at, off, format!("`quiet` should be two times, like \"22:00-07:00\", not `{q}`")),
            }
        }
        self.settings.notify = Some(rules);
        self.settings.text.push(node.to_string());
    }

    /// `overlay "Travel" key="w" { rail "travel"; zones "visible" }`
    fn overlay_node(&mut self, at: &At, node: &KdlNode) {
        self.check_props(at, node, &["key"]);
        let Some(name) = self.arg_str(at, node, 0, "a name") else { return };
        let mut set = Vec::new();
        for child in Self::children(node) {
            let k = child.name().value().to_string();
            let off = child.span().offset();
            let v = match Self::arg(child, 0).map(|e| e.value()) {
                Some(KdlValue::String(s)) => s.clone(),
                Some(KdlValue::Bool(b)) => b.to_string(),
                _ => {
                    self.err(at, off, format!("`{k}` in an overlay takes a value: {k} \"…\""));
                    continue;
                }
            };
            // Channels are checked here; anything else is a param, checked against the view
            // it's used in.
            if let Some((_, values)) = CHANNELS.iter().find(|(c, _)| *c == k)
                && !values.contains(&v.as_str())
            {
                self.err(at, off, format!("`{v}` isn't a {k}{}", suggest(&v, values)));
                continue;
            }
            set.push((k, v));
        }
        if set.is_empty() {
            return self.err(at, node.span().offset(), format!("overlay `{name}` sets nothing: give it settings, like {{ rail \"travel\" }}"));
        }
        if let Some(key) = self.str_prop(at, node, "key") {
            match Chord::parse(&key) {
                Ok(c) => self.keys.bind(Ctx::Normal, c, Command::Overlay(name.clone()), None),
                Err(m) => self.err(at, node.span().offset(), m),
            }
        }
        self.keys_text.push(node.to_string());
        self.overlays.retain(|o| o.name != name);
        self.overlays.push(Overlay { name, set });
    }

    /// `bind "h" "left" "pan -1" help="…"`: keys, then the command.
    fn bind_node(&mut self, at: &At, node: &KdlNode) -> Vec<(Chord, Command, Option<String>)> {
        self.check_props(at, node, &["help"]);
        let help = self.str_prop(at, node, "help");
        let args: Vec<&KdlEntry> = node.entries().iter().filter(|e| e.name().is_none()).collect();
        let Some((last, keys)) = args.split_last().filter(|(_, k)| !k.is_empty()) else {
            self.err(at, node.span().offset(), "`bind` takes one or more keys, then a command: bind \"h\" \"left\" \"pan -1\"");
            return Vec::new();
        };
        let command = match last.value().as_string().map(Command::parse) {
            Some(Ok(c)) => {
                if let Command::Overlay(name) = &c {
                    let (line, col) = at.pos(last.span().offset());
                    self.overlay_refs.push((at.file.to_string(), line, col, name.clone()));
                }
                c
            }
            Some(Err(m)) => {
                self.err(at, last.span().offset(), m);
                return Vec::new();
            }
            None => {
                self.err(at, last.span().offset(), "the command should be a string");
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for e in keys {
            match e.value().as_string().map(Chord::parse) {
                Some(Ok(c)) => out.push((c, command.clone(), help.clone())),
                Some(Err(m)) => self.err(at, e.span().offset(), m),
                None => self.err(at, e.span().offset(), "a key should be a string"),
            }
        }
        out
    }

    /// `zones "Asia/Manila" "Europe/London" { label "Asia/Manila" short="🇵🇭" long="Manila" }`
    fn zones_node(&mut self, at: &At, node: &KdlNode) {
        self.check_props(at, node, &[]);
        let known = |z: &str| z.parse::<chrono_tz::Tz>().is_ok();
        let suggest_tz = |z: &str| {
            let names: Vec<&str> = chrono_tz::TZ_VARIANTS.iter().map(|t| t.name()).collect();
            suggest(z, &names)
        };
        let mut decl = ZoneDecl::default();
        for e in node.entries().iter().filter(|e| e.name().is_none()) {
            match e.value().as_string() {
                Some(z) if known(z) => decl.zones.push(z.to_string()),
                Some(z) => self.err(at, e.span().offset(), format!("unknown time zone `{z}`{}", suggest_tz(z))),
                None => self.err(at, e.span().offset(), "zone names are strings, like \"Europe/London\""),
            }
        }
        for child in Self::children(node) {
            if child.name().value() != "label" {
                self.err(at, child.span().offset(), format!("unknown node `{}` in zones; did you mean `label`?", child.name().value()));
                continue;
            }
            self.check_props(at, child, &["short", "long"]);
            let Some(z) = self.arg_str(at, child, 0, "a time zone name") else { continue };
            if !known(&z) {
                self.err(at, child.span().offset(), format!("unknown time zone `{z}`{}", suggest_tz(&z)));
                continue;
            }
            let (short, long) = (self.str_prop(at, child, "short"), self.str_prop(at, child, "long"));
            decl.labels.push((z, short, long));
        }
        self.settings.zones = Some(decl);
        self.settings.text.push(node.to_string());
    }

    /// `calendar "Work" color="sage" demand=0.6 label="Work"`
    fn calendar_node(&mut self, at: &At, node: &KdlNode) {
        self.check_props(at, node, &["color", "demand", "label", "secondary", "notify"]);
        let off = node.span().offset();
        let Some(name) = self.arg_str(at, node, 0, "a name") else { return };
        let color = self.str_prop(at, node, "color");
        if let Some(c) = &color
            && crate::model::parse_color(c).is_none()
        {
            let names: Vec<&str> = crate::model::PALETTE.iter().map(|p| p.0).collect();
            self.err(at, off, format!("`color` should be a palette name ({}) or #rrggbb, not `{c}`{}", names.join(", "), suggest(c, &names)));
            return;
        }
        let demand = self.num_prop(at, node, "demand").map(|d| d as f32);
        if demand.is_some_and(|d| !(0. ..=1.).contains(&d)) {
            self.err(at, off, "`demand` should be between 0 (light) and 1 (demanding)");
            return;
        }
        if self.calendars.iter().any(|c| c.name.eq_ignore_ascii_case(&name)) {
            self.err(at, off, format!("calendar `{name}` is declared twice"));
            return;
        }
        if self.calendars.len() == crate::model::MAX_CALENDARS {
            self.err(at, off, format!("at most {} calendars", crate::model::MAX_CALENDARS));
            return;
        }
        self.settings.text.push(node.to_string());
        let label = self.str_prop(at, node, "label");
        let secondary = match Self::prop(node, "secondary").map(|e| e.value()) {
            Some(KdlValue::Bool(b)) => *b,
            Some(v) => {
                self.err(at, off, format!("`secondary` should be #true or #false, not {v}"));
                return;
            }
            None => false,
        };
        let quiet = match Self::prop(node, "notify").map(|e| e.value()) {
            Some(KdlValue::Bool(b)) => !*b,
            Some(v) => {
                self.err(at, off, format!("`notify` should be #true or #false, not {v}"));
                return;
            }
            None => false,
        };
        self.calendars.push(CalendarDecl { name, label, color, demand, secondary, quiet });
    }

    /// `energy wake="7:00" sleep="23:00" peak="16:48" reactive=#true drain=0.3 recovery="60m" { at "9:00" 0.8 }`
    fn energy_node(&mut self, at: &At, node: &KdlNode) {
        use crate::energy::Baseline;
        self.check_props(at, node, &["model", "wake", "sleep", "peak", "reactive", "drain", "recovery"]);
        let off = node.span().offset();
        // Each `energy` line layers on earlier ones (calm.kdl, then the editor's file), so
        // `energy reactive=#false` keeps your hours and your own curve.
        let mut m = self.settings.energy.clone().unwrap_or_default();
        let (mut wake, mut sleep, mut peak) = match m.baseline {
            Baseline::ThreeProcess { wake, sleep, peak } => (wake, sleep, peak),
            Baseline::Points(_) => (7., 23., 16.8),
        };
        let retimed = ["wake", "sleep", "peak"].iter().any(|k| Self::prop(node, k).is_some());
        let time = |this: &mut Self, key: &str, slot: &mut f32| {
            if let Some(s) = this.str_prop(at, node, key) {
                match clock_hours(&s) {
                    Some(h) if h < 24. => *slot = h,
                    _ => this.err(at, off, format!("`{key}` should be a time like \"7:30\", not `{s}`")),
                }
            }
        };
        time(self, "wake", &mut wake);
        time(self, "sleep", &mut sleep);
        time(self, "peak", &mut peak);
        let mut points = Vec::new();
        for child in Self::children(node) {
            if child.name().value() != "at" {
                self.err(at, child.span().offset(), format!("unknown node `{}` in energy; did you mean `at`?", child.name().value()));
                continue;
            }
            self.check_props(at, child, &[]);
            let t = Self::arg(child, 0).and_then(|e| e.value().as_string()).and_then(clock_hours);
            let level = Self::arg(child, 1).and_then(|e| match e.value() {
                KdlValue::Integer(i) => Some(*i as f32),
                KdlValue::Float(f) => Some(*f as f32),
                _ => None,
            });
            match (t, level) {
                (Some(t), Some(l)) if t < 24. && (0. ..=1.).contains(&l) => points.push((t, l)),
                _ => self.err(at, child.span().offset(), "`at` needs a time and a level, like `at \"9:30\" 0.8` (level 0–1)"),
            }
        }
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        let model = self.str_prop(at, node, "model");
        m.baseline = match model.as_deref() {
            None if !points.is_empty() => Baseline::Points(points),
            None if !retimed => m.baseline,
            Some("points") if points.len() >= 2 => Baseline::Points(points),
            Some("points") => {
                self.err(at, off, "model=\"points\" needs at least two `at \"H:MM\" LEVEL` lines inside");
                return;
            }
            None | Some("three-process") => {
                if !points.is_empty() {
                    self.err(at, off, "`at` points only apply with model=\"points\"");
                }
                Baseline::ThreeProcess { wake, sleep, peak }
            }
            Some(other) => {
                self.err(at, off, format!("unknown energy model `{other}`{}", suggest(other, &["three-process", "points"])));
                return;
            }
        };
        match Self::prop(node, "reactive").map(|e| e.value()) {
            Some(KdlValue::Bool(b)) => m.reactive = *b,
            Some(v) => self.err(at, off, format!("`reactive` should be #true or #false, not {v}")),
            None => {}
        }
        if let Some(d) = self.num_prop(at, node, "drain") {
            if (0. ..=2.).contains(&d) {
                m.drain = d as f32;
            } else {
                self.err(at, off, "`drain` is energy lost per hour at full demand, between 0 and 2");
            }
        }
        if let Some(r) = self.str_prop(at, node, "recovery") {
            match crate::sync::parse_duration(&r) {
                Some(d) => m.recovery = d.as_secs_f32() / 3600.,
                None => self.err(at, off, format!("`recovery` should look like \"45m\" or \"1h\", not `{r}`")),
            }
        }
        self.settings.text.push(node.to_string());
        self.settings.energy = Some(m);
    }

    /// `defaults "Grid" measure="demand" span=#true`: a view's param defaults, without copying the view.
    fn apply_defaults(&mut self) {
        for (file, line, col, node) in std::mem::take(&mut self.param_defaults) {
            let mut diag = |message: String| Diag { file: file.clone(), line, col, message };
            let Some(view) = node.entries().first().filter(|e| e.name().is_none()).and_then(|e| e.value().as_string()).map(str::to_string) else {
                self.diags.push(diag("`defaults` needs a view name, like `defaults \"Grid\" measure=\"demand\"`".into()));
                continue;
            };
            let names: Vec<String> = self.views.iter().map(|(v, _)| v.name.clone()).collect();
            let Some(vi) = names.iter().position(|n| *n == view) else {
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                self.diags.push(diag(format!("no view named `{view}`{}", suggest(&view, &refs))));
                continue;
            };
            let mut errors = Vec::new();
            for e in node.entries().iter().skip(1) {
                let Some(key) = e.name().map(|n| n.value().to_string()) else { continue };
                let spec = &mut self.views[vi].0;
                let pnames: Vec<String> = spec.params.iter().map(|p| p.name.clone()).collect();
                let Some(p) = spec.params.iter_mut().find(|p| p.name == key) else {
                    let refs: Vec<&str> = pnames.iter().map(String::as_str).collect();
                    errors.push(format!("view `{view}` has no param `{key}`{}", suggest(&key, &refs)));
                    continue;
                };
                let value = match e.value() {
                    KdlValue::Bool(b) => b.to_string(),
                    v => v.as_string().map(str::to_string).unwrap_or_else(|| v.to_string()),
                };
                let ok = if p.bind == Bind::Toggle { value == "true" || value == "false" } else { p.options.iter().any(|o| o.value == value) };
                if ok {
                    p.default = value;
                } else {
                    let mut valid: Vec<&str> = if p.bind == Bind::Toggle { vec!["#true", "#false"] } else { p.options.iter().map(|o| o.value.as_str()).collect() };
                    let mut seen = std::collections::HashSet::new();
                    valid.retain(|v| seen.insert(*v));
                    errors.push(format!("`{key}` can't be `{value}`; one of: {}", valid.join(", ")));
                }
            }
            self.diags.extend(errors.into_iter().map(&mut diag));
        }
    }

    /// `colors "Paper" base="light" { bg "#f6f1e7" chrome "#ece4d6" }`; `colors "dark" { … }` tweaks a built-in.
    fn colors_node(&mut self, at: &At, node: &KdlNode) {
        self.check_props(at, node, &["base"]);
        let off = node.span().offset();
        let Some(name) = self.arg_str(at, node, 0, "a theme name") else { return };
        let builtin = ["light", "dark"].contains(&name.to_ascii_lowercase().as_str());
        let base = self.str_prop(at, node, "base");
        let dark = match (builtin, base.as_deref()) {
            (true, Some(_)) => {
                self.err(at, off, format!("`{name}` is a built-in theme; `base` only applies to your own"));
                return;
            }
            (true, None) => name.eq_ignore_ascii_case("dark"),
            (false, None | Some("light")) => false,
            (false, Some("dark")) => true,
            (false, Some(b)) => {
                self.err(at, off, format!("`base` should be \"light\" or \"dark\", not `{b}`"));
                return;
            }
        };
        if name.eq_ignore_ascii_case("system") {
            self.err(at, off, "`system` follows your desktop between the light and dark themes; name your theme something else");
            return;
        }
        let mut colors = Vec::new();
        for child in Self::children(node) {
            let token = child.name().value().to_string();
            if !crate::theme::TOKENS.contains(&token.as_str()) {
                self.err(at, child.span().offset(), format!("unknown colour `{token}`{}; one of: {}", suggest(&token, crate::theme::TOKENS), crate::theme::TOKENS.join(", ")));
                continue;
            }
            self.check_props(at, child, &[]);
            match Self::arg(child, 0).and_then(|e| e.value().as_string()).and_then(crate::theme::parse_hex) {
                Some(c) => colors.push((token, c)),
                None => self.err(at, child.span().offset(), format!("`{token}` needs a colour like \"#2a2724\" (or \"#2a2724cc\" with alpha)")),
            }
        }
        // Later blocks for the same theme add to (and override) earlier ones.
        match self.settings.themes.iter_mut().find(|t| t.name.eq_ignore_ascii_case(&name)) {
            Some(t) => {
                t.dark = dark;
                t.colors.extend(colors);
            }
            None => self.settings.themes.push(crate::theme::ThemeDecl { name, dark, colors }),
        }
        self.settings.text.push(node.to_string());
    }

    /// Every source's calendar must be declared, once any calendars are.
    fn check_refs(&mut self) {
        self.apply_defaults();
        // A split names a view (hidden ones too), which for now must be a packed list.
        for (file, line, col, name) in std::mem::take(&mut self.split_refs) {
            let message = match self.views.iter().find(|(v, _)| v.name == name) {
                None => {
                    let names: Vec<&str> = self.views.iter().map(|(v, _)| v.name.as_str()).collect();
                    format!("`split` names no view `{name}`{}", suggest(&name, &names))
                }
                Some((v, _)) if !v.facet.pack => format!("`split \"{name}\"`: the view beside another is drawn as a list, so it needs `facet … pack=#true` (like Upcoming)"),
                Some((v, _)) if v.split.is_some() => format!("`split \"{name}\"`: that view has a split of its own"),
                _ => continue,
            };
            self.diags.push(Diag { file, line, col, message });
        }
        for (file, line, col, name) in std::mem::take(&mut self.overlay_refs) {
            if !self.overlays.iter().any(|o| o.name == name) {
                let names: Vec<&str> = self.overlays.iter().map(|o| o.name.as_str()).collect();
                self.diags.push(Diag { file, line, col, message: format!("no overlay named `{name}`{}", suggest(&name, &names)) });
            }
        }
        // A view's key over a global one is allowed (it's how a view takes a key over), but
        // worth a word in case it's a slip.
        for (file, line, col, view, chord) in std::mem::take(&mut self.view_chords) {
            if let Some(b) = self.keys.get(Ctx::Normal).iter().find(|b| b.chord == chord) {
                let message = format!("view `{view}`: `{chord}` takes over the global key (`{}`) in this view", b.command);
                self.warnings.push(Diag { file, line, col, message });
            }
        }
        // Themes named by `theme` must be built in or declared with `colors`.
        let mut known: Vec<String> = ["system", "light", "dark"].map(String::from).to_vec();
        known.extend(self.settings.themes.iter().map(|t| t.name.clone()));
        for (file, line, col, name) in std::mem::take(&mut self.theme_refs) {
            if !known.iter().any(|k| k.eq_ignore_ascii_case(&name)) {
                let refs: Vec<&str> = known.iter().map(String::as_str).collect();
                let message = format!("no theme named `{name}`{} (declare one with `colors \"{name}\" base=\"light\" {{ … }}`)", suggest(&name, &refs));
                self.diags.push(Diag { file, line, col, message });
            }
        }
        if self.calendars.is_empty() {
            return;
        }
        let names: Vec<&str> = self.calendars.iter().map(|c| c.name.as_str()).collect();
        let mut bad = Vec::new();
        for (file, line, col, want, explicit) in &self.cal_refs {
            if !names.iter().any(|n| n.eq_ignore_ascii_case(want)) {
                let message = if *explicit {
                    format!("no calendar named `{want}`{}", suggest(want, &names))
                } else {
                    format!("source `{want}` needs calendar=\"…\" naming one of: {}", names.join(", "))
                };
                bad.push(Diag { file: file.clone(), line: *line, col: *col, message });
            }
        }
        self.diags.extend(bad);
    }

    /// `source "Personal" url-file="…" calendar="Personal" refresh="15m"`
    fn source_node(&mut self, at: &At, node: &KdlNode) {
        self.check_props(at, node, &["url", "url-file", "path", "google", "calendar", "refresh", "writes"]);
        let off = node.span().offset();
        let Some(name) = self.arg_str(at, node, 0, "a name") else { return };
        let (url, url_file, path) = (self.str_prop(at, node, "url"), self.str_prop(at, node, "url-file"), self.str_prop(at, node, "path"));
        let google = self.str_prop(at, node, "google");
        if [&url, &url_file, &path, &google].iter().filter(|x| x.is_some()).count() != 1 {
            self.err(at, off, format!("source `{name}` needs exactly one of url=, url-file=, path= or google= (keep secret addresses in a url-file)"));
            return;
        }
        let calendar = self.str_prop(at, node, "calendar");
        if google.is_some() && calendar.is_some() {
            self.err(at, off, format!("source `{name}`: a Google account brings its own calendars, so `calendar=` doesn't apply"));
            return;
        }
        // Checked against declared calendars once everything has loaded (see `check_refs`).
        let (line, col) = at.pos(off);
        if google.is_none() {
            self.cal_refs.push((at.file.to_string(), line, col, calendar.clone().unwrap_or_else(|| name.clone()), calendar.is_some()));
        }
        let writes = self.str_prop(at, node, "writes");
        if let Some(w) = &writes
            && !["auto", "review"].contains(&w.as_str())
        {
            self.err(at, off, format!("`writes` should be \"auto\" or \"review\", not `{w}`"));
            return;
        }
        let refresh = self.str_prop(at, node, "refresh");
        if let Some(r) = &refresh
            && crate::sync::parse_refresh(r).is_none()
        {
            self.err(at, off, format!("`refresh` should look like \"15m\", \"1h\" or \"90s\", not `{r}`"));
            return;
        }
        if self.sources.iter().any(|s| s.name == name) {
            self.err(at, off, format!("source `{name}` is declared twice"));
            return;
        }
        // Dumped without the URL: it may be a secret.
        let mut shown = node.clone();
        if url.is_some() {
            shown.entries_mut().retain(|e| e.name().is_none_or(|n| n.value() != "url"));
            shown.push(("url", "<hidden>"));
        }
        self.settings.text.push(shown.to_string());
        self.sources.push(SourceDecl { name, url, url_file, path, google, calendar, refresh, writes });
    }

    fn finish(self) -> Config {
        // Splits take their side view whether or not it's in the switcher.
        let all: Vec<Spec> = self.views.iter().map(|(v, _)| v.clone()).collect();
        let mut views: Vec<(Spec, String)> = self.views.into_iter().filter(|(v, _)| !self.hidden.contains(&v.name)).collect();
        for (v, _) in &mut views {
            if let Some(s) = &mut v.split {
                s.view = all.iter().find(|o| o.name == s.name && o.facet.pack && o.split.is_none()).map(|o| Box::new(o.clone()));
            }
        }
        if let Some(order) = &self.order {
            views.sort_by_key(|(v, _)| order.iter().position(|n| *n == v.name).unwrap_or(usize::MAX));
        }
        let templates = self.template_order.iter().filter_map(|t| self.templates.get(t).map(|(_, s)| s.clone())).collect();
        let (views, view_text): (Vec<Spec>, Vec<String>) = views.into_iter().unzip();
        let mut settings = self.settings;
        if !self.sources.is_empty() {
            settings.sources = Some(self.sources);
        }
        if !self.calendars.is_empty() {
            settings.calendars = Some(self.calendars);
        }
        let base_names = views.iter().map(|v: &Spec| v.name.clone()).collect();
        let mut settings_text = std::mem::take(&mut settings.text);
        settings_text.extend(self.keys_text);
        settings.text = settings_text;
        Config { views, settings, view_text, templates, files: self.files, base_names, generated_names: Vec::new(), keys: self.keys, overlays: self.overlays, warnings: self.warnings }
    }

    // --- Node helpers ---------------------------------------------------------

    fn check_props(&mut self, at: &At, node: &KdlNode, allowed: &[&str]) {
        for e in node.entries() {
            if let Some(n) = e.name() {
                let n = n.value();
                if !allowed.contains(&n) {
                    let hint = if allowed.is_empty() { " (this node takes no properties)".into() } else { suggest(n, allowed) };
                    self.err(at, e.span().offset(), format!("unknown property `{n}` on `{}`{hint}", node.name().value()));
                }
            }
        }
    }

    fn prop<'n>(node: &'n KdlNode, key: &str) -> Option<&'n KdlEntry> {
        node.entries().iter().rev().find(|e| e.name().is_some_and(|n| n.value() == key))
    }

    fn props<'n>(node: &'n KdlNode, key: &'n str) -> impl Iterator<Item = &'n KdlEntry> + 'n {
        node.entries().iter().filter(move |e| e.name().is_some_and(|n| n.value() == key))
    }

    fn arg(node: &KdlNode, i: usize) -> Option<&KdlEntry> {
        node.entries().iter().filter(|e| e.name().is_none()).nth(i)
    }

    fn arg_str(&mut self, at: &At, node: &KdlNode, i: usize, what: &str) -> Option<String> {
        match Self::arg(node, i).map(|e| e.value()) {
            Some(KdlValue::String(s)) => Some(s.clone()),
            _ => {
                self.err(at, node.span().offset(), format!("`{}` needs {what} as a string", node.name().value()));
                None
            }
        }
    }

    fn str_prop(&mut self, at: &At, node: &KdlNode, key: &str) -> Option<String> {
        let e = Self::prop(node, key)?;
        match e.value() {
            KdlValue::String(s) => Some(s.clone()),
            v => {
                self.err(at, e.span().offset(), format!("`{key}` should be a string, not {v}"));
                None
            }
        }
    }

    fn num_prop(&mut self, at: &At, node: &KdlNode, key: &str) -> Option<f64> {
        let e = Self::prop(node, key)?;
        match e.value() {
            KdlValue::Integer(i) => Some(*i as f64),
            KdlValue::Float(f) => Some(*f),
            v => {
                self.err(at, e.span().offset(), format!("`{key}` should be a number, not {v}"));
                None
            }
        }
    }

    fn require<T>(&mut self, at: &At, node: &KdlNode, what: &str, v: Option<T>) -> Option<T> {
        if v.is_none() && !self.diags.iter().any(|d| d.line == at.pos(node.span().offset()).0) {
            self.err(at, node.span().offset(), format!("`{}` needs {what}", node.name().value()));
        }
        v
    }

    /// A string argument or property that must be one of `choices`.
    fn choice<T: Copy>(&mut self, at: &At, offset: usize, what: &str, got: &str, choices: &[(&str, T)]) -> Option<T> {
        match choices.iter().find(|(n, _)| *n == got) {
            Some((_, v)) => Some(*v),
            None => {
                let names: Vec<&str> = choices.iter().map(|(n, _)| *n).collect();
                self.err(at, offset, format!("unknown {what} `{got}`{}", suggest(got, &names)));
                None
            }
        }
    }

    fn children(node: &KdlNode) -> &[KdlNode] {
        node.children().map_or(&[], |d| d.nodes())
    }

    // --- Grammar ---------------------------------------------------------------

    fn conds(&mut self, at: &At, node: &KdlNode) -> Vec<Cond> {
        let mut out = Vec::new();
        for e in Self::props(node, "when-mark") {
            if let Some(m) = e.value().as_string().and_then(|s| self.choice(at, e.span().offset(), "mark", s, MARKS)) {
                out.push(Cond::Mark(m));
            }
        }
        for e in Self::props(node, "when-mark-not") {
            if let Some(m) = e.value().as_string().and_then(|s| self.choice(at, e.span().offset(), "mark", s, MARKS)) {
                out.push(Cond::NotMark(m));
            }
        }
        for (key, not) in [("when-param", false), ("when-param-not", true)] {
            for e in Self::props(node, key) {
                match e.value().as_string().and_then(|s| s.split_once('=')) {
                    Some((n, v)) => {
                        let (n, v) = (n.trim().to_string(), v.trim().to_string());
                        out.push(if not { Cond::NotParam(n, v) } else { Cond::Param(n, v) });
                    }
                    None => self.err(at, e.span().offset(), format!("`{key}` should look like \"name=value\"")),
                }
            }
        }
        out
    }

    fn param(&mut self, at: &At, node: &KdlNode) -> Option<Param> {
        self.check_props(at, node, &["label", "bind", "key", "row", "default", "when-mark", "when-mark-not", "when-param", "when-param-not"]);
        let name = self.arg_str(at, node, 0, "a name")?;
        let mut options = Vec::new();
        for child in Self::children(node) {
            if child.name().value() != "option" {
                let hint = suggest(child.name().value(), &["option"]);
                self.err(at, child.span().offset(), format!("unknown node `{}` in param{hint}", child.name().value()));
                continue;
            }
            self.check_props(at, child, &["value", "when-mark", "when-mark-not", "when-param", "when-param-not"]);
            let Some(label) = self.arg_str(at, child, 0, "a label") else { continue };
            let value = self.str_prop(at, child, "value").unwrap_or_else(|| label.to_lowercase());
            let when = self.conds(at, child);
            options.push(ParamOption { label, value, when });
        }
        let bind = match self.str_prop(at, node, "bind") {
            Some(b) => self.choice(at, node.span().offset(), "bind", &b, &[("segmented", Bind::Segmented), ("toggle", Bind::Toggle)])?,
            None if options.is_empty() => Bind::Toggle,
            None => Bind::Segmented,
        };
        let default = match Self::prop(node, "default").map(|e| e.value()) {
            Some(KdlValue::Bool(b)) => b.to_string(),
            Some(KdlValue::String(s)) => s.clone(),
            Some(v) => {
                self.err(at, node.span().offset(), format!("`default` should be a string or #true/#false, not {v}"));
                return None;
            }
            None if bind == Bind::Toggle => "false".into(),
            None => options.first().map(|o| o.value.clone()).unwrap_or_default(),
        };
        if bind == Bind::Segmented && options.is_empty() {
            self.err(at, node.span().offset(), format!("segmented param `{name}` needs at least one `option`"));
        }
        if bind == Bind::Segmented && !options.iter().any(|o| o.value == default) {
            self.err(at, node.span().offset(), format!("default `{default}` of `{name}` is not one of its option values"));
        }
        if bind == Bind::Toggle && !matches!(default.as_str(), "true" | "false") {
            self.err(at, node.span().offset(), format!("toggle `{name}` needs a #true or #false default"));
        }
        let row = self.num_prop(at, node, "row").map_or(1, |r| r.clamp(1., 4.) as u8);
        let when = self.conds(at, node);
        Some(Param { name, label: self.str_prop(at, node, "label"), bind, key: self.str_prop(at, node, "key"), row, default, options, when })
    }

    fn params_block(&mut self, at: &At, node: &KdlNode) -> Vec<Param> {
        self.check_props(at, node, &[]);
        let mut out = Vec::new();
        for child in Self::children(node) {
            if child.name().value() == "param" {
                out.extend(self.param(at, child));
            } else {
                let hint = suggest(child.name().value(), &["param"]);
                self.err(at, child.span().offset(), format!("unknown node `{}` in params{hint}", child.name().value()));
            }
        }
        out
    }

    fn view(&mut self, at: &At, node: &KdlNode) -> Option<Spec> {
        self.check_props(at, node, &[]);
        let errors_before = self.diags.len();
        let name = self.arg_str(at, node, 0, "a name")?;
        let mut facet = None;
        let (mut coord, mut x, mut y, mut collide) = (Coord::Cartesian, Axis::None, Axis::None, Collide::Overlay);
        let (mut mark, mut lod, mut lod_extent) = (None, Vec::new(), Extent::Time);
        let mut stops = Vec::new();
        let mut hours = (0., 1.);
        let mut zones = Ref::Lit("config".into());
        let mut params: Vec<Param> = Vec::new();
        let mut color = None;
        let mut day_span = Ref::Lit("false".into());
        let (mut rail, mut rail_style) = (Ref::Lit("off".into()), Ref::Lit("rail".into()));
        let mut all_day = true;
        let mut view_keys: Vec<(Chord, Command, Option<String>, usize, usize)> = Vec::new();
        let mut split = None;

        for child in Self::children(node) {
            let off = child.span().offset();
            let arg = |i| Self::arg(child, i).and_then(|e| e.value().as_string()).map(str::to_string);
            match child.name().value() {
                "facet" => {
                    self.check_props(at, child, &["per-line", "dir", "band", "pack"]);
                    let per = self.num_prop(at, child, "per-line");
                    let dir = self.str_prop(at, child, "dir").and_then(|d| self.choice(at, off, "dir", &d, &[("rows", Dir::Rows), ("columns", Dir::Columns)]));
                    let band = self.num_prop(at, child, "band");
                    let pack = match Self::prop(child, "pack").map(|e| e.value()) {
                        Some(KdlValue::Bool(b)) => *b,
                        Some(v) => {
                            self.err(at, off, format!("`pack` should be #true or #false, not {v}"));
                            false
                        }
                        None => false,
                    };
                    if pack && (per != Some(1.) || dir != Some(Dir::Rows)) {
                        self.err(at, off, "`pack=#true` lays days out as a list: it needs per-line=1 and dir=\"rows\"");
                    }
                    match (per, dir, band) {
                        (Some(p), Some(d), Some(b)) if p >= 1. && b >= 1. => facet = Some(Facet { per_line: p as i32, dir: d, band: b as i32, pack }),
                        _ => self.err(at, off, "`facet` needs per-line=N (≥1), dir=\"rows\"|\"columns\" and band=N (≥1)"),
                    }
                }
                "coord" => {
                    self.check_props(at, child, &[]);
                    coord = arg(0).and_then(|s| self.choice(at, off, "coord", &s, &[("cartesian", Coord::Cartesian), ("polar", Coord::Polar)])).unwrap_or(coord);
                }
                n @ ("x" | "y") => {
                    self.check_props(at, child, &[]);
                    if let Some(a) = arg(0).and_then(|s| self.choice(at, off, "axis", &s, AXES)) {
                        if n == "x" { x = a } else { y = a }
                    }
                }
                "collide" => {
                    self.check_props(at, child, &[]);
                    let choices = &[("split", Collide::Split), ("highlight-conflict", Collide::HighlightConflict), ("overlay", Collide::Overlay)];
                    collide = arg(0).and_then(|s| self.choice(at, off, "collide", &s, choices)).unwrap_or(collide);
                }
                "mark" => {
                    self.check_props(at, child, &["lod-extent"]);
                    mark = arg(0).and_then(|s| self.choice(at, off, "mark", &s, MARKS));
                    if let Some(e) = self.str_prop(at, child, "lod-extent") {
                        lod_extent = self.choice(at, off, "lod-extent", &e, &[("time", Extent::Time), ("height", Extent::Height), ("width", Extent::Width), ("min", Extent::Min)]).unwrap_or(lod_extent);
                    }
                    for l in Self::children(child) {
                        if l.name().value() != "lod" {
                            self.err(at, l.span().offset(), format!("unknown node `{}` in mark; did you mean `lod`?", l.name().value()));
                            continue;
                        }
                        let before = self.diags.len();
                        self.check_props(at, l, &["below", "mark"]);
                        let below = self.num_prop(at, l, "below");
                        let m = self.str_prop(at, l, "mark").and_then(|s| self.choice(at, l.span().offset(), "mark", &s, MARKS));
                        match (below, m) {
                            (Some(b), Some(m)) => lod.push(Lod { below: b as f32, mark: m }),
                            // Only if nothing more specific was already reported.
                            _ if self.diags.len() == before => self.err(at, l.span().offset(), "`lod` needs below=PIXELS and mark=\"…\""),
                            _ => {}
                        }
                    }
                }
                "stops" => {
                    self.check_props(at, child, &[]);
                    for s in Self::children(child) {
                        if s.name().value() != "stop" {
                            self.err(at, s.span().offset(), format!("unknown node `{}` in stops; did you mean `stop`?", s.name().value()));
                            continue;
                        }
                        self.check_props(at, s, &["cells", "align"]);
                        let sname = self.arg_str(at, s, 0, "a name");
                        let cells = self.num_prop(at, s, "cells");
                        let align = self.str_prop(at, s, "align").map_or(Some(Align::Line), |a| {
                            self.choice(at, s.span().offset(), "align", &a, &[("cell", Align::Cell), ("line", Align::Line), ("month", Align::Month), ("group", Align::Group)])
                        });
                        match (sname, cells, align) {
                            (Some(n), Some(c), Some(a)) if c >= 1. => stops.push(Stop { name: n, cells: c as i32, align: a }),
                            _ => self.err(at, s.span().offset(), "`stop` needs a name and cells=N (≥1)"),
                        }
                    }
                }
                "hours" => {
                    self.check_props(at, child, &["from", "to"]);
                    let parse = |s: Option<String>| {
                        let s = s?;
                        let (h, m) = s.split_once(':').unwrap_or((&s, "0"));
                        Some((h.trim().parse::<f32>().ok()? + m.trim().parse::<f32>().ok()? / 60.) / 24.)
                    };
                    let (from, to) = (parse(self.str_prop(at, child, "from")), parse(self.str_prop(at, child, "to")));
                    match (from, to) {
                        (Some(a), Some(b)) if (0. ..b).contains(&a) && b <= 1. => hours = (a, b),
                        _ => self.err(at, off, "`hours` needs from=\"H:MM\" and to=\"H:MM\" with from before to, within 0:00–24:00"),
                    }
                }
                "zones" => {
                    self.check_props(at, child, &[]);
                    if let Some(s) = arg(0) {
                        zones = Ref::parse(&s);
                    }
                }
                "use" => {
                    self.check_props(at, child, &[]);
                    let Some(t) = self.arg_str(at, child, 0, "a template name") else { continue };
                    let Some((tparams, _)) = self.templates.get(&t).cloned() else {
                        let names: Vec<&str> = self.templates.keys().map(String::as_str).collect();
                        self.err(at, off, format!("no params template `{t}`{}", suggest(&t, &names)));
                        continue;
                    };
                    let mut tparams = tparams;
                    for d in Self::children(child) {
                        if d.name().value() != "default" {
                            self.err(at, d.span().offset(), format!("unknown node `{}` in use; did you mean `default`?", d.name().value()));
                            continue;
                        }
                        let pname = Self::arg(d, 0).and_then(|e| e.value().as_string()).map(str::to_string);
                        let value = Self::arg(d, 1).map(|e| match e.value() {
                            KdlValue::Bool(b) => b.to_string(),
                            v => v.as_string().map(str::to_string).unwrap_or_default(),
                        });
                        match (pname, value) {
                            (Some(p), Some(v)) => match tparams.iter_mut().find(|x| x.name == p) {
                                Some(x) => x.default = v,
                                None => self.err(at, d.span().offset(), format!("template `{t}` has no param `{p}`")),
                            },
                            _ => self.err(at, d.span().offset(), "`default` needs a param name and a value"),
                        }
                    }
                    params.extend(tparams);
                }
                "param" => params.extend(self.param(at, child)),
                "split" => {
                    // `split "Upcoming" size=0.4`: that view beside this one, on the right.
                    self.check_props(at, child, &["size"]);
                    let Some(name) = self.arg_str(at, child, 0, "the name of the view beside it") else { continue };
                    let size = self.num_prop(at, child, "size").unwrap_or(0.4) as f32;
                    if !(0.15..=0.7).contains(&size) {
                        self.err(at, off, "`size` is the side view's share of the width: between 0.15 and 0.7");
                    }
                    let (line, col) = at.pos(off);
                    self.split_refs.push((at.file.to_string(), line, col, name.clone()));
                    split = Some(Split { name, size, view: None });
                }
                "keys" => {
                    self.check_props(at, child, &[]);
                    if Self::arg(child, 0).is_some() {
                        self.err(at, off, "a view's `keys` are its normal keys: no context here (modes are set outside views)");
                    }
                    for b in Self::children(child) {
                        if b.name().value() == "bind" {
                            let (line, col) = at.pos(b.span().offset());
                            for (chord, command, help) in self.bind_node(at, b) {
                                view_keys.push((chord, command, help, line, col));
                            }
                        } else {
                            self.err(at, b.span().offset(), format!("unknown node `{}` in a view's keys{}", b.name().value(), suggest(b.name().value(), &["bind"])));
                        }
                    }
                }
                "color" => {
                    self.check_props(at, child, &["measure", "split", "compose", "palette"]);
                    let get = |this: &mut Self, k: &str, dflt: &str| this.str_prop(at, child, k).map_or_else(|| Ref::Lit(dflt.into()), |s| Ref::parse(&s));
                    color = Some(ColorRef { measure: get(self, "measure", "calendar"), split: get(self, "split", "false"), compose: get(self, "compose", "blend"), palette: get(self, "palette", "scale") });
                }
                "detail" => {
                    self.check_props(at, child, &["day-span"]);
                    if let Some(e) = Self::prop(child, "day-span") {
                        day_span = match e.value() {
                            KdlValue::Bool(b) => Ref::Lit(b.to_string()),
                            KdlValue::String(s) => Ref::parse(s),
                            v => {
                                self.err(at, e.span().offset(), format!("`day-span` should be #true/#false or \"$param\", not {v}"));
                                day_span
                            }
                        };
                    }
                }
                "all-day" => {
                    self.check_props(at, child, &[]);
                    if let Some(s) = self.arg_str(at, child, 0, "shown or hidden") {
                        all_day = self.choice(at, child.span().offset(), "all-day", &s, &[("shown", true), ("hidden", false)]).unwrap_or(all_day);
                    }
                }
                "rail" => {
                    // A series beside each day's time axis: `rail show="energy" style="rail"`.
                    self.check_props(at, child, &["show", "style"]);
                    if let Some(s) = self.str_prop(at, child, "show") {
                        rail = Ref::parse(&s);
                    }
                    if let Some(s) = self.str_prop(at, child, "style") {
                        rail_style = Ref::parse(&s);
                    }
                }
                other => {
                    let known = ["facet", "coord", "x", "y", "collide", "mark", "stops", "hours", "zones", "use", "param", "keys", "split", "color", "detail", "rail", "all-day"];
                    self.err(at, off, format!("unknown node `{other}` in view{}", suggest(other, &known)));
                }
            }
        }

        let off = node.span().offset();
        let facet = self.require(at, node, "a `facet` (e.g. facet per-line=7 dir=\"rows\" band=13)", facet);
        let mark = self.require(at, node, "a `mark`", mark);
        if stops.is_empty() {
            self.err(at, off, format!("view `{name}` needs `stops` with at least one `stop`"));
        }
        if stops.windows(2).any(|w| w[0].cells >= w[1].cells) {
            self.err(at, off, format!("view `{name}`: stops must grow in cells, smallest first"));
        }
        // Every `$name` must be a param of this view; literals must be known values.
        let color = color.unwrap_or(ColorRef { measure: Ref::Lit("calendar".into()), split: Ref::Lit("false".into()), compose: Ref::Lit("blend".into()), palette: Ref::Lit("scale".into()) });
        let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
        for (what, r, valid) in [
            ("color measure", &color.measure, Measure::NAMES),
            ("color split", &color.split, &["true", "false"][..]),
            ("color compose", &color.compose, Compose::NAMES),
            ("color palette", &color.palette, &["scale", "calendar", "true", "false"][..]),
            ("detail day-span", &day_span, &["true", "false"][..]),
            ("rail show", &rail, &["off", "energy", "daylight", "zone", "travel"][..]),
            ("zones", &zones, &["primary", "config", "visible", "all"][..]),
            ("rail style", &rail_style, &["rail", "strip"][..]),
        ] {
            match r {
                Ref::Param(p) if !names.contains(&p.as_str()) => self.err(at, off, format!("{what} refers to `${p}`, but view `{name}` has no param `{p}`{}", suggest(p, &names))),
                Ref::Lit(v) if !valid.contains(&v.as_str()) => self.err(at, off, format!("{what} `{v}` is not valid{}", suggest(v, valid))),
                _ => {}
            }
        }
        // The view's keys: its params' (`key=`), then its `keys` block; each chord once.
        let mut keys: Vec<Binding> = Vec::new();
        let (vline, vcol) = at.pos(off);
        let mut chords: Vec<(Chord, usize, usize)> = Vec::new();
        for p in &params {
            let Some(k) = &p.key else { continue };
            match Chord::parse(k) {
                Ok(chord) => {
                    chords.push((chord.clone(), vline, vcol));
                    keys.push(Binding { chord, command: Command::Cycle(p.name.clone()), help: None });
                }
                Err(m) => self.err(at, off, format!("param `{}`: {m}", p.name)),
            }
        }
        for (chord, command, help, line, col) in view_keys {
            for p in command.params() {
                if !names.contains(&p) && !CHANNELS.iter().any(|(c, _)| *c == p) {
                    self.diags.push(Diag { file: at.file.to_string(), line, col, message: format!("view `{name}` has no param `{p}`{}", suggest(p, &names)) });
                }
            }
            chords.push((chord.clone(), line, col));
            keys.push(Binding { chord, command, help });
        }
        for (i, (c, line, col)) in chords.iter().enumerate() {
            if let Some(first) = keys[..i].iter().find(|b| b.chord == *c) {
                let message = format!("view `{name}`: `{c}` is bound to both `{}` and `{}`", first.command, keys[i].command);
                self.diags.push(Diag { file: at.file.to_string(), line: *line, col: *col, message });
            }
        }
        for (c, line, col) in chords {
            self.view_chords.push((at.file.to_string(), line, col, name.clone(), c));
        }
        if self.diags.len() > errors_before {
            return None;
        }
        Some(Spec { name, facet: facet?, coord, x, y, collide, mark: mark?, lod, lod_extent, stops, hours, zones, params, keys, color, day_span, rail, rail_style, all_day, split })
    }
}

const MARKS: &[(&str, Mark)] = &[("block", Mark::Block), ("pill", Mark::Pill), ("arc", Mark::Block), ("list", Mark::List), ("heat", Mark::Heat)];
const AXES: &[(&str, Axis)] =
    &[("none", Axis::None), ("time-of-day", Axis::TimeOfDay), ("lane", Axis::Lane), ("calendar", Axis::Calendar), ("demand", Axis::Demand)];

/// `views/*.kdl`-style patterns: a `*` in the last component only.
fn expand_glob(dir: &Path, pattern: &str) -> Vec<PathBuf> {
    let full = if pattern.starts_with('/') { PathBuf::from(pattern) } else { dir.join(pattern) };
    let Some(file) = full.file_name().and_then(|f| f.to_str()) else { return vec![full] };
    let Some((pre, post)) = file.split_once('*') else { return vec![full] };
    let parent = full.parent().unwrap_or(dir);
    let mut out: Vec<PathBuf> = std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|f| f.to_str()).is_some_and(|f| f.starts_with(pre) && f.ends_with(post)))
        .collect();
    out.sort();
    out
}

/// `calm config check|dump|path`. Returns an exit code.
pub fn cli(args: &[String]) -> i32 {
    let (config, diags) = load();
    match args.first().map(String::as_str) {
        Some("check") => {
            let json = args.iter().any(|a| a == "--json");
            if json {
                println!("[{}]", diags.iter().map(Diag::json).collect::<Vec<_>>().join(","));
                return if diags.is_empty() { 0 } else { 1 };
            }
            for w in &config.warnings {
                eprintln!("warning: {w}");
            }
            if diags.is_empty() {
                let names: Vec<&str> = config.views.iter().map(|v| v.name.as_str()).collect();
                println!("ok: {} views ({})", names.len(), names.join(", "));
            } else {
                for d in &diags {
                    eprintln!("{d}");
                }
            }
            if diags.is_empty() { 0 } else { 1 }
        }
        Some("dump") => {
            print!("{}", config.dump());
            for d in &diags {
                eprintln!("{d}");
            }
            if diags.is_empty() { 0 } else { 1 }
        }
        Some("set") if args.len() == 4 => {
            // `calm config set Grid facet.per-line 7`, `calm config set Grid mark pill`
            let (child, prop) = match args[2].split_once('.') {
                Some((c, p)) => (c, Some(p)),
                None => (args[2].as_str(), None),
            };
            report(edit_view(&config, &args[1], child, prop, &args[3]).map(|_| format!("saved to {}", generated_path().unwrap_or_default().display())))
        }
        Some("duplicate") if args.len() == 2 => report(duplicate_view(&config, &args[1]).map(|n| format!("made `{n}`"))),
        Some("reset") if args.len() == 2 => report(reset_view(&args[1]).map(|_| format!("`{}` reset", args[1]))),
        Some("path") => {
            println!("{}", user_path().map(|p| p.display().to_string()).unwrap_or_default());
            0
        }
        _ => {
            eprintln!("usage: calm config check [--json] | dump | path | set VIEW CHANNEL[.PROP] VALUE | duplicate VIEW | reset VIEW");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_str(src: &str) -> (Vec<Spec>, Vec<Diag>) {
        let mut l = Loader::default();
        l.file("<built-in>", BUILTIN, None);
        l.file("test.kdl", src, None);
        let diags = std::mem::take(&mut l.diags);
        (l.finish().views, diags)
    }

    #[test]
    fn builtins_parse() {
        let (views, diags) = load_str("");
        assert!(diags.is_empty(), "{diags:?}");
        let names: Vec<_> = views.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["Grid", "Lanes", "Energy", "Dial", "Density", "Today"]);
        // Today's side is Upcoming, though Upcoming isn't in the switcher.
        let today = views.iter().find(|v| v.name == "Today").unwrap();
        assert_eq!(today.split.as_ref().and_then(|s| s.view.as_ref()).map(|v| v.name.as_str()), Some("Upcoming"));
    }

    fn load_config(src: &str) -> (Config, Vec<Diag>) {
        let mut l = Loader::default();
        l.file("<built-in>", BUILTIN, None);
        l.file("<keys>", BUILTIN_KEYS, None);
        l.file("test.kdl", src, None);
        l.check_refs();
        let diags = std::mem::take(&mut l.diags);
        (l.finish(), diags)
    }

    #[test]
    fn keys() {
        use crate::keys::{Command, Ctx, Press};
        let press = |key: &str| Press { key: key.into(), typed: Some(key.into()), ..Default::default() };
        let (c, diags) = load_config(
            r#"
            keys { bind "t" "sync"; unbind "d"; }
            keys "create" { bind "x" "snap finer"; }
            view "Mine" {
                facet per-line=7 dir="rows" band=13
                mark "block"
                stops { stop "Week" cells=7; }
                param "rail" bind="segmented" default="off" { option "Off" value="off"; option "Daylight" value="daylight"; }
                rail show="$rail"
                keys { bind "j" "set rail=daylight"; }
            }
            "#,
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(c.keys.lookup(Ctx::Normal, &press("t")), Some(&Command::Sync));
        assert!(c.keys.lookup(Ctx::Normal, &press("d")).is_none());
        assert_eq!(c.keys.lookup(Ctx::Create, &press("x")), Some(&Command::Snap(-1)));
        let mine = c.views.iter().find(|v| v.name == "Mine").unwrap();
        assert_eq!(mine.keys.len(), 1);
        assert!(c.warnings.iter().any(|w| w.message.contains("`j` takes over the global key (`scroll 1`)")), "{:?}", c.warnings);

        let errors = |src: &str| load_config(src).1.into_iter().map(|d| d.message).collect::<Vec<_>>().join("\n");
        assert!(errors(r#"keys { bind "t" "todya"; }"#).contains("did you mean `today`"));
        assert!(errors(r#"keys { bind "today"; }"#).contains("one or more keys"));
        assert!(errors(r#"keys { unbind "F12"; }"#).contains("isn't bound"));
        assert!(errors(r#"keys "visual" { }"#).contains("keys context"));
        assert!(errors(r#"view "Grid" { keys { bind "x" "cycle nope"; } }"#).contains("no param `nope`"));

        // Overlays: a name, settings, and perhaps a key.
        let (c, diags) = load_config(r#"overlay "Calm" key="x" { rail "daylight"; zones "primary"; }"#);
        assert!(diags.is_empty(), "{diags:?}");
        let calm = c.overlays.iter().find(|o| o.name == "Calm").unwrap();
        assert_eq!(calm.set, [("rail".to_string(), "daylight".to_string()), ("zones".to_string(), "primary".to_string())]);
        assert_eq!(c.keys.lookup(Ctx::Normal, &press("x")), Some(&Command::Overlay("Calm".into())));
        assert!(c.overlays.iter().any(|o| o.name == "Travel"));
        assert!(errors(r#"overlay "Bad" { rail "dayligth"; }"#).contains("did you mean `daylight`"));
        assert!(errors(r#"keys { bind "x" "overlay Nope"; }"#).contains("no overlay named `Nope`"));
        // A split names a packed view, hidden or not.
        let view = |extra: &str| format!(r#"view "Mine" {{ facet per-line=1 dir="rows" band=1; mark "block"; stops {{ stop "Day" cells=1; }}; {extra} }}"#);
        assert!(errors(&view(r#"split "Upcoming" size=0.4"#)).is_empty());
        assert!(errors(&view(r#"split "Grid""#)).contains("needs `facet … pack=#true`"));
        assert!(errors(&view(r#"split "Upcomin""#)).contains("did you mean `Upcoming`"));
        assert!(errors(&view(r#"split "Upcoming" size=0.9"#)).contains("between 0.15 and 0.7"));
        // A view's keys may set session channels it has no param for.
        let ok = errors(r#"view "Mine" { facet per-line=7 dir="rows" band=13; mark "block"; stops { stop "Week" cells=7; }; keys { bind "x" "toggle rail=travel"; } }"#);
        assert!(ok.is_empty(), "{ok}");
    }

    #[test]
    fn unknown_property_suggests() {
        let (_, diags) = load_str("view \"X\" {\n  facet per-line=7 dir=\"rows\" band=13\n  mark \"block\"\n  stops { stop \"Day\" cells=1 }\n  color measur=\"hours\"\n}\n");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!((diags[0].line, diags[0].message.contains("did you mean `measure`")), (5, true));
    }

    #[test]
    fn undeclared_param_reference_is_an_error() {
        let (_, diags) = load_str("view \"X\" {\n  facet per-line=7 dir=\"rows\" band=13\n  mark \"block\"\n  stops { stop \"Day\" cells=1 }\n  color measure=\"$colour\"\n}\n");
        assert!(diags.iter().any(|d| d.message.contains("no param `colour`")), "{diags:?}");
    }

    #[test]
    fn user_views_replace_hide_and_reorder() {
        let (views, diags) = load_str(
            "hide \"Dial\"\norder \"Density\" \"Grid\"\nview \"Grid\" {\n  facet per-line=7 dir=\"rows\" band=13\n  x \"lane\"\n  y \"time-of-day\"\n  mark \"block\"\n  stops { stop \"Week\" cells=7 }\n}\n",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let names: Vec<_> = views.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["Density", "Grid", "Lanes", "Energy", "Today"]);
        assert_eq!(views[1].stops.len(), 1);
    }

    #[test]
    fn param_values_follow_level_of_detail() {
        let (views, _) = load_str("");
        let week = &views[0];
        let mut values = week.defaults();
        // Calendar colouring for events; at aggregate scale it falls back to the first measure.
        assert!(week.color_enc(&values, Mark::Block).by_calendar);
        assert_eq!(week.value("measure", &values, Mark::Heat), "hours");
        values.insert("measure".into(), "strain".into());
        assert_eq!(week.color_enc(&values, Mark::Block).measure, Measure::Strain);
        assert!(!week.color_enc(&values, Mark::Block).by_calendar);
        // By calendar is on by default: strain in each calendar's hue; off gives the strain scale.
        assert_eq!(week.color_enc(&values, Mark::Block).palette, crate::color::Palette::Calendar);
        values.insert("split".into(), "false".into());
        assert_eq!(week.color_enc(&values, Mark::Block).palette, crate::color::Palette::Scale);
        // And the toggle isn't offered while colouring by calendar already.
        values.insert("measure".into(), "calendar".into());
        assert!(!week.active(week.param("split").unwrap(), &values, Mark::Block));
    }
}

// --- In-app edits ---------------------------------------------------------------
//
// The editor never rewrites calm.kdl. It copies a view's node into views.generated.kdl
// and changes only the channels it edits there, so params, `use`, stops and comments
// survive. Every write is checked by a full reload and undone if it would not load.

fn write_generated(edit: impl FnOnce(&mut KdlDocument)) -> Result<(), String> {
    let path = generated_path().ok_or("no config directory")?;
    let before = std::fs::read_to_string(&path).ok();
    let mut doc: KdlDocument = before.as_deref().unwrap_or("").parse().map_err(|e| format!("{}: {e}", path.display()))?;
    if before.is_none() {
        doc = "// Written by calm's view editor. Edit freely, or move views into calm.kdl.\n".parse().unwrap_or_default();
    }
    edit(&mut doc);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("kdl.tmp");
    std::fs::write(&tmp, doc.to_string()).and_then(|_| std::fs::rename(&tmp, &path)).map_err(|e| e.to_string())?;
    let (_, diags) = load();
    if let Some(d) = diags.first() {
        match before {
            Some(b) => std::fs::write(&path, b).ok(),
            None => std::fs::remove_file(&path).ok(),
        };
        return Err(d.to_string());
    }
    Ok(())
}

fn view_index(doc: &KdlDocument, name: &str) -> Option<usize> {
    doc.nodes().iter().position(|n| n.name().value() == "view" && n.entries().first().and_then(|e| e.value().as_string()) == Some(name))
}

/// Sets one child's first argument (`x "lane"`) or a property (`facet per-line=7`) in `view`'s
/// node, creating the child if needed. Values are strings or integers.
pub fn edit_view(config: &Config, view: &str, child: &str, prop: Option<&str>, value: &str) -> Result<(), String> {
    let i = config.views.iter().position(|v| v.name == view).ok_or("no such view")?;
    let original: KdlDocument = config.view_text[i].parse().map_err(|e| format!("{e}"))?;
    let original = original.nodes().first().cloned().ok_or("empty view")?;
    let value: KdlValue = value.parse::<i128>().map(KdlValue::Integer).unwrap_or_else(|_| KdlValue::String(value.into()));
    write_generated(|doc| {
        let at = match view_index(doc, view) {
            Some(at) => at,
            None => {
                doc.nodes_mut().push(original);
                doc.nodes().len() - 1
            }
        };
        let node = &mut doc.nodes_mut()[at];
        let children = node.ensure_children();
        let c = match children.nodes().iter().position(|n| n.name().value() == child) {
            Some(c) => c,
            None => {
                children.nodes_mut().push(KdlNode::new(child));
                children.nodes().len() - 1
            }
        };
        let c = &mut children.nodes_mut()[c];
        match prop {
            Some(p) => {
                c.entries_mut().retain(|e| e.name().is_none_or(|n| n.value() != p));
                c.push((p, value));
            }
            None => match c.entries_mut().iter_mut().find(|e| e.name().is_none()) {
                Some(e) => *e = KdlEntry::new(value),
                None => {
                    c.insert(0, KdlEntry::new(value));
                }
            },
        }
        doc.autoformat();
    })
}

/// Copies a view under a fresh name ("Grid copy", "Grid copy 2", …); returns the name.
pub fn duplicate_view(config: &Config, view: &str) -> Result<String, String> {
    let i = config.views.iter().position(|v| v.name == view).ok_or("no such view")?;
    let name = (1..)
        .map(|n| if n == 1 { format!("{view} copy") } else { format!("{view} copy {n}") })
        .find(|n| !config.views.iter().any(|v| v.name == *n))
        .unwrap_or_default();
    let doc: KdlDocument = config.view_text[i].parse().map_err(|e| format!("{e}"))?;
    let mut node = doc.nodes().first().cloned().ok_or("empty view")?;
    if let Some(e) = node.entries_mut().first_mut() {
        *e = KdlEntry::new(name.clone());
    }
    write_generated(|doc| {
        doc.nodes_mut().push(node);
        doc.autoformat();
    })?;
    Ok(name)
}

/// Drops the app's edits to a view: back to calm.kdl or built-in, or gone if the app made it.
pub fn reset_view(view: &str) -> Result<(), String> {
    write_generated(|doc| {
        if let Some(i) = view_index(doc, view) {
            doc.nodes_mut().remove(i);
        }
    })
}

fn report(r: Result<String, String>) -> i32 {
    match r {
        Ok(m) => {
            println!("{m}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// "7:30" (or "7") as hours.
fn clock_hours(s: &str) -> Option<f32> {
    let (h, m) = s.split_once(':').unwrap_or((s, "0"));
    let (h, m) = (h.trim().parse::<f32>().ok()?, m.trim().parse::<f32>().ok()?);
    (h >= 0. && (0. ..60.).contains(&m)).then_some(h + m / 60.)
}

/// Sets one property of a top-level settings node (e.g. `energy reactive=#false`) in the
/// editor's file. It layers over calm.kdl, which is never rewritten.
pub fn edit_setting(node: &str, prop: &str, value: KdlValue) -> Result<(), String> {
    write_generated(|doc| {
        let at = match doc.nodes().iter().position(|n| n.name().value() == node) {
            Some(at) => at,
            None => {
                doc.nodes_mut().push(KdlNode::new(node));
                doc.nodes().len() - 1
            }
        };
        let n = &mut doc.nodes_mut()[at];
        n.entries_mut().retain(|e| e.name().is_none_or(|k| k.value() != prop));
        n.push((prop, value));
        doc.autoformat();
    })
}
