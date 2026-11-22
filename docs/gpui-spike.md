# GPUI animation spike (2026-11-22)

Question: can gpui give us the animation and transition flexibility DESIGN.md asks for
(spatial continuity, morphing marks between grammar specs, label LOD crossfades)?

**Verdict: yes, with gpui as a low-level painter.** The calendar surface is one `canvas`
element we drive ourselves. gpui's element tree handles the chrome: the pill, the drawer
and the HUD.

## What the spike does

`cargo run --release` (inside `devenv shell`). Keys: `1`–`5` or `Tab` switch presets,
`←/→` or scroll pans, `f` toggles the fisheye, `s` toggles stress (~6.4k events),
`Esc` closes the drawer. Click a mark to open the drawer. Drag a mark to move it in Week or
Swimlanes.

- All five presets from DESIGN.md §4 come from one grammar `Spec` (`src/spec.rs`) and one
  layout solver (`src/layout.rs`). The solver does window filtering, interval-colouring
  collision and projection into normalized `(u0,u1,v0,v1,polar,alpha)`.
- Every mark springs each channel independently, and the springs can be interrupted
  (`src/anim.rs`). A projection blends cartesian and polar by the springed `polar` channel,
  so Week → Dial physically folds the grid into a ring.
- The morph is staggered by position, so it reads as a wave. Label modes (inside, overflow,
  arc) fade out, swap and fade back in. Each preset's axis layer crossfades.
- Fisheye warp follows the mouse every frame. Drag edits the model live and the mark
  follows on a stiffer spring.
- The drawer is a gpui `div` whose position and opacity come from our spring. The pill
  uses gpui's built-in `with_animation` tween. Both systems mix fine.

## Capability findings

| Need | gpui 0.2.2 | Notes |
|---|---|---|
| Per-frame custom animation | ✅ | `window.request_animation_frame()`; stop requesting and the app goes fully idle |
| Arbitrary filled shapes (arcs, morphs) | ✅ | `Path` + `push_triangle`, rendered with MSAA |
| Rounded rects, borders, alpha | ✅ | `paint_quad` |
| Animated text colour/opacity | ✅ | `shape_line` every frame is cheap (layout cache is keyed without colour) |
| Clipping | ✅ | `with_content_mask` |
| Built-in tweens/easing | ✅ (basic) | `with_animation`, keyed by `ElementId`; fine for chrome |
| Element transforms (rotate/scale) | ❌ | Only SVG has `with_transformation`. We can't do rotated text along arcs. We'd draw our own glyph runs or accept upright labels |
| Group opacity / offscreen layers | ❌ | No compositing of a subtree into a texture. Fade by multiplying alpha per primitive (what we do) |
| Blur / backdrop filters / custom shaders | ❌ | Only box shadows. Frosted-glass effects aren't possible without forking the renderer |

None of the missing pieces block the design. Rotated arc labels are the only one we would
notice, and DESIGN.md already allows semantic symbols at macro zoom.

## Performance (Ryzen 8-core, niri/Wayland, 60 Hz panel, release build)

Stress bench: `calm --bench` runs 6,360 events and switches preset every 1.1 s for 14 s.

| | CPU per frame p50 / p95 / p99 |
|---|---|
| Naive (lyon `PathBuilder` per mark, no layers) | 3.1 / 19.8 / 28.6 ms |
| + direct triangle strips + `paint_layer` batching + skip invisible springs | **1.8 / 6.3 / 10.6 ms** |

- The worst frames are the Dial → Quarter morph, where all 6,360 marks are mid-fold arcs at
  once. A real app would aggregate the heatmap to 84 cells, so this is a deliberately
  pessimistic case. The normal dataset (~640 events) has lots of headroom.
- Paint costs about 1.5–2 µs per primitive when outside a layer, because each insert goes
  through gpui's `BoundsTree`. **Use `window.paint_layer` for dense mark sets.**
- Idle: 0 CPU ticks over 10 s once springs settle. RSS is about 73 MB.
- Presented-frame intervals were vsync-locked (p50 16.7 ms). Runs where the window was on
  an off-screen niri column got ~1 Hz frame callbacks from the compositor, so read interval
  percentiles only from a visible window.

## Caveats / follow-ups

- gpui 0.2.2 on crates.io is about a year behind Zed `main`. It needs a local `xattr` patch
  (see `Cargo.toml`) to build against current `libc`. We should probably track a pinned Zed
  git rev instead.
- No 120 Hz panel has been tested yet.
- The step + relayout pass is O(n) springs on the main thread (about 2 ms at 6k marks).
  If it matters, it could be SIMD'd or run only on in-window marks.
