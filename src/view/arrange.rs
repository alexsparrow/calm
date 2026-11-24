//! The facet arrangement: where each day cell sits on screen for a given
//! zoom (`n`, the number of cells visible) and scroll (`start`, the first
//! visible cell, both continuous).
//!
//! Three regimes, blended where they meet so zooming never jumps:
//!
//! * **strip** (`n ≤ per_line`): one line, cropped to `n` cells.
//! * **lines** (`per_line < n ≤ per_line × band`): whole lines, stacked.
//! * **bands** (beyond): lines wrap into bands laid side by side.
//!
//! Computation happens in (main, cross) space — main runs along a line —
//! and is transposed for column-wise facets.

use std::f32::consts::{FRAC_PI_2, TAU};
use std::ops::Range;

use crate::spec::{Coord, Dir, Facet};

pub type Pt = (f32, f32);
pub type Rect = [f32; 4];

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0., 1.);
    t * t * (3. - 2. * t)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp_rect(a: Rect, b: Rect, t: f32) -> Rect {
    [lerp(a[0], b[0], t), lerp(a[1], b[1], t), lerp(a[2], b[2], t), lerp(a[3], b[3], t)]
}

/// Packed facets (an agenda): a day's heading, each event's row, and the gap after a day.
pub const PACK_HEAD: f32 = 34.;
pub const PACK_ROW: f32 = 30.;
pub const PACK_GAP: f32 = 12.;

#[derive(Clone, Debug)]
pub struct Arr {
    /// Plot rectangle: x, y, w, h.
    pub plot: [f32; 4],
    pub facet: Facet,
    pub n: f32,
    pub start: f32,
    pub days: i32,
    /// Always draw cells as separate boxes (aggregate-only lenses).
    pub boxed: bool,
    /// For a packed facet: where each day starts down the list, in pixels from the first,
    /// and where the last ends (`days + 1` of them). Empty days take no room.
    pub pack: Option<std::sync::Arc<[f32]>>,
}

impl Arr {
    fn per(&self) -> f32 {
        self.facet.per_line as f32
    }

    fn band(&self) -> f32 {
        self.facet.band as f32
    }

    /// Lines visible, at least one. (A packed list is one long line.)
    pub fn lines(&self) -> f32 {
        if self.pack.is_some() {
            return 1.;
        }
        (self.n / self.per()).max(1.)
    }

    /// A packed list's offset at a (fractional) day.
    fn off(&self, offs: &[f32], d: f32) -> f32 {
        let d = d.clamp(0., (offs.len() - 1) as f32);
        let i = (d.floor() as usize).min(offs.len() - 2);
        offs[i] + (offs[i + 1] - offs[i]) * (d - i as f32)
    }

    /// The (fractional) day at a packed list's offset: the inverse of `off`.
    fn day_of(&self, offs: &[f32], o: f32) -> f32 {
        let i = offs.partition_point(|&x| x <= o).saturating_sub(1).min(offs.len() - 2);
        let h = offs[i + 1] - offs[i];
        i as f32 + if h > 0. { ((o - offs[i]) / h).clamp(0., 1.) } else { 0. }
    }

    /// Where `start` would be after scrolling `px` down a packed list.
    pub fn scrolled(&self, px: f32) -> Option<f32> {
        let offs = self.pack.as_deref()?;
        Some(self.day_of(offs, self.off(offs, self.start) + px))
    }

    /// Bands visible, at least one.
    pub fn bands(&self) -> f32 {
        (self.lines() / self.band()).max(1.)
    }

    /// 0 for one continuous line, 1 once several lines read as separate boxes.
    pub fn boxness(&self) -> f32 {
        if self.pack.is_some() {
            return 0.;
        }
        if self.boxed { 1. } else { smoothstep(1.12, 1.6, self.lines()) }
    }

    fn main_cross(&self) -> (f32, f32) {
        let [_, _, w, h] = self.plot;
        match self.facet.dir {
            Dir::Rows => (w, h),
            Dir::Columns => (h, w),
        }
    }

    /// (main0, cross0, main size, cross size) per regime.
    fn strip(&self, d: f32) -> Rect {
        let (pm, pc) = self.main_cross();
        let size = pm / self.n.max(1e-3);
        [(d - self.start) * size, 0., size, pc]
    }

    fn lines_mode(&self, d: i32) -> Rect {
        let (pm, pc) = self.main_cross();
        let p = self.facet.per_line;
        let size = pm / self.per();
        let thick = pc / self.lines();
        let line = d.div_euclid(p) as f32;
        let k = d.rem_euclid(p) as f32;
        [k * size, (line - self.start / self.per()) * thick, size, thick]
    }

    fn bands_mode(&self, d: i32) -> Rect {
        let (pm, pc) = self.main_cross();
        let (p, b) = (self.facet.per_line, self.facet.band);
        let band_m = pm / self.bands();
        // A gutter between bands, growing in as they separate.
        let gap = 16. * smoothstep(1., 1.5, self.bands());
        let size = (band_m - gap) / self.per();
        let thick = pc / self.band();
        let line = d.div_euclid(p);
        let k = d.rem_euclid(p) as f32;
        let bi = line.div_euclid(b) as f32;
        let r = line.rem_euclid(b) as f32;
        let b0 = self.start / (self.per() * self.band());
        [(bi - b0) * band_m + gap * 0.5 + k * size, r * thick, size, thick]
    }

    fn weights(&self) -> (f32, f32) {
        let (p, b) = (self.per(), self.band());
        (smoothstep(p, p * 1.2, self.n), smoothstep(p * b, p * b * 1.15, self.n))
    }

    /// The cell's screen rectangle (x0, y0, x1, y1), before insets.
    pub fn cell(&self, d: i32) -> Rect {
        if let Some(offs) = self.pack.as_deref() {
            let [x, y, w, _] = self.plot;
            let i = (d.max(0) as usize).min(offs.len() - 2);
            let top = y + offs[i] - self.off(offs, self.start);
            return [x, top, x + w, top + offs[i + 1] - offs[i]];
        }
        let (w1, w2) = self.weights();
        let mut mc = if w1 < 1. { self.strip(d as f32) } else { self.lines_mode(d) };
        if w1 > 0. && w1 < 1. {
            mc = lerp_rect(mc, self.lines_mode(d), w1);
        }
        if w2 > 0. {
            mc = lerp_rect(mc, self.bands_mode(d), w2);
        }
        let [x, y, _, _] = self.plot;
        let [m, c, sm, sc] = mc;
        match self.facet.dir {
            Dir::Rows => [x + m, y + c, x + m + sm, y + c + sc],
            Dir::Columns => [x + c, y + m, x + c + sc, y + m + sm],
        }
    }

    /// The cell with the gap that appears once cells read as boxes.
    pub fn cell_inset(&self, d: i32) -> Rect {
        let b = self.cell(d);
        let g = 2.5 * self.boxness();
        [b[0] + g, b[1] + g, b[2] - g, b[3] - g]
    }

    /// A conservative range of days that may be on screen.
    pub fn visible(&self) -> Range<i32> {
        if let Some(offs) = self.pack.as_deref() {
            let o = self.off(offs, self.start);
            let lo = offs.partition_point(|&x| x < o - 1.).saturating_sub(1);
            let hi = offs.partition_point(|&x| x <= o + self.plot[3] + 1.);
            return (lo as i32)..(hi as i32).min(self.days);
        }
        let (p, b) = (self.per(), self.band());
        let (w1, w2) = self.weights();
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        if w1 < 1. {
            lo = lo.min(self.start - 1.);
            hi = hi.max(self.start + self.n + 1.);
        }
        if w1 > 0. && w2 < 1. {
            let l0 = (self.start / p).floor() - 1.;
            lo = lo.min(l0 * p);
            hi = hi.max((l0 + self.lines() + 3.) * p);
        }
        if w2 > 0. {
            let b0 = (self.start / (p * b)).floor() - 1.;
            lo = lo.min(b0 * p * b);
            hi = hi.max((b0 + self.bands() + 3.) * p * b);
        }
        (lo.floor().max(0.) as i32)..(hi.ceil() as i32).min(self.days)
    }

    pub fn on_screen(&self, r: &Rect) -> bool {
        let [x, y, w, h] = self.plot;
        r[2] > x + 1. && r[0] < x + w - 1. && r[3] > y + 1. && r[1] < y + h - 1.
    }

    pub fn day_at(&self, (px, py): Pt) -> Option<i32> {
        self.visible().find(|&d| {
            let c = self.cell(d);
            px >= c[0] && px < c[2] && py >= c[1] && py < c[3]
        })
    }

    /// Cells per screen pixel along the scroll direction, for wheel scrolling.
    pub fn cells_per_px(&self) -> f32 {
        let (pm, pc) = self.main_cross();
        let (w1, w2) = self.weights();
        if w1 < 0.5 {
            self.n / pm
        } else if w2 < 0.5 {
            self.per() * self.lines() / pc
        } else {
            self.per() * self.band() * self.bands() / pm
        }
    }
}

/// Coordinates inside one cell: cartesian or polar, with the time axis
/// windowed to an hour range.
#[derive(Clone, Copy, Debug)]
pub struct Inner {
    pub cell: Rect,
    pub coord: Coord,
    /// Inner x and y ranges (fractions of the axis) that fill the cell.
    pub xr: (f32, f32),
    pub yr: (f32, f32),
}

impl Inner {
    pub fn nu(&self, u: f32) -> f32 {
        (u - self.xr.0) / (self.xr.1 - self.xr.0)
    }

    pub fn nv(&self, v: f32) -> f32 {
        (v - self.yr.0) / (self.yr.1 - self.yr.0)
    }

    pub fn center(&self) -> Pt {
        ((self.cell[0] + self.cell[2]) * 0.5, (self.cell[1] + self.cell[3]) * 0.5)
    }

    /// Outer and inner radius of the dial in this cell.
    pub fn radii(&self) -> (f32, f32) {
        let m = (self.cell[2] - self.cell[0]).min(self.cell[3] - self.cell[1]) * 0.5;
        // Big dials leave room for hour labels outside the ring.
        let r_out = m - (m * 0.16).clamp(3., 54.);
        (r_out * 0.36, r_out)
    }

    pub fn radius(&self, v: f32) -> f32 {
        let (r_in, r_out) = self.radii();
        r_in + self.nv(v) * (r_out - r_in)
    }

    pub fn angle(&self, u: f32) -> f32 {
        self.nu(u) * TAU - FRAC_PI_2
    }

    pub fn map(&self, u: f32, v: f32) -> Pt {
        match self.coord {
            Coord::Cartesian => {
                let [x0, y0, x1, y1] = self.cell;
                (x0 + self.nu(u) * (x1 - x0), y0 + self.nv(v) * (y1 - y0))
            }
            Coord::Polar => {
                let (cx, cy) = self.center();
                let (r, th) = (self.radius(v), self.angle(u));
                (cx + r * th.cos(), cy + r * th.sin())
            }
        }
    }
}

impl Inner {
    /// The inverse of `map`: inner (u, v) for a screen point (outside the cell, beyond 0..1).
    pub fn unmap(&self, (x, y): Pt) -> (f32, f32) {
        let (nu, nv) = match self.coord {
            Coord::Cartesian => {
                let [x0, y0, x1, y1] = self.cell;
                ((x - x0) / (x1 - x0).max(1e-3), (y - y0) / (y1 - y0).max(1e-3))
            }
            Coord::Polar => {
                let (cx, cy) = self.center();
                let (dx, dy) = (x - cx, y - cy);
                let (r_in, r_out) = self.radii();
                let nu = ((dy.atan2(dx) + FRAC_PI_2) / TAU).rem_euclid(1.);
                (nu, ((dx * dx + dy * dy).sqrt() - r_in) / (r_out - r_in).max(1e-3))
            }
        };
        (self.xr.0 + nu * (self.xr.1 - self.xr.0), self.yr.0 + nv * (self.yr.1 - self.yr.0))
    }
}
