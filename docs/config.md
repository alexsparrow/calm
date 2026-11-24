# Configuring calm

calm is configured in KDL. Files load in this order, later ones winning by view name:

1. the built-in views ([`assets/views.kdl`](../assets/views.kdl));
2. `~/.config/calm/calm.kdl`, yours, plus anything it `include`s;
3. `~/.config/calm/views.generated.kdl`, written by the in-app editor (`E`). It's ordinary KDL you
   can edit or move into calm.kdl, but calm rewrites it when you use the editor.

Edits apply live while calm runs. A config with errors is not applied: the last good one stays, and
the first error shows in the top bar.

```sh
calm config check          # validate; exit 1 with file:line:col diagnostics on errors
calm config check --json   # the same, as [{"file","line","column","message"}]
calm config dump           # the effective config: built-ins merged with yours
calm config path           # where calm.kdl lives
calm config set VIEW CHANNEL[.PROP] VALUE   # e.g. set Grid mark pill, set Grid facet.per-line 14
calm config duplicate VIEW                  # copy as "VIEW copy"
calm config reset VIEW                      # drop the editor's changes (or the editor's own view)
```

`set`, `duplicate` and `reset` are exactly what the editor does: they write views.generated.kdl,
check the whole config, and undo the write if it would not load.

`dump` is the best starting point: copy a built-in view, rename it, change it. Both KDL v2
(`#true`) and v1 (`true`) syntax are accepted. Unknown nodes and properties are errors, never
ignored.

## Top level

| Node | Meaning |
|---|---|
| `view "Name" { … }` | Define a view. Same name as an existing one replaces it; a new name adds one. |
| `params "name" { param … }` | A reusable set of params, pulled into views with `use "name"`. |
| `include "path"` | Read another file, relative to this one. `*` is allowed in the last component: `include "views/*.kdl"`. |
| `hide "Name" "Other"` | Remove views (built-in or included). |
| `defaults "View" param=value …` | Set a view's param defaults without copying the view, e.g. `defaults "Grid" measure="demand" span=#true`. Checked against that view's params. |
| `keys { bind "t" "today" … }`, `keys "create" { … }` | Keys, and what they do. See [Keys](#keys). |
| `overlay "Name" key="w" { rail "travel"; zones "visible" }` | Settings that go on together over whichever view you're in, and off again. See [Overlays](#overlays). |
| `order "Name" "Other" …` | Order the lens switcher; unlisted views follow. Keys `1`–`9` follow this order. |
| `zones "Zone" … { label "Zone" short="…" long="…" }` | Extra time zones (IANA names) shown beside the current one; whichever you're in is dropped. `short` labels the axes (a flag emoji works), `long` names it on hover. |
| `calendar "Name" color="sage" demand=0.4 label="…" secondary=#true` | A calendar: one chip in the top bar, a colour, and the default demand (0 light – 1 demanding) of its events for Energy and strain. `label` is what the chip says, handy for Google calendars named by an address (`calendar "you@work.com" label="Work"`); chips shorten long names, and show only their dots when even short names don't fit. `notify=#false` keeps its events from reminding you. `secondary=#true` lists the calendar in the ☰ menu (top right) instead of the chip row; calendars that don't fit even as dots go there too. `color` is a palette name (sage, clay, slate, ochre, plum, teal, rose, moss, sky, lavender, sand, stone) or `#rrggbb` (its hue is kept, saturation softened to suit the theme). Up to 12; chips follow this order. Without any `calendar` nodes, each source gets a calendar named by its `calendar=` (or its own name), coloured from the palette in order. |
| `source "Name" url-file="…" calendar="…" refresh="15m"` | A calendar feed. Exactly one of `url-file=` (a file holding a secret address; chmod 600), `url=`, `path=` (a local .ics), or `google="you@gmail.com"` (a Google account through the API: all its calendars, free/busy included, synced incrementally every 5 minutes; no `calendar=`; see [google.md](google.md)). `calendar` names the calendar its events belong to (several sources can share one); once you declare calendars it must be one of them. `refresh`: `90s`, `15m`, `1h` (at least a minute). |
| `energy …` | Your energy through the day, drawn in Energy and used for strain. See [Energy](#energy). |
| `location lat=14.6 lon=121.0` | Where you are, for the daylight rail (degrees, north and east positive). Without it, calm uses the coordinates the time-zone database gives for your zone's city. |
| `sync writes="auto"` | Whether your edits send by themselves (`auto`: after a 2-second pause) or wait in the changes panel (`O`) until you commit them (`review`). A source's own `writes=` overrides it. Changes from the command line (agents, scripts) always wait. See [editing.md](editing.md). |
| `notify before="5m" place="15m" video="2m" demanding="10m" quiet="22:00-07:00"` | Reminders: how long before an ordinary event, one with a place (to get there), a video call (found by its link) and a demanding one (demand 0.7 or more), and quiet hours when nothing's said. These are the defaults, except `quiet` (none). All-day, tentative, cancelled and free events, and free/busy calendars, are never announced; add `notify=#false` to a `calendar` to silence it too. Reminders go through your desktop's notification daemon (`notify-send`, or D-Bus), so its Do Not Disturb applies as well as calm's (`Q`, `calm dnd`). The app sends them while it's open; `calm agent` (as a user service, say) sends them while it isn't, and only one of them ever does. |
| `default-calendar "Name"` | Where new events go (`S`). Default: your Google account's primary calendar. |
| `window opacity=0.8 blur=#false` | Translucent background (0.2–1) and compositor blur. Read at start-up. |
| `theme "system"` | `system` (follow the desktop), `light`, `dark`, or one of your own themes. `theme light="Paper" dark="Night"` picks which themes `system` (and `D`) switch between. |
| `colors "Name" base="light" { token "#rrggbb" … }` | Theme colours. `colors "light"` / `colors "dark"` tweak the built-ins; any other name declares your own theme, starting from `base`. Tokens: `bg`, `chrome` (the top and bottom bars: unset, they match the background), `surface` (panels, chips), `border`, `ink`, `muted`, `faint`, `hairline`, `now`, `weekend`, `cell`, `energy`, `shadow`. `#rrggbbaa` adds alpha. Calendar colours come from `calendar`. |

`zones` and `source` replace the older `~/.config/calm/config` and `sources` files, which are
still read when calm.kdl doesn't declare them. Command-line flags override all of these.

## A view

```kdl
view "Grid" {
    facet per-line=7 dir="rows" band=13
    coord "cartesian"
    x "lane"
    y "time-of-day"
    collide "split"
    mark "block" {
        lod below=240 mark="list"
        lod below=60 mark="heat"
    }
    stops {
        stop "Day" cells=1 align="cell"
        stop "Week" cells=7 align="line"
        stop "Month" cells=35 align="month"
    }
    hours from="7:00" to="21:30"
    zones "all"
    use "colour"
    color measure="$measure" split="$split" compose="$compose" palette="$split"
    detail day-span="$span"
}
```

Required: `facet`, `mark`, and `stops` with at least one `stop`.

| Node | Values | Meaning |
|---|---|---|
| `split "View" size=0.4 scroll="together"` | `scroll`: `together` (default), `own` | Another view beside this one, on the right, taking `size` of the width (0.15–0.7). This view animates as usual; the other slides in. `together`: it starts at the first day this view shows and they scroll as one (scroll either; this view settles on a day when you stop). `own`: it starts at today and scrolls by itself. For now it must be a packed list (`facet … pack=#true`, like Upcoming); it can be hidden from the switcher (`hide`) and still used here. The built-in Today is a dial of your day, its radius your energy, split with Upcoming. |
| `facet per-line=N dir=… band=N pack=#false` | `dir`: `rows`, `columns` | How day cells tile the screen. Days wrap `per-line` to a line (7 = weeks, 1 = one day per line); lines stack in `dir`; every `band` lines start a new band beside (rows) or below (columns) the last. `pack=#true` (with `per-line=1 dir="rows"`) makes a list: each day as tall as its events, empty days left out (today stays, saying so if it's empty), headed "Today", "Tomorrow" and then the date, with `list` rows saying when, what and where. The built-in Upcoming view is this, with `x "none" y "none" mark "list"`: an agenda, as in Google Calendar's Schedule or Outlook's list. It's hidden from the switcher, as Today's side; `calm.kdl` can show it again by defining a view of its own. |
| `coord "…"` | `cartesian` (default), `polar` | Coordinates inside each cell. Polar draws dials. |
| `x "…"`, `y "…"` | `time-of-day`, `lane`, `calendar`, `demand`, `none` (default) | Axes inside a cell. `lane` is sub-columns for overlapping events. `demand` puts events at their demand over your energy curve: higher is more in a straight view, and further out is more on a dial, where your energy fills from the middle. |
| `collide "…"` | `split`, `highlight-conflict`, `overlay` (default) | Overlaps on the time axis: side by side, side by side and outlined, or drawn over each other. |
| `mark "…" [lod-extent=…] { lod … }` | `block`, `pill`, `list`, `heat` | What each event is drawn as (in polar coordinates blocks and pills bend into arcs; `arc` is accepted as another name for `block`), and below which cell size (`lod below=PIXELS mark=…`, in order) to draw something simpler. `lod-extent` picks what is measured: `time` (default; the cell along its time axis, so turning a view keeps its thresholds meaningful), `height`, `width`, or `min` (the smaller side). |
| `stops { stop "Name" cells=N align=… }` | `align`: `cell`, `line` (default), `month`, `group` | Zoom snap points, smallest first. `cells` is how many days are visible. `month` snaps to whole weeks spanning a month; `group` to multiples of `cells` from the data's start (quarters, years). |
| `hours from="H:MM" to="H:MM"` | `0:00`–`24:00` | The time window the view opens with (`F` toggles the full day). Default: the whole day. |
| `zones "…"` | `config` (default; `all` is the old name), `visible`, `primary` | Which zones label the time axis: the configured ones, those and the zones your journeys in view go to and from, or just yours. `Z` hides the extras (`toggle zones=primary`), and the Travel overlay (`W`) sets `visible`. |
| `use "params-name" { default "param" value }` | | Include a `params` set, optionally overriding defaults. |
| `param …` | | A param just for this view (see below). |
| `color measure=… split=… compose=… palette=…` | literal or `"$param"` | What colours marks. See below. |
| `detail day-span=…` | `#true`, `#false` or `"$param"` | The first-start → last-end bar inside aggregate cells. |
| `all-day "shown"` | `shown` (default), `hidden` | All-day events, drawn to suit the mark: bars in a band above the time axis (multi-day events as one bar, stacked so each keeps its row), rings inside a dial's centre, leading rows in lists, and stripes along the top of aggregate cells. They don't count towards energy, strain or the hours-based colour measures. |
| `rail show=… style=…` | `show`: `off` (default), `energy`, `daylight`, `zone`, `travel`; `style`: `rail` (default) or `strip` | A series beside each day's time axis. `energy` uses the Demand colour scale, so an event coloured by Demand matches the rail when its demand matches your energy, and strained events get a dot on it. `daylight` is the sun's elevation where you are (night → dawn → full day). `zone` is how far your clock is from home's while you travel: quiet at home, warm east, cool west, with each journey labelled where it starts and ends. A move between zones is an event whose start and end are in different zones: in Google Calendar, give the event separate start and end time zones; a feed says the same with `DTSTART;TZID=…` and `DTEND;TZID=…`. Clicking a journey shows it in local times ("16:35 BST → 13:20 +1 KST · +8 h"); `calm travel` lists every move found. Journeys (flights, trains, drives) draw as dashes, labelled with their local times where they start ("16:35 BST → KST +8h") and end ("13:20 KST"). `travel` is just the journeys, the rest of the rail clear; the Travel overlay (`W`) shows it in every timed view, with `zones "visible"`. `rail` widens with the value; `strip` is colour only. Needs a cartesian time axis. Grid shows energy by default; change it in the editor (`E`). |

### Colour

- `measure`: `calendar` (each event its calendar's colour), or one of `duration` (event length),
  `hours`, `events`, `demand`, `strain`, `free`, `starts`, `ends`, `span`. For events (`block`,
  `pill`, `list`) only `calendar`, `duration`, `demand` and `strain` mean anything; the rest
  reduce over a day for `heat`.
- `split`: `true` to break the measure down by calendar in aggregate cells (`heat`).
- `compose`: how split parts combine in a cell: `blend`, `stripes`, `stack`, `dominant`.
- `palette`: where hue comes from. `scale` (default) uses the measure's own ramp; `calendar` uses
  each event's calendar colour, with the measure setting how strong it is (pale is easy or short,
  deep is strained or long). The edge bar keeps full calendar colour. A toggle param's `true`
  means `calendar`. The built-in views tie `split` and `palette` to the one "By calendar" toggle.

## Energy

```kdl
energy wake="7:00" sleep="23:00"                 // the default model, for your hours
energy model="points" {                          // or your own curve (levels 0–1)
    at "7:00" 0.3
    at "10:00" 0.9
    at "14:00" 0.5
    at "17:00" 0.7
    at "22:00" 0.2
}
energy reactive=#true drain=0.3 recovery="60m"   // how the day's demands draw it down
```

- **Default: the three-process model of alertness** (Åkerstedt & Folkard). Alertness is a
  homeostatic process that falls exponentially while awake and recovers asleep, plus a 24-hour
  circadian rhythm. Rates are from Åkerstedt & Folkard (1995): −3.5 %/h awake, +28.7 %/h asleep.
  The asymptotes (2.4, 14.3), circadian amplitude (2.5) and peak (16:48) are the commonly quoted
  values for the model. On the model's 1–16 scale, 7 marks critically low alertness, so energy
  maps 7 → 0 and 14 → 1. You're at zero while asleep, and waking eases in over about an hour.
  `wake` and `sleep` set your hours. `peak` shifts the circadian phase; the authors note evening
  types may need it later by up to four hours.
- **Layering**: each `energy` line builds on earlier ones, so a later `energy reactive=#false` (the editor writes these to views.generated.kdl) keeps your hours or your own curve.
- **`model="points"`**: your own curve, a smooth loop through `at "H:MM" LEVEL` points.
- **`reactive`** (default on): time spent on events drains energy at `drain` per hour at full
  demand (scaled by the event's demand, from its calendar), and it recovers with time constant
  `recovery` when you're free. A long demanding stretch sags the curve and later meetings show as
  strained; a free hour lets it climb back. Each day starts fresh.

## Params

A param is a named value the view exposes in the UI. Channels read it as `"$name"`.

```kdl
param "measure" label="Colour" bind="segmented" key="c" row=1 default="calendar" {
    option "Calendar" value="calendar" when-mark-not="heat"
    option "Hours" value="hours" when-mark="heat"
}
param "split" label="By calendar" bind="toggle" key="b" default=#true when-mark="heat"
```

| Property | Meaning |
|---|---|
| `label` | Shown before a segmented control, or on a toggle. |
| `bind` | `segmented` (choose one option) or `toggle` (on/off). Default: segmented if it has options. |
| `key` | A key that cycles the options, or flips the toggle: shorthand for `bind "c" "cycle measure"` in the view's `keys`. Only taken while the param is offered. |
| `row` | Which control row, counting up from the lens switcher (1 is nearest). |
| `default` | An option `value`, or `#true`/`#false` for toggles. |
| `when-mark="…"`, `when-mark-not="…"`, `when-param="name=value"`, `when-param-not="name=value"` | Only offer the param (or, on an `option`, the option) when these hold. Repeatable; all must hold. |

`option "Label" value="…"` defaults `value` to the lowercased label.

Rules worth knowing:
- A toggle that isn't offered right now counts as off. A segmented param that isn't offered uses
  its default.
- A choice that isn't available right now (after zooming to `heat`, say) falls back to the first
  option that is. The stored choice survives, so zooming back restores it.
- Where controls appear is up to calm: one bar per `row`, legends on the top-most row.

## Keys

Every key does a **command**, written as text. The built-in keys are
[`assets/keys.kdl`](../assets/keys.kdl); yours go on top, chord by chord, and `calm config dump`
shows the result. The same commands run from the `:` prompt (`:set rail=daylight`), which
still takes dates first (`:31` goes to the 31st) and completes commands, views, overlays and
settings as you type (↑ ↓ to choose, Tab to take one). `?` lists whatever the keys are now.

```kdl
keys {
    bind "t" "today"                  // replaces whatever T did
    bind "ctrl-d" "pan 1"
    bind "g" "toggle rail=daylight"   // a key for a look
    unbind "d"                        // nothing on D
}
keys "create" {                       // only while scheduling (S)
    bind "+" "snap finer"
}
view "Dial" {
    // …
    keys {                            // only in this view, before the global keys
        bind "j" "set rail=energy"
    }
}
```

`bind` takes one or more keys and then the command. Keys are one character (`t`, `?`, `:`) or a
name (`left`, `right`, `up`, `down`, `tab`, `enter`, `escape`, `space`, `backspace`, `delete`,
`home`, `end`, `pageup`, `pagedown`, `f1`–`f12`), after modifiers: `ctrl-`, `alt-`, `shift-`,
`cmd-`. A capital letter is Shift with it (`N` is `shift-n`). For a symbol, write what Shift
types (`?`, not `shift-/`).

**Contexts.** `keys` with no name are the normal keys. `keys "create"` and `keys "adjust"` apply
in those modes and fall back to the view's keys, then the normal ones. `keys "search"` applies
in the search field, where any other key is typing.

**A view's keys** come before the global ones, so a view can take a key over (Dial could make
`j` and `k` turn the dial). `calm config check` mentions each key a view takes over, as a
warning, in case it's a slip. A view's key whose param isn't offered right now (see `when-` on
params) lets the key through to the global one.

**Commands**

| Command | Does |
|---|---|
| `pan -1`, `pan 1` | Back / forward a page at the current zoom |
| `scroll -1`, `scroll 1` | Scroll the hours, in views with hours; else pan |
| `zoom in`, `zoom out` | Towards a day / a year |
| `today`, `full-day` | Go to today · full day / default hours |
| `view 3`, `view lanes`, `view next`, `view prev` | Switch view: by position (as the switcher orders them) or by name |
| `search`, `prompt` | Open `/` search · the `:` prompt |
| `pick` | Label the events in view with letters; type one's letters to select it (Backspace takes one back, Esc gives up) |
| `match next`, `match prev` | Step through search matches (selecting them; in the field, just moving) |
| `complete` | Complete the search field's facet |
| `create`, `adjust` | Schedule and Adjust modes |
| `delete`, `changes`, `sync` | Delete the selected event · changes waiting to sync · sync now |
| `editor`, `menu`, `theme`, `help`, `stats` | Open or flip these |
| `close` | Close the innermost open thing, else the search, else the selection |
| `step left` (`right`, `up`, `down`) | In a mode: move the cursor, or the selected event |
| `stretch left` (…) | Adjust: resize the selected event |
| `place`, `done`, `back` | Schedule: set the start, then the end · leave the mode · back a step |
| `snap finer`, `snap coarser`, `free-only` | In a mode: the snap step · free time only |
| `cycle NAME`, `toggle NAME` | A param of the current view: step through its options, or flip it |
| `set NAME=VALUE …` | Set the current view's params, or the session's `rail` and `zones` (all of them, or none if one doesn't fit) |
| `toggle NAME=VALUE …` | Set them; again, put back what they were |
| `overlay NAME` | Turn an overlay on or off |

`bind` takes `help="…"` for what `?` says, when the command's own description won't do:
`bind "z" "toggle zones=primary" help="Extra time zones"`.

**Params and channels.** `set` and `toggle` change a view's params by name, which last per view.
`rail` and `zones` are also channels you can set on any view, whatever it binds them to: they last
for the session, across views, until toggled back (`set` them to the view's value to undo a `set`).

## Overlays

An overlay is a named bundle of settings, turned on and off over whichever view you're in:

```kdl
overlay "Travel" key="w" {
    rail "travel"
    zones "visible"
}
```

Inside, each line sets a channel (`rail`, `zones`) or a param (checked against the view when you
use it; a view without that param says so). `key=` binds it; `overlay "Travel"` from `:` or a
`bind` does the same. Turning it on remembers what it changed; turning it off puts that back.
Travel (`W`) is built in this way, in [`assets/keys.kdl`](../assets/keys.kdl): replace it by
declaring your own `overlay "Travel"`.

## Example: a strain-first week

```kdl
// ~/.config/calm/calm.kdl
hide "Dial"
order "Grid" "Strain" "Lanes" "Energy" "Density"

view "Strain" {
    facet per-line=7 dir="rows" band=13
    x "lane"
    y "time-of-day"
    collide "split"
    mark "block" {
        lod below=240 mark="list"
        lod below=60 mark="heat"
    }
    stops {
        stop "Week" cells=7 align="line"
        stop "Month" cells=35 align="month"
    }
    hours from="8:00" to="20:00"
    use "colour" {
        default "measure" "strain"
    }
    color measure="$measure" split="$split" compose="$compose" palette="$split"
    detail day-span="$span"
}
```

After saving, `calm config check` should print `ok: 5 views (Grid, Strain, Lanes, Energy, Density)`.
