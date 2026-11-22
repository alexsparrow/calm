# Architecture & Product Design Document: Grammar of Time (Working Title)

> **Document Status: Draft / Incomplete Exploration**
> This document captures the conceptual architecture, core primitives, and interaction model for a declarative, calm-first calendar system. It is intentionally incomplete—many edge cases (such as multi-day spanning events, complex recurrence exceptions, and high-density mobile viewports) remain open design questions. **Implementation technologies, rendering engines, and storage layers are deliberately omitted** so they can be chosen independently during engineering.

---

## 1. Vision & Design Philosophy

Traditional calendars force a single spatial metaphor—the rigid vertical 7-column box grid—onto every context of human time. While effective for finding open slots across a workweek, this fixed layout creates visual noise, obscures cross-calendar conflicts, and treats high-focus deep work identically to routine administrative tasks.

**Core Thesis:** A calendar should function like a **Grammar of Graphics** for temporal data. Rather than hardcoding fixed screens ("Day," "Week," "Month"), the system treats time items as a normalized dataset and renders views from lightweight, composable specifications.

### Guiding Principles

* **Calm by Default, Power on Demand:** Strip away structural chrome, harsh gridlines, and dense toolbars. Surface analytical depth (conflicts, cognitive load, energy curves) only when the user shifts lenses or inspects a specific moment.
* **Spatial Continuity Over Hard Cuts:** Changing views should feel like folding, stretching, or rotating the same physical surface rather than reloading a separate screen.
* **Decouple Label from Duration:** Never trap text inside microscopic 15-minute slivers. Let labels overflow into open whitespace, stack cleanly, or transition into semantic symbols at macro zoom levels.

---

## 2. Normalized Event Model (Input Space)

To support arbitrary spatial projections, every item—whether a scheduled meeting, an all-day milestone, a focus block, or an unscheduled task—is normalized into a uniform record before layout calculation.

### Core Attributes

* **Identity & Content:** `id`, `title`, `notes`, `status` (`confirmed` | `tentative` | `task`)
* **Temporal Bounds:** `start` (timestamp), `end` (timestamp), `isAllDay` (boolean)
* **Categorical Facets:** `calendar` (e.g., Work, Personal, Family), `location`, `tags[]`
* **Quantitative / Semantic Attributes:** `demand` (a.k.a. cognitive load, normalized `0.0–1.0`; see below), `priority`

### Two Kinds of "Energy"

These are easy to conflate, so the model keeps them apart:

* **Event demand** (`demand`, per event, `0.0–1.0`): how much an event asks of you. It belongs
  to the event, like `priority`. No calendar standard carries it (iCalendar has no such
  property), so it is inferred from signals such as calendar, title, attendees and duration,
  overridden by the user, and optionally persisted as an `X-` property.
* **Personal energy** (a curve over time of day, `0.0–1.0`): how much capacity *you* typically
  have at a given hour, such as a morning peak, a post-lunch dip and an evening second wind.
  It belongs to the person, not to any event. It starts from a default chronotype and may
  later be tuned or learned.

The interesting signal is the **mismatch** between them: demanding events scheduled in a
personal energy dip (*strain*), or light admin occupying a peak.

### Derived Temporal Dimensions (Computed at Query Time)

The layout engine enriches each event with projected temporal coordinates relative to the active viewport:

* `timeOfDay` (minutes from midnight, `0–1440`)
* `dayOfWeek` (`0–6`)
* `weekIndex` (relative or ISO week number)
* `duration` (minutes)
* `conflictGroupId` & `isConflict` (computed overlap flags within and across calendars)

---

## 3. The Grammar Specification

Every calendar view is defined by a declarative **View Specification** composed of orthogonal channels (the positional ones, `color` and `detail`, plus `facet`, `lod` and `stops`, which make zoom part of the grammar):

| Channel | Purpose | Possible Values |
| --- | --- | --- |
| **`coord`** | Coordinate system & spatial warping | `cartesian`, `polar` (radial clock/ring), `fisheye` (non-linear magnification around a focal timestamp) |
| **`x`** | Primary horizontal (or angular $\theta$) mapping | `continuousTime`, `timeOfDay`, `dayOfWeek`, `weekIndex`, `calendar`, `none` |
| **`y`** | Primary vertical (or radial $r$) mapping | `timeOfDay`, `dayOfWeek`, `calendar`, `demand`, `priority`, `none` |
| **`wrap`** | Modulo folding rules for continuous axes | `none`, `daily` ($\bmod 24\text{h}$), `weekly` ($\bmod 7\text{d}$) |
| **`collide`** | Spatial resolution for overlapping events | `split` (sub-lanes), `stack` (vertical pile), `overlay` (translucent layers), `highlight-conflict`, `aggregate` (sum/density) |
| **`mark`** | Visual geometry & semantic Level-of-Detail | `block`, `pill-overflow`, `arc`, `dot`, `heatmap` |
| **`color`** | What drives fill: a measure, a stat over the events in the mark (an event is a set of one; a heatmap cell is a day), and a scale. Optionally faceted by calendar, with the parts *composed* | measure: `hours`, `count`, `demand`, `strain`, `free`, `starts` (min start), `ends` (max end), `span`; facet: `none`, `calendar`; compose: `blend`, `stripes`, `stack`, `dominant` |
| **`detail`** | A secondary glyph nested inside aggregate cells, independent of colour | `none`, `daySpan` (first start → last end on a mini time axis) |
| **`facet`** | How day cells tile the plane: a wrap of days into *lines*, lines stacked and wrapped into *bands*. This is the nesting operator: `y = week / timeOfDay` reads "time of day, nested within weeks" | `perLine` (7 = week rows, 1 = day rows), `dir` (`rows`, `columns`), `band` (lines per band, e.g. 13 = a quarter) |
| **`lod`** | Semantic zoom: which `mark` to draw as cells shrink, with hysteresis | e.g. `block ≥ 240px`, `list ≥ 60px`, else `heat` |
| **`stops`** | Zoom snap points and how each aligns to the calendar | `day`, `week` (line), `month` (lines spanning a month), `quarter`/`year` (groups from the origin) |
| **`zones`** | Which time zones label the time axis. The zones themselves are a global setting (the system zone plus configured extras); a view can only narrow them | `all`, `primary` |

With nesting, `x` and `y` describe coordinates *inside* a day cell, and `coord` applies per cell. A polar
lens zoomed out becomes small multiples of dials.

### Zoom Is Not a Channel

Zoom is the number of day cells visible, and nothing else. The `facet` decides how that many cells
arrange: one cropped line (a day, part of a week), whole lines (week → month → quarter) or side-by-side
bands (a year as four quarters). The arrangement blends between these regimes so zooming is continuous.
The `lod` decides what each cell draws at its current size; crossing a threshold morphs marks in screen
space (blocks fold into month-view rows, rows collapse into aggregate cells). `stops` give the motion
places to rest. The same rules make Week zoom to a year, Lanes and Energy zoom to small multiples of
day-rows, and Dial zoom to a grid of dials, with no lens-specific zoom code.

---

## 4. Canonical View Presets (Reference Mappings)

Out of the box, the user interacts with curated presets—each simply a saved Grammar Specification:

1. **Classic Week Grid (Baseline):**
`coord: cartesian` · `x: dayOfWeek` · `y: timeOfDay` · `collide: split` · `mark: block`
*Use case:* Scanning standard weekly availability.
2. **Cross-Calendar Conflict Swimlanes:**
`coord: cartesian` · `x: timeOfDay` · `y: calendar` · `collide: highlight-conflict` · `mark: pill-overflow`
*Use case:* Comparing separate calendars side-by-side; double-bookings align vertically without squeezing columns.
3. **Cognitive Load & Energy Topography:**
`coord: cartesian` · `x: timeOfDay` · `y: demand` · `collide: stack` · `mark: pill-overflow`, over a personal-energy area layer
*Use case:* Auditing daily pacing. Events float at their demand above your personal energy curve, so strain (demand above the curve) and wasted peaks read at a glance, as does heavy focus work fragmented by low-value admin.
4. **24-Hour Mindful Dial:**
`coord: polar` · `x (θ): timeOfDay` · `y (r): calendar` · `collide: overlay` · `mark: arc`
*Use case:* Intraday awareness where open hours read as tangible empty wedges rather than empty scroll space.
5. **Macro Quarter Density:**
`coord: cartesian` · `x: weekIndex` · `y: dayOfWeek` · `collide: aggregate` · `mark: heatmap`
*Use case:* Multi-month workload balancing and identifying crunch periods before they happen.

---

## 5. Layout Solver & Transition Pipeline

To keep the interface serene, switching specifications should follow a four-stage pipeline:

1. **Filter & Window:** Slice the normalized event store to the active temporal window (plus buffer days for smooth panning).
2. **Collision & Grouping Pass:** Group events sharing identical discrete coordinates (e.g., same `dayOfWeek` or `calendar` lane) and run interval-tree overlap detection to assign sub-lane indices or compute aggregate density metrics.
3. **Coordinate Projection:** Map each event's temporal and categorical values into normalized viewport coordinates `(x, y, width, height)` or polar parameters `(startAngle, endAngle, innerRadius, outerRadius)`.
4. **Semantic Level-of-Detail (LOD) & Morphing:**
* Determine label visibility and overflow rules based on available mark dimensions.
* Interpolate spatial transforms between the previous layout state and the new layout state so events visibly glide into their new roles.

---

## 6. Interaction Model (Concealed Power)

* **Ambient Canvas:** The viewport opens directly into the active lens with generous margins, muted organic tones, and minimal axis ticks.
* **Floating Grammar Pill:** A single unobtrusive pill anchors the bottom of the screen, displaying the active spec in plain language (e.g., `Cartesian · Time × Calendar · Pills`). Tapping any token cycles that channel's mapping in place.
* **Progressive Disclosure Drawers:** Clicking an event or an empty time gap expands an inline contextual drawer for editing, nudging duration (`-15m` / `+15m`), or converting open whitespace into a protected focus block.
* **Ambient Pacing Cues:** Subtle indicators highlight back-to-back context switches (zero buffer between different categories) or unscheduled recovery gaps without intrusive alerts.

---

## 7. Open Questions & Incomplete Areas (To Be Defined)

Because this document is an early architectural sketch, several critical areas are intentionally left open for future design and engineering iterations:

* **Multi-Day & All-Day Events:** How do 3-day trips or week-long deadlines behave when `y` is mapped to `energy` or `coord` is set to `polar`? (Likely requires a dedicated top/outer "horizon track" outside the primary projection).
* **Direct Manipulation & Drag-and-Drop:** Dragging an event in a Classic Grid cleanly mutates `start` and `dayOfWeek`. Dragging an event in **Energy Topography** mutates `start` and `energy`—we need clear affordances so users know which attributes a drag gesture will modify.
* **Semantic Zoom Thresholds:** Exact hysteresis rules for smoothly crossfading between `block` → `pill-overflow` → `heatmap` during continuous pinch/scroll zooming without visual flicker.
* **Recurrence & Task Integration:** How unscheduled tasks with due dates (rather than fixed start times) enter the layout solver as draggable/floating candidates.
* **Small-Screen / Touch Adaptation:** How horizontal swimlanes and overflow labels degrade gracefully on narrow mobile viewports.
