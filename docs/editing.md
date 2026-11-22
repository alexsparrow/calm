# Editing and write sync

Goal: create, move, resize, edit and delete events in calm. Every edit feels instant and works
offline. No edit is ever silently lost, and nobody else's change is ever silently overwritten.
Google Calendar is the first writable source; iCal feeds stay read-only.

This builds on [storage-sync.md](storage-sync.md). The read path doesn't change.

## Principles

1. **Two kinds of state, never mixed.**
   - **The mirror** (`components`, `occurrences`) holds only what the server has told us.
   - **The outbox** holds what you asked for that the server hasn't confirmed yet.
   - What you see is *mirror ⊕ outbox*, computed when the model is built.
   - The read sync can therefore replace the mirror freely without trampling pending edits, and
     a pending edit can be replayed against fresh server state.
2. **Intent is durable before it's visible.** An edit is written to the outbox, in a SQLite
   transaction, before the UI shows it. A crash, a quit or a dead network loses nothing. Pending
   edits resume on the next launch.
3. **Optimistic, but honest.** Edits apply on screen at once. While unconfirmed they look subtly
   pending. If one fails or conflicts, it stays visibly marked with the reason and a way out
   (retry, edit or discard) until you deal with it. It is never quietly dropped.
4. **Conditional, minimal writes.**
   - Updates send only the fields you changed (`PATCH`), conditional on the version you edited
     (`If-Match: <etag>`).
   - A concurrent change elsewhere is detected, never overwritten.
   - Because writes are field-level, changes to *different* fields merge automatically.
5. **Every request is safe to repeat.** Networks lose responses. Creates carry an ID calm
   generates. Updates and deletes are conditional, so a retry of something that actually
   succeeded is recognised, not doubled.
6. **One worker per source, reads and writes in order.** Each source's sync thread first flushes
   its outbox, then does its incremental read. A read and a write never race on the same
   calendar.

## What can be edited

| Calendar | Editable |
|---|---|
| Google, access `owner` or `writer` | Yes |
| Google, `reader` or `freeBusyReader` | No: read-only, said so in the drawer |
| iCal feed or `.ics` file | No: read-only by nature |

Within a writable calendar, an event you're a guest on (not the organiser) is editable only if
Google says guests may modify it (`guestsCanModify`). Otherwise it's read-only except for your
response, which comes in the second slice (below).

## Model

### The outbox

A new table, `outbox`, in the same database:

| Column | Meaning |
|---|---|
| `seq` | Local order (autoincrement). Ops on one event are sent in this order. |
| `op_id` | Random ID, for logs and dedup. |
| `source`, `calendar` | Where the event lives (`Google`, calendar ID). |
| `event` | Google event ID. For a create, the ID calm generated. |
| `kind` | `create`, `update` or `delete`. |
| `series`, `original` | For one occurrence of a recurring event: its series, and its original start. |
| `base` | JSON of the fields you changed, *as they were* when you edited (for three-way merge). |
| `patch` | JSON of the fields you changed, as you want them. Creates hold the whole event. |
| `notify` | Whether guests are notified: `all`, `none` or `ask` (see *Guests*). |
| `state` | `draft` (in review mode, until committed), `pending`, `sending`, `failed`, `conflict` or `waiting` (on you, e.g. "notify guests?"). |
| `origin` | Where the change came from: `app` or `cli` (an agent or script), shown in the changes panel. |
| `attempts`, `next_attempt_at`, `last_error` | Retry bookkeeping, as for reads. |
| `theirs` | For a conflict: the server's values of the clashing fields. |
| `created_at` | When you made the edit. |

### Coalescing

Edits to one event that is still unsent merge into one op:
- Two moves become one update with the later times.
- Create followed by an edit becomes a create with the edited fields.
- Create followed by delete becomes nothing at all.

Once an op is `sending`, later edits queue behind it as new ops, and take the new `etag` its
response brings.

### The overlay

When building the dataset, pending ops are applied over the mirror in `seq` order:
- Creates add events.
- Updates replace fields.
- Deletes hide events.

Each affected event carries a `sync` state (`pending`, `failed` or `conflict`) for drawing. The
overlay is cheap: a handful of ops against thousands of events.

## The write path

```
you edit ─▶ validate ─▶ outbox (transaction) ─▶ UI shows it (overlay)
                                │
               source's sync worker, next tick or immediately
                                ▼
            send op ─▶ outcome ─▶ mirror updated + op removed   (one transaction)
                              └─▶ retry later | conflict | failed (op kept, UI marks it)
```

### Sending

| Op | Request |
|---|---|
| create | `POST /calendars/{cal}/events` with the generated `id` and the full event |
| update | `GET` the event, merge (below), then `PATCH` only what's left, with `If-Match:` the version just read |
| delete | `GET` the event (a change since you saw it is a conflict), then `DELETE` with `If-Match:` |

Fetching just before writing means calm never has to keep a version per event, or per instance
of a series. A conflict is caught however old the change is, and a lost response resolves
itself (the next attempt finds the server already as you wanted). It costs one extra request
per change, which is nothing at a person's volume. A `412` between the read and the write
just means another round, up to three.

All carry `sendUpdates` from the op's `notify`.

### Outcomes

| Response | Meaning | What calm does |
|---|---|---|
| `2xx` | Done | Upsert the returned event into the mirror and re-expand that calendar, then remove the op. Both happen in **one transaction**, so the UI never shows the edit twice or not at all. |
| `409` on create | That ID exists: an earlier attempt succeeded and its response was lost | `GET` the event; if it matches what we sent, treat it as `2xx`. |
| `412` | It changed between our read and our write | Read and merge again (up to three rounds). |
| `404` / `410` on update | Deleted elsewhere | Conflict: "deleted elsewhere" (recreate it, or discard your change). |
| `404` / `410` on delete | Already gone | Success. |
| `401` | Access token expired | Refresh it and retry immediately. |
| `403` (scope, or permission) | Can't write | `failed`: "sign in again to allow editing", or "this calendar is read-only". The op is kept. |
| `400` | Google rejected the content | `failed`, with Google's message. You can edit or discard it. |
| `429`, `5xx`, network | Temporary | Retry with backoff (1, 2, 4… minutes, capped at an hour). The op stays `pending`, and the status line says "N changes waiting to sync". |

### Three-way merge

The *base* is the changed fields as they were when you edited; *ours* is the op's patch; *theirs*
is the server's current version.

- **Fields only you changed** (theirs still equals base): re-send the patch with the new `etag`.
- **Fields they changed and you didn't:** theirs stands. Your patch doesn't touch those fields.
- **Same field, same new value** (theirs equals ours, for every patched field): already done. This
  is also how a *lost response* resolves: the retry gets `412`, and the server already has our
  values.
- **Same field, different values:** `conflict`. The drawer shows both ("you: 3–4 pm · them:
  3:30–4:30 pm"), and you choose. *Keep mine* re-sends against the new `etag`. *Keep theirs*
  drops the op.

Times count as one field: start and end move together.

### Ordering with reads

Each source's worker runs: flush outbox, then incremental read.
- The read brings our own writes back. They carry the `etag` we already stored, so they change
  nothing.
- *Not yet built:* when the read shows that an event with a pending op changed on the server,
  merge at once, so a conflict appears before sending rather than at it. Today it's caught when
  the change is sent.

### Durability and restarts

- **On launch:** ops left `sending` (calm quit mid-request) go back to `pending`. Idempotency
  covers the case where the request had actually landed.
- **When ops leave the outbox:** only after success (moved into the mirror) or after you
  discard them. `failed` and `conflict` ops wait for you, indefinitely.

## Reviewing before sending

The outbox can be a staging area rather than a conveyor belt.

```kdl
sync writes="auto"      // default: changes send after a short pause
sync writes="review"    // changes wait until you commit them
source "Google" google="you@gmail.com" writes="review"   // or per source
```

The ☰ menu toggles it too, and so does `:sync review` once the command palette exists.

- **`auto`** (the default). Changes are sent after a 2-second pause, so that a burst of drags or typing
  coalesces into one request per event.
- **`review`.**
  - Changes stay staged (state `draft`) until you commit them. Reading carries on as normal, so
    you still see other people's changes as they happen.
  - Drafts are checked against each incoming change, so a conflict shows up while you're still
    reviewing, not at commit time.

### The changes panel

`O` (or the status line's "3 changes", or ☰ → Changes) opens a panel listing what's queued. Each
change is one line in plain words, with a checkbox:

```
☑  Move     Standup         Tue 9:00 → 10:00
☑  Create   Focus           Wed 9:00–11:00 · Personal
☐  Delete   Lunch with Sam  Thu 12:30            [notify 2 guests ▾]
⚠  Rename   Planning → Q1 planning   changed elsewhere: they moved it to 3 pm
                                          [Keep mine] [Keep theirs]

[Commit 2]   [Discard selected]   [Preview: on]
```

- **One line per event, whatever you did to it.** Twenty drags of the same event are one "Move".
  Lines expand to show each field: what it was, then what it will be.
- **Revert a line** to drop the change. The event snaps back to the server's version.
- **Preview** toggles between drawing your drafts and drawing the calendar as it is on the server.
  It's the quickest way to see what a batch of changes does to your week.
- **Guest notifications are decided here**, per change. That's better than the 30-second prompt
  in `auto` mode: nothing is sent until you've seen it.
- **Failed and conflicting changes** sit at the top of the panel, with their reason and choices,
  whichever mode you're in. In `auto` mode the panel is simply where those land.

### What "commit" promises

Google's API has no transactions, so a commit can't be all-or-nothing across events. calm does
the next best thing:

1. **Pre-flight check.** Before sending anything, calm fetches the current version of every
   change being committed and merges as usual. If any would conflict, it stops, shows them, and
   sends nothing.
2. **Send.** It sends in the order you made the changes. Each success leaves the panel.
3. **Partial failure.** If one fails mid-way (network, rejection), the rest still go. What failed
   stays selected in the panel with its reason, ready to retry. The status line says "2 of 3
   changes committed · 1 failed".

Each commit is written to a local `history` table (what changed, when, from what to what). That
backs a "Recent commits" list in the panel, and undo of committed changes (a new change that
reverses it, staged for review in `review` mode).

### For agents

Changes made from the command line (`origin = cli`) are **always staged**, whatever the mode:
your own edits can send automatically while an agent's wait for you. This makes calm a safe place
for an agent to propose changes:

```sh
calm event move <id> --start 10:00          # staged, not sent
calm outbox --json                           # what's staged, with diffs
calm commit [--all | <change>…]              # what you approve, sent
calm discard <change>…
```

An agent can plan your week by staging moves; you look at the preview, untick what you don't
want, and commit. The panel shows "from agent" on these changes.

How far that holds: the app commits them only when you press Commit. `calm commit` run at a
terminal asks you to confirm by typing `y`, and without a terminal (as an agent would run it) it
refuses unless given `--yes`. That stops an agent committing by accident or by habit. It is not a
security boundary: anything with your shell can also edit your files. Treat it as a guard rail.

## Recurring events

- **This event:** edits the instance. Google keeps it as an exception, with ID
  `{series}_{original start}`.
- **All events:** patches the series. A time change moves every instance by the same amount and
  keeps the rule.
- **This and following** (second slice): ends the series before this instance (`UNTIL`) and
  creates a new series from it. Two ops that must both succeed, sent as a pair.

When you drag an instance of a series, calm asks "This event / All events" in a small popover at
the drop point before queuing anything. Esc puts it back.

## Guests

Moving a meeting can email everyone on it, so this is decided deliberately, never by accident:

- **Events with no other guests:** `sendUpdates=none`, and nothing to ask.
- **Events with guests, edited in the drawer:** a "Notify guests" toggle, on by default.
- **Events with guests, dragged:**
  - In `auto` mode, the event moves at once. A small "Notify 4 guests? Send · Don't send" bar
    appears, and the op waits in state `waiting` until you answer. If you answer nothing within
    30 s, it defaults to *Send*, which is Google's own default when you move a meeting in its UI.
  - In `review` mode, you decide in the changes panel before committing.

## Interaction

Clicking and dragging never change your calendar by themselves: a stray click or drag only
selects. To change times you enter a mode, and its bar under the top bar says what the next
click does. The same works in every view, cartesian or dial; in views without a time of day
(aggregates, lists) the unit is a whole day, and events made there are all-day.

- **New** (`N`, or + at the top): click where the event starts, then where it ends. The span
  follows the pointer between the two clicks, with its date, times and length beside it.
  Clicking the same spot twice makes an hour. Clicking above a day's time axis (its all-day
  band) makes an all-day event. It goes to your `default-calendar`, else your primary, and opens
  in the drawer with its title selected.
- **Adjust** (`A`, or ↕ at the top): drag an event to move it, across days too; drag its start or
  end edge to resize it (cartesian time views). The drawer stays closed while you adjust, out of
  the way. Read-only events say why instead of moving.
- **Snap:** 5, 10, 15, 30 or 60 minutes (`[` and `]`, or the bar); 15 by default. A moved event's
  start lands on the step.
- **Free time only** (`F`, or the bar): starts must be in free time and new events stop at the
  next busy one; a move onto busy time is refused. Busy means confirmed timed events in the
  calendars you're showing, not secondary ones.
- **Keyboard:** arrows or `hjkl` move the cursor by a step along time and by a day across (a week
  vertically in day views), and `Enter` places. In Adjust, they move the selected event and
  `Shift` resizes it. `Esc` steps back: the start you placed, the drag, then the mode.
- **Undo a move:** each move shows "Moved … · Undo". In `auto` mode the change waits while the
  undo is offered (8 s) instead of the usual 2, so Undo cancels it before anything is sent: an
  update that returns an event to where it was is dropped from the outbox, not sent. A run of
  moves to the same event undoes to where it started.
- **Edit:** the drawer becomes editable. That covers title, time, all-day, calendar, location
  and description, plus delete. Changes save as you leave a field.
- **Delete:** `Delete` or `Backspace` on the selected event. It disappears with an "Undo" toast;
  there is no confirmation dialog.
- **Undo and redo:** `Ctrl+Z` and `Ctrl+Shift+Z`, from a local history of ops.
  - Undoing an unsent op removes it from the outbox, or un-coalesces it.
  - Undoing a sent op queues its inverse.

States on screen:
- **Pending:** a slightly dimmed fill and a small dot.
- **Failed:** an outline in the strain colour, with the reason and Retry / Discard in the drawer.
- **Conflict:** an amber outline, with both versions and Keep mine / Keep theirs in the drawer.
- **Status line:** "N changes waiting to sync" while any are queued, and the first problem when
  something needs you.

Not yet: a popover asking "this event or all events" when you move an instance of a series (it
moves the instance), the "notify guests?" bar (moves use the drawer's notify setting), and
picking an event from the keyboard.

## For scripts and agents

The same op pipeline, from the command line:

```sh
calm event add "Focus" --start "2026-11-24 09:00" --end 11:00 --calendar Personal
calm event move <id> --start "2026-11-24 10:00"
calm event delete <id>
calm event list --day 2026-11-24 --json        # with ids, etags and sync state
calm outbox [--json]                           # what's waiting, failed or in conflict
```

These write to the outbox and return at once. The running app (or the next one) sends them.

## Sign-in

Writing needs `https://www.googleapis.com/auth/calendar.events` in addition to `calendar.readonly`
(which still covers the calendar list and free/busy). `calm google login` requests both. An
existing read-only sign-in keeps working for reading. Edits made with it queue and stay `failed`
("sign in again to allow editing") until you sign in again. Nothing is lost.

## Testing

The existing fake Google (`google::Api`) grows knobs:
- Per-request status codes.
- Concurrent edits ("someone else changes field X now").
- **Lost responses** (apply the write, then return a network error).
- Delays.

On top of that:
- **Unit tests** for coalescing, the overlay, and the merge table (every row above).
- **Scenario tests:**
  - Edit offline, then reconnect.
  - Edit while someone else edits the same field, and a different field.
  - Create, then lose the response.
  - Delete an event deleted elsewhere.
  - Restart mid-send.
  - A recurring instance versus its series.
- **A convergence property test:** random sequences of local edits, remote edits and network
  faults. After everything flushes, the mirror equals the fake server's state, and every local
  edit was either applied or is visible as a conflict or failure. None vanished.

## Slices

1. **Engine:**
   - The outbox, the overlay, the Google write path, merge and the outcomes table.
   - `auto` and `review` modes, the pre-flight check, and the `history` table.
   - The `calm event`, `calm outbox`, `calm commit` and `calm discard` commands.
   - All of the tests above.
   - No UI changes beyond pending and failed marks.
2. **Drawer editing and the changes panel:**
   - Text fields (calm needs a small text input).
   - Delete and undo, default calendar, the guest notification toggle.
   - The changes panel, with its checkboxes, reverts, preview and commit.
3. **Direct manipulation:** the New and Adjust modes (create, move, resize, in every view; the
   all-day band; snapping; free time only; keyboard), with undo before sending. Still to come:
   the recurring-instance popover and the "notify guests?" bar.
4. **Later:** "this and following", replying to invitations, converting between all-day and
   timed, CalDAV (iCloud, Fastmail) behind the same outbox.

## Decided, but worth a second look

- **Merge granularity:** times count as one field; everything else is per field.
- **Undo** is the safety net for deletes; there is no confirmation dialog.
- **Notifying guests after a drag** defaults to Send after 30 s with no answer.
- **New events** go to `default-calendar`, else the account's primary calendar.
