//! Original domestic-cup knockout system for fm-cli.
//!
//! Inspiration (not code) from the FA Cup / FM domestic cups: a single-match
//! open random draw with no seeding, extra time then penalties to force a
//! winner, a neutral-ground feel for the final, escalating prize money, and
//! lower-league "minnows" entering so giant-killings can happen. All numbers
//! and logic here are our own; pure functions where possible for testing.

use serde::{Deserialize, Serialize};

use rand::seq::SliceRandom;
use rand::Rng;

use crate::model::ClubId;

/// A cup entrant. League clubs reference their real `club_id`; generated
/// minnows use synthetic ids in a high range and carry a fixed strength.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CupTeam {
    pub id: ClubId,
    pub name: String,
    /// Baseline strength used when a side has no live squad (minnows, or a
    /// quick-simulated league club). League clubs override this with a live
    /// squad rating at kickoff.
    pub strength: f32,
    /// True for real league clubs (live-rated); false for synthetic minnows.
    pub league_club: bool,
}

/// One knockout tie in the current round.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CupTie {
    pub home: ClubId,
    pub away: ClubId,
    pub played: bool,
    pub home_goals: u8,
    pub away_goals: u8,
    /// Penalty scores when the tie went to a shootout (0/0 otherwise).
    pub home_pens: u8,
    pub away_pens: u8,
    pub extra_time: bool,
    pub shootout: bool,
}

impl CupTie {
    pub fn winner(&self) -> Option<ClubId> {
        if !self.played {
            return None;
        }
        if self.home_goals > self.away_goals {
            Some(self.home)
        } else if self.away_goals > self.home_goals {
            Some(self.away)
        } else if self.home_pens > self.away_pens {
            Some(self.home)
        } else {
            Some(self.away)
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cup {
    pub name: String,
    /// Survivors entering the current round.
    pub survivors: Vec<CupTeam>,
    /// Ties drawn for the current round.
    pub ties: Vec<CupTie>,
    /// 1-based round number.
    pub round: u16,
    /// Set once the competition is decided.
    pub winner: Option<CupTeam>,
}

impl Cup {
    /// Create a cup and draw the first round. `teams` should be a power of two
    /// (callers pad with minnows); an odd count gives the last team a bye.
    pub fn new(name: &str, teams: Vec<CupTeam>, rng: &mut impl Rng) -> Self {
        let mut cup = Cup {
            name: name.to_string(),
            survivors: teams,
            ties: Vec::new(),
            round: 1,
            winner: None,
        };
        cup.draw_round(rng);
        cup
    }

    /// Draw the current survivors into ties (open random draw, no seeding).
    pub fn draw_round(&mut self, rng: &mut impl Rng) {
        self.ties.clear();
        if self.survivors.len() <= 1 {
            self.winner = self.survivors.first().cloned();
            return;
        }
        let mut order: Vec<usize> = (0..self.survivors.len()).collect();
        order.shuffle(rng);
        let mut i = 0;
        while i + 1 < order.len() {
            self.ties.push(CupTie {
                home: self.survivors[order[i]].id,
                away: self.survivors[order[i + 1]].id,
                played: false,
                home_goals: 0,
                away_goals: 0,
                home_pens: 0,
                away_pens: 0,
                extra_time: false,
                shootout: false,
            });
            i += 2;
        }
        // Odd survivor gets a bye: carried straight into the next round via
        // advance() (its id is not in any tie this round).
    }

    /// The team that received a bye this round, if any.
    pub fn bye(&self) -> Option<&CupTeam> {
        if self.survivors.len() % 2 == 1 {
            // The shuffle put a random team last; but survivors order is stable
            // and draw pairs the shuffled order, so recover the unpaired id by
            // set difference.
            let paired: std::collections::HashSet<ClubId> = self
                .ties
                .iter()
                .flat_map(|t| [t.home, t.away])
                .collect();
            self.survivors.iter().find(|t| !paired.contains(&t.id))
        } else {
            None
        }
    }

    /// Advance winners (plus any bye) into the next round and redraw.
    /// Returns true when the cup is finished (winner set).
    pub fn advance(&mut self, rng: &mut impl Rng) -> bool {
        if self.winner.is_some() {
            return true;
        }
        if self.ties.iter().any(|t| !t.played) {
            return false; // round not complete
        }
        let mut next: Vec<CupTeam> = Vec::new();
        for tie in &self.ties {
            if let Some(w) = tie.winner() {
                if let Some(team) = self.survivors.iter().find(|t| t.id == w) {
                    next.push(team.clone());
                }
            }
        }
        if let Some(b) = self.bye().cloned() {
            next.push(b);
        }
        self.survivors = next;
        self.round += 1;
        if self.survivors.len() == 1 {
            self.winner = self.survivors.first().cloned();
            self.ties.clear();
            return true;
        }
        self.draw_round(rng);
        false
    }

    pub fn team_name(&self, id: ClubId) -> String {
        self.survivors
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.name.clone())
            .unwrap_or_else(|| "?".to_string())
    }

    /// The human's unplayed tie this round, if they are still involved.
    pub fn human_tie_index(&self, human: ClubId) -> Option<usize> {
        self.ties
            .iter()
            .position(|t| !t.played && (t.home == human || t.away == human))
    }
}

/// Name a round by how many teams contest it (survivors before the draw).
pub fn round_name(survivors: usize) -> String {
    match survivors {
        0 | 1 => "Complete".to_string(),
        2 => "Final".to_string(),
        3..=4 => "Semi-final".to_string(),
        5..=8 => "Quarter-final".to_string(),
        n => format!("Round of {}", next_pow2(n)),
    }
}

fn next_pow2(mut n: usize) -> usize {
    let mut p = 1;
    while p < n {
        p <<= 1;
    }
    // Report the bracket size for the round (round up), matching FM's
    // "Round of 32/16" labelling.
    if p == n {
        n
    } else {
        p
    }
}

/// Escalating prize money by how many teams remain (fewer = later = richer).
pub fn prize_for_round(survivors: usize) -> i64 {
    match survivors {
        2 => 8_000_000,       // final
        3..=4 => 3_500_000,   // semi
        5..=8 => 1_800_000,   // quarter
        9..=16 => 900_000,
        17..=32 => 450_000,
        _ => 200_000,
    }
}

/// A penalty shootout that always produces a winner. Best-of-five then sudden
/// death; each kick's success is weighted by the takers' relative strength but
/// stays high (a shootout is a coin-flip with a thumb on the scale).
pub fn shootout(rng: &mut impl Rng, home_strength: f32, away_strength: f32) -> (u8, u8) {
    let total = (home_strength + away_strength).max(1.0);
    let hp = (0.62 + 0.16 * (home_strength / total - 0.5)).clamp(0.55, 0.9);
    let ap = (0.62 + 0.16 * (away_strength / total - 0.5)).clamp(0.55, 0.9);
    let mut h = 0u8;
    let mut a = 0u8;
    // Five kicks each.
    for _ in 0..5 {
        if rng.gen::<f32>() < hp {
            h += 1;
        }
        if rng.gen::<f32>() < ap {
            a += 1;
        }
    }
    // Sudden death until separated.
    while h == a {
        let hs = rng.gen::<f32>() < hp;
        let as_ = rng.gen::<f32>() < ap;
        if hs {
            h += 1;
        }
        if as_ {
            a += 1;
        }
    }
    (h, a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_pcg::Pcg64Mcg;

    fn team(id: ClubId, s: f32) -> CupTeam {
        CupTeam {
            id,
            name: format!("Club {}", id),
            strength: s,
            league_club: true,
        }
    }

    #[test]
    fn draw_pairs_all_teams() {
        let mut rng = Pcg64Mcg::new(1);
        let teams: Vec<CupTeam> = (1..=8).map(|i| team(i, 20.0)).collect();
        let cup = Cup::new("Cup", teams, &mut rng);
        assert_eq!(cup.ties.len(), 4);
        let mut ids: Vec<ClubId> = cup.ties.iter().flat_map(|t| [t.home, t.away]).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn odd_field_gives_one_bye() {
        let mut rng = Pcg64Mcg::new(2);
        let teams: Vec<CupTeam> = (1..=7).map(|i| team(i, 20.0)).collect();
        let cup = Cup::new("Cup", teams, &mut rng);
        assert_eq!(cup.ties.len(), 3);
        assert!(cup.bye().is_some());
    }

    #[test]
    fn full_bracket_produces_one_winner() {
        let mut rng = Pcg64Mcg::new(3);
        let teams: Vec<CupTeam> = (1..=16).map(|i| team(i, 20.0)).collect();
        let mut cup = Cup::new("Cup", teams, &mut rng);
        let mut guard = 0;
        loop {
            for t in &mut cup.ties {
                // Deterministic: home always wins.
                t.played = true;
                t.home_goals = 1;
                t.away_goals = 0;
            }
            if cup.advance(&mut rng) {
                break;
            }
            guard += 1;
            assert!(guard < 10, "bracket did not converge");
        }
        assert!(cup.winner.is_some());
    }

    #[test]
    fn shootout_always_has_a_winner() {
        let mut rng = Pcg64Mcg::new(4);
        for _ in 0..500 {
            let (h, a) = shootout(&mut rng, 25.0, 18.0);
            assert_ne!(h, a, "shootout ended level");
        }
    }

    #[test]
    fn round_names_match_field() {
        assert_eq!(round_name(2), "Final");
        assert_eq!(round_name(4), "Semi-final");
        assert_eq!(round_name(8), "Quarter-final");
        assert_eq!(round_name(16), "Round of 16");
        assert_eq!(round_name(24), "Round of 32");
    }

    #[test]
    fn prize_grows_toward_final() {
        assert!(prize_for_round(2) > prize_for_round(4));
        assert!(prize_for_round(4) > prize_for_round(8));
        assert!(prize_for_round(8) > prize_for_round(16));
    }
}
