# calm

A calendar built on a *grammar of time*: every view is a declarative spec (coordinates × axes
× collision × mark), and switching views morphs the same marks rather than cutting to a new
screen. See [DESIGN.md](DESIGN.md) for the idea and [docs/gpui-spike.md](docs/gpui-spike.md) for
why gpui.

calm reads Google Calendar (through its API, [docs/google.md](docs/google.md)) and iCal feeds into
an offline cache ([docs/storage-sync.md](docs/storage-sync.md)), and edits Google events through a
reviewable outbox of changes ([docs/editing.md](docs/editing.md)). With no sources configured, or
with `--demo`, it shows synthetic data around today's date.

## Installing (Nix)

calm is a flake (Linux for now):

```sh
nix run github:<you>/calm            # or, from a checkout:
nix run .                            # build and run
nix profile install .                # install `calm`
```

Or add it to a NixOS or home-manager configuration through the overlay:

```nix
# flake inputs
inputs.calm.url = "path:/path/to/calm";   # or a git URL
# then
nixpkgs.overlays = [ inputs.calm.overlays.default ];
home.packages = [ pkgs.calm ];            # or environment.systemPackages
```

The package wraps `calm` with the graphics and windowing libraries gpui loads at runtime (Vulkan,
Wayland, X11), so it runs as is; a working Vulkan driver is the one thing your system provides.

## Running from source

```sh
devenv shell            # or `direnv allow` once
cargo run --release
```

| Keys | |
|---|---|
| `1`–`9`, `Tab` | Switch view (Grid · Lanes · Energy · Dial · Density by default) |
| Ctrl+scroll, `−` `=`, right-edge slider | Zoom: Day · Week · Month · Quarter · Year (continuous, snaps to stops) |
| `←` `→` | Previous / next at the current zoom (day, week, month…) |
| Scroll, Shift+scroll | Scroll hours when a single day/week is in view, otherwise scroll through time |
| Alt+scroll | Zoom the hour window |
| `T` | Today |
| `D` | Toggle light / dark (follows the system by default) |
| `C` / `B` / `V` | Cycle colour measure · by calendar (split cells, or calendar hues at the measure's strength) · cycle composition |
| `G` | Day-span glyph in aggregate cells |
| `F`, click the hour labels | Toggle between the default hours and the full 24 |
| `Z` | Show / hide extra time zones |
| `R` | Sync calendars now |
| `E` | Edit the current view (layout, axes, mark, rail: off · energy · daylight) and your energy settings; saved to `views.generated.kdl` |
| `M`, ☰ top right | Menu: secondary calendars (and any that don't fit the top bar), edit view, sync, theme, help |
| `N` | New event (next half hour, in your default calendar) |
| Click an event | Details; for your Google events, edit the title, date, time and location in place (Enter or clicking away saves) |
| `Delete` | Delete the selected event, with Undo |
| `O` | Changes: what's staged, waiting, failed or in conflict; commit, revert, preview |
| `?` | Help · `` ` `` frame stats |

The top bar shows the time here, with the time in each extra zone beneath it.

### Configuration

Everything lives in one file, `~/.config/calm/calm.kdl`, applied live as you save:

```kdl
zones "Asia/Manila" "Europe/London" {
    label "Europe/London" short="🇬🇧" long="London"
}
source "Personal" url-file="~/.config/calm/secrets/personal.url" calendar="Personal" refresh="15m"
window opacity=0.8
theme "system"
```

Every view is declared in KDL too: the built-ins live in `assets/views.kdl`, and calm.kdl can add,
replace, hide and reorder views. Views expose params (the colour controls, Day span) that calm turns
into controls and keys. `E` opens an editor for the current view; its changes go to
`views.generated.kdl`, loaded after calm.kdl, so your own file is never rewritten.

```sh
calm config check [--json]                 # validate, with file:line:col errors
calm config dump                           # the effective config, built-ins included
calm config set Grid mark pill             # the editor's operations, for scripts and agents
calm config duplicate Grid · calm config reset "Grid copy"
```

Reference: [docs/config.md](docs/config.md).

### Changes from the command line

Google calendars can be edited from the command line. Every change is staged until you commit it,
so an agent can propose changes for you to review:

```sh
calm event list --day tomorrow          # with IDs
calm event add "Focus" --start "tomorrow 9:00" --duration 90m
calm event move <id> --start 10:30      # --all for a whole recurring series
calm event delete <id>
calm outbox                             # what's staged, waiting, failed or in conflict
calm commit --all                       # asks at a terminal; --yes otherwise
calm discard <n>
```

Changes show in calm straight away (dimmed with a dot until Google confirms them) and are sent
conditionally, so someone else's concurrent change is merged or shown as a conflict, never
overwritten. Design: [docs/editing.md](docs/editing.md).

### Calendars

For Google, sign in once and calm reads every calendar in your account, free/busy-only shares
included, and keeps them in sync incrementally: `calm google login you@gmail.com`, then
`source "Google" google="you@gmail.com"`. Setup (your own OAuth client): [docs/google.md](docs/google.md).

Each `source` in calm.kdl is a calendar feed: `url-file=` (a file holding the secret iCal address,
chmod 600), `url=`, or `path=` for a local `.ics`; `refresh=` how often to fetch. `calendar=` says which
calendar its events belong to. Calendars are the chips in the top bar; declare them to set their
colour and how demanding their events are:

```kdl
calendar "Work" color="slate" demand=0.6
calendar "Home" color="sage" demand=0.3
source "Work" url-file="~/.config/calm/secrets/work.url" calendar="Work"
```

Without `calendar` nodes, each source's `calendar=` (or name) becomes one, coloured from the
palette. Events
are cached in `~/.local/state/calm/calendar.sqlite3`, so calm opens instantly and works offline;
the top bar shows when each source last synced or why it failed. `R` syncs now. A failed or
suspicious fetch (an error page, a feed that suddenly loses most of its events) never replaces
the cached copy.

### Time zones

Extra zones are a global setting: `zones "America/New_York" "Europe/London"` in calm.kdl, or
`--zones`. The primary zone is the system's. Grid gets a column of
hour labels per zone, and Lanes and Energy get a row per zone; a trailing `+` or `−` means the next or
previous day. Each view's grammar can narrow labelling to the primary zone (`zones: primary`, used
by Dial). The details drawer shows an event's time in every zone.

Each zone can have a short label for the axes (a flag works well) and a long name, shown with the
current time and UTC offset when you hover a zone label: `label "Asia/Manila" short="🇵🇭" long="Manila"`
inside `zones`.

Whichever configured zone you are currently in is dropped, so the same config works wherever you are.

The older `~/.config/calm/config` and `sources` files are still read when calm.kdl doesn't set zones
or sources.

Click an event to see its details. Click an overview cell (a month or year box) to zoom into it. The calendar
chips in the top bar show and hide calendars, and the remaining lanes widen to fill.

### Developer flags

| Flag | |
|---|---|
| `--lens N`, `--zoom STOP`, `--theme dark\|light`, `--offset DAYS`, `--zones A,B` | Start state |
| `--color MEASURE[:scale\|:calendar\|:COMPOSE]`, `--span`, `--edit` | Start colour, day-span glyph, view editor open |
| `--translucent`, `--opacity X`, `--blur` | Translucent window: 0.8 background opacity by default, optional compositor blur |
| `--still` | Springs settle in one frame (for screenshots) |
| `--hover x,y`, `--select x,y` | Scripted pointer, applied once the window size settles |
| `--cycle` | Advance lenses every 1.1 s |
| `--demo` | Synthetic data even when sources are configured |
| `--bench` | 2× data (`CALM_MULT` to change), scripted lens + zoom tour for 22 s, print frame/CPU percentiles. `CALM_SKIP=decor,marks,labels,chrome` disables parts for bisection |

## Layout

- `src/spec.rs`: the grammar: channels, params, and how values resolve.
- `src/viewconf.rs`: the KDL loader (views, params, settings), validation, `calm config`, and the editor's writes.
- `assets/views.kdl`: the built-in views.
- `src/layout.rs`: collision and projection into *inner* cell coordinates. Every mark belongs to a day
  cell, so zooming and scrolling never relayout.
- `src/view/mod.rs`: springs, input, and the render loop. No frames are requested once
  everything settles.
- `src/view/arrange.rs`: the facet arrangement (strip → lines → bands, blended) and in-cell coordinates.
- `src/view/scene.rs`: screen-space morphs, marks, label placement, arc text,
  axes, and paint.
- `src/view/chrome.rs`: top bar, lens switcher, tooltip, drawer, and help.
- `patches/gpui`: gpui 0.2.2 plus `Window::paint_glyph_rotated`, which sets text along arcs.
- `patches/xattr`: a build fix for current `libc`.
- `assets/fonts`: Inter (OFL), subset to Latin.
