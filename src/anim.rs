//! Interruptible critically-ish damped springs. Retargeting mid-flight keeps
//! velocity, so rapid view switching never snaps.

#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub x: f32,
    pub v: f32,
    pub target: f32,
}

#[derive(Clone, Copy)]
pub struct SpringCfg {
    pub stiffness: f32,
    pub damping: f32,
}

pub const GENTLE: SpringCfg = SpringCfg { stiffness: 120.0, damping: 20.0 };
pub const SNAPPY: SpringCfg = SpringCfg { stiffness: 600.0, damping: 48.0 };

const MAX_STEP: f32 = 1.0 / 240.0;

impl Spring {
    pub fn new(x: f32) -> Self {
        Self { x, v: 0.0, target: x }
    }

    /// Advances the spring, returning true while it is still moving.
    pub fn step(&mut self, dt: f32, cfg: SpringCfg) -> bool {
        if self.settled() {
            self.x = self.target;
            self.v = 0.0;
            return false;
        }
        let mut remaining = dt;
        while remaining > 0.0 {
            let h = remaining.min(MAX_STEP);
            let a = -cfg.stiffness * (self.x - self.target) - cfg.damping * self.v;
            self.v += a * h;
            self.x += self.v * h;
            remaining -= h;
        }
        true
    }

    pub fn settled(&self) -> bool {
        (self.x - self.target).abs() < 1e-4 && self.v.abs() < 1e-3
    }
}
