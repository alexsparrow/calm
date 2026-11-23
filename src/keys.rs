//! Keys: chords bound to commands, per context.
//!
//! Every key does a command, written as text (`pan -1`, `cycle measure`,
//! `toggle rail=travel`), so the same commands run from the `:` prompt, and a keymap is
//! data: the built-ins (`assets/keys.kdl`), then yours, then a view's own. The view
//! runs commands; this module only reads them and matches keys.

use std::fmt;

/// Where a key is pressed. Modes and the search field look their keys up first; a mode
/// falls back to the view's keys and then the normal ones, the search field doesn't (keys
/// there are text).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Ctx {
    Normal,
    Create,
    Adjust,
    Search,
}

impl Ctx {
    pub const NAMES: &[(&str, Ctx)] = &[("normal", Ctx::Normal), ("create", Ctx::Create), ("adjust", Ctx::Adjust), ("search", Ctx::Search)];

    pub fn name(self) -> &'static str {
        Self::NAMES.iter().find(|(_, c)| *c == self).map_or("normal", |(n, _)| n)
    }
}

/// A key as pressed: what's needed to match it, independent of the UI toolkit.
#[derive(Clone, Debug, Default)]
pub struct Press {
    /// The key itself (`n`, `left`, `/`).
    pub key: String,
    /// What it types, if anything (`N` for Shift+N, `?` for Shift+/).
    pub typed: Option<String>,
    pub ctrl: bool,
    pub alt: bool,
    pub cmd: bool,
    pub shift: bool,
}

/// A key and its modifiers, as written in config: `n`, `N` (Shift+N), `?`, `ctrl-n`,
/// `shift-tab`, `alt-left`.
#[derive(Clone, PartialEq, Eq, Debug, Hash)]
pub struct Chord {
    key: String,
    ctrl: bool,
    alt: bool,
    cmd: bool,
    shift: bool,
}

/// Keys with names rather than characters.
const NAMED: &[&str] = &[
    "left", "right", "up", "down", "tab", "enter", "escape", "space", "backspace", "delete", "home", "end", "pageup", "pagedown", "insert", "f1", "f2", "f3",
    "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12",
];

impl Chord {
    pub fn parse(s: &str) -> Result<Chord, String> {
        // The key is last; `-` may itself be the key (`-`, `ctrl--`).
        let (mods, key) = match s.strip_suffix('-') {
            Some(rest) if rest.is_empty() || rest.ends_with('-') => (rest.strip_suffix('-').unwrap_or(rest), "-"),
            _ => s.rsplit_once('-').unwrap_or(("", s)),
        };
        let mut c = Chord { key: String::new(), ctrl: false, alt: false, cmd: false, shift: false };
        for m in mods.split('-').filter(|m| !m.is_empty()) {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" => c.ctrl = true,
                "alt" => c.alt = true,
                "cmd" | "super" => c.cmd = true,
                "shift" => c.shift = true,
                other => return Err(format!("`{other}` isn't a modifier (ctrl, alt, shift, cmd)")),
            }
        }
        let mut chars = key.chars();
        match (chars.next(), chars.next()) {
            (Some(ch), None) if ch.is_ascii_uppercase() => {
                c.key = ch.to_ascii_lowercase().to_string();
                c.shift = true;
            }
            (Some(ch), None) if !ch.is_whitespace() => c.key = ch.to_string(),
            _ if NAMED.contains(&key.to_ascii_lowercase().as_str()) => c.key = key.to_ascii_lowercase(),
            _ => return Err(format!("`{key}` isn't a key: use one character, or {}", NAMED[..12].join(", "))),
        }
        // Shift with a symbol is the symbol it types (`?`, not `shift-/`).
        if c.shift && c.is_symbol() {
            return Err(format!("write the character Shift types (e.g. `?`, `:`) rather than `{s}`"));
        }
        Ok(c)
    }

    fn is_symbol(&self) -> bool {
        self.key.chars().count() == 1 && !self.key.chars().all(|c| c.is_ascii_alphabetic())
    }

    pub fn matches(&self, p: &Press) -> bool {
        if (self.ctrl, self.alt, self.cmd) != (p.ctrl, p.alt, p.cmd) {
            return false;
        }
        if self.is_symbol() {
            // Whatever it took to type it: `?` is Shift+/ on one layout and not on another.
            return p.typed.as_deref() == Some(self.key.as_str()) || (p.key == self.key && !p.shift);
        }
        p.key.to_ascii_lowercase() == self.key && p.shift == self.shift
    }

    /// For the help card: `N`, `Shift+N`, `Ctrl+N`, `←`, `Esc`.
    pub fn label(&self) -> String {
        let key = match self.key.as_str() {
            "left" => "\u{2190}".into(),
            "right" => "\u{2192}".into(),
            "up" => "\u{2191}".into(),
            "down" => "\u{2193}".into(),
            "escape" => "Esc".into(),
            "pageup" => "PgUp".into(),
            "pagedown" => "PgDn".into(),
            k if k.chars().count() == 1 => k.to_uppercase(),
            k => {
                let mut c = k.chars();
                c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
            }
        };
        let mods = [(self.ctrl, "Ctrl+"), (self.alt, "Alt+"), (self.cmd, "Super+"), (self.shift, "Shift+")];
        mods.iter().filter(|(on, _)| *on).map(|(_, m)| *m).collect::<String>() + &key
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mods = [(self.ctrl, "ctrl-"), (self.alt, "alt-"), (self.cmd, "cmd-")];
        for (_, m) in mods.iter().filter(|(on, _)| *on) {
            f.write_str(m)?;
        }
        if self.shift && self.key.chars().count() == 1 {
            f.write_str(&self.key.to_uppercase())
        } else {
            write!(f, "{}{}", if self.shift { "shift-" } else { "" }, self.key)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    pub fn delta(self) -> (i32, i32) {
        match self {
            Dir::Left => (-1, 0),
            Dir::Right => (1, 0),
            Dir::Up => (0, -1),
            Dir::Down => (0, 1),
        }
    }
}

/// Something calm can do.
#[derive(Clone, PartialEq, Debug)]
pub enum Command {
    /// Back or forward a page (`pan -1`, `pan 1`).
    Pan(i32),
    /// Scroll the hours, in views that have them; else pan.
    Scroll(i32),
    /// `zoom in` (towards a day) or `zoom out` (towards a year): -1 or 1.
    Zoom(i32),
    Today,
    FullDay,
    /// `view 3`: by position.
    View(usize),
    /// `view next` / `view prev`.
    ViewStep(i32),
    Search,
    /// The `:` prompt: a date to go to, or a command.
    Prompt,
    /// The next or previous search match (`match next` / `match prev`).
    Match(i32),
    /// Complete the search field's facet (`complete`).
    Complete,
    Create,
    Adjust,
    Delete,
    Changes,
    Sync,
    Editor,
    Menu,
    Theme,
    Help,
    Stats,
    /// Close whatever's open, or let go of the selection.
    Close,
    /// Triple the demo data.
    Stress,
    /// Modes: move the cursor, or nudge the selected event (`step left`).
    Step(Dir),
    /// Adjust: resize the selected event from its end (`stretch left`).
    Stretch(Dir),
    /// Create: set the start, then the end, at the cursor.
    Place,
    /// Leave the mode.
    Done,
    /// Back a step in a mode: drop a half-placed event or a drag, else leave.
    Back,
    /// `snap finer` / `snap coarser`.
    Snap(i32),
    FreeOnly,
    /// A view param: toggles flip, the rest step through their options (`cycle measure`).
    Cycle(String),
    /// `set rail=daylight zones=primary`.
    Set(Vec<(String, String)>),
    /// `toggle rail=travel zones=visible`: set them, or put back what they were.
    Toggle(Vec<(String, String)>),
    /// A named toggle from config (`overlay "Travel" { rail "travel"; zones "visible" }`).
    Overlay(String),
}

/// How the help card files a command: its section, and a row it shares with its opposite.
pub struct Describe {
    pub group: Group,
    /// Commands with the same row share a line (`← →  Previous / next`).
    pub row: &'static str,
    pub text: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Around,
    Finding,
    Changing,
    Looking,
    Mode,
    Typing,
    /// Not listed.
    Hidden,
}

/// Commands that take no argument, by name.
const PLAIN: &[(&str, Command)] = &[
    ("today", Command::Today),
    ("full-day", Command::FullDay),
    ("search", Command::Search),
    ("prompt", Command::Prompt),
    ("complete", Command::Complete),
    ("create", Command::Create),
    ("adjust", Command::Adjust),
    ("delete", Command::Delete),
    ("changes", Command::Changes),
    ("sync", Command::Sync),
    ("editor", Command::Editor),
    ("menu", Command::Menu),
    ("theme", Command::Theme),
    ("help", Command::Help),
    ("stats", Command::Stats),
    ("close", Command::Close),
    ("stress", Command::Stress),
    ("place", Command::Place),
    ("done", Command::Done),
    ("back", Command::Back),
    ("free-only", Command::FreeOnly),
];

/// Every command's first word, for suggestions.
pub const NAMES: &[&str] = &[
    "pan", "scroll", "zoom", "today", "full-day", "view", "search", "prompt", "match", "complete", "create", "adjust", "delete", "changes", "sync", "editor",
    "menu", "theme", "help", "stats", "close", "stress", "step", "stretch", "place", "done", "back", "snap", "free-only", "cycle", "set",
    "toggle", "overlay",
];

fn sign(s: &str, neg: &[&str], pos: &[&str]) -> Option<i32> {
    if neg.contains(&s) {
        Some(-1)
    } else if pos.contains(&s) {
        Some(1)
    } else {
        None
    }
}

fn dir(s: &str) -> Option<Dir> {
    Some(match s {
        "left" => Dir::Left,
        "right" => Dir::Right,
        "up" => Dir::Up,
        "down" => Dir::Down,
        _ => return None,
    })
}

fn assignments(words: &[&str]) -> Result<Vec<(String, String)>, String> {
    if words.is_empty() {
        return Err("expected name=value".into());
    }
    words
        .iter()
        .map(|w| match w.split_once('=') {
            Some((k, v)) if !k.is_empty() && !v.is_empty() => Ok((k.to_string(), v.to_string())),
            _ => Err(format!("`{w}` should be name=value")),
        })
        .collect()
}

impl Command {
    pub fn parse(s: &str) -> Result<Command, String> {
        let words: Vec<&str> = s.split_whitespace().collect();
        let Some((&head, rest)) = words.split_first() else { return Err("empty command".into()) };
        let one = |what: &str| -> Result<&str, String> {
            match rest {
                [w] => Ok(w),
                _ => Err(format!("`{head}` takes {what}")),
            }
        };
        let none = |c: Command| if rest.is_empty() { Ok(c) } else { Err(format!("`{head}` takes nothing after it")) };
        Ok(match head {
            "pan" | "scroll" => {
                let n = one("-1 or 1")?.parse::<i32>().ok().filter(|n| *n != 0).ok_or_else(|| format!("`{head}` takes a number of steps, like -1 or 1"))?;
                if head == "pan" { Command::Pan(n) } else { Command::Scroll(n) }
            }
            "zoom" => Command::Zoom(sign(one("in or out")?, &["in"], &["out"]).ok_or("`zoom` takes in or out")?),
            "view" => match one("a number, next or prev")? {
                w => match (sign(w, &["prev", "previous"], &["next"]), w.parse::<usize>()) {
                    (Some(d), _) => Command::ViewStep(d),
                    (_, Ok(n)) if n >= 1 => Command::View(n - 1),
                    _ => return Err("`view` takes a number (1 is the first), next or prev".into()),
                },
            },
            "match" => Command::Match(sign(one("next or prev")?, &["prev", "previous"], &["next"]).ok_or("`match` takes next or prev")?),
            "snap" => Command::Snap(sign(one("finer or coarser")?, &["finer"], &["coarser"]).ok_or("`snap` takes finer or coarser")?),
            "step" | "stretch" => {
                let d = dir(one("left, right, up or down")?).ok_or_else(|| format!("`{head}` takes left, right, up or down"))?;
                if head == "step" { Command::Step(d) } else { Command::Stretch(d) }
            }
            "cycle" => Command::Cycle(one("a param name")?.to_string()),
            "overlay" if !rest.is_empty() => Command::Overlay(rest.join(" ")),
            "overlay" => return Err("`overlay` takes an overlay's name".into()),
            "set" => Command::Set(assignments(rest)?),
            // `toggle span` flips a toggle param; `toggle a=b …` sets or restores.
            "toggle" => match rest {
                [w] if !w.contains('=') => Command::Cycle(w.to_string()),
                _ => Command::Toggle(assignments(rest)?),
            },
            _ => match PLAIN.iter().find(|(n, _)| *n == head) {
                Some((_, c)) => none(c.clone())?,
                None => {
                    let close = NAMES.iter().min_by_key(|n| strsim(n, head)).filter(|n| strsim(n, head) <= 2);
                    return Err(format!("unknown command `{head}`{}", close.map(|n| format!(" (did you mean `{n}`?)")).unwrap_or_default()));
                }
            },
        })
    }

    /// The params it reads or writes, to check against a view's.
    pub fn params(&self) -> Vec<&str> {
        match self {
            Command::Cycle(p) => vec![p.as_str()],
            Command::Set(a) | Command::Toggle(a) => a.iter().map(|(k, _)| k.as_str()).collect(),
            _ => Vec::new(),
        }
    }

    pub fn describe(&self) -> Describe {
        use Group::*;
        let d = |group, row, text: &str| Describe { group, row, text: text.to_string() };
        match self {
            Command::Pan(_) => d(Around, "pan", "Previous / next"),
            Command::Scroll(_) => d(Around, "scroll", "Scroll hours or time"),
            Command::Zoom(_) => d(Around, "zoom", "Zoom (day \u{2026} year)"),
            Command::View(_) => d(Around, "view", "Switch view"),
            Command::ViewStep(_) => d(Around, "view-step", "Next / previous view"),
            Command::FullDay => d(Around, "full-day", "Full day / default hours"),
            Command::Today => d(Around, "today", "Today"),
            Command::Search => d(Finding, "search", "Search (with: in: at: series: is:)"),
            Command::Match(_) => d(Finding, "match", "Next / previous match"),
            Command::Prompt => d(Finding, "prompt", "Go to a date (31, 12-31, dec, +2w), or a command"),
            Command::Complete => d(Typing, "complete", "Complete the facet"),
            Command::Create => d(Changing, "create", "Schedule an event"),
            Command::Adjust => d(Changing, "adjust", "Adjust: move, resize"),
            Command::Delete => d(Changing, "delete", "Delete the selected event"),
            Command::Changes => d(Changing, "changes", "Changes waiting to sync"),
            Command::Sync => d(Changing, "sync", "Sync now"),
            Command::Editor => d(Looking, "editor", "Edit this view"),
            Command::Menu => d(Looking, "menu", "Menu: more calendars, actions"),
            Command::Theme => d(Looking, "theme", "Light / dark"),
            Command::Close => d(Looking, "close", "Close"),
            Command::Help => d(Looking, "help", "These keys"),
            Command::Stats => d(Looking, "stats", "Frame stats"),
            Command::Stress => d(Hidden, "stress", ""),
            Command::Step(_) => d(Mode, "step", "Move the cursor, or the event"),
            Command::Stretch(_) => d(Mode, "stretch", "Resize the event"),
            Command::Place => d(Mode, "place", "Set the start, then the end"),
            Command::Done => d(Mode, "done", "Done"),
            Command::Back => d(Mode, "back", "Back a step, or leave"),
            Command::Snap(_) => d(Mode, "snap", "Snap finer / coarser"),
            Command::FreeOnly => d(Mode, "free-only", "Free time only"),
            Command::Cycle(p) => Describe { group: Looking, row: "", text: format!("Cycle {p}") },
            Command::Set(a) => Describe { group: Looking, row: "", text: format!("Set {}", show(a)) },
            Command::Toggle(a) => Describe { group: Looking, row: "", text: format!("Toggle {}", show(a)) },
            Command::Overlay(name) => Describe { group: Looking, row: "", text: name.clone() },
        }
    }
}

impl fmt::Display for Command {
    /// As it would be written: `pan -1`, `toggle rail=travel`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let word = |n: i32, neg: &str, pos: &str| if n < 0 { neg.to_string() } else { pos.to_string() };
        let d = |d: &Dir| format!("{d:?}").to_lowercase();
        let assign = |a: &[(String, String)]| a.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ");
        let s = match self {
            Command::Pan(n) => format!("pan {n}"),
            Command::Scroll(n) => format!("scroll {n}"),
            Command::Zoom(n) => format!("zoom {}", word(*n, "in", "out")),
            Command::View(i) => format!("view {}", i + 1),
            Command::ViewStep(n) => format!("view {}", word(*n, "prev", "next")),
            Command::Match(n) => format!("match {}", word(*n, "prev", "next")),
            Command::Snap(n) => format!("snap {}", word(*n, "finer", "coarser")),
            Command::Step(x) => format!("step {}", d(x)),
            Command::Stretch(x) => format!("stretch {}", d(x)),
            Command::Cycle(p) => format!("cycle {p}"),
            Command::Set(a) => format!("set {}", assign(a)),
            Command::Toggle(a) => format!("toggle {}", assign(a)),
            Command::Overlay(name) => format!("overlay {name}"),
            c => PLAIN.iter().find(|(_, p)| p == c).map(|(n, _)| n.to_string()).unwrap_or_default(),
        };
        f.write_str(&s)
    }
}

fn show(a: &[(String, String)]) -> String {
    a.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join(", ")
}

/// Edit distance, for suggestions.
fn strsim(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(row[j + 1] + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

#[derive(Clone, Debug)]
pub struct Binding {
    pub chord: Chord,
    pub command: Command,
    /// What the help card says, if not the command's own description.
    pub help: Option<String>,
}

/// Bindings by context, latest first: a later `bind` of the same chord replaces the earlier.
#[derive(Clone, Debug, Default)]
pub struct Keymap {
    pub contexts: Vec<(Ctx, Vec<Binding>)>,
}

impl Keymap {
    fn list(&mut self, ctx: Ctx) -> &mut Vec<Binding> {
        match self.contexts.iter().position(|(c, _)| *c == ctx) {
            Some(i) => &mut self.contexts[i].1,
            None => {
                self.contexts.push((ctx, Vec::new()));
                &mut self.contexts.last_mut().unwrap().1
            }
        }
    }

    pub fn bind(&mut self, ctx: Ctx, chord: Chord, command: Command, help: Option<String>) {
        let list = self.list(ctx);
        match list.iter_mut().find(|b| b.chord == chord) {
            Some(b) => (b.command, b.help) = (command, help),
            None => list.push(Binding { chord, command, help }),
        }
    }

    /// Whether it was bound.
    pub fn unbind(&mut self, ctx: Ctx, chord: &Chord) -> bool {
        let list = self.list(ctx);
        let n = list.len();
        list.retain(|b| b.chord != *chord);
        list.len() < n
    }

    pub fn get(&self, ctx: Ctx) -> &[Binding] {
        self.contexts.iter().find(|(c, _)| *c == ctx).map_or(&[], |(_, l)| l.as_slice())
    }

    pub fn lookup(&self, ctx: Ctx, p: &Press) -> Option<&Command> {
        self.get(ctx).iter().find(|b| b.chord.matches(p)).map(|b| &b.command)
    }
}

/// Help card rows for some bindings: (keys, what), commands sharing a row merged, in the
/// order first bound. Keys for a pair are zipped (`← →, H L`); a run of five or more
/// (`1` … `9`) shows its ends.
pub fn rows<'a>(bindings: impl IntoIterator<Item = &'a Binding>, group: impl Fn(&Describe) -> bool) -> Vec<(String, String)> {
    // Row → (text, members: (command, its key labels)).
    let mut rows: Vec<(String, String, Vec<(&Command, Vec<String>)>)> = Vec::new();
    for b in bindings {
        let d = b.command.describe();
        if !group(&d) {
            continue;
        }
        let id = if d.row.is_empty() || b.help.is_some() { format!("{:?}", b.command) } else { d.row.to_string() };
        let i = match rows.iter().position(|r| r.0 == id) {
            Some(i) => i,
            None => {
                rows.push((id, b.help.clone().unwrap_or(d.text), Vec::new()));
                rows.len() - 1
            }
        };
        let members = &mut rows[i].2;
        match members.iter_mut().find(|(c, _)| **c == b.command) {
            Some((_, keys)) if keys.contains(&b.chord.label()) => {}
            Some((_, keys)) => keys.push(b.chord.label()),
            None => members.push((&b.command, vec![b.chord.label()])),
        }
    }
    rows.into_iter()
        .map(|(_, text, members)| {
            let keys = match members.len() {
                1 => members[0].1.join(", "),
                n if n > 4 => format!("{} \u{2013} {}", members[0].1[0], members[n - 1].1[0]),
                _ => {
                    let deep = members.iter().map(|m| m.1.len()).max().unwrap_or(0);
                    (0..deep).map(|k| joined(members.iter().filter_map(|m| m.1.get(k).cloned()).collect())).collect::<Vec<_>>().join(", ")
                }
            };
            (keys, text)
        })
        .collect()
}

/// Keys side by side; three or more with the same modifiers say them once
/// (`Shift+← → ↑ ↓`).
fn joined(keys: Vec<String>) -> String {
    let prefix = |k: &str| k.rfind('+').filter(|&i| i + 1 < k.len()).map_or(String::new(), |i| k[..=i].to_string());
    let first = prefix(&keys[0]);
    if keys.len() >= 3 && !first.is_empty() && keys.iter().all(|k| prefix(k) == first) {
        return first + &keys.iter().map(|k| &k[prefix(k).len()..]).collect::<Vec<_>>().join(" ");
    }
    keys.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: &str, typed: Option<&str>, shift: bool, ctrl: bool) -> Press {
        Press { key: key.into(), typed: typed.map(str::to_string), ctrl, shift, ..Default::default() }
    }

    #[test]
    fn chords() {
        let c = |s| Chord::parse(s).unwrap();
        assert!(c("n").matches(&press("n", Some("n"), false, false)));
        assert!(!c("n").matches(&press("n", Some("N"), true, false)));
        assert!(c("N").matches(&press("n", Some("N"), true, false)));
        assert!(c("N").matches(&press("N", Some("N"), true, false)));
        assert!(c("?").matches(&press("/", Some("?"), true, false)));
        assert!(c(":").matches(&press(";", Some(":"), true, false)));
        assert!(!c("/").matches(&press("/", Some("?"), true, false)));
        assert!(c("ctrl-n").matches(&press("n", None, false, true)));
        assert!(!c("n").matches(&press("n", None, false, true)));
        assert!(c("shift-tab").matches(&press("tab", None, true, false)));
        assert!(!c("tab").matches(&press("tab", None, true, false)));
        assert!(c("-").matches(&press("-", Some("-"), false, false)));
        assert!(c("ctrl--").matches(&press("-", None, false, true)));
        assert_eq!(c("ctrl-N").label(), "Ctrl+Shift+N");
        assert_eq!(c("shift-left").label(), "Shift+\u{2190}");
        assert_eq!(c("escape").label(), "Esc");
        assert_eq!(c("H").to_string(), "H");
        assert!(Chord::parse("shift-/").is_err());
        assert!(Chord::parse("hyper-x").is_err());
        assert!(Chord::parse("leftt").is_err());
    }

    #[test]
    fn commands() {
        assert_eq!(Command::parse("pan -1"), Ok(Command::Pan(-1)));
        assert_eq!(Command::parse("view 3"), Ok(Command::View(2)));
        assert_eq!(Command::parse("view prev"), Ok(Command::ViewStep(-1)));
        assert_eq!(Command::parse("toggle span"), Ok(Command::Cycle("span".into())));
        assert_eq!(Command::parse("toggle rail=travel zones=visible"), Ok(Command::Toggle(vec![("rail".into(), "travel".into()), ("zones".into(), "visible".into())])));
        assert!(Command::parse("set rail").is_err());
        assert!(Command::parse("today now").is_err());
        assert!(Command::parse("todya").unwrap_err().contains("`today`"));
        for text in ["pan -1", "zoom in", "view 3", "view prev", "step left", "toggle rail=travel zones=visible", "cycle measure", "free-only"] {
            assert_eq!(Command::parse(text).unwrap().to_string(), text);
        }
    }

    #[test]
    fn keymap_rows() {
        let mut k = Keymap::default();
        fn b(k: &mut Keymap, key: &str, cmd: &str) {
            k.bind(Ctx::Normal, Chord::parse(key).unwrap(), Command::parse(cmd).unwrap(), None);
        }
        b(&mut k, "left", "pan -1");
        b(&mut k, "right", "pan 1");
        b(&mut k, "h", "pan -1");
        b(&mut k, "l", "pan 1");
        for n in 1..=9 {
            b(&mut k, &n.to_string(), &format!("view {n}"));
        }
        b(&mut k, "t", "today");
        b(&mut k, "t", "sync");
        let rows = rows(k.get(Ctx::Normal), |_| true);
        assert_eq!(rows[0], ("\u{2190} \u{2192}, H L".to_string(), "Previous / next".to_string()));
        assert_eq!(rows[1].0, "1 \u{2013} 9");
        assert_eq!(rows[2], ("T".to_string(), "Sync now".to_string()));
        assert!(k.unbind(Ctx::Normal, &Chord::parse("t").unwrap()));
        assert!(k.lookup(Ctx::Normal, &press("t", Some("t"), false, false)).is_none());
    }
}
