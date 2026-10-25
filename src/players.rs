//! Individual player agents: motives, memory, and treatment.
//!
//! Every simulated footballer carries a `Mind` alongside their attributes:
//! ambition (wants the big stage), loyalty (tolerates setbacks), greed
//! (wage demands), professionalism (floor under bad spells). The mind remembers
//! how the manager treats them — expected vs actual game time — and turns
//! unhappiness into transfer requests that the market then resolves.
//!
//! This is a weighted-threshold decision model, not a neural network: small,
//! legible, and fully deterministic per seed.

use rand::Rng;

/// How many league apps a squad rank should expect per 38-game season,
/// expressed as a share of matches played so far.
fn pace_for_rank(rank: usize) -> f32 {
    if rank < 11 {
        0.95 // nailed-on starter
    } else if rank < 18 {
        0.55 // rotation option
    } else {
        0.25 // depth / prospect
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PlayerMind {
    pub ambition: f32,
    pub loyalty: f32,
    pub greed: f32,
    pub professionalism: f32,
    pub unhappy: bool,
    pub requested: bool,
}

impl PlayerMind {
    pub fn new(rng: &mut impl Rng) -> Self {
        PlayerMind {
            ambition: rng.gen_range(0.0..1.0),
            loyalty: rng.gen_range(0.0..1.0),
            greed: rng.gen_range(0.0..1.0),
            professionalism: rng.gen_range(0.0..1.0),
            unhappy: false,
            requested: false,
        }
    }

    /// Weekly treatment check. `apps` is league appearances so far, `played`
    /// is team matches so far, `rank` is squad rank by ability (0 = best).
    /// Falls short badly → unhappy + morale hit; far short + disloyal →
    /// transfer request stands until the market sells them. Meeting or beating
    /// expectation lifts morale. Professionals never sink as far.
    pub fn evaluate(&mut self, apps: u16, played: u16, rank: usize, age: u8, morale: &mut u8) {
        // A teenager is a prospect, not a snubbed starter. Sitting behind a
        // 90-rated star doesn't judge him against the first XI yet, so he
        // neither sulks nor demands a move — he waits and develops. Without
        // this, the best academies drove their own kids to transfer requests
        // the moment they blocked a debut, and no prospect could ever break in.
        let expected = played as f32 * if age <= 19 { 0.15 } else { pace_for_rank(rank) };
        let shortfall = expected - apps as f32;
        if shortfall >= 6.0 {
            self.unhappy = true;
            *morale = morale.saturating_sub(4).max(40);
            if shortfall >= 10.0 && self.loyalty < 0.40 && age >= 21 {
                self.requested = true;
            }
        } else if shortfall <= 0.0 {
            self.unhappy = false;
            *morale = (*morale + 2).min(85);
        }
        let floor = (35.0 + self.professionalism * 20.0) as u8;
        if *morale < floor {
            *morale = floor;
        }
    }

    /// Morale drift target: the disgruntled sit at 45, everyone else at 65.
    pub fn morale_target(&self) -> u8 {
        if self.requested || self.unhappy {
            45
        } else {
            65
        }
    }

    /// Wage multiplier applied when signing: greedy players cost more.
    pub fn wage_mult(&self) -> f32 {
        1.0 + self.greed * 0.3
    }

    /// Ambitious players want European football; small clubs struggle to keep
    /// them once they outperform (used by the market as unrest pressure).
    pub fn wants_bigger_stage(&self) -> bool {
        self.ambition > 0.75 && self.unhappy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_pcg::Pcg64Mcg;

    #[test]
    fn neglected_starter_gets_unhappy() {
        let mut rng = Pcg64Mcg::new(3);
        let mut m = PlayerMind::new(&mut rng);
        m.loyalty = 0.9; // loyal: unhappy but no request
        let mut morale = 70u8;
        m.evaluate(0, 10, 5, 22, &mut morale);
        assert!(m.unhappy);
        assert!(!m.requested);
        assert!(morale < 70);
    }

    #[test]
    fn neglected_disloyal_requests_move() {
        let mut rng = Pcg64Mcg::new(4);
        let mut m = PlayerMind::new(&mut rng);
        m.loyalty = 0.1;
        let mut morale = 70u8;
        m.evaluate(0, 12, 3, 22, &mut morale);
        assert!(m.requested);
        assert_eq!(m.morale_target(), 45);
    }

    #[test]
    fn playing_meets_expectation() {
        let mut rng = Pcg64Mcg::new(5);
        let mut m = PlayerMind::new(&mut rng);
        m.unhappy = true;
        let mut morale = 60u8;
        m.evaluate(10, 10, 2, 25, &mut morale);
        assert!(!m.unhappy);
        assert!(morale > 60);
    }

    #[test]
    fn professionals_have_higher_floor() {
        let mut rng = Pcg64Mcg::new(6);
        let mut m = PlayerMind::new(&mut rng);
        m.professionalism = 1.0;
        m.loyalty = 0.9;
        let mut morale = 70u8;
        for _ in 0..10 {
            m.evaluate(0, 30, 0, 25, &mut morale);
        }
        assert!(morale >= 50);
    }

    #[test]
    fn prospects_are_patient() {
        // A teenager blocking behind a star neither sulks nor hands in a
        // request — he's a prospect, not a neglected starter. The identical
        // seat for a 25-year-old is a different story.
        let mut rng = Pcg64Mcg::new(7);
        let mut m = PlayerMind::new(&mut rng);
        m.loyalty = 0.1; // would ask out the moment he was genuinely upset
        let mut morale = 70u8;
        m.evaluate(0, 30, 20, 18, &mut morale);
        assert!(!m.unhappy);
        assert!(!m.requested);
        m.evaluate(0, 30, 20, 25, &mut morale);
        assert!(m.unhappy);
        assert!(morale < 70);
    }
}
