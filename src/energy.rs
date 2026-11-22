//! Personal energy through the day: a baseline (a model of alertness, or your own
//! curve), optionally drawn down by what the day asks of you.
//!
//! The default baseline is the three-process model of alertness (Åkerstedt & Folkard):
//! alertness is the sum of a homeostatic process S, falling exponentially with time
//! awake and recovering exponentially during sleep, and a circadian process C, a
//! sinusoid over 24 hours. (Its third process, sleep inertia after forced awakening,
//! is left out: its size isn't given in the sources we used. Waking instead eases in
//! over about an hour, the duration they give for spontaneous awakening.)
//!
//! Rates are as published in Åkerstedt & Folkard (1995), "Quantitative model for
//! vigilance regulation and its application in transport operations": S falls 3.5 %
//! of its value per hour awake and recovers 28.7 % per hour asleep. The asymptotes
//! (2.4, 14.3), circadian amplitude (2.5) and acrophase (16:48) are the values commonly
//! quoted for the model (e.g. Åkerstedt et al. 2004, "Predictions from the three-process
//! model of alertness"); `peak` lets evening types shift the phase, which the authors
//! suggest can be needed by up to four hours.
//!
//! The model's output is on its 1–16 alertness scale, where 7 marks the onset of
//! critically low alertness. Energy maps 7 → 0 and 14 → 1.
//!
//! Reactive energy: demanding time drains a reserve, rest restores it. With demand
//! `d` (0..1) the drain is `drain · d` per hour, and the deficit recovers with time
//! constant `recovery`. Energy = baseline − deficit. So a long run of demanding
//! meetings sags the curve, and a free hour lets it climb back.

use crate::model::Event;

/// Samples per hour in a day's curve (every 10 minutes), and per day.
pub const PER_HOUR: usize = 6;
pub const SAMPLES: usize = 24 * PER_HOUR + 1;

#[derive(Clone, Debug, PartialEq)]
pub enum Baseline {
    /// Hours (0..24) of waking, sleeping and the circadian peak.
    ThreeProcess { wake: f32, sleep: f32, peak: f32 },
    /// Your own curve: (hour, level 0..1), a periodic Catmull–Rom spline through them.
    Points(Vec<(f32, f32)>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnergyModel {
    pub baseline: Baseline,
    pub reactive: bool,
    /// Energy lost per hour at full demand.
    pub drain: f32,
    /// Hours for a deficit to recover by about two thirds (the time constant).
    pub recovery: f32,
}

impl Default for EnergyModel {
    fn default() -> Self {
        Self { baseline: Baseline::ThreeProcess { wake: 7., sleep: 23., peak: 16.8 }, reactive: true, drain: 0.3, recovery: 1. }
    }
}

// Three-process constants (see the module docs for sources).
const S_LOW: f32 = 2.4;
const S_HIGH: f32 = 14.3;
const C_AMPLITUDE: f32 = 2.5;

/// Per-hour rate constants from the published percentages.
fn rates() -> (f32, f32) {
    (-(1f32 - 0.035).ln(), -(1f32 - 0.287).ln())
}

impl EnergyModel {
    /// The baseline at `hour` (0..24), before any drain.
    pub fn baseline(&self, hour: f32) -> f32 {
        let h = hour.rem_euclid(24.);
        match &self.baseline {
            Baseline::ThreeProcess { wake, sleep, peak } => {
                let awake_len = (sleep - wake).rem_euclid(24.);
                let since_wake = (h - wake).rem_euclid(24.);
                if since_wake >= awake_len {
                    return 0.;
                }
                // Steady state: S at waking, after a regular night (closed form of
                // wake decay followed by sleep recovery repeating day after day).
                let (k_wake, k_sleep) = rates();
                let a = (-k_wake * awake_len).exp();
                let b = (-k_sleep * (24. - awake_len)).exp();
                let s0 = (S_HIGH - (S_HIGH - S_LOW) * b - S_LOW * a * b) / (1. - a * b);
                let s = S_LOW + (s0 - S_LOW) * (-k_wake * since_wake).exp();
                let c = C_AMPLITUDE * (std::f32::consts::TAU * (h - peak) / 24.).cos();
                // Waking up takes a while: the authors put a spontaneous awakening at about
                // an hour, so ease in over that (95 % after an hour) rather than step up.
                let waking = 1. - (-3. * since_wake).exp();
                ((s + c - 7.) / 7.).clamp(0., 1.) * waking
            }
            Baseline::Points(points) => spline(points, h),
        }
    }

    /// One day's curve, `SAMPLES` levels from midnight to midnight, given that day's events.
    pub fn day<'a>(&self, events: impl Iterator<Item = &'a Event> + Clone) -> Vec<f32> {
        let dt = 1. / PER_HOUR as f32;
        let mut deficit = 0f32;
        (0..SAMPLES)
            .map(|k| {
                let hour = k as f32 * dt;
                let level = self.baseline(hour);
                if !self.reactive {
                    return level;
                }
                let minute = hour * 60.;
                // What the hour asks of you: the most demanding thing on, if anything.
                let demand = events
                    .clone()
                    .filter(|e| e.start_min <= minute && minute < e.end_min())
                    .map(|e| e.energy)
                    .fold(0f32, f32::max);
                let out = (level - deficit).clamp(0., 1.);
                deficit += (self.drain * demand - deficit / self.recovery.max(0.05)) * dt;
                deficit = deficit.max(0.);
                out
            })
            .collect()
    }
}

fn spline(points: &[(f32, f32)], h: f32) -> f32 {
    let n = points.len();
    match n {
        0 => return 0.5,
        1 => return points[0].1,
        _ => {}
    }
    let i = points.iter().rposition(|&(kh, _)| kh <= h).map_or(-1, |i| i as isize);
    let at = |k: isize| {
        let j = k.rem_euclid(n as isize) as usize;
        let wraps = k.div_euclid(n as isize) as f32;
        (points[j].0 + 24. * wraps, points[j].1)
    };
    let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
    let t = ((h - p1.0) / (p2.0 - p1.0).max(1e-3)).clamp(0., 1.);
    let (t2, t3) = (t * t, t * t * t);
    let v = 0.5
        * (2. * p1.1
            + (-p0.1 + p2.1) * t
            + (2. * p0.1 - 5. * p1.1 + 4. * p2.1 - p3.1) * t2
            + (-p0.1 + 3. * p1.1 - 3. * p2.1 + p3.1) * t3);
    v.clamp(0., 1.)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_process_shape() {
        let m = EnergyModel::default();
        // Asleep is zero; awake it rises from waking into the day, and falls by bedtime.
        assert!(m.baseline(7.) < 0.01 && m.baseline(7.5) > 0.4);
        assert_eq!(m.baseline(3.), 0.);
        let (wake, noon, evening, late) = (m.baseline(8.), m.baseline(12.), m.baseline(17.), m.baseline(22.5));
        assert!(wake > 0.5 && noon > wake && evening > late && late < 0.45, "{wake} {noon} {evening} {late}");
    }

    #[test]
    fn demanding_time_drains_and_rest_restores() {
        let calm = EnergyModel { reactive: false, ..Default::default() };
        let m = EnergyModel::default();
        let meeting = |start_h: f32, hours: f32| Event {
            title: "".into(),
            calendar: 0,
            day: 0,
            start_min: start_h * 60.,
            dur_min: hours * 60.,
            energy: 0.9,
            status: crate::model::Status::Confirmed,
            location: None,
            notes: None,
            group: 0,
            strain: 0.,
            all_day: None,
        };
        let events = [meeting(9., 3.)];
        let curve = m.day(events.iter());
        let base = calm.day(events.iter());
        let at = |c: &[f32], h: f32| c[(h * PER_HOUR as f32) as usize];
        // Three demanding hours leave you lower than your baseline...
        assert!(at(&base, 12.) - at(&curve, 12.) > 0.2);
        // ...and a free afternoon recovers most of it.
        assert!(at(&base, 16.) - at(&curve, 16.) < 0.06);
        // Longer is worse than shorter at the same demand.
        let short = m.day([meeting(9., 1.)].iter());
        assert!(at(&short, 12.) > at(&curve, 12.));
    }
}
