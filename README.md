# calm

A calm, read-only calendar viewer built on a *grammar of time*: every view is a declarative
spec (coordinates × axes × collision × mark), and switching views morphs the same marks
rather than cutting to a new screen. See [DESIGN.md](DESIGN.md) for the idea and
[docs/gpui-spike.md](docs/gpui-spike.md) for why gpui.

The data is synthetic for now, generated around today's real date. Sync and editing are out
of scope.

## Running

```sh
devenv shell            # or `direnv allow` once
cargo run --release
```

| Keys | |
|---|---|
| `1`–`5`, `Tab` | Week · Lanes · Energy · Dial · Quarter |
| Ctrl+scroll, `−` `=`, right-edge slider | Zoom: Day · Week · Month · Quarter · Year (continuous, snaps to stops) |
| `←` `→` | Previous / next at the current zoom (day, week, month…) |
| Scroll, Shift+scroll | Scroll hours when a single day/week is in view, otherwise scroll through time |
| Alt+scroll | Zoom the hour window |
| `T` | Today |
| `F` | Fisheye around the pointer |
| `D` | Toggle light / dark (follows the system by default) |
| `C` / `B` / `V` | Cycle colour measure · toggle split by calendar · cycle composition |
| `G` | Day-span glyph in Quarter cells |
| `Z` | Show / hide extra time zones |
| `?` | Help · `` ` `` frame stats |

### Time zones

Extra zones are a global setting: put `zones = America/New_York, Europe/London` in
`~/.config/calm/config`, or pass `--zones`. The primary zone is the system's. Week gets a column of
hour labels per zone, and Lanes and Energy get a row per zone; a trailing `+` or `−` means the next or
previous day. Each view's grammar can narrow labelling to the primary zone (`zones: primary`, used
by Dial). The details drawer shows an event's time in every zone.

Zones can be given a short label for the axes and a long name, shown with the current time and UTC
offset when you hover a zone label:

```
zones = Asia/Manila, Europe/London
short.Asia/Manila = 🇵🇭
long.Asia/Manila = Manila
```

Whichever configured zone you are currently in is dropped, so the same config works wherever you are.

Click an event to see its details. Click an overview cell (a month or year box) to zoom into it. The calendar
chips in the top bar show and hide calendars, and the remaining lanes widen to fill.

### Developer flags

| Flag | |
|---|---|
| `--lens N`, `--zoom STOP`, `--theme dark\|light`, `--offset DAYS`, `--zones A,B` | Start state |
| `--translucent`, `--opacity X`, `--blur` | Translucent window: 0.8 background opacity by default, optional compositor blur |
| `--still` | Springs settle in one frame (for screenshots) |
| `--hover x,y`, `--select x,y` | Scripted pointer, applied once the window size settles |
| `--cycle` | Advance lenses every 1.1 s |
| `--bench` | 2× data (`CALM_MULT` to change), scripted lens + zoom tour for 22 s, print frame/CPU percentiles. `CALM_SKIP=decor,marks,labels,chrome` disables parts for bisection |

## Layout

- `src/spec.rs`: the grammar and the five presets.
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
