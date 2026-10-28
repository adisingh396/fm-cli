//! Original match-engine mathematics for fm-cli.
//!
//! Pure functions (no I/O, no game state) so the numbers are unit-testable.
//! Constants are our own tuning, informed by public 2022/23 Premier League
//! baselines: ~2.7-2.85 goals/game, ~11.5 shots/team-game, ~0.11 xG/shot,
//! ~48% home wins. No FM code or data is used.

use rand::Rng;

use crate::model::{Attributes, Position, Role, Tactic};

/// Shots per team per game at an even matchup.
pub const SHOTS_BASE: f32 = 11.5;
/// Expected goals per shot at an even matchup (kept for reference/tuning).
#[allow(dead_code)]
pub const XG_PER_SHOT_HOME: f32 = 0.135;
#[allow(dead_code)]
pub const XG_PER_SHOT_AWAY: f32 = 0.113;
/// Per-player per-match base yellow probability (~3 cards/game over 22 players).
pub const YELLOW_P: f64 = 0.14;
/// Per-player per-match straight-red probability (~1 red per 6 games).
pub const RED_P: f64 = 0.008;
/// Per-player per-match base injury probability before condition scaling.
pub const INJURY_P: f64 = 0.02;

/// Position suitability of a role: 1.0 natural, 0.85-0.9 competent, 0.65 awkward.
pub fn role_suitability(role: Role, pos: Position) -> f32 {
    match (role, pos) {
        (Role::Goalkeeper | Role::SweeperKeeper, Position::GK) => 1.0,
        (
            Role::CentralDefender
            | Role::BallPlayingDefender
            | Role::FullBack
            | Role::WingBack,
            Position::DEF,
        ) => 1.0,
        (
            Role::Anchor
            | Role::DeepLyingPlaymaker
            | Role::BoxToBox
            | Role::AdvancedPlaymaker
            | Role::Winger
            | Role::InsideForward,
            Position::MID,
        ) => 1.0,
        (
            Role::ShadowStriker
            | Role::TargetMan
            | Role::Poacher
            | Role::FalseNine,
            Position::FWD,
        ) => 1.0,
        // Competent cross-line picks.
        (Role::Winger | Role::InsideForward, Position::FWD) => 0.9,
        (
            Role::TargetMan | Role::Poacher | Role::FullBack | Role::WingBack,
            Position::MID,
        ) => 0.9,
        (Role::CentralDefender | Role::BallPlayingDefender, Position::MID) => 0.85,
        // Anyone in goal, or a keeper outfield, is awkward.
        (_, Position::GK) => 0.65,
        (Role::Goalkeeper | Role::SweeperKeeper, _) => 0.65,
        _ => 0.85,
    }
}

/// Attribute rating (1-99 scale) for a role: a weighted blend of what the role
/// actually asks a player to do. This is what makes tactical choices bite — a
/// False Nine leans on passing/vision/dribbling, a Poacher on finishing/pace,
/// so the same player rates very differently in different roles.
pub fn role_rating(a: &Attributes, role: Role) -> f32 {
    let w = |v: u8| v as f32;
    match role {
        Role::Goalkeeper => w(a.goalkeeping),
        Role::SweeperKeeper => w(a.goalkeeping) * 0.7 + w(a.passing) * 0.2 + w(a.pace) * 0.1,
        Role::CentralDefender => {
            w(a.tackling) * 0.4 + w(a.heading) * 0.25 + w(a.strength) * 0.25 + w(a.composure) * 0.1
        }
        Role::BallPlayingDefender => {
            w(a.tackling) * 0.35 + w(a.passing) * 0.3 + w(a.composure) * 0.2 + w(a.strength) * 0.15
        }
        Role::FullBack => {
            w(a.tackling) * 0.3 + w(a.pace) * 0.25 + w(a.stamina) * 0.2 + w(a.passing) * 0.25
        }
        Role::WingBack => {
            w(a.pace) * 0.3 + w(a.stamina) * 0.25 + w(a.dribbling) * 0.2 + w(a.passing) * 0.25
        }
        Role::Anchor => {
            w(a.tackling) * 0.4 + w(a.strength) * 0.25 + w(a.stamina) * 0.2 + w(a.vision) * 0.15
        }
        Role::DeepLyingPlaymaker => {
            w(a.passing) * 0.4 + w(a.vision) * 0.35 + w(a.composure) * 0.25
        }
        Role::BoxToBox => {
            w(a.stamina) * 0.3 + w(a.strength) * 0.2 + w(a.tackling) * 0.25 + w(a.passing) * 0.25
        }
        Role::Winger => w(a.pace) * 0.35 + w(a.dribbling) * 0.35 + w(a.passing) * 0.3,
        Role::InsideForward => w(a.dribbling) * 0.35 + w(a.shooting) * 0.35 + w(a.pace) * 0.3,
        Role::AdvancedPlaymaker => {
            w(a.passing) * 0.4 + w(a.vision) * 0.35 + w(a.dribbling) * 0.25
        }
        Role::ShadowStriker => w(a.shooting) * 0.4 + w(a.dribbling) * 0.3 + w(a.pace) * 0.3,
        Role::TargetMan => w(a.heading) * 0.35 + w(a.strength) * 0.35 + w(a.shooting) * 0.3,
        Role::Poacher => w(a.shooting) * 0.5 + w(a.composure) * 0.3 + w(a.pace) * 0.2,
        Role::FalseNine => {
            w(a.passing) * 0.3 + w(a.vision) * 0.3 + w(a.dribbling) * 0.25 + w(a.composure) * 0.15
        }
    }
}

/// A player's effectiveness in a role, given the natural position they play.
/// Combines the per-attribute role rating with the position-line suitability,
/// so a striker asked to sit at centre-back keeps their raw quality but loses a
/// chunk to being out of position, while a technical forward slots into a False
/// Nine almost seamlessly.
pub fn role_fit(a: &Attributes, role: Role, natural: Position) -> f32 {
    role_rating(a, role) * role_suitability(role, natural)
}

/// Roles that naturally live on each formation line (GK=0, DEF=1, MID=2, FWD=3).
pub fn roles_for_line(line: usize) -> &'static [Role] {
    match line {
        0 => &[Role::Goalkeeper, Role::SweeperKeeper],
        1 => &[
            Role::CentralDefender,
            Role::BallPlayingDefender,
            Role::FullBack,
            Role::WingBack,
        ],
        2 => &[
            Role::Anchor,
            Role::DeepLyingPlaymaker,
            Role::BoxToBox,
            Role::Winger,
            Role::AdvancedPlaymaker,
        ],
        _ => &[
            Role::InsideForward,
            Role::ShadowStriker,
            Role::TargetMan,
            Role::Poacher,
            Role::FalseNine,
        ],
    }
}

/// Formation line index for a natural position (GK=0, DEF=1, MID=2, FWD=3).
pub fn line_of(pos: Position) -> usize {
    match pos {
        Position::GK => 0,
        Position::DEF => 1,
        Position::MID => 2,
        Position::FWD => 3,
    }
}

/// Best-fitting role for a player on a given line, and its fit score. A
/// midfielder pushed up front resolves to False Nine or Shadow Striker rather
/// than Poacher, because their attributes suit the creative roles better.
pub fn best_role_for(a: &Attributes, pos: Position, line: usize) -> (Role, f32) {
    let mut best = (roles_for_line(line)[0], 0.0f32);
    for &role in roles_for_line(line) {
        let mut fit = role_fit(a, role, pos);
        if pos == Position::DEF && line == 1 {
            // A full-back is not merely a shorter centre-back. Pace, stamina
            // and carrying/crossing ability identify the wide defender profile;
            // heading and strength identify the central profile. This keeps a
            // real LB such as Nuno Mendes out of the centre-back leaderboard.
            let wide_signal = (a.pace as f32 + a.stamina as f32 + a.dribbling as f32
                - a.heading as f32 - a.strength as f32)
                / 35.0;
            match role {
                Role::FullBack | Role::WingBack => fit += wide_signal.max(0.0),
                Role::CentralDefender | Role::BallPlayingDefender => {
                    fit -= wide_signal.max(0.0) * 0.35
                }
                _ => {}
            }
        }
        if fit > best.1 {
            best = (role, fit);
        }
    }
    best
}

/// Combined (attack, defense) multipliers for a tactic.
pub fn team_multipliers(t: &Tactic) -> (f32, f32) {
    let (m_atk, m_def) = t.mentality.modifiers();
    let (i_atk, i_def) = t.instructions.modifiers();
    (
        (m_atk * i_atk).clamp(0.7, 1.4),
        (m_def * i_def).clamp(0.7, 1.4),
    )
}

/// Expected-goals lambdas for (home, away) from pre-match strengths.
/// An even matchup yields ~1.55 / ~1.30 (2.85 total, home edge baked in).
/// This function owns home advantage; callers must NOT add their own.
pub fn goal_lambdas(home_strength: f32, away_strength: f32) -> (f32, f32) {
    let ratio = (home_strength.max(1.0) / away_strength.max(1.0)).clamp(0.35, 2.8);
    let h = (1.55 * ratio.powf(0.9)).clamp(0.2, 3.6);
    let a = (1.30 * ratio.powf(-0.9)).clamp(0.15, 3.2);
    (h, a)
}

fn poisson<R: Rng + ?Sized>(rng: &mut R, lambda: f32) -> u32 {
    if lambda <= 0.0 {
        return 0;
    }
    let l = (-lambda).exp();
    let mut k = 0u32;
    let mut p = 1.0f32;
    loop {
        k += 1;
        p *= rng.gen::<f32>();
        if p <= l {
            break;
        }
        if k > 200 {
            break;
        }
    }
    k - 1
}

/// Draw one shot's chance quality (its xG), FM-style: mostly speculative
/// efforts, some decent looks, occasional clear-cut chances, rare penalties.
/// `sf` (~0.5..2.3) is the side's attacking strength factor; better attacks
/// carve out slightly better chances, not just more of them.
fn shot_quality<R: Rng + ?Sized>(rng: &mut R, sf: f32) -> f32 {
    let roll = rng.gen::<f32>();
    let base = if roll < 0.03 {
        0.76 // penalty
    } else if roll < 0.16 {
        rng.gen_range(0.28..0.45) // clear-cut chance
    } else if roll < 0.45 {
        rng.gen_range(0.10..0.22) // decent look
    } else {
        rng.gen_range(0.02..0.09) // speculative / long shot
    };
    let edge = 0.85 + 0.30 * (sf - 1.0).clamp(-0.5, 0.9);
    (base * edge).clamp(0.02, 0.95)
}

/// Convert a side's shots into (goals, xG). Each shot is an independent
/// Bernoulli trial on its own quality, so goals track chance quality with
/// realistic (binomial) variance — a low-xG side rarely bags a hatful, and a
/// high-xG side rarely blanks, but upsets still happen.
fn convert_shots<R: Rng + ?Sized>(rng: &mut R, shots: u32, sf: f32) -> (u32, f32) {
    let mut goals = 0u32;
    let mut xg = 0.0f32;
    for _ in 0..shots {
        let q = shot_quality(rng, sf);
        xg += q;
        if rng.gen::<f32>() < q {
            goals += 1;
        }
    }
    (goals, xg)
}

/// Simulate shots, xG and goals for both sides.
/// Returns (home_goals, away_goals, home_shots, away_shots, home_xg, away_xg).
/// Goals come from per-shot conversion so the score and xG stay consistent.
pub fn simulate_score<R: Rng + ?Sized>(
    rng: &mut R,
    home_strength: f32,
    away_strength: f32,
) -> (u32, u32, u32, u32, f32, f32) {
    let (hl, al) = goal_lambdas(home_strength, away_strength);
    let shots_h = poisson(rng, (SHOTS_BASE * (hl / 1.55)).clamp(2.0, 24.0));
    let shots_a = poisson(rng, (SHOTS_BASE * (al / 1.30)).clamp(2.0, 24.0));
    let (gh, xg_h) = convert_shots(rng, shots_h, hl / 1.55);
    let (ga, xg_a) = convert_shots(rng, shots_a, al / 1.30);
    (gh, ga, shots_h, shots_a, xg_h, xg_a)
}

/// Post-match rating on the 1-10 scale: 6.7 baseline with event deltas.
/// `card`: 0 none, 1 yellow, 2+ red.
pub fn rating(
    goals: u8,
    assists: u8,
    clean_sheet: bool,
    is_def_unit: bool,
    errors: u8,
    card: u8,
) -> f32 {
    let mut r = 6.7 + goals as f32 * 1.0 + assists as f32 * 0.7;
    if clean_sheet && is_def_unit {
        r += 0.4;
    }
    r -= errors as f32 * 0.9;
    if card == 1 {
        r -= 0.5;
    } else if card >= 2 {
        r -= 1.5;
    }
    r.clamp(5.0, 10.0)
}

/// Per-player injury probability for one match.
pub fn injury_probability(condition: u8, sharpness: u8, pressing: u8) -> f64 {
    let mut p = INJURY_P;
    if condition < 70 {
        p *= 3.0;
    } else if condition < 85 {
        p *= 1.8;
    }
    if sharpness < 80 {
        p *= 1.6;
    }
    if pressing > 65 {
        p *= 1.5;
    }
    p.min(0.25)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_pcg::Pcg64Mcg;

    #[test]
    fn even_matchup_lambdas_sum_near_285() {
        let (h, a) = goal_lambdas(50.0, 50.0);
        assert!((h - 1.55).abs() < 0.01);
        assert!((a - 1.30).abs() < 0.01);
    }

    #[test]
    fn stronger_side_scores_more_expected() {
        let (h, a) = goal_lambdas(80.0, 40.0);
        assert!(h > a);
        let (h2, a2) = goal_lambdas(40.0, 80.0);
        assert!(a2 > h2);
    }

    #[test]
    fn lambdas_stay_within_caps() {
        let (h, a) = goal_lambdas(200.0, 1.0);
        assert!(h <= 3.6 && a >= 0.15);
    }

    #[test]
    fn score_simulation_ranges_sane() {
        let mut rng = Pcg64Mcg::new(7);
        for _ in 0..200 {
            let (gh, ga, sh, sa, xgh, xga) = simulate_score(&mut rng, 50.0, 50.0);
            assert!((2..=24).contains(&sh) && (2..=24).contains(&sa));
            assert!(xgh >= 0.0 && xga >= 0.0);
            assert!(gh <= 12 && ga <= 12);
        }
    }

    #[test]
    fn goals_track_xg_over_many_games() {
        // Per-shot conversion should keep total goals close to total xG
        // across a sample (finishing regresses to xG), unlike overdispersed
        // independent Poisson on the xG total.
        let mut rng = Pcg64Mcg::new(11);
        let mut goals = 0u32;
        let mut xg = 0.0f32;
        for _ in 0..2000 {
            let (gh, ga, _, _, xgh, xga) = simulate_score(&mut rng, 55.0, 45.0);
            goals += gh + ga;
            xg += xgh + xga;
        }
        let ratio = goals as f32 / xg;
        assert!(
            (0.9..=1.1).contains(&ratio),
            "goals/xg ratio {:.3} drifted from 1.0 (goals {}, xg {:.1})",
            ratio,
            goals,
            xg
        );
    }

    #[test]
    fn rating_events_match_baseline() {
        assert!((rating(0, 0, false, false, 0, 0) - 6.7).abs() < 0.01);
        assert!((rating(1, 0, false, false, 0, 0) - 7.7).abs() < 0.01);
        assert!((rating(0, 0, true, true, 0, 0) - 7.1).abs() < 0.01);
        assert!((rating(0, 0, false, false, 0, 2) - 5.2).abs() < 0.01);
        assert_eq!(rating(0, 0, false, false, 5, 2), 5.0); // clamped
        assert_eq!(rating(5, 3, true, true, 0, 0), 10.0); // clamped
    }

    #[test]
    fn suitability_values() {
        assert_eq!(role_suitability(Role::Goalkeeper, Position::GK), 1.0);
        assert_eq!(role_suitability(Role::Poacher, Position::GK), 0.65);
        assert_eq!(role_suitability(Role::Goalkeeper, Position::FWD), 0.65);
        assert_eq!(role_suitability(Role::Winger, Position::FWD), 0.9);
        assert_eq!(role_suitability(Role::Poacher, Position::FWD), 1.0);
    }

    #[test]
    fn injury_probability_monotonic() {
        let fresh = injury_probability(100, 100, 50);
        let tired = injury_probability(60, 70, 80);
        assert!(tired > fresh);
        assert!(fresh > 0.0 && tired <= 0.25);
    }

    fn attrs(overrides: &[(&str, u8)]) -> Attributes {
        let mut a = Attributes {
            pace: 50,
            shooting: 50,
            passing: 50,
            dribbling: 50,
            tackling: 50,
            heading: 50,
            strength: 50,
            stamina: 50,
            vision: 50,
            composure: 50,
            goalkeeping: 50,
            first_touch: 50,
        };
        for (k, v) in overrides {
            match *k {
                "pace" => a.pace = *v,
                "shooting" => a.shooting = *v,
                "passing" => a.passing = *v,
                "dribbling" => a.dribbling = *v,
                "tackling" => a.tackling = *v,
                "heading" => a.heading = *v,
                "strength" => a.strength = *v,
                "stamina" => a.stamina = *v,
                "vision" => a.vision = *v,
                "composure" => a.composure = *v,
                "goalkeeping" => a.goalkeeping = *v,
                "first_touch" => a.first_touch = *v,
                _ => {}
            }
        }
        a
    }

    #[test]
    fn role_rating_reflects_the_role() {
        // A finisher rates as a Poacher; a creator rates as a False Nine.
        let finisher = attrs(&[("shooting", 95), ("pace", 90), ("composure", 85)]);
        let creator = attrs(&[("passing", 95), ("vision", 95), ("dribbling", 90)]);
        assert!(role_rating(&finisher, Role::Poacher) > role_rating(&finisher, Role::FalseNine));
        assert!(role_rating(&creator, Role::FalseNine) > role_rating(&creator, Role::Poacher));
    }

    #[test]
    fn best_role_picks_false_nine_for_a_creative_forward() {
        // A technical forward pushed up top should resolve to a False Nine,
        // not a Poacher — the whole point of the tactical experiment.
        let creator = attrs(&[("passing", 92), ("vision", 92), ("dribbling", 88), ("shooting", 55)]);
        let (role, _) = best_role_for(&creator, Position::FWD, 3);
        assert_eq!(role, Role::FalseNine);

        let finisher = attrs(&[("shooting", 94), ("pace", 88), ("composure", 82), ("passing", 50)]);
        let (role, _) = best_role_for(&finisher, Position::FWD, 3);
        assert_eq!(role, Role::Poacher);
    }

    #[test]
    fn out_of_position_costs_fit() {
        // Same player, same role, but playing out of their natural line is worse.
        let a = attrs(&[("tackling", 80), ("heading", 80), ("strength", 80)]);
        let natural = role_fit(&a, Role::CentralDefender, Position::DEF);
        let out = role_fit(&a, Role::CentralDefender, Position::FWD);
        assert!(natural > out);
    }
}
