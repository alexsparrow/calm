# Roadmap

Rough order; each item gets its own design note in `docs/` when it starts.

Done:

- **Storage and sync:** an SQLite offline cache, validated atomic syncs, iCal URLs and files,
  and Google Calendar through its API. See [storage-sync.md](storage-sync.md) and
  [google.md](google.md).
- **Editing:** the outbox, conditional writes with merging, review mode, the changes panel, the
  CLI, and the New and Adjust modes. See [editing.md](editing.md).
- **Views as files:** KDL config with params and live reload, and the in-app view editor.
- **Themes:** colour tokens as data, light and dark built in, your own in calm.kdl.
- **Calendars from your accounts**, with labels, colours and secondary calendars.
- **Nix flake packaging.**
- **Search and date jumps:** `/` with facets, a match list and timeline, `n`/`N`; `:` dates.

Next, roughly in order:

1. **Editing, the rest:** the "this event / all events" popover for recurring events, the
   "notify guests?" prompt, picking events from the keyboard, undo of sent changes from the
   history, then "this and following", replies to invitations, and all-day ↔ timed.
2. **Calm notifications:** being told about events without being startled by them. calm
   decides when and what to say, and the desktop's notification daemon (Noctalia here)
   delivers it and applies its Do Not Disturb.
   - **Ambient first:** `calm next --json` (the next event, minutes to go, what follows, whether
     you're in a focus block) for status bars. A Noctalia scripted (Lua) widget in the repo shows
     "Standup · 12m", getting a little stronger as it nears. The top bar's clock shows the same.
   - **`calm agent`:** a user service that keeps the cache synced while the window is closed and
     sends reminders. Later it can also say when changes are waiting for review.
   - **One heads-up per event,** timed to what it needs: travel time for a place, about 2 min for
     a video call, earlier for a demanding meeting in an energy dip. Worded as a transition
     ("Wrap up — Standup in 5 min, then free until 2"). No repeats, no badges.
   - **Silence by default** for all-day, tentative and declined events, free/busy calendars and
     your own focus blocks; quiet hours and per-calendar settings in calm.kdl.
   - **Later:** a morning summary in place of a stream of pings; holding everything but the
     block's end during focus; notification rules once the rules engine exists.
   - Turn off Noctalia's own calendar reminders, or you'll be told twice.
3. **Keybindings and command palette:** all bindings remappable through gpui actions; a vim
   layer (`hjkl` is already in, then counts and `gg`/`G`); the `:` line (date jumps, below) grows
   into a command line with fuzzy completion (`:view dial`, `:zoom year`, `:new 9-10 Focus`).
4. **Search and filters:** filters over any event field (title, people, series, location,
   status, time, rule tags), declared in KDL like views, with params the UI turns into controls.
   They're toggled from the top bar, with calendar visibility as one built-in filter, and views
   can declare their own. The drawer gets "only this series" and "only with this person". A
   filter either hides non-matching events or dims them. Events already carry their people, and
   search's query language (`with:` `in:` `at:` `series:` `is:`) is the start of the filter one.
   Search (`/`) and date jumps (`:`) are done (see the README); saving a search as a filter
   comes with this.
5. **Rules engine:** declarative rules (conditions on title, attendees, calendar, time, tags →
   set, tag or adjust derived properties) whose outputs the grammar can use as channels. Ordered,
   explainable (the drawer shows which rules fired). Small expression language before any embedded
   scripting.
6. **CalDAV** (iCloud, Fastmail) behind the same outbox.
7. **View presets:** a library of views to share, and an editor where the plain-language
   grammar description is directly editable.
8. **macOS packaging:** `.app` and DMG; verify the rotated-glyph shader path on Metal. A
   home-manager module for calm's config.

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
