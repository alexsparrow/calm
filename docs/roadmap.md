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
   "notify guests?" prompt, undo of sent changes from the
   history, then "this and following", replies to invitations, and all-day ↔ timed.
2. **Calm notifications.** Done: one heads-up per event through the desktop's notification
   daemon, timed to what it needs (video 2 min, a place 15, demanding 10, else 5) and worded as a
   transition ("Standup in 5 min · 9:30 – 9:45 am · then free until 12:30 pm"); silent for
   all-day, tentative, cancelled, free and free/busy events and `notify=#false` calendars; quiet
   hours; Do Not Disturb (`Q`, `:dnd 1h`, `calm dnd`) shown as a pill; `calm next [--json]` for
   status bars; `calm agent` while the window's closed (one notifier at a time, by a lock).
   Next:
   - **A Noctalia widget** in the repo showing `calm next`, a little stronger as it nears.
   - **`calm agent` keeps the cache synced** too, and says when changes are waiting for review.
   - **Calmer still:** a morning summary in place of a stream of pings; holding all but the
     block's end during focus; travel time from real places; notification rules once the rules
     engine exists. Declined events need your response status from the sync first.
   - Turn off Noctalia's own calendar reminders, or you'll be told twice.
3. **Travel and time zones** (`W`, the `zone` rail, or click a journey). Started: events whose start and end zones
   differ are moves between zones (Google's separate start and end zones, or `TZID`s in a
   feed). From them calm knows where you are when: `calm travel`, the `zone` rail (dashed in
   transit, labelled with local times where journeys start and end), hour columns for the zones you visit (`W`),
   and local times in the drawer. All of it lives in `src/travel.rs`, reading the cache's raw events, so
   nothing else in calm knows about travel. Next:
   - **Time where you'll be:** a view setting `zone "local"` lays each day out in the zone
     you're in that day, with a seam where a journey jumps the clock (hours skipped east,
     repeated west). Switching between home and local morphs the events, like changing views.
   - **Body clock:** sleep follows local nights while the energy model's circadian phase
     catches up at about a day per zone (faster west than east; Waterhouse et al., *Lancet*
     2007), so jet lag shows in the energy curve and strain. "Body clock: 4 am" in the drawer.
   - **Easier to say:** an "Arrives in…" field in the drawer (writing Google's end zone);
     perhaps airport codes ("LHR → ICN") for events that don't carry zones.
   - **Caveat seen in real data:** an outbound journey with no return keeps you abroad until the
     next move, so trips need both legs.
4. **Keys and commands, then more of calm as grammar.** Done: every key runs a command
   (`pan -1`, `cycle measure`, `toggle rail=daylight`), bound in KDL per context and per view, and
   the same commands run from `:`; `?` is built from the keymap. Also done: `zones` is a channel
   (`config`, `visible`, `primary`) that a session can set on any view, like `rail` (which gained
   `travel`), and overlays name bundles of settings: Travel (`W`) is
   `overlay "Travel" key="w" { rail "travel"; zones "visible" }`, and `Z` is
   `toggle zones=primary`. Next:
   - **More channels a session can set:** the colour measure, mark, all-day events, and filters
     once they exist; overlays show in the top bar while on.
   - **The prompt grows:** fuzzy completion and a palette over the same commands
     (`:view dial`, `:zoom year`, `:new 9-10 Focus`); counts and sequences (`3l`, `gg`); keys in
     reusable `params` blocks; a `calm do …` command line for agents.
5. **Search and filters:** filters over any event field (title, people, series, location,
   status, time, rule tags), declared in KDL like views, with params the UI turns into controls.
   They're toggled from the top bar, with calendar visibility as one built-in filter, and views
   can declare their own. The drawer gets "only this series" and "only with this person". A
   filter either hides non-matching events or dims them. Events already carry their people, and
   search's query language (`with:` `in:` `at:` `series:` `is:`) is the start of the filter one.
   Search (`/`) and date jumps (`:`) are done (see the README); saving a search as a filter
   comes with this.
6. **Rules engine:** declarative rules (conditions on title, attendees, calendar, time, tags →
   set, tag or adjust derived properties) whose outputs the grammar can use as channels. Ordered,
   explainable (the drawer shows which rules fired). Small expression language before any embedded
   scripting.
7. **CalDAV** (iCloud, Fastmail) behind the same outbox.
8. **View presets:** a library of views to share, and an editor where the plain-language
   grammar description is directly editable.
9. **macOS packaging:** `.app` and DMG; verify the rotated-glyph shader path on Metal. A
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
