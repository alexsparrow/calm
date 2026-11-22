# Storage and read-only sync

Goal: calm shows your real calendars, opens instantly from an offline cache, and never loses or
garbles data because a feed misbehaved. The sync side is read-only; editing comes later.

## Principles

1. **The cache is the source of truth for the UI.** calm renders from local storage at startup,
   with no network wait. Sync runs in the background and swaps in new data when it succeeds.
2. **A bad fetch never destroys good data.** Every source keeps its last good snapshot. Fetches
   are parsed and validated in full before anything is replaced, and replacement is one
   transaction.
3. **Be loud about staleness, quiet otherwise.** A small per-source indicator ("synced 4 min
   ago", "offline: showing cache from 09:12", "feed error") rather than dialogs.
4. **Sources are adapters.** iCal URLs and local `.ics` files first; CalDAV and the Google
   Calendar API slot in later behind the same interface.
5. **Secrets never reach logs, the repo, or the UI.** Feed URLs with tokens live in files with
   `0600` permissions (later the system keyring); calm warns if they are readable by others.

## Configuration

`~/.config/calm/sources`, TOML:

```toml
[[source]]
name = "Personal"
url_file = "~/.config/calm/secrets/google-personal.url"   # or url = "https://…", or path = "…/x.ics"
calendar = "Personal"      # which calendar (colour, lane) its events join
refresh = "15m"
```

`url_file` keeps the secret out of the main config, which is then safe to commit to dotfiles.
`calm --demo` (or no sources configured) keeps today's synthetic data.

## Storage

One SQLite database at `$XDG_STATE_HOME/calm/calendar.sqlite3`
(`~/.local/state/calm/…`), in WAL mode. It is a cache, but rebuilding it costs a full re-fetch of
every source, so it is state rather than `~/.cache`. Bundled SQLite: no system dependency.

| Table | Holds |
|---|---|
| `meta` | Schema version (migrations are numbered SQL applied in order). |
| `sources` | One row per configured source: config hash, `etag`, `last_modified`, `content_hash`, `last_ok_at`, `last_attempt_at`, `last_error`, `event_count`, `failures`, `next_attempt_at`. |
| `sync_runs` | Append-only log: source, start/finish, outcome (`ok`, `unchanged`, `error`, `rejected`), HTTP status, bytes, event count, message. Trimmed to the last ~500 runs. |
| `components` | Each `VEVENT` as received: `(source_id, uid, recurrence_id)` key, `sequence`, the raw text, and normalised fields (summary, start/end as UTC instants plus original `TZID`, all-day flag, `RRULE`, `EXDATE`s, status, location, description, organizer, attendees, categories, transparency). |
| `occurrences` | Recurrences expanded over a rolling window (one year back, one forward): `(source_id, uid, start_utc)`, end, all-day flag, and which component produced it. Indexed by start. This is what views query. |
| `derived` | Properties computed by the rules engine per occurrence. Empty until that exists; kept separate so rules can be re-run without re-syncing. |

Raw `VEVENT` text is kept so parsing bugs can be fixed and re-applied without a re-fetch.

## Sync algorithm (per source)

1. **Schedule.** After a success, the next attempt is `refresh` later. After a failure, back off
   exponentially (1, 2, 4… minutes, capped at an hour, with jitter). A manual refresh (`r`)
   ignores the schedule.
2. **Fetch conditionally.** Send `If-None-Match` / `If-Modified-Since`. A `304`, or a body whose
   hash matches `content_hash`, is recorded as `unchanged` and touches nothing else.
3. **Parse fully.** Unfold lines, parse every component, resolve `TZID`s (IANA names directly,
   Windows names via a mapping table, unknown ones from the feed's `VTIMEZONE` offsets).
   Individual bad events are skipped and counted; the feed is rejected if more than 5% fail, or
   if it has no `VCALENDAR` at all (HTML error pages and captive portals arrive with `200 OK`).
4. **Sanity-check against the last good snapshot.** A feed that suddenly loses more than half its
   events (from at least 20) is held as *suspect*: logged as `rejected` and not applied. If the
   next fetch returns the same content, it is accepted, since a calendar can genuinely shrink.
   This two-strike rule is the main defence against flaky feeds wiping a calendar.
5. **Expand recurrences.** For each `RRULE` master, expand over the window, remove `EXDATE`s,
   replace instances that have a `RECURRENCE-ID` override (matching on `UID` plus original
   start), and drop `STATUS:CANCELLED` instances.
6. **Commit atomically.** In one transaction, replace that source's `components` and
   `occurrences` and update its `sources` row. Readers (the UI) see either all of the old data or
   all of the new.
7. **Notify.** The UI reloads that source's occurrences and morphs to the new state.

Sources sync independently and concurrently (on background threads), so one hanging feed
delays nothing else; every request has a connect and a total timeout.

## Into the app's model

An occurrence becomes an `Event` in the primary time zone: its start and end instants are
converted to local dates and minutes. For now:

- **Multi-day timed events** are split into per-day segments at local midnight.
- **All-day events** are kept but not drawn on the time grid; they need the horizon track that
  DESIGN.md leaves open. Day headers will show a count until then.
- **Demand** defaults by calendar (as the synthetic data does) until the rules engine can set it.
- **Calendars** are the four fixed ones for now; a source names which it joins. Dynamic
  calendars (one per source, with configurable colours) come next.

## Known limits of iCal feeds

Google's "secret address in iCal format" is regenerated on Google's schedule, which can lag
changes by hours. calm can only be as fresh as the feed. For reliable, timely sync the next
adapters are CalDAV (Google, iCloud, Fastmail) and the Google Calendar API, both needing OAuth.
The storage and validation above are shared by every adapter.

## Testing

- Fixture `.ics` files covering: recurrence with `EXDATE` and overrides, cancelled instances,
  `TZID` and floating times, all-day and multi-day events, DST transitions, a truncated feed, an
  HTML error page, and a feed that suddenly halves.
- Unit tests for parsing, expansion and validation; an end-to-end test that syncs a `path`
  source into a temporary database and reads back occurrences.
