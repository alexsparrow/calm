# Roadmap

Rough order; each item gets its own design note in `docs/` when it starts.

1. **Storage and read-only sync** (iCal URLs and files done): SQLite offline cache, validated atomic syncs,
   iCal URLs and files first, CalDAV and Google API later. See [storage-sync.md](storage-sync.md).
2. **Nix flake packaging**, plus a home-manager module for calm's config.
3. **Keybindings and command palette**: all bindings remappable through gpui actions; opt-in vim
   layer (`hjkl`, counts, `gg`/`G`) and a `:` command line with fuzzy completion.
4. **Rules engine**: declarative rules (conditions on title, attendees, calendar, time, tags →
   set, tag or adjust derived properties) whose outputs the grammar can use as channels. Ordered,
   explainable (the drawer shows which rules fired). Small expression language before any embedded
   scripting.
5. **View specs as files** (KDL config, params, live reload done): presets library, sharing, and an editor where the plain-language
   grammar description is directly editable.
6. **Themes**: colour tokens as data; a few built in (calm light/dark, Nord…).
7. **macOS packaging**: `.app` and DMG; verify the rotated-glyph shader path on Metal.

## Ideas

- **Overrun risk.** Rules derive `overrun_risk` (0–1) or `expected_overrun` (minutes) from
  heuristics: many attendees, known long-running series or organisers, keywords ("workshop",
  "planning", "retro"), no buffer before the next event, end-of-day slots. Later it could learn
  from "this ran over" marks or from opening the next event late. Visualised as:
  - a *ghost tail* past a block's end (a new `detail` glyph on event marks; a translucent arc on
    the Dial);
  - collision cues where a tail runs into the next event or a recovery gap, reusing the conflict
    styling;
  - an `overrun risk` colour measure per event, and "minutes at risk" per day at Quarter/Year;
  - extra strain on the Energy lens when tails push demanding work into a dip.
  Needs the rules engine and real data first.
- **Detail around now.** Focus + context in time: the current period at full detail, coarsening
  smoothly into the past and future (e.g. today as blocks, this week as lists, the month as heat)
  within one continuous view. A grammar-level arrangement, not a mode. Not yet.
- **Boilerplate notes.** Hide descriptions repeated verbatim across many unrelated events
  (Google's auto-event notice, meeting-link footers), whatever their language.
- **Floating chrome**: controls that float over the canvas and fade when idle, to give the
  calendar the full window. Parked; unsure it beats fixed controls.
- **Horizon track** for all-day and multi-day events (open question in DESIGN.md).
- **Dynamic calendars**: one per source with configurable colours, replacing the fixed four.
