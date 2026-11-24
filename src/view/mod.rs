//! The calendar view: owns the dataset, per-lens zoom state, mark animation
//! and input.
//!
//! Marks are laid out in inner cell coordinates and projected through the
//! lens's facet arrangement every frame, so zooming and scrolling track input
//! exactly. Semantic changes (lens, level of detail, hidden calendars) morph
//! in *screen space*: each mark remembers the shape it was last drawn with
//! and springs from there to its live target, so a block can fold into an
//! arc or collapse into a month-view row whatever the projection does.

mod arrange;
mod chrome;
mod commands;
mod editing;
mod hints;
mod input;
mod modes;
mod search;
mod scene;

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use chrono::Datelike;
use gpui::{
    App, Context, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, Render, ScrollDelta,
    ScrollWheelEvent, Subscription, Window, WindowAppearance, canvas, div, prelude::*,
};

use crate::anim::{GENTLE, SNAPPY, Spring, SpringCfg};
use crate::color::{self, CellStats, ColorEnc};
use crate::layout::{self, LabelMode, Target};
use crate::model::{self, Calendar, DAYS, Dataset, MAX_CALENDARS};
use crate::spec::{Align, Axis, Coord, Detail, Dir, Extent, Mark, Spec, Values};
use crate::text::Fonts;
use crate::theme::Theme;
use arrange::{Arr, Inner, Pt};
use scene::Shape;

/// Zoom and scroll: responsive but still smoothed.
const VIEWPORT: SpringCfg = SpringCfg { stiffness: 320., damping: 36. };
/// How long input must pause before the zoom settles onto a stop.
const SNAP_AFTER: Duration = Duration::from_millis(220);
/// Left gutter when nothing is labelled beside the plot: just breathing room.
const GUTTER_MIN: f32 = 20.;
/// Bottom margin: the lens switcher and colour bar, plus a little air; and with the
/// second (compose / legend) row that aggregate views add.
/// Room for the bottom row (picker, controls, zoom): 18 up, 40 tall, and a little air.
const CONTROLS_ROW: f32 = 74.;

enum Hit {
    Rect([f32; 4]),
    Poly(Vec<Pt>),
}

impl Hit {
    fn contains(&self, (x, y): Pt) -> bool {
        match self {
            Hit::Rect(b) => x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3],
            Hit::Poly(pts) => {
                let mut inside = false;
                let mut j = pts.len() - 1;
                for i in 0..pts.len() {
                    let ((xi, yi), (xj, yj)) = (pts[i], pts[j]);
                    if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                        inside = !inside;
                    }
                    j = i;
                }
                inside
            }
        }
    }
}

struct MarkAnim {
    target: Target,
    alpha: Spring,
    /// Screen shape at the start of the current morph, if one is running.
    from: Option<Shape>,
    /// Morph progress, 0 → 1.
    t: Spring,
    delay: f32,
    /// What was drawn last frame: the starting point for the next morph.
    shown: Option<Shape>,
    /// Label-mode crossfade: fade out, swap mode, fade back in.
    label: Spring,
    label_mode: LabelMode,
    /// Whether label placement found room; springed so collisions fade.
    placed: Spring,
    hover: Spring,
}

/// Zoom, scroll and level of detail for one lens.
#[derive(Clone)]
struct Lens {
    /// log2 of the number of day cells visible.
    zoom: Spring,
    /// First visible cell (continuous).
    start: Spring,
    /// Window on the inner time axis, as fractions of a day.
    hours: [Spring; 2],
    /// The hour window this lens opens with; `F` toggles between it and the full day.
    default_hours: (f32, f32),
    mark: Mark,
    /// Visibility of the aggregate (heat) layer and of the detailed-mark furniture.
    heat: Spring,
    grid: Spring,
    last_input: Option<Instant>,
    /// Where the current scroll gesture began, so the snap can follow its direction.
    gesture_from: Option<f32>,
}

#[derive(Default)]
struct Stats {
    intervals: Vec<f32>,
    cpu: Vec<f32>,
}

impl Stats {
    fn push(&mut self, interval: f32, cpu: f32) {
        self.intervals.push(interval);
        self.cpu.push(cpu);
        if self.intervals.len() > 4096 {
            self.intervals.drain(..2048);
            self.cpu.drain(..2048);
        }
    }
    fn pct(xs: &[f32], p: f32) -> f32 {
        if xs.is_empty() {
            return 0.;
        }
        let mut v = xs.to_vec();
        v.sort_by(f32::total_cmp);
        v[((v.len() - 1) as f32 * p) as usize]
    }
    fn recent(xs: &[f32]) -> &[f32] {
        &xs[xs.len().saturating_sub(120)..]
    }
}

struct Bench {
    /// False for `--cycle`: advance forever without printing stats.
    report: bool,
    start: Instant,
    last_step: Instant,
    step: usize,
    stats: Stats,
}

pub struct CalmView {
    focus: FocusHandle,
    fonts: Fonts,
    theme: Theme,
    theme_override: Option<bool>,
    /// calm.kdl's themes, and whether the desktop is in dark mode.
    looks: crate::theme::Looks,
    system_dark: bool,
    /// Window background opacity when translucent.
    translucent: Option<f32>,
    _appearance: Subscription,
    data: Dataset,
    cells: Vec<CellStats>,
    /// The views, from built-ins and calm.kdl, and each one's current param values.
    views: Vec<Spec>,
    values: Vec<Values>,
    /// Config files and their modification times, for hot reload; problems from the last load.
    config_files: Vec<(std::path::PathBuf, Option<std::time::SystemTime>)>,
    config_problems: Vec<String>,
    /// The loaded config, for the editor (each view's KDL, which views the app has edited).
    conf: crate::viewconf::Config,
    /// What `toggle a=b …` changed, to put back (`None`: the view's own): by (view, command).
    restore: std::collections::HashMap<(String, String), Vec<(String, Option<String>)>>,
    editor: Spring,
    editor_on: bool,
    /// The editor's last outcome: (message, is_error).
    editor_note: Option<(String, bool)>,
    detail_t: Spring,
    enc_from: ColorEnc,
    enc_to: ColorEnc,
    enc_t: Spring,
    marks: Vec<MarkAnim>,
    /// Marks with a morph in flight, drawn even if their day is off screen.
    flipping: Vec<usize>,
    /// Marks drawn last frame.
    drawn_marks: Vec<usize>,
    preset: usize,
    lenses: Vec<Lens>,
    hidden: [bool; MAX_CALENDARS],
    /// The calendars, in chip order; slots past the end stay hidden.
    calendars: Vec<Calendar>,
    /// How energy is modelled (calm.kdl's `energy`).
    energy: crate::energy::EnergyModel,
    /// Time-zone moves found in your events (see `crate::travel`), and whether they're shown
    /// (`W`: the zone rail in every timed view, and hour columns for the zones they visit).
    travel: Option<crate::travel::Travel>,
    /// Channels set for this session on every view (`set rail=travel`), over the views' own.
    over: crate::spec::Values,
    /// Where you are, for daylight: calm.kdl's `location`, else your zone's city.
    place: Option<crate::sun::Place>,
    now_min: f32,
    dirty: bool,
    stagger: bool,
    viewport: (f32, f32),
    layers: Vec<Spring>,
    /// Plot margins that fit what is actually drawn beside the plot.
    margin_l: Spring,
    margin_b: Spring,
    margin_t: Spring,
    /// Extra time zones (a global setting) and whether they are shown.
    zones: crate::zones::Zones,
    /// Live sync, when sources are configured.
    sync: Option<Sync>,
    /// What the command line pinned, which config reloads must not override.
    pinned: Pinned,
    /// Zone labels drawn this frame, for hover: (rect, zone; `None` is primary).
    zone_hits: std::cell::RefCell<Vec<([f32; 4], Option<chrono_tz::Tz>)>>,
    hover_zone: Option<Option<chrono_tz::Tz>>,
    switcher: Spring,
    drawer: Spring,
    tooltip: Spring,
    help: Spring,
    help_on: bool,
    /// The top-right menu (secondary calendars, actions).
    menu: Spring,
    menu_on: bool,
    /// The view's controls, popped up from their button when they don't fit the bottom row.
    params_open: bool,
    /// The drawer's text fields, for an event you may edit.
    drawer_edit: Option<editing::DrawerEdit>,
    /// A note at the bottom (e.g. "Deleted · Undo").
    toast: Option<editing::Toast>,
    /// Events whose guests you chose not to notify, by (store key, event ID).
    quiet: std::collections::HashSet<(String, String)>,
    /// The changes panel, and the changes you've unticked in it.
    changes_on: bool,
    changes_unticked: std::collections::HashSet<i64>,
    /// Put the cursor in the drawer's title once it appears (a new event).
    focus_title: bool,
    /// The drawer's open date or time picker.
    picker: Option<editing::Picker>,
    /// New or Adjust, when you've entered one (see `modes`); its snap step in minutes, whether
    /// it keeps to free time, where its cursor is, and the grip under the pointer.
    mode: Option<modes::Mode>,
    snap: i64,
    free_only: bool,
    cursor: Option<modes::Slot>,
    hover_grip: Option<modes::Grip>,
    /// The `/` or `:` field (and one asked for, to open on the next frame), whether its text
    /// changed since the last search, the last search's results, and a day just jumped to.
    prompt: Option<search::Prompt>,
    pending_prompt: Option<search::Kind>,
    prompt_query_dirty: bool,
    found: Option<search::Found>,
    jump_mark: Option<(i32, Instant)>,
    /// Text to open the field with (`--search`, `--jump`).
    prompt_seed: Option<String>,
    /// `--picker`: open this picker once the drawer has an event.
    pending_picker: Option<u8>,
    hud_on: bool,
    mouse: Pt,
    hits: Vec<(usize, Hit)>,
    /// Picking an event by its letters (`P`).
    hints: Option<hints::Hints>,
    pending_pick: Option<String>,
    /// A packed view's day offsets (see `Arr::pack`), from the last layout.
    pack: Option<std::sync::Arc<[f32]>>,
    /// The view beside the current one (`split`): the last one shown, so it can slide out;
    /// how far it's in (0–1); where its list starts; its layout and day offsets.
    side: Option<Box<Spec>>,
    side_t: Spring,
    side_start: Spring,
    side_targets: Vec<layout::Target>,
    side_pack: Option<std::sync::Arc<[f32]>>,
    /// Do Not Disturb, as last read (it's a file the agent and `calm dnd` share), and when.
    dnd: crate::notify::Dnd,
    dnd_read: Instant,
    /// `--hover` given: ignore real pointer moves.
    scripted_pointer: bool,
    hovered: Option<usize>,
    hover_day: Option<i32>,
    selected: Option<usize>,
    /// Last selection, kept while the drawer slides out.
    shown: Option<usize>,
    stress: bool,
    last_frame: Option<Instant>,
    cpu_last: Rc<Cell<f32>>,
    stats: Stats,
    animating: bool,
    drawn: (usize, usize, usize),
    bench: Option<Bench>,
    /// Screenshot mode: springs settle in a single frame.
    still: bool,
    /// Scripted pointer for screenshots: applied once the viewport is stable.
    pending: (Option<Pt>, Option<Pt>),
    stable_frames: u32,
    /// What the previous frame did, for explaining long intervals in benchmarks.
    last_trace: String,
}

fn stops_log(spec: &Spec) -> Vec<f32> {
    spec.stops.iter().map(|s| (s.cells as f32).log2()).collect()
}

impl CalmView {
    pub fn new(opts: Options, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let appearance = cx.observe_window_appearance(window, |this, window, cx| {
            this.system_dark = is_dark(window.appearance());
            this.retheme();
            cx.notify();
        });
        // Notice edits to calm.kdl (and its includes) and apply them live.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let alive = this.update(cx, |this, cx| {
                if stamp(&this.config_files.iter().map(|(f, _)| f.clone()).collect::<Vec<_>>()) != this.config_files {
                    this.reload_config();
                    cx.notify();
                }
            });
            if alive.is_err() {
                break;
            }
        })
        .detach();
        // Keep the now-line and clock honest without waking every frame.
        cx.spawn(async move |this, cx| {
            loop {
                // Wake just after each minute turns, so the clock and now-line move together.
                let into_minute = chrono::Local::now().timestamp_subsec_millis() as u64 + 1000 * (chrono::Timelike::second(&chrono::Local::now()) as u64);
                cx.background_executor().timer(Duration::from_millis(60_050 - into_minute.min(60_000))).await;
                let alive = this.update(cx, |this, cx| {
                    this.now_min = model::now_minutes();
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();

        // Live mode: configured sources, shown from the offline cache at once and kept
        // fresh by the background sync. `--demo` (and benchmarks) use synthetic data.
        let (config, diags) = crate::viewconf::load();
        let calendars = calendars_for(&config.settings, opts.demo || opts.bench);
        let names: Vec<String> = calendars.iter().map(|c| c.name.clone()).collect();
        let (sources, problems) = crate::sync::load_sources(config.settings.sources.as_deref(), &names);
        for p in &problems {
            eprintln!("calm: {p}");
        }
        let live = !opts.demo && !opts.bench && !sources.is_empty();
        // Reminders, while the window's open (`calm agent` does it while it's closed).
        if live {
            crate::notify::spawn();
        }
        let data = if live {
            load_live(&sources, &calendars)
        } else {
            model::generate(if opts.bench { std::env::var("CALM_MULT").ok().and_then(|m| m.parse().ok()).unwrap_or(2) } else { 1 })
        };
        let energy = config.settings.energy.clone().unwrap_or_default();
        let mut data = data;
        data.set_energy(&energy);
        let sync = live.then(|| start_sync(&sources, &problems));
        // Check for sync news once a second; cheap, and the UI only redraws on change.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            if this.update(cx, |this, cx| this.poll_sync(cx)).is_err() {
                break;
            }
        })
        .detach();
        let today = data.today;
        let config_problems: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
        for d in &config_problems {
            eprintln!("calm: {d}");
        }
        let views = config.views.clone();
        let lenses: Vec<Lens> = views.iter().map(|spec| new_lens(spec, today)).collect();
        let mut layers = vec![Spring::new(0.); views.len()];
        layers[0] = Spring::new(1.);
        let mut this = Self {
            focus,
            fonts: Fonts::new(),
            theme: theme_for(opts.dark, is_dark(window.appearance()), opts.opacity, &calendars, &looks_of(&config.settings)),
            theme_override: opts.dark,
            looks: looks_of(&config.settings),
            system_dark: is_dark(window.appearance()),
            translucent: opts.opacity,
            _appearance: appearance,
            cells: Vec::new(),
            values: views.iter().map(Spec::defaults).collect(),
            config_files: watch_list(&config.files),
            config_problems,
            conf: config.clone(),
            restore: Default::default(),
            editor: Spring::new(0.),
            editor_on: opts.edit,
            editor_note: None,
            detail_t: Spring::new(0.),
            enc_from: views[0].color_enc(&views[0].defaults(), views[0].mark),
            enc_to: views[0].color_enc(&views[0].defaults(), views[0].mark),
            views,
            enc_t: Spring::new(1.),
            marks: Vec::new(),
            flipping: Vec::new(),
            drawn_marks: Vec::new(),
            preset: 0,
            lenses,
            hidden: std::array::from_fn(|c| c >= calendars.len()),
            calendars,
            now_min: model::now_minutes(),
            dirty: true,
            stagger: false,
            viewport: (0., 0.),
            layers,
            margin_l: Spring::new(GUTTER_MIN),
            margin_b: Spring::new(CONTROLS_ROW),
            margin_t: Spring::new(scene::MARGIN_T),
            zones: crate::zones::Zones::load(opts.zones.as_deref(), config.settings.zones.as_ref()),
            sync,
            pinned: Pinned { zones: opts.zones.is_some(), theme: opts.dark.is_some(), synthetic: opts.demo || opts.bench },
            zone_hits: Default::default(),
            hover_zone: None,
            switcher: Spring::new(0.),
            drawer: Spring::new(0.),
            tooltip: Spring::new(0.),
            help: Spring::new(0.),
            help_on: opts.help,
            menu: Spring::new(0.),
            menu_on: opts.menu,
params_open: opts.controls,
            drawer_edit: None,
            toast: None,
            quiet: Default::default(),
            changes_on: opts.changes,
            changes_unticked: Default::default(),
            focus_title: false,
            picker: None,
            mode: None,
            snap: 15,
            free_only: false,
            cursor: None,
            hover_grip: None,
            prompt: None,
            pending_prompt: None,
            prompt_query_dirty: false,
            found: None,
            jump_mark: None,
            prompt_seed: None,
            pending_picker: opts.picker,
            hud_on: opts.bench,
            mouse: (-1., -1.),
            hits: Vec::new(),
            hints: None,
            pending_pick: opts.pick.clone(),
            pack: None,
            side: None,
            side_t: Spring::new(0.),
            side_start: Spring::new(0.),
            side_targets: Vec::new(),
            side_pack: None,
            dnd: crate::notify::dnd(chrono::Utc::now()),
            dnd_read: Instant::now(),
            hovered: None,
            hover_day: None,
            selected: None,
            shown: None,
            stress: opts.bench,
            last_frame: None,
            cpu_last: Rc::new(Cell::new(0.)),
            stats: Stats::default(),
            animating: true,
            drawn: (0, 0, 0),
            bench: (opts.bench || opts.cycle).then(|| Bench {
                report: opts.bench,
                start: Instant::now(),
                last_step: Instant::now(),
                step: 0,
                stats: Stats::default(),
            }),
            still: opts.still,
            pending: (opts.hover, opts.select),
            scripted_pointer: opts.hover.is_some(),
            stable_frames: 0,
            last_trace: String::new(),
            data,
            energy,
            place: None,
            travel: None,
            over: Default::default(),
        };
        this.refresh_travel();
        // `--travel`: the Travel overlay on, as `W` would turn it on.
        if let Some(o) = opts.travel.then(|| this.conf.overlays.iter().find(|o| o.name == "Travel").cloned()).flatten() {
            let key = (String::new(), crate::keys::Command::Toggle(o.set.clone()).to_string());
            this.restore.insert(key, o.set.iter().map(|(k, _)| (k.clone(), None)).collect());
            this.over.extend(o.set);
        }
        this.restat();
        this.resolve_place(&config.settings);
        if let Some(q) = opts.search.clone() {
            this.open_prompt(search::Kind::Search);
            this.prompt_seed = Some(q);
        } else if let Some(q) = opts.jump.clone() {
            this.open_prompt(search::Kind::Jump);
            this.prompt_seed = Some(q);
        }
        match opts.mode.as_deref() {
            Some("new") => this.toggle_mode(true),
            Some("adjust") => this.toggle_mode(false),
            _ => {}
        }
        if let Some(lens) = opts.lens {
            this.set_preset(lens.min(this.views.len() - 1));
        }
        if opts.offset != 0 {
            let l = &mut this.lenses[this.preset];
            let s = (l.start.target + opts.offset as f32).max(0.);
            l.start = Spring::new(s);
        }
        if let Some(stop) = &opts.zoom {
            let spec = this.spec();
            if let Some(i) = spec.stops.iter().position(|s| s.name.eq_ignore_ascii_case(stop)) {
                this.zoom_to_stop(i, None);
                let l = &mut this.lenses[this.preset];
                l.zoom = Spring::new(l.zoom.target);
                l.start = Spring::new(l.start.target);
            }
        }
        if opts.span {
            this.set_param("span", "true");
            this.detail_t = Spring::new(1.);
        }
        if let Some(spec) = &opts.color {
            // `measure[:scale|:calendar|:compose]`; a compose implies by calendar.
            let mut parts = spec.split(':');
            this.set_param("measure", parts.next().unwrap_or(""));
            match parts.next() {
                Some("scale") => this.set_param("split", "false"),
                Some("calendar") => this.set_param("split", "true"),
                Some(c) => {
                    this.set_param("compose", c);
                    this.set_param("split", "true");
                }
                None => {}
            }
            let enc = this.color();
            this.enc_from = enc;
            this.enc_to = enc;
        }
        this
    }

    fn spec(&self) -> &Spec {
        &self.views[self.preset]
    }

    fn lens(&self) -> &Lens {
        &self.lenses[self.preset]
    }

    /// The arrangement for lens `p`, live or (with `target`) where it is heading.
    fn arr_of(&self, p: usize, target: bool) -> Arr {
        let l = &self.lenses[p];
        let spec = &self.views[p];
        let (x, y, w, h) = self.plot();
        let (z, s) = if target { (l.zoom.target, l.start.target) } else { (l.zoom.x, l.start.x) };
        let pack = spec.facet.pack.then(|| self.pack.clone()).flatten();
        Arr { plot: [x, y, w, h], facet: spec.facet, n: z.exp2(), start: s, days: DAYS, boxed: spec.x == Axis::None, pack }
    }

    /// The plot rectangle (x, y, w, h) inside the window.
    fn plot(&self) -> (f32, f32, f32, f32) {
        let (vw, vh) = self.viewport;
        let (l, b) = (self.margin_l.x, self.margin_b.x);
        let t = self.margin_t.x;
        let w = (vw - l - scene::MARGIN_R).max(200.);
        // Room for the view beside it, as it slides in.
        let side = self.side_width() * self.side_t.x.clamp(0., 1.);
        (l, t, (w - side).max(200.), (vh - t - b).max(200.))
    }

    /// The side view's width with the gap before it, when fully in.
    fn side_width(&self) -> f32 {
        let (vw, _) = self.viewport;
        let w = (vw - self.margin_l.x - scene::MARGIN_R).max(200.);
        self.side.as_ref().map_or(0., |_| (w * self.spec_split_size()).max(240.) + 24.)
    }

    fn spec_split_size(&self) -> f32 {
        self.views.iter().filter_map(|v| v.split.as_ref()).find(|s| self.side.as_ref().is_some_and(|v| v.name == s.name)).map_or(0.4, |s| s.size)
    }

    /// The side view's arrangement: right of the plot, sliding in from the edge.
    pub(super) fn side_arr(&self) -> Option<Arr> {
        let spec = self.side.as_ref()?;
        let (x, y, w, h) = self.plot();
        let full = self.side_width();
        // Just right of the plot, which narrows as it comes in: so it slides in from the edge.
        let left = x + w + 24.;
        // Together: from the first day the view shows. Its own: from where it's scrolled.
        let start = if self.side_together() { self.lens().start.x } else { self.side_start.x };
        Some(Arr { plot: [left, y, (full - 24.).max(1.), h], facet: spec.facet, n: 14., start, days: DAYS, boxed: true, pack: self.side_pack.clone() })
    }

    /// Whether the side view scrolls with this one (`split … scroll="together"`).
    fn side_together(&self) -> bool {
        self.spec().split.as_ref().is_none_or(|s| s.together)
    }

    /// Lays out the side view's list (when there is one), as `relayout` does the main one.
    fn relayout_side(&mut self) {
        let Some(spec) = self.side.clone() else { return };
        let mut targets = layout::solve(&self.data.events, &spec, spec.mark, &self.hidden);
        self.dim_targets(&spec, &mut targets);
        self.side_pack = Some(self.pack_offsets(&targets));
        self.side_targets = targets;
    }

    /// A search dims what doesn't match; a list, what's over today.
    fn dim_targets(&self, spec: &Spec, targets: &mut [layout::Target]) {
        let now = chrono::Local::now();
        let now_min = (chrono::Timelike::hour(&now) * 60 + chrono::Timelike::minute(&now)) as f32;
        for (i, t) in targets.iter_mut().enumerate() {
            if self.search_dims(i) {
                t.alpha *= search::DIM;
            }
            let e = &self.data.events[i];
            if spec.facet.pack && e.day == self.data.today && e.all_day.is_none() && e.start_min + e.dur_min <= now_min {
                t.alpha *= 0.5;
            }
        }
    }

    /// A packed list's day offsets: each day as tall as its rows, empty days none (today
    /// always shows).
    fn pack_offsets(&self, targets: &[layout::Target]) -> std::sync::Arc<[f32]> {
        let mut rows = vec![0usize; DAYS as usize];
        for (t, e) in targets.iter().zip(&self.data.events) {
            if let layout::Kind::Row { rank } = t.kind
                && t.alpha > 0.01
                && let Some(r) = rows.get_mut(e.day as usize)
            {
                *r = (*r).max(rank as usize + 1);
            }
        }
        let today = self.data.today as usize;
        let mut offs = Vec::with_capacity(rows.len() + 1);
        let mut y = 0.;
        for (d, &n) in rows.iter().enumerate() {
            offs.push(y);
            if n > 0 || d == today {
                y += arrange::PACK_HEAD + n.max(usize::from(d == today)) as f32 * arrange::PACK_ROW + arrange::PACK_GAP;
            }
        }
        offs.push(y);
        offs.into()
    }

    /// How view `p` labels its time axis: its `zones` channel, or the session's.
    pub(super) fn zones_of(&self, p: usize) -> crate::spec::ZoneAxes {
        self.views[p].zones(&self.over, &self.values[p], self.lenses[p].mark)
    }

    /// Extra zones labelling the current lens's time axis.
    fn shown_zones(&self) -> Vec<chrono_tz::Tz> {
        self.shown_zones_of(self.preset)
    }

    /// Extra zones labelling lens `p`'s time axis: none, the configured ones, or those and
    /// the zones your journeys in view go to and from.
    pub(super) fn shown_zones_of(&self, p: usize) -> Vec<chrono_tz::Tz> {
        use crate::spec::ZoneAxes;
        let how = self.zones_of(p);
        if how == ZoneAxes::Primary {
            return Vec::new();
        }
        let mut zones = self.zones.extra.clone();
        if let (ZoneAxes::Visible, Some(t)) = (how, &self.travel) {
            let l = &self.lenses[p];
            let (s, n) = (l.start.target.floor() as i32, l.zoom.target.exp2().ceil() as i32);
            let at = |d: i32| self.data.date(d).and_hms_opt(0, 0, 0).unwrap().and_utc();
            let date = self.data.date(self.data.today);
            let shown = |z: chrono_tz::Tz, zones: &[chrono_tz::Tz]| {
                let off = crate::zones::offset(z, date);
                off == self.zones.primary_offset(date) || zones.iter().any(|o| crate::zones::offset(*o, date) == off)
            };
            for tz in t.zones_between(at(s - 1), at(s + n + 1)) {
                if !shown(tz, &zones) {
                    zones.push(tz);
                }
            }
        }
        zones
    }

    /// Left gutter wide enough for the labels the current lens draws there.
    fn gutter_target(&self) -> f32 {
        let spec = self.spec();
        let lines = self.arr_of(self.preset, true).lines();
        let single = lines < 1.12;
        match (spec.coord, spec.facet.per_line, spec.facet.dir) {
            (Coord::Polar, _, _) => GUTTER_MIN,
            _ if spec.facet.pack => GUTTER_MIN,
            // Weekday names beside the columns.
            (_, _, Dir::Columns) => 48.,
            // Calendar names, or the demand scale and "Your energy".
            (_, 1, _) if single => if spec.y == Axis::Calendar { 100. } else { 96. },
            (_, 1, _) => GUTTER_MIN,
            // Hour labels for a single week; month names once weeks stack; nothing for bands.
            // Plus a column per extra time zone.
            _ if single => 60. + scene::ZONE_COL * self.shown_zones().len() as f32,
            _ if self.arr_of(self.preset, true).bands() < 1.3 => 64.,
            _ => GUTTER_MIN,
        }
    }

    fn arr(&self) -> Arr {
        self.arr_of(self.preset, false)
    }

    /// Inner coordinates of a cell in lens `p`.
    fn inner(&self, p: usize, mut cell: [f32; 4]) -> Inner {
        let spec = &self.views[p];
        // Room above the time axis for all-day bars.
        let band = self.band_rows_at(p, cell);
        if band > 0 {
            cell[1] = (cell[1] + band as f32 * scene::BAND_ROW + 3.).min(cell[3] - 8.);
        }
        // Room for the energy rail along the time axis.
        if self.rail_of(p).is_some() {
            if spec.y == Axis::TimeOfDay {
                cell[0] = (cell[0] + scene::RAIL_W).min(cell[2] - 4.);
            } else {
                cell[1] = (cell[1] + scene::RAIL_W).min(cell[3] - 4.);
            }
        }
        if spec.coord == Coord::Cartesian && spec.x != Axis::None {
            // Boxed cells keep a header strip for their date label.
            let b = self.arr_of(p, false).boxness();
            cell[1] = (cell[1] + 17. * b).min(cell[3] - 4.);
        }
        let l = &self.lenses[p];
        // A dial is a clock face: always the whole day, whatever the hour window.
        let hours = if spec.coord == Coord::Polar { (0., 1.) } else { (l.hours[0].x, l.hours[1].x) };
        let (xr, yr) = match (spec.x, spec.y) {
            (Axis::TimeOfDay, _) => (hours, (0., 1.)),
            (_, Axis::TimeOfDay) => ((0., 1.), hours),
            _ => ((0., 1.), (0., 1.)),
        };
        Inner { cell, coord: spec.coord, xr, yr, outward: spec.coord == Coord::Polar && spec.y == Axis::Demand }
    }

    /// Whether the current lens shows aggregates (so colour means a reduction over a day).
    fn aggregate(&self) -> bool {
        self.lens().mark == Mark::Heat
    }

    /// Day-cell statistics, over the events a search leaves (all of them without one).
    fn restat(&mut self) {
        self.cells = match self.found.as_ref().filter(|f| !f.hits.is_empty()) {
            Some(f) => {
                let kept: Vec<model::Event> = self.data.events.iter().zip(&f.hits).filter(|(_, h)| **h).map(|(e, _)| e.clone()).collect();
                color::cell_stats(&kept, &self.hidden, DAYS as usize)
            }
            None => color::cell_stats(&self.data.events, &self.hidden, DAYS as usize),
        };
    }

    fn set_data(&mut self, mut data: Dataset) {
        data.set_energy(&self.energy);
        // Keep the selected event selected across reloads (an edit reloads the data).
        let keep = self.selected.and_then(|i| self.data.events.get(i)).map(|e| (e.edit.as_ref().map(|r| (r.key.clone(), r.id.clone())), e.title.clone(), e.day));
        let reselect = keep.and_then(|(id, title, day)| {
            data.events.iter().position(|e| match (&id, &e.edit) {
                (Some((k, i)), Some(r)) => &r.key == k && &r.id == i,
                _ => e.title == title && e.day == day,
            })
        });
        self.data = data;
        self.marks.clear();
        self.flipping.clear();
        self.drawn_marks.clear();
        self.selected = reselect;
        self.hovered = None;
        self.dirty = true;
        self.restat();
        self.research();
        self.refresh_travel();
    }

    /// Reads time-zone moves from the cache (real data only). Nothing shows unless an event
    /// moves between zones or a view's rail is `zone`.
    fn refresh_travel(&mut self) {
        self.travel = self.sync.is_some().then(crate::travel::load).flatten();
    }

    fn set_preset(&mut self, preset: usize) {
        if preset == self.preset {
            return;
        }
        self.preset = preset;
        // A view with another beside it: that one slides in, its list from today.
        if let Some(side) = self.views[preset].split.as_ref().and_then(|s| s.view.clone()) {
            if self.side.as_ref().is_none_or(|s| s.name != side.name) || self.side_t.x < 0.01 {
                self.side_start = Spring::new(self.data.today as f32);
            }
            self.side = Some(side);
        }
        for (i, l) in self.layers.iter_mut().enumerate() {
            l.target = if i == preset { 1. } else { 0. };
        }
        self.switcher.target = preset as f32;
        self.dirty = true;
        self.stagger = true;
    }

    fn color(&self) -> ColorEnc {
        self.spec().color_enc(&self.values[self.preset], self.lens().mark)
    }

    fn detail(&self) -> Detail {
        self.spec().detail(&self.values[self.preset], self.lens().mark)
    }

    fn set_param(&mut self, name: &str, value: &str) {
        if self.spec().param(name).is_some() {
            self.values[self.preset].insert(name.to_string(), value.to_string());
        }
    }

    /// Start a crossfade whenever the effective encoding changes, whether
    /// from the colour bar or from switching lens.
    fn sync_color(&mut self) {
        let enc = self.color();
        if enc != self.enc_to {
            self.enc_from = self.enc_to;
            self.enc_to = enc;
            self.enc_t = Spring { target: 1., ..Spring::new(0.) };
        }
    }

    fn toggle_calendar(&mut self, c: usize) {
        self.hidden[c] = !self.hidden[c];
        if self.hidden.iter().all(|&h| h) {
            self.hidden[c] = false;
        }
        self.restat();
        self.dirty = true;
    }

    // --- Zoom, scroll and snapping ------------------------------------------

    /// Index of the stop nearest the lens's target zoom.
    fn nearest_stop(&self) -> usize {
        let z = self.lens().zoom.target;
        let logs = stops_log(self.spec());
        (0..logs.len()).min_by(|&a, &b| (logs[a] - z).abs().total_cmp(&(logs[b] - z).abs())).unwrap_or(0)
    }

    /// (cells, start) for stop `i`, aligned per the stop's rule around `center`.
    fn aligned(&self, i: usize, center: f32) -> (f32, f32) {
        let spec = self.spec();
        let stop = spec.stops[i].clone();
        let p = spec.facet.per_line as f32;
        let n = stop.cells as f32;
        let center = center.clamp(0., DAYS as f32 - 1.);
        let (n, start) = match stop.align {
            Align::Cell => (n, (center - n * 0.5 + 0.5).floor()),
            Align::Line => (n, ((center - n * 0.5 + p * 0.5) / p).floor() * p),
            Align::Group => (n, (center / n).floor() * n),
            Align::Month => {
                let date = self.data.date(center as i32);
                let first = date.with_day(1).unwrap();
                let next = if first.month() == 12 {
                    first.with_year(first.year() + 1).unwrap().with_month(1).unwrap()
                } else {
                    first.with_month(first.month() + 1).unwrap()
                };
                let d0 = (first - self.data.origin).num_days() as f32;
                let d1 = (next - self.data.origin).num_days() as f32 - 1.;
                let (l0, l1) = ((d0 / p).floor(), (d1 / p).floor());
                ((l1 - l0 + 1.) * p, l0 * p)
            }
        };
        (n, start.clamp(0., (DAYS as f32 - n).max(0.)))
    }

    fn center_target(&self) -> f32 {
        let l = self.lens();
        l.start.target + l.zoom.target.exp2() * 0.5
    }

    fn zoom_to_stop(&mut self, i: usize, center: Option<f32>) {
        let center = center.unwrap_or_else(|| self.center_target());
        let (n, mut start) = self.aligned(i, center);
        // A packed list starts at the day it's going to (today, a jump), not around it.
        if self.spec().facet.pack {
            start = center.floor();
        }
        let l = &mut self.lenses[self.preset];
        l.zoom.target = n.log2();
        l.start.target = start;
        l.last_input = None;
    }

    /// Continuous zoom by `delta` (log2 cells), keeping the cell under `anchor` in place.
    fn zoom_by(&mut self, delta: f32, anchor: Option<Pt>) {
        let spec = self.spec();
        let logs = stops_log(spec);
        let (lo, hi) = (logs[0], *logs.last().unwrap());
        let arr = self.arr_of(self.preset, true);
        let old_n = arr.n;
        let a = anchor.and_then(|pt| arr.day_at(pt)).map(|d| d as f32 + 0.5).unwrap_or(arr.start + old_n * 0.5);
        let f = ((a - arr.start) / old_n).clamp(0., 1.);
        let l = &mut self.lenses[self.preset];
        l.zoom.target = (l.zoom.target + delta).clamp(lo, hi);
        let n = l.zoom.target.exp2();
        l.start.target = (a - f * n).clamp(0., (DAYS as f32 - n).max(0.));
        l.last_input = Some(Instant::now());
    }

    /// Step to the next stop out (`dir > 0`) or in.
    fn zoom_step(&mut self, dir: i32) {
        let i = self.nearest_stop() as i32;
        let next = (i + dir).clamp(0, self.spec().stops.len() as i32 - 1) as usize;
        self.zoom_to_stop(next, None);
    }

    /// Previous/next by the current stop's unit.
    fn pan(&mut self, dir: i32) {
        let i = self.nearest_stop();
        let stop = self.spec().stops[i].clone();
        let shift = match stop.align {
            Align::Month => 30.5,
            _ => stop.cells as f32,
        };
        let center = self.center_target() + dir as f32 * shift;
        self.zoom_to_stop(i, Some(center));
    }

    fn go_today(&mut self) {
        self.side_start.target = self.data.today as f32;
        let i = self.nearest_stop();
        self.zoom_to_stop(i, Some(self.data.today as f32 + 0.5));
    }

    /// Free scroll along the flow; snaps to alignment once input pauses.
    fn scroll_cells(&mut self, px: f32) {
        let arr = self.arr_of(self.preset, true);
        let l = &mut self.lenses[self.preset];
        let n = l.zoom.target.exp2();
        if l.last_input.is_none() {
            l.gesture_from = Some(l.start.target);
        }
        let next = arr.scrolled(px).unwrap_or(l.start.target + px * arr.cells_per_px());
        l.start.target = next.clamp(0., (DAYS as f32 - n).max(0.));
        l.last_input = Some(Instant::now());
    }

    /// Scroll (shift) or zoom (scale around `anchor` in 0..1) the inner hour window.
    fn scroll_hours(&mut self, shift: f32, scale: f32, anchor: f32) {
        let l = &mut self.lenses[self.preset];
        let (h0, h1) = (l.hours[0].target, l.hours[1].target);
        let span = ((h1 - h0) * scale).clamp(3. / 24., 1.);
        let pivot = h0 + (h1 - h0) * anchor;
        let lo = (pivot - span * anchor + shift).clamp(0., 1. - span);
        l.hours[0].target = lo;
        l.hours[1].target = lo + span;
    }

    /// Between the lens's default hours and the whole day.
    fn toggle_full_day(&mut self) {
        let l = &mut self.lenses[self.preset];
        let full = l.hours[1].target - l.hours[0].target > 0.999;
        let (lo, hi) = if full { l.default_hours } else { (0., 1.) };
        l.hours[0].target = lo;
        l.hours[1].target = hi;
    }

    /// Whether `pos` is on the hour labels: the left gutter for a vertical time axis,
    /// the band above the plot for a horizontal one.
    fn on_hour_axis(&self, (x, y): Pt) -> bool {
        let spec = self.spec();
        if spec.coord != Coord::Cartesian || self.lens().mark != spec.mark || self.arr().lines() > 1.05 {
            return false;
        }
        let (px, py, w, h) = self.plot();
        match (spec.x, spec.y) {
            (_, Axis::TimeOfDay) => x < px && y >= py && y <= py + h,
            (Axis::TimeOfDay, _) => y < py && y > py - 40. - scene::ZONE_ROW * self.shown_zones().len() as f32 && x >= px && x <= px + w,
            _ => false,
        }
    }

    /// Whether wheel scrolling should move the hour window rather than the days.
    fn scrolls_hours(&self) -> bool {
        let spec = self.spec();
        let l = self.lens();
        spec.coord == Coord::Cartesian
            && (spec.x == Axis::TimeOfDay || spec.y == Axis::TimeOfDay)
            && l.mark == spec.mark
            && self.arr().lines() < 1.05
            && (l.hours[1].target - l.hours[0].target) < 0.999
    }

    fn snap(&mut self) {
        let Some(at) = self.lens().last_input else { return };
        if at.elapsed() < SNAP_AFTER {
            return;
        }
        let i = self.nearest_stop();
        // A deliberate swipe (over a tenth of a page) carries on to the next page in its
        // direction instead of springing back to where it started.
        let l = &mut self.lenses[self.preset];
        let n = l.zoom.target.exp2();
        let moved = l.gesture_from.take().map_or(0., |from| l.start.target - from);
        let bias = if moved.abs() > 0.1 * n { moved.signum() * 0.45 * n } else { 0. };
        let center = self.center_target() + bias;
        self.zoom_to_stop(i, Some(center));
    }

    /// Recompute the level of detail from the live cell size.
    fn update_lod(&mut self) {
        let spec = self.spec();
        if spec.lod.is_empty() {
            return;
        }
        let arr = self.arr();
        let c = arr.cell((arr.start + arr.n * 0.5) as i32);
        let (w, h) = (c[2] - c[0], c[3] - c[1]);
        let extent = match spec.lod_extent {
            Extent::Time if spec.x == Axis::TimeOfDay && spec.coord == crate::spec::Coord::Cartesian => w,
            Extent::Time | Extent::Height => h,
            Extent::Width => w,
            Extent::Min => w.min(h),
        };
        let current = self.lens().mark;
        let mark = spec.lod_mark(extent, current);
        if mark != current {
            self.lenses[self.preset].mark = mark;
            self.dirty = true;
        }
    }

    fn relayout(&mut self) {
        let spec = self.spec().clone();
        let mark = self.lens().mark;
        let arr = self.arr();
        let mut targets = layout::solve(&self.data.events, &spec, mark, &self.hidden);
        self.relayout_side();
        self.dim_targets(&spec, &mut targets);
        // A packed list: each day as tall as its rows.
        self.pack = spec.facet.pack.then(|| self.pack_offsets(&targets));
        for (i, t) in targets.into_iter().enumerate() {
            // Stagger by screen position so a lens change reads as a wave.
            let delay = if self.stagger {
                let c = arr.cell(t.day);
                0.12 * ((c[0] - arr.plot[0]) / arr.plot[2]).clamp(0., 1.)
            } else {
                0.
            };
            match self.marks.get_mut(i) {
                Some(m) => {
                    if m.target == t && !self.stagger {
                        continue;
                    }
                    let moved = m.target.kind != t.kind || m.target.g != t.g || m.target.day != t.day || self.stagger;
                    if moved
                        && let Some(shape) = m.shown.take() {
                            m.from = Some(shape);
                            m.t = Spring { target: 1., ..Spring::new(0.) };
                            m.delay = delay;
                            self.flipping.push(i);
                        }
                    if t.label == LabelMode::Hidden {
                        // Aggregating away: labels would only pile up as marks converge.
                        m.label = Spring::new(0.);
                    } else if m.label_mode != t.label {
                        m.label.target = 0.;
                    }
                    m.alpha.target = t.alpha;
                    if m.shown.is_none() && m.from.is_none() {
                        // Off screen: no one sees the transition, so skip it.
                        m.alpha = Spring::new(t.alpha);
                        m.label_mode = t.label;
                        m.label = Spring::new(1.);
                    }
                    m.target = t;
                }
                None => self.marks.push(MarkAnim {
                    target: t,
                    alpha: Spring { target: t.alpha, ..Spring::new(0.) },
                    from: None,
                    t: Spring::new(1.),
                    delay: 0.,
                    shown: None,
                    label: Spring::new(1.),
                    label_mode: t.label,
                    placed: Spring::new(0.),
                    hover: Spring::new(0.),
                }),
            }
        }
        self.flipping.sort_unstable();
        self.flipping.dedup();
        self.dirty = false;
        self.stagger = false;
    }

    fn step(&mut self, dt: f32) -> bool {
        let mut moving = false;
        // Morphs in flight.
        let mut still_flipping = Vec::with_capacity(self.flipping.len());
        for &i in &self.flipping {
            let m = &mut self.marks[i];
            if m.delay > 0. {
                m.delay -= dt;
                still_flipping.push(i);
                moving = true;
                continue;
            }
            if m.t.step(dt, GENTLE) {
                still_flipping.push(i);
                moving = true;
            } else {
                m.from = None;
            }
        }
        self.flipping = still_flipping;
        // Only marks on screen or mid-morph animate; everything else snaps when it
        // changes (see `relayout`), so cost tracks what is visible, not the dataset.
        let mut live = self.drawn_marks.clone();
        live.extend_from_slice(&self.flipping);
        // Hovering lights every piece of the event, e.g. both sides of midnight.
        let group = self.hovered.map(|i| self.data.events[i].group);
        let lit: Vec<usize> = match group {
            Some(g) => self.group_of(g),
            None => Vec::new(),
        };
        live.extend_from_slice(&lit);
        live.extend(self.hovered);
        live.sort_unstable();
        live.dedup();
        for i in live {
            let m = &mut self.marks[i];
            moving |= m.alpha.step(dt, GENTLE);
            if m.label_mode != m.target.label && m.label.x < 0.05 {
                m.label_mode = m.target.label;
            }
            if m.label_mode == m.target.label {
                m.label.target = 1.;
            }
            moving |= m.label.step(dt, SNAPPY);
            moving |= m.placed.step(dt, SNAPPY);
            m.hover.target = if lit.contains(&i) { 1. } else { 0. };
            moving |= m.hover.step(dt, SNAPPY);
        }
        for (p, l) in self.lenses.iter_mut().enumerate() {
            let base = self.views[p].mark;
            l.heat.target = if l.mark == Mark::Heat { 1. } else { 0. };
            l.grid.target = if l.mark == base { 1. } else { 0. };
            let [h0, h1] = &mut l.hours;
            for s in [&mut l.zoom, &mut l.start, h0, h1] {
                moving |= s.step(dt, VIEWPORT);
            }
            moving |= l.heat.step(dt, GENTLE);
            moving |= l.grid.step(dt, GENTLE);
            // Keep frames coming until a pending snap has fired.
            moving |= p == self.preset && l.last_input.is_some();
        }
        for l in &mut self.layers {
            moving |= l.step(dt, GENTLE);
        }
        self.drawer.target = if self.drawer_open() { 1. } else { 0. };
        let hovering = self.hovered.is_some() || self.hover_day.is_some() || self.hover_zone.is_some();
        self.tooltip.target = if hovering && self.selected.is_none() && self.mode.is_none() { 1. } else { 0. };
        self.help.target = if self.help_on { 1. } else { 0. };
        self.editor.target = if self.editor_on { 1. } else { 0. };
        self.menu.target = if self.menu_on { 1. } else { 0. };
        self.detail_t.target = if self.detail() == Detail::DaySpan { 1. } else { 0. };
        self.margin_l.target = self.gutter_target();
        self.side_t.target = if self.spec().split.as_ref().is_some_and(|s| s.view.is_some()) { 1. } else { 0. };
        moving |= self.side_t.step(dt, GENTLE);
        moving |= self.side_start.step(dt, VIEWPORT);
        // A row of hour labels per extra zone above a horizontal time axis.
        let rows = if self.spec().x == Axis::TimeOfDay { self.shown_zones().len() } else { 0 };
        // Room for the mode bar under the top bar while you're in a mode.
        let mode_bar = if self.mode.is_some() { 40. } else { 0. };
        self.margin_t.target = scene::MARGIN_T + scene::ZONE_ROW * rows as f32 + mode_bar;
        self.margin_b.target = CONTROLS_ROW;
        for (s, cfg) in [
            (&mut self.switcher, SNAPPY),
            (&mut self.drawer, GENTLE),
            (&mut self.tooltip, SNAPPY),
            (&mut self.help, SNAPPY),
            (&mut self.editor, SNAPPY),
            (&mut self.menu, SNAPPY),
            (&mut self.enc_t, GENTLE),
            (&mut self.detail_t, GENTLE),
            (&mut self.margin_l, GENTLE),
            (&mut self.margin_b, GENTLE),
            (&mut self.margin_t, GENTLE),
        ] {
            moving |= s.step(dt, cfg);
        }
        if self.selected.is_some() {
            self.shown = self.selected;
        }
        moving
    }

    fn hit(&self, pos: Pt) -> Option<usize> {
        self.hits.iter().rev().find(|(_, h)| h.contains(pos)).map(|(i, _)| *i)
    }

    fn plot_contains(&self, (x, y): Pt) -> bool {
        let (px, py, w, h) = self.plot();
        x >= px && x <= px + w && y >= py && y <= py + h
    }

    /// The day under the pointer when cells are overviews rather than detail.
    fn overview_day(&self, pos: Pt) -> Option<i32> {
        let arr = self.arr();
        let overview = self.lens().mark != self.spec().mark || arr.lines() > 1.3 || self.spec().x == Axis::None;
        (overview && self.plot_contains(pos)).then(|| arr.day_at(pos)).flatten()
    }

    /// The drawer shows the selected event, except while you adjust events (it would be in the way).
    fn drawer_open(&self) -> bool {
        self.selected.is_some() && !self.adjusting()
    }

    fn over_chrome(&self, (x, y): Pt) -> bool {
        let drawer = self.drawer_open() && x > self.viewport.0 - chrome::DRAWER_W - 24.;
        drawer || y < chrome::TOPBAR_H || y > self.viewport.1 - self.margin_b.x + 6.
    }

    fn on_move(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        // `--hover` holds the pointer still for screenshots; the real one would reset it.
        if self.scripted_pointer {
            return;
        }
        let pos: Pt = (ev.position.x.into(), ev.position.y.into());
        self.mouse = pos;
        if self.mode.is_some() && self.mode_move(pos) {
            cx.notify();
        }
        let zone = self.zone_hits.borrow().iter().find(|(r, _)| pos.0 >= r[0] && pos.0 <= r[2] && pos.1 >= r[1] && pos.1 <= r[3]).map(|(_, z)| *z);
        if zone != self.hover_zone {
            self.hover_zone = zone;
            cx.notify();
        }
        let (hovered, day) = if self.over_chrome(pos) {
            (None, None)
        } else {
            let hit = self.hit(pos);
            (hit, if hit.is_none() && self.aggregate() { self.overview_day(pos) } else { None })
        };
        if hovered != self.hovered || day != self.hover_day || self.tooltip.x > 0.01 {
            self.hovered = hovered;
            self.hover_day = day;
            cx.notify();
        }
    }

    fn on_down(&mut self, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        let pos: Pt = (ev.position.x.into(), ev.position.y.into());
        // A click gives up picking by letters.
        if self.hints.take().is_some() {
            cx.notify();
            return;
        }
        if self.over_chrome(pos) {
            return;
        }
        // A click outside the open menu just closes it.
        if self.menu_on || self.params_open {
            self.menu_on = false;
            self.params_open = false;
            cx.notify();
            return;
        }
        if self.mode_down(pos, cx) {
            cx.notify();
            return;
        }
        if self.on_hour_axis(pos) {
            self.toggle_full_day();
            cx.notify();
            return;
        }
        if let Some(i) = self.hit(pos) {
            self.selected = Some(i);
        } else if let Some(day) = self.overview_day(pos) {
            // Drill down: an overview cell opens at the next level of detail.
            if self.spec().x == Axis::None {
                self.set_preset(0);
            }
            let spec = self.spec();
            let i = spec.stops.iter().position(|s| s.cells >= spec.facet.per_line).unwrap_or(0);
            self.zoom_to_stop(i, Some(day as f32 + 0.5));
        } else {
            self.selected = None;
        }
        cx.notify();
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let (dx, dy): (f32, f32) = match ev.delta {
            ScrollDelta::Pixels(p) => (p.x.into(), p.y.into()),
            ScrollDelta::Lines(l) => (l.x * 48., l.y * 48.),
        };
        // Touchpad swipes are never perfectly straight: follow the dominant axis only,
        // so a vertical swipe doesn't also nudge the days (and then snap back).
        let (dx, dy) = if dy.abs() >= dx.abs() { (0., dy) } else { (dx, 0.) };
        // Over the view beside: scroll its list, and with it (if they go together) this view,
        // which settles on a day once you stop.
        if let Some(arr) = self.side_arr().filter(|a| self.side_t.x > 0.5 && self.mouse.0 >= a.plot[0])
            && let Some(start) = arr.scrolled(-dy)
        {
            if self.side_together() {
                let n = self.lens().zoom.target.exp2();
                // No swipe bias (see `snap`): it settles on the day nearest the top.
                let l = &mut self.lenses[self.preset];
                l.start.target = start.clamp(0., (DAYS as f32 - n).max(0.));
                l.start.x = l.start.target;
                l.last_input = Some(Instant::now());
            } else {
                self.side_start.target = start.max(0.);
                self.side_start.x = self.side_start.target;
            }
            cx.notify();
            return;
        }
        if ev.modifiers.control || ev.modifiers.platform {
            // Semantic zoom around the pointer.
            self.zoom_by(-dy * 0.006, Some(self.mouse));
        } else if ev.modifiers.alt {
            let (x, y, w, h) = self.plot();
            let anchor = if self.spec().y == Axis::TimeOfDay { (self.mouse.1 - y) / h } else { (self.mouse.0 - x) / w };
            self.scroll_hours(0., (1. - dy * 0.003).clamp(0.5, 2.), anchor.clamp(0., 1.));
        } else if ev.modifiers.shift {
            self.scroll_cells(-(dx + dy));
        } else if self.scrolls_hours() && dy != 0. {
            let (_, _, w, h) = self.plot();
            let l = self.lens();
            let span = l.hours[1].target - l.hours[0].target;
            let px = if self.spec().y == Axis::TimeOfDay { h } else { w };
            self.scroll_hours(-dy / px * span, 1., 0.5);
            if dx != 0. {
                self.scroll_cells(-dx);
            }
        } else {
            self.scroll_cells(-(dy + dx));
        }
        cx.notify();
    }

    /// Benchmark script: lens changes and zoom sweeps, one step every 1.1 s.
    fn tick_bench(&mut self, interval: f32, cx: &mut Context<Self>) {
        let cpu = self.cpu_last.get();
        let Some(b) = &mut self.bench else { return };
        if b.start.elapsed() > Duration::from_millis(500) {
            b.stats.push(interval, cpu);
        }
        let advance = b.last_step.elapsed() > Duration::from_millis(1100);
        let step = b.step;
        if advance {
            b.last_step = Instant::now();
            b.step += 1;
        }
        let done = b.report && b.start.elapsed() > Duration::from_secs(22);
        if advance {
            // (lens, stop) pairs exercising morphs, level-of-detail changes and big zooms.
            const SCRIPT: [(usize, &str); 12] = [
                (0, "Month"),
                (0, "Quarter"),
                (0, "Year"),
                (0, "Week"),
                (1, "Week"),
                (1, "Day"),
                (2, "Day"),
                (3, "Day"),
                (3, "Week"),
                (4, "Quarter"),
                (4, "Year"),
                (0, "Week"),
            ];
            let (lens, stop) = SCRIPT[step % SCRIPT.len()];
            self.set_preset(lens);
            if let Some(i) = self.spec().stops.iter().position(|s| s.name == stop) {
                self.zoom_to_stop(i, None);
            }
        }
        let Some(b) = &self.bench else { return };
        if done {
            let s = &b.stats;
            println!(
                "bench: events={} frames={} interval ms p50={:.2} p95={:.2} p99={:.2} | cpu ms p50={:.2} p95={:.2} p99={:.2} max={:.2}",
                self.data.events.len(),
                s.intervals.len(),
                Stats::pct(&s.intervals, 0.5),
                Stats::pct(&s.intervals, 0.95),
                Stats::pct(&s.intervals, 0.99),
                Stats::pct(&s.cpu, 0.5),
                Stats::pct(&s.cpu, 0.95),
                Stats::pct(&s.cpu, 0.99),
                Stats::pct(&s.cpu, 1.0),
            );
            cx.quit();
        }
    }
}

fn is_dark(appearance: WindowAppearance) -> bool {
    matches!(appearance, WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

fn looks_of(settings: &crate::viewconf::Settings) -> crate::theme::Looks {
    crate::theme::Looks {
        choice: settings.theme.clone(),
        light: settings.theme_light.clone(),
        dark: settings.theme_dark.clone(),
        themes: settings.themes.clone(),
    }
}

/// The theme to draw with: `D` (or the command line) first, then calm.kdl's choice, then the desktop.
fn theme_for(override_dark: Option<bool>, system_dark: bool, translucent: Option<f32>, calendars: &[Calendar], looks: &crate::theme::Looks) -> Theme {
    let dark = override_dark.or(looks.forced()).unwrap_or(system_dark);
    let theme = looks.build(dark).with_calendars(calendars);
    match translucent {
        Some(opacity) => theme.translucent(opacity),
        None => theme,
    }
}

impl Render for CalmView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_drawer(window, cx);
        self.sync_prompt(window, cx);
        self.refresh_search(cx);
        // Do Not Disturb may change from outside (`calm dnd`, a status bar) or run out.
        if self.dnd_read.elapsed() > Duration::from_secs(2) {
            self.dnd = crate::notify::dnd(chrono::Utc::now());
            self.dnd_read = Instant::now();
        }
        if !self.hits.is_empty()
            && let Some(typed) = self.pending_pick.take()
        {
            self.start_hints(cx);
            for c in typed.chars() {
                self.hint_key(&crate::keys::Press { key: c.to_string(), typed: Some(c.to_string()), ..Default::default() });
            }
        }
        let t0 = Instant::now();
        let size = window.viewport_size();
        let viewport = (f32::from(size.width), f32::from(size.height));
        self.stable_frames += 1;
        if viewport != self.viewport {
            self.stable_frames = 0;
            self.viewport = viewport;
        }

        // After idle, treat the first frame as a nominal 60 Hz step.
        let interval = match (self.animating, self.last_frame) {
            (true, Some(last)) => t0.duration_since(last).as_secs_f32(),
            _ => 1. / 60.,
        };
        if self.animating && self.last_frame.is_some() {
            self.stats.push(interval * 1000., self.cpu_last.get());
        }
        self.last_frame = Some(t0);
        if self.bench.as_ref().is_some_and(|b| b.report) && interval > 0.025 {
            eprintln!("long interval {:.1}ms after: {}", interval * 1000., self.last_trace);
        }
        self.tick_bench(interval * 1000., cx);

        self.snap();
        self.update_lod();
        if self.dirty {
            self.relayout();
        }
        self.sync_color();
        let dt = if self.still { 5. } else { interval.min(1. / 20.) };
        let first = self.stats.intervals.is_empty() && self.stable_frames <= 1;
        let moving = self.step(dt);
        if first {
            // Open at the right margins rather than animating into them.
            self.margin_l = Spring::new(self.margin_l.target);
            self.margin_b = Spring::new(self.margin_b.target);
            self.margin_t = Spring::new(self.margin_t.target);
        }
        let t_build = Instant::now();
        let built = self.build(window);
        let build_ms = t_build.elapsed().as_secs_f32() * 1000.;
        let trace = self.bench.as_ref().is_some_and(|b| b.report);
        let trace_info = format!(
            "lens={} n={:.0} mark={:?} flipping={} counts={:?} pre={:.1}",
            self.preset,
            self.arr().n,
            self.lens().mark,
            self.flipping.len(),
            built.counts,
            t_build.duration_since(t0).as_secs_f32() * 1000.
        );
        self.last_trace = trace_info.clone();
        self.drawn = built.counts;
        let waiting = self.pending.0.is_some() || self.pending.1.is_some();
        if waiting && self.stable_frames > 3 {
            // In a mode, `--select` is the press (a new event's start, or a drag's) and
            // `--hover` where the pointer is now.
            if self.mode.is_some() {
                if let Some(pos) = self.pending.1.take() {
                    self.mode_down(pos, cx);
                }
                if let Some(pos) = self.pending.0 {
                    self.mode_move(pos);
                }
            }
            if let Some(pos) = self.pending.0.take() {
                self.mouse = pos;
                self.hover_zone = self.zone_hits.borrow().iter().find(|(r, _)| pos.0 >= r[0] && pos.0 <= r[2] && pos.1 >= r[1] && pos.1 <= r[3]).map(|(_, z)| *z);
                self.hovered = self.hit(pos);
                self.hover_day = if self.hovered.is_none() { self.overview_day(pos) } else { None };
            }
            if let Some(pos) = self.pending.1.take() {
                self.selected = self.hit(pos);
            }
        }
        self.animating = moving || built.relabel || waiting || self.dirty || self.bench.is_some() || self.jump_mark.is_some();
        if self.animating {
            window.request_animation_frame();
        }

        let frame = built.frame;
        let cpu_last = self.cpu_last.clone();
        let th = self.theme;

        div()
            .size_full()
            .bg(th.bg)
            .font_family("Inter")
            .text_color(th.ink)
            .track_focus(&self.focus)
            // While you type in a field, keys are text, not shortcuts.
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if this.prompting(window, cx) {
                    if let Some(cmd) = this.command_for(crate::keys::Ctx::Search, &commands::press(&ev.keystroke))
                        && this.prompt_command(&cmd, cx)
                    {
                        cx.notify();
                    }
                } else if !this.typing(window, cx) {
                    this.on_key(ev, cx)
                }
            }))
            .on_mouse_move(cx.listener(|this, ev, _, cx| this.on_move(ev, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev, window, cx| {
                    window.focus(&this.focus);
                    this.on_down(ev, cx)
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.mode_up(cx);
                    cx.notify();
                }),
            )
            .on_scroll_wheel(cx.listener(|this, ev, _, cx| this.on_scroll(ev, cx)))
            .when_some(self.mode_cursor(), |d, c| d.cursor(c))
            // Bar backgrounds sit under the canvas, so headers and labels draw over them.
            .children(self.chrome_bands())
            .child(
                canvas(
                    |_, _, _| {},
                    move |_, _, window, cx| {
                        scene::paint(frame, window, cx);
                        let total = t0.elapsed().as_secs_f32() * 1000.;
                        cpu_last.set(total);
                        if trace && total > 10. {
                            eprintln!("slow: {trace_info} build {build_ms:.1} total {total:.1}ms");
                        }
                    },
                )
                .size_full(),
            )
            .when(!scene::skip("chrome"), |d| d.child(self.top_bar(cx)).child(self.zoom_slider(cx)).child(self.switcher(cx)))
            .children(self.tooltip_card())
            .children(self.drawer_card(cx))
            .children(self.editor_panel(cx))
            .children(self.menu_panel(cx))
            .children(self.mode_bar(cx))
            .children(self.hints_layer())
            .children(self.prompt_panel(cx))
            .children(self.toast_card(cx))
            .children(self.changes_panel(cx))
            .children(self.help_card())
            .children(self.hud())
    }
}

#[derive(Default)]
pub struct Options {
    pub bench: bool,
    pub still: bool,
    /// Open with the view editor showing (screenshots).
    pub edit: bool,
    /// Open with the top-right menu showing (screenshots).
    pub menu: bool,
    /// Open with the changes panel showing (screenshots).
    pub changes: bool,
    /// Open with the keyboard help showing (screenshots).
    pub help: bool,
    /// `--pick LETTERS`: start picking by letters (for screenshots), with these typed ("" for none).
    pub pick: Option<String>,
    /// Start with travel shown, as `W` does.
    pub travel: bool,
    /// Start in an editing mode: `new` or `adjust` (screenshots).
    pub mode: Option<String>,
    /// Open `/` or `:` with this text typed (screenshots).
    pub search: Option<String>,
    pub jump: Option<String>,
    /// Open the drawer's picker (0 date, 1 start, 2 end) once an event is selected (screenshots).
    pub picker: Option<u8>,
    /// Open with the controls popup showing, when they're folded into a button (screenshots).
    pub controls: bool,
    pub cycle: bool,
    /// Background opacity; `None` is an opaque window.
    pub opacity: Option<f32>,
    /// Ask the compositor to blur what shows through.
    pub blur: bool,
    pub lens: Option<usize>,
    pub dark: Option<bool>,
    pub offset: i32,
    pub zoom: Option<String>,
    pub hover: Option<Pt>,
    pub select: Option<Pt>,
    /// `measure[:compose]`, e.g. `hours:stripes`; a compose implies by-calendar.
    pub color: Option<String>,
    pub span: bool,
    /// Extra time zones, overriding the config file: `America/New_York,Europe/London`.
    pub zones: Option<String>,
    /// Synthetic data even when sources are configured.
    pub demo: bool,
}

impl Options {
    pub fn from_args() -> Self {
        let mut o = Self::default();
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            match a.as_str() {
                "--bench" => o.bench = true,
                "--still" => o.still = true,
                "--edit" => o.edit = true,
                "--menu" => o.menu = true,
                "--changes" => o.changes = true,
                "--mode" => o.mode = args.next(),
                "--help-card" => o.help = true,
                "--pick" => o.pick = Some(args.next().unwrap_or_default()),
                "--travel" => o.travel = true,
                "--search" => o.search = args.next(),
                "--jump" => o.jump = args.next(),
                "--picker" => o.picker = args.next().and_then(|n| n.parse().ok()),
                "--controls" => o.controls = true,
                "--cycle" => o.cycle = true,
                // Like a translucent terminal: a mostly solid background, no blur.
                "--translucent" => o.opacity = o.opacity.or(Some(0.8)),
                "--opacity" => o.opacity = args.next().and_then(|v| v.parse::<f32>().ok()).map(|v| v.clamp(0.2, 1.)),
                "--blur" => {
                    o.blur = true;
                    o.opacity = o.opacity.or(Some(0.8));
                }
                "--lens" => o.lens = args.next().and_then(|n| n.parse::<usize>().ok()).map(|n| n.saturating_sub(1)),
                "--theme" => o.dark = args.next().map(|t| t == "dark"),
                "--offset" => o.offset = args.next().and_then(|n| n.parse().ok()).unwrap_or(0),
                "--zoom" => o.zoom = args.next(),
                "--hover" => o.hover = args.next().and_then(|s| parse_pt(&s)),
                "--select" => o.select = args.next().and_then(|s| parse_pt(&s)),
                "--color" => o.color = args.next(),
                "--span" => o.span = true,
                "--zones" => o.zones = args.next(),
                "--demo" => o.demo = true,
                _ => eprintln!("calm: ignoring unknown argument {a}"),
            }
        }
        o
    }
}

fn parse_pt(s: &str) -> Option<Pt> {
    let (x, y) = s.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// Live-sync state held by the view.
struct Sync {
    rx: std::sync::mpsc::Receiver<crate::sync::Update>,
    commands: std::sync::mpsc::Sender<crate::sync::Command>,
    sources: Vec<crate::sync::Source>,
    states: Vec<crate::sync::store::SourceState>,
    problems: Vec<String>,
    /// The outbox as last seen: its fingerprint, and each change's state.
    outbox_seen: String,
    outbox: Vec<crate::sync::outbox::State>,
}

/// Reads every configured source's occurrences from the offline cache.
/// Whether queued changes are drawn as if done (the changes panel's Preview), or the calendar
/// as the server has it.
pub(super) static PREVIEW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

fn load_live(sources: &[crate::sync::Source], calendars: &[Calendar]) -> Dataset {
    let span = model::generate_span();
    let store = crate::sync::store::Store::open(&crate::sync::db_path()).ok();
    let mut stored = store.as_ref().and_then(|s| s.occurrences(span.0, span.1).ok()).unwrap_or_default();
    // Queued changes show as if done, marked, until the server confirms them.
    if let Some(s) = &store {
        crate::sync::google::overlay(s, &mut stored, chrono::Utc::now(), PREVIEW.load(std::sync::atomic::Ordering::Relaxed));
    }
    // Which calendar each stored key belongs to: an iCal source's own, or for a Google
    // account (`Account/calendar-id`) the calendar named like that Google calendar.
    let index = |name: &str| calendars.iter().position(|c| c.name.eq_ignore_ascii_case(name));
    let mut keys: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for s in sources {
        keys.insert(s.name.clone(), s.calendar);
        if matches!(s.location, crate::sync::Location::Google(_)) {
            for c in store.as_ref().and_then(|st| st.calendars(&s.name).ok()).unwrap_or_default() {
                keys.insert(format!("{}/{}", s.name, c.id), index(&c.name).unwrap_or(0));
            }
        }
    }
    // Calendars you may change: Google calendars you own or can write to.
    let mut editable: std::collections::HashSet<String> = std::collections::HashSet::new();
    for s in sources {
        if matches!(s.location, crate::sync::Location::Google(_)) {
            for c in store.as_ref().and_then(|st| st.calendars(&s.name).ok()).unwrap_or_default() {
                if c.access == "owner" || c.access == "writer" {
                    editable.insert(format!("{}/{}", s.name, c.id));
                }
            }
        }
    }
    model::from_occurrences(&stored, |key| keys.get(key).copied().unwrap_or(0), |key| editable.contains(key), calendars)
}

impl CalmView {
    /// Drains sync news: reload data when a source changed, refresh status otherwise.
    fn poll_sync(&mut self, cx: &mut Context<Self>) {
        let Some(sync) = &mut self.sync else { return };
        let (mut changed, mut any) = (false, false);
        while let Ok(update) = sync.rx.try_recv() {
            any = true;
            changed |= matches!(update, crate::sync::Update::Changed);
        }
        // Changes queued elsewhere (the command line) or sent since: show them.
        if let Ok(store) = crate::sync::store::Store::open(&crate::sync::db_path()) {
            let seen = store.outbox_fingerprint();
            if seen != sync.outbox_seen {
                sync.outbox = store.ops().unwrap_or_default().iter().map(|o| o.state).collect();
                sync.outbox_seen = seen;
                (any, changed) = (true, true);
            }
        }
        if !any {
            // Ages in the status line ("3 min ago") still move.
            return;
        }
        sync.states = crate::sync::store::Store::open(&crate::sync::db_path()).and_then(|s| s.states()).unwrap_or_default();
        if changed {
            // A Google account may have brought new calendars: give them chips first.
            let calendars = calendars_for(&self.conf.settings, false);
            if calendars != self.calendars {
                self.hidden = std::array::from_fn(|c| c >= calendars.len());
                self.theme = self.theme.with_calendars(&calendars);
                self.calendars = calendars;
            }
            let Some(sync) = &self.sync else { return };
            let data = load_live(&sync.sources, &self.calendars);
            self.set_data(data);
        }
        cx.notify();
    }

    /// One line for the top bar: how fresh the calendars are.
    fn sync_line(&self) -> Option<(String, bool)> {
        let sync = self.sync.as_ref()?;
        if let Some(p) = sync.problems.first() {
            return Some((p.clone(), true));
        }
        // Queued changes come first: they're yours.
        use crate::sync::outbox::State as Q;
        let count = |states: &[Q]| sync.outbox.iter().filter(|s| states.contains(s)).count();
        let plural = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
        let (attention, staged, waiting) = (count(&[Q::Failed, Q::Conflict]), count(&[Q::Draft]), count(&[Q::Pending, Q::Sending]));
        if attention > 0 {
            return Some((format!("{} need{} attention", plural(attention, "change"), if attention == 1 { "s" } else { "" }), true));
        }
        if staged > 0 {
            return Some((format!("{} staged", plural(staged, "change")), false));
        }
        if waiting > 0 {
            return Some((format!("{} waiting to sync", plural(waiting, "change")), false));
        }
        let now = chrono::Utc::now().timestamp();
        let ago = |t: i64| match (now - t).max(0) / 60 {
            0 => "just now".to_string(),
            m if m < 60 => format!("{m} min ago"),
            m => format!("{} h ago", m / 60),
        };
        let states: Vec<_> = sync.sources.iter().map(|s| sync.states.iter().find(|st| st.name == s.name)).collect();
        if let Some(st) = states.iter().flatten().find(|st| st.last_error.is_some() && st.failures > 0) {
            let when = st.last_ok_at.map(|t| format!(" · showing cache from {}", ago(t))).unwrap_or_default();
            return Some((format!("{}: {}{when}", st.name, st.last_error.as_deref().unwrap_or("sync failed")), true));
        }
        if states.iter().any(|s| s.and_then(|s| s.last_ok_at).is_none()) {
            return Some(("Syncing…".into(), false));
        }
        let oldest = states.iter().flatten().filter_map(|s| s.last_ok_at).min()?;
        Some((format!("Synced {}", ago(oldest)), false))
    }
}

/// Fresh zoom state for a view: its first stop of at least a line, around today.
fn new_lens(spec: &Spec, today: i32) -> Lens {
    let p = spec.facet.per_line;
    let first = spec.stops.iter().find(|s| s.cells >= p).unwrap_or(&spec.stops[0]);
    let n = first.cells as f32;
    let start = match spec.facet.dir {
        // Days run down columns: with many weeks in view, put today's a few columns in
        // (a third of the way, at most four); with one, start on it.
        Dir::Columns => {
            let back = ((first.cells / p.max(1)) / 3).min(4);
            ((today.div_euclid(p) - back).max(0) * p) as f32
        }
        Dir::Rows => (today.div_euclid(p) * p) as f32,
    };
    let hours = spec.hours;
    Lens {
        zoom: Spring::new(n.log2()),
        start: Spring::new(start),
        hours: [Spring::new(hours.0), Spring::new(hours.1)],
        default_hours: hours,
        mark: spec.mark,
        heat: Spring::new(if spec.mark == Mark::Heat { 1. } else { 0. }),
        grid: Spring::new(if spec.mark == Mark::Heat { 0. } else { 1. }),
        last_input: None,
        gesture_from: None,
    }
}

/// Modification times of config files, to notice edits.
fn stamp(files: &[std::path::PathBuf]) -> Vec<(std::path::PathBuf, Option<std::time::SystemTime>)> {
    files.iter().map(|f| (f.clone(), std::fs::metadata(f).and_then(|m| m.modified()).ok())).collect()
}

/// Files to watch: everything the config read, plus calm.kdl itself even if absent (so creating it works).
fn watch_list(files: &[std::path::PathBuf]) -> Vec<(std::path::PathBuf, Option<std::time::SystemTime>)> {
    let mut files = files.to_vec();
    if let Some(p) = crate::viewconf::user_path()
        && !files.contains(&p)
    {
        files.push(p);
    }
    stamp(&files)
}

impl CalmView {
    /// Re-reads the view config. Invalid configs keep the current views and report why;
    /// valid ones apply, keeping each surviving view's zoom and param choices by name.
    fn reload_config(&mut self) {
        let (config, diags) = crate::viewconf::load();
        self.config_files = watch_list(&config.files);
        self.config_problems = diags.iter().map(|d| d.to_string()).collect();
        if !diags.is_empty() || config.views.is_empty() {
            return;
        }
        self.apply_settings(&config.settings);
        let today = self.data.today;
        let current = self.views[self.preset].name.clone();
        let (mut lenses, mut values, mut layers) = (Vec::new(), Vec::new(), Vec::new());
        for v in &config.views {
            match self.views.iter().position(|o| o.name == v.name) {
                Some(i) => {
                    let mut lens = self.lenses[i].clone();
                    lens.mark = v.mark;
                    lens.default_hours = v.hours;
                    lenses.push(lens);
                    let mut vals = v.defaults();
                    for (k, val) in &self.values[i] {
                        if vals.contains_key(k) {
                            vals.insert(k.clone(), val.clone());
                        }
                    }
                    values.push(vals);
                    layers.push(self.layers[i]);
                }
                None => {
                    lenses.push(new_lens(v, today));
                    values.push(v.defaults());
                    layers.push(Spring::new(0.));
                }
            }
        }
        self.preset = config.views.iter().position(|v| v.name == current).unwrap_or(0);
        for (i, l) in layers.iter_mut().enumerate() {
            l.target = if i == self.preset { 1. } else { 0. };
        }
        self.switcher = Spring::new(self.preset as f32);
        self.conf = config.clone();
        self.views = config.views;
        (self.lenses, self.values, self.layers) = (lenses, values, layers);
        if let Some(side) = self.views[self.preset].split.as_ref().and_then(|s| s.view.clone()) {
            self.side = Some(side);
        }
        self.dirty = true;
        self.stagger = true;
    }

    /// The top bar's status: config problems first, then sync.
    fn status_line(&self) -> Option<(String, bool)> {
        match self.config_problems.first() {
            Some(p) => {
                // `path:line:col: message` → `file:line:col: message`, kept short for the top bar.
                let short = p.rsplit('/').next().unwrap_or(p);
                let short: String = if short.chars().count() > 72 { short.chars().take(71).chain(std::iter::once('…')).collect() } else { short.to_string() };
                Some((short, true))
            }
            None => self.sync_line(),
        }
    }
}

#[derive(Clone, Copy)]
struct Pinned {
    zones: bool,
    theme: bool,
    /// `--demo` or `--bench`: never sync.
    synthetic: bool,
}

/// Starts the background sync engine for `sources`.
fn start_sync(sources: &[crate::sync::Source], problems: &[String]) -> Sync {
    let (rx, commands) = crate::sync::start(sources.to_vec());
    let states = crate::sync::store::Store::open(&crate::sync::db_path()).and_then(|s| s.states()).unwrap_or_default();
    Sync { rx, commands, sources: sources.to_vec(), states, problems: problems.to_vec(), outbox_seen: String::new(), outbox: Vec::new() }
}

impl CalmView {
    /// Applies non-view settings from a freshly loaded config: zones, theme and sources.
    fn apply_settings(&mut self, settings: &crate::viewconf::Settings) {
        self.resolve_place(settings);
        let energy = settings.energy.clone().unwrap_or_default();
        if energy != self.energy {
            self.energy = energy;
            self.data.set_energy(&self.energy);
            self.restat();
            self.dirty = true;
        }
        if !self.pinned.zones {
            self.zones = crate::zones::Zones::load(None, settings.zones.as_ref());
        }
        let looks = looks_of(settings);
        if looks != self.looks {
            // A new choice of theme takes over from `D`, unless the command line pinned one.
            if !self.pinned.theme {
                self.theme_override = None;
            }
            self.looks = looks;
            self.retheme();
        }
        if self.pinned.synthetic {
            return;
        }
        let calendars = calendars_for(settings, false);
        let recolour = calendars != self.calendars;
        if recolour {
            // New calendars: fresh chips (all shown) and colours; events re-map below.
            self.hidden = std::array::from_fn(|c| c >= calendars.len());
            self.theme = self.theme.with_calendars(&calendars);
            self.calendars = calendars;
        }
        let names: Vec<String> = self.calendars.iter().map(|c| c.name.clone()).collect();
        let (sources, problems) = crate::sync::load_sources(settings.sources.as_deref(), &names);
        let key = |s: &[crate::sync::Source]| s.iter().map(|x| format!("{}|{}", x.name, x.config_hash)).collect::<Vec<_>>();
        let current = self.sync.as_ref().map(|s| key(&s.sources)).unwrap_or_default();
        if recolour || key(&sources) != current {
            // Dropping the old engine's command channel stops its thread.
            self.sync = (!sources.is_empty()).then(|| start_sync(&sources, &problems));
            let data = if sources.is_empty() { model::generate(1) } else { load_live(&sources, &self.calendars) };
            self.set_data(data);
        } else if let Some(s) = &mut self.sync {
            s.problems = problems;
        }
    }
}

impl CalmView {
    fn edit_channel(&mut self, child: &str, prop: Option<&str>, value: &str) {
        let name = self.spec().name.clone();
        self.editor_note = Some(match crate::viewconf::edit_view(&self.conf, &name, child, prop, value) {
            Ok(()) => {
                self.reload_config();
                ("Saved".into(), false)
            }
            Err(e) => (e.rsplit('/').next().unwrap_or(&e).to_string(), true),
        });
    }

    fn duplicate_current(&mut self) {
        let name = self.spec().name.clone();
        self.editor_note = Some(match crate::viewconf::duplicate_view(&self.conf, &name) {
            Ok(copy) => {
                self.reload_config();
                if let Some(i) = self.views.iter().position(|v| v.name == copy) {
                    self.set_preset(i);
                }
                (format!("Made “{copy}”"), false)
            }
            Err(e) => (e, true),
        });
    }

    fn reset_current(&mut self) {
        let name = self.spec().name.clone();
        self.editor_note = Some(match crate::viewconf::reset_view(&name) {
            Ok(()) => {
                self.reload_config();
                (format!("“{name}” reset"), false)
            }
            Err(e) => (e, true),
        });
    }
}

impl CalmView {
    /// Indices of every piece of event group `g`, in day order. Pieces of one event
    /// are a day apart, so look near the hovered one rather than scanning everything.
    pub(super) fn group_of(&self, g: u32) -> Vec<usize> {
        let events = &self.data.events;
        let Some(i) = self.hovered.filter(|&i| events[i].group == g).or_else(|| events.iter().position(|e| e.group == g)) else {
            return Vec::new();
        };
        let day = events[i].day;
        // Events are sorted by day; a multi-day event spans at most a few weeks.
        let lo = events.partition_point(|e| e.day < day - 31);
        let hi = events.partition_point(|e| e.day <= day + 31);
        (lo..hi).filter(|&k| events[k].group == g).collect()
    }
}

/// The calendar list: the demo's for synthetic data or when nothing syncs, else as
/// declared in calm.kdl, else derived from the sources.
pub(crate) fn calendars_for(settings: &crate::viewconf::Settings, synthetic: bool) -> Vec<Calendar> {
    let wanted = crate::sync::wanted_calendars(settings.sources.as_deref());
    if synthetic || wanted.is_empty() {
        return model::demo_calendars();
    }
    model::resolve_calendars(settings.calendars.as_deref(), &wanted)
}

impl CalmView {
    /// What view `p`'s rail shows right now, and whether its width follows the value.
    /// It needs a cartesian time axis inside its cells (and a place, for daylight).
    pub(super) fn rail_of(&self, p: usize) -> Option<(crate::spec::Rail, bool)> {
        let spec = &self.views[p];
        let timed = spec.x == Axis::TimeOfDay || spec.y == Axis::TimeOfDay;
        let mark = self.lenses[p].mark;
        if spec.coord != Coord::Cartesian || !timed || mark != spec.mark {
            return None;
        }
        spec.rail(&self.over, &self.values[p], mark).filter(|(kind, _)| match kind {
            crate::spec::Rail::Daylight => self.place.is_some(),
            crate::spec::Rail::Zone | crate::spec::Rail::Travel => self.travel.is_some(),
            crate::spec::Rail::Energy => true,
        })
    }

    /// Where you are: the configured location, else your zone's principal city.
    fn resolve_place(&mut self, settings: &crate::viewconf::Settings) {
        self.place = settings.location.or_else(|| self.zones.primary.and_then(|tz| crate::sun::zone_place(tz.name())));
    }
}

impl CalmView {
    fn edit_energy(&mut self, prop: &str, value: kdl::KdlValue) {
        self.editor_note = Some(match crate::viewconf::edit_setting("energy", prop, value) {
            Ok(()) => {
                self.reload_config();
                ("Saved".into(), false)
            }
            Err(e) => (e.rsplit('/').next().unwrap_or(&e).to_string(), true),
        });
    }
}

impl CalmView {
    pub(super) fn sync_now(&self) {
        if let Some(s) = &self.sync {
            let _ = s.commands.send(crate::sync::Command::Refresh);
        }
    }

    pub(super) fn toggle_theme(&mut self) {
        let dark = !self.theme.dark;
        self.theme_override = Some(dark);
        self.retheme();
    }

    /// Rebuilds the theme from the current choices.
    pub(super) fn retheme(&mut self) {
        self.theme = theme_for(self.theme_override, self.system_dark, self.translucent, &self.calendars, &self.looks);
    }
}

impl CalmView {
    /// Rows of all-day bars view `p` sets aside above the time axis on `day`'s line: as many
    /// as the most stacked day of that line needs, so the time axis lines up along a line
    /// (a week in Grid; just the day where each day is its own line). None unless the view
    /// shows all-day events and draws timed marks (lists and aggregates show them their own
    /// way; dials put them in the centre).
    pub(super) fn band_rows(&self, p: usize, day: i32) -> u8 {
        let spec = &self.views[p];
        let mark = self.lenses[p].mark;
        if !spec.all_day || spec.coord != Coord::Cartesian || !matches!(mark, Mark::Block | Mark::Pill) {
            return 0;
        }
        let per_line = spec.facet.per_line.max(1);
        let start = day.div_euclid(per_line) * per_line;
        (start..start + per_line).filter_map(|d| self.data.all_day_rows.get(d as usize).copied()).max().unwrap_or(0)
    }

    /// `band_rows` for whichever day the cell `cell` shows.
    fn band_rows_at(&self, p: usize, cell: [f32; 4]) -> u8 {
        let spec = &self.views[p];
        if !spec.all_day || spec.coord != Coord::Cartesian || !matches!(self.lenses[p].mark, Mark::Block | Mark::Pill) {
            return 0;
        }
        let centre = ((cell[0] + cell[2]) * 0.5, (cell[1] + cell[3]) * 0.5);
        self.arr_of(p, false).day_at(centre).map_or(0, |d| self.band_rows(p, d))
    }
}

/// App-wide key bindings (text fields).
pub fn bind_keys(cx: &mut App) {
    input::bind_keys(cx);
}

impl CalmView {
    /// Whether changes to `key`'s source wait for review (`sync writes="review"`).
    fn review_mode(&self, key: &str) -> bool {
        let source = key.split('/').next().unwrap_or(key);
        let s = &self.conf.settings;
        let per_source = s.sources.as_deref().and_then(|ss| ss.iter().find(|d| d.name == source)).and_then(|d| d.writes.clone());
        per_source.or(s.writes.clone()).as_deref() == Some("review")
    }

    /// Queues a change you made: staged in review mode, otherwise sent after a short pause
    /// (so a burst of edits to one event goes as one). Shows it at once either way.
    pub(super) fn queue_change(&mut self, op: crate::sync::outbox::Op, cx: &mut Context<Self>) -> Option<i64> {
        self.queue_change_held(op, 2, cx)
    }

    /// `queue_change`, sent no sooner than `hold` seconds from now (while an undo is offered).
    pub(super) fn queue_change_held(&mut self, mut op: crate::sync::outbox::Op, hold: i64, cx: &mut Context<Self>) -> Option<i64> {
        use crate::sync::outbox::State;
        let review = self.review_mode(&op.key());
        op.state = if review { State::Draft } else { State::Pending };
        op.next_attempt_at = (!review).then(|| chrono::Utc::now().timestamp() + hold);
        let seq = crate::sync::store::Store::open(&crate::sync::db_path()).ok().and_then(|mut s| s.queue(op).ok()).flatten();
        self.reload_live();
        if !review {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(hold as u64 * 1000 + 200)).await;
                let _ = this.update(cx, |this, _| {
                    if let Some(s) = &this.sync {
                        let _ = s.commands.send(crate::sync::Command::Flush);
                    }
                });
            })
            .detach();
        }
        seq
    }

    /// Rebuilds the data from the cache and outbox (after a change of yours).
    pub(super) fn reload_live(&mut self) {
        if let Some(sync) = &self.sync {
            let data = load_live(&sync.sources, &self.calendars);
            self.set_data(data);
        }
    }
}
