//! Individual manager agents: traits, motives, memory, and a career.
//!
//! Every club is run by a persistent `Manager` with their own name, trait
//! profile, and career record (seasons, trophies travel with them between
//! jobs). Decisions are made from local information through small weighted
//! threshold models — one layer of weighted inputs, i.e. perceptron-style,
//! with per-manager weights so identical situations resolve differently per
//! individual. Deterministic per seed, never a global optimizer.
//!
//! Honest accounting of every decision rule lives in the doc comments below
//! and in docs/FM2023_PROBE.md ("World model accounting").

use rand::Rng;

use crate::gen::Gen;
use crate::tune::Params;

#[derive(Clone, Debug)]
pub(crate) struct Manager {
    pub name: String,
    pub seasons: u16,
    pub trophies: u16,
    /// Age in years. Managers are hired in their late 30s–50s and retire with
    /// rising probability through their 60s (hard cap at 72) — nobody runs a
    /// club for half a century.
    pub age: u8,
    /// Seasons since THIS manager last won a league title. Resets to 0 on a
    /// title and on every new appointment, so a drought is always measured
    /// against the current regime, never the club's whole history.
    pub title_drought: u16,
    /// 0.20-0.49: share of the XI rotated when alternatives exist.
    pub rotation: f32,
    /// 0.6-1.5: transfer aggressiveness (attempts + clause triggering).
    pub ambition: f32,
    /// Lifts/drags results vs the club's level (tuned band).
    pub quality: f32,
    /// 0-1: trust in youth (U23 selection boost + development focus).
    pub youth_trust: f32,
    /// 0-1: high loyalty rejects poaching; low loyalty chases bigger jobs.
    pub loyalty: f32,
    /// 0-1: perceived job safety. Insecure managers play stars, not kids.
    pub security: f32,
}

impl Manager {
    pub fn new_random(rng: &mut impl Rng, p: &Params) -> Self {
        Manager {
            name: Gen::person_name(rng),
            seasons: 0,
            trophies: 0,
            age: rng.gen_range(38..=58),
            title_drought: 0,
            rotation: rng.gen_range(0.20..0.50),
            ambition: rng.gen_range(0.6..1.5),
            quality: rng.gen_range(p.q_init_lo..p.q_init_hi),
            youth_trust: rng.gen_range(0.0..1.0),
            loyalty: rng.gen_range(0.0..1.0),
            security: 0.6,
        }
    }

    /// An unproven hire after a sacking: wider quality spread, shaky security,
    /// and typically younger — a club rolling the dice on an up-and-comer.
    pub fn fresh_hire(rng: &mut impl Rng, p: &Params) -> Self {
        let mut m = Self::new_random(rng, p);
        m.quality = rng.gen_range(p.q_fresh_lo..p.q_fresh_hi);
        m.security = 0.5;
        m.age = rng.gen_range(36..=52);
        m
    }

    /// Whether this manager hangs up the tracksuit this summer. Flat below 60,
    /// then climbing through the 60s to a hard cap at 72 — the natural ceiling
    /// that stops any 40- or 50-season tenure without a hardcoded term limit.
    pub fn retires(&self, rng: &mut impl Rng) -> bool {
        let p: f64 = match self.age {
            0..=59 => 0.0,
            60..=61 => 0.08,
            62..=63 => 0.13,
            64..=65 => 0.20,
            66..=67 => 0.30,
            68 => 0.45,
            69 => 0.60,
            70 => 0.75,
            71 => 0.88,
            _ => 1.0,
        };
        rng.gen_bool(p.clamp(0.0, 1.0))
    }

    /// Insecure managers stop rotating and ride their stars.
    pub fn effective_rotation(&self) -> f32 {
        if self.security < 0.4 {
            self.rotation * 0.5
        } else {
            self.rotation
        }
    }

    /// Selection boost for under-23 players under a trusting manager.
    pub fn youth_boost(&self) -> f32 {
        1.0 + self.youth_trust * 0.15
    }

    /// Whether this manager would leave for a bigger club when approached.
    pub fn wants_bigger_job(&self) -> bool {
        self.ambition > 1.0 && self.loyalty < 0.6
    }

    /// Slow regression toward average quality: eras rise and fade, nobody
    /// stays blessed or cursed forever on one old hire. Ages the manager by a
    /// season. (Title drought is advanced by the league loop, not here, so it
    /// stays an exact count of trophyless seasons.)
    pub fn regress(&mut self, rate: f32) {
        self.quality = (self.quality * rate + 1.0 * (1.0 - rate)).clamp(0.82, 1.20);
        self.seasons += 1;
        self.age = self.age.saturating_add(1);
    }

    /// Update perceived safety from finish vs expectation (rep rank).
    /// `over` > 0 means finished above the squad's station.
    pub fn review_season(&mut self, over: i32) {
        if over >= 3 {
            self.security = (self.security + 0.2).min(1.0);
        } else if over <= -3 {
            self.security = (self.security - 0.25).max(0.0);
        } else {
            self.security = (self.security + 0.05).min(1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tune::Params;
    use rand_pcg::Pcg64Mcg;

    fn mgr() -> Manager {
        let mut rng = Pcg64Mcg::new(11);
        Manager::new_random(&mut rng, &Params::default())
    }

    #[test]
    fn traits_differ_per_individual() {
        // Rolled traits vary across individuals (the raw material of
        // different behavior; decisions themselves run through the net).
        let mut rng = Pcg64Mcg::new(12);
        let rots: Vec<f32> = (0..20).map(|_| Manager::new_random(&mut rng, &Params::default()).rotation).collect();
        assert!(rots.iter().any(|&r| (r - rots[0]).abs() > 1e-6));
    }

    #[test]
    fn insecurity_kills_rotation() {
        let mut m = mgr();
        m.rotation = 0.4;
        m.security = 0.9;
        assert_eq!(m.effective_rotation(), 0.4);
        m.security = 0.2;
        assert_eq!(m.effective_rotation(), 0.2);
    }

    #[test]
    fn security_tracks_results() {
        let mut m = mgr();
        m.security = 0.6;
        m.review_season(4);
        assert!(m.security > 0.6);
        m.review_season(-5);
        assert!(m.security < 0.6);
    }
}
