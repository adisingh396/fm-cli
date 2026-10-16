//! Evolutionary calibration ("train the AI"): learns simulation parameters
//! from real-world football, instead of hand-setting them.
//!
//! Honest accounting of what this is and isn't:
//! - It IS a black-box optimizer (evolution strategies): a population of
//!   parameter vectors each simulates seasons; fitness = closeness of the
//!   simulated decade statistics to real-world targets; elites survive and
//!   mutate. The transfer policy network's weights evolve the same way.
//! - It ISN'T gradient descent or deep RL: the simulator is not
//!   differentiable, so there is no backprop through matches. ES is the
//!   correct tool for non-differentiable simulators.
//! - Randomness (seeded RNG) REMAINS in matches — football has variance. What
//!   gets learned are decision policies and rates, so trends converge to
//!   reality across seasons instead of depending on one person's constants.
//!
//! Real-world targets are approximate 2015-2025 norms (Big-5 leagues, UCL,
//! Ballon d'Or, transfer market): champion ~88±7 pts (38-game), golden boot
//! ~30±5, UCL winners almost always elite clubs, Ballon winners score 20+
//! for contending teams, 4-10 big-fee deals per window, record fees €100-300m.

use serde::{Deserialize, Serialize};
use rand::Rng;

use crate::data::Database;
use crate::net;

/// Standard-normal sample (Box-Muller).
fn gauss(rng: &mut impl Rng) -> f32 {
    let mut u1: f32 = 0.0;
    while u1 <= 0.0 {
        u1 = rng.gen::<f32>();
    }
    let u2: f32 = rng.gen::<f32>();
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f32::consts::PI * u2).cos()
}

/// All tunable simulation constants. Every field is f32; integral ones are
/// rounded at use. Defaults reproduce the hand-tuned behavior.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Params {
    pub swing_league_lo: f32,
    pub swing_league_hi: f32,
    pub swing_cup_lo: f32,
    pub swing_cup_hi: f32,
    pub strength_exp: f32,
    pub blend_squad: f32,
    pub zero_shift: f32,
    pub rep_slope: f32,
    pub rep_base: f32,
    pub morale_win: f32,
    pub morale_loss: f32,
    pub fatigue_match: f32,
    pub fatigue_rest: f32,
    pub knowledge: f32,
    pub open_pct: f32,
    pub sell_base: f32,
    pub sell_distress: f32,
    pub req_discount: f32,
    pub min_improve: f32,
    pub gap_starter: f32,
    pub gap_clause: f32,
    pub gap_surplus: f32,
    pub log_fee: f32,
    pub agent_fee: f32,
    pub budget_ratio: f32,
    pub wage_ratio: f32,
    pub inflation: f32,
    pub dev_mid_p: f32,
    pub clause_mult: f32,
    pub drift_title: f32,
    pub drift_topq: f32,
    pub drift_bot: f32,
    pub mean_rev: f32,
    pub sack_worst_p: f32,
    pub sack_elite_p: f32,
    pub elite_rep: f32,
    pub elite_pos: f32,
    pub q_init_lo: f32,
    pub q_init_hi: f32,
    pub q_fresh_lo: f32,
    pub q_fresh_hi: f32,
    pub q_regress: f32,
    pub rot_dip: f32,
    pub policy: Vec<f32>,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            swing_league_lo: 0.87,
            swing_league_hi: 1.13,
            swing_cup_lo: 0.88,
            swing_cup_hi: 1.12,
            strength_exp: 1.7,
            // Squad-driven strength only: reputation must never buy goals
            // directly — giants win through better elevens, not badges.
            blend_squad: 1.0,
            zero_shift: 16.0,
            rep_slope: 0.26,
            rep_base: 10.0,
            morale_win: 2.0,
            morale_loss: 3.0,
            fatigue_match: 22.0,
            fatigue_rest: 25.0,
            knowledge: 0.40,
            open_pct: 0.9,
            sell_base: 0.95,
            sell_distress: 0.85,
            req_discount: 0.9,
            min_improve: 2.0,
            gap_starter: 8.0,
            gap_clause: 5.0,
            gap_surplus: 12.0,
            log_fee: 45.0,
            agent_fee: 0.1,
            budget_ratio: 0.25,
            wage_ratio: 0.5,
            inflation: 1.04,
            dev_mid_p: 0.5,
            clause_mult: 2.2,
            drift_title: 2.5,
            drift_topq: 1.0,
            drift_bot: -2.0,
            mean_rev: 0.06,
            sack_worst_p: 0.6,
            sack_elite_p: 0.5,
            elite_rep: 88.0,
            elite_pos: 2.0,
            q_init_lo: 0.9,
            q_init_hi: 1.1,
            q_fresh_lo: 0.85,
            q_fresh_hi: 1.15,
            q_regress: 0.92,
            rot_dip: 0.30,
            policy: net::default_weights(),
        }
    }
}

impl Params {
    pub fn load() -> Option<Self> {
        std::fs::read_to_string("tuned_params.json")
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .filter(|p: &Self| p.policy.len() == net::NET_WEIGHTS)
    }

    pub fn save(&self) -> std::io::Result<()> {
        std::fs::write("tuned_params.json", serde_json::to_string_pretty(self).unwrap())
    }

    /// Clamp every field to sane ranges (keeps mutants realistic).
    pub fn clamp_all(&mut self) {
        let c = |v: &mut f32, lo: f32, hi: f32| *v = v.clamp(lo, hi);
        c(&mut self.swing_league_lo, 0.7, 1.0);
        c(&mut self.swing_league_hi, 1.0, 1.3);
        c(&mut self.swing_cup_lo, 0.7, 1.0);
        c(&mut self.swing_cup_hi, 1.0, 1.3);
        c(&mut self.strength_exp, 1.0, 2.5);
        c(&mut self.blend_squad, 0.8, 1.0);
        c(&mut self.zero_shift, 8.0, 24.0);
        c(&mut self.rep_slope, 0.1, 0.5);
        c(&mut self.rep_base, 0.0, 20.0);
        c(&mut self.morale_win, 0.0, 6.0);
        c(&mut self.morale_loss, 0.0, 8.0);
        c(&mut self.fatigue_match, 5.0, 40.0);
        c(&mut self.fatigue_rest, 5.0, 50.0);
        c(&mut self.knowledge, 0.05, 1.0);
        c(&mut self.open_pct, 0.5, 1.0);
        c(&mut self.sell_base, 0.7, 1.0);
        c(&mut self.sell_distress, 0.5, 1.0);
        c(&mut self.req_discount, 0.5, 1.0);
        c(&mut self.min_improve, 0.0, 5.0);
        c(&mut self.gap_starter, 0.0, 20.0);
        c(&mut self.gap_clause, 0.0, 15.0);
        c(&mut self.gap_surplus, 0.0, 25.0);
        c(&mut self.log_fee, 20.0, 100.0);
        c(&mut self.agent_fee, 0.0, 0.3);
        c(&mut self.budget_ratio, 0.05, 0.6);
        c(&mut self.wage_ratio, 0.2, 1.0);
        c(&mut self.inflation, 1.0, 1.15);
        c(&mut self.dev_mid_p, 0.0, 1.0);
        c(&mut self.clause_mult, 1.0, 5.0);
        c(&mut self.drift_title, 0.0, 6.0);
        c(&mut self.drift_topq, 0.0, 3.0);
        c(&mut self.drift_bot, -5.0, 0.0);
        c(&mut self.mean_rev, 0.0, 0.2);
        c(&mut self.sack_worst_p, 0.0, 1.0);
        c(&mut self.sack_elite_p, 0.0, 1.0);
        c(&mut self.elite_rep, 80.0, 95.0);
        c(&mut self.elite_pos, 1.0, 4.0);
        c(&mut self.q_init_lo, 0.7, 1.0);
        c(&mut self.q_init_hi, 1.0, 1.3);
        c(&mut self.q_fresh_lo, 0.7, 1.0);
        c(&mut self.q_fresh_hi, 1.0, 1.3);
        c(&mut self.q_regress, 0.8, 0.99);
        c(&mut self.rot_dip, 0.0, 0.5);
        for x in &mut self.policy {
            *x = x.clamp(-3.0, 3.0);
        }
    }

    /// Gaussian mutation of scalars (relative sigma) + policy weights.
    pub fn mutated(&self, rng: &mut impl rand::Rng, sigma: f32) -> Self {
        let mut p = self.clone();
        macro_rules! mutf {
            ($f:ident, $s:expr) => {
                p.$f += gauss(rng) * sigma * $s;
            };
        }
        mutf!(swing_league_lo, 0.05);
        mutf!(swing_league_hi, 0.05);
        mutf!(swing_cup_lo, 0.05);
        mutf!(swing_cup_hi, 0.05);
        mutf!(strength_exp, 0.15);
        mutf!(blend_squad, 0.08);
        mutf!(zero_shift, 1.5);
        mutf!(rep_slope, 0.03);
        mutf!(rep_base, 1.5);
        mutf!(morale_win, 0.5);
        mutf!(morale_loss, 0.5);
        mutf!(fatigue_match, 3.0);
        mutf!(fatigue_rest, 3.0);
        mutf!(knowledge, 0.08);
        mutf!(open_pct, 0.05);
        mutf!(sell_base, 0.04);
        mutf!(sell_distress, 0.05);
        mutf!(req_discount, 0.05);
        mutf!(min_improve, 0.4);
        mutf!(gap_starter, 1.5);
        mutf!(gap_clause, 1.0);
        mutf!(gap_surplus, 2.0);
        mutf!(log_fee, 6.0);
        mutf!(agent_fee, 0.02);
        mutf!(budget_ratio, 0.04);
        mutf!(wage_ratio, 0.08);
        mutf!(inflation, 0.01);
        mutf!(dev_mid_p, 0.1);
        mutf!(clause_mult, 0.3);
        mutf!(drift_title, 0.4);
        mutf!(drift_topq, 0.2);
        mutf!(drift_bot, 0.4);
        mutf!(mean_rev, 0.015);
        mutf!(sack_worst_p, 0.1);
        mutf!(sack_elite_p, 0.1);
        mutf!(elite_rep, 1.5);
        mutf!(elite_pos, 0.4);
        mutf!(q_init_lo, 0.03);
        mutf!(q_init_hi, 0.03);
        mutf!(q_fresh_lo, 0.04);
        mutf!(q_fresh_hi, 0.04);
        mutf!(q_regress, 0.015);
        mutf!(rot_dip, 0.05);
        for x in &mut p.policy {
            *x += gauss(rng) * 0.15;
        }
        p.clamp_all();
        // keep ranges ordered
        if p.swing_league_lo > p.swing_league_hi {
            std::mem::swap(&mut p.swing_league_lo, &mut p.swing_league_hi);
        }
        if p.swing_cup_lo > p.swing_cup_hi {
            std::mem::swap(&mut p.swing_cup_lo, &mut p.swing_cup_hi);
        }
        if p.q_init_lo > p.q_init_hi {
            std::mem::swap(&mut p.q_init_lo, &mut p.q_init_hi);
        }
        if p.q_fresh_lo > p.q_fresh_hi {
            std::mem::swap(&mut p.q_fresh_lo, &mut p.q_fresh_hi);
        }
        if p.sell_distress > p.sell_base {
            std::mem::swap(&mut p.sell_distress, &mut p.sell_base);
        }
        p
    }
}

/// One season of headline outcomes for fitness evaluation.
#[derive(Clone, Debug, Default)]
pub struct SeasonSample {
    pub champ_pts: Vec<u16>,
    pub champs: Vec<(String, String)>, // (league, club)
    pub boot: u16,
    pub ucl_rep: u8,
    pub ballon_g: u16,
    pub ballon_elite: bool,
    pub deals: usize,
    pub max_fee: i64,
}

fn mean(xs: &[f32]) -> f32 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().sum::<f32>() / xs.len() as f32
}

fn sd(xs: &[f32], m: f32) -> f32 {
    if xs.is_empty() {
        return 0.0;
    }
    (xs.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / xs.len() as f32).sqrt()
}

/// Fitness (lower = more realistic): weighted distance to real-world norms.
pub fn loss(samples: &[SeasonSample]) -> f32 {
    if samples.is_empty() {
        return 1e9;
    }
    let pts: Vec<f32> = samples.iter().flat_map(|s| s.champ_pts.iter().map(|&p| p as f32)).collect();
    let pm = mean(&pts);
    let psd = sd(&pts, pm);
    let boots: Vec<f32> = samples.iter().map(|s| s.boot as f32).collect();
    let bm = mean(&boots);
    let mut l = 0.0;
    l += ((pm - 88.0) / 7.0).powi(2); // champion points ~88±7
    l += ((psd - 7.0) / 4.0).powi(2); // spread across leagues
    l += ((bm - 30.0) / 5.0).powi(2); // golden boot ~30±5
    let over52 = samples.iter().filter(|s| s.boot > 52).count() as f32;
    l += over52 * 4.0; // freak boots heavily penalized
    let ucl_elite = samples.iter().filter(|s| s.ucl_rep >= 80).count() as f32 / samples.len() as f32;
    l += 2.0 * (1.0 - ucl_elite).powi(2); // UCL winners almost always elite
    let belite = samples.iter().filter(|s| s.ballon_elite).count() as f32 / samples.len() as f32;
    l += 1.5 * (0.9 - belite).powi(2); // Ballon winners score 20+ for contenders
    let deals: Vec<f32> = samples.iter().map(|s| s.deals as f32).collect();
    let dm = mean(&deals);
    l += if dm < 4.0 {
        (4.0 - dm).powi(2) / 4.0
    } else if dm > 10.0 {
        (dm - 10.0).powi(2) / 9.0
    } else {
        0.0
    };
    let fees: Vec<f32> = samples.iter().map(|s| s.max_fee as f32).collect();
    let fm = mean(&fees);
    l += if fm < 80.0 {
        (80.0 - fm).powi(2) / 6400.0
    } else if fm > 320.0 {
        (fm - 320.0).powi(2) / 40000.0
    } else {
        0.0
    };
    // Title concentration per league (HHI over the window): neither frozen
    // monopoly (1.0) nor pure lottery (~1/n). Target band 0.25-0.6.
    use std::collections::HashMap;
    let mut leagues: HashMap<&str, HashMap<&str, usize>> = HashMap::new();
    for s in samples {
        for (l, c) in &s.champs {
            *leagues.entry(l).or_default().entry(c).or_insert(0) += 1;
        }
    }
    for (_, counts) in &leagues {
        let total: usize = counts.values().sum();
        if total == 0 {
            continue;
        }
        let hhi: f32 = counts.values().map(|&c| (c as f32 / total as f32).powi(2)).sum();
        l += if hhi > 0.6 {
            (hhi - 0.6).powi(2) * 4.0
        } else if hhi < 0.2 {
            (0.2 - hhi).powi(2) * 4.0
        } else {
            0.0
        };
    }
    l
}

/// Evolution-strategies calibration: population of parameter vectors, each
/// evaluated over `eval_seasons` seasons; elites survive and mutate.
/// Progress (generation, best loss) is printed; best vector returned.
pub fn calibrate(
    db: &Database,
    generations: usize,
    pop_size: usize,
    eval_seasons: u32,
    seed: u64,
) -> Params {
    use rand::SeedableRng;
    use rand_pcg::Pcg64Mcg;
    let mut rng = Pcg64Mcg::seed_from_u64(seed);
    let mut pop: Vec<Params> = (0..pop_size)
        .map(|_| Params::default().mutated(&mut rng, 1.0))
        .collect();
    pop[0] = Params::default(); // keep the hand-tuned incumbent in the race
    let mut best = Params::default();
    let mut best_loss = f32::INFINITY;
    for g in 0..generations {
        let mut scored: Vec<(f32, usize)> = Vec::new();
        for (i, p) in pop.iter().enumerate() {
            let samples = crate::season::eval_seasons(db, p, eval_seasons, seed + g as u64 * 100 + i as u64);
            let l = loss(&samples);
            scored.push((l, i));
            if l < best_loss {
                best_loss = l;
                best = p.clone();
            }
        }
        scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        println!(
            "  gen {}: best={:.3} median={:.3}",
            g + 1,
            scored[0].0,
            scored[scored.len() / 2].0
        );
        let elite_n = (pop_size / 3).max(1);
        let elites: Vec<Params> = scored.iter().take(elite_n).map(|&(_, i)| pop[i].clone()).collect();
        let mut next: Vec<Params> = vec![best.clone()];
        while next.len() < pop_size {
            let parent = &elites[(rng.gen::<usize>()) % elites.len()];
            let sigma = if g < generations / 2 { 1.0 } else { 0.4 };
            next.push(parent.mutated(&mut rng, sigma));
        }
        pop = next;
    }
    println!("  final best loss: {:.3}", best_loss);
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_pcg::Pcg64Mcg;

    #[test]
    fn default_params_sane() {
        let p = Params::default();
        assert!(p.swing_league_lo < p.swing_league_hi);
        assert_eq!(p.policy.len(), net::NET_WEIGHTS);
    }

    #[test]
    fn json_roundtrip() {
        let p = Params::default();
        let s = serde_json::to_string(&p).unwrap();
        let q: Params = serde_json::from_str(&s).unwrap();
        assert_eq!(q.strength_exp, p.strength_exp);
        assert_eq!(q.policy.len(), net::NET_WEIGHTS);
    }

    #[test]
    fn loss_prefers_realistic() {
        let good = SeasonSample {
            champ_pts: vec![88, 91, 85, 76, 84],
            champs: vec![("A".into(), "X".into()), ("A".into(), "Y".into())],
            boot: 30,
            ucl_rep: 95,
            ballon_g: 28,
            ballon_elite: true,
            deals: 6,
            max_fee: 150,
        };
        let bad = SeasonSample {
            champ_pts: vec![112, 110],
            champs: vec![("A".into(), "X".into()), ("A".into(), "X".into())],
            boot: 63,
            ucl_rep: 60,
            ballon_g: 8,
            ballon_elite: false,
            deals: 0,
            max_fee: 20,
        };
        assert!(loss(&[good]) < loss(&[bad]));
    }

    #[test]
    fn mutation_stays_in_bounds() {
        use rand::SeedableRng;
        let mut rng = Pcg64Mcg::new(9);
        let p = Params::default().mutated(&mut rng, 2.0);
        assert!((0.0..=1.0).contains(&p.knowledge));
        assert!(p.policy.iter().all(|x| (-3.0..=3.0).contains(x)));
    }
}
