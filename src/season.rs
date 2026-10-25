//! Season & multi-year sandbox: simulate the Big 5 leagues, the Champions
//! League, the Europa League and international tournaments (World Cup every
//! 4 years, Euros in between), for one season or a whole era, with aging,
//! attribute-based development, retirements and youth regens.
//!
//! All logic is original. Match strength and goal/assist attribution are
//! driven by each player's real attributes (pace, shooting, passing, …) via
//! `engine::simulate_score` (per-shot xG), so distributions look believable
//! (champion ~85-95 pts, golden boot ~25-36) and the best actual squads win.

use std::collections::{HashMap, HashSet};

use rand::seq::SliceRandom;
use rand::Rng;
use rand_pcg::Pcg64Mcg;

use crate::data::Database;
use crate::engine;
use crate::gen::Gen;
use crate::managers::Manager;
use crate::matchsim::{self, MatchPlayer, Side, Venue};
use crate::model::*;
use crate::net;
use crate::players::PlayerMind;
use crate::tune::{Params, SeasonSample};

const BIG5: [&str; 5] = ["Premier League", "La Liga", "Serie A", "Bundesliga", "Ligue 1"];
const BASE_YEAR: u16 = 2026;
/// Squad-quality gap expander: raises the ratio between elite and weak sides
/// without pushing lambdas into blow-out territory (tuned against output).
/// Deliberately strong: the merged datasets compress ability (elite ~27,
/// mid ~22), so only a steep exponent restores real separation.
const STRENGTH_EXP: f32 = 1.7;

/// Nations treated as European for the Euros (best-effort by label; anything
/// not listed simply won't appear in the Euros, only the World Cup).
const EUROPE: &[&str] = &[
    "England", "France", "Spain", "Germany", "Italy", "Portugal", "Netherlands", "Holland",
    "Belgium", "Croatia", "Denmark", "Switzerland", "Austria", "Poland", "Serbia", "Scotland",
    "Ukraine", "Sweden", "Norway", "Wales", "Czech Republic", "Czechia", "Turkey", "Türkiye",
    "Republic of Ireland", "Ireland", "Hungary", "Greece", "Romania", "Slovakia", "Slovenia",
    "Russia", "Finland", "Iceland", "Northern Ireland", "Bosnia and Herzegovina", "Albania",
    "North Macedonia", "Bulgaria", "Montenegro", "Georgia", "Kosovo", "Luxembourg",
];

/// Squad-registration rules: at most 25 registered players per season, at
/// least 2 of them club-trained (when the squad has that many) — the shape of
/// UEFA's list rules, without the association-trained split this scale can't
/// see. Registration is computed from ability with the homegrown floor
/// enforced by swapping the weakest registered non-homegrown out.
const MAX_REGISTERED: usize = 25;
const MIN_HOMEGROWN: usize = 2;

/// Compute one club's registered list: the best 25 by effective rating, then
/// the homegrown floor (weakest registered non-homegrown out for the best
/// unregistered homegrown, until the floor or the supply runs out).
fn register_squad(idxs: &[usize], pool: &[WP]) -> Vec<usize> {
    let mut cands: Vec<usize> = idxs
        .iter()
        .copied()
        .filter(|&i| !pool[i].retired)
        .collect();
    cands.sort_by(|&a, &b| {
        pool[b]
            .effective()
            .partial_cmp(&pool[a].effective())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut reg: Vec<usize> = cands.into_iter().take(MAX_REGISTERED).collect();
    let hg_avail = idxs
        .iter()
        .filter(|&&i| !pool[i].retired && pool[i].homegrown)
        .count();
    let mut need = MIN_HOMEGROWN.min(hg_avail).saturating_sub(
        reg.iter().filter(|&&i| pool[i].homegrown).count(),
    );
    while need > 0 {
        // Weakest registered non-homegrown...
        let out_pos = reg
            .iter()
            .enumerate()
            .filter(|(_, &i)| !pool[i].homegrown)
            .min_by(|(_, &a), (_, &b)| {
                pool[a]
                    .effective()
                    .partial_cmp(&pool[b].effective())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(p, _)| p);
        // ...for the best unregistered homegrown.
        let incoming = idxs
            .iter()
            .copied()
            .filter(|&i| !pool[i].retired && pool[i].homegrown && !reg.contains(&i))
            .max_by(|&a, &b| {
                pool[a]
                    .effective()
                    .partial_cmp(&pool[b].effective())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        match (out_pos, incoming) {
            (Some(p), Some(i)) => {
                reg[p] = i;
                need -= 1;
            }
            _ => break,
        }
    }
    reg
}

/// Players eligible for selection: the registered 25 when registration has
/// been computed, else the full squad (first season ever / safety fallback).
fn selectable(teams: &[Team], ti: usize) -> Vec<usize> {
    if teams[ti].registered.is_empty() {
        teams[ti].players.clone()
    } else {
        teams[ti].registered.clone()
    }
}

/// The XI a club would field from its registered list (falling back to the
/// full squad before the first registration is ever computed).
fn xi_of(w: &World, ti: usize) -> Vec<usize> {
    best_xi_indices(&selectable(&w.teams, ti), &w.pool)
}

/// A player in the working set, backed by real attributes.
/// Agent state: morale and fatigue evolve with results and minutes and feed
/// the effective rating that selection and match strength both use.
struct WP {
    name: String,
    nation: String,
    pos: Position,
    attrs: Attributes,
    ovr: u8,
    pot: u8,
    age: u8,
    team: usize,
    retired: bool,
    morale: u8,  // 0-100, drifts to 65; results move it
    fatigue: u8, // 0-100, +22 per match involved, rests midweek
    /// Seasons since a positional retraining; 0 = fully adapted. While > 0 the
    /// player plays slightly below their attributes in the new position.
    adapt: u8,
    /// True while out on loan (selected by the loanee club, not the parent).
    on_loan: bool,
    /// Club-trained here (came through this academy, never transferred away).
    /// Loans keep it — a loaned youngster is still the parent's own product.
    /// Registration protects these prospects and reports them.
    homegrown: bool,
    // per-season
    apps: u16,
    goals: u16,
    assists: u16,
    rating_sum: f32,
    rating_apps: u16,
    ucl_goals: u16,
    el_goals: u16,
    intl_goals: u16,
    ucl_assists: u16,
    el_assists: u16,
    intl_assists: u16,
    ucl_apps: u16,
    el_apps: u16,
    intl_apps: u16,
    // detailed per-season stat lines (aggregated across all competitions),
    // fed by the possession engine so a spectator sees real match output.
    shots: u16,
    sot: u16,
    /// Expected goals from his own shots — chance quality, not just goals, so
    /// a striker can be judged on the chances he gets to even when he's
    /// finishing below them (and an over-scorer is shown running hot).
    xg: f32,
    /// Expected assists: the xG of the chances his final ball created.
    xa: f32,
    /// Aerial duels won/lost at set pieces — the centre-back's ledger.
    aerials_won: u16,
    aerials_lost: u16,
    key_passes: u16,
    big_chances: u16,
    passes_att: u32,
    passes_cmp: u32,
    dribbles_att: u16,
    dribbles_cmp: u16,
    tackles: u16,
    interceptions: u16,
    fouls: u16,
    saves: u16,
    clean_sheets: u16,
    league_won: bool,
    ucl_result: u8, // 0 none, 1 R16, 2 QF, 3 SF, 4 final, 5 winner
    el_result: u8,
    intl_result: u8,
    // market
    value: i64,
    release_clause: Option<i64>,
    wage: i64,
    locked: u8, // years committed to current club before another move
    // career (accumulated across seasons for the decade legends board)
    career_goals: u32,
    career_assists: u32,
    career_kp: u32,
    career_bcc: u32,
    career_ti: u32,
    career_cs: u32,
    career_apps: u32,
    career_xg: f32,
    career_xa: f32,
    career_aerials: u32,
    career_rating_sum: f32,
    career_rating_apps: u32,
    ballons: u8,
}

impl WP {
    fn avg_rating(&self) -> f32 {
        if self.rating_apps == 0 {
            0.0
        } else {
            self.rating_sum / self.rating_apps as f32
        }
    }
    /// Match-effective rating: base overall modulated by morale (±7%) and
    /// fatigue (up to −25% when exhausted). Selection and team strength both
    /// use this, so tired or unhappy players genuinely cost results and
    /// rotation is functional, not cosmetic.
    fn effective(&self) -> f32 {
        let m = 1.0 + (self.morale as f32 - 65.0) * 0.002;
        let f = 1.0 - self.fatigue as f32 * 0.0025;
        // A player still adapting to a retrained position plays slightly down.
        let a = 1.0 - self.adapt as f32 * 0.05;
        (self.ovr as f32 * m * f * a).max(1.0)
    }
    /// Fatigue-neutral rating for MARKET assessment (who is surplus, who is
    /// settled, who needs what): end-of-season fatigue is calendar noise, and
    /// judging a star's status by how tired he is in June sells Musialas to
    /// West Ham. Morale and adaptation stay — mood and retraining are real.
    fn effective_fresh(&self) -> f32 {
        let m = 1.0 + (self.morale as f32 - 65.0) * 0.002;
        let a = 1.0 - self.adapt as f32 * 0.05;
        (self.ovr as f32 * m * a).max(1.0)
    }
    /// Goals across every competition (league + UCL + Europa + international).
    fn overall_goals(&self) -> u16 {
        self.goals + self.ucl_goals + self.el_goals + self.intl_goals
    }
    /// Assists across every competition.
    fn overall_assists(&self) -> u16 {
        self.assists + self.ucl_assists + self.el_assists + self.intl_assists
    }
    /// Goals against the quality of chances he actually received. Positive =
    /// finishing above the chances (a hot year), negative = leaving goals out
    /// there — the single most useful number for judging a striker honestly.
    fn goals_minus_xg(&self) -> f32 {
        self.overall_goals() as f32 - self.xg
    }
    /// Share of contested headers actually won, 0-100 (0 when he contested none).
    fn aerial_pct(&self) -> f32 {
        let t = (self.aerials_won + self.aerials_lost) as f32;
        if t < 1.0 {
            0.0
        } else {
            self.aerials_won as f32 / t * 100.0
        }
    }
    /// Ball-winning per appearance: tackles plus interceptions per game, the
    /// rate form of the defensive output (the raw total is on the career board).
    fn ti_per_game(&self) -> f32 {
        if self.rating_apps == 0 {
            0.0
        } else {
            (self.tackles + self.interceptions) as f32 / self.rating_apps as f32
        }
    }
    /// Likelihood of being the scorer: finishing quality × position.
    fn goal_weight(&self) -> f32 {
        let a = &self.attrs;
        let finish = a.shooting as f32 * 0.44
            + a.composure as f32 * 0.20
            + a.pace as f32 * 0.14
            + a.dribbling as f32 * 0.12
            + a.heading as f32 * 0.10;
        let pf = match self.pos {
            Position::FWD => 1.0,
            Position::MID => 0.34,
            Position::DEF => 0.07,
            Position::GK => 0.003,
        };
        // Steep exponent: the world's elite finisher pulls clear of the rest,
        // so a genuine talisman racks up 30-40 and can win the Ballon d'Or
        // repeatedly, rather than the tally spreading into weak ~25 seasons.
        pf * finish.max(1.0).powf(2.3)
    }
    /// Likelihood of assisting: creativity × position.
    /// (Retained for the international attribution path / future tuning.)
    #[allow(dead_code)]
    fn assist_weight(&self) -> f32 {
        let a = &self.attrs;
        let create = a.passing as f32 * 0.4 + a.vision as f32 * 0.4 + a.dribbling as f32 * 0.2;
        let pf = match self.pos {
            Position::MID => 1.0,
            Position::FWD => 0.7,
            Position::DEF => 0.3,
            Position::GK => 0.02,
        };
        pf * create.max(1.0)
    }
}

struct Team {
    name: String,
    league_idx: usize,
    rep: u8,
    players: Vec<usize>,
    strength: f32,
    p: u16,
    w: u16,
    d: u16,
    l: u16,
    gf: u16,
    ga: u16,
    league_titles: u16,
    ucl_titles: u16,
    el_titles: u16,
    /// Domestic super cups won (Supercopa / Community Shield / Supercoppa /
    /// Trophée des Champions / DFL-Supercup) — a minor honour, tracked for the
    /// trophy cabinet and the manager's record.
    super_cups: u16,
    budget: i64,
    /// Financial power 20..=99: the slow-moving money behind the club. Distinct
    /// from reputation (sporting stature) and from the one-window transfer
    /// budget — wealth earns revenue, revenue funds budgets, results grow
    /// wealth. This loop is what lets a Russo-style rise (or a giant's fall)
    /// compound across decades instead of resetting every summer.
    wealth: u8,
    /// Historical stature: the club's reputation at the start of the sim — its
    /// brand, fanbase and stadium, which outlast any slump. Rep and wealth
    /// mean-revert partly toward this (not a flat league average) and a brand
    /// revenue floor derives from it, so a fallen giant (a Man United or
    /// Arsenal) keeps the means to rebuild and isn't shut out for 50 years,
    /// while a club with no history can't dominate for half a century either.
    heritage: u8,
    /// Cash carried between windows (can go mildly negative = board debt).
    cash: i64,
    /// Last season's total revenue (base + prizes + Europe), in the same
    /// abstract millions as fees and wages. Budgets and the wage cap derive
    /// from this, never from reputation directly.
    revenue_last: i64,
    /// Revenue audit trail for the current season: wealth-driven base, league
    /// merit prize, European prize money, and net transfer spend (fees+agents
    /// out minus fees in). Reported, never fudged — the money's wiring in public.
    rev_base: i64,
    prize_league: i64,
    prize_euro: i64,
    net_spend: i64,
    /// This window's opening allocation (pre-spend), for honest reporting —
    /// the live `budget` field is the unspent remainder by report time.
    budget_alloc: i64,
    /// 0.3 (sellers / misers) .. 0.95 (state-backed spenders): how much of its
    /// means the board actually lets the manager burn each summer.
    willingness: f32,
    /// Youth academy quality 1..=5: intake size, wonderkid odds and how fast
    /// youngsters develop on its watch.
    academy: u8,
    /// Seasons since this club last won its league (Big5 only). Boards lose
    /// patience with droughts no trophy cabinet excuses — cups don't save you.
    title_drought: u8,
    /// Registered 25 for the season (squad-registration rules): only these may
    /// play league/European football. Unregistered players still train, get
    /// paid, and can be sold or loaned — exactly like real life.
    registered: Vec<usize>,
    /// The individual in the dugout (see managers.rs): traits, motives,
    /// memory, and a career that moves between clubs.
    manager: Manager,
    /// Tactical system (defenders, midfielders, forwards) picked by the manager
    /// from the squad each season — e.g. (4, 3, 3) or (3, 5, 2).
    shape: (usize, usize, usize),
}

impl Team {
    fn pts(&self) -> u16 {
        self.w * 3 + self.d
    }
    fn gd(&self) -> i32 {
        self.gf as i32 - self.ga as i32
    }
}

struct Nation {
    name: String,
    players: Vec<usize>,
    strength: f32,
}

#[derive(Clone, Copy, PartialEq)]
enum Comp {
    League,
    Ucl,
    Europa,
    Intl,
}

#[derive(Clone, Copy, PartialEq)]
enum IntlKind {
    None,
    WorldCup,
    Euros,
}

struct World {
    pool: Vec<WP>,
    /// Parallel minds vector: minds[i] is the motives/memory of pool[i].
    /// Must stay in lockstep (push together, never remove pool entries).
    minds: Vec<PlayerMind>,
    teams: Vec<Team>,
    league_teams: Vec<Vec<usize>>,
    nation_pool: Vec<String>,
    next_regen: u32,
    elite_cap: u8,
    /// Transfer-market inflation multiplier; grows each year so late-era fees
    /// stay big (a 2070 galáctico still costs hundreds of millions).
    inflation: f32,
    /// Active season-long loans: (player, home club, away club).
    loans: Vec<SeasonLoan>,
    /// Released players without a club; train alone and can be signed freely.
    free_agents: Vec<usize>,
    /// Names already used, to keep regen identities distinct.
    used_names: std::collections::HashSet<String>,
    /// Learned/hand simulation constants (see tune.rs). Loaded from
    /// tuned_params.json when present, defaults otherwise.
    params: Params,
}

/// One season-long loan: a young player developing on someone else's minutes.
#[derive(Clone, Copy)]
struct SeasonLoan {
    player: usize,
    home: usize,
    away: usize,
}

struct SeasonOutcome {
    year: u16,
    champions: Vec<(String, String)>,
    cup_winners: Vec<(String, String)>, // (league label, domestic cup winner)
    super_cups: Vec<(String, String)>,  // (league label, super cup winner)
    trebles: Vec<String>,               // clubs that won league + domestic cup + UCL
    boot: (String, u16, String, Attributes),
    assist_king: (String, u16, String),
    ucl_winner: String,
    /// Winner's reputation at win time (for honest realism checks later).
    ucl_winner_rep: u8,
    ucl_top: (String, u16),
    europa_winner: String,
    intl_kind: IntlKind,
    intl_winner: String,
    // Ballon d'Or: name, club, league g, league a, UCL g, UCL a, overall g, overall a
    ballon: (String, String, u16, u16, u16, u16, u16, u16),
    // Managerial merry-go-round moves that closed this season.
    manager_moves: Vec<String>,
    // All-competitions top scorer: name, overall goals, overall assists, club
    overall_boot: (String, u16, u16, String),
    // Ballon d'Or justification: (component label, detail, points) in display order
    ballon_why: Vec<(String, String, f32)>,
    transfers: Vec<String>,
    /// Tied league-golden-boot names (same goals as the winner, winner excluded).
    boot_ties: Vec<String>,
    /// Tied all-competitions top-scorer names (same overall goals, winner excluded).
    overall_ties: Vec<String>,
    /// Ballon d'Or top 5 (name, points) so margins are auditable, not asserted.
    ballon_top: Vec<(String, f32)>,
}

pub struct SeasonReport {
    lines: Vec<String>,
}

impl SeasonReport {
    pub fn print(&self) {
        for l in &self.lines {
            println!("{}", l);
        }
    }
    pub fn as_text(&self) -> String {
        self.lines.join("\n")
    }
}

// ── Strength (position-aware best XI) ───────────────────────────────────

/// Base rating = average of a best XI picked by position (1 GK, 4 DEF, 3 MID,
/// 3 FWD), each rated at their own position — by EFFECTIVE rating, so tired
/// or unhappy players drag the team's strength down and rotation matters.
fn base_xi(idxs: &[usize], pool: &[WP]) -> f32 {
    let mut buckets: [Vec<f32>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for &i in idxs {
        if pool[i].retired {
            continue;
        }
        let slot = match pool[i].pos {
            Position::GK => 0,
            Position::DEF => 1,
            Position::MID => 2,
            Position::FWD => 3,
        };
        buckets[slot].push(pool[i].effective());
    }
    for b in &mut buckets {
        b.sort_by(|a, c| c.partial_cmp(a).unwrap());
    }
    let want = [1usize, 4, 3, 3];
    let mut chosen: Vec<f32> = Vec::new();
    for s in 0..4 {
        for k in 0..want[s] {
            if let Some(v) = buckets[s].get(k) {
                chosen.push(*v);
            }
        }
    }
    // Backfill any missing slots from the overall best available.
    if chosen.len() < 11 {
        let mut rest: Vec<f32> = idxs
            .iter()
            .filter(|&&i| !pool[i].retired)
            .map(|&i| pool[i].effective())
            .collect();
        rest.sort_by(|a, c| c.partial_cmp(a).unwrap());
        for v in rest {
            if chosen.len() >= 11 {
                break;
            }
            chosen.push(v);
        }
    }
    if chosen.is_empty() {
        return 10.0;
    }
    chosen.iter().sum::<f32>() / chosen.len() as f32
}

/// Reputation mapped onto the squad-rating scale. Kept for reference/tests;
/// live strength uses the tuned slope/base in `match_strength`.
#[allow(dead_code)]
fn rep_scaled(rep: u8) -> f32 {
    10.0 + rep as f32 * 0.26
}

/// Synthetic transfer value in abstract "millions", from ability, age and
/// position. A steep exponent means the genuine elite are worth 250m+, not
/// 150m, while squad players stay cheap — keeps DB stars, galácticos and
/// youth comparable. Goalkeepers price at ~55% of an equally-rated outfielder:
/// the market simply never pays striker money for a keeper (Kepa's €80m is
/// the all-time record), so no €126m-for-a-31-year-old-keeper seasons.
fn market_value(ovr: u8, age: u8, pos: Position) -> i64 {
    // Ability on the 0-100 scale; divisor keeps the (steep) value curve in the
    // same abstract-millions band the sim was tuned around.
    let base = (ovr as f32 / 33.0).powf(4.8);
    // Age curve with real teeth past 30: veterans keep their ability but lose
    // market value fast, so 30+ stars stop moving for €200m+ — ability alone
    // never prices a declining asset like a peak one.
    let age_f = if age <= 27 {
        1.0 + (27 - age) as f32 * 0.04
    } else {
        (1.0 - (age.saturating_sub(27)) as f32 * 0.11).max(0.12)
    };
    let pos_f = if pos == Position::GK { 0.55 } else { 1.0 };
    (base * age_f * pos_f).max(1.0) as i64
}

/// Club revenue estimate in abstract "millions", anchored to the Deloitte
/// Football Money League curve: ~1100 for a rep-100 superclub (Real Madrid),
/// ~700 at rep 90 (City/Bayern/PSG tier), ~450 at rep 80, ~280 at rep 70,
/// ~150 at rep 60, tailing to ~50 for minnows. Piecewise-linear between anchors.
fn revenue(rep: u8) -> f32 {
    let anchors = [
        (100.0, 1100.0),
        (90.0, 700.0),
        (80.0, 450.0),
        (70.0, 280.0),
        (60.0, 150.0),
        (50.0, 90.0),
        (0.0, 50.0),
    ];
    let r = rep as f32;
    for w in anchors.windows(2) {
        let (r_hi, v_hi) = w[0];
        let (r_lo, v_lo) = w[1];
        if r >= r_lo {
            let t = if r_hi > r_lo { (r - r_lo) / (r_hi - r_lo) } else { 0.0 };
            return v_lo + t * (v_hi - v_lo);
        }
    }
    50.0
}

/// Indices of a club's best XI (1 GK, 4 DEF, 3 MID, 3 FWD), rated by
/// fatigue-neutral rating. Used for market surplus checks: a benched player
/// the manager won't pick is sellable. Deliberately NOT fatigue-aware — the
/// window runs on June legs, and judging status by tiredness mislabels
/// starters as surplus.
fn best_xi_indices(idxs: &[usize], pool: &[WP]) -> Vec<usize> {
    let mut per: [Vec<(f32, usize)>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for &i in idxs {
        if pool[i].retired {
            continue;
        }
        let slot = match pool[i].pos {
            Position::GK => 0,
            Position::DEF => 1,
            Position::MID => 2,
            Position::FWD => 3,
        };
        per[slot].push((pool[i].effective_fresh(), i));
    }
    for b in &mut per {
        b.sort_by(|a, c| c.0.partial_cmp(&a.0).unwrap());
    }
    let want = [1usize, 4, 3, 3];
    let mut chosen = Vec::new();
    for s in 0..4 {
        for k in 0..want[s] {
            if let Some(&(_, i)) = per[s].get(k) {
                chosen.push(i);
            }
        }
    }
    if chosen.len() < 11 {
        let mut rest: Vec<(f32, usize)> = idxs
            .iter()
            .filter(|&&i| !pool[i].retired && !chosen.contains(&i))
            .map(|&i| (pool[i].effective_fresh(), i))
            .collect();
        rest.sort_by(|a, c| c.0.partial_cmp(&a.0).unwrap());
        for (_, i) in rest {
            if chosen.len() >= 11 {
                break;
            }
            chosen.push(i);
        }
    }
    chosen
}

/// The position of a club's weakest starter, and that starter's rating.
fn weakest_slot(idxs: &[usize], pool: &[WP]) -> (Position, f32) {
    let xi = best_xi_indices(idxs, pool);
    let mut worst: Option<(f32, Position)> = None;
    for &i in &xi {
        let r = pool[i].attrs.overall(pool[i].pos) as f32;
        if worst.map_or(true, |(w, _)| r < w) {
            worst = Some((r, pool[i].pos));
        }
    }
    worst.map(|(r, p)| (p, r)).unwrap_or((Position::FWD, 0.0))
}

fn in_best_xi(pi: usize, idxs: &[usize], pool: &[WP]) -> bool {
    best_xi_indices(idxs, pool).contains(&pi)
}

// ── Tactical roles ─────────────────────────────────────────────────────
// Managers don't just pick eleven names — they assign each slot a role, and a
// role re-weights which attributes matter. A midfielder with elite passing and
// vision deployed as the central striker becomes a False Nine; the same player
// as a Poacher would be wasted. Role fit (rating × suitability) drives both
// team strength and who scores.

/// Candidate shapes: (defenders, midfielders, forwards); keeper always 1.
const SHAPES: [(usize, usize, usize); 5] = [(4, 4, 2), (4, 3, 3), (3, 5, 2), (5, 3, 2), (4, 5, 1)];

// Role rating, line mapping and best-fit selection now live in `engine` so the
// interactive game and the season sim share one definition. See
// `engine::role_rating`, `engine::role_fit`, `engine::best_role_for`,
// `engine::roles_for_line`, and `engine::line_of`.
use crate::engine::{best_role_for, line_of, role_rating};

/// Scoring multiplier by role: poachers and shadow strikers live for goals,
/// false nines create as much as they finish, wide/defensive roles chip in.
/// (Retained for the rotation talisman heuristic / future tuning.)
#[allow(dead_code)]
fn role_attack_mult(role: Role) -> f32 {
    match role {
        Role::Poacher => 1.3,
        Role::ShadowStriker => 1.25,
        Role::TargetMan => 1.15,
        Role::InsideForward => 1.1,
        Role::FalseNine => 1.0,
        Role::AdvancedPlaymaker => 0.8,
        Role::Winger => 0.9,
        Role::BoxToBox => 0.7,
        Role::DeepLyingPlaymaker => 0.45,
        Role::WingBack => 0.5,
        Role::FullBack => 0.35,
        Role::Anchor => 0.3,
        Role::CentralDefender => 0.35,
        Role::BallPlayingDefender => 0.35,
        Role::Goalkeeper => 0.02,
        Role::SweeperKeeper => 0.02,
    }
}

/// Assist multiplier by role: playmakers and creators carry the supply.
/// (Retained for the international attribution path / future tuning.)
#[allow(dead_code)]
fn role_assist_mult(role: Role) -> f32 {
    match role {
        Role::AdvancedPlaymaker => 1.35,
        Role::DeepLyingPlaymaker => 1.2,
        Role::FalseNine => 1.2,
        Role::Winger => 1.15,
        Role::InsideForward => 0.9,
        Role::BoxToBox => 0.8,
        Role::WingBack => 0.9,
        Role::FullBack => 0.8,
        Role::ShadowStriker => 0.7,
        Role::Anchor => 0.6,
        _ => 0.7,
    }
}

/// Pick the shape maximizing total role-fit over the squad's best options.
fn pick_shape(idxs: &[usize], pool: &[WP]) -> (usize, usize, usize) {
    let mut best_shape = SHAPES[0];
    let mut best_score = 0.0f32;
    for &shape in &SHAPES {
        let counts = [1usize, shape.0, shape.1, shape.2];
        let mut score = 0.0;
        for line in 0..4 {
            // List membership is authoritative: loaned players live in the
            // loanee's list (and play there), never in the parent's.
            let mut fits: Vec<f32> = idxs
                .iter()
                .filter(|&&i| !pool[i].retired)
                .map(|&i| best_role_for(&pool[i].attrs, pool[i].pos, line).1 * pool[i].effective())
                .collect();
            fits.sort_by(|a, b| b.partial_cmp(a).unwrap());
            score += fits.iter().take(counts[line]).sum::<f32>();
        }
        if score > best_score {
            best_score = score;
            best_shape = shape;
        }
    }
    best_shape
}

/// Select the XI ((player, role, line) triples) for a shape: fill each line's
/// slots with the best available players by effective rating, assigning each
/// their best-fitting role on that line. On-loan players belong to the loanee.
fn select_xi(
    idxs: &[usize],
    pool: &[WP],
    shape: (usize, usize, usize),
) -> Vec<(usize, Role, usize)> {
    let counts = [1usize, shape.0, shape.1, shape.2];
    let mut used = vec![false; pool.len()];
    let mut xi = Vec::with_capacity(11);
    for line in 0..4 {
        let mut cands: Vec<(f32, usize)> = idxs
            .iter()
            .copied()
            .filter(|&i| !pool[i].retired)
            .map(|i| {
                let eff = pool[i].effective();
                // Prefer naturals for the line, but allow anyone at suitability cost.
                let line_bonus = if line_of(pool[i].pos) == line { 1.0 } else { 0.82 };
                (eff * line_bonus, i)
            })
            .collect();
        cands.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        for (_, i) in cands.into_iter().take(counts[line]) {
            if used[i] {
                continue;
            }
            used[i] = true;
            let (role, _) = best_role_for(&pool[i].attrs, pool[i].pos, line);
            xi.push((i, role, line));
        }
    }
    // Backfill short lines (thin squads) with the best remaining, any position.
    if xi.len() < 11 {
        let mut rest: Vec<(f32, usize)> = idxs
            .iter()
            .copied()
            .filter(|&i| !pool[i].retired && !used[i])
            .map(|i| (pool[i].effective(), i))
            .collect();
        rest.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        for (_, i) in rest {
            if xi.len() >= 11 {
                break;
            }
            let (role, _) = best_role_for(&pool[i].attrs, pool[i].pos, line_of(pool[i].pos));
            xi.push((i, role, line_of(pool[i].pos)));
        }
    }
    xi
}


fn match_strength(idxs: &[usize], pool: &[WP], rep: u8, shape: (usize, usize, usize), p: &Params) -> f32 {
    // Tactical XI: each slot's contribution is role fit (rating × suitability)
    // scaled by current form, so roles, retraining and morale all move results.
    let base = {
        let xi = select_xi(idxs, pool, shape);
        if xi.is_empty() {
            10.0
        } else {
            let sum: f32 = xi
                .iter()
                .map(|&(i, role, _)| {
                    let fit = role_rating(&pool[i].attrs, role)
                        * engine::role_suitability(role, pool[i].pos);
                    let form = pool[i].effective() / pool[i].ovr.max(1) as f32;
                    fit * form
                })
                .sum();
            sum / xi.len() as f32
        }
    };
    // Squad-led blend: the actual XI matters most, reputation refines.
    // Then a zero-point shift so a few points of squad gap become real
    // separation (elite ~10+, mid ~6-8, weak ~3) instead of noise.
    let blended = base * p.blend_squad + (p.rep_base + rep as f32 * p.rep_slope) * (1.0 - p.blend_squad);
    (blended - p.zero_shift).max(2.0).powf(p.strength_exp)
}

#[allow(dead_code)]
fn weighted_pick<R: Rng + ?Sized>(
    idxs: &[usize],
    pool: &[WP],
    rng: &mut R,
    weight: impl Fn(usize, &WP) -> f32,
    exclude: Option<usize>,
) -> Option<usize> {
    let total: f32 = idxs
        .iter()
        .filter(|&&i| Some(i) != exclude && !pool[i].retired)
        .map(|&i| weight(i, &pool[i]))
        .sum();
    if total <= 0.0 {
        return None;
    }
    let mut r = rng.gen::<f32>() * total;
    for &i in idxs {
        if Some(i) == exclude || pool[i].retired {
            continue;
        }
        r -= weight(i, &pool[i]);
        if r <= 0.0 {
            return Some(i);
        }
    }
    None
}

/// Build a rotated matchday squad with roles: the tactical XI from select_xi,
/// then rest logic — each starter (except the talisman) sits with probability
/// = effective rotation (sharply higher when exhausted), replaced by the best
/// same-line alternative who takes the best-fitting role on that line. Bench
/// filled to ~15. Returns (player, role) pairs.
fn matchday_squad(
    idxs: &[usize],
    pool: &[WP],
    shape: (usize, usize, usize),
    mgr: &Manager,
    comp: Comp,
    rng: &mut impl Rng,
) -> Vec<(usize, Role)> {
    let xi = select_xi(idxs, pool, shape);
    // The talisman (best attacking output) is never rested — he plays every
    // game, so a genuine star sustains a big season tally year after year.
    let talisman = xi
        .iter()
        .copied()
        .max_by(|&(a, _ra, _), &(b, _rb, _)| {
            pool[a]
                .goal_weight()
                .partial_cmp(&pool[b].goal_weight())
                .unwrap()
        })
        .map(|(i, _, _)| i);
    let rot = mgr.effective_rotation();
    let yboost = mgr.youth_boost();
    let in_xi = |i: usize| xi.iter().any(|&(x, _, _)| x == i);
    let mut bench: Vec<usize> = idxs
        .iter()
        .copied()
        .filter(|&i| !pool[i].retired && !in_xi(i))
        .collect();
    bench.sort_by(|&a, &b| pool[b].ovr.cmp(&pool[a].ovr));
    let score = |i: usize| -> f32 {
        let mut s = pool[i].effective();
        if pool[i].age <= 21 {
            s *= yboost;
        }
        s
    };
    let mut squad: Vec<(usize, Role)> = Vec::with_capacity(15);
    let squad_has = |squad: &Vec<(usize, Role)>, i: usize| squad.iter().any(|&(x, _)| x == i);
    // Managers field full-strength sides in Europe and internationals — stars
    // and creators play through the rotation that rests them in league games.
    // Same rule for every club, so UCL scoring rates reflect the best players,
    // not whoever happened to avoid the rest lottery.
    let comp_factor = match comp {
        Comp::League => 1.0,
        _ => 0.35,
    };
    for &(s, _role, line) in &xi {
        if Some(s) != talisman {
            // Exhausted players are rested almost regardless of rotation trait.
            let p_rest = if pool[s].fatigue > 60 {
                ((rot + 0.45) * comp_factor).min(0.95)
            } else {
                rot * comp_factor
            };
            if rng.gen::<f32>() < p_rest {
                // Best same-line alternative by selection score.
                let mut cands: Vec<usize> = bench
                    .iter()
                    .copied()
                    .filter(|&b| line_of(pool[b].pos) == line && !squad_has(&squad, b))
                    .collect();
                cands.sort_by(|&a, &b| {
                    score(b).partial_cmp(&score(a)).unwrap_or(std::cmp::Ordering::Equal)
                });
                if let Some(&alt) = cands.first() {
                    let (arole, _) = best_role_for(&pool[alt].attrs, pool[alt].pos, line);
                    squad.push((alt, arole));
                    bench.retain(|&b| b != alt);
                    continue;
                }
            }
        }
        // Starter keeps the role from the tactical XI.
        let (srole, _) = best_role_for(&pool[s].attrs, pool[s].pos, line);
        squad.push((s, srole));
    }
    bench.sort_by(|&a, &b| {
        score(b).partial_cmp(&score(a)).unwrap_or(std::cmp::Ordering::Equal)
    });
    for &b in &bench {
        if squad.len() >= 15 {
            break;
        }
        if !squad_has(&squad, b) {
            let (brole, _) = best_role_for(&pool[b].attrs, pool[b].pos, line_of(pool[b].pos));
            squad.push((b, brole));
        }
    }
    squad
}

/// The 11 who actually start: the first eleven of the rotated matchday squad,
/// each carried into the engine with their live fatigue and morale/adaptation.
fn build_lineup(md: &[(usize, Role)], pool: &[WP]) -> Vec<MatchPlayer> {
    md.iter()
        .take(11)
        .map(|&(i, role)| {
            let m = 1.0 + (pool[i].morale as f32 - 65.0) * 0.002;
            let a = 1.0 - pool[i].adapt as f32 * 0.05;
            // Marked-man regression: a prolific scorer draws the whole league's
            // attention as the season wears on — double-marked, man-marked,
            // never given a yard — and legs tire from carrying the goal load.
            // Past ~24 goals each further one is harder, docking his form (so
            // finishing and everything else dip), which caps freak 60-goal
            // seasons at a realistic ceiling while leaving normal 25-35 goal
            // campaigns essentially untouched.
            let sg = pool[i].overall_goals();
            let marked = if sg > 22 {
                (1.0 - 0.015 * (sg - 22) as f32).max(0.78)
            } else {
                1.0
            };
            MatchPlayer {
                id: i,
                pos: pool[i].pos,
                role,
                attrs: pool[i].attrs,
                start_fatigue: (pool[i].fatigue as f32 / 100.0).clamp(0.0, 1.0),
                form: (m * a * marked).max(0.5),
            }
        })
        .collect()
}

/// Fold one match's per-player output back into the season pool: goals and
/// assists into the right competition, the detailed lines (passes, dribbles,
/// key passes, tackles…) into the all-comps aggregates, a real rating for every
/// starter, plus the result's morale swing and a fatigue/injury cost.
fn apply_match(o: &matchsim::MatchOutcome, comp: Comp, pool: &mut [WP], p: &Params) {
    let base_fatigue = p.fatigue_match.round() as u8;
    for s in &o.stats {
        let i = s.id;
        match comp {
            Comp::League => {
                pool[i].goals += s.goals as u16;
                pool[i].assists += s.assists as u16;
                pool[i].apps += 1;
            }
            Comp::Ucl => {
                pool[i].ucl_goals += s.goals as u16;
                pool[i].ucl_assists += s.assists as u16;
                pool[i].ucl_apps += 1;
            }
            Comp::Europa => {
                pool[i].el_goals += s.goals as u16;
                pool[i].el_assists += s.assists as u16;
                pool[i].el_apps += 1;
            }
            Comp::Intl => {
                pool[i].intl_goals += s.goals as u16;
                pool[i].intl_assists += s.assists as u16;
                pool[i].intl_apps += 1;
            }
        }
        // Detailed lines aggregate across every competition.
        pool[i].shots += s.shots as u16;
        pool[i].sot += s.shots_on_target as u16;
        pool[i].xg += s.xg;
        pool[i].xa += s.xa;
        pool[i].aerials_won += s.aerials_won as u16;
        pool[i].aerials_lost += s.aerials_lost as u16;
        pool[i].key_passes += s.key_passes as u16;
        pool[i].big_chances += s.big_chances_created as u16;
        pool[i].passes_att += s.passes_att as u32;
        pool[i].passes_cmp += s.passes_cmp as u32;
        pool[i].dribbles_att += s.dribbles_att as u16;
        pool[i].dribbles_cmp += s.dribbles_cmp as u16;
        pool[i].tackles += s.tackles as u16;
        pool[i].interceptions += s.interceptions as u16;
        pool[i].fouls += s.fouls as u16;
        pool[i].saves += s.saves as u16;
        if s.clean_sheet {
            pool[i].clean_sheets += 1;
        }
        // A rating for every appearance, in every competition, from real play.
        pool[i].rating_sum += s.rating;
        pool[i].rating_apps += 1;

        // Result-driven morale swing + fatigue; an injury spikes fatigue (the
        // sandbox has no separate sidelined list, so the cost is carried here).
        let (my, opp) = if s.side_a {
            (o.goals_a, o.goals_b)
        } else {
            (o.goals_b, o.goals_a)
        };
        let dmorale = match my.cmp(&opp) {
            std::cmp::Ordering::Greater => p.morale_win.round() as i16,
            std::cmp::Ordering::Less => -(p.morale_loss.round() as i16),
            std::cmp::Ordering::Equal => 0,
        };
        pool[i].morale = (pool[i].morale as i16 + dmorale).clamp(30, 90) as u8;
        let mut fat = base_fatigue;
        if s.injury_days > 0 {
            fat = fat.saturating_add(30);
        }
        pool[i].fatigue = pool[i].fatigue.saturating_add(fat).min(100);
    }
}

/// Play one match through the possession engine. `venue` says who is at home
/// and `agg` carries any first-leg aggregate so second legs are played to the
/// tie. Both squads are rotated by their managers first, then 22 individuals
/// interact; the per-player output is folded straight back into the season.
fn play(
    home: usize,
    away: usize,
    comp: Comp,
    venue: Venue,
    agg: (u8, u8),
    teams: &mut [Team],
    pool: &mut [WP],
    p: &Params,
    rng: &mut Pcg64Mcg,
) -> (u32, u32) {
    // Only registered players may play league/European football — the
    // registration list is the team sheet's legal boundary.
    let hsel = selectable(teams, home);
    let asel = selectable(teams, away);
    let hmd = matchday_squad(&hsel, pool, teams[home].shape, &teams[home].manager.clone(), comp, rng);
    let amd = matchday_squad(&asel, pool, teams[away].shape, &teams[away].manager.clone(), comp, rng);
    if hmd.is_empty() || amd.is_empty() {
        return (0, 0);
    }
    let hxi = build_lineup(&hmd, pool);
    let axi = build_lineup(&amd, pool);
    let mut side_h = Side {
        players: &hxi,
        ambition: teams[home].manager.ambition,
        quality: teams[home].manager.quality,
        threat: 1.0,
    };
    let mut side_a = Side {
        players: &axi,
        ambition: teams[away].manager.ambition,
        quality: teams[away].manager.quality,
        threat: 1.0,
    };
    // Both dugouts scout the matchup off the two XIs (never reputation):
    // underdogs start cagier, favourites start on the front foot.
    let (th, ta) = matchsim::threat_ratio(&side_h, &side_a);
    side_h.threat = th;
    side_a.threat = ta;
    let o = matchsim::simulate(&side_h, &side_a, venue, agg, rng);
    apply_match(&o, comp, pool, p);
    (o.goals_a as u32, o.goals_b as u32)
}

/// Record one league result into both clubs' rows.
fn record_result(teams: &mut [Team], home: usize, away: usize, hg: u32, ag: u32) {
    teams[home].p += 1;
    teams[home].gf += hg as u16;
    teams[home].ga += ag as u16;
    teams[away].p += 1;
    teams[away].gf += ag as u16;
    teams[away].ga += hg as u16;
    match hg.cmp(&ag) {
        std::cmp::Ordering::Greater => {
            teams[home].w += 1;
            teams[away].l += 1;
        }
        std::cmp::Ordering::Less => {
            teams[home].l += 1;
            teams[away].w += 1;
        }
        std::cmp::Ordering::Equal => {
            teams[home].d += 1;
            teams[away].d += 1;
        }
    }
}

/// Midweek recovery between rounds: fatigue drains, morale drifts toward each
/// player's treatment target, and minds evaluate game time vs expectation
/// (unhappy players and transfer requests emerge here, not in a global loop).
fn weekly_recovery(
    team_ids: &[usize],
    teams: &[Team],
    pool: &mut [WP],
    minds: &mut [PlayerMind],
    p: &Params,
) {
    for &t in team_ids {
        // Squad rank by ability defines expected game time.
        let mut order: Vec<usize> = teams[t].players.clone();
        order.sort_by(|&a, &b| pool[b].ovr.cmp(&pool[a].ovr));
        let played = teams[t].p;
        for (rank, &i) in order.iter().enumerate() {
            if pool[i].retired {
                continue;
            }
            pool[i].fatigue = pool[i].fatigue.saturating_sub(p.fatigue_rest.round() as u8);
            let target = minds[i].morale_target();
            if pool[i].morale < target {
                pool[i].morale += 1;
            } else if pool[i].morale > target {
                pool[i].morale -= 1;
            }
            let apps = pool[i].apps;
            let age = pool[i].age;
            let morale = &mut pool[i].morale;
            minds[i].evaluate(apps, played, rank, age, morale);
        }
    }
}

fn run_league(
    team_ids: &[usize],
    teams: &mut [Team],
    pool: &mut [WP],
    minds: &mut [PlayerMind],
    p: &Params,
    rng: &mut Pcg64Mcg,
) {
    // Proper weekly rounds (circle method): each team plays once per round,
    // then a midweek recovery runs — the shared timeline every agent lives on.
    let n = team_ids.len();
    if n < 2 {
        return;
    }
    let even_n = if n % 2 == 1 { n - 1 } else { n };
    let mut order: Vec<usize> = team_ids[..even_n].to_vec();
    // Two legs: second leg swaps venues.
    for leg in 0..2 {
        for _ in 0..even_n.saturating_sub(1) {
            for i in 0..even_n / 2 {
                let (a, b) = (order[i], order[even_n - 1 - i]);
                let (home, away) = if leg == 0 { (a, b) } else { (b, a) };
                let (hg, ag) = play(home, away, Comp::League, Venue::HomeA, (0, 0), teams, pool, p, rng);
                record_result(teams, home, away, hg, ag);
            }
            // Rotate all but the first entry (standard circle method).
            let last = order.pop().unwrap();
            order.insert(1, last);
            weekly_recovery(team_ids, teams, pool, minds, p);
        }
    }
    // Odd team out (bye) still recovers on the same timeline.
    if n % 2 == 1 {
        weekly_recovery(team_ids, teams, pool, minds, p);
    }
}

fn rank(team_ids: &[usize], teams: &[Team]) -> Vec<usize> {
    let mut v = team_ids.to_vec();
    v.sort_by(|&a, &b| {
        teams[b]
            .pts()
            .cmp(&teams[a].pts())
            .then(teams[b].gd().cmp(&teams[a].gd()))
            .then(teams[b].gf.cmp(&teams[a].gf))
    });
    v
}

/// Two-legged knockout tie (home and away, no away-goals rule — abolished in
/// reality). Aggregates both legs; level after 180' goes to strength-weighted
/// penalties. Two legs halve the fluke factor vs single ties, so the better
/// side goes through far more often while upsets still happen.
fn knockout_tie(
    a: usize,
    b: usize,
    comp: Comp,
    teams: &mut [Team],
    pool: &mut [WP],
    p: &Params,
    rng: &mut Pcg64Mcg,
) -> usize {
    // First leg at a's ground, second at b's. The second leg carries the
    // aggregate so both managers play the tie: a side trailing on aggregate
    // pushes, the side ahead manages the game out.
    let (g1a, g1b) = play(a, b, comp, Venue::HomeA, (0, 0), teams, pool, p, rng);
    let (g2b, g2a) = play(b, a, comp, Venue::HomeA, (g1b as u8, g1a as u8), teams, pool, p, rng);
    let (ta, tb) = (g1a + g2a, g1b + g2b);
    if ta > tb {
        a
    } else if tb > ta {
        b
    } else {
        let pa = teams[a].strength / (teams[a].strength + teams[b].strength);
        if rng.gen::<f32>() < pa {
            a
        } else {
            b
        }
    }
}

/// A lightweight domestic-cup tie (no per-player stat attribution — the cup
/// exists mainly to enable domestic doubles and continental trebles).
fn cup_tie(a: usize, b: usize, teams: &[Team], p: &Params, rng: &mut Pcg64Mcg) -> usize {
    let hs = teams[a].strength * rng.gen_range(p.swing_cup_lo..p.swing_cup_hi);
    let as_ = teams[b].strength * rng.gen_range(p.swing_cup_lo..p.swing_cup_hi);
    let hs = teams[a].strength * rng.gen_range(0.84..1.16);
    let as_ = teams[b].strength * rng.gen_range(0.84..1.16);
    let (ga, gb, _, _, _, _) = engine::simulate_score(rng, hs, as_);
    if ga > gb {
        a
    } else if gb > ga {
        b
    } else {
        let pa = teams[a].strength / (teams[a].strength + teams[b].strength);
        if rng.gen::<f32>() < pa {
            a
        } else {
            b
        }
    }
}

/// Single-elimination domestic cup among a league's clubs. Byes handle odd
/// fields. Returns the winning team index.
/// Single-elimination domestic cup among a league's clubs. Byes handle odd
/// fields. Returns (winner, losing finalist) so the season can also stage a
/// super cup (the Final Four in Spain and Italy pulls in the cup runner-up).
fn run_domestic_cup(ids: &[usize], teams: &[Team], p: &Params, rng: &mut Pcg64Mcg) -> Option<(usize, usize)> {
    if ids.is_empty() {
        return None;
    }
    let mut round = ids.to_vec();
    round.shuffle(rng);
    let mut last_final: Option<(usize, usize)> = None;
    while round.len() > 1 {
        let mut next = Vec::new();
        let mut i = 0;
        while i < round.len() {
            if i + 1 < round.len() {
                let (a, b) = (round[i], round[i + 1]);
                let w = cup_tie(a, b, teams, p, rng);
                // When the round that produced this tie had exactly two teams,
                // that tie was the final — remember the beaten finalist.
                if round.len() == 2 {
                    last_final = Some((w, if w == a { b } else { a }));
                }
                next.push(w);
                i += 2;
            } else {
                next.push(round[i]); // bye
                i += 1;
            }
        }
        round = next;
    }
    match last_final {
        Some((w, l)) => Some((w, l)),
        // Single-entry field (no final was ever played): no distinct runner-up.
        None => round.first().map(|&w| (w, w)),
    }
}

/// Resolve a super cup from an ordered, pre-deduped participant list: a single
/// final for two teams, or a Final Four (semis seeded 1v4 / 2v3, then the
/// final) for four. Ties run on team strength via `cup_tie`. Returns the
/// winner, or None for an empty field.
fn super_cup_winner(field: &[usize], teams: &[Team], p: &Params, rng: &mut Pcg64Mcg) -> Option<usize> {
    match field.len() {
        0 => None,
        1 => Some(field[0]),
        2 => Some(cup_tie(field[0], field[1], teams, p, rng)),
        3 => {
            // Odd field: top seed gets a bye to the final.
            let s = cup_tie(field[1], field[2], teams, p, rng);
            Some(cup_tie(field[0], s, teams, p, rng))
        }
        _ => {
            // Final Four: 1v4 and 2v3 semis, then the final.
            let s1 = cup_tie(field[0], field[3], teams, p, rng);
            let s2 = cup_tie(field[1], field[2], teams, p, rng);
            Some(cup_tie(s1, s2, teams, p, rng))
        }
    }
}

// ── World build ────────────────────────────────────────────────────────

/// FM2023 rows carry 2022-vintage ages into a 2026 start. FM skills are 1-20
/// scaled ×5, so a row whose every attribute is a multiple of 5 is FM-vintage
/// (an FC row hitting that by chance is ~1-in-250-billion); shift it +4 years.
fn fm_vintage_age(age: u8, a: &Attributes) -> u8 {
    let fm_vintage = [
        a.pace, a.shooting, a.passing, a.dribbling, a.tackling,
        a.heading, a.strength, a.stamina, a.vision,
        a.composure, a.goalkeeping, a.first_touch,
    ]
    .iter()
    .all(|v| v % 5 == 0);
    if fm_vintage {
        (age + 4).clamp(16, 40)
    } else {
        age.clamp(16, 40)
    }
}

fn build_world(db: &Database, params: Params, rng: &mut Pcg64Mcg) -> World {
    let mut pool: Vec<WP> = Vec::new();
    let mut teams: Vec<Team> = Vec::new();
    let mut league_teams: Vec<Vec<usize>> = vec![Vec::new(); BIG5.len()];
    // The FM 2023 and FC 26 sets both list many of the same players, often at
    // different clubs, under subtly different names and ages ("Harry Kane", 29
    // at Spurs in the 2023 snapshot vs "Harry Edward Kane", 33 at Bayern in the
    // 2026 one; "Kylian Mbappe" vs "Kylian Mbappe Lottin"). Group every
    // candidate by first name + position, then fold rows that share
    // surname-length tokens within a few years of age into ONE man, keeping the
    // best row (highest overall; ties prefer the older age = the newer
    // snapshot). The survivor keeps his latest club — Kane lands at Bayern,
    // never as a ghost copy at Spurs — while genuinely different players sharing
    // a first name are untouched.
    let fold = |s: &str| -> Vec<String> {
        s.to_lowercase()
            .split_whitespace()
            .map(|t| {
                let mut out = String::new();
                let mut prev = '\0';
                for c in t.chars() {
                    // Fold accents so "Mbappé" and "Mbappe", "Martínez" and
                    // "Martinez" match across the two source datasets.
                    let f = match c {
                        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ā' => 'a',
                        'é' | 'è' | 'ê' | 'ë' | 'ē' => 'e',
                        'í' | 'ì' | 'î' | 'ï' | 'ī' => 'i',
                        'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' | 'ō' => 'o',
                        'ú' | 'ù' | 'û' | 'ü' | 'ū' => 'u',
                        'ñ' => 'n',
                        'ç' | 'č' | 'ć' => 'c',
                        'ý' | 'ÿ' => 'y',
                        'š' => 's',
                        'ž' | 'ź' | 'ż' => 'z',
                        'đ' => 'd',
                        other => other,
                    };
                    if !f.is_alphanumeric() {
                        continue;
                    }
                    // Collapse doubled letters: "haaland" and "håland" both
                    // become "haland", "mbappe" matches "mbappé", and so on.
                    if f == prev {
                        continue;
                    }
                    prev = f;
                    out.push(f);
                }
                out
            })
            .filter(|t| t.len() >= 2)
            .collect()
    };
    let pos_key = |p: Position| -> u8 {
        match p {
            Position::GK => 0,
            Position::DEF => 1,
            Position::MID => 2,
            Position::FWD => 3,
        }
    };
    // Same man? Shared surname-length tokens with a small age gap (the two
    // snapshots are ~3 years apart), or one token list contained in the other.
    let same_man = |a_t: &[String], a_age: u8, b_t: &[String], b_age: u8| -> bool {
        if (a_age as i32 - b_age as i32).abs() > 5 {
            return false;
        }
        let shared: Vec<&String> = a_t.iter().filter(|t| b_t.contains(t)).collect();
        if !shared.iter().any(|t| t.len() >= 4) {
            return false;
        }
        let subset =
            a_t.iter().all(|t| b_t.contains(t)) || b_t.iter().all(|t| a_t.contains(t));
        shared.len() >= 2 || subset
    };
    // Chosen clubs per league (top 20 by reputation), collected up front so the
    // dedupe pre-pass and the squad build below agree on the field.
    let mut league_clubs: Vec<Vec<ClubId>> = vec![Vec::new(); BIG5.len()];
    for (li, lname) in BIG5.iter().enumerate() {
        let mut clubs: Vec<&Club> = db
            .clubs()
            .iter()
            .filter(|c| c.league.eq_ignore_ascii_case(lname))
            .collect();
        clubs.sort_by(|a, b| b.reputation.cmp(&a.reputation));
        clubs.truncate(20);
        league_clubs[li] = clubs.iter().map(|c| c.id).collect();
    }
    // Every candidate row (generous top-30 per club; dedupe losses follow).
    struct Cand {
        pid: PlayerId,
        ovr: u8,
        age: u8,
        toks: Vec<String>,
    }
    let mut by_first: std::collections::HashMap<(String, u8), Vec<Cand>> =
        std::collections::HashMap::new();
    for ids in &league_clubs {
        for cid in ids {
            let Some(c) = db.clubs().iter().find(|c| c.id == *cid) else { continue };
            let mut ranked: Vec<(u8, PlayerId)> = c
                .squad
                .iter()
                .filter_map(|pid| {
                    db.players()
                        .iter()
                        .find(|p| p.id == *pid)
                        .map(|p| (p.overall(), *pid))
                })
                .collect();
            ranked.sort_by(|a, b| b.0.cmp(&a.0));
            for (_, pid) in ranked.into_iter().take(30) {
                let Some(p) = db.players().iter().find(|p| p.id == pid) else { continue };
                if p.name.trim().len() < 2 {
                    continue;
                }
                let toks = fold(&p.name);
                if toks.len() < 2 {
                    continue;
                }
                by_first
                    .entry((toks[0].clone(), pos_key(p.position)))
                    .or_default()
                    .push(Cand { pid, ovr: p.overall(), age: p.age.clamp(16, 40), toks });
            }
        }
    }
    // Within each first-name group, keep one row per man: best overall, ties to
    // the older age (the newer snapshot). Distinct players survive side by side.
    let mut winners: std::collections::HashSet<PlayerId> = std::collections::HashSet::new();
    for cands in by_first.values() {
        let mut order: Vec<usize> = (0..cands.len()).collect();
        order.sort_by(|&a, &b| {
            cands[b].ovr.cmp(&cands[a].ovr).then(cands[b].age.cmp(&cands[a].age))
        });
        let mut kept: Vec<usize> = Vec::new();
        for &ci in &order {
            let dup = kept.iter().any(|&ki| {
                same_man(&cands[ci].toks, cands[ci].age, &cands[ki].toks, cands[ki].age)
            });
            if !dup {
                kept.push(ci);
                winners.insert(cands[ci].pid);
            }
        }
    }

    for (li, _lname) in BIG5.iter().enumerate() {
        let ids = league_clubs[li].clone();
        let mut clubs: Vec<&Club> = ids
            .iter()
            .filter_map(|cid| db.clubs().iter().find(|c| c.id == *cid))
            .collect();
        // League order already set above (reputation-sorted); keep it stable.
        for c in clubs {
            let tid = teams.len();
            let mut pidx = Vec::new();
            // Best 26 by overall (see pull_db_squad rationale): merged FM/FC
            // squads must field their FC stars, not the weaker-scale rows.
            let mut ranked: Vec<(u8, PlayerId)> = c
                .squad
                .iter()
                .filter_map(|pid| {
                    db.players()
                        .iter()
                        .find(|p| p.id == *pid)
                        .map(|p| (p.overall(), *pid))
                })
                .collect();
            ranked.sort_by(|a, b| b.0.cmp(&a.0));
            for (_, pid) in ranked.into_iter().take(26) {
                if !winners.contains(&pid) {
                    continue; // folded into another row's survivor (see pre-pass)
                }
                if let Some(p) = db.players().iter().find(|p| p.id == pid) {
                    if p.name.trim().len() < 2 {
                        continue;
                    }
                    let page = fm_vintage_age(p.age, &p.attrs);
                    pidx.push(pool.len());
                    pool.push(WP {
                        name: p.name.clone(),
                        nation: p.nation.clone(),
                        pos: p.position,
                        attrs: p.attrs,
                        ovr: p.overall(),
                        pot: p.potential.max(p.overall()),
                        age: page,
                        team: tid,
                        retired: false,
                        morale: 70,
                        fatigue: 0,
                        adapt: 0,
                        on_loan: false,
                        // Database players arrive with unknown academy history:
                        // conservatively not counted as club-trained.
                        homegrown: false,
                        apps: 0,
                        goals: 0,
                        assists: 0,
                        rating_sum: 0.0,
                        rating_apps: 0,
                        ucl_goals: 0,
                        el_goals: 0,
                        intl_goals: 0,
                        ucl_assists: 0,
                        el_assists: 0,
                        intl_assists: 0,
                        ucl_apps: 0,
                        el_apps: 0,
                        intl_apps: 0,
                        shots: 0,
                        sot: 0,
                        xg: 0.0,
                        xa: 0.0,
                        aerials_won: 0,
                        aerials_lost: 0,
                        key_passes: 0,
                        big_chances: 0,
                        passes_att: 0,
                        passes_cmp: 0,
                        dribbles_att: 0,
                        dribbles_cmp: 0,
                        tackles: 0,
                        interceptions: 0,
                        fouls: 0,
                        saves: 0,
                        clean_sheets: 0,
                        league_won: false,
                        ucl_result: 0,
                        el_result: 0,
                        intl_result: 0,
                        value: market_value(p.overall(), page, p.position),
                        // Stars carry a release clause scaled to the market (flatter
                        // elite scaling keeps them huge but bounded); lesser
                        // players have no clause and move on the normal market.
                        release_clause: {
                            let o = p.overall();
                            if o >= 82 {
                                let v = market_value(o, page, p.position) as f32;
                                let age_f = (1.0 - (page.saturating_sub(29)) as f32 * 0.12).max(0.35);
                                let mult = (params.clause_mult + (o.saturating_sub(82)) as f32 * 0.12).min(3.0);
                                Some((v * mult * age_f) as i64)
                            } else {
                                None
                            }
                        },
                        // Wages run ~40% of market value (value × 0.4): wage bills land
                        // near half of revenue like real clubs, so money doesn't
                        // pile to the cap and budgets actually bind.
                        wage: (market_value(p.overall(), page, p.position) * 2 / 5).max(1),
                        locked: {
                            let o = p.overall();
                            let h = ((pool.len() as u64).wrapping_mul(2654435761) % 3) as u8;
                            if o >= 90 {
                                5 + h // 5-7 year deals for generational talents
                            } else if o >= 78 {
                                3 + h
                            } else {
                                h
                            }
                        },
                        career_goals: 0,
                        career_assists: 0,
                        career_kp: 0,
                        career_bcc: 0,
                        career_ti: 0,
                        career_cs: 0,
                        career_xg: 0.0,
                        career_xa: 0.0,
                        career_aerials: 0,
                        career_apps: 0,
                        career_rating_sum: 0.0,
                        career_rating_apps: 0,
                        ballons: 0,
                    });
                }
            }
            if pidx.len() < 11 {
                for _ in &pidx {
                    pool.pop();
                }
                continue;
            }
            let shape = pick_shape(&pidx, &pool);
            let strength = match_strength(&pidx, &pool, c.reputation, shape, &params);
            league_teams[li].push(tid);
            // Wealth starts at reputation (the 2024/25 money table, roughly) with
            // a little idiosyncrasy so equally-reputed clubs differ; willingness
            // rises with wealth but keeps individual character (some rich clubs
            // hoard, some mid clubs gamble). Academies roughly follow stature.
            let wealth = ((c.reputation as i32) + rng.gen_range(-4..=4)).clamp(20, 99) as u8;
            let willingness = (0.38 + (wealth as f32 / 100.0) * 0.42 + rng.gen_range(0.0..0.12)).clamp(0.3, 0.95);
            let academy = ((c.reputation as i32) / 24 + rng.gen_range(0..=1)).clamp(1, 5) as u8;
            let revenue_last = revenue(wealth) as i64;
            teams.push(Team {
                name: c.name.clone(),
                league_idx: li,
                rep: c.reputation,
                players: pidx,
                strength,
                p: 0,
                w: 0,
                d: 0,
                l: 0,
                gf: 0,
                ga: 0,
                league_titles: 0,
                ucl_titles: 0,
                el_titles: 0,
                super_cups: 0,
                budget: 0,
                wealth,
                heritage: c.reputation,
                cash: (revenue_last as f32 * 0.3) as i64,
                revenue_last,
                rev_base: 0,
                prize_league: 0,
                prize_euro: 0,
                net_spend: 0,
                budget_alloc: 0,
                willingness,
                academy,
                title_drought: 0,
                registered: Vec::new(),
                manager: Manager::new_random(rng, &params),
                shape,
            });
        }
    }

    // Europe's second tier (Porto, Ajax, Celtic…): real UCL fields are full of
    // them, and without farmers every group game is elite-vs-elite — which is
    // exactly why star UCL rates sat at half their league level. They play no
    // domestic league here (league_idx 5 = outside the Big 5) but contest the
    // UCL, trade in the window, and can grow into genuine powers over decades.
    let fillers = [
        ("FC Porto", 66u8),
        ("SL Benfica", 65),
        ("AFC Ajax", 64),
        ("PSV Eindhoven", 62),
        ("Celtic FC", 60),
        ("RB Salzburg", 59),
        ("Club Brugge", 58),
        ("Shakhtar Donetsk", 57),
    ];
    for (fname, frep) in fillers {
        let tid = teams.len();
        let mut local_names: std::collections::HashSet<String> =
            pool.iter().map(|w| w.name.clone()).collect();
        let mut pidx = Vec::new();
        let plan = [
            (Position::GK, 2),
            (Position::DEF, 6),
            (Position::MID, 7),
            (Position::FWD, 5),
        ];
        // Squad tier maps to low-70s overalls with a few 78+ standouts — the real
        // shape of an outsider squad (a couple of stars, honest workers behind
        // them). Uniform mid-70s proved too strong: farmers were winning the
        // whole competition back-to-back instead of the occasional Porto 2004.
        let s = (frep as f32 / 2.7).round() as i32;
        let mut standouts = 3;
        for (pos, count) in plan {
            for _ in 0..count {
                let mut g = Gen::new(rng.gen::<u64>());
                let tier = if standouts > 0 {
                    standouts -= 1;
                    s + 3
                } else {
                    s
                };
                let gp = g.player(pos, tier);
                if gp.name.trim().len() < 2 || !local_names.insert(gp.name.clone()) {
                    continue;
                }
                let age = gp.age.clamp(16, 36);
                let wonder = gp.potential >= 90;
                let ovr = age_cap_ovr(gp.overall(), age, wonder);
                pidx.push(pool.len());
                pool.push(WP {
                    name: gp.name.clone(),
                    nation: gp.nation.clone(),
                    pos,
                    attrs: gp.attrs,
                    ovr,
                    pot: ovr.max(gp.potential).min(94),
                    age,
                    team: tid,
                    retired: false,
                    morale: 70,
                    fatigue: 0,
                    adapt: 0,
                    on_loan: false,
                    homegrown: false,
                    apps: 0,
                    goals: 0,
                    assists: 0,
                    rating_sum: 0.0,
                    rating_apps: 0,
                    ucl_goals: 0,
                    el_goals: 0,
                    intl_goals: 0,
                    ucl_assists: 0,
                    el_assists: 0,
                    intl_assists: 0,
                    ucl_apps: 0,
                    el_apps: 0,
                    intl_apps: 0,
                    shots: 0,
                    sot: 0,
                    xg: 0.0,
                    xa: 0.0,
                    aerials_won: 0,
                    aerials_lost: 0,
                    key_passes: 0,
                    big_chances: 0,
                    passes_att: 0,
                    passes_cmp: 0,
                    dribbles_att: 0,
                    dribbles_cmp: 0,
                    tackles: 0,
                    interceptions: 0,
                    fouls: 0,
                    saves: 0,
                    clean_sheets: 0,
                    league_won: false,
                    ucl_result: 0,
                    el_result: 0,
                    intl_result: 0,
                    value: market_value(ovr, age, pos),
                    release_clause: None,
                    wage: (market_value(ovr, age, pos) * 2 / 5).max(1),
                    locked: {
                        let h = ((pool.len() as u64).wrapping_mul(2654435761) % 3) as u8;
                        if ovr >= 78 { 2 + h } else { h }
                    },
                    career_goals: 0,
                    career_assists: 0,
                    career_kp: 0,
                    career_bcc: 0,
                    career_ti: 0,
                    career_cs: 0,
                    career_xg: 0.0,
                    career_xa: 0.0,
                    career_aerials: 0,
                    career_apps: 0,
                    career_rating_sum: 0.0,
                    career_rating_apps: 0,
                    ballons: 0,
                });
            }
        }
        if pidx.len() < 11 {
            for _ in &pidx {
                pool.pop();
            }
            continue;
        }
        let shape = pick_shape(&pidx, &pool);
        let strength = match_strength(&pidx, &pool, frep, shape, &params);
        let wealth = frep;
        let willingness =
            (0.38 + (wealth as f32 / 100.0) * 0.42 + rng.gen_range(0.0..0.12)).clamp(0.3, 0.95);
        let academy = ((frep as i32) / 24 + rng.gen_range(0..=1)).clamp(1, 5) as u8;
        let revenue_last = revenue(wealth) as i64;
        teams.push(Team {
            name: fname.to_string(),
            league_idx: 5,
            rep: frep,
            players: pidx,
            strength,
            p: 0,
            w: 0,
            d: 0,
            l: 0,
            gf: 0,
            ga: 0,
            league_titles: 0,
            ucl_titles: 0,
            el_titles: 0,
            super_cups: 0,
            budget: 0,
            wealth,
            heritage: frep,
            cash: (revenue_last as f32 * 0.3) as i64,
            revenue_last,
            rev_base: 0,
            prize_league: 0,
            prize_euro: 0,
            net_spend: 0,
            budget_alloc: 0,
            willingness,
            academy,
            title_drought: 0,
            registered: Vec::new(),
            manager: Manager::new_random(rng, &params),
            shape,
        });
    }

    let used_names: std::collections::HashSet<String> =
        pool.iter().map(|w| w.name.clone()).collect();

    // First registration: every club registers its best 25 before a ball is
    // kicked, so the rules hold from season one (no window has run yet).
    for ti in 0..teams.len() {
        let reg = register_squad(&teams[ti].players, &pool);
        teams[ti].registered = reg;
    }

    let nation_pool: Vec<String> = pool
        .iter()
        .filter(|w| w.nation.trim().len() >= 2)
        .map(|w| w.nation.clone())
        .collect();
    // Elite ceiling for regens: the percentile of current overalls, CLAMPED to
    // the band real generational talent actually occupies (94–96). Without the
    // floor, a thin or aging initial pool drags the ceiling down with it — the
    // best prospects inherit it, cap out early, and world talent ratchets
    // downward every generation because nobody is allowed to be the next Messi
    // or Yamal. Without the ceiling, growth has no human limit at all.
    let mut ovrs: Vec<u8> = pool.iter().map(|w| w.ovr).collect();
    ovrs.sort_unstable();
    let elite_cap = ovrs
        .get(ovrs.len() * 98 / 100)
        .copied()
        .unwrap_or(90)
        .clamp(94, 96);

    World {
        pool,
        minds: Vec::new(), // filled just below, one mind per player
        teams,
        league_teams,
        nation_pool,
        next_regen: 1,
        elite_cap,
        inflation: 1.0,
        params,
        loans: Vec::new(),
        free_agents: Vec::new(),
        used_names,
    }
}

/// Give every pooled player a mind (call once after the initial pool build).
fn seed_minds(w: &mut World, rng: &mut Pcg64Mcg) {
    w.minds = (0..w.pool.len()).map(|_| PlayerMind::new(rng)).collect();
}

// ── One season ─────────────────────────────────────────────────────────

fn reset_season(w: &mut World) {
    for t in &mut w.teams {
        t.p = 0;
        t.w = 0;
        t.d = 0;
        t.l = 0;
        t.gf = 0;
        t.ga = 0;
    }
    for (i, p) in w.pool.iter_mut().enumerate() {
        // Bank this season into the career totals before wiping it (drives the
        // decade "biggest mark" and career top-10 boards). Every counter banks
        // here, all competitions included — nothing accrues live mid-season,
        // so career tables always reconcile exactly with season lines.
        p.career_goals += p.overall_goals() as u32;
        p.career_assists += p.overall_assists() as u32;
        p.career_kp += p.key_passes as u32;
        p.career_bcc += p.big_chances as u32;
        p.career_ti += (p.tackles + p.interceptions) as u32;
        p.career_cs += p.clean_sheets as u32;
        p.career_apps += p.apps as u32;
        p.career_xg += p.xg;
        p.career_xa += p.xa;
        p.career_aerials += (p.aerials_won + p.aerials_lost) as u32;
        p.career_rating_sum += p.rating_sum;
        p.career_rating_apps += p.rating_apps as u32;
        p.apps = 0;
        p.goals = 0;
        p.assists = 0;
        p.rating_sum = 0.0;
        p.rating_apps = 0;
        p.ucl_goals = 0;
        p.el_goals = 0;
        p.intl_goals = 0;
        p.ucl_assists = 0;
        p.el_assists = 0;
        p.intl_assists = 0;
        p.ucl_apps = 0;
        p.el_apps = 0;
        p.intl_apps = 0;
        p.shots = 0;
        p.sot = 0;
        p.xg = 0.0;
        p.xa = 0.0;
        p.aerials_won = 0;
        p.aerials_lost = 0;
        p.key_passes = 0;
        p.big_chances = 0;
        p.passes_att = 0;
        p.passes_cmp = 0;
        p.dribbles_att = 0;
        p.dribbles_cmp = 0;
        p.tackles = 0;
        p.interceptions = 0;
        p.fouls = 0;
        p.saves = 0;
        p.clean_sheets = 0;
        p.league_won = false;
        p.ucl_result = 0;
        p.el_result = 0;
        p.intl_result = 0;
        // New season bounce: fresh legs, reset mood. Transfer requests stand
        // until the market resolves them; unhappiness gets a clean slate.
        p.fatigue = 0;
        p.morale = 65;
        w.minds[i].unhappy = false;
    }
    for ti in 0..w.teams.len() {
        // Yearly tactical refresh: the manager re-picks the shape from the
        // registered squad, then strength follows the tactical XI.
        let sel = selectable(&w.teams, ti);
        let shape = pick_shape(&sel, &w.pool);
        let s = match_strength(&sel, &w.pool, w.teams[ti].rep, shape, &w.params)
            * w.teams[ti].manager.quality;
        w.teams[ti].shape = shape;
        w.teams[ti].strength = s;
    }
}

fn intl_kind_for(year: u16) -> IntlKind {
    // World Cup 2026/2030/2034…, Euros 2028/2032…, nothing in odd years.
    if year % 4 == 2 {
        IntlKind::WorldCup
    } else if year % 4 == 0 {
        IntlKind::Euros
    } else {
        IntlKind::None
    }
}

fn run_one_season(w: &mut World, year: u16, rng: &mut Pcg64Mcg) -> SeasonOutcome {
    reset_season(w);
    // Every club that lifts silverware this year. The boardroom reads this
    // before it reads the league table: what a manager WON is part of the
    // season's verdict, not an afterthought to it.
    let mut honours: HashSet<usize> = HashSet::new();

    for li in 0..BIG5.len() {
        let ids = w.league_teams[li].clone();
        if ids.len() >= 4 {
            run_league(&ids, &mut w.teams, &mut w.pool, &mut w.minds, &w.params, rng);
        }
    }

    let mut tables: Vec<Vec<usize>> = Vec::new();
    let mut champions: Vec<(String, String)> = Vec::new();
    let mut champ_idxs: Vec<usize> = Vec::new();
    for li in 0..BIG5.len() {
        let table = rank(&w.league_teams[li], &w.teams);
        if let Some(&champ) = table.first() {
            w.teams[champ].league_titles += 1;
            w.teams[champ].title_drought = 0;
            w.teams[champ].manager.title_drought = 0;
            champ_idxs.push(champ);
            honours.insert(champ);
            let cname = w.teams[champ].name.clone();
            for &pi in &w.teams[champ].players {
                w.pool[pi].league_won = true;
            }
            champions.push((BIG5[li].to_string(), cname));
        }
        tables.push(table);
    }
    // Title droughts tick for every Big5 club that didn't win its league.
    // Outsider clubs (no domestic league here) are judged on Europe instead.
    // Both the club and the current manager's drought advance together, so a
    // manager's trophyless count is always an exact, per-regime season tally.
    for li in 0..BIG5.len() {
        for &t in &w.league_teams[li] {
            if !champ_idxs.contains(&t) {
                w.teams[t].title_drought = w.teams[t].title_drought.saturating_add(1);
                w.teams[t].manager.title_drought =
                    w.teams[t].manager.title_drought.saturating_add(1);
            }
        }
    }

    // Domestic cups (one knockout per league). The winning manager's career
    // record credits it like any other trophy — a cup is a cup.
    let mut cup_winners: Vec<(String, String)> = Vec::new();
    let mut cup_result: Vec<Option<(usize, usize)>> = vec![None; BIG5.len()];
    for li in 0..BIG5.len() {
        let ids = w.league_teams[li].clone();
        if ids.len() >= 4 {
            if let Some((win, finalist)) = run_domestic_cup(&ids, &w.teams, &w.params, rng) {
                w.teams[win].manager.trophies += 1;
                honours.insert(win);
                cup_winners.push((BIG5[li].to_string(), w.teams[win].name.clone()));
                cup_result[li] = Some((win, finalist));
            }
        }
    }

    // Domestic super cups — the season's curtain-raiser between the league
    // champion and the domestic cup winner. Spain (La Liga) and Italy (Serie A)
    // use the modern Final Four — champion, league runner-up, cup winner and
    // cup runner-up, two semifinals and a final — while England, Germany and
    // France contest a single match. A club that already did the Double meets
    // the league runner-up instead, exactly as the real competitions resolve
    // it. The ties run on team strength (which derives from the players), like
    // the domestic cup, and the winner banks a minor honour + a manager trophy.
    let mut super_cups: Vec<(String, String)> = Vec::new();
    for li in 0..BIG5.len() {
        let Some(&champ) = tables[li].first() else { continue };
        let Some((cup_w, cup_f)) = cup_result[li] else { continue };
        let runner = tables[li].get(1).copied();
        // Build the participant list for this league's format, in the real
        // seeding order, then dedupe (a Double winner collapses entries, which
        // the fallbacks below top up from the league table).
        let mut field: Vec<usize> = Vec::new();
        let mut push = |t: usize, field: &mut Vec<usize>| {
            if !field.contains(&t) {
                field.push(t);
            }
        };
        if li == 1 || li == 2 {
            // Final Four seeding: champion v cup runner-up, cup winner v league
            // runner-up. Collapsed entries are backfilled from 3rd/4th place.
            push(champ, &mut field);
            push(cup_w, &mut field);
            if let Some(r) = runner {
                push(r, &mut field);
            }
            push(cup_f, &mut field);
            for &t in tables[li].iter().skip(2) {
                if field.len() >= 4 {
                    break;
                }
                push(t, &mut field);
            }
        } else {
            // Single match: champion vs cup winner, or the league runner-up if
            // the champion also lifted the cup.
            push(champ, &mut field);
            if cup_w != champ {
                push(cup_w, &mut field);
            } else if let Some(r) = runner {
                push(r, &mut field);
            }
        }
        if let Some(win) = super_cup_winner(&field, &w.teams, &w.params, rng) {
            w.teams[win].super_cups += 1;
            w.teams[win].manager.trophies += 1;
            honours.insert(win);
            super_cups.push((BIG5[li].to_string(), w.teams[win].name.clone()));
        }
    }

    // Champions League: top 4 per Big-5 league (20), the 8 outsiders (Porto,
    // Ajax, Celtic… — the farmers and future powers), plus the 4 strongest
    // 5th-placed sides. 32 teams, Swiss phase, top 16 to the knockouts.
    let mut ucl: Vec<usize> = Vec::new();
    for table in &tables {
        for &t in table.iter().take(4) {
            ucl.push(t);
        }
    }
    for ti in 0..w.teams.len() {
        if w.teams[ti].league_idx == 5 {
            ucl.push(ti);
        }
    }
    let mut fifths: Vec<usize> = tables.iter().filter_map(|t| t.get(4).copied()).collect();
    fifths.sort_by(|&a, &b| {
        w.teams[b].strength.partial_cmp(&w.teams[a].strength).unwrap_or(std::cmp::Ordering::Equal)
    });
    for &t in fifths.iter().take(4) {
        if !ucl.contains(&t) {
            ucl.push(t);
        }
    }
    ucl.truncate(32);
    let ucl_winner_idx = run_cup(&ucl, Comp::Ucl, &mut w.teams, &mut w.pool, &w.params, rng);
    if let Some(t) = ucl_winner_idx {
        honours.insert(t);
    }
    let ucl_winner = ucl_winner_idx
        .map(|t| {
            w.teams[t].ucl_titles += 1;
            w.teams[t].manager.trophies += 1;
            w.teams[t].name.clone()
        })
        .unwrap_or_else(|| "-".to_string());

    // Treble = same club wins its league + its domestic cup + the UCL.
    let mut trebles: Vec<String> = Vec::new();
    if ucl_winner != "-" {
        let won_league = champions.iter().any(|(_, c)| *c == ucl_winner);
        let won_cup = cup_winners.iter().any(|(_, c)| *c == ucl_winner);
        if won_league && won_cup {
            trebles.push(ucl_winner.clone());
        }
    }

    // Europa League: the next tier of finishers not already in the UCL.
    let mut europa: Vec<usize> = Vec::new();
    let mut d2 = 4;
    while europa.len() < 32 && d2 < 16 {
        for table in &tables {
            if europa.len() >= 32 {
                break;
            }
            if let Some(&t) = table.get(d2) {
                if !ucl.contains(&t) && !europa.contains(&t) {
                    europa.push(t);
                }
            }
        }
        d2 += 1;
    }
    let europa_winner_idx = if europa.len() >= 8 {
        run_cup(&europa, Comp::Europa, &mut w.teams, &mut w.pool, &w.params, rng).map(|t| {
            w.teams[t].el_titles += 1;
            w.teams[t].manager.trophies += 1;
            t
        })
    } else {
        None
    };
    if let Some(t) = europa_winner_idx {
        honours.insert(t);
    }
    let europa_winner = europa_winner_idx
        .map(|t| w.teams[t].name.clone())
        .unwrap_or_else(|| "-".to_string());

    // International tournament (calendar-driven). Squads get a summer break
    // first — nobody plays a World Cup at 100 fatigue after league + Europe.
    let intl_kind = intl_kind_for(year);
    let intl_winner = match intl_kind {
        IntlKind::None => "-".to_string(),
        kind => {
            for p in w.pool.iter_mut() {
                p.fatigue = p.fatigue.min(25);
            }
            let mut nations = build_nations(&w.pool, kind);
            run_international(&mut nations, &mut w.pool, &w.params, rng).unwrap_or_else(|| "-".to_string())
        }
    };

    // Golden boot.
    let mut boot_idx: Vec<usize> = (0..w.pool.len()).collect();
    boot_idx.sort_by(|&a, &b| w.pool[b].goals.cmp(&w.pool[a].goals));
    let bi = boot_idx[0];
    let boot = (
        w.pool[bi].name.clone(),
        w.pool[bi].goals,
        w.teams[w.pool[bi].team].name.clone(),
        w.pool[bi].attrs,
    );

    // Playmaker (most league assists).
    let mut ast_idx: Vec<usize> = (0..w.pool.len()).collect();
    ast_idx.sort_by(|&a, &b| w.pool[b].assists.cmp(&w.pool[a].assists));
    let ai = ast_idx[0];
    let assist_king = (
        w.pool[ai].name.clone(),
        w.pool[ai].assists,
        w.teams[w.pool[ai].team].name.clone(),
    );

    let mut ut: Vec<usize> = (0..w.pool.len()).collect();
    ut.sort_by(|&a, &b| w.pool[b].ucl_goals.cmp(&w.pool[a].ucl_goals));
    let ucl_top = (w.pool[ut[0]].name.clone(), w.pool[ut[0]].ucl_goals);

    // All-competitions top scorer (league + UCL + Europa + international).
    let mut ov_idx: Vec<usize> = (0..w.pool.len()).collect();
    ov_idx.sort_by(|&a, &b| w.pool[b].overall_goals().cmp(&w.pool[a].overall_goals()));
    let ob = ov_idx[0];
    let overall_boot = (
        w.pool[ob].name.clone(),
        w.pool[ob].overall_goals(),
        w.pool[ob].overall_assists(),
        w.teams[w.pool[ob].team].name.clone(),
    );

    // Ballon d'Or, with the scoring breakdown kept for the report. The score
    // now carries media/reputation bias and a per-player narrative swing, so
    // the vote behaves like journalists', not a pure stat formula.
    let mut bd: Vec<(usize, f32)> = (0..w.pool.len())
        .map(|i| (i, ballon_score(&w.pool[i], w.teams[w.pool[i].team].rep, year)))
        .collect();
    bd.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    let bwin = bd[0].0;
    w.pool[bwin].ballons += 1;
    let (_, why_parts) = ballon_breakdown(&w.pool[bwin], w.teams[w.pool[bwin].team].rep, year);
    let ballon_why: Vec<(String, String, f32)> = why_parts
        .into_iter()
        .map(|(label, detail, pts)| (label.to_string(), detail, pts))
        .collect();
    let ballon_top: Vec<(String, f32)> = bd
        .iter()
        .take(5)
        .map(|&(i, sc)| (w.pool[i].name.clone(), sc))
        .collect();
    // Ties are first-class citizens: same goals, shared honour, shown as such.
    let boot_ties: Vec<String> = boot_idx
        .iter()
        .filter(|&&i| i != bi && w.pool[i].goals == w.pool[bi].goals)
        .map(|&i| w.pool[i].name.clone())
        .collect();
    let overall_ties: Vec<String> = ov_idx
        .iter()
        .filter(|&&i| i != ob && w.pool[i].overall_goals() == w.pool[ob].overall_goals())
        .map(|&i| w.pool[i].name.clone())
        .collect();
    let ballon = (
        w.pool[bwin].name.clone(),
        w.teams[w.pool[bwin].team].name.clone(),
        w.pool[bwin].goals,
        w.pool[bwin].assists,
        w.pool[bwin].ucl_goals,
        w.pool[bwin].ucl_assists,
        w.pool[bwin].overall_goals(),
        w.pool[bwin].overall_assists(),
    );

    // Trophies go on the winning managers' career records too.
    for table in &tables {
        if let Some(&champ) = table.first() {
            w.teams[champ].manager.trophies += 1;
        }
    }

    // End-of-season club evolution: reputation drifts on success/failure (with
    // mean-reversion) and managers move (a great coach at a small club gets the
    // big job), so the hierarchy is never frozen — clubs and eras rise and fall.
    let manager_moves = post_season_evolution(w, &tables, &honours, rng);
    // Money loop closes the season: prizes and Europe flow into cash/revenue,
    // wealth compounds — the transfer window that follows spends the proceeds.
    settle_finances(w, &tables, rng);

    // Refresh the winner's reputation AFTER settle: settle already applies the
    // outsider bump and season drift, so the realism check judges the club as
    // it stood when it lifted the trophy — never a stale pre-season number or
    // a decades-drifted present value.
    let ucl_winner_rep = ucl_winner_idx.map(|t| w.teams[t].rep).unwrap_or(0);
    SeasonOutcome {
        year,
        champions,
        cup_winners,
        super_cups,
        trebles,
        boot,
        assist_king,
        ucl_winner,
        ucl_winner_rep,
        ucl_top,
        europa_winner,
        intl_kind,
        intl_winner,
        ballon,
        overall_boot,
        ballon_why,
        manager_moves,
        transfers: Vec::new(),
        boot_ties,
        overall_ties,
        ballon_top,
    }
}

/// Ballon d'Or score broken into named components so the report can justify
/// the award bullet by bullet. Returns (total, components in display order).
fn ballon_breakdown(w: &WP, club_rep: u8, year: u16) -> (f32, Vec<(&'static str, String, f32)>) {
    let mut parts: Vec<(&'static str, String, f32)> = Vec::new();
    let g = w.goals as f32 * 6.0;
    parts.push(("League goals", format!("{} × 6", w.goals), g));
    let a = w.assists as f32 * 4.0;
    parts.push(("League assists", format!("{} × 4", w.assists), a));
    // Creativity beyond the final ball: carving out big chances is rewarded, so
    // an assist king / creator competes with a pure poacher for the award.
    let cc = w.big_chances as f32 * 1.4;
    parts.push(("Chances created", format!("{} × 1.4", w.big_chances), cc));
    // Appearances across every competition the player featured in — league,
    // Europe and international caps all count, so a season carrying both club
    // and country is rewarded, not just the domestic games.
    let total_apps = w.apps + w.ucl_apps + w.el_apps + w.intl_apps;
    let ap = total_apps as f32 * 0.35;
    parts.push(("Appearances", format!("{} × 0.35 (all comps)", total_apps), ap));
    let rt = (w.avg_rating() - 6.5).max(0.0) * 62.0;
    parts.push((
        "Average rating",
        format!("{:.2} above 6.5 baseline", (w.avg_rating() - 6.5).max(0.0)),
        rt,
    ));
    let lt = if w.league_won { 35.0 } else { 0.0 };
    parts.push((
        "League title",
        if w.league_won { "won".to_string() } else { "no".to_string() },
        lt,
    ));
    let ug = w.ucl_goals as f32 * 7.0;
    parts.push(("UCL goals", format!("{} × 7", w.ucl_goals), ug));
    let ua = w.ucl_assists as f32 * 5.0;
    parts.push(("UCL assists", format!("{} × 5", w.ucl_assists), ua));
    let ur = match w.ucl_result {
        5 => 70.0,
        4 => 35.0,
        3 => 18.0,
        2 => 8.0,
        _ => 0.0,
    };
    parts.push(("UCL run", run_label(w.ucl_result, "UCL"), ur));
    let eg = w.el_goals as f32 * 3.0;
    parts.push(("Europa goals", format!("{} × 3", w.el_goals), eg));
    // Every competition's assists count, mirroring the goal weights: without
    // these two lines the all-comps header never reconciles with the parts.
    let ea = w.el_assists as f32 * 2.0;
    parts.push(("Europa assists", format!("{} × 2", w.el_assists), ea));
    let er = match w.el_result {
        5 => 22.0,
        4 => 11.0,
        _ => 0.0,
    };
    parts.push(("Europa run", run_label(w.el_result, "EL"), er));
    let ig = w.intl_goals as f32 * 6.0;
    parts.push(("International goals", format!("{} × 6", w.intl_goals), ig));
    let ia = w.intl_assists as f32 * 4.0;
    parts.push(("International assists", format!("{} × 4", w.intl_assists), ia));
    let ir = match w.intl_result {
        5 => 55.0,
        4 => 28.0,
        3 => 14.0,
        _ => 0.0,
    };
    parts.push(("International run", run_label(w.intl_result, "tournament"), ir));
    // All-round game, weighted small so it only tips the award to a defender or
    // a holding midfielder in a genuinely monumental season (a Rodri/Modric or
    // a title-winning centre-back), never over a prolific forward in a normal
    // year. Ball-winning: a destroyer's tackles and interceptions.
    let dw = (w.tackles + w.interceptions) as f32 * 0.22;
    parts.push(("Ball-winning", format!("{} tackles+int × 0.22", w.tackles + w.interceptions), dw));
    // Clean sheets reward the men who keep them — defenders and keepers only.
    let is_def = matches!(w.pos, Position::DEF | Position::GK);
    let cs = if is_def { w.clean_sheets as f32 * 3.2 } else { 0.0 };
    parts.push(("Clean sheets", format!("{} × 3.2", w.clean_sheets), cs));
    // Defensive masterclass: a defender or keeper who anchors a title win with a
    // wall of clean sheets gets a one-off historic bump — the rare season a
    // Cannavaro/Maldini-type actually takes the Ballon off the forwards.
    let rock = if is_def && w.league_won && w.clean_sheets >= 18 { 35.0 } else { 0.0 };
    parts.push((
        "Defensive masterclass",
        if rock > 0.0 { format!("title + {} clean sheets", w.clean_sheets) } else { "no".to_string() },
        rock,
    ));
    // Control of the game: completed passes, dribbles and the final ball before
    // the chance — each tiny, so only a true metronome/creator climbs on them.
    let poss = w.passes_cmp as f32 * 0.004 + w.dribbles_cmp as f32 * 0.05 + w.key_passes as f32 * 0.3;
    parts.push((
        "Build-up & creation",
        format!("{} passes, {} dribbles, {} key passes", w.passes_cmp, w.dribbles_cmp, w.key_passes),
        poss,
    ));
    // Winning everything in one year is historic — a league + Champions League
    // double (the treble's spine) is rewarded over and above the parts, so the
    // heartbeat of a clean-sweep side (a Rodri/Modric year) can take the award.
    let sweep = if w.league_won && w.ucl_result == 5 { 45.0 } else { 0.0 };
    parts.push((
        "Clean-sweep season",
        if sweep > 0.0 { "league + UCL".to_string() } else { "no".to_string() },
        sweep,
    ));
    // Underdog story: carrying a lower-reputation club to a Champions League
    // final or title is the stuff of legend and voters reward it — the
    // counterweight to the big-club media bias below.
    let underdog = if club_rep < 78 && w.ucl_result >= 4 { 38.0 } else { 0.0 };
    parts.push((
        "Underdog run",
        if underdog > 0.0 { format!("deep UCL run at rep {}", club_rep) } else { "no".to_string() },
        underdog,
    ));
    // The Ballon is voted by journalists, not computed — so two human factors
    // sit on top of the on-pitch maths, enough to decide the close calls the
    // way real PR and storylines do, never to crown a nobody.
    // Media & reputation: coverage follows the badge; a superclub star is seen,
    // debated and remembered more than an equal season at a provincial club.
    let media = (club_rep as f32 - 70.0).max(0.0) * 1.1;
    parts.push(("Media & reputation", format!("club reputation {}", club_rep), media));
    // Narrative swing: recency, storyline and voter whimsy — deterministic per
    // player per year (stable within a run), a bounded boost that reorders only
    // genuinely tight races. This is the subjectivity behind every real-world
    // Ballon controversy, instead of a purely robotic formula.
    let swing = ballon_narrative(&w.name, year);
    parts.push(("Narrative & recency", "journalists' mood".to_string(), swing));
    let total: f32 = parts.iter().map(|(_, _, s)| s).sum();
    (total, parts)
}

/// Deterministic pseudo-random Ballon "narrative" boost in [0, 28), from a
/// stable hash of the player's name and the year — so the same season always
/// tells the same story, yet different players and years diverge. One-sided
/// (never negative) so the printed breakdown always reconciles with the total.
fn ballon_narrative(name: &str, year: u16) -> f32 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in name.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h ^= year as u64;
    h = h.wrapping_mul(0x100000001b3);
    let u = ((h >> 40) & 0xffff) as f32 / 65536.0; // [0,1)
    u * 28.0
}

fn run_label(result: u8, comp: &str) -> String {
    match result {
        5 => format!("won the {}", comp),
        4 => format!("{} finalist", comp),
        3 => format!("{} semi-final", comp),
        2 => format!("{} quarter-final", comp),
        1 => format!("{} knockouts", comp),
        _ => "group stage / none".to_string(),
    }
}

fn ballon_score(w: &WP, club_rep: u8, year: u16) -> f32 {
    ballon_breakdown(w, club_rep, year).0
}

/// End-of-season reputation drift + manager merry-go-round. Keeps the world
/// from freezing: title winners gain prestige, strugglers lose it, everything
/// mean-reverts, and the best over-achieving manager takes a struggling giant's
/// job — so Barça can slump and recover, Arsenal can have an era, and a great
/// coach, not just a big badge, drives success.
fn post_season_evolution(
    w: &mut World,
    tables: &[Vec<usize>],
    honours: &HashSet<usize>,
    rng: &mut Pcg64Mcg,
) -> Vec<String> {
    // 1. Reputation drift by league finish, with mean-reversion toward 65.
    for table in tables {
        let n = table.len();
        if n == 0 {
            continue;
        }
        for (pos, &t) in table.iter().enumerate() {
            let delta: f32 = if pos == 0 {
                w.params.drift_title
            } else if pos < n / 4 {
                w.params.drift_topq
            } else if pos >= (3 * n) / 4 {
                w.params.drift_bot
            } else {
                0.0
            };
            let cur = w.teams[t].rep as f32;
            // Mean-reversion stops any club running away forever — but it pulls
            // toward the club's own historical stature, not a flat league
            // average. A fallen giant (heritage ~88) reverts up toward the
            // mid-70s and claws back; a club with no history (heritage ~62)
            // reverts down, so it can enjoy an era but not reign for 50 years.
            // The anchor leans 60/40 average/heritage, so eras still turn over.
            let heritage = w.teams[t].heritage as f32;
            let anchor = 0.6 * 65.0 + 0.4 * heritage;
            let drifted = (cur + delta) * (1.0 - w.params.mean_rev) + anchor * w.params.mean_rev;
            w.teams[t].rep = drifted.round().clamp(30.0, 100.0) as u8;
        }
    }

    // 2. Manager churn. Overperformance = how far a club finished above where
    //    its reputation ranked it in the league.
    let mut over: Vec<(usize, i32)> = Vec::new(); // (team, overperformance)
    for table in tables {
        let mut by_rep = table.clone();
        by_rep.sort_by(|&a, &b| w.teams[b].rep.cmp(&w.teams[a].rep));
        for (finish_pos, &t) in table.iter().enumerate() {
            let rep_pos = by_rep.iter().position(|&x| x == t).unwrap_or(finish_pos);
            over.push((t, rep_pos as i32 - finish_pos as i32));
        }
    }
    // Best over-achiever takes a struggling giant's seat, unless loyalty or
    // low ambition keeps them. The individual's NAME, traits and trophy count
    // travel (career continues at the new club, seasons reset); the sacked
    // manager is replaced by a fresh hire at the smaller club.
    let mut moves: Vec<String> = Vec::new();
    over.sort_by(|a, b| b.1.cmp(&a.1));
    let risers: Vec<usize> = over.iter().take(3).map(|(t, _)| *t).collect();
    let mut fallers: Vec<usize> = over.iter().rev().take(6).map(|(t, _)| *t).collect();
    fallers.sort_by(|&a, &b| w.teams[b].rep.cmp(&w.teams[a].rep)); // biggest club first
    for (i, &riser) in risers.iter().enumerate() {
        if let Some(&faller) = fallers.get(i) {
            // A club that just lifted something doesn't hand its dugout to a
            // newcomer the same summer — the seat only opens when it's empty.
            if honours.contains(&faller) {
                continue;
            }
            if w.teams[faller].rep > w.teams[riser].rep && w.teams[riser].manager.wants_bigger_job() {
                let rq = w.teams[riser].manager.quality;
                let fq = w.teams[faller].manager.quality;
                let rname = w.teams[riser].manager.name.clone();
                let rtroph = w.teams[riser].manager.trophies;
                let rage = w.teams[riser].manager.age;
                let fname = w.teams[faller].manager.name.clone();
                w.teams[faller].manager.quality = rq.max(fq);
                w.teams[faller].manager.name = rname.clone();
                w.teams[faller].manager.seasons = 0;
                w.teams[faller].manager.trophies = rtroph;
                w.teams[faller].manager.age = rage;
                w.teams[faller].manager.title_drought = 0;
                w.teams[faller].manager.security = 0.6;
                w.teams[faller].manager.loyalty = w.teams[faller].manager.loyalty.max(0.3);
                w.teams[riser].manager = Manager::fresh_hire(rng, &w.params);
                moves.push(format!(
                    "{} sacks {} and appoints {} ({} trophies)",
                    w.teams[faller].name, fname, rname, rtroph
                ));
            }
        }
    }
    // Sack the very worst under-achievers outright (fresh, uncertain hire).
    let worst: Vec<usize> = over.iter().rev().take(3).map(|(t, _)| *t).collect();
    for t in worst {
        if rng.gen_bool(w.params.sack_worst_p.clamp(0.0, 1.0) as f64) {
            let old = w.teams[t].manager.name.clone();
            w.teams[t].manager = Manager::fresh_hire(rng, &w.params);
            moves.push(format!("{} sacks {} after a dismal campaign", w.teams[t].name, old));
        }
    }
    // Elite expectations: giants are judged on titles, not survival. A rep-88+
    // club finishing outside the top 2 faces the sack even from 3rd — this is
    // what stops a good squad (e.g. Barcelona) rotting for a decade under one
    // mediocre appointment while a rival dominates.
    //
    // Two things keep this honest. First, honours: a board fires nobody for
    // "missing the title race" in the summer he won the league, the Champions
    // League or a domestic cup — the shield is tied to THAT season's silverware,
    // so it never compounds into a permanent tenure. Second, severity: each
    // place below the board's line multiplies the pressure, so a runner-up has
    // a bad year while a side that dropped out of Europe has a crisis, instead
    // of every second-placed manager at a big club rolling the same dice.
    for table in tables {
        for (finish_pos, &t) in table.iter().enumerate() {
            let elite_rep = w.params.elite_rep.round() as u8;
            let elite_pos = w.params.elite_pos.round().max(1.0) as usize;
            if w.teams[t].rep >= elite_rep && finish_pos + 1 > elite_pos && !honours.contains(&t) {
                let gap = ((finish_pos + 1) as f32 - w.params.elite_pos).max(1.0);
                let p = (w.params.sack_elite_p * 0.20 * gap).clamp(0.0, 0.85);
                if rng.gen_bool(p as f64) {
                    let old = w.teams[t].manager.name.clone();
                    w.teams[t].manager = Manager::fresh_hire(rng, &w.params);
                    let where_ = finish_pos + 1;
                    moves.push(format!(
                        "{} sack {} for missing the title race (finished {})",
                        w.teams[t].name, old, where_
                    ));
                }
            }
            // Title droughts end tenures cups can't save: a big club whose
            // CURRENT manager has gone six-plus seasons without a league title
            // starts rolling the axe every year, harder the longer it runs. No
            // brave cup run saves a regime a board expects to win leagues.
            // Judged on stature (rep OR wealth): a fallen giant whose rep has
            // already drifted down doesn't get to lower expectations and hide
            // — the wealth and fanbase still demand titles. Measured against
            // the manager's own tenure, so the number is never a club's whole
            // trophyless history pinned on a man who just arrived.
            let drought = w.teams[t].manager.title_drought;
            let stature = w.teams[t].rep.max(w.teams[t].wealth);
            // …but only when it actually cost the club Europe. A giant that
            // finished 2nd–4th still plays Champions League football, and
            // firing a manager for a trophyless *league* while he keeps the
            // club in Europe is how a board churns away its own stability.
            // (A table too small to have European places can't be judged on
            // them — hence the length check.)
            let missed_europe = table.len() < 8 || finish_pos >= 4;
            if stature >= 85 && drought >= 6 && missed_europe {
                let p = (0.25 + 0.10 * (drought - 6) as f32).min(0.8);
                if rng.gen_bool(p.clamp(0.0, 1.0) as f64) {
                    let old = w.teams[t].manager.name.clone();
                    w.teams[t].manager = Manager::fresh_hire(rng, &w.params);
                    moves.push(format!(
                        "{} sack {} after {} trophyless league years",
                        w.teams[t].name, old, drought
                    ));
                }
            }
        }
    }
    // NOTE: no title-winner bonus on purpose. Winners already keep a proven
    // manager while rivals churn theirs — adding a bonus too would compound
    // into an unbeatable feedback loop (whoever wins first keeps winning).
    // Every manager reviews the season (safety follows results) and regresses
    // slowly toward average — eras rise and fade.
    for t in 0..w.teams.len() {
        w.teams[t].manager.regress(w.params.q_regress.clamp(0.8, 0.99));
    }
    // Security follows overperformance, computed per league table.
    for table in tables {
        let mut by_rep = table.clone();
        by_rep.sort_by(|&a, &b| w.teams[b].rep.cmp(&w.teams[a].rep));
        for (finish_pos, &t) in table.iter().enumerate() {
            let rep_pos = by_rep.iter().position(|&x| x == t).unwrap_or(finish_pos);
            w.teams[t].manager.review_season(rep_pos as i32 - finish_pos as i32);
        }
    }
    // Age and impatience close the merry-go-round. Managers retire with rising
    // probability through their 60s (hard cap at 72), and boards lose patience
    // at random regardless of results — the De Laurentiis factor every club
    // carries, big or mid-table. Together these churn tenures so no reign runs
    // 40 or 50 seasons, and even a stable mid-table club eventually changes its
    // man, without any hardcoded term limit. A manager appointed this very
    // summer (seasons < 2) is spared the random axe — nobody is hired and
    // sacked in the same window.
    for t in 0..w.teams.len() {
        if w.teams[t].manager.retires(rng) {
            let old = w.teams[t].manager.name.clone();
            let yrs = w.teams[t].manager.seasons;
            let age = w.teams[t].manager.age;
            w.teams[t].manager = Manager::fresh_hire(rng, &w.params);
            moves.push(format!(
                "{} retires after {} seasons at {} (aged {})",
                old, yrs, w.teams[t].name, age
            ));
            continue;
        }
        let impatience = (0.05 + (1.0 - w.teams[t].manager.security) * 0.12).clamp(0.0, 0.3);
        if w.teams[t].manager.seasons >= 2 && rng.gen_bool(impatience as f64) {
            let old = w.teams[t].manager.name.clone();
            w.teams[t].manager = Manager::fresh_hire(rng, &w.params);
            moves.push(format!("{} part ways with {}", w.teams[t].name, old));
        }
    }
    moves
}



fn retire_prob(age: u8, is_gk: bool) -> f64 {
    if age >= 42 {
        return 1.0; // hard cap — nobody plays on at 42
    }
    let base = match age {
        0..=31 => 0.0,
        32 => 0.10,
        33 => 0.18,
        34 => 0.30,
        35 => 0.45,
        36 => 0.62,
        37 => 0.78,
        38 => 0.90,
        39 => 0.96,
        40 => 0.99,
        _ => 0.99,
    };
    if is_gk {
        base * 0.6
    } else {
        base
    }
}

/// Growth with diminishing returns toward human limits: full steps below 82,
/// +2s into the low 90s, +1s toward 95, and above that only rare
/// breakthroughs. Wonderkids still reach the low 90s (Messi/Ronaldo peaked at
/// 94 overall with real weaknesses elsewhere); wall-to-wall 99s need miracle
/// breakthrough chains that essentially never complete.
fn grow_attr(v: &mut u8, base: i32, rng: &mut Pcg64Mcg) {
    let cur = *v as i32;
    let d = if cur < 82 {
        base
    } else if cur < 90 {
        base.min(2)
    } else if cur < 95 {
        1
    } else if rng.gen_bool(0.06) {
        1
    } else {
        0
    };
    *v = (cur + d).clamp(1, 99) as u8;
}

/// Complex-enough annual development: attributes grow toward potential while
/// young, plateau in the prime, and decline with age (pace/stamina first).
fn develop_player(p: &mut WP, dev_mid_p: f32, youth_boost: f32, rng: &mut Pcg64Mcg) {
    // Attributes live on the 0-100 scale, so a year's growth/decline moves a
    // few points (not one). Profiles change unevenly — pace and stamina fade
    // first while technique, vision and composure linger — so a veteran can
    // reinvent themselves rather than just losing "overall". And minutes
    // develop: a teenager who actually plays (15+ apps) grows faster, doubly
    // so under a manager who trusts youth — the academy-to-first-team path.
    let bump = |v: &mut u8, d: i32| {
        *v = (*v as i32 + d).clamp(1, 99) as u8;
    };
    // Growth slows near human limits (see grow_attr): even generational
    // talents stall around 92-94 in their best attributes (Messi/Ronaldo
    // peaked at 94 overall with real weaknesses elsewhere), instead of
    // strolling to wall-to-wall 99s. Decline still uses raw bump below.
    if p.age <= 21 {
        // Fast growth years, biased to the player's natural strengths. Minutes
        // develop — and international caps count as minutes too, so a teenager
        // breaking into his national team grows from that experience, not only
        // from club games.
        let minutes = p.apps + p.intl_apps;
        let mut steps = rng.gen_range(1..=3);
        if minutes >= 15 && rng.gen_bool(0.35) {
            steps += 1; // playing develops
        }
        if youth_boost > 1.05 && minutes >= 15 && rng.gen_bool(0.5) {
            steps += 1; // …especially for a manager who trusts kids
        }
        for _ in 0..steps {
            match p.pos {
                Position::FWD => {
                    grow_attr(&mut p.attrs.shooting, 3, rng);
                    if rng.gen_bool(0.5) {
                        grow_attr(&mut p.attrs.dribbling, 3, rng);
                    } else {
                        grow_attr(&mut p.attrs.composure, 3, rng);
                    }
                }
                Position::MID => {
                    grow_attr(&mut p.attrs.passing, 3, rng);
                    if rng.gen_bool(0.5) {
                        grow_attr(&mut p.attrs.vision, 3, rng);
                    } else {
                        grow_attr(&mut p.attrs.stamina, 3, rng);
                    }
                }
                Position::DEF => {
                    grow_attr(&mut p.attrs.tackling, 3, rng);
                    if rng.gen_bool(0.5) {
                        grow_attr(&mut p.attrs.heading, 3, rng);
                    } else {
                        grow_attr(&mut p.attrs.strength, 3, rng);
                    }
                }
                Position::GK => grow_attr(&mut p.attrs.goalkeeping, 3, rng),
            }
        }
    } else if p.age <= 25 && rng.gen_bool(dev_mid_p.clamp(0.0, 1.0) as f64) {
        match p.pos {
            Position::FWD => grow_attr(&mut p.attrs.shooting, 3, rng),
            Position::MID => grow_attr(&mut p.attrs.passing, 3, rng),
            Position::DEF => grow_attr(&mut p.attrs.tackling, 3, rng),
            Position::GK => grow_attr(&mut p.attrs.goalkeeping, 3, rng),
        }
        // Technique/mentality can still tick up in the mid-twenties.
        if rng.gen_bool(0.4) {
            match p.pos {
                Position::FWD => grow_attr(&mut p.attrs.composure, 2, rng),
                Position::MID => grow_attr(&mut p.attrs.vision, 2, rng),
                Position::DEF => grow_attr(&mut p.attrs.composure, 2, rng),
                Position::GK => grow_attr(&mut p.attrs.composure, 2, rng),
            }
        }
    } else if p.age >= 30 {
        // Physical decline accelerates with age; technique fades far slower, so
        // an ageing forward can drop pace yet keep finishing, vision, composure.
        // Keepers are no exception forever: reflexes (goalkeeping) go from 33,
        // command (composure) from 36 — a 37-year-old keeper is experienced,
        // not ageless, so no more Karius-to-PSG-at-peak-value seasons.
        let sev = 3 + (p.age.saturating_sub(31)) as i32; // ~3..12 points a year
        bump(&mut p.attrs.pace, -rng.gen_range(2..=sev + 2));
        bump(&mut p.attrs.stamina, -rng.gen_range(1..=sev + 2));
        if p.pos == Position::GK {
            if p.age >= 33 {
                bump(&mut p.attrs.goalkeeping, -rng.gen_range(1..=3));
            }
            if p.age >= 36 && rng.gen_bool(0.5) {
                bump(&mut p.attrs.composure, -rng.gen_range(1..=2));
            }
        }
        if p.age >= 33 && rng.gen_bool(0.5) {
            bump(&mut p.attrs.dribbling, -rng.gen_range(1..=4));
        }
        if p.age >= 34 && rng.gen_bool(0.4) {
            bump(&mut p.attrs.strength, -rng.gen_range(1..=3));
        }
        // A wise old head can still add a touch of vision/composure.
        if rng.gen_bool(0.25) {
            grow_attr(&mut p.attrs.vision, 1, rng);
            grow_attr(&mut p.attrs.composure, 1, rng);
        }
    }
    // Recompute overall from the (changed) attributes, capped to potential.
    p.ovr = p.attrs.overall(p.pos).min(p.pot);
}

fn age_and_regen(w: &mut World, rng: &mut Pcg64Mcg) {
    // Transfer-market inflation creeps up each year (like real football), so
    // fees and budgets keep growing and late-era deals stay blockbuster-sized.
    w.inflation *= w.params.inflation.clamp(1.0, 1.15);
    let dev_mid_p = w.params.dev_mid_p;
    // Per-club youth trust (a borrowing-safe snapshot): youngsters develop
    // faster on minutes under managers who believe in them. Same snapshot
    // trick for revenues, feeding the superstar wage spiral below.
    let boosts: Vec<f32> = w.teams.iter().map(|t| t.manager.youth_boost()).collect();
    let revs: Vec<i64> = w.teams.iter().map(|t| t.revenue_last).collect();
    // Spanish superclub assets carry prohibitive buy-out clauses: Spanish law
    // makes the *player* able to buy himself out, so the club sets the number
    // and a giant sets it deliberately out of everyone's reach. A billion for a
    // prized youngster is symbolic — it means "not for sale to a domestic
    // rival or a state-backed superclub", not "price him".
    let spanish_giant: Vec<bool> = w
        .teams
        .iter()
        .map(|t| t.league_idx == 1 && t.rep >= 88)
        .collect();
    for (idx, p) in w.pool.iter_mut().enumerate() {
        if p.retired {
            continue;
        }
        p.age = p.age.saturating_add(1);
        let yb = boosts.get(p.team).copied().unwrap_or(1.0);
        develop_player(p, dev_mid_p, yb, rng);
        // Revalue every player to the inflated market each year — without this,
        // fees and clauses freeze at 2026 prices while budgets inflate, so by
        // the 2030s every star moves at a decade-old discount. Wages inflate
        // too (real pay tracks revenue): frozen nominal wages would make wage
        // bills trivial within a decade, so nobody ever needs to sell, top
        // clubs hoard everyone, mid-table defenses starve on kids, and the
        // 2060s fill with 70-goal seasons against hollowed-out leagues.
        p.value = (market_value(p.ovr, p.age, p.pos) as f32 * w.inflation) as i64;
        p.wage = ((p.wage as f32 * w.params.inflation.clamp(1.0, 1.15)) as i64).max(1);
        // Superstar wage spiral: elite players' agents demand a cut of what the
        // CLUB earns (6–16% of revenue by greed), not just a multiple of market
        // value. Hoarding three 95s means each takes a sixth of turnover, so
        // super-teams break up under their own wage bills instead of coasting —
        // the demand scales with the club, exactly like real negotiations.
        if p.ovr >= 85 {
            let rev = revs.get(p.team).copied().unwrap_or(300) as f32;
            let greed = w.minds[idx].greed.clamp(0.0, 1.0);
            let demand = (rev * (0.06 + 0.10 * greed)) as i64;
            if demand > p.wage {
                p.wage = demand;
            }
        }
        // Release clauses reprice every year from current market value for all
        // 82+ players — never a stale nominal discount for buyers to exploit.
        // The elite scaling is deliberately flatter than it used to be (a 95
        // overall costs ~2.5× value, not ~6×): superstar clauses stay huge but
        // bounded, so fees can't compound into €867m absurdities on their own.
        if p.ovr >= 82 {
            let giant = spanish_giant.get(p.team).copied().unwrap_or(false);
            let core = p.ovr >= 85 || (p.age <= 22 && p.pot >= 88);
            if giant && core {
                // €1bn, in this model's million-unit scale. Nobody's budget or
                // FFP ceiling clears it, so the clause can't be *triggered* —
                // it is a refusal with a number attached. It expires on its own
                // the moment he ages out of the core or drops below the bar.
                p.release_clause = Some(1_000);
            } else {
                let v = market_value(p.ovr, p.age, p.pos) as f32 * w.inflation;
                // Clauses decay with age past 29: nobody holds a €138m clause on a
                // 31-year-old keeper — veteran clauses price the declining asset.
                let age_f = (1.0 - (p.age.saturating_sub(29)) as f32 * 0.12).max(0.35);
                let mult = (w.params.clause_mult + (p.ovr.saturating_sub(82)) as f32 * 0.12).min(3.0);
                p.release_clause = Some((v * mult * age_f) as i64);
            }
        } else {
            p.release_clause = None;
        }
        // Clubs renew key men before deals lapse — but not always, or the market
        // freezes: no talent cascades down, mid-table fields children forever
        // and retained 95-finishers feast for 70-goal seasons. Generational
        // talents (90+) mostly stay; very good players (82+) hit the market
        // about half the time, keeping the pyramid fluid like reality.
        if p.ovr >= 90 && p.locked <= 1 && p.age <= 32 && rng.gen_bool(0.80) {
            p.locked = 5 + (rng.gen::<u8>() % 3); // 5..=7 more years
        } else if p.ovr >= 82 && p.ovr < 90 && p.locked <= 1 && p.age <= 32 && rng.gen_bool(0.55) {
            p.locked = 3 + (rng.gen::<u8>() % 3); // 3..=5 more years
        }
        // Champion restlessness: highly ambitious winners in their prime
        // sometimes want a new mountain (the Ronaldo mechanism) — dominance
        // rots dynasties from within as stars chase fresh challenges, which is
        // what breaks 9-peat lockstep without touching anyone's quality.
        if p.league_won
            && p.age <= 30
            && p.ovr >= 82
            && !w.minds[idx].requested
            && w.minds[idx].ambition > 0.8
            && rng.gen_bool(0.10)
        {
            w.minds[idx].requested = true;
            w.minds[idx].unhappy = true;
        }
        let is_gk = p.pos == Position::GK;
        if rng.gen_bool(retire_prob(p.age, is_gk)) {
            p.retired = true;
        }
    }

    let team_count = w.teams.len();
    for ti in 0..team_count {
        let active: Vec<usize> = w.teams[ti]
            .players
            .iter()
            .copied()
            .filter(|&i| !w.pool[i].retired)
            .collect();
        w.teams[ti].players = active;
        let rep = w.teams[ti].rep;

        // Guaranteed academy intake: every club graduates a class every summer,
        // scaled by how good its academy is. The old rule only ever topped a
        // squad UP to 20, so a club that had never dipped below 22 — i.e. every
        // healthy club — produced zero club-trained players forever: the
        // registration floor had no supply, and the world's talent pool simply
        // stopped renewing once the real stars aged out.
        let academy = w.teams[ti].academy;
        let intake = match academy {
            5 => rng.gen_range(2..=3),
            3..=4 => rng.gen_range(1..=2),
            _ => usize::from(rng.gen_bool(0.65)),
        };
        // Pure anti-runaway valve (40), not a pathway gate: academies graduate
        // whether or not the XI is full, which is the whole point of the fix —
        // this only stops a pathological squad from growing without limit.
        if w.teams[ti].players.len() < 40 {
            for _ in 0..intake {
                let idx = spawn_regen(w, ti, rep, rng);
                w.teams[ti].players.push(idx);
            }
        }
        // Safety net: if retirements hollowed the squad out, replenish depth.
        let deficit = 18i32 - w.teams[ti].players.len() as i32;
        for _ in 0..deficit.max(0) {
            let idx = spawn_regen(w, ti, rep, rng);
            w.teams[ti].players.push(idx);
        }
    }
}

/// Age physiology for generated players: a 16-year-old debuts ~68, not 90 —
/// even prodigies arrive unfinished and grow into their potential through
/// minutes and coaching. Applied uniformly to every generated teenager (never
/// to real database players), so wonderkid careers still happen, just from the
/// right starting point.
fn age_cap_ovr(ovr: u8, age: u8, wonder: bool) -> u8 {
    let cap: u8 = match age {
        16 => 68,
        17 => 71,
        18 => 74,
        19 => 77,
        _ => 99,
    };
    ovr.min(cap + if wonder { 8 } else { 0 })
}

fn spawn_regen(w: &mut World, team: usize, rep: u8, rng: &mut Pcg64Mcg) -> usize {
    let pos = match rng.gen_range(0..10) {
        0 | 1 => Position::GK,
        2..=4 => Position::DEF,
        5..=7 => Position::MID,
        _ => Position::FWD,
    };
    // The academy pipeline: better academies intake stronger prospects and roll
    // wonderkids more often (La Masia-tier ~8%, a 1★ shed ~3%) — but any club
    // can occasionally produce one, so new dynasties and Messi/Ronaldo-tier
    // careers stay possible without being scheduled.
    // Tier-space strength mapping onto the 0-100 attribute scale (×3 in
    // generation): a top club's prospect debuts ~65-70, a minnow's ~30-40.
    let academy = w.teams.get(team).map(|t| t.academy).unwrap_or(3) as f32;
    let strength = ((rep as f32 / 4.0 + academy - 3.0).round() as i32).clamp(7, 26);
    let mut g = Gen::new(w.next_regen as u64 ^ rng.gen::<u64>());
    w.next_regen += 1;
    let gp = g.player(pos, strength);
    let age = rng.gen_range(16..=19) as u8;
    // Most youth top out mid-tier; the rare wonderkid reaches the elite band.
    let wonder_p = 0.02 + academy * 0.012;
    let wonder = rng.gen::<f32>() < wonder_p;
    let ovr = age_cap_ovr(gp.overall().min(w.elite_cap.saturating_sub(2)), age, wonder);
    let ceiling = if wonder {
        w.elite_cap
    } else {
        (w.elite_cap as i32 - rng.gen_range(10..=30)).max(46) as u8
    };
    let pot = ovr.max(ceiling.min(w.elite_cap));
    let nation = if w.nation_pool.is_empty() {
        String::new()
    } else {
        w.nation_pool[rng.gen_range(0..w.nation_pool.len())].clone()
    };
    // Distinct identity from an inexhaustible, nation-aware supply: no bounded
    // retries, no recycled names even a century out.
    let name = Gen::regen_name(rng, &nation, &w.used_names);
    w.used_names.insert(name.clone());
    let idx = w.pool.len();
    w.pool.push(WP {
        name,
        nation,
        pos,
        attrs: gp.attrs,
        ovr,
        pot,
        age,
        team,
        retired: false,
        morale: 70,
        fatigue: 0,
        adapt: 0,
        on_loan: false,
        // Fresh out of this club's own academy: club-trained by definition.
        homegrown: true,
        apps: 0,
        goals: 0,
        assists: 0,
        rating_sum: 0.0,
        rating_apps: 0,
        ucl_goals: 0,
        el_goals: 0,
        intl_goals: 0,
        ucl_assists: 0,
        el_assists: 0,
        intl_assists: 0,
        ucl_apps: 0,
        el_apps: 0,
        intl_apps: 0,
        shots: 0,
        sot: 0,
        xg: 0.0,
        xa: 0.0,
        aerials_won: 0,
        aerials_lost: 0,
        key_passes: 0,
        big_chances: 0,
        passes_att: 0,
        passes_cmp: 0,
        dribbles_att: 0,
        dribbles_cmp: 0,
        tackles: 0,
        interceptions: 0,
        fouls: 0,
        saves: 0,
        clean_sheets: 0,
        league_won: false,
        ucl_result: 0,
        el_result: 0,
        intl_result: 0,
        value: (market_value(ovr, age, pos) as f32 * w.inflation) as i64,
        release_clause: None,
        wage: ((market_value(ovr, age, pos) as f32 * w.inflation * 2.0 / 5.0) as i64).max(1),
        locked: 1,
        career_goals: 0,
        career_assists: 0,
        career_kp: 0,
        career_bcc: 0,
        career_ti: 0,
        career_cs: 0,
        career_xg: 0.0,
        career_xa: 0.0,
        career_aerials: 0,
        career_apps: 0,
        career_rating_sum: 0.0,
        career_rating_apps: 0,
        ballons: 0,
    });
    w.minds.push(PlayerMind::new(rng));
    idx
}

/// Find a loan home for a young surplus prospect: a strictly smaller club where
/// he'd start at his position (real minutes or no deal — parking kids helps
/// nobody), that stays under the 25-man cap itself. Relaxed mode (squad-cap
/// compliance) accepts any smaller club with room.
fn find_loan(w: &World, parent: usize, pi: usize, relaxed: bool) -> Option<usize> {
    let pos = w.pool[pi].pos;
    let rating = w.pool[pi].attrs.overall(pos) as f32;
    let parent_rep = w.teams[parent].rep;
    let mut best: Option<(usize, (u8, u8))> = None; // (club, prefer-domestic-then-rank)
    for ti in 0..w.teams.len() {
        if ti == parent || w.teams[ti].rep >= parent_rep {
            continue;
        }
        let active = w.teams[ti]
            .players
            .iter()
            .filter(|&&i| !w.pool[i].retired)
            .count();
        if active >= MAX_REGISTERED {
            continue;
        }
        if !relaxed {
            let (need, incumbent) = weakest_slot(&w.teams[ti].players, &w.pool);
            if need != pos || rating < incumbent {
                continue;
            }
        }
        let domestic = if w.teams[ti].league_idx == w.teams[parent].league_idx {
            1u8
        } else {
            0u8
        };
        // Domestic loans first, then the highest-ranked taker.
        let key = (domestic, w.teams[ti].rep);
        if best.map_or(true, |(_, bk)| key > bk) {
            best = Some((ti, key));
        }
    }
    best.map(|(ti, _)| ti)
}

/// Execute a season-long loan: the prospect moves to the loanee's list (where
/// selection picks him — list membership is authoritative), keeps his parent
/// club-trained status and contract, and returns next preseason. The loanee
/// pays a small fee (~2% of value); wages split (see wage_bill).
fn do_loan(
    w: &mut World,
    parent: usize,
    away: usize,
    pi: usize,
    moved: &mut std::collections::HashSet<usize>,
) -> String {
    let fee = (w.pool[pi].value / 50).max(1);
    w.teams[parent].players.retain(|&x| x != pi);
    w.teams[away].players.push(pi);
    w.teams[away].budget -= fee;
    w.teams[parent].budget += fee;
    w.teams[away].net_spend += fee;
    w.teams[parent].net_spend -= fee;
    w.pool[pi].team = away;
    w.pool[pi].on_loan = true;
    w.pool[pi].morale = 70;
    moved.insert(pi);
    w.loans.push(SeasonLoan { player: pi, home: parent, away });
    format!(
        "{} ({}, {}) -> {} (loan)",
        w.pool[pi].name,
        w.pool[pi].age,
        w.pool[pi].pos.short(),
        w.teams[away].name
    )
}

/// Bilateral willingness: may this player move to this buyer? Surplus men go
/// anywhere within 15 rep points (ambitious ones refuse big steps down);
/// settled starters need a clearly bigger club, a triggered clause, or a
/// transfer request (which works laterally like a soft clause). One predicate
/// for buyer- and seller-initiated paths alike, so neither can bypass it.
/// True when two clubs are genuine local or national enemies — the derbies a
/// century of history makes categorical rather than financial. A release
/// clause, a fatter wallet and a bigger badge do not buy a Clásico: the player
/// and the selling board simply refuse. Not "expensive", not "unlikely" — no.
fn is_direct_rival(club_a: &str, club_b: &str) -> bool {
    const RIVALS: &[(&str, &str)] = &[
        ("Real Madrid", "FC Barcelona"),
        ("Real Madrid", "Atlético de Madrid"),
        ("Arsenal", "Spurs"),
        ("Manchester City", "Man Utd"),
        ("Liverpool", "Man Utd"),
        ("Liverpool", "Everton"),
        ("Newcastle Utd", "Sunderland"),
        ("Milano FC", "Lombardia FC"),
        ("Juventus", "Lombardia FC"),
        ("AS Roma", "Latium"),
        ("Borussia Dortmund", "FC Bayern München"),
        ("Paris SG", "OM"),
    ];
    RIVALS.iter().any(|&(a, b)| {
        (club_a.contains(a) && club_b.contains(b)) || (club_a.contains(b) && club_b.contains(a))
    })
}

/// Will this player actually make this move? Attributes and reputation set the
/// price of a transfer; this decides whether it happens at all — the sporting
/// and personal veto that keeps the market from being a pure auction.
#[allow(clippy::too_many_arguments)]
fn transfer_willing(
    surplus: bool,
    clause_deal: bool,
    requested: bool,
    ambition: f32,
    loyalty: f32,
    homegrown: bool,
    buyer_rep: u8,
    seller_rep: u8,
    buyer_name: &str,
    seller_name: &str,
    gap_clause: f32,
    gap_starter: f32,
) -> bool {
    // 1. The derby line. Crossing a direct rival is categorical: a homegrown
    //    player never does it, and anyone else has to be genuinely mercenary —
    //    already demanding a move of his own — before loyalty to his own club
    //    stops being the deciding vote. A met clause is not an argument here,
    //    which is what stops a Clásico icon being sold across the Clásico.
    if is_direct_rival(buyer_name, seller_name) {
        if homegrown {
            return false;
        }
        if loyalty > 0.15 || !requested {
            return false;
        }
    }
    // 2. Boyhood pull. A settled club-trained starter doesn't leave the
    //    academy that raised him for a sideways or minor step up — only a real
    //    upgrade, or a request of his own, breaks that. Benched players still
    //    circulate: sitting outside the XI is its own answer.
    if !surplus && homegrown && loyalty >= 0.50 && !requested && buyer_rep <= seller_rep + 6 {
        return false;
    }
    if surplus {
        (buyer_rep as i32) + 15 >= seller_rep as i32
            && !((buyer_rep as i32) + 12 < seller_rep as i32 && ambition > 0.6)
    } else if clause_deal || requested {
        buyer_rep as f32 + gap_clause >= seller_rep as f32
    } else {
        buyer_rep as f32 >= seller_rep as f32 + gap_starter
    }
}

/// Season-long loans end here: every loanee returns to the parent before the
/// window opens, so squads (and budgets) are assessed honestly.
fn return_loans(w: &mut World) {
    if w.loans.is_empty() {
        return;
    }
    let returning = std::mem::take(&mut w.loans);
    for loan in returning {
        let pi = loan.player;
        w.teams[loan.away].players.retain(|&x| x != pi);
        if !w.pool[pi].retired && !w.teams[loan.home].players.contains(&pi) {
            w.teams[loan.home].players.push(pi);
        }
        w.pool[pi].team = loan.home;
        w.pool[pi].on_loan = false;
    }
}

/// Between-seasons transfer window (original club-management model): assign
/// each club a reputation-based budget, then let clubs (richest first) buy
/// upgrades for their weakest starting position. Fees come from market value
/// or a triggered release clause; sellers bank the fee to reinvest. Need-based
/// buying plus squad limits spread talent up the pyramid instead of letting a
/// single club hoard it. Returns headline (big-fee) moves for the report.
/// Between-seasons transfer window with realistic budget discipline. Each club
/// gets a limited, reputation-based budget (it does NOT roll over), so a club
/// can only fund a marquee signing by selling — buying a star in one area
/// spends the money it would need elsewhere. Managers with higher ambition
/// attempt more business. Settled starters only leave for a clearly bigger
/// club or on a triggered release clause, so elite players stop bouncing.
/// Returns (headline lines, deals completed, biggest fee, agent fees paid).
/// Budgets are conserved across the window: every fee a buyer pays lands in a
/// seller's books; only agent cuts leave the system (observable in net_spend).
fn transfer_window(w: &mut World, rng: &mut Pcg64Mcg) -> (Vec<String>, usize, i64, i64) {
    let mut log: Vec<(i64, String)> = Vec::new();
    let mut deals_done: usize = 0;
    let mut max_fee: i64 = 0;
    let mut agent_paid: i64 = 0;
    let mut star_lines: Vec<String> = Vec::new();
    let mut moved: std::collections::HashSet<usize> = std::collections::HashSet::new();
    // Headline fees are reported in 2026 money (deflated by cumulative
    // inflation), exactly like the finance tables — so a late-era deal reads as
    // a real ~€200m blockbuster, not a nominal "€1.4bn" that only looks like it
    // dwarfs revenue because the books are quoted in different money.
    let dfl = w.inflation.max(1.0);
    let rl = |v: i64| (v as f32 / dfl).round() as i64;
    return_loans(w);
    // Budgets come from the money loop, not reputation: one summer's allocation
    // (last season's revenue × board willingness) plus a small dividend from
    // carried cash. Reserves are a backstop for a galáctico, not a war chest
    // that buys anyone every summer — a mid-table club with no Europe simply
    // cannot outbid a superclub, and the finance table shows why.
    for t in &mut w.teams {
        // Boards with piles spend: effective willingness rises with reserves
        // (up to +0.3), so surplus converts into ambition instead of just
        // sitting there — while misers still hoard-and-siphon by character.
        let pile = (t.cash.max(0) as f32 / 1000.0).clamp(0.0, 1.0);
        let effective_will = if t.cash < -300 {
            t.willingness * 0.5
        } else {
            (t.willingness + 0.3 * pile).min(1.0)
        };
        // Insolvent clubs get no allocation at all — debt cannot finance
        // galácticos. They spend only what sales bring in (sell-to-buy below
        // still accumulates into this zeroed budget).
        // Revenue is already nominal (it tracks football inflation in
        // settle_finances), so no extra inflation factor here — that would
        // square inflation and mint nominal billions out of thin air.
        t.budget = if t.cash < 0 {
            0
        } else {
            (t.cash.max(0) as f32 * 0.15
                + t.revenue_last as f32 * effective_will * w.params.budget_ratio) as i64
        };
        t.budget_alloc = t.budget;
        // New window, new books: transfer net spend accumulates from here.
        t.net_spend = 0;
    }
    // Contracts tick down: a player committed to his club can't move yet.
    for p in &mut w.pool {
        p.locked = p.locked.saturating_sub(1);
    }
    // Proactive squad trimming: bloated (>23 active), wage-strapped (>90% of
    // the revenue cap), or cash-negative (FFP fire-sale mode: sell before you
    // can buy anything) clubs move their most valuable benched player down the
    // pyramid — stars don't rot on apex benches while mid-table defenses
    // starve on kids (the recipe behind the 70-goal seasons). Logged when big.
    for ti in 0..w.teams.len() {
        for _ in 0..2 {
            let active = w.teams[ti]
                .players
                .iter()
                .filter(|&&i| !w.pool[i].retired)
                .count();
            let over_wages = wage_bill(w, ti)
                > (wage_cap(w.teams[ti].revenue_last, &w.params) as f32 * 0.9) as i64;
            let insolvent = w.teams[ti].cash < 0;
            if active <= 23 && !over_wages && !insolvent {
                break;
            }
            match sell_surplus(w, ti, &mut moved) {
                Some((v, name, dest, ovr, age, pot)) => {
                    deals_done += 1;
                    if let Some(entry) =
                        sale_line(w.params.log_fee.round() as i64, v, rl(v), name, dest, ovr, age, pot)
                    {
                        log.push(entry);
                    }
                }
                None => break,
            }
        }
    }
    let mut order: Vec<usize> = (0..w.teams.len()).collect();
    // Shuffled acting order: no global turn sequence, each manager moves when
    // their number comes up. Ambition sets attempts, not priority.
    order.shuffle(rng);

    for &buyer in &order {
        // Scouting knowledge: a manager fully knows their own league, plus a
        // ~40% random sample of the wider world scouted this window. Targets
        // outside that knowledge simply don't exist to them.
        let buyer_league = w.teams[buyer].league_idx;
        let mut known: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for ti in 0..w.teams.len() {
            if ti == buyer {
                continue;
            }
            for &pi in &w.teams[ti].players.clone() {
                if w.teams[ti].league_idx == buyer_league || rng.gen_bool(0.40) {
                    known.insert(pi);
                }
            }
        }
        let attempts = (1.0 + w.teams[buyer].manager.ambition * 2.0).round() as i32; // 2..4
        for _ in 0..attempts {
            let (need_pos, incumbent) = weakest_slot(&w.teams[buyer].players, &w.pool);
            let buyer_rep = w.teams[buyer].rep;

            // Find the best willing upgrade at the position of need, among players
            // this manager actually knows (same league fully scouted, ~40% of
            // the wider world). Unknown players can't be bid for.
            let mut target: Option<(usize, i64, f32)> = None; // (idx, ask, rating)
            for ti in 0..w.teams.len() {
                if ti == buyer {
                    continue;
                }
                let seller_rep = w.teams[ti].rep;
                let xi = best_xi_indices(&w.teams[ti].players, &w.pool);
                for &pi in &w.teams[ti].players {
                    // Loaned players are not on the market mid-loan (recall them
                    // first in real life; here their parent simply doesn't list
                    // them, and the loanee can't sell what it doesn't own).
                    if moved.contains(&pi) || w.pool[pi].locked > 0 || !known.contains(&pi) || w.pool[pi].on_loan {
                        continue;
                    }
                    let p = &w.pool[pi];
                    if p.retired || p.pos != need_pos {
                        continue;
                    }
                    let rating = p.attrs.overall(p.pos) as f32;
                    if rating < incumbent + w.params.min_improve {
                        continue;
                    }
                    // Seller's ask: a triggered release clause is a fixed price
                    // anyone can meet, but a club with no clause won't sell a
                    // starter to a domestic *rival* cheaply — it charges a heavy
                    // premium so you can't just buy Yamal off Barça to bolster
                    // Real. Transfer-requested players still go at a discount.
                    // Triggering a RIVAL's clause costs extra on top (tapping-up
                    // battles, fan-fury premiums — the Figo tax, priced in).
                    let clause_src = p.release_clause.is_some();
                    let mut ask = match p.release_clause {
                        Some(rc) => rc,
                        None => {
                            let mut a = (p.value as f32 * 1.25) as i64;
                            let same_league = w.teams[buyer].league_idx == w.teams[ti].league_idx;
                            let rival = same_league
                                && seller_rep >= 80
                                && buyer_rep as i32 <= seller_rep as i32 + 3;
                            if rival {
                                a = (a as f32 * 1.7) as i64;
                            }
                            a
                        }
                    };
                    if clause_src {
                        let same_league = w.teams[buyer].league_idx == w.teams[ti].league_idx;
                        if same_league
                            && seller_rep >= 80
                            && buyer_rep as i32 <= seller_rep as i32 + 3
                        {
                            ask = (ask as f32 * 1.6) as i64;
                        }
                    }
                    if w.minds[pi].requested {
                        ask = (ask as f32 * w.params.req_discount) as i64;
                    }
                    if ask < 1 {
                        continue;
                    }
                    // Nothing a club may not legally buy: the FFP-style ceiling
                    // (35% of turnover, fee + agent) is applied at *selection*
                    // time, so a symbolic billion-euro buy-out can't monopolise
                    // a need slot and leave the buyer walking away with nobody
                    // — he simply moves to the next man he can afford.
                    let ask_agent = (ask as f32 * w.params.agent_fee) as i64;
                    if ask + ask_agent > (w.teams[buyer].revenue_last as f32 * 0.35) as i64 {
                        continue;
                    }
                    let clause_deal = p.release_clause.map_or(false, |rc| ask >= rc);
                    // Benched/surplus players are sellable to any club that meets
                    // the fee — unwanted players move freely, which keeps talent
                    // circulating instead of rotting on apex benches.
                    let surplus = !xi.contains(&pi);
                    // Surplus (benched) player: any equal/bigger club, or a
                    // moderately smaller one where he'd actually start (veteran
                    // / rotation players drop down for minutes — this keeps
                    // talent circulating instead of piling up at apex clubs).
                    let willing = transfer_willing(
                        surplus,
                        clause_deal,
                        w.minds[pi].requested,
                        w.minds[pi].ambition,
                        w.minds[pi].loyalty,
                        w.pool[pi].homegrown,
                        buyer_rep,
                        seller_rep,
                        &w.teams[buyer].name,
                        &w.teams[ti].name,
                        w.params.gap_clause,
                        w.params.gap_starter,
                    );
                    if !willing {
                        continue;
                    }
                    // Veteran penalty in target selection: at equal raw ratings
                    // a 25yo prospect beats a 31yo backup by a clear margin —
                    // clubs buy futures, not pasts (no more Karius-to-PSG
                    // seasons). Thirty is the cliff edge, then 2 points a year.
                    let score = rating - (p.age.saturating_sub(30) as f32) * 2.0;
                    if target.map_or(true, |(_, _, br)| score > br) {
                        target = Some((pi, ask, score));
                    }
                }
            }

            let (pi, ask, _rating) = match target {
                Some(t) => t,
                None => break, // nobody worth signing at the need position
            };

            // Squad registration has teeth at acquisition time: a full 25-man
            // squad must move someone out before buying — sell-to-buy for
            // space, exactly like the wage check below does for money.
            let active_here = w.teams[buyer]
                .players
                .iter()
                .filter(|&&i| !w.pool[i].retired)
                .count();
            if active_here >= MAX_REGISTERED && sell_surplus(w, buyer, &mut moved).is_none() {
                continue; // no room and nothing movable — deal dead
            }

            // Bilateral negotiation between the two clubs' agents. The buyer
            // opens below ask (trying a deal, openness varies); the seller
            // accepts at their threshold — 95%, or lower for transfer requests
            // and cash-strapped sellers. On rejection the buyer counters once
            // at full ask when affordable (a seller never turns down their own
            // asking price); otherwise the deal is dead this window.
            let seller = w.pool[pi].team;
            let seller_threshold =
                if w.minds[pi].requested || w.teams[seller].budget < 20 { w.params.sell_distress } else { w.params.sell_base };
            let mut fee = (ask as f32 * w.params.open_pct) as i64;
            let mut agreed = fee as f32 >= ask as f32 * seller_threshold;
            if !agreed && (ask as f32) <= w.teams[buyer].budget as f32 {
                fee = ask; // one improved bid, take it or leave it
                agreed = true;
            }
            if !agreed {
                continue;
            }

            // Financial Sustainability gate (UEFA-style squad-cost thinking):
            // no single deal may cost more than 35% of the buyer's annual
            // revenue (fee + agent). Galácticos stay possible for true giants —
            // ~35% of a superclub's turnover is still a world-record fee — but
            // a club spending near its whole revenue on one man is impossible,
            // whatever the war chest. Revenue is nominal, so the ceiling tracks
            // inflation and the cap stays a constant share of real turnover.
            let agent_est = (fee as f32 * w.params.agent_fee) as i64;
            if fee + agent_est > (w.teams[buyer].revenue_last as f32 * 0.35) as i64 {
                continue;
            }

            // Direct-rival refusal: a settled starter never joins a domestic
            // rival at a sane price (post-Figo embargo politics) — only a
            // superstar ransom talks (3× market value, 5× at the superclasico
            // tier where both clubs sit 88+). Surplus players move freely;
            // unwanted men have no embargo — EXCEPT across a superclasico,
            // where even the unwanted don't cross directly below ransom.
            // Requested players are equally bound: wanting out doesn't grant
            // a free pass to the enemy (they still have the whole market).
            {
                let same_league = w.teams[buyer].league_idx == w.teams[seller].league_idx;
                let seller_rep = w.teams[seller].rep;
                let rival = same_league
                    && seller_rep >= 80
                    && (buyer_rep as i32 - seller_rep as i32).abs() <= 6;
                let superclasico = rival && seller_rep >= 88 && buyer_rep >= 88;
                let settled = best_xi_indices(&w.teams[seller].players, &w.pool).contains(&pi);
                let ransom_mult = if superclasico { 5.0 } else { 3.0 };
                let bound = superclasico || (rival && settled && !w.minds[pi].requested);
                if bound
                    && (w.pool[pi].value as f32 * ransom_mult) as i64 > fee.max(ask)
                {
                    continue;
                }
            }

            // The buying manager's policy network has the final say. Inputs are
            // the same signals a manager weighs — need, value, wage room, own
            // ambition, reputation gap, request status — so identical deals pass
            // for one individual and fail for another. Genuine need (a big gap
            // at the position) forces even a sceptical network's hand.
            let need = ((w.pool[pi].attrs.overall(w.pool[pi].pos) as f32 - incumbent) / 10.0)
                .clamp(0.0, 1.0);
            let value_fit =
                (1.0 - fee as f32 / (w.teams[buyer].budget.max(1)) as f32).clamp(0.0, 1.0);
            let wage_room = (1.0
                - wage_bill(w, buyer) as f32 / (wage_cap(w.teams[buyer].revenue_last, &w.params).max(1)) as f32)
                .clamp(0.0, 1.0);
            let rep_gap = ((buyer_rep as f32 - w.teams[seller].rep as f32) / 20.0 * 0.5 + 0.5)
                .clamp(0.0, 1.0);
            let inputs = [
                need,
                value_fit,
                wage_room,
                (w.teams[buyer].manager.ambition / 1.5).clamp(0.0, 1.0),
                rep_gap,
                if w.minds[pi].requested { 1.0 } else { 0.0 },
            ];
            if need < 0.4 && net::forward(&w.params.policy, &inputs) <= 0.5 {
                continue;
            }

            // Fund the deal: sell the most valuable benched player if short.
            // (The seller banks the fee inside sell_surplus.)
            if fee > w.teams[buyer].budget {
                if let Some(sale) = sell_surplus(w, buyer, &mut moved) {
                    if sale.0 >= w.params.log_fee.round() as i64 {
                        let to = sale.2.clone().unwrap_or_else(|| "smaller club".to_string());
                        log.push((sale.0, format!("{} sold by {} to {} ({}m)", sale.1, w.teams[buyer].name, to, rl(sale.0))));
                    }
                }
            }
            if fee > w.teams[buyer].budget {
                continue; // cannot fund it even after selling
            }

            // Wages + agent fee: signing must fit the wage bill, and the buyer
            // pays a 10% agent fee on top of the fee. If the wage bill would
            // burst, the club must offload first (or the deal collapses).
            let agent = (fee as f32 * w.params.agent_fee) as i64;
            let cap = wage_cap(w.teams[buyer].revenue_last, &w.params);
            let new_wage = w.pool[pi].wage;
            if wage_bill(w, buyer) + new_wage > cap {
                // Try to free wages by selling a benched player; else abort.
                if let Some(sale) = sell_surplus(w, buyer, &mut moved) {
                    if sale.0 >= w.params.log_fee.round() as i64 {
                        let to = sale.2.clone().unwrap_or_else(|| "smaller club".to_string());
                        log.push((sale.0, format!("{} sold by {} to {} ({}m)", sale.1, w.teams[buyer].name, to, rl(sale.0))));
                    }
                }
                if wage_bill(w, buyer) + new_wage > cap {
                    continue; // can't fit the wages — walk away
                }
            }
            if w.teams[buyer].budget < fee + agent {
                continue; // agent fee tips it over budget
            }

            // Execute the transfer.
            let seller = w.pool[pi].team;
            w.teams[seller].players.retain(|&x| x != pi);
            w.teams[seller].budget += fee;
            w.teams[buyer].players.push(pi);
            w.teams[buyer].budget -= fee + agent;
            w.teams[buyer].net_spend += fee + agent;
            w.teams[seller].net_spend -= fee;
            agent_paid += agent;
            w.pool[pi].team = buyer;
            // A permanent move away ends club-trained status here.
            w.pool[pi].homegrown = false;
            moved.insert(pi);
            // New multi-year contract: commit the player and drop the easy
            // release clause, so stars settle instead of moving every summer.
            w.pool[pi].locked = 3 + (rng.gen::<u8>() % 3); // 3..=5 year deal
            w.pool[pi].release_clause = None;
            let (ovr, age) = (w.pool[pi].ovr, w.pool[pi].age);
            w.pool[pi].value = (market_value(ovr, age, w.pool[pi].pos) as f32 * w.inflation) as i64;
            // Greedy players negotiate a premium on top of the standard wage.
            w.pool[pi].wage = (((w.pool[pi].value * 2 / 5).max(1) as f32) * w.minds[pi].wage_mult()) as i64;
            // A completed move settles the player: request cleared, mood restored.
            w.minds[pi].requested = false;
            w.minds[pi].unhappy = false;
            w.pool[pi].morale = 75;
            if fee >= w.params.log_fee.round() as i64 {
                log.push((fee, format!("{} -> {} ({}m, +{}m agent)", w.pool[pi].name, w.teams[buyer].name, rl(fee), rl(agent))));
            }
            // Star moves are always traceable, whatever the fee: a sub-threshold
            // 82+ move must never become a ghost in the career tables.
            if ovr >= 82 && fee < w.params.log_fee.round() as i64 {
                star_lines.push(format!("{} -> {} ({}m)", w.pool[pi].name, w.teams[buyer].name, rl(fee)));
            }
            // Same for elite prospects: a teenager with a superstar ceiling
            // moving cheap is tomorrow's "where did he come from?" mystery.
            if ovr < 82
                && w.pool[pi].age <= 21
                && w.pool[pi].pot >= 85
                && fee < w.params.log_fee.round() as i64
            {
                star_lines.push(format!(
                    "{} ({}, pot {}) -> {} ({}m, prospect)",
                    w.pool[pi].name,
                    w.pool[pi].age,
                    w.pool[pi].pot,
                    w.teams[buyer].name,
                    rl(fee)
                ));
            }
            // Count every completed deal (not just logged headlines) for fitness.
            deals_done += 1;
            if fee > max_fee {
                max_fee = fee;
            }
        }
    }
    // ── Season-long loans ──
    // Big clubs develop prospects on someone else's minutes; small clubs get a
    // starter they could never afford. Capped per club so markets don't flood.
    let mut loan_lines: Vec<String> = Vec::new();
    {
        let mut out_count = vec![0u8; w.teams.len()];
        let mut in_count = vec![0u8; w.teams.len()];
        for parent in 0..w.teams.len() {
            // Young surplus prospects with real headroom, youngest first.
            // Locked contracts don't block loans (the parent keeps ownership).
            // Fringe kids (a few league apps, never established) qualify even
            // inside the XI fringe: they need a full season of starts somewhere.
            // Preseason (zero apps) is surplus-only — starters don't go out.
            let xi = xi_of(w, parent);
            let mut cands: Vec<usize> = w.teams[parent]
                .players
                .iter()
                .copied()
                .filter(|&pi| {
                    (!xi.contains(&pi) || (w.pool[pi].apps > 0 && w.pool[pi].apps < 12))
                        && !moved.contains(&pi)
                        && !w.pool[pi].retired
                        && !w.pool[pi].on_loan
                        && !w.minds[pi].requested
                        && w.pool[pi].age <= 21
                        && w.pool[pi].pot.saturating_sub(w.pool[pi].ovr) >= 5
                })
                .collect();
            cands.sort_by_key(|&pi| w.pool[pi].age);
            for pi in cands {
                if out_count[parent] >= 3 {
                    break;
                }
                let Some(away) = find_loan(w, parent, pi, false) else {
                    continue;
                };
                if in_count[away] >= 2 {
                    continue;
                }
                loan_lines.push(do_loan(w, parent, away, pi, &mut moved));
                out_count[parent] += 1;
                in_count[away] += 1;
                deals_done += 1;
            }
        }
    }
    // ── Squad registration ──
    // Over-25 squads comply first: a youngster goes out on lenient loan terms,
    // else the surplus is sold; whatever remains past that is a board failure
    // the next window inherits. Then every club registers its best 25 with the
    // homegrown floor enforced.
    for ti in 0..w.teams.len() {
        loop {
            let active = w.teams[ti]
                .players
                .iter()
                .filter(|&&i| !w.pool[i].retired)
                .count();
            if active <= MAX_REGISTERED {
                break;
            }
            // Youngest non-starter goes first (veterans get sold, not parked).
            let xi = xi_of(w, ti);
            let kid = w.teams[ti]
                .players
                .iter()
                .copied()
                .filter(|&pi| {
                    !xi.contains(&pi)
                        && !moved.contains(&pi)
                        && !w.pool[pi].retired
                        && !w.pool[pi].on_loan
                })
                .min_by_key(|&pi| w.pool[pi].age);
            let mut progressed = false;
            if let Some(pi) = kid {
                if let Some(away) = find_loan(w, ti, pi, true) {
                    loan_lines.push(do_loan(w, ti, away, pi, &mut moved));
                    deals_done += 1;
                    progressed = true;
                }
            }
            if !progressed {
                match sell_surplus(w, ti, &mut moved) {
                    Some(_) => {
                        deals_done += 1;
                    }
                    None => break,
                }
            }
        }
        w.teams[ti].registered = register_squad(&w.teams[ti].players, &w.pool);
    }
    log.sort_by(|a, b| b.0.cmp(&a.0));
    let mut lines: Vec<String> = log.into_iter().take(8).map(|(_, s)| s).collect();
    lines.extend(loan_lines.into_iter().take(3));
    lines.extend(star_lines.into_iter().take(4));
    (lines, deals_done, max_fee, agent_paid)
}

/// A club's weekly wage bill (sum of active players' wages). A player out on
/// loan counts half here (at the club that lists him): the standard split is
/// that the parent keeps subsidising part of the deal off these books.
fn wage_bill(w: &World, club: usize) -> i64 {
    w.teams[club]
        .players
        .iter()
        .filter(|&&i| !w.pool[i].retired)
        .map(|&i| {
            if w.pool[i].on_loan {
                w.pool[i].wage / 2
            } else {
                w.pool[i].wage
            }
        })
        .sum()
}

/// Wage capacity as a share of revenue (tuned ratio; real wage-to-turnover
/// ratios run 50-70%, UEFA's squad-cost ceiling is 70% from 2025/26). Derived
/// from the club's own last revenue — never reputation — so a rich club with a
/// stacked squad still has to sell before signing, and a poor club can't
/// sustain galáctico wages even one summer.
fn wage_cap(revenue_last: i64, p: &Params) -> i64 {
    (revenue_last as f32 * p.wage_ratio).max(4.0) as i64
}

/// Settle the money loop at season's end: league prizes, European money,
/// operating costs and wages flow into cash and revenue, and wealth drifts on
/// sustained success. Called with the finished tables, before the transfer
/// window that spends the proceeds.
fn settle_finances(w: &mut World, tables: &[Vec<usize>], rng: &mut Pcg64Mcg) {
    // League position → index in table.
    let mut pos_of: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for table in tables {
        for (pos, &t) in table.iter().enumerate() {
            pos_of.insert(t, pos);
        }
    }
    for t in 0..w.teams.len() {
        // Best European run anywhere in the squad (set per player in run_cup).
        let mut euro_ucl = 0u8;
        let mut euro_el = 0u8;
        for &pi in &w.teams[t].players {
            euro_ucl = euro_ucl.max(w.pool[pi].ucl_result);
            euro_el = euro_el.max(w.pool[pi].el_result);
        }
        // League prize money: champions take ~140, sliding to ~15 for the
        // bottom half — in the same abstract millions as fees and wages.
        // All nominal: base and prizes scale with football inflation, exactly
        // like wages and fees do. Without this, a fixed 2026-money revenue
        // against inflating wages guarantees every giant ends at the -500
        // debt floor by the 2060s while nominal budgets parade as billions.
        let pos = pos_of.get(&t).copied().unwrap_or(10);
        let league_prize = match pos {
            0 => 140.0,
            1 => 115.0,
            2 => 95.0,
            3 => 80.0,
            4 => 65.0,
            5 => 50.0,
            6..=9 => 35.0,
            _ => 15.0,
        };
        // European money dwarfs domestic prizes at the sharp end.
        let ucl_prize = match euro_ucl {
            5 => 160.0,
            4 => 110.0,
            3 => 75.0,
            2 => 45.0,
            1 => 22.0,
            _ => 0.0,
        };
        let el_prize = match euro_el {
            5 => 55.0,
            4 => 35.0,
            _ => 0.0,
        };
        let infl = w.inflation.max(1.0);
        // Brand revenue floor: a club's commercial pull (shirts, tours, global
        // fanbase) outlasts sporting slumps, so base revenue never falls below
        // ~55% of its heritage-level turnover. This is what lets a fallen Man
        // United or Arsenal keep spending power to rebuild instead of spiralling
        // to minnow income and vanishing from the title race for 50 years.
        // Commercial floor anchored to 80% of heritage revenue: a fallen giant
        // keeps most of its global brand through a sporting dip, the way
        // Barcelona, Liverpool and United actually do. At 55% a single bad year
        // cut the commercial base by nearly half, which forced distress sales,
        // which caused the next bad year — a self-fulfilling decline.
        let brand_floor = revenue(w.teams[t].heritage) * 0.80;
        let base = revenue(w.teams[t].wealth).max(brand_floor) * infl;
        let revenue = (base + (league_prize + ucl_prize + el_prize) * infl) as i64;
        let wages = wage_bill(w, t);
        let operating = (base * 0.25) as i64;
        w.teams[t].revenue_last = revenue;
        // Audit trail: the season's money, stream by stream (nominal — the
        // report deflates by inflation for honest cross-era comparison).
        w.teams[t].rev_base = base as i64;
        w.teams[t].prize_league = (league_prize * infl) as i64;
        w.teams[t].prize_euro = ((ucl_prize + el_prize) * infl) as i64;
        // Unspent budgets mostly revert — boards reclaim war chests, so piles
        // can't ratchet forever on rollover alone.
        w.teams[t].cash = (w.teams[t].cash + (w.teams[t].budget as f32 * 0.3) as i64).clamp(-500, 1500);
        w.teams[t].cash += revenue - wages - operating;
        // Treasury management: boards sweep everything above their reserve
        // target (300m + ambition × 700m) into dividends, infrastructure and
        // debt service — ambitious boards keep big war chests, misers siphon
        // almost everything above a small reserve (the Glazer mechanism). Cash
        // therefore differs by character instead of pinning at the cap for
        // everyone, while the cap itself stays as a pure backstop.
        let reserve = 300.0 + w.teams[t].willingness * 700.0;
        if w.teams[t].cash as f32 > reserve {
            w.teams[t].cash = reserve as i64;
        }
        // Debt compounds: borrowing to compete is a spiral, not a strategy.
        if w.teams[t].cash < 0 {
            w.teams[t].cash += (w.teams[t].cash as f32 * 0.10) as i64;
        }
        w.teams[t].cash = w.teams[t].cash.clamp(-500, 1500);
        // A club that ends the season in the red has no spending capacity until
        // it trades back into the black: zero its reported allocation so the
        // finance table never shows "-500m cash, 600m budget". The upcoming
        // window will re-derive a real budget only once cash is positive again
        // — debt genuinely cannot fund signings, in the books and on paper.
        if w.teams[t].cash < 0 {
            w.teams[t].budget_alloc = 0;
            w.teams[t].budget = 0;
        }
        // Wealth follows sustained success, with diminishing returns at the
        // top (empires are expensive to maintain) and real decline paths:
        // obscurity erodes, missing Europe entirely costs, and financial
        // distress drags stature down with it. Mean-reversion always applies,
        // so money never fully decouples from sporting stature.
        let mut dw = 0i32;
        if pos == 0 {
            dw += if w.teams[t].wealth < 90 { 2 } else { 1 };
        } else if pos < 4 {
            dw += 1;
        } else if pos >= 14 {
            dw -= 2;
        } else if pos > 10 && euro_ucl == 0 && euro_el == 0 {
            dw -= 1;
        }
        if euro_ucl >= 4 {
            dw += 2;
        } else if euro_ucl >= 2 {
            dw += 1;
        } else if euro_el >= 4 {
            dw += 1;
        }
        if w.teams[t].cash < -200 {
            dw -= 1;
        }
        // Empire maintenance: holding 95+ wealth without winning the league
        // costs two points — staying on top is expensive (wage demands,
        // complacency, every opponent's cup final), so supremacy must be
        // re-earned yearly instead of parked forever.
        if w.teams[t].wealth >= 95 && pos != 0 {
            dw -= 2;
        }
        {
            let wt = (w.teams[t].wealth as i32 + dw).clamp(20, 99) as f32;
            let rt = w.teams[t].rep as f32;
            w.teams[t].wealth = (wt + (rt - wt) * 0.08).round().clamp(20.0, 99.0) as u8;
        }
        // Outsiders play no domestic league, so league-finish drift never
        // touches them — without this, a twice-champion Porto would still read
        // "rep 65" forever while playing like elites. Their reputation follows
        // their money instead (which does respond to European runs), plus an
        // immediate bump for conquering Europe at all.
        if w.teams[t].league_idx == 5 {
            let wt = w.teams[t].wealth as f32;
            let rt = w.teams[t].rep as f32;
            w.teams[t].rep = (rt + (wt - rt) * 0.20).round().clamp(20.0, 99.0) as u8;
            if euro_ucl >= 4 {
                w.teams[t].rep = w.teams[t].rep.max(78);
            }
        }
        // Academy investment follows the money, slowly: rich clubs build
        // elite academies over the years, broke ones let theirs rot — so new
        // money eventually produces its own wonderkids, compounding a rise.
        let rev = w.teams[t].revenue_last;
        let ac = w.teams[t].academy;
        if rev > 450 && ac < 5 && rng.gen_bool(0.25) {
            w.teams[t].academy += 1;
        } else if rev < 120 && ac > 1 && rng.gen_bool(0.20) {
            w.teams[t].academy -= 1;
        } else if rev >= 120 && ac < 5 && rng.gen_bool(0.04) {
            w.teams[t].academy += 1;
        }
    }
}

/// Format one surplus/cascade sale for the log: fee-gated headlines plus
/// unconditional tracing for stars and elite prospects, so no career move
/// ever goes off-camera. `v` is the nominal fee (the fee-sorted log key);
/// `disp` is the same fee deflated to 2026 money for display. Returns
/// (nominal fee, line).
fn sale_line(
    log_fee: i64,
    v: i64,
    disp: i64,
    name: String,
    dest: Option<String>,
    ovr: u8,
    age: u8,
    pot: u8,
) -> Option<(i64, String)> {
    let to = dest.unwrap_or_else(|| "smaller club".to_string());
    if v >= log_fee || ovr >= 82 {
        Some((v, format!("{} -> {} ({}m)", name, to, disp)))
    } else if age <= 21 && pot >= 85 {
        Some((
            v,
            format!(
                "{} ({}, pot {}) -> {} ({}m, prospect)",
                name, age, pot, to, disp
            ),
        ))
    } else {
        None
    }
}

/// Sell a club's most valuable benched (non-starter) player to free agency for
/// cash. Returns (fee, name, destination, overall, age, potential). Thins
/// squad depth — the cost of funding a buy.
fn sell_surplus(
    w: &mut World,
    club: usize,
    moved: &mut std::collections::HashSet<usize>,
) -> Option<(i64, String, Option<String>, u8, u8, u8)> {
    let xi = best_xi_indices(&selectable(&w.teams, club), &w.pool);
    let registered = w.teams[club].registered.clone();
    let mut best: Option<(usize, i64)> = None;
    let mut best_unreg: Option<(usize, i64)> = None;
    for &pi in &w.teams[club].players {
        // Never sell another club's player, and never cash in a club-trained
        // youngster — prospects go out on loan, not onto the market.
        if xi.contains(&pi)
            || moved.contains(&pi)
            || w.pool[pi].retired
            || w.pool[pi].on_loan
            || (w.pool[pi].homegrown && w.pool[pi].age <= 21)
        {
            continue;
        }
        let v = (w.pool[pi].value as f32 * 0.85) as i64;
        // Unregistered dead weight goes first — it can't play anyway — then
        // the most valuable benched player as before.
        if !registered.contains(&pi) {
            if best_unreg.map_or(true, |(_, bv)| v > bv) {
                best_unreg = Some((pi, v));
            }
        } else if best.map_or(true, |(_, bv)| v > bv) {
            best = Some((pi, v));
        }
    }
    let (pi, v) = best_unreg.or(best)?;
    let pos = w.pool[pi].pos;
    let rating = w.pool[pi].attrs.overall(pos) as f32;
    let seller_rep = w.teams[club].rep;

    // Find the player a smaller home FIRST: a smaller club that would upgrade
    // its weakest starter at his position signs him (talent flows down). If
    // nobody can take him, he stays as depth — no destruction of good players
    // just because budgets bind league-wide.
    let mut dest: Option<usize> = None;
    for ti in 0..w.teams.len() {
        if ti == club || w.teams[ti].rep >= seller_rep {
            continue;
        }
        // No dumping stars to minnows: the destination must sit within 15 rep
        // points below the seller. And ambitious players refuse big steps down
        // (a Saka doesn't drop to a relegation fight; a loyal veteran winding
        // down may).
        if w.teams[ti].rep + 15 < seller_rep {
            continue;
        }
        if w.teams[ti].rep + 12 < seller_rep && w.minds[pi].ambition > 0.6 {
            continue;
        }
        let (need, incumbent) = weakest_slot(&w.teams[ti].players, &w.pool);
        if need == pos && rating > incumbent + 1.0 && w.teams[ti].budget >= v {
            match dest {
                Some(d) if w.teams[d].rep >= w.teams[ti].rep => {}
                _ => dest = Some(ti),
            }
        }
    }
    let ti = dest?;
    w.teams[club].players.retain(|&x| x != pi);
    w.teams[ti].budget -= v;
    // The seller banks the fee here, in every path — money is conserved by
    // construction, and no call site may drop it.
    w.teams[club].budget += v;
    w.teams[ti].players.push(pi);
    w.pool[pi].team = ti;
    w.pool[pi].homegrown = false;
    w.teams[ti].net_spend += v;
    w.teams[club].net_spend -= v;
    w.pool[pi].locked = 2 + (moved.len() as u8 % 3);
    w.pool[pi].value = market_value(w.pool[pi].ovr, w.pool[pi].age, w.pool[pi].pos);
    w.pool[pi].wage = (w.pool[pi].value * 2 / 5).max(1);
    moved.insert(pi);
    Some((
        v,
        w.pool[pi].name.clone(),
        Some(w.teams[ti].name.clone()),
        w.pool[pi].ovr,
        w.pool[pi].age,
        w.pool[pi].pot,
    ))
}




fn run_cup(field: &[usize], comp: Comp, teams: &mut [Team], pool: &mut [WP], p: &Params, rng: &mut Pcg64Mcg) -> Option<usize> {
    if field.len() < 8 {
        return None;
    }
    // Recovery between cup games (the league gets a midweek rest every round;
    // without this, +22 fatigue per game with no rest leaves everyone at 100
    // by the knockouts — differences compress, stars get benched, and UCL
    // scoring collapses relative to the league for purely calendrical reasons).
    let rest_field = |teams: &mut [Team], pool: &mut [WP], p: &Params| {
        for &t in field {
            for &pi in &teams[t].players {
                pool[pi].fatigue = pool[pi]
                    .fatigue
                    .saturating_sub(p.fatigue_rest.round() as u8);
            }
        }
    };
    // Outsiders play domestic seasons we don't simulate — without this they'd
    // enter every European tie unnaturally fresh while league clubs carry 38
    // games in their legs. Phantom weekend load first, then the breather.
    for &t in field {
        if teams[t].league_idx == 5 {
            for &pi in &teams[t].players.clone() {
                let f = &mut pool[pi].fatigue;
                *f = (*f).max(35);
            }
        }
    }
    rest_field(teams, pool, p); // pre-tournament breather, like a winter break

    // Swiss league phase (the real 2024+ format, not the old groups): every team
    // plays 8 games against 8 different opponents (4 home, 4 away) drawn from
    // across the pots, then one table ranks everyone and the top 16 advance.
    // More games for every qualifier (8, not 6) means the scoring charts have
    // the volume for genuine 8-12-goal European campaigns.
    // Circle-method rounds give distinct opponents each round; the first 8
    // rounds are everyone's 8 fixtures. Odd fields sit one out per round.
    let mut seeded = field.to_vec();
    seeded.sort_by(|&a, &b| teams[b].strength.partial_cmp(&teams[a].strength).unwrap());
    // Snake the strength order (1st, last, 2nd, second-last…) so the opening
    // rounds pair giants with outsiders — the pot diversity of the real draw,
    // giving favourites their farmers instead of eight straight coin-flips.
    let mut ring: Vec<usize> = Vec::with_capacity(seeded.len());
    let (mut lo, mut hi) = (0usize, seeded.len());
    let mut take_top = true;
    while lo < hi {
        if take_top {
            ring.push(seeded[lo]);
            lo += 1;
        } else {
            hi -= 1;
            ring.push(seeded[hi]);
        }
        take_top = !take_top;
    }
    if ring.len() % 2 == 1 {
        ring.push(usize::MAX); // bye marker
    }
    let mut pts: HashMap<usize, i32> = field.iter().map(|&t| (t, 0)).collect();
    let mut gd: HashMap<usize, i32> = field.iter().map(|&t| (t, 0)).collect();
    for round in 0..8 {
        let m = ring.len() / 2;
        for i in 0..m {
            let (a, b) = (ring[i], ring[ring.len() - 1 - i]);
            if a == usize::MAX || b == usize::MAX {
                continue;
            }
            // Alternate home advantage by round so everyone gets ~4 and ~4.
            let (home, away) = if round % 2 == 0 { (a, b) } else { (b, a) };
            let (hg, ag) = play(home, away, comp, Venue::HomeA, (0, 0), teams, pool, p, rng);
            *gd.get_mut(&home).unwrap() += hg as i32 - ag as i32;
            *gd.get_mut(&away).unwrap() += ag as i32 - hg as i32;
            match hg.cmp(&ag) {
                std::cmp::Ordering::Greater => *pts.get_mut(&home).unwrap() += 3,
                std::cmp::Ordering::Less => *pts.get_mut(&away).unwrap() += 3,
                std::cmp::Ordering::Equal => {
                    *pts.get_mut(&home).unwrap() += 1;
                    *pts.get_mut(&away).unwrap() += 1;
                }
            }
        }
        // Rotate all but the first entry (standard circle method).
        if ring.len() > 2 {
            let last = ring.pop().unwrap();
            ring.insert(1, last);
        }
        rest_field(teams, pool, p);
    }

    let mut advancing: Vec<usize> = field.to_vec();
    advancing.sort_by(|&a, &b| pts[&b].cmp(&pts[&a]).then(gd[&b].cmp(&gd[&a])));
    advancing.truncate(16);

    let set_result = |teams: &[Team], pool: &mut [WP], t: usize, r: u8| {
        for &pi in &teams[t].players {
            let cur = match comp {
                Comp::Europa => pool[pi].el_result,
                _ => pool[pi].ucl_result,
            };
            if cur < r {
                match comp {
                    Comp::Europa => pool[pi].el_result = r,
                    _ => pool[pi].ucl_result = r,
                }
            }
        }
    };
    for &t in &advancing {
        set_result(teams, pool, t, 1);
    }
    advancing.shuffle(rng);

    let mut round = advancing;
    let mut tag = 2u8; // R16 winners reach the QF, QF winners the SF, …
    while round.len() > 2 {
        let mut next = Vec::new();
        let mut i = 0;
        while i + 1 < round.len() {
            let win = knockout_tie(round[i], round[i + 1], comp, teams, pool, p, rng);
            set_result(teams, pool, win, tag);
            next.push(win);
            i += 2;
        }
        // Odd team out rides a bye into the next round.
        if i < round.len() {
            next.push(round[i]);
        }
        round = next;
        tag += 1;
        rest_field(teams, pool, p);
    }
    // The final is a single leg at a neutral venue — no aggregate to manage,
    // both sides go for it, as in reality.
    if round.len() == 2 {
        let (a, b) = (round[0], round[1]);
        let asel = selectable(teams, a);
        let bsel = selectable(teams, b);
        let hmd = matchday_squad(&asel, pool, teams[a].shape, &teams[a].manager.clone(), comp, rng);
        let amd = matchday_squad(&bsel, pool, teams[b].shape, &teams[b].manager.clone(), comp, rng);
        let winner = if hmd.is_empty() || amd.is_empty() {
            if rng.gen::<f32>() < 0.5 { a } else { b }
        } else {
            let axi = build_lineup(&hmd, pool);
            let bxi = build_lineup(&amd, pool);
            let mut side_a = Side { players: &axi, ambition: teams[a].manager.ambition, quality: teams[a].manager.quality, threat: 1.0 };
            let mut side_b = Side { players: &bxi, ambition: teams[b].manager.ambition, quality: teams[b].manager.quality, threat: 1.0 };
            let (ta, tb) = matchsim::threat_ratio(&side_a, &side_b);
            side_a.threat = ta;
            side_b.threat = tb;
            let o = matchsim::simulate(
                &side_a,
                &side_b,
                Venue::Neutral,
                (0, 0),
                rng,
            );
            apply_match(&o, comp, pool, p);
            if o.goals_a == o.goals_b {
                // Level after 90': stronger shootout nerve (keeper + composure)
                // edges it — still the players, not a coin flip.
                let nerve = |xi: &[MatchPlayer]| -> f32 {
                    xi.iter()
                        .map(|mp| {
                            mp.attrs.goalkeeping as f32 * 0.3 + mp.attrs.composure as f32 * 0.7
                        })
                        .sum::<f32>()
                };
                if rng.gen::<f32>() < nerve(&axi) / (nerve(&axi) + nerve(&bxi)).max(1.0) {
                    a
                } else {
                    b
                }
            } else if o.goals_a > o.goals_b {
                a
            } else {
                b
            }
        };
        let loser = if winner == a { b } else { a };
        set_result(teams, pool, winner, 5);
        set_result(teams, pool, loser, 4);
        Some(winner)
    } else {
        round.first().copied()
    }
}

// ── National teams + tournaments ───────────────────────────────────────

fn build_nations(pool: &[WP], kind: IntlKind) -> Vec<Nation> {
    let euro_only = kind == IntlKind::Euros;
    let mut by_nation: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, w) in pool.iter().enumerate() {
        if w.retired {
            continue;
        }
        let n = w.nation.trim();
        if n.len() < 2 {
            continue;
        }
        if euro_only && !EUROPE.iter().any(|e| e.eq_ignore_ascii_case(n)) {
            continue;
        }
        by_nation.entry(n.to_string()).or_default().push(i);
    }
    let mut nations: Vec<Nation> = Vec::new();
    for (name, mut idxs) in by_nation {
        idxs.sort_by(|&a, &b| pool[b].ovr.cmp(&pool[a].ovr));
        idxs.truncate(23);
        if idxs.len() < 15 {
            continue;
        }
        let mut ov: Vec<f32> = idxs.iter().map(|&i| pool[i].ovr as f32).collect();
        ov.sort_by(|a, b| b.partial_cmp(a).unwrap());
        let strength = ov.iter().take(11).sum::<f32>() / 11.0;
        nations.push(Nation { name, players: idxs, strength });
    }
    nations.sort_by(|a, b| b.strength.partial_cmp(&a.strength).unwrap());
    let cap = if euro_only { 24 } else { 32 };
    nations.truncate(cap);
    nations
}

fn nation_xi(players: &[usize], pool: &[WP]) -> Vec<MatchPlayer> {
    // Best available XI for the national side: a keeper, four defenders, three
    // midfielders and three forwards, each in the role that best fits them.
    let shape: [(usize, usize); 4] = [(0, 1), (1, 4), (2, 3), (3, 3)];
    let mut used = vec![false; players.len()];
    let mut xi: Vec<MatchPlayer> = Vec::with_capacity(11);
    let mk = |slot: usize, pool: &[WP]| -> MatchPlayer {
        let pi = players[slot];
        let w = &pool[pi];
        let line = line_of(w.pos);
        let (role, _) = best_role_for(&w.attrs, w.pos, line);
        let m = 1.0 + (w.morale as f32 - 65.0) * 0.002;
        MatchPlayer {
            id: pi,
            pos: w.pos,
            role,
            attrs: w.attrs,
            start_fatigue: (w.fatigue as f32 / 100.0).clamp(0.0, 1.0),
            form: m.max(0.6),
        }
    };
    for (line, count) in shape {
        let mut cands: Vec<(f32, usize)> = (0..players.len())
            .filter(|&s| !used[s])
            .map(|s| {
                let w = &pool[players[s]];
                (best_role_for(&w.attrs, w.pos, line).1, s)
            })
            .collect();
        cands.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal));
        for (_, s) in cands.into_iter().take(count) {
            used[s] = true;
            xi.push(mk(s, pool));
        }
    }
    if xi.len() < 11 {
        for s in 0..players.len() {
            if xi.len() >= 11 {
                break;
            }
            if !used[s] {
                used[s] = true;
                xi.push(mk(s, pool));
            }
        }
    }
    xi
}

/// An international fixture, now played through the same possession engine as
/// club football (neutral venue, single leg). Nation quality separates strong
/// sides from weak ones through the eleven on the pitch, so cup runs are less
/// of a lottery than the old strength-swing draw.
fn play_nation(a: usize, b: usize, nations: &[Nation], pool: &mut [WP], p: &Params, rng: &mut Pcg64Mcg) -> (u32, u32) {
    let axi = nation_xi(&nations[a].players, pool);
    let bxi = nation_xi(&nations[b].players, pool);
    if axi.len() < 7 || bxi.len() < 7 {
        return (0, 0);
    }
    let mut side_a = Side { players: &axi, ambition: 1.0, quality: 1.0, threat: 1.0 };
    let mut side_b = Side { players: &bxi, ambition: 1.0, quality: 1.0, threat: 1.0 };
    let (ta, tb) = matchsim::threat_ratio(&side_a, &side_b);
    side_a.threat = ta;
    side_b.threat = tb;
    let o = matchsim::simulate(
        &side_a,
        &side_b,
        Venue::Neutral,
        (0, 0),
        rng,
    );
    apply_match(&o, Comp::Intl, pool, p);
    (o.goals_a as u32, o.goals_b as u32)
}

fn run_international(nations: &mut [Nation], pool: &mut [WP], p: &Params, rng: &mut Pcg64Mcg) -> Option<String> {
    let n = nations.len();
    if n < 8 {
        return None;
    }
    let groups_n = (n / 4).max(2);
    let mut seeded: Vec<usize> = (0..n).collect();
    seeded.sort_by(|&a, &b| nations[b].strength.partial_cmp(&nations[a].strength).unwrap());
    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); groups_n];
    for (i, &t) in seeded.iter().enumerate() {
        groups[i % groups_n].push(t);
    }
    let mut advancing: Vec<usize> = Vec::new();
    for g in &groups {
        let mut pts: HashMap<usize, i32> = g.iter().map(|&t| (t, 0)).collect();
        let mut gd: HashMap<usize, i32> = g.iter().map(|&t| (t, 0)).collect();
        for a in 0..g.len() {
            for b in (a + 1)..g.len() {
                let (hg, ag) = play_nation(g[a], g[b], nations, pool, p, rng);
                *gd.get_mut(&g[a]).unwrap() += hg as i32 - ag as i32;
                *gd.get_mut(&g[b]).unwrap() += ag as i32 - hg as i32;
                match hg.cmp(&ag) {
                    std::cmp::Ordering::Greater => *pts.get_mut(&g[a]).unwrap() += 3,
                    std::cmp::Ordering::Less => *pts.get_mut(&g[b]).unwrap() += 3,
                    std::cmp::Ordering::Equal => {
                        *pts.get_mut(&g[a]).unwrap() += 1;
                        *pts.get_mut(&g[b]).unwrap() += 1;
                    }
                }
            }
        }
        let mut ranked = g.clone();
        ranked.sort_by(|x, y| pts[y].cmp(&pts[x]).then(gd[y].cmp(&gd[x])));
        advancing.extend(ranked.into_iter().take(2));
    }
    for &t in &advancing {
        for &pi in &nations[t].players {
            if pool[pi].intl_result < 1 {
                pool[pi].intl_result = 1;
            }
        }
    }
    advancing.shuffle(rng);
    let mut round = advancing;
    let mut tag = 2u8;
    while round.len() > 1 {
        let mut next = Vec::new();
        let mut i = 0;
        while i + 1 < round.len() {
            let (a, b) = (round[i], round[i + 1]);
            let (ga, gb) = play_nation(a, b, nations, pool, p, rng);
            let win = if ga > gb {
                a
            } else if gb > ga {
                b
            } else {
                let pa = nations[a].strength / (nations[a].strength + nations[b].strength);
                if rng.gen::<f32>() < pa {
                    a
                } else {
                    b
                }
            };
            let is_final = round.len() == 2;
            for &pi in &nations[win].players {
                let r = if is_final { 5 } else { tag };
                if pool[pi].intl_result < r {
                    pool[pi].intl_result = r;
                }
            }
            if is_final {
                let loser = if win == a { b } else { a };
                for &pi in &nations[loser].players {
                    if pool[pi].intl_result < 4 {
                        pool[pi].intl_result = 4;
                    }
                }
            }
            next.push(win);
            i += 2;
        }
        round = next;
        tag += 1;
    }
    round.first().map(|&t| nations[t].name.clone())
}

// ── Evaluation harness for the trainer ───────────────────────────────────

/// Run `seasons` seasons under `params` and return per-season headline stats
/// for fitness evaluation. A preseason window runs before each season so
/// every sample includes transfer activity.
pub(crate) fn eval_seasons(
    db: &Database,
    params: &Params,
    seasons: u32,
    seed: u64,
) -> Vec<SeasonSample> {
    let mut rng = Pcg64Mcg::new(seed as u128 | 1);
    let mut w = build_world(db, params.clone(), &mut rng);
    seed_minds(&mut w, &mut rng);
    let mut out = Vec::new();
    for y in 0..seasons {
        let (_, deals, max_fee, _) = transfer_window(&mut w, &mut rng);
        let o = run_one_season(&mut w, BASE_YEAR + y as u16, &mut rng);
        let mut champ_pts = Vec::new();
        for li in 0..BIG5.len() {
            let table = rank(&w.league_teams[li], &w.teams);
            if let Some(&t) = table.first() {
                champ_pts.push(w.teams[t].pts());
            }
        }
        let ucl_rep = w
            .teams
            .iter()
            .find(|t| t.name == o.ucl_winner)
            .map(|t| t.rep)
            .unwrap_or(0);
        let belite = w.pool.iter().find(|p| p.name == o.ballon.0).map(|p| {
            p.goals >= 20 && (p.league_won || p.ucl_result >= 2 || p.intl_result >= 3)
        }).unwrap_or(false);
        out.push(SeasonSample {
            champ_pts,
            champs: o.champions.clone(),
            boot: o.boot.1,
            ucl_rep,
            ballon_g: o.ballon.2,
            ballon_elite: belite,
            deals,
            max_fee,
        });
        if y + 1 < seasons {
            age_and_regen(&mut w, &mut rng);
        }
    }
    out
}

// ── Public: single season (detailed) ───────────────────────────────────

pub fn simulate_full_season(db: &Database, seed: u64) -> SeasonReport {    let mut rng = Pcg64Mcg::new(seed as u128 | 1);
    let params = Params::load().unwrap_or_default();
    let mut w = build_world(db, params, &mut rng);
    seed_minds(&mut w, &mut rng);
    let out = run_one_season(&mut w, BASE_YEAR, &mut rng);

    let mut lines = Vec::new();
    lines.push(format!("# Season {} Report", out.year));
    lines.push(String::new());

    for (li, lname) in BIG5.iter().enumerate() {
        let table = rank(&w.league_teams[li], &w.teams);
        if table.is_empty() {
            continue;
        }
        lines.push(format!("## {}", lname));
        lines.push(String::new());
        lines.push("| # | Club | P | W | D | L | GF | GA | GD | Pts |".to_string());
        lines.push("|---|------|---|---|---|---|----|----|----|-----|".to_string());
        for (i, &t) in table.iter().enumerate().take(6) {
            let tm = &w.teams[t];
            lines.push(format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                i + 1,
                trunc(&tm.name, 22),
                tm.p,
                tm.w,
                tm.d,
                tm.l,
                tm.gf,
                tm.ga,
                tm.gd(),
                tm.pts()
            ));
        }
        let mut scorers: Vec<usize> = (0..w.pool.len())
            .filter(|&i| w.teams[w.pool[i].team].league_idx == li)
            .collect();
        scorers.sort_by(|&a, &b| w.pool[b].goals.cmp(&w.pool[a].goals));
        lines.push(String::new());
        lines.push("**Top scorers** (goals/assists, avg rating, key attrs):".to_string());
        for &s in scorers.iter().take(3) {
            let p = &w.pool[s];
            lines.push(format!(
                "- {} — {}g {}a, avg {:.2} (PAC {} SHO {} PAS {} DRI {})",
                trunc(&p.name, 20),
                p.goals,
                p.assists,
                p.avg_rating(),
                p.attrs.pace,
                p.attrs.shooting,
                p.attrs.passing,
                p.attrs.dribbling
            ));
        }
        lines.push(String::new());
    }

    lines.push("## European Golden Boot (Big 5)".to_string());
    lines.push(String::new());
    let mut boot: Vec<usize> = (0..w.pool.len()).collect();
    boot.sort_by(|&a, &b| w.pool[b].goals.cmp(&w.pool[a].goals));
    for &s in boot.iter().take(5) {
        let p = &w.pool[s];
        lines.push(format!(
            "- {} — {}g ({} — {})",
            trunc(&p.name, 20),
            p.goals,
            w.teams[p.team].name,
            p.nation
        ));
    }
    lines.push(String::new());

    // ── Top 10 in every department (this season, all competitions), with the
    // position each player operates in — the Fbref-style leaderboards. ──
    lines.push("## Top 10 by department (this season)".to_string());
    lines.push(String::new());
    lines.push("_Positions: GK, CB, BPD (ball-playing CB), FB, WB (wing-back), DM, DLP (deep playmaker), CM, AP (advanced playmaker), W (winger), IF (inside forward), SS (second striker), TM (target man), ST, F9 (false nine)._".to_string());
    lines.push(String::new());
    let elig: Vec<usize> = (0..w.pool.len()).filter(|&i| w.pool[i].rating_apps > 0).collect();
    let outfield: Vec<usize> = elig.iter().copied().filter(|&i| w.pool[i].pos != Position::GK).collect();
    let elig_pass: Vec<usize> = outfield.iter().copied().filter(|&i| w.pool[i].passes_att >= 600).collect();
    let elig_rate: Vec<usize> = elig.iter().copied().filter(|&i| w.pool[i].rating_apps >= 15).collect();
    let gks: Vec<usize> = elig.iter().copied().filter(|&i| w.pool[i].pos == Position::GK).collect();
    top10(&mut lines, "Goals", "G (A)", &w.pool, &w.teams, &elig,
        |p| p.overall_goals() as f32, |p| format!("{} ({})", p.overall_goals(), p.overall_assists()));
    top10(&mut lines, "Assists", "A (G)", &w.pool, &w.teams, &elig,
        |p| p.overall_assists() as f32, |p| format!("{} ({})", p.overall_assists(), p.overall_goals()));
    top10(&mut lines, "Big chances created", "BCC", &w.pool, &w.teams, &elig,
        |p| p.big_chances as f32, |p| format!("{}", p.big_chances));
    top10(&mut lines, "Key passes", "KP", &w.pool, &w.teams, &elig,
        |p| p.key_passes as f32, |p| format!("{}", p.key_passes));
    top10(&mut lines, "Dribbles completed", "Drb", &w.pool, &w.teams, &elig,
        |p| p.dribbles_cmp as f32, |p| format!("{}", p.dribbles_cmp));
    top10(&mut lines, "Tackles", "Tkl", &w.pool, &w.teams, &elig,
        |p| p.tackles as f32, |p| format!("{}", p.tackles));
    top10(&mut lines, "Interceptions", "Int", &w.pool, &w.teams, &elig,
        |p| p.interceptions as f32, |p| format!("{}", p.interceptions));
    top10(&mut lines, "Pass completion (outfield, ≥600 passes)", "Cmp%", &w.pool, &w.teams, &elig_pass,
        |p| p.passes_cmp as f32 / p.passes_att.max(1) as f32,
        |p| format!("{:.1}% ({}/{})", p.passes_cmp as f32 / p.passes_att.max(1) as f32 * 100.0, p.passes_cmp, p.passes_att));
    top10(&mut lines, "Goalkeeper saves", "Sv", &w.pool, &w.teams, &gks,
        |p| p.saves as f32, |p| format!("{} ({} clean sheets)", p.saves, p.clean_sheets));
    top10(&mut lines, "Clean sheets", "CS", &w.pool, &w.teams, &elig,
        |p| p.clean_sheets as f32, |p| format!("{}", p.clean_sheets));
    top10(&mut lines, "Average rating (≥15 apps)", "Avg", &w.pool, &w.teams, &elig_rate,
        |p| p.avg_rating(), |p| format!("{:.2}", p.avg_rating()));

    // ── Anomalies: who defied their means, and the story behind it. ──
    // Academy watch: the best teenagers on the books — the next decade's
    // Ballon shortlist starts here.
    lines.push("## Academy watch (best under-20s)".to_string());
    lines.push(String::new());
    lines.push("| # | Player | Age | Pos | Club | Ovr | Pot |".to_string());
    lines.push("|--:|--------|----:|:--:|------|----:|----:|".to_string());
    let mut kids: Vec<usize> = (0..w.pool.len()).filter(|&i| w.pool[i].age <= 19).collect();
    kids.sort_by(|&a, &b| {
        (w.pool[b].ovr as u16 + w.pool[b].pot as u16)
            .cmp(&(w.pool[a].ovr as u16 + w.pool[a].pot as u16))
    });
    for (r, &i) in kids.iter().take(8).enumerate() {
        let p = &w.pool[i];
        lines.push(format!(
            "| {} | {} | {} | {} | {} | {} | {} |",
            r + 1,
            trunc(&p.name, 22),
            p.age,
            pos_tag(p),
            trunc(&w.teams[p.team].name, 18),
            p.ovr,
            p.pot
        ));
    }
    lines.push(String::new());

    anomaly_breakdown(&mut lines, &w);

    lines.push("## Continental Cups".to_string());
    lines.push(String::new());
    lines.push(format!("- Champions League: **{}** (top scorer {} {}g)", out.ucl_winner, out.ucl_top.0, out.ucl_top.1));
    lines.push(format!("- Europa League: **{}**", out.europa_winner));
    lines.push(String::new());

    // UCL scoring chart with appearances, so a 6-goal tally over 15 games reads
    // differently from 6 in 8 — the rate is the story, not just the total.
    lines.push("### Champions League scoring chart".to_string());
    lines.push(String::new());
    lines.push("| # | Player | Pos | Club | G | Apps |".to_string());
    lines.push("|--:|--------|:--:|------|:-:|:----:|".to_string());
    let mut ucl: Vec<usize> = (0..w.pool.len()).filter(|&i| w.pool[i].ucl_goals > 0).collect();
    ucl.sort_by(|&a, &b| {
        w.pool[b].ucl_goals.cmp(&w.pool[a].ucl_goals)
            .then(w.pool[a].ucl_apps.cmp(&w.pool[b].ucl_apps))
    });
    for (r, &i) in ucl.iter().take(10).enumerate() {
        lines.push(format!(
            "| {} | {} | {} | {} | {} | {} |",
            r + 1,
            trunc(&w.pool[i].name, 22),
            pos_tag(&w.pool[i]),
            trunc(&w.teams[w.pool[i].team].name, 18),
            w.pool[i].ucl_goals,
            w.pool[i].ucl_apps
        ));
    }
    lines.push(String::new());

    // Opening books: who starts with what. Transfer budgets derive from these
    // columns (revenue × willingness + carried cash), so a mid-table club with
    // no Europe cannot outbid a superclub — and the table shows why. Reg/HG
    // show the registered squad size and club-trained count: the registration
    // rules in one glance.
    lines.push("### Club finances (opening books)".to_string());
    lines.push(String::new());
    // Money columns read in 2026 money (deflated by cumulative inflation), so
    // a 2070 book compares honestly with a 2026 one instead of parading
    // nominal billions as if anyone could spend them twice over.
    let d = w.inflation.max(1.0);
    let real = |v: i64| (v as f32 / d) as i64;
    lines.push("| Club | Wealth | Revenue | Cash | Will. | Reg | HG |".to_string());
    lines.push("|------|-------:|--------:|-----:|:-----:|----:|---:|".to_string());
    let mut by_money: Vec<usize> = (0..w.teams.len()).collect();
    by_money.sort_by(|&a, &b| w.teams[b].revenue_last.cmp(&w.teams[a].revenue_last));
    for &t in by_money.iter().take(10) {
        let tm = &w.teams[t];
        let hg = tm.registered.iter().filter(|&&i| w.pool[i].homegrown).count();
        lines.push(format!(
            "| {} | {} | {}m | {}m | {:.2} | {} | {} |",
            trunc(&tm.name, 20),
            tm.wealth,
            real(tm.revenue_last),
            real(tm.cash),
            tm.willingness,
            tm.registered.len(),
            hg
        ));
    }
    lines.push(String::new());

    let intl_label = match out.intl_kind {
        IntlKind::WorldCup => "World Cup",
        IntlKind::Euros => "European Championship",
        IntlKind::None => "No major tournament",
    };
    lines.push(format!("## International: {}", intl_label));
    lines.push(String::new());
    lines.push(format!("- Winner: **{}**", out.intl_winner));
    lines.push(String::new());

    lines.push("## Ballon d'Or".to_string());
    lines.push(String::new());
    let mut bd: Vec<(usize, f32)> = (0..w.pool.len())
        .map(|i| (i, ballon_score(&w.pool[i], w.teams[w.pool[i].team].rep, BASE_YEAR)))
        .collect();
    bd.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    for (r, (i, sc)) in bd.iter().take(5).enumerate() {
        let p = &w.pool[*i];
        if r == 0 {
            lines.push(format!(
                "### Winner: {} ({} pts)",
                trunc(&p.name, 24),
                sc.round() as i32
            ));
            lines.push(String::new());
            lines.push("Why he won it:".to_string());
            let (_, parts) = ballon_breakdown(p, w.teams[p.team].rep, BASE_YEAR);
            for (label, detail, pts) in parts {
                if pts > 0.0 {
                    lines.push(format!("- {} ({}): +{} pts", label, detail, pts.round() as i32));
                }
            }
            lines.push(String::new());
            lines.push("Also in the running:".to_string());
        } else {
            lines.push(format!(
                "- {}. {} — {}g {}a, UCL {}g {}a, Intl {}g {}a ({} pts)",
                r + 1,
                trunc(&p.name, 20),
                p.goals,
                p.assists,
                p.ucl_goals,
                p.ucl_assists,
                p.intl_goals,
                p.intl_assists,
                sc.round() as i32
            ));
        }
    }
    if bd.len() >= 2 {
        lines.push(format!(
            "Winning margin: +{} over {}",
            (bd[0].1 - bd[1].1).round() as i32,
            trunc(&w.pool[bd[1].0].name, 20)
        ));
    }
    SeasonReport { lines }
}

// ── Public: multi-year (dynasty) ───────────────────────────────────────

pub fn simulate_decade(db: &Database, years: u16, seed: u64) -> SeasonReport {
    let mut rng = Pcg64Mcg::new(seed as u128 | 1);
    let params = Params::load().unwrap_or_default();
    let mut w = build_world(db, params, &mut rng);
    seed_minds(&mut w, &mut rng);
    let mut history: Vec<SeasonOutcome> = Vec::new();
    let mut pending_transfers: Vec<String> = Vec::new();

    for y in 0..years {
        let mut out = run_one_season(&mut w, BASE_YEAR + y, &mut rng);
        // Attach the summer window that preceded this season.
        out.transfers = std::mem::take(&mut pending_transfers);
        history.push(out);
        if y + 1 < years {
            age_and_regen(&mut w, &mut rng);
            let (lines, _, _, _) = transfer_window(&mut w, &mut rng);
            pending_transfers = lines;
        }
    }

    // Bank the final season into the career totals so the decade boards are
    // complete — every counter banks here exactly as at each reset, so career
    // tables always reconcile with season lines.
    for p in w.pool.iter_mut() {
        p.career_goals += p.overall_goals() as u32;
        p.career_assists += p.overall_assists() as u32;
        p.career_kp += p.key_passes as u32;
        p.career_bcc += p.big_chances as u32;
        p.career_ti += (p.tackles + p.interceptions) as u32;
        p.career_cs += p.clean_sheets as u32;
        p.career_apps += p.apps as u32;
        p.career_xg += p.xg;
        p.career_xa += p.xa;
        p.career_aerials += (p.aerials_won + p.aerials_lost) as u32;
        p.career_rating_sum += p.rating_sum;
        p.career_rating_apps += p.rating_apps as u32;
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "# {}-Year Simulation ({}–{})",
        years,
        BASE_YEAR,
        BASE_YEAR + years - 1
    ));
    lines.push(String::new());

    lines.push("## Champions by year".to_string());
    lines.push(String::new());
    lines.push("| Year | Premier League | La Liga | Serie A | Bundesliga | Ligue 1 | UCL | Europa | International |".to_string());
    lines.push("|------|----------------|---------|--------|------------|---------|-----|--------|---------------|".to_string());
    for o in &history {
        let ch = |name: &str| -> String {
            o.champions
                .iter()
                .find(|(l, _)| l == name)
                .map(|(_, c)| trunc(c, 17))
                .unwrap_or_else(|| "-".to_string())
        };
        let intl = match o.intl_kind {
            IntlKind::WorldCup => format!("WC: {}", trunc(&o.intl_winner, 13)),
            IntlKind::Euros => format!("Euro: {}", trunc(&o.intl_winner, 11)),
            IntlKind::None => "-".to_string(),
        };
        lines.push(format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            o.year,
            ch("Premier League"),
            ch("La Liga"),
            ch("Serie A"),
            ch("Bundesliga"),
            ch("Ligue 1"),
            trunc(&o.ucl_winner, 17),
            trunc(&o.europa_winner, 17),
            intl,
        ));
    }
    lines.push(String::new());

    lines.push("## Ballon d'Or by year".to_string());
    lines.push(String::new());
    lines.push("Goals/assists shown as League | UCL | Overall (all competitions).".to_string());
    lines.push(String::new());
    for o in &history {
        lines.push(format!(
            "### {} — {} ({})",
            o.year,
            trunc(&o.ballon.0, 24),
            trunc(&o.ballon.1, 20)
        ));
        lines.push(String::new());
        let boot_str = boot_line(&o.boot.0, o.boot.1, &o.boot_ties);
        let overall_str = format!(
            "{} ({}g/{}a){}",
            trunc(&o.overall_boot.0, 16),
            o.overall_boot.1,
            o.overall_boot.2,
            if o.overall_ties.is_empty() {
                String::new()
            } else {
                format!(
                    ", tied with {}",
                    o.overall_ties.iter().map(|n| trunc(n, 16)).collect::<Vec<_>>().join(" & ")
                )
            }
        );
        lines.push(format!(
            "League **{}g {}a** · UCL **{}g {}a** · Overall **{}g {}a** · Golden Boot (league): {} · Top scorer (all comps): {}",
            o.ballon.2,
            o.ballon.3,
            o.ballon.4,
            o.ballon.5,
            o.ballon.6,
            o.ballon.7,
            boot_str,
            overall_str,
        ));
        lines.push(String::new());
        lines.push("Why he won it:".to_string());
        lines.push(String::new());
        for (label, detail, pts) in &o.ballon_why {
            if *pts > 0.0 {
                lines.push(format!("- {} ({}): +{} pts", label, detail, pts.round() as i32));
            }
        }
        lines.push(String::new());
        // Full top 5 with margins — a 12-point win and a 200-point coronation
        // read very differently, and the report should show which it was.
        lines.push("Full vote:".to_string());
        for (r, (name, sc)) in o.ballon_top.iter().enumerate() {
            let margin = if r == 0 {
                String::new()
            } else {
                format!(" (-{} behind)", (o.ballon_top[0].1 - sc).round() as i32)
            };
            lines.push(format!(
                "- {}. {} ({} pts){}",
                r + 1,
                trunc(name, 20),
                sc.round() as i32,
                margin
            ));
        }
        lines.push(String::new());
    }

    // Domestic cup winners + doubles/trebles.
    lines.push("## Domestic cups".to_string());
    lines.push(String::new());
    for o in &history {
        let cups: Vec<String> = o
            .cup_winners
            .iter()
            .map(|(_, c)| trunc(c, 16))
            .collect();
        lines.push(format!("- **{}**: {}", o.year, cups.join(" · ")));
    }
    lines.push(String::new());

    // Super cups: the season curtain-raisers (Supercopa de España and
    // Supercoppa Italiana as a Final Four; Community Shield, DFL-Supercup and
    // Trophée des Champions as a single match).
    lines.push("## Super cups (champion vs cup winner; Spain & Italy a Final Four)".to_string());
    lines.push(String::new());
    for o in &history {
        if o.super_cups.is_empty() {
            continue;
        }
        let sc: Vec<String> = o
            .super_cups
            .iter()
            .map(|(l, c)| format!("{}: {}", league_short(l), trunc(c, 16)))
            .collect();
        lines.push(format!("- **{}**: {}", o.year, sc.join(" · ")));
    }
    lines.push(String::new());
    let treble_years: Vec<String> = history
        .iter()
        .filter(|o| !o.trebles.is_empty())
        .map(|o| format!("{} {}", o.year, o.trebles.join(", ")))
        .collect();
    lines.push(String::new());
    lines.push("## Trebles (league + domestic cup + Champions League)".to_string());
    lines.push(String::new());
    if treble_years.is_empty() {
        lines.push("- None — no club swept all three in one season.".to_string());
    } else {
        for t in &treble_years {
            lines.push(format!("- {}", t));
        }
    }
    lines.push(String::new());

    // Headline transfers across the era.
    lines.push("## Notable transfers (per summer window)".to_string());
    lines.push(String::new());
    let mut any_tx = false;
    for o in &history {
        if o.transfers.is_empty() {
            continue;
        }
        any_tx = true;
        lines.push(format!("- **{}**: {}", o.year, o.transfers.join(" · ")));
    }
    if !any_tx {
        lines.push("- (no big-fee moves recorded)".to_string());
    }
    lines.push(String::new());

    // Where the money is: wealth, last revenue, cash and board willingness per
    // club — the answer to "how can THEY afford him?". Budgets derive from
    // these, never from reputation alone. Reg/HG/Loans show the registered
    // squad, club-trained count and active loans: registration rules in public.
    // All money columns read in 2026 money (see opening books).
    lines.push("## Club finances (why each club can spend what it spends)".to_string());
    lines.push(String::new());
    lines.push("| Club | Wealth | Revenue | Cash | Will. | Budget | Reg | HG | Loans |".to_string());
    lines.push("|------|-------:|--------:|-----:|:-----:|-------:|----:|---:|:-----:|".to_string());
    let dd = w.inflation.max(1.0);
    let dreal = |v: i64| (v as f32 / dd) as i64;
    let mut by_money: Vec<usize> = (0..w.teams.len()).collect();
    by_money.sort_by(|&a, &b| w.teams[b].revenue_last.cmp(&w.teams[a].revenue_last));
    for &t in by_money.iter().take(16) {
        let tm = &w.teams[t];
        let hg = tm.registered.iter().filter(|&&i| w.pool[i].homegrown).count();
        let loans_out = w.loans.iter().filter(|l| l.home == t).count();
        let loans_in = w.loans.iter().filter(|l| l.away == t).count();
        // Budget here is the window's opening allocation (capacity), not the
        // unspent remainder — the remainder would read as if big spenders are
        // poor and misers rich.
        lines.push(format!(
            "| {} | {} | {}m | {}m | {:.2} | {}m | {} | {} | {}+{} |",
            trunc(&tm.name, 20),
            tm.wealth,
            dreal(tm.revenue_last),
            dreal(tm.cash),
            tm.willingness,
            dreal(tm.budget_alloc),
            tm.registered.len(),
            hg,
            loans_out,
            loans_in
        ));
    }
    lines.push(String::new());

    // Money flows (last season): every revenue stream and the preseason transfer
    // balance that built these squads, per club — the economy's wiring in
    // public, so a fee that dwarfs a budget or a fortune with no source would
    // show up here, not hide. Also 2026 money.
    lines.push("## Money flows, last season (base | league merit | Europe | preseason net spend)".to_string());
    lines.push(String::new());
    lines.push("| Club | Base | Merit | Europe | Net spend |".to_string());
    lines.push("|------|-----:|------:|-------:|----------:|".to_string());
    for &t in by_money.iter().take(12) {
        let tm = &w.teams[t];
        lines.push(format!(
            "| {} | {}m | {}m | {}m | {:+}m |",
            trunc(&tm.name, 20),
            dreal(tm.rev_base),
            dreal(tm.prize_league),
            dreal(tm.prize_euro),
            dreal(tm.net_spend)
        ));
    }
    lines.push(String::new());

    lines.push("## Most league titles".to_string());
    lines.push(String::new());
    let mut by_titles: Vec<usize> = (0..w.teams.len()).filter(|&t| w.teams[t].league_titles > 0).collect();
    by_titles.sort_by(|&a, &b| w.teams[b].league_titles.cmp(&w.teams[a].league_titles));
    // Cutoff-inclusive: every club tied at the boundary is shown, so no
    // 1-title champion is silently omitted while another 1-title club appears.
    let mut shown = 0usize;
    for &t in &by_titles {
        if shown >= 8 && w.teams[t].league_titles < w.teams[by_titles[shown - 1]].league_titles {
            break;
        }
        lines.push(format!(
            "- {} — {} titles ({} UCL, {} EL)",
            trunc(&w.teams[t].name, 24),
            w.teams[t].league_titles,
            w.teams[t].ucl_titles,
            w.teams[t].el_titles
        ));
        shown += 1;
    }
    lines.push("## Most Champions Leagues".to_string());
    lines.push(String::new());
    let mut by_ucl: Vec<usize> = (0..w.teams.len()).filter(|&t| w.teams[t].ucl_titles > 0).collect();
    by_ucl.sort_by(|&a, &b| w.teams[b].ucl_titles.cmp(&w.teams[a].ucl_titles));
    let mut shown_ucl = 0usize;
    for &t in &by_ucl {
        if shown_ucl >= 6 && w.teams[t].ucl_titles < w.teams[by_ucl[shown_ucl - 1]].ucl_titles {
            break;
        }
        lines.push(format!("- {} — {}", trunc(&w.teams[t].name, 24), w.teams[t].ucl_titles));
        shown_ucl += 1;
    }
    lines.push(String::new());

    lines.push("## Multiple Ballon d'Or winners".to_string());
    lines.push(String::new());
    let mut multi: Vec<usize> = (0..w.pool.len()).filter(|&i| w.pool[i].ballons > 1).collect();
    multi.sort_by(|&a, &b| w.pool[b].ballons.cmp(&w.pool[a].ballons));
    for &i in multi.iter().take(8) {
        lines.push(format!(
            "- {} — {} ({})",
            trunc(&w.pool[i].name, 24),
            w.pool[i].ballons,
            w.pool[i].nation
        ));
    }
    lines.push(String::new());

    lines.push("## Managerial records (career trophies travel with the manager)".to_string());
    lines.push(String::new());
    let mut mgrs: Vec<usize> = (0..w.teams.len()).collect();
    mgrs.sort_by(|&a, &b| {
        w.teams[b]
            .manager
            .trophies
            .cmp(&w.teams[a].manager.trophies)
            .then(
                w.teams[b]
                    .manager
                    .seasons
                    .cmp(&w.teams[a].manager.seasons),
            )
    });
    for &t in mgrs.iter().take(8) {
        let tm = &w.teams[t];
        if tm.manager.trophies == 0 && tm.manager.seasons < 3 {
            continue;
        }
        lines.push(format!(
            "- {} ({}): {} trophies in {} seasons",
            tm.manager.name, tm.name, tm.manager.trophies, tm.manager.seasons
        ));
    }
    lines.push(String::new());

    lines.push("## Managerial merry-go-round (latest season moves)".to_string());
    lines.push(String::new());
    let mut shown_moves = 0;
    for o in history.iter().rev() {
        for m in &o.manager_moves {
            lines.push(format!("- **{}**: {}", o.year, m));
            shown_moves += 1;
            if shown_moves >= 12 {
                break;
            }
        }
        if shown_moves >= 12 {
            break;
        }
    }
    if shown_moves == 0 {
        lines.push("- A quiet year — no sackings or appointments.".to_string());
    }
    lines.push(String::new());

    // ── The players who left the biggest mark over the whole era. ──
    lines.push("## Decade legends (career over the simulated era)".to_string());
    lines.push(String::new());
    let career: Vec<usize> = (0..w.pool.len()).filter(|&i| w.pool[i].career_apps > 0).collect();
    let career_rate = |p: &WP| -> f32 {
        if p.career_rating_apps == 0 { 0.0 } else { p.career_rating_sum / p.career_rating_apps as f32 }
    };
    // A composite "mark" so an all-round great, not just a poacher, tops it.
    // Stated exactly: career goals + 0.8 per assist + 40 per Ballon d'Or.
    top10(&mut lines, "Biggest mark (career G + 0.8×A + 40×BdOr)", "G / A / BdOr", &w.pool, &w.teams, &career,
        |p| p.career_goals as f32 + p.career_assists as f32 * 0.8 + p.ballons as f32 * 40.0,
        |p| format!("{} / {} / {}", p.career_goals, p.career_assists, p.ballons));
    top10(&mut lines, "Career goals", "G", &w.pool, &w.teams, &career,
        |p| p.career_goals as f32, |p| format!("{}", p.career_goals));
    top10(&mut lines, "Career assists", "A", &w.pool, &w.teams, &career,
        |p| p.career_assists as f32, |p| format!("{}", p.career_assists));
    top10(&mut lines, "Career big chances created", "BCC", &w.pool, &w.teams, &career,
        |p| p.career_bcc as f32, |p| format!("{}", p.career_bcc));
    top10(&mut lines, "Career key passes", "KP", &w.pool, &w.teams, &career,
        |p| p.career_kp as f32, |p| format!("{}", p.career_kp));
    // Siege effect, stated plainly: sides that defend for their lives make
    // more tackles — so career ball-winning is led by relegation-battlers,
    // exactly as real tackle charts are. Totals show apps and per-game rate
    // so longevity never reads as godliness.
    top10(&mut lines, "Career tackles + interceptions", "T+I (apps, per game)", &w.pool, &w.teams, &career,
        |p| p.career_ti as f32, |p| format!("{} in {} ({:.1}/g)", p.career_ti, p.career_apps,
            if p.career_apps > 0 { p.career_ti as f32 / p.career_apps as f32 } else { 0.0 }));
    // Best defenders — QUALITY boards, not raw volume, and split by role so the
    // list isn't wall-to-wall centre-backs. The pure T+I list above is led by
    // relegation-battlers who defend for their lives (the real siege effect);
    // these judge defenders like real ones. Centre-backs are rated on clean
    // sheets kept at strong defences, rating and duels won; full-backs and
    // wing-backs on the same solidity PLUS the attacking end-product (assists,
    // key passes) that actually distinguishes a great modern wide defender.
    let is_fb = |p: &WP| matches!(pos_tag(p), "FB" | "WB");
    let cb_career: Vec<usize> = career
        .iter()
        .copied()
        .filter(|&i| w.pool[i].pos == Position::DEF && w.pool[i].career_apps >= 80 && !is_fb(&w.pool[i]))
        .collect();
    let fb_career: Vec<usize> = career
        .iter()
        .copied()
        .filter(|&i| w.pool[i].pos == Position::DEF && w.pool[i].career_apps >= 80 && is_fb(&w.pool[i]))
        .collect();
    let cb_score = |p: &WP| -> f32 {
        let apps = p.career_apps.max(1) as f32;
        p.career_cs as f32 * 2.0 + (career_rate(p) - 6.5).max(0.0) * 70.0 + (p.career_ti as f32 / apps) * 8.0
    };
    let fb_score = |p: &WP| -> f32 {
        let apps = p.career_apps.max(1) as f32;
        let going_forward = (p.career_assists as f32 + p.career_kp as f32 * 0.5) / apps;
        p.career_cs as f32 * 1.6
            + (career_rate(p) - 6.5).max(0.0) * 60.0
            + (p.career_ti as f32 / apps) * 5.0
            + going_forward * 60.0
    };
    top10(&mut lines, "Best centre-backs (career: clean sheets + rating + duels)", "CS / avg / T+I·g / role", &w.pool, &w.teams, &cb_career,
        cb_score,
        |p| format!("{} CS, {:.2}, {:.1}/g, {}", p.career_cs, career_rate(p),
            p.career_ti as f32 / p.career_apps.max(1) as f32, pos_tag(p)));
    top10(&mut lines, "Best full-backs & wing-backs (career: solidity + end product)", "CS / avg / A+KP / role", &w.pool, &w.teams, &fb_career,
        fb_score,
        |p| format!("{} CS, {:.2}, {}A {}KP, {}", p.career_cs, career_rate(p),
            p.career_assists, p.career_kp, pos_tag(p)));
    let career_rate200: Vec<usize> = career.iter().copied().filter(|&i| w.pool[i].career_rating_apps >= 100).collect();
    top10(&mut lines, "Career average rating (≥100 apps)", "Avg", &w.pool, &w.teams, &career_rate200,
        |p| career_rate(p), |p| format!("{:.2}", career_rate(p)));

    // ── Why the era looked the way it did. ──
    decade_trends(&mut lines, &history, &w);

    lines.push("## Detected realism glitches".to_string());
    lines.push(String::new());
    let glitches = detect_glitches(&w, &history);
    if glitches.is_empty() {
        lines.push("- (none flagged this run)".to_string());
    } else {
        for g in glitches {
            lines.push(format!("- {}", g));
        }
    }
    SeasonReport { lines }
}

fn detect_glitches(w: &World, history: &[SeasonOutcome]) -> Vec<String> {
    let mut out = Vec::new();

    let mut max_pts = 0u16;
    for li in 0..BIG5.len() {
        let table = rank(&w.league_teams[li], &w.teams);
        if let Some(&t) = table.first() {
            max_pts = max_pts.max(w.teams[t].pts());
        }
    }
    if max_pts > 102 {
        out.push(format!("Final-season champion reached {} pts (Juventus' 102 is the real record).", max_pts));
    }

    let max_boot = history.iter().map(|o| o.boot.1).max().unwrap_or(0);
    if max_boot > 52 {
        out.push(format!(
            "A single-league golden boot hit {} goals (the real single-league record is ~50; all-comps tallies of 60-73 are fine).",
            max_boot
        ));
    }

    for o in history {
        // Below ~72 is Porto-2004 territory at best; anything lower is a fluke.
        // Judged by reputation AT WIN TIME, never the drifted present value.
        if o.ucl_winner != "-" && o.ucl_winner_rep < 72 {
            out.push(format!(
                "{} won the {} Champions League at reputation {} (elite bias a touch weak).",
                o.ucl_winner, o.year, o.ucl_winner_rep
            ));
        }
    }

    for o in history {
        if o.ballon.6 == 0 {
            out.push(format!(
                "{} Ballon d'Or ({}) scored no goals in any competition.",
                o.year, o.ballon.0
            ));
        }
    }

    let oldest = w.pool.iter().filter(|p| !p.retired).map(|p| p.age).max().unwrap_or(0);
    if oldest > 42 {
        out.push(format!("A player is still active at age {} (retirement cap broken).", oldest));
    }

    // Immortal cup-specialist: a big club's manager surviving a long league
    // drought under their own tenure means the drought axe failed. Cups must
    // never buy a decade of league failure at a stature-85+ club.
    for t in &w.teams {
        let stature = t.rep.max(t.wealth);
        if stature >= 85 && t.manager.title_drought >= 15 && t.manager.seasons >= 15 {
            out.push(format!(
                "{} kept {} for {} seasons on a {}-season league drought (drought axe failed).",
                t.name, t.manager.name, t.manager.seasons, t.manager.title_drought
            ));
        }
    }

    // Methuselah manager: nobody runs a club into their mid-70s. The age cap
    // and retirement curve should make this impossible.
    for t in &w.teams {
        if t.manager.age > 72 || t.manager.seasons > 30 {
            out.push(format!(
                "{} has managed {} for {} seasons (aged {}) — tenure cap broken.",
                t.manager.name, t.name, t.manager.seasons, t.manager.age
            ));
        }
    }

    // Doomsday finance: negative cash must never coexist with a real budget.
    // Insolvent clubs run sell-to-buy only (allocation zeroed at the window).
    for t in &w.teams {
        if t.cash < 0 && t.budget_alloc > 0 {
            out.push(format!(
                "{} holds {}m cash with a {}m budget (debt financing galácticos).",
                t.name, t.cash, t.budget_alloc
            ));
        }
    }

    for lname in BIG5.iter() {
        let winners: Vec<&String> = history
            .iter()
            .filter_map(|o| o.champions.iter().find(|(l, _)| l == lname).map(|(_, c)| c))
            .collect();
        if winners.len() == history.len() && winners.len() >= 4 {
            let first = winners[0];
            if winners.iter().all(|c| *c == first) {
                // A superclub monopoly (rep 88+, cf. Bayern's 11 straight or
                // PSG's decade) is realistic; a lesser club sweeping every
                // year means the league never competes.
                let rep = w
                    .teams
                    .iter()
                    .find(|t| &t.name == first)
                    .map(|t| t.rep)
                    .unwrap_or(0);
                if rep < 88 {
                    out.push(format!(
                        "{} won by {} (rep {}) in all {} years (no title turnover).",
                        lname,
                        first,
                        rep,
                        history.len()
                    ));
                }
            }
        }
    }

    out
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    }
}

/// Short country tag for a Big-5 league label, for compact super-cup lines.
fn league_short(league: &str) -> &'static str {
    match league {
        "Premier League" => "ENG",
        "La Liga" => "ESP",
        "Serie A" => "ITA",
        "Bundesliga" => "GER",
        "Ligue 1" => "FRA",
        _ => "EUR",
    }
}

/// Render a scoring title with its ties acknowledged: "Haaland (38g)", or
/// "Haaland & Mastantuono (38g each, tied)" — never a false sole winner.
fn boot_line(name: &str, goals: u16, ties: &[String]) -> String {
    if ties.is_empty() {
        format!("{} ({}g)", trunc(name, 16), goals)
    } else {
        format!(
            "{} & {} ({}g each, tied)",
            trunc(name, 16),
            ties.iter().map(|n| trunc(n, 16)).collect::<Vec<_>>().join(" & "),
            goals
        )
    }
}

/// A player's short positional tag (CB, DM, AP, ST…) from the role that best
/// fits their attributes — so stat tables read like a real scouting sheet.
fn pos_tag(w: &WP) -> &'static str {
    best_role_for(&w.attrs, w.pos, line_of(w.pos)).0.pos_label()
}

/// A club's leading scorer this season (index into pool), for storylines.
fn club_top_scorer(w: &World, club: usize) -> Option<usize> {
    (0..w.pool.len())
        .filter(|&i| w.pool[i].team == club)
        .max_by_key(|&i| w.pool[i].goals)
        .filter(|&i| w.pool[i].goals > 0)
}

/// Narrate the season's genuine surprises: a club finishing far above the
/// station its reputation (and money) would predict, or a giant falling short —
/// with the players and numbers that drove it, so an anomaly is explained, not
/// just asserted.
fn anomaly_breakdown(lines: &mut Vec<String>, w: &World) {
    lines.push("## Anomalies & storylines".to_string());
    lines.push(String::new());
    let mut found = false;
    for (li, lname) in BIG5.iter().enumerate() {
        let table = rank(&w.league_teams[li], &w.teams);
        if table.len() < 8 {
            continue;
        }
        let mut by_rep = table.clone();
        by_rep.sort_by(|&a, &b| w.teams[b].rep.cmp(&w.teams[a].rep));
        let rep_rank: HashMap<usize, usize> =
            by_rep.iter().enumerate().map(|(r, &t)| (t, r)).collect();

        // Overachiever: a top-3 finish from a side reputation ranked ≥4 places lower.
        if let Some((&t, fin)) = table
            .iter()
            .enumerate()
            .map(|(f, t)| (t, f))
            .filter(|&(t, f)| f <= 2 && rep_rank[t] as i32 - f as i32 >= 4)
            .max_by_key(|&(t, f)| rep_rank[t] as i32 - f as i32)
        {
            let tm = &w.teams[t];
            let why = match club_top_scorer(w, t) {
                Some(s) => format!(
                    "driven by {} ({}, {}g {}a, avg {:.2}) and a {:+} goal difference",
                    trunc(&w.pool[s].name, 20),
                    pos_tag(&w.pool[s]),
                    w.pool[s].goals,
                    w.pool[s].overall_assists(),
                    w.pool[s].avg_rating(),
                    tm.gd()
                ),
                None => format!("on a {:+} goal difference", tm.gd()),
            };
            lines.push(format!(
                "- **{} — {}**: finished **{}** on {} pts, where its reputation slotted it ~{}. Overachieved {}.",
                lname,
                trunc(&tm.name, 22),
                fin + 1,
                tm.pts(),
                rep_rank[&t] + 1,
                why
            ));
            found = true;
        }

        // Disappointment: a reputation top-3 side finishing 8th or worse.
        if let Some((&t, fin)) = table
            .iter()
            .enumerate()
            .map(|(f, t)| (t, f))
            .filter(|&(t, f)| rep_rank[t] <= 2 && f >= 7)
            .max_by_key(|&(_t, f)| f)
        {
            let tm = &w.teams[t];
            lines.push(format!(
                "- **{} — {}**: only **{}** despite a top-{} reputation and budget — {:+} GD, a season to forget.",
                lname,
                trunc(&tm.name, 22),
                fin + 1,
                rep_rank[&t] + 1,
                tm.gd()
            ));
            found = true;
        }
    }
    if !found {
        lines.push("- No great shocks: every league broadly followed the money this year.".to_string());
    }
    lines.push(String::new());
}

/// Explain the shape of the era: who dominated and why, how open each league
/// was, and which nation owned the Ballon d'Or — inferred from the actual
/// history, not asserted.
fn decade_trends(lines: &mut Vec<String>, history: &[SeasonOutcome], w: &World) {
    lines.push("## Why the era unfolded this way".to_string());
    lines.push(String::new());

    // Most-decorated league club across the Big Five.
    let mut titles: HashMap<String, u32> = HashMap::new();
    for o in history {
        for (_l, c) in &o.champions {
            *titles.entry(c.clone()).or_insert(0) += 1;
        }
    }
    if let Some((club, n)) = titles.iter().max_by_key(|(_, n)| **n) {
        let rep = w.teams.iter().find(|t| &t.name == club).map(|t| t.rep).unwrap_or(0);
        lines.push(format!(
            "- **{}** dominated with {} league titles — the era's deepest resources (reputation ~{}) let it keep reloading its squad through the market and regenerate through decline.",
            trunc(club, 24), n, rep
        ));
    }

    // How open the big leagues were (distinct champions).
    for lname in ["Premier League", "La Liga", "Serie A"] {
        let mut champs: Vec<&String> = history
            .iter()
            .filter_map(|o| o.champions.iter().find(|(l, _)| l == lname).map(|(_, c)| c))
            .collect();
        champs.sort();
        champs.dedup();
        let distinct = champs.len();
        let verdict = if distinct <= 2 {
            "a closed shop"
        } else if distinct <= 4 {
            "top-heavy but contested"
        } else {
            "wide open"
        };
        lines.push(format!(
            "- **{}** was {} — {} different champions across the era.",
            lname, verdict, distinct
        ));
    }

    // UCL: is Europe concentrated or spread?
    let mut ucl: HashMap<&String, u32> = HashMap::new();
    for o in history {
        *ucl.entry(&o.ucl_winner).or_insert(0) += 1;
    }
    if let Some((club, n)) = ucl.iter().max_by_key(|(_, n)| **n) {
        if *n >= 2 {
            lines.push(format!(
                "- **{}** ruled Europe with {} Champions Leagues; continental success then fed its revenue and reputation, compounding the gap.",
                trunc(club, 24), n
            ));
        } else {
            lines.push("- Europe was unpredictable — no club retained the Champions League, upsets spread the trophy around.".to_string());
        }
    }

    // Ballon d'Or nation dominance.
    let mut nations: HashMap<String, u32> = HashMap::new();
    for o in history {
        // ballon.1 is the winner's club; nation is looked up from the pool.
        if let Some(p) = w.pool.iter().find(|p| p.name == o.ballon.0) {
            if !p.nation.is_empty() {
                *nations.entry(p.nation.clone()).or_insert(0) += 1;
            }
        }
    }
    if let Some((nat, n)) = nations.iter().max_by_key(|(_, n)| **n) {
        if *n >= 2 {
            lines.push(format!("- **{}** produced the most Ballon d'Or winners ({}) — a golden generation.", nat, n));
        }
    }
    lines.push(String::new());
}

/// Append a "top 10" table for one statistical department, positions included.
/// `val` ranks; `fmt` renders the headline number(s) for each player.
fn top10(
    lines: &mut Vec<String>,
    title: &str,
    header: &str,
    pool: &[WP],
    teams: &[Team],
    eligible: &[usize],
    val: impl Fn(&WP) -> f32,
    fmt: impl Fn(&WP) -> String,
) {
    let mut idx: Vec<usize> = eligible.to_vec();
    idx.sort_by(|&a, &b| val(&pool[b]).partial_cmp(&val(&pool[a])).unwrap_or(std::cmp::Ordering::Equal));
    lines.push(format!("### {}", title));
    lines.push(String::new());
    lines.push(format!("| # | Player | Pos | Club | {} |", header));
    lines.push("|--:|--------|:--:|------|--:|".to_string());
    for (r, &i) in idx.iter().take(10).enumerate() {
        if val(&pool[i]) <= 0.0 {
            break;
        }
        lines.push(format!(
            "| {} | {} | {} | {} | {} |",
            r + 1,
            trunc(&pool[i].name, 22),
            pos_tag(&pool[i]),
            trunc(&teams[pool[i].team].name, 18),
            fmt(&pool[i])
        ));
    }
    lines.push(String::new());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn attrs_all(v: u8) -> Attributes {
        Attributes {
            pace: v,
            shooting: v,
            passing: v,
            dribbling: v,
            tackling: v,
            heading: v,
            strength: v,
            stamina: v,
            vision: v,
            composure: v,
            goalkeeping: v,
            first_touch: v,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn mkp(
        name: &str,
        pos: Position,
        v: u8,
        age: u8,
        pot: u8,
        team: usize,
        homegrown: bool,
    ) -> WP {
        WP {
            name: name.to_string(),
            nation: "Testland".to_string(),
            pos,
            attrs: attrs_all(v),
            ovr: v,
            pot,
            age,
            team,
            retired: false,
            morale: 70,
            fatigue: 0,
            adapt: 0,
            on_loan: false,
            homegrown,
            apps: 0,
            goals: 0,
            assists: 0,
            rating_sum: 0.0,
            rating_apps: 0,
            ucl_goals: 0,
            el_goals: 0,
            intl_goals: 0,
            ucl_assists: 0,
            el_assists: 0,
            intl_assists: 0,
            ucl_apps: 0,
            el_apps: 0,
            intl_apps: 0,
            shots: 0,
            sot: 0,
            xg: 0.0,
            xa: 0.0,
            aerials_won: 0,
            aerials_lost: 0,
            key_passes: 0,
            big_chances: 0,
            passes_att: 0,
            passes_cmp: 0,
            dribbles_att: 0,
            dribbles_cmp: 0,
            tackles: 0,
            interceptions: 0,
            fouls: 0,
            saves: 0,
            clean_sheets: 0,
            league_won: false,
            ucl_result: 0,
            el_result: 0,
            intl_result: 0,
            value: 40,
            release_clause: None,
            wage: 8,
            locked: 0,
            career_goals: 0,
            career_assists: 0,
            career_kp: 0,
            career_bcc: 0,
            career_ti: 0,
            career_cs: 0,
            career_xg: 0.0,
            career_xa: 0.0,
            career_aerials: 0,
            career_apps: 0,
            career_rating_sum: 0.0,
            career_rating_apps: 0,
            ballons: 0,
        }
    }

    fn mkteam(
        rng: &mut Pcg64Mcg,
        name: &str,
        league_idx: usize,
        rep: u8,
        players: Vec<usize>,
        revenue_last: i64,
    ) -> Team {
        Team {
            name: name.to_string(),
            league_idx,
            rep,
            players,
            strength: 50.0,
            p: 0,
            w: 0,
            d: 0,
            l: 0,
            gf: 0,
            ga: 0,
            league_titles: 0,
            ucl_titles: 0,
            el_titles: 0,
            super_cups: 0,
            budget: 0,
            wealth: rep,
            heritage: rep,
            cash: 100,
            revenue_last,
            rev_base: 0,
            prize_league: 0,
            prize_euro: 0,
            net_spend: 0,
            budget_alloc: 0,
            willingness: 0.7,
            academy: 3,
            title_drought: 0,
            registered: Vec::new(),
            manager: Manager::new_random(rng, &Params::default()),
            shape: (4, 4, 2),
        }
    }

    fn mini_world(rng: &mut Pcg64Mcg) -> World {
        // Big club (0): 12 seniors ~80 + one 19yo 70/85 homegrown prospect.
        // Small club (1): starters ~65 with a weak 55 MID slot.
        let mut pool = Vec::new();
        let mut push = |p: WP| -> usize {
            let i = pool.len();
            pool.push(p);
            i
        };
        let mut big: Vec<usize> = Vec::new();
        big.push(push(mkp("Big Keeper", Position::GK, 80, 28, 80, 0, false)));
        for k in 0..4 {
            big.push(push(mkp(&format!("Big Def {}", k), Position::DEF, 80, 27, 80, 0, false)));
        }
        for k in 0..4 {
            big.push(push(mkp(&format!("Big Mid {}", k), Position::MID, 80, 26, 80, 0, false)));
        }
        for k in 0..2 {
            big.push(push(mkp(&format!("Big Fwd {}", k), Position::FWD, 80, 25, 80, 0, false)));
        }
        let prospect = push(mkp("Kid Prospect", Position::MID, 70, 19, 85, 0, true));
        big.push(prospect);
        let mut small: Vec<usize> = Vec::new();
        small.push(push(mkp("Small Keeper", Position::GK, 65, 29, 65, 1, false)));
        for k in 0..4 {
            small.push(push(mkp(&format!("Small Def {}", k), Position::DEF, 65, 28, 65, 1, false)));
        }
        for k in 0..2 {
            small.push(push(mkp(&format!("Small Mid {}", k), Position::MID, 65, 27, 65, 1, false)));
        }
        small.push(push(mkp("Small Weakmid", Position::MID, 55, 30, 55, 1, false)));
        for k in 0..3 {
            small.push(push(mkp(&format!("Small Fwd {}", k), Position::FWD, 65, 26, 65, 1, false)));
        }
        let minds = (0..pool.len()).map(|_| PlayerMind::new(rng)).collect();
        let teams = vec![
            mkteam(rng, "Big FC", 0, 90, big, 800),
            mkteam(rng, "Small Town", 1, 60, small, 120),
        ];
        World {
            pool,
            minds,
            teams,
            league_teams: vec![vec![0], vec![1]],
            nation_pool: Vec::new(),
            next_regen: 1,
            elite_cap: 90,
            inflation: 1.0,
            loans: Vec::new(),
            free_agents: Vec::new(),
            used_names: HashSet::new(),
            params: Params::default(),
        }
    }

    #[test]
    fn loans_flow_to_starters_and_return() {
        let mut rng = Pcg64Mcg::new(3);
        let mut w = mini_world(&mut rng);
        let prospect = w.pool.iter().position(|p| p.name == "Kid Prospect").unwrap();
        transfer_window(&mut w, &mut rng);
        // The prospect must be on loan at the small club (a starter there),
        // not sold, not retired, with the loan recorded and a fee paid.
        assert_eq!(w.loans.len(), 1, "expected exactly one loan, got {:?}", w.loans.len());
        let loan = w.loans[0];
        assert_eq!((loan.player, loan.home, loan.away), (prospect, 0, 1));
        assert!(w.pool[prospect].on_loan);
        assert_eq!(w.pool[prospect].team, 1);
        assert!(w.teams[1].players.contains(&prospect));
        assert!(!w.teams[0].players.contains(&prospect));
        assert!(!w.pool[prospect].retired);
        assert!(w.pool[prospect].homegrown, "loans must keep club-trained status");
        // Returns come home next window.
        return_loans(&mut w);
        assert!(w.loans.is_empty());
        assert!(!w.pool[prospect].on_loan);
        assert_eq!(w.pool[prospect].team, 0);
        assert!(w.teams[0].players.contains(&prospect));
        assert!(!w.teams[1].players.contains(&prospect));
    }

    #[test]
    fn registration_caps_and_homegrown_floor() {
        let mut rng = Pcg64Mcg::new(5);
        let mut w = mini_world(&mut rng);
        // Bloat Big FC to 27 actives with the homegrown prospect ranked last.
        for k in 0..14 {
            let i = w.pool.len();
            w.pool.push(mkp(&format!("Filler {}", k), Position::MID, 78, 24, 78, 0, false));
            w.teams[0].players.push(i);
            w.minds.push(PlayerMind::new(&mut rng));
        }
        transfer_window(&mut w, &mut rng);
        // Every club registered at most 25...
        for t in 0..w.teams.len() {
            assert!(
                w.teams[t].registered.len() <= MAX_REGISTERED,
                "{} registered {}",
                w.teams[t].name,
                w.teams[t].registered.len()
            );
            // ...all registered are active squad members...
            for &pi in &w.teams[t].registered.clone() {
                assert!(w.teams[t].players.contains(&pi));
                assert!(!w.pool[pi].retired);
            }
            // ...and the homegrown floor holds wherever supply exists.
            let hg_avail = w.teams[t]
                .players
                .iter()
                .filter(|&&i| !w.pool[i].retired && w.pool[i].homegrown)
                .count();
            let hg_reg = w.teams[t]
                .registered
                .iter()
                .filter(|&&i| w.pool[i].homegrown)
                .count();
            assert_eq!(
                hg_reg,
                MIN_HOMEGROWN.min(hg_avail),
                "{} homegrown floor broken",
                w.teams[t].name
            );
        }
        // The protected prospect is still owned (never sold), here or on loan.
        let prospect = w.pool.iter().position(|p| p.name == "Kid Prospect").unwrap();
        assert!(!w.pool[prospect].retired);
        assert!(w.pool[prospect].team == 0 || w.pool[prospect].team == 1);
    }

    #[test]
    fn ballon_rewards_all_round_not_only_goals() {
        // The all-round components give a dominant holding midfielder (treble,
        // top rating, huge ball-winning) a real path past a modest forward —
        // yet a genuinely prolific forward still wins in a normal year, so the
        // overall trend (forwards usually win) is preserved.
        let year = 2030;
        let rep = 88;
        let mut dm = mkp("Rodri", Position::MID, 86, 27, 90, 0, false);
        dm.apps = 34;
        dm.goals = 8;
        dm.assists = 9;
        dm.big_chances = 8;
        dm.tackles = 120;
        dm.interceptions = 70;
        dm.passes_cmp = 2700;
        dm.dribbles_cmp = 35;
        dm.rating_sum = 7.7 * 34.0;
        dm.rating_apps = 34;
        dm.league_won = true;
        dm.ucl_result = 5;
        dm.ucl_goals = 2;
        dm.ucl_apps = 12;

        let mut modest = mkp("Journeyman", Position::FWD, 80, 27, 80, 1, false);
        modest.apps = 34;
        modest.goals = 15;
        modest.assists = 3;
        modest.rating_sum = 7.0 * 34.0;
        modest.rating_apps = 34;

        let mut elite = mkp("Golden Boot", Position::FWD, 88, 26, 92, 2, false);
        elite.apps = 34;
        elite.goals = 34;
        elite.assists = 9;
        elite.big_chances = 14;
        elite.rating_sum = 7.6 * 34.0;
        elite.rating_apps = 34;
        elite.league_won = true;
        elite.ucl_result = 4;
        elite.ucl_goals = 7;
        elite.ucl_apps = 12;

        let s_dm = ballon_score(&dm, rep, year);
        let s_modest = ballon_score(&modest, rep, year);
        let s_elite = ballon_score(&elite, rep, year);
        assert!(s_dm > s_modest, "all-round DM {:.0} should beat a modest forward {:.0}", s_dm, s_modest);
        assert!(s_elite > s_dm, "a prolific forward {:.0} should still edge the DM {:.0}", s_elite, s_dm);
    }

    #[test]
    fn register_squad_prefers_best_with_floor() {
        let mut rng = Pcg64Mcg::new(9);
        let mut pool: Vec<WP> = Vec::new();
        // 26 actives: 25 at 70, one homegrown at 40 (ranked dead last).
        for k in 0..25 {
            pool.push(mkp(&format!("P{}", k), Position::MID, 70, 25, 70, 0, false));
        }
        pool.push(mkp("Kid", Position::MID, 40, 18, 80, 0, true));
        let idxs: Vec<usize> = (0..pool.len()).collect();
        let reg = register_squad(&idxs, &pool);
        assert_eq!(reg.len(), 25);
        let kid = pool.len() - 1;
        assert!(reg.contains(&kid), "homegrown floor must rescue the last-ranked prospect");
        // ...by displacing exactly one non-homegrown.
        assert_eq!(reg.iter().filter(|&&i| !pool[i].homegrown).count(), 24);
        let _ = rng;
    }

    #[test]
    fn super_cup_and_domestic_cup_resolve() {
        // The domestic cup now reports a distinct beaten finalist, and the
        // super cup resolves a winner from both the single-match and the
        // Final Four shapes — all drawn from the field, never inventing teams.
        let mut rng = Pcg64Mcg::new(44);
        let w = mini_world(&mut rng);
        let (win, finalist) = run_domestic_cup(&[0, 1], &w.teams, &w.params, &mut rng).unwrap();
        assert_ne!(win, finalist, "two-team cup must have a distinct runner-up");
        assert!(win < 2 && finalist < 2);
        let sc = super_cup_winner(&[0, 1], &w.teams, &w.params, &mut rng).unwrap();
        assert!(sc == 0 || sc == 1);
        let ff = super_cup_winner(&[0, 1, 0, 1], &w.teams, &w.params, &mut rng).unwrap();
        assert!(ff == 0 || ff == 1);
        assert_eq!(super_cup_winner(&[0], &w.teams, &w.params, &mut rng), Some(0));
        assert_eq!(super_cup_winner(&[], &w.teams, &w.params, &mut rng), None);
    }

    #[test]
    fn regen_defenders_are_not_all_centre_backs() {
        // Over many generated defenders, full-backs / wing-backs must appear —
        // the stat tables shouldn't be wall-to-wall CBs. (pos_tag reads the
        // best-fitting role from attributes.)
        let mut g = crate::gen::Gen::new(7);
        let mut fullbacks = 0;
        let mut centre = 0;
        for _ in 0..200 {
            let p = g.player(Position::DEF, 20);
            match best_role_for(&p.attrs, Position::DEF, 1).0.pos_label() {
                "FB" | "WB" => fullbacks += 1,
                "CB" | "BPD" => centre += 1,
                _ => {}
            }
        }
        assert!(fullbacks > 30, "almost no full-backs generated ({}/200)", fullbacks);
        assert!(centre > 30, "almost no centre-backs generated ({}/200)", centre);
    }

    #[test]
    fn growth_slows_near_human_limits() {
        // Diminishing returns: full steps below 80, +2 into the high 80s, +1
        // toward 94, and ~nothing above except rare breakthroughs.
        let mut rng = Pcg64Mcg::new(202);
        let mut v = 70u8;
        grow_attr(&mut v, 3, &mut rng);
        assert_eq!(v, 73);
        v = 85;
        grow_attr(&mut v, 3, &mut rng);
        assert_eq!(v, 87);
        v = 90;
        grow_attr(&mut v, 3, &mut rng);
        assert_eq!(v, 91);
        v = 96;
        grow_attr(&mut v, 3, &mut rng);
        assert!(v <= 97, "above 94 only rare breakthroughs may land");
        assert_eq!(v, v.clamp(1, 99));
    }

    #[test]
    fn wonderkids_peak_like_messi_not_video_game() {
        // A 75-shooting 17-year-old developed to 25 with full minutes must land
        // in the low 90s (Messi/Ronaldo zone), never strolling to 99.
        let mut rng = Pcg64Mcg::new(203);
        let mut pool = vec![mkp("Kid", Position::FWD, 75, 17, 99, 0, true)];
        pool[0].attrs.shooting = 75;
        for age in 18..=25 {
            pool[0].age = age;
            pool[0].apps = 30;
            develop_player(&mut pool[0], 1.0, 1.1, &mut rng);
        }
        let s = pool[0].attrs.shooting;
        assert!(s >= 82, "wonderkid must actually develop, got {}", s);
        assert!(s <= 96, "wonderkid must not stroll to 99, got {}", s);
        let ovr = pool[0].attrs.overall(pool[0].pos);
        assert!(ovr <= 95, "overall must stay GOAT-plausible, got {}", ovr);
    }

    #[test]
    fn fm_vintage_ages_shift_four_years() {
        // All-multiples-of-5 attributes mark an FM2023 (2022-vintage) row:
        // van Sas listed 19 must read 23. Continuous FC rows are untouched.
        let mut fm = attrs_all(80);
        fm.pace = 85;
        fm.shooting = 75;
        assert_eq!(fm_vintage_age(19, &fm), 23);
        let mut fc = attrs_all(80);
        fc.pace = 83; // not a multiple of 5 → FC row
        assert_eq!(fm_vintage_age(19, &fc), 19);
        // Clamped at the top end, never below.
        assert_eq!(fm_vintage_age(39, &fm), 40);
    }

    #[test]
    fn greedy_stars_demand_revenue_share() {
        // A greedy 90-ovr star's wage must ratchet toward a share of club
        // revenue (6-16% by greed), not sit at its signing nominal forever —
        // this is what breaks super-teams up under their own wage bills.
        let mut rng = Pcg64Mcg::new(101);
        let mut w = mini_world(&mut rng);
        let star = w.pool.iter().position(|p| p.name == "Big Mid 0").unwrap();
        w.pool[star].attrs = attrs_all(90);
        w.pool[star].ovr = 90;
        w.pool[star].pot = 95;
        w.pool[star].wage = 10;
        w.minds[star].greed = 0.95;
        // Team 0 revenue is 800: demand = 800 × (0.06 + 0.095) = 124.
        age_and_regen(&mut w, &mut rng);
        assert!(
            w.pool[star].wage >= 120,
            "greedy star wage {} did not ratchet to revenue share",
            w.pool[star].wage
        );
    }

    #[test]
    fn insolvent_clubs_get_no_allocation() {
        // Debt cannot finance galácticos: a cash-negative club opens the
        // window with zero allocation and spends only what sales bring in.
        let mut rng = Pcg64Mcg::new(103);
        let mut w = mini_world(&mut rng);
        w.teams[0].cash = -100;
        w.teams[0].revenue_last = 800;
        // Replicate the window's budget opening without running the market.
        for t in &mut w.teams {
            let pile = (t.cash.max(0) as f32 / 1000.0).clamp(0.0, 1.0);
            let effective_will = if t.cash < -300 {
                t.willingness * 0.5
            } else {
                (t.willingness + 0.3 * pile).min(1.0)
            };
            t.budget = if t.cash < 0 {
                0
            } else {
                (t.cash.max(0) as f32 * 0.15
                    + t.revenue_last as f32 * effective_will * w.params.budget_ratio) as i64
            };
        }
        assert_eq!(w.teams[0].budget, 0, "insolvent club got an allocation");
        assert!(w.teams[1].budget >= 0);
    }

    #[test]
    fn revenue_tracks_football_inflation() {
        // Wages and fees inflate every year; revenue must too — otherwise a
        // fixed 2026-money income against 7x wages guarantees every giant
        // ends at the -500 debt floor by the 2060s. Nominal revenue scales
        // with inflation so real budgets stay constant across eras.
        let mut rng = Pcg64Mcg::new(104);
        let mut w = mini_world(&mut rng);
        w.inflation = 7.0;
        let tables = vec![vec![0usize], vec![1usize]];
        settle_finances(&mut w, &tables, &mut rng);
        // Big club: base 700 * 7 = 4900 plus prizes — nominal, not 2026 money.
        assert!(
            w.teams[0].revenue_last > 3000,
            "revenue didn't inflate: {}",
            w.teams[0].revenue_last
        );
        // Real revenue (deflated) stays in the sane 2026 band.
        let real = w.teams[0].revenue_last as f32 / w.inflation;
        assert!(
            (500.0..=1200.0).contains(&real),
            "real revenue left the plausible band: {}",
            real
        );
    }

    #[test]
    fn insolvent_clubs_fire_sale() {
        // Cash below zero forces sales even in a settled squad: the reshuffle
        // must move a surplus man down the pyramid, not freeze.
        let mut rng = Pcg64Mcg::new(102);
        let mut w = mini_world(&mut rng);
        // A smaller-but-close neighbor that can afford him, with low ambition
        // so the destination discipline passes.
        w.teams[1].league_idx = 0;
        w.teams[1].rep = 78;
        let deadwood = w.pool.len();
        w.pool.push(mkp("Deadwood", Position::MID, 70, 24, 70, 0, false));
        w.teams[0].players.push(deadwood);
        w.minds.push(PlayerMind::new(&mut rng));
        let di = w.minds.len() - 1;
        w.minds[di].ambition = 0.5;
        w.teams[1].revenue_last = 200;
        w.teams[0].cash = -100;
        transfer_window(&mut w, &mut rng);
        assert_ne!(
            w.pool[deadwood].team, 0,
            "insolvent club failed to move surplus man"
        );
        assert!(!w.pool[deadwood].retired, "surplus man destroyed instead of sold");
    }

    #[test]
    fn transfer_willing_requested_goes_lateral() {
        // A requesting settled starter may leave laterally (soft clause) but
        // never downward for money: dynasties rot through exits, not freefalls.
        // (Neither player is academy-trained and the two clubs aren't rivals,
        // so only the ordinary rules are in play.)
        let (a, b) = ("Umbrella Utd", "Sparta FC");
        assert!(transfer_willing(false, false, true, 0.9, 0.9, false, 95, 99, a, b, 4.0, 8.0));
        assert!(transfer_willing(false, false, true, 0.9, 0.9, false, 99, 99, a, b, 4.0, 8.0));
        assert!(!transfer_willing(false, false, true, 0.9, 0.9, false, 70, 99, a, b, 4.0, 8.0));
        // Unrequested settled starters need a clearly bigger club.
        assert!(!transfer_willing(false, false, false, 0.9, 0.9, false, 95, 99, a, b, 4.0, 8.0));
        assert!(transfer_willing(false, false, false, 0.9, 0.9, false, 99, 90, a, b, 4.0, 8.0));
        // Surplus men circulate within 15 rep points; ambitious ones refuse
        // even smaller drops, while low-ambition veterans wind down freely.
        assert!(transfer_willing(true, false, false, 0.9, 0.9, false, 80, 90, a, b, 4.0, 8.0));
        assert!(!transfer_willing(true, false, false, 0.9, 0.9, false, 70, 90, a, b, 4.0, 8.0));
        assert!(transfer_willing(true, false, false, 0.3, 0.9, false, 76, 90, a, b, 4.0, 8.0));
        assert!(!transfer_willing(true, false, false, 0.3, 0.9, false, 70, 90, a, b, 4.0, 8.0));
    }

    #[test]
    fn direct_rivals_never_trade_each_other() {
        // A met clause is not a Clásico. Crossing a direct rival is categorical:
        // an academy player never does it, and everyone else has to already have
        // demanded a move while being genuinely disloyal to even consider it.
        let (mad, bar) = ("Real Madrid", "FC Barcelona");
        assert!(!transfer_willing(false, true, true, 0.9, 0.9, true, 99, 95, mad, bar, 4.0, 8.0));
        assert!(!transfer_willing(false, true, false, 0.9, 0.05, false, 99, 95, mad, bar, 4.0, 8.0));
        assert!(!transfer_willing(true, true, false, 0.9, 0.05, false, 99, 95, mad, bar, 4.0, 8.0));
        assert!(transfer_willing(false, true, true, 0.9, 0.05, false, 99, 95, mad, bar, 4.0, 8.0));
        // The same deal between non-rivals is untouched.
        assert!(transfer_willing(false, true, true, 0.9, 0.9, false, 99, 95, mad, "Bayern", 4.0, 8.0));
        // The matrix itself, in both directions and with look-alike names.
        assert!(is_direct_rival("Paris SG", "OM"));
        assert!(is_direct_rival("OM", "Paris SG"));
        assert!(is_direct_rival("Juventus", "Lombardia FC"));
        assert!(!is_direct_rival("Real Madrid", "Real Betis"));
        assert!(!is_direct_rival("Arsenal", "Manchester City"));
        assert!(is_direct_rival("Milano FC", "Lombardia FC"));
        assert!(!is_direct_rival("Milano FC", "Juventus"));
    }

    #[test]
    fn title_drought_ends_cup_specialists() {
        // A big club whose current manager has gone years without a league
        // title must eventually sack him — no brave cup runs save a regime a
        // board expects to win leagues. Measured against the manager's tenure.
        let mut seen = false;
        for seed in 0..25u64 {
            let mut rng = Pcg64Mcg::new(1000 + seed as u128);
            let mut w = mini_world(&mut rng);
            w.teams[0].manager.title_drought = 8;
            // Keep him too young to retire and settled enough that the result
            // under test is the drought axe, not age or random impatience.
            w.teams[0].manager.age = 45;
            w.teams[0].manager.seasons = 8;
            let tables = vec![vec![0usize], vec![1usize]];
            let moves = post_season_evolution(&mut w, &tables, &HashSet::new(), &mut rng);
            if moves.iter().any(|m| m.contains("trophyless")) {
                seen = true;
                break;
            }
        }
        assert!(seen, "eight-year drought at a giant never cost a job");
    }

    #[test]
    fn managers_retire_with_age() {
        // Nobody runs a club into their mid-70s: an ancient manager must be
        // replaced, closing out any 40- or 50-season tenure.
        let mut seen = false;
        for seed in 0..20u64 {
            let mut rng = Pcg64Mcg::new(5000 + seed as u128);
            let mut w = mini_world(&mut rng);
            w.teams[0].manager.age = 71;
            w.teams[0].manager.seasons = 25;
            let tables = vec![vec![0usize], vec![1usize]];
            let moves = post_season_evolution(&mut w, &tables, &HashSet::new(), &mut rng);
            if moves.iter().any(|m| m.contains("retires")) {
                seen = true;
                break;
            }
        }
        assert!(seen, "a 71-year-old manager never retired across 20 seasons");
    }

    #[test]
    fn champions_get_restless() {
        // Highly ambitious title-winners in their prime must sometimes demand a
        // new challenge (the Ronaldo mechanism) — over many trials, at least
        // one request emerges. This is what rots dynasties from within.
        let mut rng = Pcg64Mcg::new(77);
        let mut seen = false;
        for _ in 0..100 {
            let mut w = mini_world(&mut rng);
            let star = w.pool.iter().position(|p| p.name == "Big Mid 0").unwrap();
            // Prime 85-ovr champion with sky-high ambition.
            w.pool[star].attrs = attrs_all(85);
            w.pool[star].ovr = 85;
            w.pool[star].pot = 90;
            w.pool[star].league_won = true;
            w.minds[star].ambition = 0.9;
            w.minds[star].requested = false;
            age_and_regen(&mut w, &mut rng);
            if w.minds[star].requested {
                seen = true;
                break;
            }
        }
        assert!(seen, "no restlessness in 100 champion-seasons");
    }

    #[test]
    fn tired_stars_are_not_surplus() {
        // End-of-season fatigue must never mislabel a starter as sellable:
        // an exhausted 80-ovr senior stays in the best XI over a fresh 78.
        let mut rng = Pcg64Mcg::new(31);
        let mut w = mini_world(&mut rng);
        let star = w.pool.iter().position(|p| p.name == "Big Mid 0").unwrap();
        w.pool[star].fatigue = 100;
        let xi = best_xi_indices(&w.teams[0].players, &w.pool);
        assert!(
            xi.contains(&star),
            "exhausted starter dropped from best XI by fatigue noise"
        );
    }

    #[test]
    fn career_totals_reconcile_with_seasons() {
        // Career counters must be exactly the sum of banked seasons — derived,
        // never independently simulated. Play a fake season into the pool,
        // reset, and check the arithmetic holds to the unit.
        let mut rng = Pcg64Mcg::new(21);
        let mut w = mini_world(&mut rng);
        w.pool[0].apps = 30;
        w.pool[0].goals = 12;
        w.pool[0].assists = 5;
        w.pool[0].key_passes = 40;
        w.pool[0].big_chances = 9;
        w.pool[0].tackles = 20;
        w.pool[0].interceptions = 15;
        w.pool[0].rating_sum = 210.0;
        w.pool[0].rating_apps = 30;
        w.pool[0].ucl_goals = 4;
        w.pool[0].ucl_assists = 2;
        reset_season(&mut w);
        let p = &w.pool[0];
        assert_eq!(p.career_goals, 12 + 4);
        assert_eq!(p.career_assists, 5 + 2);
        assert_eq!(p.career_kp, 40);
        assert_eq!(p.career_bcc, 9);
        assert_eq!(p.career_ti, 35);
        assert_eq!(p.career_apps, 30);
        assert!((p.career_rating_sum - 210.0).abs() < 1e-6);
        assert_eq!(p.career_rating_apps, 30);
        // ...and the season counters are wiped for the new year.
        assert_eq!(p.apps, 0);
        assert_eq!(p.goals, 0);
        assert_eq!(p.ucl_goals, 0);
        assert_eq!(p.key_passes, 0);
    }

    #[test]
    fn money_is_conserved_on_sales() {
        // Every fee a buyer pays must land in a seller's books: the sum of all
        // clubs' net transfer spend must exactly equal the agent fees paid —
        // fees cancel pairwise, only agent cuts leave the system.
        let mut rng = Pcg64Mcg::new(77);
        let mut w = mini_world(&mut rng);
        w.teams[0].cash = 500;
        let (_, _, _, agent_paid) = transfer_window(&mut w, &mut rng);
        let net: i64 = w.teams.iter().map(|t| t.net_spend).sum();
        assert_eq!(
            net, agent_paid,
            "money leaked: net spend {} != agent drain {}",
            net, agent_paid
        );
        for t in 0..w.teams.len() {
            assert!(w.teams[t].budget >= 0, "negative budget");
            assert!(
                w.teams[t].registered.len() <= MAX_REGISTERED,
                "registration overflow"
            );
        }
    }
}
