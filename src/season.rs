//! Season & multi-year sandbox: simulate the Big 5 leagues, the Champions
//! League, the Europa League and international tournaments (World Cup every
//! 4 years, Euros in between), for one season or a whole era, with aging,
//! attribute-based development, retirements and youth regens.
//!
//! All logic is original. Match strength and goal/assist attribution are
//! driven by each player's real attributes (pace, shooting, passing, …) via
//! `engine::simulate_score` (per-shot xG), so distributions look believable
//! (champion ~85-95 pts, golden boot ~25-36) and the best actual squads win.

use std::collections::HashMap;

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

const BIG5: [&str; 5] = [
    "Premier League",
    "La Liga",
    "Serie A",
    "Bundesliga",
    "Ligue 1",
];
const BASE_YEAR: u16 = 2026;
/// Squad-quality gap expander: raises the ratio between elite and weak sides
/// without pushing lambdas into blow-out territory (tuned against output).
/// Deliberately strong: the merged datasets compress ability (elite ~27,
/// mid ~22), so only a steep exponent restores real separation.
const STRENGTH_EXP: f32 = 1.7;

/// Nations treated as European for the Euros (best-effort by label; anything
/// not listed simply won't appear in the Euros, only the World Cup).
const EUROPE: &[&str] = &[
    "England",
    "France",
    "Spain",
    "Germany",
    "Italy",
    "Portugal",
    "Netherlands",
    "Holland",
    "Belgium",
    "Croatia",
    "Denmark",
    "Switzerland",
    "Austria",
    "Poland",
    "Serbia",
    "Scotland",
    "Ukraine",
    "Sweden",
    "Norway",
    "Wales",
    "Czech Republic",
    "Czechia",
    "Turkey",
    "Türkiye",
    "Republic of Ireland",
    "Ireland",
    "Hungary",
    "Greece",
    "Romania",
    "Slovakia",
    "Slovenia",
    "Russia",
    "Finland",
    "Iceland",
    "Northern Ireland",
    "Bosnia and Herzegovina",
    "Albania",
    "North Macedonia",
    "Bulgaria",
    "Montenegro",
    "Georgia",
    "Kosovo",
    "Luxembourg",
    "Armenia",
    "Azerbaijan",
    "Belarus",
    "Estonia",
    "Faroe Islands",
    "Kazakhstan",
    "Latvia",
    "Lithuania",
    "Malta",
    "Moldova",
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
    let mut cands: Vec<usize> = idxs.iter().copied().filter(|&i| !pool[i].retired).collect();
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
    let mut need = MIN_HOMEGROWN
        .min(hg_avail)
        .saturating_sub(reg.iter().filter(|&&i| pool[i].homegrown).count());
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

/// Players available to actually take the pitch: bans are served. Used at
/// matchday-squad build time so team-strength snapshots keep the full squad.
fn available(teams: &[Team], ti: usize, pool: &[WP]) -> Vec<usize> {
    selectable(teams, ti)
        .into_iter()
        .filter(|&i| pool.get(i).map(|p| p.ban_games == 0).unwrap_or(false))
        .collect()
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
    /// Permanent ID in the source database when this world player was loaded
    /// from `data/players.csv`. `None` identifies a fully generated world-only
    /// profile (including freshly persisted simregen players before they are
    /// reloaded from CSV in a later world).
    db_id: Option<PlayerId>,
    name: String,
    nation: String,
    pos: Position,
    attrs: Attributes,
    ovr: u8,
    pot: u8,
    age: u8,
    team: usize,
    retired: bool,
    /// Retired from international football only. The player keeps playing for
    /// a club and remains eligible for domestic cups, but no longer enters any
    /// national-team pool (World Cups, Euros, Asian Cups, friendlies).
    intl_retired: bool,
    intl_retire_age: u8,
    club_retire_age: u8,
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
    /// Matches the player is banned from (red card): 1 for a second yellow,
    /// 2 for a straight red. Served on the team's next fixtures.
    ban_games: u8,
    // detailed per-season stat lines (aggregated across all competitions),
    // fed by the possession engine so a spectator sees real match output.
    shots: u16,
    sot: u16,
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
    locked: u8, // years remaining on the current contract; not an immovable lock if a bigger-club offer and player intent align
    /// Lifetime earnings actually banked from salary, signing bonuses and
    /// performance bonuses -- the player's financial story, not his market
    /// fee (fees belong to clubs; agents were historically the only men
    /// who quietly left rich).
    career_earnings: i64,
    signing_bonus: i64,
    bonus_paid: i64,
    contract_option: bool,
    /// Career club path, appended on every permanent move — the scouting
    /// cascade record ("Bryne FK → Molde FK → Salzburg → Dortmund → City").
    /// Seeded lazily with the origin club on the player's first transfer.
    path: Vec<String>,
    // career (accumulated across seasons for the decade legends board)
    career_goals: u32,
    career_assists: u32,
    career_kp: u32,
    career_bcc: u32,
    career_ti: u32,
    career_cs: u32,
    career_apps: u32,
    career_rating_sum: f32,
    career_rating_apps: u32,
    ballons: u8,
    /// Peak window: seasons remaining in a genuine prime stretch (the
    /// "Salah in 17/18" effect), where key weapon attributes tick up
    /// together. Entered by elites 24-31, occasionally an "Indian summer"
    /// past 33 for disciplined gym-rats (the Messi-39 case).
    peak: u8,
    /// Intensity of the current peak (attribute points nudged per season).
    peak_str: u8,
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
    /// Appearances across every competition.
    fn overall_apps(&self) -> u16 {
        self.apps + self.ucl_apps + self.el_apps + self.intl_apps
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
        // repeatedly. Outlier scoring seasons remain possible; league prestige
        // and the transfer market, not an artificial scoring cap, contextualise them.
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

/// A simulated club. Its `nation` names its footballing country (for the
/// Club World Cup's two-clubs-per-country qualification cap, the equivalent
/// of FIFA's confederation rankings note).
struct Team {
    name: String,
    nation: String,
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
    /// Consecutive league titles (own domestic league). Drives the dynasty
    /// cycle: complacent boards refresh less, comfortable squads sag, and
    /// dynastic managers leave at the top — so no club wins 40 of 50.
    consec_titles: u8,
    /// Challenger momentum seasons remaining: a young squad breaking into the
    /// top four rides confidence (morale floor) and a braver transfer budget
    /// for a while — the Leicester/Monaco spike, and the force that pries
    /// leagues open while the giant is between eras.
    momentum: u8,
}

/// Pick the spatial `tactics::Shape` that best matches this AI team's (d,m,f)
/// line shape. Ambition nudges toward the more expansive variants (a brave
/// 3-5-2 pushes the wing-backs higher; a nervous 4-3-3 sits flatter), so two
/// clubs fielding the same numbers don't necessarily occupy the same pitch.
fn team_shape(t: &Team) -> crate::tactics::Shape {
    let (d, m, f) = t.shape;
    let ambitious = t.manager.ambition > 1.1;
    let f_ = match (d, m, f) {
        (4, 3, 3) => {
            if ambitious {
                Formation::F433
            } else {
                Formation::F433Flat
            }
        }
        (4, 5, 1) => Formation::F4141,
        (4, 4, 2) => Formation::F442,
        (3, 5, 2) => {
            if ambitious {
                Formation::F352Attacking
            } else {
                Formation::F352
            }
        }
        (3, 4, 3) => Formation::F343,
        (4, 2, 3) => Formation::F4411, // (4,2,3,1 collapsed) → 4-4-1-1 family
        (5, 3, 2) => Formation::F532,
        (5, 4, 1) => Formation::F541,
        _ => Formation::F442,
    };
    // Build the shape through tactics::shape so the AI's width/defensive-line
    // dials warp the base coordinates the same way the human's do.
    let instr = crate::model::TeamInstructions {
        width: 50,
        tempo: 50,
        directness: 50,
        pressing: 50,
        defensive_line: def_line(t),
        time_wasting: 50,
    };
    crate::tactics::shape(f_, &instr)
}

/// Effective defensive line (0..100) for an AI team, derived from manager
/// ambition: gung-ho coaches hold a higher line, pragmatists sit deeper.
fn def_line(t: &Team) -> u8 {
    (50.0 + (t.manager.ambition - 1.0) * 40.0)
        .round()
        .clamp(30.0, 78.0) as u8
}

impl Team {
    fn pts(&self) -> u16 {
        self.w * 3 + self.d
    }
    fn gd(&self) -> i32 {
        self.gf as i32 - self.ga as i32
    }
}

/// A persistent national-team coach. Quality measures tactical/coaching
/// influence while discipline captures the national coaching culture: Japan,
/// Germany and Spain are built around repeatable systems; part-time
/// associations and chronically changing benches rarely gain that same
/// defensive cohesion from one tournament to the next.
#[derive(Clone, Debug)]
struct NationalCoach {
    name: String,
    quality: f32,
    ambition: f32,
    discipline: f32,
    possession: f32,
    pressure: f32,
    directness: f32,
}

impl Default for NationalCoach {
    fn default() -> Self {
        Self {
            name: String::new(),
            quality: 1.0,
            ambition: 1.0,
            discipline: 0.65,
            possession: 0.55,
            pressure: 0.5,
            directness: 0.55,
        }
    }
}

/// The long-lived national-team programme stored in `World`. A `Nation`
/// transient is materialized for one tournament, but this profile is what
/// persists: the coach brain, the tactical identity, the working XI, and the
/// familiarity/cohesion built by shared selections and results over time.
#[derive(Clone, Debug)]
struct NationalState {
    coach: NationalCoach,
    formation: Formation,
    instructions: TeamInstructions,
    /// 0..1. How much of the working XI and structure carries forward.
    chemistry: f32,
    /// 0..1 tactical familiarisation. It grows only with shared fixtures and
    /// retained players, so a one-off all-star roster does not arrive as an
    /// expert running machine.
    familiarity: f32,
    /// The persisted first-choice XI (pool indices), for continuity.
    lineup: Vec<usize>,
    /// Full-season international experience for the federation programme.
    caps_together: u16,
    /// Seasons at the federation. Replacement ends the spell and resets the
    /// tactical upside that accumulated familiarity had created.
    tenure: u16,
}

impl Default for NationalState {
    fn default() -> Self {
        Self {
            coach: NationalCoach::default(),
            formation: Formation::F442,
            instructions: TeamInstructions::default(),
            chemistry: 0.0,
            familiarity: 0.0,
            lineup: Vec::new(),
            caps_together: 0,
            tenure: 0,
        }
    }
}

#[derive(Clone)]
struct Nation {
    name: String,
    players: Vec<usize>,
    strength: f32,
    state: NationalState,
    /// Current best XI for the tournament, drawn from state/current pool.
    lineup: Vec<usize>,
    /// Tournament sharpness multiplier (1.0 = normal): defending champions
    /// without elite depth arrive with sagging legs and a scouted gameplan
    /// against them — the structural reason golden generations don't repeat.
    edge: f32,
}

/// World Cup tournament accumulator for one player (isolated from
/// friendlies/other internationals so a World Cup run is auditable — e.g.
/// a 39-year-old's Indian-summer run can be read match by match).
#[derive(Default, Clone, Copy)]
struct WcAgg {
    apps: u32,
    goals: u32,
    assists: u32,
    shots: u32,
    sot: u32,
    xg: f32,
    kp: u32,
    bcc: u32,
    passes_att: u32,
    passes_cmp: u32,
    dribbles_att: u32,
    dribbles_cmp: u32,
    tackles: u32,
    interceptions: u32,
    fouls: u32,
    rating_sum: f32,
}

fn absorb_wc_stats(stats: &[matchsim::MatchStats], agg: &mut HashMap<usize, WcAgg>) {
    for s in stats {
        let e = agg.entry(s.id).or_default();
        e.apps += 1;
        e.goals += s.goals as u32;
        e.assists += s.assists as u32;
        e.shots += s.shots as u32;
        e.sot += s.shots_on_target as u32;
        e.xg += s.xg;
        e.kp += s.key_passes as u32;
        e.bcc += s.big_chances_created as u32;
        e.passes_att += s.passes_att as u32;
        e.passes_cmp += s.passes_cmp as u32;
        e.dribbles_att += s.dribbles_att as u32;
        e.dribbles_cmp += s.dribbles_cmp as u32;
        e.tackles += s.tackles as u32;
        e.interceptions += s.interceptions as u32;
        e.fouls += s.fouls as u32;
        e.rating_sum += s.rating;
    }
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
    /// AFC Asian Cup — a full continental championship for the AFC world.
    AsianCup,
    /// Copa América — CONMEBOL's continental championship.
    CopaAmerica,
    /// Africa Cup of Nations — CAF's continental championship.
    Afcon,
    /// CONCACAF Gold Cup — North/Central America + the Caribbean.
    GoldCup,
}

/// A simulated domestic league's display label and confederation, parallel to
/// `World::league_teams`. Indices 0..5 are the Big 5; 5 is the European
/// outsider group (no league of their own, confed UEFA); extras from 6 are
/// the AFC/CONMEBOL/CONCACAF top flights (their own tables and confed cups).
struct LeagueMeta {
    label: String,
    confed: Option<crate::world_model::Confederation>,
}

/// A long-lived Nemesis-style relationship between an actor and a target.
/// Actors and targets may be players or clubs; the label keeps them
/// inspectable in reports instead of treating grievances as random noise.
#[derive(Clone, Debug)]
struct NemesisRel {
    actor_kind: &'static str,
    actor_id: usize,
    target_kind: &'static str,
    target_id: usize,
    tension: f32,
    memory: u8,
    story: String,
}

struct World {
    pool: Vec<WP>,
    /// Parallel minds vector: minds[i] is the motives/memory of pool[i].
    /// Must stay in lockstep (push together, never remove pool entries).
    minds: Vec<PlayerMind>,
    teams: Vec<Team>,
    league_teams: Vec<Vec<usize>>,
    league_meta: Vec<LeagueMeta>,
    nation_pool: Vec<String>,
    next_regen: u32,
    elite_cap: u8,
    /// Transfer-market inflation multiplier; grows each year so late-era fees
    /// stay big (a 2070 galáctico still costs hundreds of millions).
    inflation: f32,
    /// Active season-long loans: (player, home club, away club).
    loans: Vec<SeasonLoan>,
    /// Persistent Nemesis records: departures, sacked managers and failed
    /// contract negotiations continue to colour later dealings and performances.
    nemesis: Vec<NemesisRel>,
    /// Host-queue for the World Cup: each entry is a bid-won host nation set
    /// for one tournament cycle (2030, 2034…). The 2026 tournament's real
    /// hosts (USA, Canada, Mexico) are the queue head; after every World Cup,
    /// a bid vote produces the *next* tournament's host set and pushes it here.
    host_queue: Vec<Vec<String>>,
    /// Host nations of the last two tournaments (disqualifies them from
    /// bidding for the next one, like FIFA's rotation policy).
    recent_hosts: Vec<String>,
    /// Last two World Cup winners (most recent last) — the defending-champion
    /// reality check. A repeat is only believable for a nation with genuine
    /// elite depth (France/Brazil class); a thin golden generation ages four
    /// years and finds no replacements, so it does not go back-to-back.
    wc_hist: Vec<String>,
    /// Continental champions history for Club World Cup qualification:
    /// (competition label — "UCL"/"AFC"/"CONCACAF"/"CONMEBOL", winning team).
    /// The CWC hands out its 4-year-cycle slots to these winners.
    continent_hist: Vec<(String, usize)>,
    /// Released players without a club; train alone and can be signed freely.
    free_agents: Vec<usize>,
    /// Long-lived national programmes: each federation's coach, tactical
    /// system, working XI and familiarity. Countries do not rebuild an expert
    /// unit every summer merely because a new dataset drops in.
    national_states: HashMap<String, NationalState>,
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
    /// Confederation cup winners for the non-UEFA world (AFC Champions League,
    /// CONCACAF Champions Cup, CONMEBOL Libertadores).
    afc_winner: String,
    concacaf_winner: String,
    conmebol_winner: String,
    /// FIFA Club World Cup winner (in CWC years; "-" otherwise).
    club_world_cup_winner: String,
    intl_kind: IntlKind,
    intl_winner: String,
    /// World Cup host nations this cycle (empty in non-WC years).
    wc_hosts: Vec<String>,
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
            let t = if r_hi > r_lo {
                (r - r_lo) / (r_hi - r_lo)
            } else {
                0.0
            };
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

/// Rating of the weakest current starter at a specific position. This lets a
/// club judge an exceptional market opportunity against the player he would
/// actually replace, rather than against an unrelated weakest XI position.
fn weakest_slot_at_position(idxs: &[usize], pool: &[WP], pos: Position) -> f32 {
    best_xi_indices(idxs, pool)
        .into_iter()
        .filter(|&i| pool[i].pos == pos)
        .map(|i| pool[i].attrs.overall(pos) as f32)
        .reduce(f32::min)
        .unwrap_or(0.0)
}

/// A public clause at or below the player's current market value is a genuine
/// market opportunity. Clubs may pursue it even when that position is not the
/// single weakest slot in their XI; ordinary transfers remain need-led.
fn is_clause_opportunity(p: &WP) -> bool {
    p.release_clause
        .is_some_and(|clause| clause > 0 && clause <= p.value.max(1))
}

fn acquisition_incumbent(
    buyer_players: &[usize],
    pool: &[WP],
    candidate: &WP,
    need_pos: Position,
    need_incumbent: f32,
) -> Option<f32> {
    if candidate.pos == need_pos {
        Some(need_incumbent)
    } else if is_clause_opportunity(candidate) {
        Some(weakest_slot_at_position(buyer_players, pool, candidate.pos))
    } else {
        None
    }
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
/// slots with the best available players by actual FC overall (the database
/// metric of record), assigning each their best-fitting role on that line.
/// On-loan players belong to the loanee. Positional preference is a small
/// bonus only — the XI is built from natural overall, not blended role math.
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
            .filter(|&i| !(line == 0 && pool[i].pos != Position::GK))
            .map(|i| {
                let w = &pool[i];
                // Actual FC overall is the primary score; positional line is a
                // strong bonus so volunteers stay on their line, and role fit
                // only nudges ties — the squad best eleven, honestly.
                let line_bonus = if line_of(w.pos) == line { 1.0 } else { 0.82 };
                (w.ovr as f32 * line_bonus, i)
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
            .map(|i| (pool[i].ovr as f32, i))
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

fn match_strength(
    idxs: &[usize],
    pool: &[WP],
    rep: u8,
    shape: (usize, usize, usize),
    p: &Params,
) -> f32 {
    // Club power = the XI's actual FC overall (the database metric of record),
    // as its players were selected in select_xi. Form and morale previously
    // weighted selection through `effective()`; actual overall is the primary
    // driver — the squad is picked exactly like the display rating says, and
    // form rides as a mere tilt, not a blended blend.
    let base = {
        let xi = select_xi(idxs, pool, shape);
        if xi.is_empty() {
            10.0
        } else {
            let sum: f32 = xi.iter().map(|&(i, _, _)| pool[i].ovr as f32).sum();
            sum / xi.len() as f32
        }
    };
    // Squad-led blend: the actual XI matters most, reputation refines.
    // Then a zero-point shift so a few points of squad gap become real
    // separation (elite ~10+, mid ~6-8, weak ~3) instead of noise.
    let blended =
        base * p.blend_squad + (p.rep_base + rep as f32 * p.rep_slope) * (1.0 - p.blend_squad);
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
                    score(b)
                        .partial_cmp(&score(a))
                        .unwrap_or(std::cmp::Ordering::Equal)
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
        score(b)
            .partial_cmp(&score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for &b in &bench {
        if squad.len() >= 16 {
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
                (1.0 - 0.005 * (sg - 22) as f32).max(0.90)
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
                traits: Vec::new(),
                weak_foot: 3,
                foot: crate::model::Foot::Right,
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
        // Guest-friendly opponents use synthetic ids outside the pool
        // (irrelevant regens with no career to credit): skip them silently.
        if i >= pool.len() {
            continue;
        }
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

        // Discipline: a sending-off costs the next fixtures — one match for
        // a second yellow, two for a straight red.
        if s.sent_off {
            pool[i].ban_games = if s.yellows >= 2 { 1 } else { 2 };
        }

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
        // Durability is a real skill: high-stamina professionals accumulate
        // less match fatigue, so a healthy elite stays available instead of
        // being ground down by the same flat cost as a fragile journeyman.
        let stamina_factor = 1.12 - (pool[i].attrs.stamina as f32 / 100.0) * 0.30;
        let mut fat = (base_fatigue as f32 * stamina_factor).round() as u8;
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
///
/// The detailed entry point returns the full `MatchOutcome` so the API can
/// expose per-player stat lines and minute-by-minute events without running
/// a second match. `play` remains the compatibility wrapper used everywhere
/// else in the season engine.
fn play_detailed(
    home: usize,
    away: usize,
    comp: Comp,
    venue: Venue,
    agg: (u8, u8),
    teams: &mut [Team],
    pool: &mut [WP],
    p: &Params,
    rng: &mut Pcg64Mcg,
) -> Option<matchsim::MatchOutcome> {
    let hsel = available(teams, home, pool);
    let asel = available(teams, away, pool);
    let hmd = matchday_squad(
        &hsel,
        pool,
        teams[home].shape,
        &teams[home].manager.clone(),
        comp,
        rng,
    );
    let amd = matchday_squad(
        &asel,
        pool,
        teams[away].shape,
        &teams[away].manager.clone(),
        comp,
        rng,
    );
    if hmd.is_empty() || amd.is_empty() {
        return None;
    }
    let hxi = build_lineup(&hmd, pool);
    let axi = build_lineup(&amd, pool);
    // The matchday squad's spare bodies (12th-16th man) are the bench: five
    // substitution windows live inside the engine, so rotation actually
    // happens and squad players bank real minutes instead of a season of
    // nothing.
    let hbench = build_lineup(&hmd[11.min(hmd.len())..], pool);
    let abench = build_lineup(&amd[11.min(amd.len())..], pool);
    let mut hxi = hxi;
    let mut axi = axi;
    // A man facing his old club tends to find one extra gear; this does not
    // decide a season, but it is exactly the kind of personal motive an
    // abstract "revenge game" multiplier should capture.
    for mp in &mut hxi {
        if pool[mp.id].path.iter().any(|n| *n == teams[away].name) {
            mp.form = (mp.form * 1.03).clamp(0.5, 1.2);
        }
    }
    for mp in &mut axi {
        if pool[mp.id].path.iter().any(|n| *n == teams[home].name) {
            mp.form = (mp.form * 1.03).clamp(0.5, 1.2);
        }
    }
    let mut side_h = Side {
        players: &hxi,
        ambition: teams[home].manager.ambition,
        quality: teams[home].manager.quality,
        threat: 1.0,
        shape: Some(team_shape(&teams[home])),
        width: 50,
        defensive_line: def_line(&teams[home]),
        bench: &hbench,
        crowd: 1.0,
    };
    let mut side_a = Side {
        players: &axi,
        ambition: teams[away].manager.ambition,
        quality: teams[away].manager.quality,
        threat: 1.0,
        shape: Some(team_shape(&teams[away])),
        width: 50,
        defensive_line: def_line(&teams[away]),
        bench: &abench,
        crowd: 1.0,
    };
    // Both dugouts scout the matchup off the two XIs (never reputation):
    // underdogs start cagier, favourites start on the front foot.
    let (th, ta) = matchsim::threat_ratio(&side_h, &side_a);
    side_h.threat = th;
    side_a.threat = ta;
    let o = matchsim::simulate(&side_h, &side_a, venue, agg, rng);
    apply_match(&o, comp, pool, p);
    // Bans tick down after every fixture: a banned player misses exactly his
    // team's next 1-2 matches.
    for &pi in teams[home].players.iter().chain(teams[away].players.iter()) {
        if let Some(p) = pool.get_mut(pi) {
            p.ban_games = p.ban_games.saturating_sub(1);
        }
    }
    Some(o)
}

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
    play_detailed(home, away, comp, venue, agg, teams, pool, p, rng)
        .map(|o| (o.goals_a as u32, o.goals_b as u32))
        .unwrap_or((0, 0))
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
            // Recovery is individualized: stamina and professionalism decide
            // how much of the week's rest actually lands. A durable pro
            // clears fatigue faster; a fragile, unprofessional player does not.
            let recover = (p.fatigue_rest
                * (0.70 + pool[i].attrs.stamina as f32 / 100.0 * 0.60)
                * (0.85 + minds[i].professionalism * 0.30))
                .round() as u8;
            pool[i].fatigue = pool[i].fatigue.saturating_sub(recover);
            let target = minds[i].morale_target();
            if pool[i].morale < target {
                pool[i].morale += 1;
            } else if pool[i].morale > target {
                pool[i].morale -= 1;
            }
            let apps = pool[i].apps;
            let morale = &mut pool[i].morale;
            minds[i].evaluate(apps, played, rank, morale);
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
                let (hg, ag) = play(
                    home,
                    away,
                    Comp::League,
                    Venue::HomeA,
                    (0, 0),
                    teams,
                    pool,
                    p,
                    rng,
                );
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
    let (g2b, g2a) = play(
        b,
        a,
        comp,
        Venue::HomeA,
        (g1b as u8, g1a as u8),
        teams,
        pool,
        p,
        rng,
    );
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
fn run_domestic_cup(
    ids: &[usize],
    teams: &[Team],
    p: &Params,
    rng: &mut Pcg64Mcg,
) -> Option<(usize, usize)> {
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
fn super_cup_winner(
    field: &[usize],
    teams: &[Team],
    p: &Params,
    rng: &mut Pcg64Mcg,
) -> Option<usize> {
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

/// Majority nationality of a squad — the data-driven fallback when the league
/// registry can't name a club's country. A league label ("Ligue 1") or a
/// continent ("Europe") is NOT a nation: such a value must never become a
/// club's country, because academy regens inherit it and "Ligue 1" then
/// qualifies for World Cups.
fn majority_nation(idxs: &[usize], pool: &[WP]) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for &i in idxs {
        let n = pool[i].nation.trim();
        if n.len() >= 2 {
            *counts.entry(n).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(n, _)| n.to_string())
        .unwrap_or_else(|| "Europe".to_string())
}

/// National federation grant: a nation whose clubs produce at a high level
/// (strong home league, European minutes, deep pool) gets its development
/// infrastructure co-funded. Real federations (the JFA's nationwide training
/// centers, FIFA's worldwide academies programme) exist precisely so
/// disciplined midsized nations don't die out — their home academy isn't
/// PSG-tier, but it doesn't collapse either. Returns the academy-tier bonus
/// (+0.0..+1.0) for a club of this nation based on its federation's output.
fn federation_bonus(w: &World, nation: &str) -> f32 {
    let mut tops: Vec<i32> = w
        .pool
        .iter()
        .filter(|p| !p.retired && p.nation == nation && p.age <= 23)
        .map(|p| p.ovr as i32)
        .collect();
    if tops.is_empty() {
        return 0.0;
    }
    tops.sort_unstable();
    let top_avg = (tops.iter().rev().take(11).sum::<i32>() as f32) / (tops.len().min(11) as f32);
    // Depth beats one-star output: a nation whose whole youth bench is
    // 70+ grows its academies, wherever its ceiling sits.
    ((top_avg - 62.0) / 18.0).clamp(0.0, 1.0) * federation_capacity(nation)
}

/// Current football infrastructure prior. Population and the number of CSV
/// records are not development quality: Japan's established pathway should
/// outperform China/India today even when the latter have larger raw pools.
/// Long-term club investment can still move a nation upward, but only slowly.
fn federation_capacity(nation: &str) -> f32 {
    let rank = crate::world_model::nations()
        .into_iter()
        .find(|n| n.name == nation)
        .map(|n| n.ranking)
        .unwrap_or(150);
    match rank {
        1..=20 => 1.00,
        21..=35 => 0.88,
        36..=55 => 0.72,
        56..=80 => 0.48,
        81..=120 => 0.30,
        _ => 0.18,
    }
}

fn federation_academy_cap(nation: &str) -> u8 {
    let capacity = federation_capacity(nation);
    if capacity >= 0.95 {
        5
    } else if capacity >= 0.70 {
        4
    } else if capacity >= 0.45 {
        4
    } else if capacity >= 0.25 {
        3
    } else {
        2
    }
}

fn federation_player_cap(nation: &str, elite_cap: u8) -> u8 {
    let capacity = federation_capacity(nation);
    if capacity >= 0.95 {
        elite_cap
    } else if capacity >= 0.85 {
        elite_cap.saturating_sub(2)
    } else if capacity >= 0.70 {
        elite_cap.min(84)
    } else if capacity >= 0.45 {
        elite_cap.min(80)
    } else if capacity >= 0.25 {
        elite_cap.min(77)
    } else {
        elite_cap.min(74)
    }
}

fn valid_nationality(nation: &str) -> bool {
    crate::world_model::nations()
        .iter()
        .any(|n| n.name == nation)
}

/// Aditya Singh is a real player in the main 2026 database (Kerala
/// Blasters, ISL). With `FM_ADITY_PRODIGY=1` this opt-in hook retunes that
/// real player to the focal-prodigy experiment spec (maxed 99 forward,
/// 5m clause) and enables the focal save; without it, he simply lives a
/// normal database career.
fn inject_aditya_prodigy(
    pool: &mut Vec<WP>,
    teams: &mut [Team],
    league_meta: &[LeagueMeta],
    params: &Params,
) -> Option<usize> {
    if std::env::var("FM_ADITY_PRODIGY").ok().as_deref() != Some("1") {
        return None;
    }
    if let Some(idx) = pool.iter().position(|p| p.name == "Aditya Singh") {
        // Retune the real database player to the focal spec in place.
        let value = market_value(99, 15, Position::FWD);
        let p = &mut pool[idx];
        // Winger shape: maxed pace/creation, slightly less finishing,
        // heading and physical presence — still rounds to a 99 FWD overall.
        p.attrs = Attributes {
            pace: 99,
            shooting: 98,
            passing: 98,
            dribbling: 99,
            tackling: 35,
            heading: 98,
            strength: 78,
            stamina: 95,
            vision: 99,
            composure: 99,
            goalkeeping: 8,
            first_touch: 99,
        };
        p.ovr = 99;
        p.pot = 99;
        p.age = 15;
        p.morale = 82;
        p.value = value;
        p.release_clause = Some(5);
        p.wage = (value / 3).max(1);
        p.locked = 5;
        p.contract_option = true;
        p.homegrown = true;
        return Some(idx);
    }
    let isl = league_meta
        .iter()
        .position(|m| m.label == "Indian Super League")?;
    let team = teams
        .iter()
        .position(|t| t.league_idx == isl && t.name.contains("Kerala"))
        .or_else(|| teams.iter().position(|t| t.league_idx == isl))?;
    // 99 FWD overall exactly: winger-shaped profile (maxed pace/creation,
    // slightly less finishing/heading/physicality) that still rounds to 99.
    let attrs = Attributes {
        pace: 99,
        shooting: 98,
        passing: 98,
        dribbling: 99,
        tackling: 35,
        heading: 98,
        strength: 78,
        stamina: 95,
        vision: 99,
        composure: 99,
        goalkeeping: 8,
        first_touch: 99,
    };
    debug_assert_eq!(attrs.overall(Position::FWD), 99);
    let value = market_value(99, 15, Position::FWD);
    let idx = pool.len();
    pool.push(WP {
        // `data/players.csv` represents Aditya in the normal loader; if the
        // row is ever absent and a fresh focal player is synthesized, leave
        // it world-only rather than pointing at a stale external identifier.
        db_id: None,
        name: "Aditya Singh".to_string(),
        nation: "India".to_string(),
        pos: Position::FWD,
        attrs,
        ovr: 99,
        pot: 99,
        age: 15,
        team,
        retired: false,
        intl_retired: false,
        intl_retire_age: 0,
        club_retire_age: 0,
        morale: 82,
        fatigue: 0,
        adapt: 0,
        on_loan: false,
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
        ban_games: 0,
        shots: 0,
        sot: 0,
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
        value,
        release_clause: Some(5),
        wage: (value / 3).max(1),
        locked: 5,
        career_earnings: 0,
        signing_bonus: 0,
        bonus_paid: 0,
        contract_option: true,
        path: Vec::new(),
        career_goals: 0,
        career_assists: 0,
        career_kp: 0,
        career_bcc: 0,
        career_ti: 0,
        career_cs: 0,
        career_apps: 0,
        career_rating_sum: 0.0,
        career_rating_apps: 0,
        ballons: 0,
        peak: 0,
        peak_str: 1,
    });
    teams[team].players.push(idx);
    let shape = pick_shape(&teams[team].players, pool);
    teams[team].shape = shape;
    teams[team].strength =
        match_strength(&teams[team].players, pool, teams[team].rep, shape, params);
    Some(idx)
}

fn players_csv_cell(value: &str) -> String {
    let escaped = value.replace('"', "\"\"");
    if escaped.contains(',') || escaped.contains('"') || escaped.contains('\n') {
        format!("\"{}\"", escaped)
    } else {
        escaped
    }
}

fn player_csv_id_for_generation(index: usize) -> u32 {
    9_900_001u32 + index.saturating_sub(1) as u32
}

/// Append the initial world-build regen roster to `data/players.csv`, once.
///
/// These are genuine national-team-eligible players with club, league,
/// attributes and youth headroom, not anonymous friendlies-only filler:
/// their `nation` mirrors the club they were initially generated for, so
/// they can later be called up by that country. The `simregen` marker makes
/// the append idempotent; the exporter deliberately uses the **initial**
/// roster (before season play), while all later youth and career changes
/// continue to live in the world pool / `careers.json`.
fn append_initial_regen_players_csv(
    path: &str,
    pool: &[WP],
    teams: &[Team],
    league_meta: &[LeagueMeta],
    regen_start: usize,
) -> usize {
    let Ok(content) = std::fs::read_to_string(path) else {
        return 0;
    };
    if content.contains(",simregen,") {
        return 0;
    }

    let mut lines = content.lines();
    let Some(header_line) = lines.next() else {
        return 0;
    };
    let headers = crate::data::parse_csv_line(header_line);
    let header_index = |name: &str| headers.iter().position(|h| h.eq_ignore_ascii_case(name));
    let Some(i_common) = header_index("common_name") else {
        return 0;
    };
    let Some(i_first) = header_index("first_name") else {
        return 0;
    };
    let Some(i_last) = header_index("last_name") else {
        return 0;
    };
    let Some(i_edition) = header_index("edition") else {
        return 0;
    };

    let mut existing_names = std::collections::HashSet::new();
    let mut existing_ids = std::collections::HashSet::new();
    for line in lines {
        let fields = crate::data::parse_csv_line(line);
        if fields.len() < headers.len() / 2 {
            continue;
        }
        if let Some(id_text) = fields.first() {
            if let Ok(id) = id_text.trim().parse::<u32>() {
                existing_ids.insert(id);
            }
        }
        if fields.get(i_edition).map(|v| v.trim()) == Some("simregen") {
            return 0;
        }
        let common = fields.get(i_common).map(|v| v.trim()).unwrap_or("");
        let display = if common.trim().len() >= 2 {
            crate::data::clean_name(common)
        } else {
            crate::data::clean_name(
                format!(
                    "{} {}",
                    fields.get(i_first).map(|v| v.trim()).unwrap_or(""),
                    fields.get(i_last).map(|v| v.trim()).unwrap_or("")
                )
                .trim(),
            )
        };
        if display.trim().len() >= 2 {
            existing_names.insert(display.to_lowercase());
        }
    }

    let start = regen_start.min(pool.len());
    let mut out = String::new();
    let mut next = 1usize;
    let mut exported = 0usize;
    for p in &pool[start..] {
        let Some(team) = teams.get(p.team) else {
            continue;
        };
        let normalized_name = crate::data::clean_name(&p.name);
        if normalized_name.trim().len() < 2
            || existing_names.contains(&normalized_name.to_lowercase())
        {
            continue;
        }
        let id = player_csv_id_for_generation(next);
        next += 1;
        if existing_ids.contains(&id) {
            continue;
        }
        existing_names.insert(normalized_name.to_lowercase());
        existing_ids.insert(id);
        let (first_name, last_name) = match normalized_name.split_once(' ') {
            Some((f, l)) => (f.to_string(), l.to_string()),
            None => (normalized_name.clone(), String::new()),
        };
        let position = match p.pos {
            Position::GK => "GK",
            Position::DEF => "LB",
            Position::MID => "CM",
            Position::FWD => "LW",
        };
        let birth_year = (2026i32 - p.age as i32).clamp(1950, 2026);
        let league = league_meta
            .get(team.league_idx)
            .map(|m| m.label.as_str())
            .unwrap_or("Regen Division");
        let row = [
            id.to_string(),
            players_csv_cell(&normalized_name),
            players_csv_cell(&first_name),
            players_csv_cell(&last_name),
            p.ovr.to_string(),
            position.to_string(),
            String::new(),
            players_csv_cell(&team.name),
            players_csv_cell(league),
            players_csv_cell(&p.nation),
            "Men's Football".to_string(),
            "Right".to_string(),
            "3".to_string(),
            "3".to_string(),
            "178".to_string(),
            "75".to_string(),
            format!("{:04}-01-01", birth_year),
            String::new(),
            p.attrs.pace.to_string(),
            p.attrs.shooting.to_string(),
            p.attrs.passing.to_string(),
            p.attrs.dribbling.to_string(),
            p.attrs.tackling.to_string(),
            p.attrs.strength.to_string(),
            p.attrs.passing.to_string(),
            p.attrs.shooting.to_string(),
            p.attrs.heading.to_string(),
            p.attrs.passing.to_string(),
            p.attrs.shooting.to_string(),
            p.attrs.dribbling.to_string(),
            p.attrs.vision.to_string(),
            p.attrs.shooting.to_string(),
            p.attrs.passing.to_string(),
            p.attrs.first_touch.to_string(),
            p.attrs.pace.to_string(),
            p.attrs.pace.to_string(),
            p.attrs.dribbling.to_string(),
            p.attrs.composure.to_string(),
            p.attrs.first_touch.to_string(),
            p.attrs.shooting.to_string(),
            p.attrs.heading.to_string(),
            p.attrs.stamina.to_string(),
            p.attrs.strength.to_string(),
            p.attrs.shooting.to_string(),
            p.attrs.tackling.to_string(),
            p.attrs.tackling.to_string(),
            p.attrs.tackling.to_string(),
            p.attrs.vision.to_string(),
            p.attrs.composure.to_string(),
            p.attrs.composure.to_string(),
            p.attrs.tackling.to_string(),
            p.attrs.tackling.to_string(),
            p.attrs.tackling.to_string(),
            p.attrs.goalkeeping.to_string(),
            p.attrs.goalkeeping.to_string(),
            p.attrs.goalkeeping.to_string(),
            p.attrs.goalkeeping.to_string(),
            p.attrs.goalkeeping.to_string(),
            "simregen".to_string(),
            "2026-09-12".to_string(),
        ]
        .join(",");
        out.push_str(&row);
        out.push('\n');
        exported += 1;
    }
    if exported == 0 {
        return 0;
    }
    use std::io::Write;
    match std::fs::OpenOptions::new().append(true).open(path) {
        Ok(mut file) => {
            if file.write_all(out.as_bytes()).is_err() {
                return 0;
            }
            exported
        }
        Err(_) => 0,
    }
}

fn build_world(db: &Database, params: Params, rng: &mut Pcg64Mcg) -> World {
    let mut pool: Vec<WP> = Vec::new();
    let mut teams: Vec<Team> = Vec::new();
    // league_teams is allocated later, once the extra-league count is known.
    // A single 2026 dump can still list the same man twice — at two clubs,
    // under subtly different spellings ("Kylian Mbappe" vs "Kylian Mbappe
    // Lottin"), or as a duplicate row. Group every candidate by first name +
    // position, then fold rows that share surname-length tokens within a few
    // years of age into ONE man, keeping the best row (highest overall; ties
    // prefer the older age). The survivor keeps the better club, while
    // genuinely different players sharing a first name are untouched.
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
        let subset = a_t.iter().all(|t| b_t.contains(t)) || b_t.iter().all(|t| a_t.contains(t));
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
    // Real European outsiders (Track 5 / world expansion): the top 8 clubs by
    // reputation from the other UEFA tier-1 leagues in the database — Porto,
    // Benfica, Ajax, Celtic, Brugge, Galatasaray… — loaded with their real
    // squads instead of procedural farmers. They play no domestic league in
    // the sandbox (league_idx 5) but contest the UCL, Europa and the market.
    let mut oc: Vec<&Club> = db
        .clubs()
        .iter()
        .filter(|c| {
            if BIG5.iter().any(|b| c.league.eq_ignore_ascii_case(b)) {
                return false;
            }
            match crate::world_model::lookup(&c.league) {
                Some(l) => l.confed == crate::world_model::Confederation::UEFA && l.tier == 1,
                None => false,
            }
        })
        .collect();
    oc.sort_by(|a, b| b.reputation.cmp(&a.reputation));
    let outsider_clubs: Vec<ClubId> = oc.iter().take(8).map(|c| c.id).collect();

    // Non-UEFA world (Track 5 / world expansion): every other tier-1 men's
    // league in the database, grouped by the catalogue — AFC (ISL, Saudi,
    // K League, CSL…), CONMEBOL (Argentina, Brazil, Colombia) and CONCACAF
    // (MLS, Mexico). Each becomes a *simulated* domestic league (tables,
    // champions, domestic cup, and a confederation cup), not just a one-off
    // club injection. Up to 20 strongest clubs per league.
    let mut extra_groups: Vec<(Vec<ClubId>, crate::world_model::Confederation, String)> =
        Vec::new();
    for l in crate::world_model::catalogue() {
        if l.confed == crate::world_model::Confederation::UEFA || l.tier != 1 {
            continue;
        }
        let mut clubs: Vec<&Club> = db
            .clubs()
            .iter()
            .filter(|x| crate::world_model::lookup(&x.league).map(|y| y.id) == Some(l.id))
            .collect();
        clubs.sort_by(|a, b| b.reputation.cmp(&a.reputation));
        clubs.truncate(20);
        if clubs.len() >= 8 {
            extra_groups.push((
                clubs.iter().map(|c| c.id).collect(),
                l.confed,
                l.name.clone(),
            ));
        }
    }
    let extra_count = extra_groups.len();
    let regen_start = pool.len();

    // Regen leagues: first-tier competitions the dump does not carry
    // playable clubs/players for (one Qatari, six Iranian/Uzbek men in
    // 19,790 rows). Clubs are real names, squads procedural regens — so
    // Iran, Qatar, Uzbekistan, Jordan, Indonesia and Iraq field genuine
    // 15+ national pools and Asian qualification is contested on merit,
    // and India/China must beat them to earn a World Cup slot.
    // (label, nation, tier, clubs, rep range)
    // TIER calibration (post-hoc): Gen::player samples key attributes
    // uniformly up to (tier+13)*3 and national XIs pick the top 11 of
    // ~100-150 regens, so the tier must be far lower than the intended
    // XI strength. Tier 16 ≈ low-70s XI ceiling; 13-15 ≈ high-60s.
    let regen_leagues: &[(&str, &str, i32, u8, u8, &[&str])] = &[
        (
            "J-League",
            "Japan",
            16,
            62,
            70,
            &[
                "Vissel Kobe",
                "Yokohama F. Marinos",
                "Kawasaki Frontale",
                "Urawa Red Diamonds",
                "Gamba Osaka",
                "Kashiwa Reysol",
                "FC Tokyo",
                "Cerezo Osaka",
            ],
        ),
        (
            "Iran Pro League",
            "Iran",
            17,
            56,
            64,
            &[
                "Persepolis",
                "Esteghlal",
                "Sepahan",
                "Tractor",
                "Zob Ahan",
                "Foolad",
                "Paykan",
                "Malavan",
            ],
        ),
        (
            "Qatar Stars League",
            "Qatar",
            16,
            54,
            62,
            &[
                "Al Sadd",
                "Al Duhail",
                "Al Gharafa",
                "Al Rayyan",
                "Al Arabi",
                "Al Wakrah",
            ],
        ),
        (
            "Uzbekistan Super League",
            "Uzbekistan",
            17,
            52,
            60,
            &[
                "Bunyodkor",
                "Pakhtakor",
                "Nasaf",
                "AGMK",
                "Navbahor",
                "Sogdiana",
            ],
        ),
        (
            "Jordan Pro League",
            "Jordan",
            16,
            48,
            56,
            &[
                "Al Wehdat",
                "Al Faisaly",
                "Al Hussein",
                "Al Salt",
                "Shabab Al Urdun",
                "Al Ramtha",
            ],
        ),
        (
            "Indonesia Liga 1",
            "Indonesia",
            15,
            48,
            56,
            &[
                "Persija Jakarta",
                "Persib Bandung",
                "Arema FC",
                "Bali United",
                "Borneo FC",
                "Persebaya",
                "PSIS Semarang",
                "Madura United",
            ],
        ),
        (
            "Iraq Stars League",
            "Iraq",
            15,
            46,
            54,
            &[
                "Al Shorta",
                "Al Quwa Al Jawiya",
                "Al-Zawraa",
                "Naft Al Wasat",
                "Al Talaba",
                "Erbil",
            ],
        ),
        // ── CONMEBOL: domestic top flights beyond the DB's cup groups ────
        (
            "Brazilian Serie A",
            "Brazil",
            18,
            66,
            74,
            &[
                "Palmeiras",
                "Flamengo",
                "Sao Paulo",
                "Corinthians",
                "Santos",
                "Gremio",
                "Internacional",
                "Bahia",
            ],
        ),
        (
            "Chilean Primera Division",
            "Chile",
            16,
            52,
            60,
            &[
                "Colo-Colo",
                "Universidad de Chile",
                "Universidad Catolica",
                "Cobresal",
                "Union Espanola",
                "Deportes Antofagasta",
            ],
        ),
        (
            "Uruguayan Primera Division",
            "Uruguay",
            18,
            60,
            68,
            &[
                "Penarol",
                "Nacional",
                "Defensor",
                "Montevideo City Torque",
                "Danubio",
                "Liverpool Montevideo",
            ],
        ),
        (
            "Peruvian Primera Division",
            "Peru",
            15,
            50,
            58,
            &[
                "Universitario",
                "Alianza Lima",
                "Sporting Cristal",
                "Melgar",
                "Cienciano",
                "Comerciantes Unidos",
            ],
        ),
        (
            "Paraguayan Division de Honor",
            "Paraguay",
            15,
            50,
            58,
            &[
                "Club Olimpia",
                "Cerro Porteno",
                "Libertad",
                "Sportivo Luqueno",
                "Guarani",
                "Nacional Asuncion",
            ],
        ),
        (
            "Bolivian Primera Division",
            "Bolivia",
            14,
            46,
            54,
            &[
                "Bolivar",
                "The Strongest",
                "Wilstermann",
                "Blooming",
                "Oriente Petrolero",
                "Royal Pari",
            ],
        ),
        (
            "Ecuadorian Serie A",
            "Ecuador",
            16,
            52,
            60,
            &[
                "Barcelona SC",
                "Emelec",
                "Liga de Quito",
                "Aucas",
                "Independiente del Valle",
                "Orense",
            ],
        ),
        (
            "Venezuelan Primera Division",
            "Venezuela",
            14,
            46,
            54,
            &[
                "Caracas FC",
                "Deportivo Tachira",
                "Deportivo La Guaira",
                "Portuguesa",
                "Estudiantes de Merida",
                "Carabobo",
            ],
        ),
        // ── UEFA: countries whose top flights are absent from the dump ──
        (
            "Serbian SuperLiga",
            "Serbia",
            17,
            56,
            64,
            &[
                "Red Star Belgrade",
                "Partizan Belgrade",
                "TSC Backa Topola",
                "Cukaricki",
                "Radnicki Nis",
                "Napredak Krusevac",
            ],
        ),
        (
            "Romanian Liga I",
            "Romania",
            16,
            52,
            60,
            &[
                "FCSB",
                "CFR Cluj",
                "UTA Arad",
                "Universitatea Craiova",
                "Rapid Bucuresti",
                "Hermannstadt",
            ],
        ),
        (
            "Bulgarian First League",
            "Bulgaria",
            15,
            50,
            58,
            &[
                "Ludogorets Razgrad",
                "Levski Sofia",
                "CSKA Sofia",
                "Cherno More",
                "Botev Plovdiv",
                "Arda Kardzhali",
            ],
        ),
        (
            "Bosnian Premier League",
            "Bosnia and Herzegovina",
            15,
            50,
            58,
            &[
                "Zrinjski Mostar",
                "Borac Banja Luka",
                "FK Sarajevo",
                "Zeljeznicar Sarajevo",
                "Velez Mostar",
                "Siroki Brijeg",
            ],
        ),
        (
            "Slovak Superliga",
            "Slovakia",
            15,
            50,
            58,
            &[
                "Slovan Bratislava",
                "Spartak Trnava",
                "DAC Dunajska Streda",
                "Zilina",
                "Ruzomberok",
                "Michalovce",
            ],
        ),
        (
            "Slovenian PrvaLiga",
            "Slovenia",
            14,
            48,
            56,
            &[
                "Olimpija Ljubljana",
                "Maribor",
                "Celje",
                "Koper",
                "Domzale",
                "Mura",
            ],
        ),
        (
            "Icelandic Besta deild",
            "Iceland",
            15,
            50,
            58,
            &[
                "Valur",
                "KR Reykjavik",
                "Vikingur Reykjavik",
                "FH Hafnarfjordur",
                "KA Akureyri",
                "IBV",
            ],
        ),
        (
            "Israeli Premier League",
            "Israel",
            15,
            50,
            58,
            &[
                "Maccabi Tel Aviv",
                "Maccabi Haifa",
                "Hapoel Be'er Sheva",
                "Beitar Jerusalem",
                "Hapoel Haifa",
                "Bnei Sakhnin",
            ],
        ),
        (
            "Georgian Erovnuli Liga",
            "Georgia",
            15,
            50,
            58,
            &[
                "Dinamo Tbilisi",
                "Dinamo Batumi",
                "Torpedo Kutaisi",
                "Dila Gori",
                "Iberia 1999",
                "Samtredia",
            ],
        ),
        (
            "Albanian Kategoria Superiore",
            "Albania",
            14,
            48,
            56,
            &[
                "Partizani Tirana",
                "Dinamo City",
                "Egnatia",
                "Tirana",
                "Vllaznia",
                "Skenderbeu",
            ],
        ),
        (
            "Montenegrin First League",
            "Montenegro",
            13,
            46,
            54,
            &[
                "Buducnost Podgorica",
                "Sutjeska Niksic",
                "Decic",
                "Rudar Pljevlja",
                "Jezero",
                "Iskra Danilovgrad",
            ],
        ),
        (
            "North Macedonian First League",
            "North Macedonia",
            13,
            46,
            54,
            &[
                "Shkendija",
                "Vardar Skopje",
                "Sileks",
                "Pelister",
                "Tikves",
                "Brera Strumica",
            ],
        ),
        (
            "Armenian Premier League",
            "Armenia",
            13,
            46,
            54,
            &[
                "Pyunik",
                "Noah Yerevan",
                "Alashkert",
                "Ararat-Armenia",
                "Urartu",
                "Shirak",
            ],
        ),
        (
            "Azerbaijani Premier League",
            "Azerbaijan",
            14,
            48,
            56,
            &[
                "Qarabag",
                "Neftchi Baku",
                "Zira",
                "Sabah",
                "Gabala",
                "Sumqayit",
            ],
        ),
        (
            "Belarusian Premier League",
            "Belarus",
            13,
            46,
            54,
            &[
                "Dinamo Minsk",
                "BATE Borisov",
                "Isloch",
                "Slavia Mozyr",
                "Energetik",
                "Vitebsk",
            ],
        ),
        (
            "Kazakh Premier League",
            "Kazakhstan",
            13,
            46,
            54,
            &[
                "Astana",
                "Tobol Kostanay",
                "Aktobe",
                "Ordabasy",
                "Kaisar Kyzylorda",
                "Zhetysu",
            ],
        ),
        (
            "Estonian Meistriliiga",
            "Estonia",
            12,
            44,
            52,
            &[
                "Levadia Tallinn",
                "Flora Tallinn",
                "Nomme United",
                "Paide",
                "Trans Narva",
                "Kalev Tallinn",
            ],
        ),
        (
            "Latvian Virsliga",
            "Latvia",
            12,
            44,
            52,
            &["RFS", "Auda", "Tukums 2000", "Liepaja", "Jelgava", "Metta"],
        ),
        (
            "Lithuanian A Lyga",
            "Lithuania",
            13,
            46,
            54,
            &[
                "Zalgiris Vilnius",
                "Suduva",
                "Panevezys",
                "Dainava",
                "Riteriai",
                "Hegelmann",
            ],
        ),
        (
            "Welsh Cymru Premier",
            "Wales",
            13,
            46,
            54,
            &[
                "The New Saints",
                "Bala Town",
                "Connah's Quay Nomads",
                "Briton Ferry",
                "Caernarfon Town",
                "Llanelli Town",
            ],
        ),
        (
            "Northern Irish Premiership",
            "Northern Ireland",
            14,
            48,
            56,
            &[
                "Linfield",
                "Cliftonville",
                "Glentoran",
                "Coleraine",
                "Carrick Rangers",
                "Dungannon Swifts",
            ],
        ),
        (
            "Maltese Premier League",
            "Malta",
            12,
            44,
            52,
            &[
                "Hibernians",
                "Birkirkara",
                "Hamrun Spartans",
                "Floriana",
                "Gzira United",
                "Mosta",
            ],
        ),
        (
            "Faroe Islands Betri Deildin",
            "Faroe Islands",
            12,
            44,
            52,
            &[
                "B36 Torshavn",
                "HB Torshavn",
                "KI Klaksvik",
                "NSI Runavik",
                "EB/Streymur",
                "TB Tvoyro",
            ],
        ),
        (
            "Luxembourg National Division",
            "Luxembourg",
            12,
            44,
            52,
            &[
                "F91 Dudelange",
                "Differdange 03",
                "Progres Niederkorn",
                "Racing FC Union Luxembourg",
                "Jeunesse Esch",
                "Victoria Rosport",
            ],
        ),
        (
            "Moldovan Super Liga",
            "Moldova",
            12,
            44,
            52,
            &[
                "Sheriff Tiraspol",
                "Petrocub",
                "Balti",
                "Spartani",
                "Dacia Buiucani",
                "Zimbru",
            ],
        ),
        (
            "Kosovo Superliga",
            "Kosovo",
            13,
            46,
            54,
            &[
                "Drita Gjilan",
                "Ballkani",
                "Feronikeli 74",
                "Llapi",
                "Dukagjini",
                "GiIani",
            ],
        ),
        // ── CAF: no first-tier leagues exist in the dump at all ──────────
        (
            "Egyptian Premier League",
            "Egypt",
            17,
            56,
            64,
            &[
                "Al Ahly",
                "Zamalek",
                "Pyramids FC",
                "Al Masry",
                "ENPPI",
                "Al Bank Al Ahly",
            ],
        ),
        (
            "South African Premiership",
            "South Africa",
            16,
            54,
            62,
            &[
                "Mamelodi Sundowns",
                "Orlando Pirates",
                "Kaizer Chiefs",
                "Cape Town City",
                "SuperSport United",
                "Stellenbosch FC",
            ],
        ),
        (
            "Moroccan Botola",
            "Morocco",
            16,
            54,
            62,
            &[
                "Wydad Casablanca",
                "Raja Casablanca",
                "AS FAR Rabat",
                "RS Berkane",
                "FUS Rabat",
                "Hassania Agadir",
            ],
        ),
        (
            "Algerian Ligue 1",
            "Algeria",
            16,
            54,
            62,
            &[
                "MC Alger",
                "CR Belouizdad",
                "USM Alger",
                "JS Kabylie",
                "ES Setif",
                "ASO Chlef",
            ],
        ),
        (
            "Tunisian Ligue 1",
            "Tunisia",
            15,
            50,
            58,
            &[
                "Esperance de Tunis",
                "Club Africain",
                "Etoile du Sahel",
                "CS Sfaxien",
                "Stade Tunisien",
                "US Monastir",
            ],
        ),
        (
            "Nigerian Professional Football League",
            "Nigeria",
            15,
            50,
            58,
            &[
                "Enyimba",
                "Remo Stars",
                "Rivers United",
                "Kano Pillars",
                "Shooting Stars",
                "Katsina United",
            ],
        ),
        (
            "Ghana Premier League",
            "Ghana",
            15,
            50,
            58,
            &[
                "Asante Kotoko",
                "Hearts of Oak",
                "Medeama",
                "Dreams FC",
                "Bechem United",
                "Berekum Chelsea",
            ],
        ),
        (
            "Angolan Girabola",
            "Angola",
            14,
            48,
            56,
            &[
                "1 de Agosto",
                "Petro de Luanda",
                "Sagrada Esperanca",
                "CD Huila",
                "Bravos do Maquis",
                "Academica Petroleos",
            ],
        ),
        (
            "Ivorian Ligue 1",
            "Côte d'Ivoire",
            16,
            54,
            62,
            &[
                "ASEC Mimosas",
                "Africa Sports",
                "Stella Club d'Adjame",
                "SO Armee",
                "Bouake FC",
                "Academie Amadou Diallo",
            ],
        ),
        (
            "Tanzanian Mainland Premier League",
            "Tanzania",
            14,
            48,
            56,
            &[
                "Young Africans",
                "Simba SC",
                "Azam FC",
                "Singida Black Stars",
                "Kagera Sugar",
                "Dodoma Jiji",
            ],
        ),
        (
            "Zambian Super League",
            "Zambia",
            13,
            46,
            54,
            &[
                "Zanaco",
                "Power Dynamos",
                "Kabwe Warriors",
                "Green Buffaloes",
                "Nkana",
                "Napsa Stars",
            ],
        ),
        (
            "Zimbabwean Premier Soccer League",
            "Zimbabwe",
            13,
            46,
            54,
            &[
                "Highlanders",
                "CAPS United",
                "Chicken Inn",
                "Dynamos",
                "Harare City",
                "Bulawayo Chiefs",
            ],
        ),
        (
            "Kenyan FKF Premier League",
            "Kenya",
            13,
            46,
            54,
            &[
                "Gor Mahia",
                "AFC Leopards",
                "Tusker",
                "Kenya Police",
                "KCB",
                "Sofapaka",
            ],
        ),
        (
            "Burkinabe Premier League",
            "Burkina Faso",
            13,
            46,
            54,
            &[
                "AS Douanes",
                "Rahimo FC",
                "Salitas",
                "AS Vitesse",
                "ASFB",
                "Royal FC",
            ],
        ),
        (
            "Gabonese Ligue 1",
            "Gabon",
            12,
            44,
            52,
            &[
                "AS Mangasport",
                "CF Mounana",
                "AS Dikaki",
                "105 Libreville",
                "Oyem Athletic",
                "Akanda FC",
            ],
        ),
        // ── CONCACAF: the south and south Caribbean are invisible ────────
        (
            "Costa Rican Primera Division",
            "Costa Rica",
            14,
            48,
            56,
            &[
                "Deportivo Saprissa",
                "LD Alajuelense",
                "CS Herediano",
                "CS Cartagines",
                "AD Guanacasteca",
                "Santos de Guapiles",
            ],
        ),
        (
            "Guatemalan Liga Nacional",
            "Guatemala",
            13,
            46,
            54,
            &[
                "Municipal",
                "Comunicaciones",
                "Coban Imperial",
                "Guastatoya",
                "Xelaju MC",
                "Antigua GFC",
            ],
        ),
        (
            "Honduran Liga Nacional",
            "Honduras",
            13,
            46,
            54,
            &[
                "Olimpia",
                "Motagua",
                "Marathon CD",
                "Real Espana",
                "Honduras Progreso",
                "Vida",
            ],
        ),
        (
            "Panamanian Liga Panamena de Futbol",
            "Panama",
            13,
            46,
            54,
            &[
                "Tauro FC",
                "Plaza Amador",
                "Sporting San Miguelito",
                "Arabe Unido",
                "San Francisco FC",
                "Atletico Chiriqui",
            ],
        ),
        (
            "Salvadoran Primera Division",
            "El Salvador",
            12,
            44,
            52,
            &[
                "Aguila",
                "FAS",
                "Alianza",
                "Once Deportivo",
                "Luis Angel Firpo",
                "Isidro Metapan",
            ],
        ),
        (
            "Jamaican Premier League",
            "Jamaica",
            12,
            44,
            52,
            &[
                "Mount Pleasant FA",
                "Arnett Gardens",
                "Tivoli Gardens",
                "Harbour View",
                "Portmore United",
                "Chapelton Maroons",
            ],
        ),
        // ── More AFC depth for the East ──────────────────────────────────
        (
            "UAE Pro League",
            "United Arab Emirates",
            17,
            56,
            64,
            &[
                "Al Ain",
                "Shabab Al Ahli",
                "Sharjah FC",
                "Al Wasl",
                "Al Nasr",
                "Al Wahda",
            ],
        ),
        (
            "Oman Pro League",
            "Oman",
            14,
            48,
            56,
            &[
                "Al Seeb",
                "Dhofar",
                "Al Nahda",
                "Suwaiq",
                "Oman Club",
                "Al Arabi Oman",
            ],
        ),
        (
            "Philippines Football League",
            "Philippines",
            13,
            46,
            54,
            &[
                "Kaya FC Iloilo",
                "United City FC",
                "Stallion Laguna",
                "Dynamic Herb Cebu",
                "Maharlika Taguig",
                "One Taguig",
            ],
        ),
        // ── OFC's neglected second side ───────────────────────────────────
        (
            "New Caledonia Super Ligue",
            "New Caledonia",
            11,
            42,
            50,
            &[
                "AS Magenta",
                "AS Mont-Dore",
                "Tiga Sport",
                "AS Lossi",
                "AS Wetr",
                "AS Dumbea",
            ],
        ),
    ];

    let mut league_teams: Vec<Vec<usize>> =
        vec![Vec::new(); BIG5.len() + 1 + extra_count + regen_leagues.len()];
    // Every candidate row (generous top-30 per club; dedupe losses follow).
    struct Cand {
        pid: PlayerId,
        ovr: u8,
        age: u8,
        toks: Vec<String>,
    }
    let mut by_first: std::collections::HashMap<(String, u8), Vec<Cand>> =
        std::collections::HashMap::new();
    for ids in league_clubs
        .iter()
        .chain(std::iter::once(&outsider_clubs))
        .chain(extra_groups.iter().map(|(ids, _, _)| ids))
    {
        for cid in ids {
            let Some(c) = db.clubs().iter().find(|c| c.id == *cid) else {
                continue;
            };
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
                let Some(p) = db.players().iter().find(|p| p.id == pid) else {
                    continue;
                };
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
                    .push(Cand {
                        pid,
                        ovr: p.overall(),
                        age: p.age.clamp(16, 40),
                        toks,
                    });
            }
        }
    }
    // Within each first-name group, keep one row per man: best overall, ties to
    // the older age. Distinct players survive side by side.
    let mut winners: std::collections::HashSet<PlayerId> = std::collections::HashSet::new();
    for cands in by_first.values() {
        let mut order: Vec<usize> = (0..cands.len()).collect();
        order.sort_by(|&a, &b| {
            cands[b]
                .ovr
                .cmp(&cands[a].ovr)
                .then(cands[b].age.cmp(&cands[a].age))
        });
        let mut kept: Vec<usize> = Vec::new();
        for &ci in &order {
            let dup = kept.iter().any(|&ki| {
                same_man(
                    &cands[ci].toks,
                    cands[ci].age,
                    &cands[ki].toks,
                    cands[ki].age,
                )
            });
            if !dup {
                kept.push(ci);
                winners.insert(cands[ci].pid);
            }
        }
    }

    // Build groups: the five Big-5 leagues (indices 0..5) plus the real
    // outsider clubs as group 5 (league_idx 5 = "no domestic league here"),
    // then every non-UEFA top flight at indices 6+.
    let mut groups: Vec<(&Vec<ClubId>, usize)> = league_clubs
        .iter()
        .enumerate()
        .map(|(i, ids)| (ids, i))
        .collect();
    groups.push((&outsider_clubs, 5));
    for (k, (ids, _, _)) in extra_groups.iter().enumerate() {
        groups.push((ids, 6 + k));
    }
    for (ids, li) in groups {
        let mut clubs: Vec<&Club> = ids
            .iter()
            .filter_map(|cid| db.clubs().iter().find(|c| c.id == *cid))
            .collect();
        // League order already set above (reputation-sorted); keep it stable.
        for c in clubs {
            let tid = teams.len();
            let mut pidx = Vec::new();
            // Best 26 by overall (see pull_db_squad rationale): a club fields
            // its strongest 26, not whoever the dump lists first.
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
                    let page = p.age.clamp(16, 40);
                    pidx.push(pool.len());
                    pool.push(WP {
                        db_id: Some(p.id),
                        name: p.name.clone(),
                        nation: p.nation.clone(),
                        pos: p.position,
                        attrs: p.attrs,
                        ovr: p.overall(),
                        pot: p.potential.max(p.overall()),
                        age: page,
                        team: tid,
                        retired: false,
                        intl_retired: false,
                        intl_retire_age: 0,
                        club_retire_age: 0,
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
                        ban_games: 0,
                        shots: 0,
                        sot: 0,
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
                                let age_f =
                                    (1.0 - (page.saturating_sub(29)) as f32 * 0.12).max(0.35);
                                let mult = (params.clause_mult
                                    + (o.saturating_sub(82)) as f32 * 0.12)
                                    .min(3.0);
                                Some((v * mult * age_f) as i64)
                            } else {
                                None
                            }
                        },
                        // Wages run ~40% of market value (value × 0.4): wage bills land
                        // near half of revenue like real clubs, so money doesn't
                        // pile to the cap and budgets actually bind.
                        wage: (market_value(p.overall(), page, p.position) * 2 / 5).max(1),
                        career_earnings: 0,
                        signing_bonus: 0,
                        bonus_paid: 0,
                        contract_option: false,
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
                        path: Vec::new(),
                        career_goals: 0,
                        career_assists: 0,
                        career_kp: 0,
                        career_bcc: 0,
                        career_ti: 0,
                        career_cs: 0,
                        career_apps: 0,
                        career_rating_sum: 0.0,
                        career_rating_apps: 0,
                        ballons: 0,
                        peak: 0,
                        peak_str: 1,
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
            let willingness =
                (0.38 + (wealth as f32 / 100.0) * 0.42 + rng.gen_range(0.0..0.12)).clamp(0.3, 0.95);
            let academy = ((c.reputation as i32) / 24 + rng.gen_range(0..=1)).clamp(1, 5) as u8;
            let revenue_last = revenue(wealth) as i64;
            teams.push(Team {
                name: c.name.clone(),
                nation: crate::world_model::lookup(&c.league)
                    .map(|l| l.nation.clone())
                    .filter(|n| valid_nationality(n))
                    .unwrap_or_else(|| majority_nation(&pidx, &pool)),
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
                consec_titles: 0,
                momentum: 0,
            });
        }
    }

    // Build the regen leagues: real club names, procedural squads whose
    // nationals feed the national pools (build_nations needs 15+ active
    // players per side, which Iran/Qatar/Uzbekistan/Jordan/Indonesia/Iraq
    // could never reach from the dump alone).
    {
        // Local uniqueness set across the whole pool so far.
        let mut regen_names: std::collections::HashSet<String> =
            pool.iter().map(|w| w.name.clone()).collect();
        for (k, (label, nation, tier, rep_lo, rep_hi, clubs)) in regen_leagues.iter().enumerate() {
            let li = BIG5.len() + 1 + extra_count + k;
            for (ci, cname) in clubs.iter().enumerate() {
                let tid = teams.len();
                let mut pidx = Vec::new();
                let plan = [
                    (Position::GK, 2u8),
                    (Position::DEF, 6),
                    (Position::MID, 6),
                    (Position::FWD, 4),
                ];
                // Within-league hierarchy: first clubs stronger.
                let club_tier = tier
                    + if ci < 2 {
                        1
                    } else if ci >= clubs.len() - 2 {
                        -1
                    } else {
                        0
                    };
                let mut standouts = 2;
                for (pos, count) in plan {
                    for _ in 0..count {
                        let mut g = Gen::new((rng.gen::<u64>() ^ (tid as u64 * 2654435761)) as u64);
                        let t = if standouts > 0 {
                            standouts -= 1;
                            club_tier + 3
                        } else {
                            club_tier
                        };
                        let gp = g.player(pos, t);
                        let name = Gen::regen_name(rng, nation, &regen_names);
                        regen_names.insert(name.clone());
                        // ~25% intake is youth (16-19): age_cap_ovr already
                        // keeps them off their adult ceiling, which is what
                        // stops the top-11 national-pool selection from
                        // finding a full XI of 30-year-old 90s. The rest are
                        // established adults (23-34) at full strength.
                        let age = if rng.gen_bool(0.25) {
                            rng.gen_range(16..=19) as u8
                        } else {
                            rng.gen_range(23..=34) as u8
                        };
                        let wonder = gp.potential >= 90;
                        let ovr = age_cap_ovr(gp.overall(), age, wonder);
                        pidx.push(pool.len());
                        pool.push(WP {
                            db_id: None,
                            name,
                            nation: (*nation).to_string(),
                            pos,
                            attrs: gp.attrs,
                            ovr,
                            pot: ovr.max(gp.potential).min(94),
                            age,
                            team: tid,
                            retired: false,
                            intl_retired: false,
                            intl_retire_age: 0,
                            club_retire_age: 0,
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
                            ban_games: 0,
                            shots: 0,
                            sot: 0,
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
                            career_earnings: 0,
                            signing_bonus: 0,
                            bonus_paid: 0,
                            contract_option: false,
                            locked: {
                                let h = ((pool.len() as u64).wrapping_mul(2654435761) % 3) as u8;
                                if ovr >= 78 {
                                    2 + h
                                } else {
                                    h
                                }
                            },
                            path: Vec::new(),
                            career_goals: 0,
                            career_assists: 0,
                            career_kp: 0,
                            career_bcc: 0,
                            career_ti: 0,
                            career_cs: 0,
                            career_apps: 0,
                            career_rating_sum: 0.0,
                            career_rating_apps: 0,
                            ballons: 0,
                            peak: 0,
                            peak_str: 1,
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
                let rep = rng.gen_range(*rep_lo..=*rep_hi);
                let strength = match_strength(&pidx, &pool, rep, shape, &params);
                let wealth = rep;
                let willingness =
                    (0.38 + (wealth as f32 / 100.0) * 0.42 + rng.gen_range(0.0..0.12))
                        .clamp(0.3, 0.95);
                let academy = ((rep as i32) / 24 + rng.gen_range(0..=1)).clamp(1, 5) as u8;
                let revenue_last = revenue(wealth) as i64;
                league_teams[li].push(tid);
                teams.push(Team {
                    name: (*cname).to_string(),
                    nation: (*nation).to_string(),
                    league_idx: li,
                    rep,
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
                    heritage: rep,
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
                    consec_titles: 0,
                    momentum: 0,
                });
            }
        }
    }

    // Fallback only: if the database offered no real European outsiders
    // (e.g. a dataset-free run), build the classic eight procedural ones so
    // the UCL field still has its farmers. When real outsider squads loaded
    // above (league_idx 5), this block is skipped entirely.
    if outsider_clubs.is_empty() {
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
                        db_id: None,
                        name: gp.name.clone(),
                        nation: gp.nation.clone(),
                        pos,
                        attrs: gp.attrs,
                        ovr,
                        pot: ovr.max(gp.potential).min(94),
                        age,
                        team: tid,
                        retired: false,
                        intl_retired: false,
                        intl_retire_age: 0,
                        club_retire_age: 0,
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
                        ban_games: 0,
                        shots: 0,
                        sot: 0,
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
                        career_earnings: 0,
                        signing_bonus: 0,
                        bonus_paid: 0,
                        contract_option: false,
                        locked: {
                            let h = ((pool.len() as u64).wrapping_mul(2654435761) % 3) as u8;
                            if ovr >= 78 {
                                2 + h
                            } else {
                                h
                            }
                        },
                        path: Vec::new(),
                        career_goals: 0,
                        career_assists: 0,
                        career_kp: 0,
                        career_bcc: 0,
                        career_ti: 0,
                        career_cs: 0,
                        career_apps: 0,
                        career_rating_sum: 0.0,
                        career_rating_apps: 0,
                        ballons: 0,
                        peak: 0,
                        peak_str: 1,
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
                nation: majority_nation(&pidx, &pool), // procedural fallback outsiders
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
                consec_titles: 0,
                momentum: 0,
            });
        }
    }

    // League metadata (labels + confederations), parallel to league_teams.
    let mut league_meta: Vec<LeagueMeta> = BIG5
        .iter()
        .map(|l| LeagueMeta {
            label: l.to_string(),
            confed: Some(crate::world_model::Confederation::UEFA),
        })
        .collect();
    league_meta.push(LeagueMeta {
        label: "European Outsiders".to_string(),
        confed: Some(crate::world_model::Confederation::UEFA),
    });
    for (_, confed, label) in &extra_groups {
        league_meta.push(LeagueMeta {
            label: label.clone(),
            confed: Some(*confed),
        });
    }
    for (label, nation, _, _, _, _) in regen_leagues.iter() {
        league_meta.push(LeagueMeta {
            label: (*label).to_string(),
            confed: Some(crate::world_model::confed_of(nation)),
        });
    }

    let _ = inject_aditya_prodigy(&mut pool, &mut teams, &league_meta, &params);

    let used_names: std::collections::HashSet<String> =
        pool.iter().map(|w| w.name.clone()).collect();

    // First registration: every club registers its best 25 before a ball is
    // kicked, so the rules hold from season one (no window has run yet).
    for ti in 0..teams.len() {
        let reg = register_squad(&teams[ti].players, &pool);
        teams[ti].registered = reg;
    }

    // A football-culture weighted global pool, not one entry per database row.
    // The old multiset rewarded sheer population/CSV coverage: China and India
    // generated more elite diaspora players than Japan simply because they had
    // more source records. Each real nation now receives a bounded number of
    // tickets based on its current football infrastructure/ranking.
    let mut seen_nations: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut nation_pool: Vec<String> = Vec::new();
    for w in &pool {
        let nation = w.nation.trim();
        if nation.len() < 2
            || !valid_nationality(nation)
            || !seen_nations.insert(nation.to_string())
        {
            continue;
        }
        let tickets = (2.0 + federation_capacity(nation) * 8.0).round() as usize;
        for _ in 0..tickets.max(1) {
            nation_pool.push(nation.to_string());
        }
    }
    // Elite ceiling for regens = the era's ELITE BAND, not a percentile of
    // everyone. The old 96th-percentile-of-all-players reading (~79-80) sat
    // far below the actual world class (85-92): once 2026's stars retired,
    // no regen could replace them, the whole world compressed into a
    // 6-overall band, and twenty equal sides took turns winning World Cups.
    // Anchor instead to the average of the best 50 players on earth (minus a
    // couple of points), floored by the old percentile so a weak dataset
    // still degrades gracefully.
    let mut ovrs: Vec<u8> = pool.iter().map(|w| w.ovr).collect();
    ovrs.sort_unstable();
    let p96 = ovrs
        .get(ovrs.len() * 96 / 100)
        .copied()
        .unwrap_or(33)
        .max(28);
    let top50_sum: u32 = ovrs.iter().rev().take(50).map(|&v| v as u32).sum();
    let top50_avg = (top50_sum as f32 / 50.0_f32.min(ovrs.len() as f32)).round() as u8;
    let elite_cap = p96.max(top50_avg.saturating_sub(2));

    // Opt-in one-shot physical persistence of the initial regen roster.
    // In normal play these players are database-grade individuals in memory,
    // but this switch appends their full CSV rows to `data/players.csv`
    // (marked `simregen`) so the expanded identities survive in the source
    // file. The marker is duplicate-safe: later rebuilds do not grow the CSV
    // indefinitely.
    if std::env::var("FM_PERSIST_REGEN_DB").ok().as_deref() == Some("1") {
        let added = append_initial_regen_players_csv(
            "data/players.csv",
            &pool,
            &teams,
            &league_meta,
            regen_start,
        );
        if added > 0 {
            eprintln!(
                "  Persisted {} generated source players to data/players.csv.",
                added
            );
        }
    }

    World {
        pool,
        minds: Vec::new(), // filled just below, one mind per player
        teams,
        league_teams,
        league_meta,
        nation_pool,
        next_regen: 1,
        elite_cap,
        inflation: 1.0,
        params,
        loans: Vec::new(),
        nemesis: Vec::new(),
        continent_hist: Vec::new(),
        host_queue: vec![vec![
            "United States".to_string(),
            "Canada".to_string(),
            "Mexico".to_string(),
        ]],
        recent_hosts: Vec::new(),
        wc_hist: Vec::new(),
        free_agents: Vec::new(),
        national_states: HashMap::new(),
        used_names,
    }
}

/// Give every pooled player a mind (call once after the initial pool build).
fn seed_minds(w: &mut World, rng: &mut Pcg64Mcg) {
    w.minds = (0..w.pool.len()).map(|_| PlayerMind::new(rng)).collect();
    if let Some(i) = w.pool.iter().position(|p| p.name == "Aditya Singh") {
        w.minds[i].ambition = 0.92;
        w.minds[i].loyalty = 0.55;
        w.minds[i].greed = 0.55;
        w.minds[i].professionalism = 0.95;
        w.minds[i].contract_happiness = 0.72;
    }
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
        p.ban_games = 0;
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
    // World Cup 2026/2030/2034…, Euros 2028/2032…, AFC Asian Cup 2027/2031/2035.
    // The Club World Cup years (2029/2033/2037…) are now also the window
    // for the world's other major continental championships: Copa América,
    // AFCON and the CONCACAF Gold Cup rotate through them, one every four
    // years, so each federation gets its own full international tournament
    // rather than only friendlies.
    if year % 4 == 2 {
        IntlKind::WorldCup
    } else if year % 4 == 0 {
        IntlKind::Euros
    } else if year % 4 == 3 {
        IntlKind::AsianCup
    } else {
        match (year / 4) % 3 {
            0 => IntlKind::CopaAmerica,
            1 => IntlKind::Afcon,
            _ => IntlKind::GoldCup,
        }
    }
}

/// Optional recorder for international fixtures, so interactive consumers
/// (the web season) can surface every international match in the same match
/// list as club football. Callback args: (outcome, home nation, away nation,
/// competition label, stage label).
type IntlSink<'a> = Option<&'a mut dyn FnMut(matchsim::MatchOutcome, &str, &str, &str, &str)>;

/// The summer international block exactly as the batch sandbox plays it: the
/// calendar-driven major tournament (World Cup / Euros / AFC Asian Cup / Copa
/// America / AFCON / Gold Cup), then the four-window friendly calendar with
/// guest nations. Pass a sink to record the fixtures (the web season does).
fn run_summer_internationals(w: &mut World, year: u16, rng: &mut Pcg64Mcg, sink: IntlSink) -> String {
    let winner = summer_major_tournament(w, year, rng, sink);
    friendly_windows(w, rng, &[0, 1, 2, 3], None);
    winner
}

/// The calendar-driven major tournament (World Cup / Euros / continental
/// championship) with hosts, defending-champion drag and state persistence.
fn summer_major_tournament(
    w: &mut World,
    year: u16,
    rng: &mut Pcg64Mcg,
    mut sink: IntlSink,
) -> String {
    // International tournament (calendar-driven). Squads get a summer break
    // first — nobody plays a World Cup at 100 fatigue after league + Europe.
    let intl_kind = intl_kind_for(year);
    // Hosts for this cycle's World Cup: appointed by the latest bid vote and
    // carried in the queue (2026 = USA/Canada/Mexico; after every World Cup,
    // the next host set is decided and queued).
    let wc_hosts: Vec<String> = if intl_kind == IntlKind::WorldCup {
        w.host_queue
            .first()
            .cloned()
            .unwrap_or_else(|| vec!["United States".into(), "Canada".into(), "Mexico".into()])
    } else {
        Vec::new()
    };
    // International tournament (calendar-driven) with hosts boost for WC.
    let intl_winner = match intl_kind {
        IntlKind::None => "-".to_string(),
        kind => {
            for p in w.pool.iter_mut() {
                p.fatigue = p.fatigue.min(25);
            }
            let mut nations = build_nations(&w.pool, kind, &wc_hosts, false, &w.national_states);
            if kind == IntlKind::WorldCup {
                // ── Defending-champion reality check ────────────────────
                // Repeats are real (Brazil '58/'62) but belong to nations
                // with genuine elite DEPTH: a federation that can replace
                // half its XI at the same level. A thin golden generation
                // (one great cohort, no successors) arrives four years
                // older, scouted by everyone, out of celebrations and
                // short rest — and structurally does not go back-to-back.
                // Depth is measured from the pool: how many compatriots
                // play within 3 of the champion's XI level.
                if let Some(champ) = w.wc_hist.last().cloned() {
                    for n in nations.iter_mut() {
                        if n.name != champ {
                            continue;
                        }
                        let xi_level = n.strength as i32;
                        let depth = w
                            .pool
                            .iter()
                            .filter(|p| {
                                !p.retired && p.nation == champ && p.ovr as i32 >= xi_level - 3
                            })
                            .count();
                        // Genuine elite sides (a France/Brazil-class XI) keep
                        // enough of their edge to repeat — history's repeat
                        // winners were exactly that class. A golden cohort
                        // without replacements (outlier champions) decays
                        // hard: four years older, fully scouted, no heirs.
                        let elite_xi = n.strength >= 85.0;
                        let repeat_drag = if depth >= 16 {
                            0.99 // true dynasty depth
                        } else if depth >= 12 {
                            if elite_xi {
                                0.975
                            } else {
                                0.955
                            }
                        } else if elite_xi {
                            0.965
                        } else {
                            0.925 // golden cohort, no replacements
                        };
                        n.edge *= repeat_drag;
                        // Going for a THIRD straight title without elite
                        // depth: even history's best never did that cold.
                        let going_three =
                            w.wc_hist.len() >= 2 && w.wc_hist[w.wc_hist.len() - 2] == champ;
                        if going_three && depth < 16 {
                            n.edge *= 0.97;
                        }
                    }
                }
            }
            let label = match kind {
                IntlKind::WorldCup => "World Cup",
                IntlKind::Euros => "UEFA Euros",
                IntlKind::AsianCup => "AFC Asian Cup",
                IntlKind::CopaAmerica => "Copa América",
                IntlKind::Afcon => "Africa Cup of Nations",
                IntlKind::GoldCup => "Gold Cup",
                IntlKind::None => "Internationals",
            };
            let winner = if kind == IntlKind::WorldCup {
                run_world_cup_2026(year, &mut nations, &wc_hosts, &mut w.pool, &w.params, rng, sink)
            } else {
                run_international(&mut nations, &mut w.pool, &w.params, rng, label, sink)
            };
            // Persist the tournament build-up: chemistry and caps carry into
            // the friendly windows and the next cycle.
            for n in &nations {
                w.national_states.insert(n.name.clone(), n.state.clone());
            }
            if kind == IntlKind::WorldCup {
                if let Some(name) = &winner {
                    if name != "-" {
                        w.wc_hist.push(name.clone());
                        if w.wc_hist.len() > 2 {
                            w.wc_hist.remove(0);
                        }
                    }
                }
            }
            winner.unwrap_or_else(|| "-".to_string())
        }
    };
    // Award the next World Cup's hosting now (bid-appointed), keeping rotation
    // realistic: the current hosts go on the cooldown list before the new bid.
    if intl_kind == IntlKind::WorldCup {
        if !w.host_queue.is_empty() {
            let curr = w.host_queue.remove(0);
            w.recent_hosts.extend(curr);
        }
        if w.recent_hosts.len() > 8 {
            let drop = w.recent_hosts.len() - 8;
            w.recent_hosts.drain(0..drop);
        }
        let next = decide_next_hosts(w, rng);
        w.host_queue.push(next);
    }
    intl_winner
}

/// The four-window friendly calendar with guest nations. `rounds` selects
/// which of the four windows run (the web season splits them across
/// mid-season international breaks; the batch sandbox runs all four).
fn friendly_windows(
    w: &mut World,
    rng: &mut Pcg64Mcg,
    rounds: &[u32],
    mut sink: IntlSink,
) {
    // Friendlies (every year, World Cup included): a broad four-window
    // calendar so the whole world's footballers get real international
    // minutes, caps and goals — the mechanism that actually develops
    // outsider-league players and gives their Ballon scores genuine
    // international volume. Unlike the old one-round top-32 block, this
    // uses every federation with a bare XI (neighbors and low-level sides
    // included) and gives each nation three to five matches: regional
    // derbies, similar-strength games, and an occasional giant-vs-minnow
    // mismatch. Caps and goals land, but no "result" is recorded
    // (friendlies don't move the resume the way majors do).
    {
        let mut nations = build_nations(&w.pool, IntlKind::None, &[], true, &w.national_states);
        if nations.len() >= 2 {
            // Index order is strongest-first (build_nations sorts).
            let n = nations.len();
            // Round 0: similar strength. Round 1: same-confederation,
            // nearest strength. Round 2: shuffled. Round 3: strongest vs
            // weakest (the minnow-vs-giant showcase). Each nation plays at
            // most once per round; the pool drains as matches are played.
            for &round in rounds {
                let mut avail: Vec<usize> = (0..n).collect();
                if round == 2 {
                    avail.shuffle(rng);
                }
                let mut pairs: Vec<(usize, usize)> = Vec::new();
                if round == 3 {
                    // Giant-vs-minnow showcase: strongest vs weakest.
                    for i in 0..(n / 2) {
                        pairs.push((i, n - 1 - i));
                    }
                } else if round == 1 {
                    // Regional window: greedy same-confederation pairing,
                    // nearest strength first; leftovers pair cross-confederation.
                    let mut used = vec![false; n];
                    for &a in &avail {
                        if used[a] {
                            continue;
                        }
                        let ca = crate::world_model::confed_of(&nations[a].name);
                        let mut best: Option<usize> = None;
                        let mut best_gap = f32::INFINITY;
                        for &b in &avail {
                            if b == a || used[b] {
                                continue;
                            }
                            if crate::world_model::confed_of(&nations[b].name) != ca {
                                continue;
                            }
                            let gap = (nations[a].strength - nations[b].strength).abs();
                            if gap < best_gap {
                                best_gap = gap;
                                best = Some(b);
                            }
                        }
                        if let Some(b) = best {
                            used[a] = true;
                            used[b] = true;
                            pairs.push((a, b));
                        }
                    }
                    for &a in &avail {
                        if used[a] {
                            continue;
                        }
                        let mut best: Option<usize> = None;
                        let mut best_gap = f32::INFINITY;
                        for &b in &avail {
                            if b == a || used[b] {
                                continue;
                            }
                            let gap = (nations[a].strength - nations[b].strength).abs();
                            if gap < best_gap {
                                best_gap = gap;
                                best = Some(b);
                            }
                        }
                        if let Some(b) = best {
                            used[a] = true;
                            used[b] = true;
                            pairs.push((a, b));
                        }
                    }
                } else {
                    // Similar-strength window (also used for shuffled round 2,
                    // where `avail` is already shuffled so this is a random
                    // draw against the grain of the strength sort).
                    let mut used = vec![false; n];
                    for &a in &avail {
                        if used[a] {
                            continue;
                        }
                        let mut best: Option<usize> = None;
                        let mut best_gap = f32::INFINITY;
                        for &b in &avail {
                            if b == a || used[b] {
                                continue;
                            }
                            let gap = (nations[a].strength - nations[b].strength).abs();
                            if gap < best_gap {
                                best_gap = gap;
                                best = Some(b);
                            }
                        }
                        if let Some(b) = best {
                            used[a] = true;
                            used[b] = true;
                            pairs.push((a, b));
                        }
                    }
                }
                for (a, b) in pairs {
                    let (_, _, _, outcome) =
                        play_nation(a, b, &nations, &mut w.pool, &w.params, rng);
                    if let Some(f) = sink.as_mut() {
                        f(
                            outcome,
                            &nations[a].name,
                            &nations[b].name,
                            "Friendly",
                            "International window",
                        );
                    }
                    bump_chem(&mut nations, a, b);
                }
                // Drain fatigue between windows so a 4-match year doesn't
                // leave everyone exhausted into the next season.
                for p in w.pool.iter_mut() {
                    if !p.retired {
                        p.fatigue = p.fatigue.saturating_sub(20);
                    }
                }
            }
        }
        // Guest window: federations too thin to field a real side still
        // play — a real nation hosts anonymous regens who may or may not
        // contain a decent player. Credit only ever lands on real players.
        if nations.len() >= 4 {
            use std::collections::HashMap;
            let mut counts: HashMap<String, u32> = HashMap::new();
            for p in w.pool.iter() {
                if !p.retired && !p.intl_retired {
                    *counts.entry(p.nation.clone()).or_default() += 1;
                }
            }
            let mut guest_nations: Vec<String> = Vec::new();
            for n in crate::world_model::nations() {
                let name = n.name.to_string();
                if !nations.iter().any(|x| x.name == name)
                    && counts.get(&name).copied().unwrap_or(0) < 11
                {
                    guest_nations.push(name);
                }
            }
            guest_nations.shuffle(rng);
            for gname in guest_nations.into_iter().take(4) {
                // Host: prefer a same-confederation nation, else anyone.
                let gconf = crate::world_model::confed_of(&gname);
                let host = nations
                    .iter()
                    .position(|x| crate::world_model::confed_of(&x.name) == gconf)
                    .unwrap_or_else(|| rng.gen_range(0..nations.len()));
                let cap = federation_capacity(&gname);
                // Mostly weak journeymen; occasionally a one-off decent side.
                let base = if rng.gen_bool(0.12) {
                    66.0 + cap * 10.0
                } else {
                    48.0 + cap * 16.0
                };
                play_guest_friendly(host, &nations, base, &mut w.pool, &w.params, rng);
                {
                    let st = &mut nations[host].state;
                    st.caps_together = st.caps_together.saturating_add(1);
                    st.chemistry = (st.chemistry + 0.05).min(1.0);
                    st.familiarity = (st.familiarity + 0.05).min(1.0);
                }
                for p in w.pool.iter_mut() {
                    if !p.retired {
                        p.fatigue = p.fatigue.saturating_sub(20);
                    }
                }
            }
        }
        // Federation programme persist: chemistry and caps carry into next
        // season, so settled squads keep their edge.
        for n in &nations {
            w.national_states.insert(n.name.clone(), n.state.clone());
        }
    }
}

fn run_one_season(mut w: &mut World, year: u16, rng: &mut Pcg64Mcg) -> SeasonOutcome {
    reset_season(w);

    // ── Dynasty cycle: the season's emotional starting point ─────────────
    // Consecutive champions return from summer comfortable — comfort is the
    // enemy, and morale (which selection and match strength both use) sags
    // per straight title until someone pips them. Momentum challengers ride
    // the opposite wave. No match is pre-decided; the edge is exactly the
    // size a dressing room's hunger actually is.
    for t in 0..w.teams.len() {
        let consec = w.teams[t].consec_titles;
        if consec >= 2 {
            let sag = 4 * (consec - 1).min(4) as u8; // −4..−16 morale
            for pi in w.teams[t].players.clone() {
                w.pool[pi].morale = w.pool[pi].morale.saturating_sub(sag);
            }
        }
        if w.teams[t].momentum > 0 {
            for pi in w.teams[t].players.clone() {
                if w.pool[pi].morale < 73 {
                    w.pool[pi].morale = 73;
                }
            }
        }
    }

    for li in 0..BIG5.len() {
        let ids = w.league_teams[li].clone();
        if ids.len() >= 4 {
            run_league(
                &ids,
                &mut w.teams,
                &mut w.pool,
                &mut w.minds,
                &w.params,
                rng,
            );
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
            let cname = w.teams[champ].name.clone();
            for &pi in &w.teams[champ].players {
                w.pool[pi].league_won = true;
            }
            champions.push((BIG5[li].to_string(), cname));
        }
        tables.push(table);
    }
    // Non-UEFA top flights (indices 6+): their own domestic league — tables,
    // champions and later a confederation cup. Kept entirely separate from
    // `tables` so UCL/Europa qualification stays Big-5-only, as in reality.
    for li in (BIG5.len() + 1)..w.league_teams.len() {
        let ids = w.league_teams[li].clone();
        if ids.len() >= 4 {
            run_league(
                &ids,
                &mut w.teams,
                &mut w.pool,
                &mut w.minds,
                &w.params,
                rng,
            );
        }
    }
    let mut extra_tables: Vec<Vec<usize>> = Vec::new();
    for li in (BIG5.len() + 1)..w.league_teams.len() {
        let table = rank(&w.league_teams[li], &w.teams);
        if let Some(&champ) = table.first() {
            w.teams[champ].league_titles += 1;
            let cname = w.teams[champ].name.clone();
            for &pi in &w.teams[champ].players {
                w.pool[pi].league_won = true;
            }
            champions.push((w.league_meta[li].label.clone(), cname));
        }
        extra_tables.push(table);
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

    // ── Dynasty bookkeeping ─────────────────────────────────────────────
    // Consecutive-title counters (every domestic league, so the cycle bites
    // Al Hilal and Boca as surely as PSG), plus challenger momentum: decay
    // first, then a young top-four finisher (2nd-4th, avg age ≤ 25.2) banks
    // two seasons of confidence — the force that pries leagues open while
    // the giant's guard drops.
    let mut league_champs: Vec<usize> = champ_idxs.clone();
    for table in &extra_tables {
        if let Some(&c) = table.first() {
            league_champs.push(c);
        }
    }
    for t in 0..w.teams.len() {
        w.teams[t].momentum = w.teams[t].momentum.saturating_sub(1);
        if league_champs.contains(&t) {
            w.teams[t].consec_titles = w.teams[t].consec_titles.saturating_add(1);
        } else {
            w.teams[t].consec_titles = 0;
        }
    }
    for table in &tables {
        for &t in table.iter().take(4).skip(1) {
            let squad: Vec<usize> = w.teams[t]
                .players
                .iter()
                .copied()
                .filter(|&i| !w.pool[i].retired)
                .collect();
            if squad.is_empty() {
                continue;
            }
            let avg_age =
                squad.iter().map(|&i| w.pool[i].age as f32).sum::<f32>() / squad.len() as f32;
            if avg_age <= 25.2 {
                w.teams[t].momentum = 2;
            }
        }
    }

    // Domestic cups (one knockout per league). The winning manager's career
    // record credits it like any other trophy — a cup is a cup.
    let mut cup_winners: Vec<(String, String)> = Vec::new();
    let mut cup_result: Vec<Option<(usize, usize)>> = vec![None; w.league_teams.len()];
    for li in 0..w.league_teams.len() {
        if li == 5 {
            continue; // European Outsiders contest Europe, not a domestic cup
        }
        let ids = w.league_teams[li].clone();
        if ids.len() >= 4 {
            if let Some((win, finalist)) = run_domestic_cup(&ids, &w.teams, &w.params, rng) {
                w.teams[win].manager.trophies += 1;
                cup_winners.push((w.league_meta[li].label.clone(), w.teams[win].name.clone()));
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
        let Some(&champ) = tables[li].first() else {
            continue;
        };
        let Some((cup_w, cup_f)) = cup_result[li] else {
            continue;
        };
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
        w.teams[b]
            .strength
            .partial_cmp(&w.teams[a].strength)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for &t in fifths.iter().take(4) {
        if !ucl.contains(&t) {
            ucl.push(t);
        }
    }
    ucl.truncate(32);
    let ucl_winner_idx = run_cup(&ucl, Comp::Ucl, &mut w.teams, &mut w.pool, &w.params, rng);
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
    let europa_winner = if europa.len() >= 8 {
        run_cup(
            &europa,
            Comp::Europa,
            &mut w.teams,
            &mut w.pool,
            &w.params,
            rng,
        )
        .map(|t| {
            w.teams[t].el_titles += 1;
            w.teams[t].manager.trophies += 1;
            w.teams[t].name.clone()
        })
        .unwrap_or_else(|| "-".to_string())
    } else {
        "-".to_string()
    };

    // Confederation cups for the non-UEFA world: AFC Champions League, the
    // CONCACAF Champions Cup and CONMEBOL Libertadores. The top four finishers
    // of each extra league in that confederation form the field; the winner
    // banks a manager's trophy like any other continental honour.
    let confed_cup = |confed: crate::world_model::Confederation,
                      w: &mut World,
                      rng: &mut Pcg64Mcg|
     -> Option<usize> {
        let mut field: Vec<usize> = Vec::new();
        for (k, table) in extra_tables.iter().enumerate() {
            let li = BIG5.len() + 1 + k;
            if w.league_meta[li].confed != Some(confed) {
                continue;
            }
            for &t in table.iter().take(4) {
                if !field.contains(&t) {
                    field.push(t);
                }
            }
        }
        if field.len() < 4 {
            return None;
        }
        // Confederation cups land in the Europa bucket, not the league: their
        // goals/apps are continental production, not DB-club league stats.
        let win = run_cup(
            &field,
            Comp::Europa,
            &mut w.teams,
            &mut w.pool,
            &w.params,
            rng,
        )?;
        w.teams[win].manager.trophies += 1;
        Some(win)
    };
    let afc_winner_idx = confed_cup(crate::world_model::Confederation::AFC, &mut w, rng);
    let concacaf_winner_idx = confed_cup(crate::world_model::Confederation::CONCACAF, &mut w, rng);
    let conmebol_winner_idx = confed_cup(crate::world_model::Confederation::CONMEBOL, &mut w, rng);
    let afc_winner = afc_winner_idx
        .map(|t| w.teams[t].name.clone())
        .unwrap_or_else(|| "-".to_string());
    let concacaf_winner = concacaf_winner_idx
        .map(|t| w.teams[t].name.clone())
        .unwrap_or_else(|| "-".to_string());
    let conmebol_winner = conmebol_winner_idx
        .map(|t| w.teams[t].name.clone())
        .unwrap_or_else(|| "-".to_string());

    // Register this season's continental champions for CWC qualification.
    if let Some(t) = ucl_winner_idx {
        w.continent_hist.push(("UCL".to_string(), t));
    }
    if let Some(t) = afc_winner_idx {
        w.continent_hist.push(("AFC".to_string(), t));
    }
    if let Some(t) = concacaf_winner_idx {
        w.continent_hist.push(("CONCACAF".to_string(), t));
    }
    if let Some(t) = conmebol_winner_idx {
        w.continent_hist.push(("CONMEBOL".to_string(), t));
    }

    // FIFA Club World Cup (2029, 2033, 2037…): 32 teams — UEFA 12, CONMEBOL 6,
    // AFC 4, CAF 4 (none simulated), CONCACAF 4, OFC 1 + the host. Qualification
    // runs off the 4-year cycle of continental champions (the `continent_hist`
    // winners above) plus a coefficient fill: continental silverware first,
    // then strength, at most two clubs per country unless that country has
    // produced multiple continental champions this cycle.
    fn cwc_quota_field(
        w: &World,
        confed: crate::world_model::Confederation,
        quota: usize,
        tag: &str,
    ) -> Vec<usize> {
        let mut field: Vec<usize> = Vec::new();
        let mut push = |field: &mut Vec<usize>, t: usize| {
            if !field.contains(&t) {
                field.push(t);
            }
        };
        // Continental champions of the cycle — direct qualifiers.
        let winners: Vec<usize> = w
            .continent_hist
            .iter()
            .rev()
            .filter(|(l, _)| l == tag)
            .take(4)
            .map(|(_, t)| *t)
            .collect();
        for &t in &winners {
            push(&mut field, t);
        }
        // Coefficient fill: continental record first, then strength.
        let mut cands: Vec<usize> = (0..w.teams.len())
            .filter(|&t| {
                !field.contains(&t) && w.league_meta[w.teams[t].league_idx].confed == Some(confed)
            })
            .collect();
        cands.sort_by(|&a, &b| {
            let (ta, tb) = (&w.teams[a], &w.teams[b]);
            (tb.ucl_titles + tb.el_titles)
                .cmp(&(ta.ucl_titles + ta.el_titles))
                .then(
                    tb.strength
                        .partial_cmp(&ta.strength)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        });
        for &t in &cands {
            if field.len() >= quota {
                break;
            }
            let nation = &w.teams[t].nation;
            let nation_champs = winners
                .iter()
                .filter(|&&x| &w.teams[x].nation == nation)
                .count();
            let nation_total = field
                .iter()
                .filter(|&&x| &w.teams[x].nation == nation)
                .count();
            // More than two national champions exempt a country; otherwise
            // two clubs per country.
            if nation_champs <= 2 && nation_total >= 2 {
                continue;
            }
            push(&mut field, t);
        }
        field
    }
    let club_world_cup_winner = if year % 4 == 1 {
        let mut field = Vec::new();
        let mut add = |f: &mut Vec<usize>, extra: Vec<usize>| {
            for t in extra {
                if !f.contains(&t) {
                    f.push(t);
                }
            }
        };
        add(
            &mut field,
            cwc_quota_field(&w, crate::world_model::Confederation::UEFA, 12, "UCL"),
        );
        add(
            &mut field,
            cwc_quota_field(
                &w,
                crate::world_model::Confederation::CONMEBOL,
                6,
                "CONMEBOL",
            ),
        );
        add(
            &mut field,
            cwc_quota_field(&w, crate::world_model::Confederation::AFC, 4, "AFC"),
        );
        add(
            &mut field,
            cwc_quota_field(
                &w,
                crate::world_model::Confederation::CONCACAF,
                4,
                "CONCACAF",
            ),
        );
        // OFC: Auckland FC when present (New Zealand's flagship now plays in
        // the A-League block, and would take the Oceania slot by the real rules).
        if let Some(t) = w.teams.iter().position(|t| t.name.starts_with("Auckland")) {
            if !field.contains(&t) {
                field.push(t);
            }
        }
        // Host nation: USA lifts an MLS slot, Mexico a Liga MX one — rotating
        // (2029 USA, 2033 Mexico, 2037 USA… if hosts have enough teams left).
        let host_label = if (year / 4) % 2 == 0 {
            "MLS"
        } else {
            "Liga MX"
        };
        if let Some(li) = w.league_meta.iter().position(|m| m.label == host_label) {
            for (k, table) in extra_tables.iter().enumerate() {
                let li2 = BIG5.len() + 1 + k;
                if li2 == li {
                    if let Some(&t) = table.first() {
                        if !field.contains(&t) {
                            field.push(t);
                        }
                    }
                }
            }
        }
        if field.len() >= 8 {
            run_cup(
                &field,
                Comp::League,
                &mut w.teams,
                &mut w.pool,
                &w.params,
                rng,
            )
            .map(|t| {
                w.teams[t].manager.trophies += 1;
                w.teams[t].name.clone()
            })
            .unwrap_or_else(|| "-".to_string())
        } else {
            "-".to_string()
        }
    } else {
        "-".to_string()
    };

    // Capture this cycle's World Cup hosts BEFORE the international block:
    // the tournament awards the next World Cup on its way out, so reading
    // the queue afterwards would report the future hosts instead.
    let wc_hosts_out: Vec<String> = if intl_kind_for(year) == IntlKind::WorldCup {
        w.host_queue
            .first()
            .cloned()
            .unwrap_or_else(|| vec!["United States".into(), "Canada".into(), "Mexico".into()])
    } else {
        Vec::new()
    };
    let intl_winner = run_summer_internationals(&mut w, year, rng, None);

    // European Golden Shoe / headline Golden Boot: Big Five league goals only.
    // Outlier scorers elsewhere remain visible in all-competitions charts and
    // trigger transfers, but do not displace elite-league scorers in this award.
    let mut boot_idx: Vec<usize> = (0..w.pool.len())
        .filter(|&i| {
            w.pool[i].team < w.teams.len() && w.teams[w.pool[i].team].league_idx < BIG5.len()
        })
        .collect();
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
        .filter(|&i| w.teams[w.pool[i].team].league_idx < BIG5.len())
        .map(|i| {
            let team = &w.teams[w.pool[i].team];
            let lf = league_factor_for(team.league_idx, &w.league_meta);
            (i, ballon_score(&w.pool[i], team.rep, year, lf))
        })
        .collect();
    bd.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    let bwin = bd[0].0;
    w.pool[bwin].ballons += 1;
    let (_, why_parts) = ballon_breakdown(
        &w.pool[bwin],
        w.teams[w.pool[bwin].team].rep,
        year,
        league_factor_for(w.teams[w.pool[bwin].team].league_idx, &w.league_meta),
    );
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
    let manager_moves = post_season_evolution(w, &tables, rng);
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
        afc_winner,
        concacaf_winner,
        conmebol_winner,
        club_world_cup_winner,
        intl_kind: intl_kind_for(year),
        intl_winner,
        wc_hosts: wc_hosts_out,
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
fn ballon_breakdown(
    w: &WP,
    club_rep: u8,
    year: u16,
    league_factor: f32,
) -> (f32, Vec<(&'static str, String, f32)>) {
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
    parts.push((
        "Appearances",
        format!("{} × 0.35 (all comps)", total_apps),
        ap,
    ));
    let rt = (w.avg_rating() - 6.5).max(0.0) * 62.0;
    parts.push((
        "Average rating",
        format!("{:.2} above 6.5 baseline", (w.avg_rating() - 6.5).max(0.0)),
        rt,
    ));
    let lt = if w.league_won { 35.0 } else { 0.0 };
    parts.push((
        "League title",
        if w.league_won {
            "won".to_string()
        } else {
            "no".to_string()
        },
        lt,
    ));
    // Knockout priority: a goal in the Champions League knockouts (and
    // especially a deep run) counts more than a group-stage stat-pad.
    let ucl_stage = match w.ucl_result {
        5 => 1.30,
        4 => 1.25,
        3 => 1.15,
        2 => 1.10,
        1 => 1.05,
        _ => 1.0,
    };
    let ug = w.ucl_goals as f32 * 7.0 * ucl_stage;
    parts.push((
        "UCL goals",
        format!("{} × 7 ×{:.2} stage", w.ucl_goals, ucl_stage),
        ug,
    ));
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
    // Exploitation normalization: international goals only prove it at a
    // major tournament. Friendlies (no tournament run at all — the big
    // stat-padding window against weak or guest sides) count at a heavy
    // discount; goals in a knockout run count at full weight and more.
    let intl_w = match w.intl_result {
        5 => 8.0,
        4 => 7.5,
        3 => 7.0,
        2 => 7.0,
        1 => 6.5,
        _ => 2.0,
    };
    let ig = w.intl_goals as f32 * intl_w;
    parts.push((
        "International goals",
        format!("{} ×{:.1}", w.intl_goals, intl_w),
        ig,
    ));
    let ia = w.intl_assists as f32 * 4.0;
    parts.push((
        "International assists",
        format!("{} × 4", w.intl_assists),
        ia,
    ));
    let ir = match w.intl_result {
        5 => 55.0,
        4 => 28.0,
        3 => 14.0,
        _ => 0.0,
    };
    parts.push((
        "International run",
        run_label(w.intl_result, "tournament"),
        ir,
    ));
    // All-round game, weighted small so it only tips the award to a defender or
    // a holding midfielder in a genuinely monumental season (a Rodri/Modric or
    // a title-winning centre-back), never over a prolific forward in a normal
    // year. Ball-winning: a destroyer's tackles and interceptions.
    let dw = (w.tackles + w.interceptions) as f32 * 0.22;
    parts.push((
        "Ball-winning",
        format!("{} tackles+int × 0.22", w.tackles + w.interceptions),
        dw,
    ));
    // Clean sheets reward the men who keep them — defenders and keepers only.
    let is_def = matches!(w.pos, Position::DEF | Position::GK);
    let cs = if is_def {
        w.clean_sheets as f32 * 3.2
    } else {
        0.0
    };
    parts.push(("Clean sheets", format!("{} × 3.2", w.clean_sheets), cs));
    // Defensive masterclass: a defender or keeper who anchors a title win with a
    // wall of clean sheets gets a one-off historic bump — the rare season a
    // Cannavaro/Maldini-type actually takes the Ballon off the forwards.
    let rock = if is_def && w.league_won && w.clean_sheets >= 18 {
        35.0
    } else {
        0.0
    };
    parts.push((
        "Defensive masterclass",
        if rock > 0.0 {
            format!("title + {} clean sheets", w.clean_sheets)
        } else {
            "no".to_string()
        },
        rock,
    ));
    // Control of the game: completed passes, dribbles and the final ball before
    // the chance — each tiny, so only a true metronome/creator climbs on them.
    let poss =
        w.passes_cmp as f32 * 0.004 + w.dribbles_cmp as f32 * 0.05 + w.key_passes as f32 * 0.3;
    parts.push((
        "Build-up & creation",
        format!(
            "{} passes, {} dribbles, {} key passes",
            w.passes_cmp, w.dribbles_cmp, w.key_passes
        ),
        poss,
    ));
    // Winning everything in one year is historic — a league + Champions League
    // double (the treble's spine) is rewarded over and above the parts, so the
    // heartbeat of a clean-sweep side (a Rodri/Modric year) can take the award.
    let sweep = if w.league_won && w.ucl_result == 5 {
        45.0
    } else {
        0.0
    };
    parts.push((
        "Clean-sweep season",
        if sweep > 0.0 {
            "league + UCL".to_string()
        } else {
            "no".to_string()
        },
        sweep,
    ));
    // Underdog story: carrying a lower-reputation club to a Champions League
    // final or title is the stuff of legend and voters reward it — the
    // counterweight to the big-club media bias below.
    let underdog = if club_rep < 78 && w.ucl_result >= 4 {
        38.0
    } else {
        0.0
    };
    parts.push((
        "Underdog run",
        if underdog > 0.0 {
            format!("deep UCL run at rep {}", club_rep)
        } else {
            "no".to_string()
        },
        underdog,
    ));
    // The Ballon is voted by journalists, not computed — so two human factors
    // sit on top of the on-pitch maths, enough to decide the close calls the
    // way real PR and storylines do, never to crown a nobody.
    // Media & reputation: coverage follows the badge; a superclub star is seen,
    // debated and remembered more than an equal season at a provincial club.
    let media = (club_rep as f32 - 70.0).max(0.0) * 1.1;
    parts.push((
        "Media & reputation",
        format!("club reputation {}", club_rep),
        media,
    ));
    // Narrative swing: recency, storyline and voter whimsy — deterministic per
    // player per year (stable within a run), a bounded boost that reorders only
    // genuinely tight races. This is the subjectivity behind every real-world
    // Ballon controversy, instead of a purely robotic formula.
    let swing = ballon_narrative(&w.name, year);
    parts.push((
        "Narrative & recency",
        "journalists' mood".to_string(),
        swing,
    ));
    // Top-5-league bias: Ballon voters only watch the big leagues week in,
    // week out, so a player in a top-5 league gets full credit while a star in
    // an outsider league is judged at a discount. The multiplier is merciless
    // on ordinary good seasons but a truly dominant year (a 40-goal season)
    // still blasts through it — the rare genuine outsider winner (a Bardghji
    // or an Oh) survives on numbers nobody else can match, not on generosity.
    let base: f32 = parts.iter().map(|(_, _, s)| s).sum();
    if (league_factor - 1.0).abs() > 0.01 {
        let bias = (league_factor - 1.0) * base;
        parts.push((
            "League prestige bias",
            format!("×{:.2} for league", league_factor),
            bias,
        ));
    }
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

/// Ballon d'Or league-prestige multiplier for a player, from the league he's
/// playing in. Callers resolve `league_idx` against `World::league_meta` and
/// pass this into the breakdown. Only the top five leagues count; everyone
/// else scores zero — a 40-goal season in a small league earns headlines
/// and a transfer, not the trophy.
fn league_factor_for(league_idx: usize, _meta: &[LeagueMeta]) -> f32 {
    // Big Five only for the Ballon d'Or: voters judge the award off the
    // top-five leagues (plus Europe's elite competition and internationals).
    // Everyone else scores zero — a 40-goal season in a small league earns
    // headlines and a transfer, not the trophy.
    if league_idx < BIG5.len() {
        1.0
    } else {
        0.0
    }
}

fn ballon_score(w: &WP, club_rep: u8, year: u16, league_factor: f32) -> f32 {
    ballon_breakdown(w, club_rep, year, league_factor).0
}

/// End-of-season reputation drift + manager merry-go-round. Keeps the world
/// from freezing: title winners gain prestige, strugglers lose it, everything
/// mean-reverts, and the best over-achieving manager takes a struggling giant's
/// job — so Barça can slump and recover, Arsenal can have an era, and a great
/// coach, not just a big badge, drives success.
fn post_season_evolution(w: &mut World, tables: &[Vec<usize>], rng: &mut Pcg64Mcg) -> Vec<String> {
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
            if w.teams[faller].rep > w.teams[riser].rep && w.teams[riser].manager.wants_bigger_job()
            {
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
            moves.push(format!(
                "{} sacks {} after a dismal campaign",
                w.teams[t].name, old
            ));
        }
    }
    // Elite expectations: giants are judged on titles, not survival. A rep-88+
    // club finishing outside the top 2 faces the sack even from 3rd — this is
    // what stops a good squad (e.g. Barcelona) rotting for a decade under one
    // mediocre appointment while a rival dominates.
    for table in tables {
        for (finish_pos, &t) in table.iter().enumerate() {
            let elite_rep = w.params.elite_rep.round() as u8;
            let elite_pos = w.params.elite_pos.round().max(1.0) as usize;
            if w.teams[t].rep >= elite_rep
                && finish_pos + 1 > elite_pos
                && rng.gen_bool(w.params.sack_elite_p.clamp(0.0, 1.0) as f64)
            {
                let old = w.teams[t].manager.name.clone();
                w.teams[t].manager = Manager::fresh_hire(rng, &w.params);
                moves.push(format!(
                    "{} sack {} for missing the title race",
                    w.teams[t].name, old
                ));
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
            if stature >= 85 && drought >= 6 {
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
        w.teams[t]
            .manager
            .regress(w.params.q_regress.clamp(0.8, 0.99));
    }
    // Security follows overperformance, computed per league table.
    for table in tables {
        let mut by_rep = table.clone();
        by_rep.sort_by(|&a, &b| w.teams[b].rep.cmp(&w.teams[a].rep));
        for (finish_pos, &t) in table.iter().enumerate() {
            let rep_pos = by_rep.iter().position(|&x| x == t).unwrap_or(finish_pos);
            w.teams[t]
                .manager
                .review_season(rep_pos as i32 - finish_pos as i32);
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
        // Dynastic burnout: a manager four-plus straight titles deep gets
        // poached for a national job, leaves at the top, or the board simply
        // craves a new voice — 30% per season. The fresh hire's inexperience
        // IS the transition dip that ends eras; no artificial nerf needed.
        if w.teams[t].consec_titles >= 4 && w.teams[t].manager.seasons >= 3 && rng.gen_bool(0.30) {
            let old = w.teams[t].manager.name.clone();
            w.teams[t].manager = Manager::fresh_hire(rng, &w.params);
            moves.push(format!(
                "{} and champion {} part ways — the board wants a new voice",
                w.teams[t].name, old
            ));
            continue;
        }
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

fn retire_prob(age: u8, is_gk: bool, ovr: u8, lower_competition: bool) -> f64 {
    if age >= 45 {
        return 1.0; // hard cap — literally nobody plays on at 45
    }
    let base = if is_gk && !lower_competition {
        // Goalkeepers age differently: the position survives on positioning
        // and composure, so the mass lands in the late 30s, with stragglers
        // into the early 40s.
        match age {
            0..=33 => 0.0,
            34 => 0.04,
            35 => 0.08,
            36 => 0.15,
            37 => 0.25,
            38 => 0.32,
            39 => 0.35,
            40 => 0.30,
            41 => 0.15,
            _ => 0.99,
        }
    } else if is_gk {
        // Even in a lower-division cage, goalkeeper reflexes decay slower
        // than sprinting: retain a proper ageing curve, but let the wage and
        // medical-support risk pull exits earlier than elite keepers.
        match age {
            0..=33 => 0.0,
            34 => 0.07,
            35 => 0.13,
            36 => 0.22,
            37 => 0.32,
            38 => 0.38,
            39 => 0.42,
            40 => 0.38,
            41 => 0.22,
            _ => 0.99,
        }
    } else if lower_competition {
        // Minor-club, low-support tenures: the body and the bank pack in
        // earlier. Outfield players are mostly finished in their early/mid
        // -30s, while the GK branch above preserves longer ageing by position.
        match age {
            0..=28 => 0.0,
            29 => 0.03,
            30 => 0.08,
            31 => 0.15,
            32 => 0.24,
            33 => 0.34,
            34 => 0.44,
            35 => 0.55,
            36 => 0.68,
            37 => 0.82,
            38 => 0.92,
            39 => 0.97,
            40 => 0.99,
            _ => 0.99,
        }
    } else if ovr >= 86 {
        // Elites (Messi/Ronaldo class) log the longest careers: apex years
        // at 27-29, then a genuinely elite player can go to 38-41 while a
        // jobbing journeyman is gone by 35-36.
        match age {
            0..=36 => 0.0,
            37 => 0.04,
            38 => 0.08,
            39 => 0.16,
            40 => 0.25,
            41 => 0.30,
            42 => 0.40,
            43 => 0.45,
            _ => 0.99,
        }
    } else {
        // General men's professional outfield population: with a prime
        // window at 26-29, the median exit lands in the late 30s, not
        // the mid-30s turnover earlier builds used.
        match age {
            0..=35 => 0.0,
            36 => 0.15,
            37 => 0.40,
            38 => 0.55,
            39 => 0.45,
            40 => 0.30,
            41 => 0.15,
            _ => 0.99,
        }
    };
    if is_gk && !lower_competition {
        base * 0.85
    } else {
        base
    }
}

/// National-team retirement: international peaks end earlier than club careers.
/// A player calls time on his country when the federation's youth pipeline
/// has passed him — his pace/stamina have dropped, he is below the national
/// call-up threshold, and continuing means saving the coach the awkward
/// decision without any realistic prospect of a new World Cup/Euro cycle.
/// Elite players can still contribute at 34-38; journeymen usually step aside
/// between 30 and 36, then keep playing domestic or exile football.
fn intl_retirement_prob(p: &WP, national_floor: i32) -> f64 {
    if p.age < 30 || p.ovr < 60 {
        return 0.0;
    }
    let exceptional = p.ovr >= 86;
    let below_floor = (p.ovr as i32) < national_floor - 3;
    let physical_drop = if p.pos == Position::GK {
        p.attrs.goalkeeping < 68 || p.attrs.composure < 60
    } else {
        p.attrs.pace < 60 || p.attrs.stamina < 55 || p.attrs.dribbling < 55
    };
    let _ = national_floor;
    let base = match p.age {
        30..=31 => {
            if physical_drop && !exceptional {
                0.10
            } else if below_floor {
                0.06
            } else {
                0.01
            }
        }
        32..=33 => {
            if physical_drop && !exceptional {
                0.22
            } else if below_floor {
                0.15
            } else if exceptional {
                0.03
            } else {
                0.06
            }
        }
        34..=35 => {
            if physical_drop {
                0.38
            } else if below_floor {
                0.30
            } else if exceptional {
                0.08
            } else {
                0.15
            }
        }
        36..=37 => {
            if physical_drop {
                0.55
            } else if below_floor {
                0.45
            } else if exceptional {
                0.20
            } else {
                0.28
            }
        }
        38..=39 => {
            if exceptional {
                0.45
            } else {
                0.70
            }
        }
        _ => {
            if exceptional {
                0.65
            } else {
                0.85
            }
        }
    };
    if p.pos == Position::GK {
        base * 0.65
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

/// Environment-first development ceiling by academy tier (1..=5): the best a
/// player can *become* under a given club's development environment. A 1–2★
/// world cannot mint world-class (90+) athletes — those need academy 4–5
/// plumbing, which in the real game takes decades of investment to reach.
/// >62 at 1★, >75 at 2★, >82 at 3★, >88 at 4★: a youngster caps out around
/// here unless he moves to an environment that can carry him further, or one
/// that has been in the elite tier long enough to matter. The 4→5 gap is
/// deliberately bigger than 1→2: revolutionary change is slow, not a toggle.
fn env_cap(academy: u8) -> u8 {
    match academy {
        1 => 62,
        2 => 75,
        3 => 82,
        4 => 88,
        _ => 95,
    }
}

/// Growth intensity 0..=1 by academy tier. Only an elite (5★) environment
/// gives multipliers near 1.0; weaker environments slow growth so even
/// their talents peak lower and later — a revolutionary leap is a
/// decades-long project, not a two-season makeover.
fn env_mult(academy: u8) -> f32 {
    match academy {
        1 => 0.25,
        2 => 0.45,
        3 => 0.65,
        4 => 0.85,
        _ => 1.0,
    }
}

/// The 90 wall: development past overall 90 is astronomical. Above 90,
/// growth is *earned*, not grown — a player only unlocks progress when that
/// season pairs elite production (20+ goal involvements, or an 8.15+ rating
/// over 25+ apps) with deep success on the big stage (a Champions League or
/// Europa semi-final-or-better, an international tournament semi, or the
/// combo of a league title plus a European knockout run). Without proof of
/// dominance and winning, 90 is a hard wall for everyone, including academy
/// 5 environments — the Ronaldo-cut is the rarest thing in football, not a
/// level on a growth trajectory.
fn proof_to_push_past_90(p: &WP) -> bool {
    let prod = (p.goals + p.assists) >= 20 || (p.apps >= 25 && p.avg_rating() >= 8.15);
    let stage = p.ucl_result >= 3
        || p.el_result >= 3
        || p.intl_result >= 3
        || (p.league_won && p.ucl_result >= 2);
    prod && stage
}

/// Complex-enough annual development: attributes grow toward potential while
/// young, plateau in the prime, and decline with age (pace/stamina first).
/// Generational shape: the "Messi at 39" profile. A late-career prime
/// that defies the decline curve isn't random luck — it belongs to
/// players whose technique, vision, dribbling and composure carry them
/// after the legs go (and to keepers/defenders of a similar elite
/// build). Generic veterans age on schedule; generational outliers
/// occasionally don't.
fn generational_shape(p: &WP) -> bool {
    let a = &p.attrs;
    match p.pos {
        Position::FWD | Position::MID => {
            a.passing >= 85
                && a.vision >= 85
                && a.dribbling >= 85
                && a.composure >= 88
                && a.stamina >= 58
        }
        Position::DEF => {
            a.tackling >= 85 && a.heading >= 85 && a.composure >= 85 && a.passing >= 80
        }
        Position::GK => a.goalkeeping >= 90 && a.composure >= 86,
    }
}

fn develop_player(p: &mut WP, dev_mid_p: f32, youth_boost: f32, academy: u8, rng: &mut Pcg64Mcg) {
    // Attributes live on the 0-100 scale, so a year's growth/decline moves a
    // few points (not one). Profiles change unevenly — pace and stamina fade
    // first while technique, vision and composure linger — so a veteran can
    // reinvent themselves rather than just losing "overall". And minutes
    // develop: a teenager who actually plays (15+ apps) grows faster, doubly
    // so under a manager who trusts youth — the academy-to-first-team path.
    let bump = |v: &mut u8, d: i32| {
        *v = (*v as i32 + d).clamp(1, 99) as u8;
    };
    // Peak window: while it lasts, the player's key weapons all inch up
    // together (peak_str 1..=2 attribute points a season). This is the
    // 17/18 Salah year written into the aging model — a real, temporary,
    // per-player surge, not a permanent rating slot change.
    if p.peak > 0 {
        match p.pos {
            Position::FWD => {
                bump(&mut p.attrs.shooting, p.peak_str as i32 / 2 + 1);
                bump(&mut p.attrs.dribbling, 1);
                bump(&mut p.attrs.composure, 1);
            }
            Position::MID => {
                bump(&mut p.attrs.passing, p.peak_str as i32 / 2 + 1);
                bump(&mut p.attrs.vision, 1);
                bump(&mut p.attrs.dribbling, 1);
            }
            Position::DEF => {
                bump(&mut p.attrs.tackling, p.peak_str as i32 / 2 + 1);
                bump(&mut p.attrs.heading, 1);
                bump(&mut p.attrs.composure, 1);
            }
            Position::GK => {
                bump(&mut p.attrs.goalkeeping, 1);
                bump(&mut p.attrs.composure, 1);
            }
        }
    }
    // Growth slows near human limits (see grow_attr): even generational
    // talents stall around 92-94 in their best attributes (Messi/Ronaldo
    // peaked at 94 overall with real weaknesses elsewhere), instead of
    // strolling to wall-to-wall 99s. Decline still uses raw bump below.
    if p.age <= 21 {
        // Fast growth years, biased to the player's natural strengths. Minutes
        // develop — and international caps count as minutes too, so a teenager
        // breaking into his national team grows from that experience, not only
        // from club games.
        // Environment pressure (academy 1–5): raw growth emerges as steps, but
        // only a fraction landed by this environment's pace. Under a ceiling,
        // growth nearly stops — a player can only outgrow the environment by
        // *leaving* it (transferring up), never just by grinding.
        let minutes = p.apps + p.intl_apps;
        let mult = env_mult(academy);
        let cap = env_cap(academy);
        let mut steps = rng.gen_range(1..=3);
        if minutes >= 15 && rng.gen_bool(0.35) {
            steps += 1; // playing develops
        }
        if youth_boost > 1.05 && minutes >= 15 && rng.gen_bool(0.5) {
            steps += 1; // …especially for a manager who trusts kids
        }
        if p.ovr >= cap {
            // At the environment's ceiling: nearly imperceptible growth only.
            if !rng.gen_bool(0.15) {
                steps = 0;
            } else {
                steps = 1;
            }
        } else {
            steps = ((steps as f32 * mult).round() as u32).max(1);
        }
        // The 90 wall: above 90 every growth step must be re-earned — real
        // production AND deep knockout/championship success that fresh season.
        // Without proof, the wall is absolute even in a 5★ academy; with proof,
        // at most one step per season (astronomical grind, ~90→94 takes four
        // proven seasons, if luck and the pot cooperate).
        if p.ovr >= 90 {
            if proof_to_push_past_90(p) {
                steps = steps.min(1);
            } else {
                steps = 0;
            }
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
    } else if p.age <= 28 && rng.gen_bool(dev_mid_p.clamp(0.0, 1.0) as f64) {
        // Late-twenties growth is slower, still environment-bound: who develops
        // and how far may approach the ceiling, but never pass it — the academy
        // wall holds there too. And the 90 wall holds harder: even in a 5★
        // academy, the roll above 90 only grows (one step) when the player
        // proved it that season.
        if p.ovr < env_cap(academy) {
            if p.ovr >= 90 {
                // Above 90: same proof standard as the 21-and-under phase.
                if proof_to_push_past_90(p) {
                    // once past the proof test, one modest step
                    match p.pos {
                        Position::FWD => grow_attr(&mut p.attrs.shooting, 3, rng),
                        Position::MID => grow_attr(&mut p.attrs.passing, 3, rng),
                        Position::DEF => grow_attr(&mut p.attrs.tackling, 3, rng),
                        Position::GK => grow_attr(&mut p.attrs.goalkeeping, 3, rng),
                    }
                }
            } else {
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
            }
        }
    } else if p.age >= 31 {
        // Decline starts in the early 30s and builds gradually; technique
        // fades far slower, so an ageing forward can lose pace yet keep
        // finishing, vision, composure. Keepers decay latest: reflexes
        // (goalkeeping) from 34, command (composure) from 37.
        let sev = 2 + (p.age.saturating_sub(31)) as i32; // ~2..12 points a year
        bump(&mut p.attrs.pace, -rng.gen_range(1..=sev + 2));
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
    // Federation development grants (by nation, not club): a strong youth
    // floor lifts the whole nation's academy output a tier — the JFA effect,
    // letting disciplined mid-sized footballing nations sustain depth.
    let fed_bonus: Vec<f32> = {
        let mut nations: Vec<String> = Vec::new();
        for t in w.teams.iter() {
            if !nations.contains(&t.nation) {
                nations.push(t.nation.clone());
            }
        }
        let mut out = Vec::new();
        for t in w.teams.iter() {
            let i = nations.iter().position(|n| n == &t.nation).unwrap_or(0);
            out.push(federation_bonus(w, &nations[i]));
        }
        out
    };
    let revs: Vec<i64> = w.teams.iter().map(|t| t.revenue_last).collect();
    let wage_bills: Vec<i64> = (0..w.teams.len()).map(|ti| wage_bill(w, ti)).collect();
    // National call-up floors for international retirement: the 11th-best
    // non-retired player of each federation. The point is not merely age;
    // a veteran who is suddenly below his federation's replacement class is
    // the first man a coach stops trusting.
    let national_thresholds: HashMap<String, i32> = {
        let mut pools: HashMap<String, Vec<i32>> = HashMap::new();
        for p in &w.pool {
            if !p.retired && !p.intl_retired && p.nation.trim().len() >= 2 {
                pools
                    .entry(p.nation.clone())
                    .or_default()
                    .push(p.ovr as i32);
            }
        }
        pools
            .into_iter()
            .map(|(nation, mut vals)| {
                vals.sort_unstable_by(|a, b| b.cmp(a));
                let floor = vals
                    .get(10)
                    .copied()
                    .or_else(|| vals.last().copied())
                    .unwrap_or(50);
                (nation, floor)
            })
            .collect()
    };
    for (idx, p) in w.pool.iter_mut().enumerate() {
        if p.retired {
            continue;
        }
        p.age = p.age.saturating_add(1);
        // ── Peak window ──────────────────────────────────────────────────
        // True top players don't merely plateau: they have a genuine
        // "peak-form" stretch where their key weapons all click at once.
        // Elites (88+) 24-31 enter on a professionalism-weighted roll;
        // ultra-disciplined ones sometimes ride a second year. A rare
        // "Indian summer" keeps a 33+ legend (the Messi-at-39 class) in
        // an anomalous prime year instead of instant decline.
        if p.peak > 0 {
            p.peak = p.peak.saturating_sub(1);
            if p.peak == 0 && w.minds[idx].professionalism > 0.7 && rng.gen_bool(0.22) {
                // Extension: disciplined peaks sometimes run a second year.
                p.peak = 1;
            }
        } else if p.ovr >= 85 && w.minds[idx].professionalism > 0.55 {
            let roll = if p.age >= 24 && p.age <= 31 {
                if p.ovr >= 92 {
                    0.07
                } else {
                    0.045
                }
            } else if p.age >= 33 && p.age <= 41 && generational_shape(p) {
                // Generational profile only: a late prime is a technique
                // phenomenon (Messi's 90 passing/94 touch/89 vision), not a
                // random veteran's. 10% a year in the window.
                0.10
            } else {
                0.0
            };
            if roll > 0.0 && rng.gen_bool(roll) {
                p.peak = if p.ovr >= 92 { rng.gen_range(2..=3) } else { 1 };
                p.peak_str = if rng.gen_bool(0.5) { 2 } else { 1 };
            }
        }
        let yb = boosts.get(p.team).copied().unwrap_or(1.0);
        let ac = w.teams.get(p.team).map(|t| t.academy).unwrap_or(3);
        // Federation grant lifts effective academy quality for development:
        // a strong youth floor raises the environment's pace and ceiling.
        let fb = fed_bonus.get(p.team).copied().unwrap_or(0.0);
        let ac_eff = ((ac as f32 + fb).floor() as u8).clamp(1, 5);
        develop_player(p, dev_mid_p, yb, ac_eff, rng);
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
        if p.ovr >= 82 || p.goals >= 30 {
            let v = market_value(p.ovr, p.age, p.pos) as f32 * w.inflation;
            // Clauses decay with age past 29: nobody holds a €138m clause on a
            // 31-year-old keeper — veteran clauses price the declining asset.
            // A monster season also re-prices the clause: a 35-40-goal number
            // in a small league is a signal the market must already price, not
            // a surprise to renegotiate from zero.
            let age_f = (1.0 - (p.age.saturating_sub(29)) as f32 * 0.12).max(0.35);
            let mult = (w.params.clause_mult + (p.ovr.saturating_sub(82)) as f32 * 0.12).min(3.0);
            // A signed clause is fixed for the life of the deal. Reprice it
            // only at renewal/final year or when the contract had no clause.
            if p.release_clause.is_none() || p.locked <= 1 {
                p.release_clause = Some((v * mult * age_f) as i64);
            }
        } else {
            p.release_clause = None;
        }
        // Contract decision -- clubs exercise options, then negotiate in earnest.
        // The club may use a one-year option if it has one. Otherwise the club
        // puts a real number on the table; the player evaluates money plus role
        // and stage, and either signs (raising morale) or leaves the table angry.
        if !p.on_loan {
            if p.contract_option && p.locked <= 1 && rng.gen_bool(0.85) {
                p.contract_option = false;
                p.locked = 2;
                p.morale = p.morale.saturating_add(2).min(85);
            } else if p.locked <= 1 && p.ovr >= 70 {
                let team = &w.teams[p.team];
                let bill = wage_bills.get(p.team).copied().unwrap_or(0);
                let offer = build_contract_offer(p, &w.minds[idx], team, &w.params, bill, rng);
                if offer.accepted {
                    p.wage = offer.wage;
                    p.signing_bonus += offer.signing_bonus;
                    p.career_earnings += offer.signing_bonus;
                    let team_i = w.teams.get_mut(p.team).unwrap();
                    team_i.cash -= offer.signing_bonus;
                    team_i.net_spend += offer.signing_bonus;
                    p.locked = offer.contract_years;
                    p.contract_option = offer.club_option;
                    let m = &mut w.minds[idx];
                    m.unhappy = false;
                    m.requested = false;
                    m.contract_happiness = offer.contract_happiness;
                    p.morale = p.morale.saturating_add(3).min(88);
                } else {
                    let m = &mut w.minds[idx];
                    m.unhappy = true;
                    m.contract_happiness = offer.contract_happiness;
                    if m.loyalty < 0.65 || rng.gen_bool(0.45) {
                        m.requested = true;
                    }
                    // Veteran holdout: an elite past prime who has turned
                    // down the money will run the oath down and walk on a
                    // free transfer, especially if he's ambitious or greedy.
                    // This is the classic Bosman ending to a club legend's
                    // tenure — sometimes mercernary, often inevitable.
                    if p.age >= 29
                        && p.ovr >= 85
                        && (m.ambition > 0.7 || m.greed > 0.7)
                        && rng.gen_bool(0.35)
                    {
                        m.requested = true;
                        m.contract_happiness = m.contract_happiness.min(0.35);
                    }
                }
            }
        }
        // Small-league dynamism: a one-season avalanche is precisely what big
        // clubs and players notice. It must not settle into a permanent
        // Ballon narrative in a weak league, but it absolutely earns the
        // player an exit conversation. This is the purchase path (Napoli sees
        // a 40-goal number elsewhere; the statement is already quoted).
        if !p.on_loan
            && p.team < w.teams.len()
            && w.teams[p.team].league_idx >= BIG5.len()
            && p.goals >= 35
            && (p.ovr >= 80 || w.minds[idx].ambition > 0.45)
        {
            w.minds[idx].requested = true;
            w.minds[idx].unhappy = true;
            w.minds[idx].contract_happiness = w.minds[idx].contract_happiness.min(0.45);
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
        // Payday restlessness: a greedy elite past 29 at a settled club
        // pushes for a money move even mid-dynasty — the mercenary impulse
        // that gives legends multi-club careers instead of one permanent
        // home.
        if p.age >= 29
            && p.ovr >= 88
            && !w.minds[idx].requested
            && (w.minds[idx].greed > 0.75 || w.minds[idx].ambition > 0.85)
            && rng.gen_bool(0.06)
        {
            w.minds[idx].requested = true;
            w.minds[idx].unhappy = true;
            w.minds[idx].contract_happiness = w.minds[idx].contract_happiness.min(0.45);
        }
        let is_gk = p.pos == Position::GK;
        if !p.intl_retired {
            let national_floor = national_thresholds
                .get(&p.nation)
                .copied()
                .unwrap_or(p.ovr as i32);
            if rng.gen_bool(intl_retirement_prob(p, national_floor)) {
                p.intl_retired = true;
                p.intl_retire_age = p.age;
            }
        }
        let lower_competition = w
            .teams
            .get(p.team)
            .map(|t| t.league_idx >= 6 && t.rep <= 62)
            .unwrap_or(false);
        let wealthy_exile = w
            .teams
            .get(p.team)
            .map(|t| t.league_idx >= 6 && !lower_competition && t.revenue_last >= 200)
            .unwrap_or(false)
            && p.intl_retired
            && p.ovr >= 70;
        let raw_retire_prob = retire_prob(p.age, is_gk, p.ovr, lower_competition);
        let club_retire_prob = if p.age >= 45 {
            1.0
        } else if wealthy_exile {
            raw_retire_prob * 0.75
        } else {
            raw_retire_prob
        };
        if rng.gen_bool(club_retire_prob) {
            p.retired = true;
            p.club_retire_age = p.age;
        }
        // ── The last payday: veterans cash in abroad ───────────────────
        // A 33-year-old star too expensive for his wage band doesn't retire
        // into thin air — he requests a move and the money leaks (Saudi
        // Pro League, MLS, Qatar, the classic plane tickets) take him for
        // 2-4 more seasons, earning double money against weaker defenders,
        // padding the numbers he'll be remembered by. Stars who can still
        // command top wages instead stay and retire inside the big five.
        if !p.retired
            && p.age >= 33
            && p.ovr >= if p.intl_retired { 70 } else { 78 }
            && p.team < w.teams.len()
        {
            let leak_exists = (0..w.teams.len()).any(|ti| {
                ti != p.team
                    && w.teams[ti].rep >= 40
                    && w.teams[ti].rep <= 70
                    && w.teams[ti].league_idx >= 6
            });
            if leak_exists {
                // Wage squeeze = the Saudi trigger: if the club can't or
                // won't carry his wage, a wealthier league recruits him.
                // Once he has left international football, that country career
                // is closed anyway, so the money-and-easier-competition move
                // becomes especially attractive.
                let wage_q = w.teams[p.team].rep >= 88
                    && (p.wage as f32) > (w.teams[p.team].revenue_last as f32) * 0.02;
                let mercenary = w.minds[idx].ambition > 0.7;
                let post_intl_payday =
                    p.intl_retired && (wage_q || mercenary || rng.gen_bool(0.32));
                if wage_q || mercenary || post_intl_payday {
                    // Find the richest leak league that fits.
                    let mut dest: Option<usize> = None;
                    let mut best_rev = 0i64;
                    for ti in 0..w.teams.len() {
                        if ti == p.team {
                            continue;
                        }
                        let t = &w.teams[ti];
                        if t.rep < 40 || t.rep > 70 || t.league_idx < 6 {
                            continue;
                        }
                        if t.revenue_last > best_rev {
                            // Wages must beat decline: Saudi pays well.
                            let ok = (p.wage as f32) * 0.9
                                <= wage_cap(t.revenue_last, &w.params) as f32 * 0.9;
                            if ok {
                                best_rev = t.revenue_last;
                                dest = Some(ti);
                            }
                        }
                    }
                    if let Some(d) = dest {
                        let old = p.team;
                        w.teams[old].players.retain(|&x| x != idx);
                        w.teams[d].players.push(idx);
                        p.team = d;
                        p.morale = 78; // sunny, paid, unmarked
                        p.locked = (rng.gen::<u8>() % 3) + 2;
                    }
                }
            }
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
        let deficit = 20i32 - w.teams[ti].players.len() as i32;
        let rep = w.teams[ti].rep;
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
    // The vanishingly rare one-academy-lucky-child: ~3% of the time an elite
    // academy (rep>=88, academy 5) mints a wonderkid destined for the
    // world-class band whatever his origin, so the world's top XI can never
    // sag below the 80s.
    let club_nation = w.teams.get(team).map(|t| t.nation.as_str()).unwrap_or("");
    let elite_trickle = rep >= 88
        && w.teams.get(team).map(|t| t.academy >= 5).unwrap_or(false)
        && federation_capacity(club_nation) >= 0.70
        && rng.gen_bool(0.03);
    let ovr_raw = age_cap_ovr(gp.overall().min(w.elite_cap.saturating_sub(2)), age, wonder);
    let talent_ceiling = if wonder {
        w.elite_cap
    } else {
        (w.elite_cap as i32 - rng.gen_range(10..=30)).max(46) as u8
    };
    // Environment wall: a 1–2★ world cannot mint world-class ceilings. A
    // wonderkid at a weak club tops out at that club's academy wall (he's his
    // country's future star, not the next Messi); only elite environments roll
    // elite ceilings. An ordinary prospect peaks even lower still.
    // Academy products are mostly LOCAL: a boy entering Barcelona's academy
    // is overwhelmingly Spanish; only truly global academies (Salzburg,
    // Benfica, the big clubs' African pipelines) draw broadly. Drawing purely
    // from the global pool sprinkled every nation's talent uniformly across
    // all academies — volume nations minted golden cohorts abroad while the
    // traditional powers quietly stopped regenerating. 60% local, 40% the
    // old global draw.
    let nation = if elite_trickle {
        // The one-in-a-hundred-years find: a once-in-a-generation talent
        // any academy could produce. Keep him local to the academy then.
        w.teams
            .get(team)
            .map(|t| t.nation.clone())
            .unwrap_or_default()
    } else {
        match w.teams.get(team) {
            Some(t) if !t.nation.trim().is_empty() && rng.gen_bool(0.60) => t.nation.clone(),
            _ if !w.nation_pool.is_empty() => {
                w.nation_pool[rng.gen_range(0..w.nation_pool.len())].clone()
            }
            _ => w
                .teams
                .get(team)
                .map(|t| t.nation.clone())
                .unwrap_or_default(),
        }
    };
    // National infrastructure limits the ordinary ceiling, not the existence
    // of individual miracles. A very rare exceptional player may escape the
    // federation cap by moving into an elite club environment, but population
    // volume alone cannot create an entire 80-rated national side.
    let capacity = federation_capacity(&nation);
    // Federations with established export pathways provide a minimum viable
    // development environment even if their domestic league is absent from
    // this dataset (Japan is the key case). This supplies opportunity rather
    // than free ability: the player still needs minutes, coaching and transfers.
    let pathway_floor = if capacity >= 0.95 {
        82
    } else if capacity >= 0.85 {
        80
    } else if capacity >= 0.70 {
        77
    } else if capacity >= 0.45 {
        73
    } else {
        68
    };
    let environment_ceiling = env_cap(academy as u8).max(pathway_floor);
    let raw_pot = if elite_trickle {
        ovr_raw.max(w.elite_cap.saturating_sub(2))
    } else {
        ovr_raw.max(talent_ceiling.min(environment_ceiling).min(w.elite_cap))
    };
    let exceptional = wonder && rng.gen_bool((0.005 + capacity * 0.015) as f64);
    let pot = if exceptional {
        raw_pot
    } else {
        raw_pot.min(federation_player_cap(&nation, w.elite_cap))
    };
    let ovr = ovr_raw.min(pot);
    // Distinct identity from an inexhaustible, nation-aware supply: no bounded
    // retries, no recycled names even a century out.
    let name = Gen::regen_name(rng, &nation, &w.used_names);
    w.used_names.insert(name.clone());
    let idx = w.pool.len();
    w.pool.push(WP {
        db_id: None,
        name,
        nation,
        pos,
        attrs: gp.attrs,
        ovr,
        pot,
        age,
        team,
        retired: false,
        intl_retired: false,
        intl_retire_age: 0,
        club_retire_age: 0,
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
        ban_games: 0,
        shots: 0,
        sot: 0,
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
        career_earnings: 0,
        signing_bonus: 0,
        bonus_paid: 0,
        contract_option: false,
        locked: 1,
        path: Vec::new(),
        career_goals: 0,
        career_assists: 0,
        career_kp: 0,
        career_bcc: 0,
        career_ti: 0,
        career_cs: 0,
        career_apps: 0,
        career_rating_sum: 0.0,
        career_rating_apps: 0,
        ballons: 0,
        peak: 0,
        peak_str: 1,
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
    w.loans.push(SeasonLoan {
        player: pi,
        home: parent,
        away,
    });
    format!(
        "{} ({}, {}) -> {} (loan)",
        w.pool[pi].name,
        w.pool[pi].age,
        w.pool[pi].pos.short(),
        w.teams[away].name
    )
}

fn record_nemesis(
    w: &mut World,
    actor_kind: &'static str,
    actor_id: usize,
    target_kind: &'static str,
    target_id: usize,
    tension: f32,
    story: String,
) {
    let tension = tension.clamp(0.0, 1.0);
    if let Some(r) = w.nemesis.iter_mut().find(|r| {
        r.actor_kind == actor_kind
            && r.actor_id == actor_id
            && r.target_kind == target_kind
            && r.target_id == target_id
    }) {
        r.tension = (r.tension + tension).clamp(0.0, 1.0);
        r.memory = r.memory.saturating_add(1);
        r.story = story;
    } else {
        w.nemesis.push(NemesisRel {
            actor_kind,
            actor_id,
            target_kind,
            target_id,
            tension,
            memory: 1,
            story,
        });
    }
}

fn nemesis_between(
    w: &World,
    actor_kind: &str,
    actor_id: usize,
    target_kind: &str,
    target_id: usize,
) -> f32 {
    w.nemesis
        .iter()
        .filter(|r| {
            r.actor_kind == actor_kind
                && r.actor_id == actor_id
                && r.target_kind == target_kind
                && r.target_id == target_id
        })
        .map(|r| r.tension)
        .fold(0.0f32, f32::max)
}

/// Bilateral willingness: may this player move to this buyer? Remaining
/// contract years are a negotiating position, not an immovable lock: with a
/// credible offer from a bigger club, a player who is open to moving up can
/// leave via a normal transfer, a triggered clause, or a negotiated
/// mutual termination/buyout of the remaining deal. Surplus men go anywhere
/// within 15 rep points (ambitious ones refuse big steps down); settled
/// starters need a clearly bigger club, a triggered clause, or a transfer
/// request (which works laterally like a soft clause). One predicate for
/// buyer- and seller-initiated paths alike, so neither can bypass it.
fn transfer_willing(
    surplus: bool,
    clause_deal: bool,
    requested: bool,
    ambition: f32,
    loyalty: f32,
    buyer_rep: u8,
    seller_rep: u8,
    gap_clause: f32,
    gap_starter: f32,
    locked: u8,
    ovr: u8,
    contract_happiness: f32,
) -> bool {
    let desperate = requested || contract_happiness < 0.35;
    // A release clause is the cleanest way past a long committed deal.
    if clause_deal {
        return buyer_rep as f32 + gap_clause >= seller_rep as f32;
    }
    let open_to_move_up = desperate || (ambition >= 0.55 && loyalty <= 0.80);
    let move_up = buyer_rep as f32 >= seller_rep as f32 + gap_starter;
    // Contract years do not freeze a player. A bigger club's offer can be
    // accepted when the player is genuinely open to moving up; the remaining
    // deal is settled as part of the move. Downward or lateral desertion
    // still requires a request/desperation.
    if locked > 1 {
        // A dissatisfied superstar will cash in even slightly laterally:
        // fellow giants pay the money, money clears the clause.
        if desperate && ovr >= 85 && contract_happiness < 0.5 {
            return (buyer_rep as f32) + 10.0 >= seller_rep as f32 || (move_up && open_to_move_up);
        }
        return (desperate && buyer_rep as f32 + gap_clause >= seller_rep as f32)
            || (move_up && open_to_move_up);
    }
    // Final-year elite control: a world-class player does not casually leave
    // the top flight for a smaller club without a request, a clause, or a
    // genuine move-up offer he is open to.
    if ovr >= 85 && locked > 0 && !desperate {
        return move_up && open_to_move_up;
    }
    if ovr >= 85 && buyer_rep < seller_rep.saturating_sub(4) && contract_happiness > 0.45 {
        return false;
    }
    if surplus {
        (buyer_rep as i32) + 15 >= seller_rep as i32
            && !(buyer_rep as i32 + 12 < seller_rep as i32 && ambition > 0.6)
    } else if desperate {
        buyer_rep as f32 + gap_clause >= seller_rep as f32
    } else {
        buyer_rep as f32 >= seller_rep as f32 + gap_starter
    }
}

/// Resolve club-to-club fee negotiation. A release clause is not an asking
/// price that can be bargained down: triggering it requires payment in full.
fn agreed_transfer_fee(
    ask: i64,
    buyer_budget: i64,
    seller_threshold: f32,
    opening_pct: f32,
    clause_deal: bool,
) -> Option<i64> {
    if clause_deal {
        return (buyer_budget >= ask).then_some(ask);
    }
    let opening = (ask as f32 * opening_pct) as i64;
    if opening as f32 >= ask as f32 * seller_threshold {
        Some(opening)
    } else if buyer_budget >= ask {
        Some(ask)
    } else {
        None
    }
}

/// Season-long loans end here: every loanee returns to the parent before the
/// window opens, so squads (and budgets) are assessed honestly.
fn return_loans(w: &mut World) -> Vec<SeasonLoan> {
    if w.loans.is_empty() {
        return Vec::new();
    }
    let returning = std::mem::take(&mut w.loans);
    for loan in &returning {
        let pi = loan.player;
        w.teams[loan.away].players.retain(|&x| x != pi);
        if !w.pool[pi].retired && !w.teams[loan.home].players.contains(&pi) {
            w.teams[loan.home].players.push(pi);
        }
        w.pool[pi].team = loan.home;
        w.pool[pi].on_loan = false;
    }
    returning
}

/// Post-loan futures, resolved once budgets exist: a youngster who starred on
/// loan either moves to the loanee permanently (loan-to-buy — the fee the
/// small club could never refuse for a player it developed) or returns to
/// fight for his parent's XI. Anyone who has grown into the parent's own
/// first eleven is recalled and stays — no re-loan. Everyone else simply
/// comes home and the window's loan-out pass may send him out again.
fn resolve_loan_futures(
    w: &mut World,
    loans: &[SeasonLoan],
    moved: &mut std::collections::HashSet<usize>,
    rng: &mut Pcg64Mcg,
) -> (Vec<String>, usize, i64) {
    let mut lines = Vec::new();
    let mut deals = 0usize;
    let mut top_fee = 0i64;
    for loan in loans {
        let (pi, home, away) = (loan.player, loan.home, loan.away);
        if w.pool[pi].retired || moved.contains(&pi) || w.pool[pi].team != home {
            continue;
        }
        // Recall: the loan turned him into a parent-XI player — he stays.
        if best_xi_indices(&w.teams[home].players, &w.pool).contains(&pi) {
            continue;
        }
        // Loan-to-buy: he starred (15+ apps) for a smaller club whose books
        // stretch to a fair fee. The parent cashes in on a player it cannot
        // itself use and reinvests (the elite-loan economy, systematised).
        let starred = w.pool[pi].apps >= 15;
        let smaller = w.teams[away].rep + 6 <= w.teams[home].rep;
        if !starred || !smaller {
            continue;
        }
        let fee = (w.pool[pi].value as f32 * 1.15) as i64;
        if fee > w.teams[away].budget {
            continue;
        }
        if wage_bill(w, away) + w.pool[pi].wage > wage_cap(w.teams[away].revenue_last, &w.params) {
            continue;
        }
        w.teams[home].players.retain(|&x| x != pi);
        w.teams[home].budget += fee;
        w.teams[home].cash += fee / 2;
        w.teams[away].budget -= fee;
        w.teams[home].net_spend -= fee;
        w.teams[away].net_spend += fee;
        w.pool[pi].team = away;
        w.pool[pi].homegrown = false;
        w.pool[pi].locked = 3 + (rng.gen::<u8>() % 3);
        if w.pool[pi].path.is_empty() {
            let origin = w.teams[home].name.clone();
            w.pool[pi].path.push(origin);
        }
        w.pool[pi].path.push(w.teams[away].name.clone());
        if w.pool[pi].path.len() > 8 {
            w.pool[pi].path.drain(0..1);
        }
        moved.insert(pi);
        lines.push(format!(
            "{} loan made permanent: {} -> {} ({}m)",
            w.pool[pi].name,
            w.teams[home].name,
            w.teams[away].name,
            ((fee as f32 / w.inflation.max(1.0)).round() as i64)
        ));
        deals += 1;
        if fee > top_fee {
            top_fee = fee;
        }
    }
    (lines, deals, top_fee)
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
    let loans_back = return_loans(w);
    // Budgets come from the money loop, not reputation: one summer's allocation
    // (last season's revenue × board willingness) plus a small dividend from
    // carried cash. Reserves are a backstop for a galáctico, not a war chest
    // that buys anyone every summer — a mid-table club with no Europe simply
    // cannot outbid a superclub, and the finance table shows why.
    // Finance/manager risk model: adjusts board willingness, not the final
    // amount of money. A calm board, a broke squad, or a transitional manager
    // changes how much of its real revenue share is actually deployed.
    let fallback = net::default_finance_weights();
    let finance_model =
        net::finance_net(if w.params.finance_policy.len() == net::FINANCE_WEIGHTS {
            &w.params.finance_policy
        } else {
            &fallback
        });
    let wage_bills: Vec<i64> = (0..w.teams.len()).map(|ti| wage_bill(w, ti)).collect();
    let prospect_ratios: Vec<f32> = w
        .teams
        .iter()
        .map(|t| {
            if t.players.is_empty() {
                0.0
            } else {
                let kids = t.players.iter().filter(|&&i| w.pool[i].age <= 21).count();
                kids as f32 / t.players.len() as f32
            }
        })
        .collect();
    for (ti, t) in w.teams.iter_mut().enumerate() {
        // Boards with piles spend: effective willingness rises with reserves
        // (up to +0.3), so surplus converts into ambition instead of just
        // sitting there — while misers still hoard-and-siphon by character.
        let pile = (t.cash.max(0) as f32 / 1000.0).clamp(0.0, 1.0);
        let cap = wage_cap(t.revenue_last, &w.params).max(1);
        let bill = wage_bills.get(ti).copied().unwrap_or(0);
        let wage_room = (1.0 - bill as f32 / cap as f32).clamp(0.0, 1.0);
        let wage_ratio = if t.revenue_last > 0 {
            bill as f32 / t.revenue_last as f32
        } else {
            0.0
        };
        let finance_inputs = [
            wage_room,
            (t.revenue_last.max(0) as f32 / 1000.0).clamp(0.0, 1.0),
            (t.budget.max(0) as f32 / 1000.0).clamp(0.0, 1.0),
            wage_ratio.clamp(0.0, 1.0),
            (t.strength / 100.0).clamp(0.0, 1.0),
            prospect_ratios.get(ti).copied().unwrap_or(0.0),
            t.willingness.clamp(0.0, 1.0),
            ((t.cash.max(0) + t.revenue_last.max(0)) as f32 / 2000.0).clamp(0.0, 1.0),
            (w.inflation - 1.0).clamp(0.0, 1.0),
            (t.academy as f32 / 5.0).clamp(0.0, 1.0),
            (t.manager.ambition / 1.5).clamp(0.0, 1.0),
            (t.rep as f32 / 100.0).clamp(0.0, 1.0),
        ];
        let (finance_score, finance_audit) = finance_model.audit(
            "budget",
            &format!("{} {}", t.name, BASE_YEAR),
            &finance_inputs,
        );
        net::append_audit(&finance_audit);
        let finance_multiplier = 0.90 + finance_score * 0.20;
        let effective_will = if t.cash < -300 {
            t.willingness * 0.5
        } else {
            ((t.willingness + 0.3 * pile).min(1.0) * finance_multiplier).clamp(0.0, 1.0)
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
        // Dynasty complacency: the board of a consecutive champion believes
        // the squad is complete — allocation shrinks per straight title (up
        // to −30%), the refresh slows, and veterans age into the gap while
        // challengers' money catches up. Momentum clubs push the boat out.
        if t.consec_titles >= 2 {
            let drag = 1.0 - 0.10 * (t.consec_titles - 1).min(3) as f32;
            t.budget = (t.budget as f32 * drag) as i64;
        }
        if t.momentum > 0 {
            t.budget = (t.budget as f32 * 1.15) as i64;
        }
        t.budget_alloc = t.budget;
        // New window, new books: transfer net spend accumulates from here.
        t.net_spend = 0;
    }
    // Loan futures first (budgets now exist): recalls home, loan-to-buy
    // conversions for kids who starred at smaller clubs.
    {
        let (lines, deals, top) = resolve_loan_futures(w, &loans_back, &mut moved, rng);
        deals_done += deals;
        if top > max_fee {
            max_fee = top;
        }
        for l in lines {
            star_lines.push(l);
        }
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
                    if let Some(entry) = sale_line(
                        w.params.log_fee.round() as i64,
                        v,
                        rl(v),
                        name,
                        dest,
                        ovr,
                        age,
                        pot,
                    ) {
                        log.push(entry);
                    }
                }
                None => break,
            }
        }
    }

    // Breakout auction: extraordinary production outside the Big Five creates
    // its own market. A 35-goal Liga MX/Argentina/AFC season need not win the
    // Ballon, but it must attract a stronger sporting challenge immediately.
    // The clause/request route prevents the small club from vetoing a fair,
    // transformative offer while still banking the fee for reinvestment.
    let mut breakouts: Vec<usize> = (0..w.pool.len())
        .filter(|&pi| {
            let p = &w.pool[pi];
            !p.retired
                && !p.on_loan
                && !moved.contains(&pi)
                && p.team < w.teams.len()
                && w.teams[p.team].league_idx >= BIG5.len()
                && p.goals >= 35
                && p.age <= 34
        })
        .collect();
    breakouts.sort_by(|&a, &b| w.pool[b].goals.cmp(&w.pool[a].goals));
    for pi in breakouts.into_iter().take(8) {
        let seller = w.pool[pi].team;
        let pos = w.pool[pi].pos;
        let rating = w.pool[pi].ovr as f32;
        let fee = w.pool[pi]
            .release_clause
            .unwrap_or_else(|| (w.pool[pi].value as f32 * 1.8) as i64)
            .max(1);
        let agent = (fee as f32 * w.params.agent_fee) as i64;
        let new_wage = (w.pool[pi].wage as f32 * (1.20 + w.minds[pi].greed * 0.25)) as i64;
        let mut destination: Option<(usize, f32)> = None;
        for buyer in 0..w.teams.len() {
            if buyer == seller || w.teams[buyer].league_idx >= BIG5.len() {
                continue;
            }
            if w.teams[buyer].rep <= w.teams[seller].rep + 5
                || w.teams[buyer].budget < fee + agent
                || fee + agent > (w.teams[buyer].revenue_last as f32 * 0.35) as i64
                || wage_bill(w, buyer) + new_wage > wage_cap(w.teams[buyer].revenue_last, &w.params)
            {
                continue;
            }
            let (need, incumbent) = weakest_slot(&w.teams[buyer].players, &w.pool);
            if need != pos || rating + 5.0 < incumbent {
                continue;
            }
            let score = w.teams[buyer].rep as f32
                + w.teams[buyer].manager.ambition * 5.0
                + (rating - incumbent).max(-5.0);
            if destination.map_or(true, |(_, best)| score > best) {
                destination = Some((buyer, score));
            }
        }
        let Some((buyer, _)) = destination else {
            continue;
        };
        let seller_name = w.teams[seller].name.clone();
        let buyer_name = w.teams[buyer].name.clone();
        let player_name = w.pool[pi].name.clone();
        let season_goals = w.pool[pi].goals;
        w.teams[seller].players.retain(|&x| x != pi);
        w.teams[buyer].players.push(pi);
        w.teams[seller].budget += fee;
        w.teams[seller].cash += fee / 2;
        w.teams[seller].net_spend -= fee;
        w.teams[buyer].budget -= fee + agent;
        w.teams[buyer].net_spend += fee + agent;
        agent_paid += agent;
        w.pool[pi].team = buyer;
        w.pool[pi].homegrown = false;
        w.pool[pi].wage = new_wage.max(1);
        w.pool[pi].locked = if w.pool[pi].ovr >= 85 { 4 } else { 3 };
        w.pool[pi].release_clause = None;
        w.pool[pi].morale = 80;
        if w.pool[pi].path.is_empty() {
            w.pool[pi].path.push(seller_name.clone());
        }
        w.pool[pi].path.push(buyer_name.clone());
        w.minds[pi].requested = false;
        w.minds[pi].unhappy = false;
        w.minds[pi].contract_happiness = 0.72;
        moved.insert(pi);
        deals_done += 1;
        max_fee = max_fee.max(fee);
        log.push((
            fee,
            format!(
                "{}: {}-goal breakout {} -> {} (clause {}m)",
                player_name,
                season_goals,
                seller_name,
                buyer_name,
                rl(fee)
            ),
        ));
    }

    // ── Scouting pass: prospects rise through the pyramid on potential ──
    // Need-buying prices only today's rating, so a 17-year-old at a minnow
    // with an 85 ceiling never moves. Real football moves him: scouts pay
    // for the ceiling, minnows sell transformative fees to rebuild the whole
    // squad, and nobody jumps straight from a fishpond to a superclub —
    // talent climbs the ladder (Bryne → Molde → Salzburg → Dortmund → City).
    // Deterministic multi-criteria scoring: potential gap, step discipline,
    // contract leverage, seller finance, buyer affordability. No dice.
    {
        let fallback = net::default_scout_weights();
        let scout_model = net::scout_net(if w.params.scout_policy.len() == net::SCOUT_WEIGHTS {
            &w.params.scout_policy
        } else {
            &fallback
        });
        let tier = |rep: u8| -> u8 {
            if rep >= 88 {
                1
            } else if rep >= 80 {
                2
            } else if rep >= 70 {
                3
            } else if rep >= 60 {
                4
            } else {
                5
            }
        };
        let mut buyers: Vec<usize> = (0..w.teams.len()).collect();
        buyers.shuffle(rng);
        for buyer in buyers {
            // Only tiers 1-3 shop the future seriously (the Madrid model —
            // light on academy trust, heavy on scouting), ~every other window,
            // with squad room. One prospect deal per club per window.
            let buyer_tier = tier(w.teams[buyer].rep);
            if buyer_tier >= 4 || !rng.gen_bool(0.5) {
                continue;
            }
            let active = w.teams[buyer]
                .players
                .iter()
                .filter(|&&i| !w.pool[i].retired)
                .count();
            if active >= 30 {
                continue;
            }
            let (need_pos, incumbent) = weakest_slot(&w.teams[buyer].players, &w.pool);
            // Best affordable prospect anywhere strictly below our rung.
            let mut best: Option<(usize, usize, i64)> = None; // (player, seller, ask)
            for seller in 0..w.teams.len() {
                if seller == buyer {
                    continue;
                }
                let seller_tier = tier(w.teams[seller].rep);
                if seller_tier <= buyer_tier {
                    continue; // shop downward only
                }
                for &pi in &w.teams[seller].players {
                    let p = &w.pool[pi];
                    // Contract years do not hide a prospect from scouts: a
                    // committed kid is simply more expensive (priced below),
                    // not invisible. Only loans/retired players are off-market.
                    if moved.contains(&pi) || p.on_loan || p.retired {
                        continue;
                    }
                    // Already-elite players need a smaller ceiling gap to be
                    // worth a prospect bid; a 90-rated kid at 95 potential is
                    // a real target even though he is not +8 from his floor.
                    let gap_req = if p.ovr >= 88 { 4 } else { 8 };
                    if p.age > 22 || p.pot < p.ovr + gap_req || p.ovr < 58 || p.pos == Position::GK
                    {
                        continue;
                    }
                    // Step discipline: 17-18s move ONE rung; 19-21s two;
                    // 22s (proven at their level) may skip three — the
                    // Mbappé Monaco→PSG jump, never Bryne→Madrid direct.
                    let rungs = seller_tier - buyer_tier;
                    let max_rungs = if p.age <= 18 {
                        1
                    } else if p.age <= 21 {
                        2
                    } else {
                        3
                    };
                    if rungs > max_rungs {
                        continue;
                    }
                    // Ask: fresh value × potential premium (the ceiling is
                    // what scouts pay for) × contract premium (committed
                    // kids cost more — the Monaco hold-out priced in) × the
                    // scout model's risk-adjusted valuation.
                    let base = market_value(p.ovr, p.age, p.pos) as f32;
                    let raw_ask = (base
                        * (1.0 + (p.pot - p.ovr) as f32 * 0.045)
                        * (1.0 + p.locked as f32 * 0.22)
                        * w.inflation) as i64;
                    let need_ratio = ((p.ovr as f32 - incumbent) / 20.0).clamp(0.0, 1.0);
                    let rep_gap = ((w.teams[buyer].rep as f32 - w.teams[seller].rep as f32) / 30.0)
                        .clamp(0.0, 1.0);
                    let seller_finance = if w.teams[seller].revenue_last > 0 {
                        (w.teams[seller].cash.max(0) as f32 / w.teams[seller].revenue_last as f32)
                            .clamp(0.0, 1.0)
                    } else if w.teams[seller].cash < 0 {
                        0.0
                    } else {
                        0.5
                    };
                    let buyer_cap_wage = wage_cap(w.teams[buyer].revenue_last, &w.params);
                    let wage_fit = if buyer_cap_wage > 0 {
                        (1.0 - p.wage as f32 / buyer_cap_wage as f32).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let scout_inputs = [
                        need_ratio,
                        (p.pot as f32 / 100.0).clamp(0.0, 1.0),
                        ((22u8.saturating_sub(p.age)) as f32 / 7.0).clamp(0.0, 1.0),
                        if w.teams[buyer].budget > 0 {
                            (raw_ask as f32 / w.teams[buyer].budget as f32).clamp(0.0, 1.0)
                        } else {
                            1.0
                        },
                        if p.pos == need_pos { 1.0 } else { 0.25 },
                        (max_rungs as f32 / 3.0).clamp(0.0, 1.0),
                        seller_finance,
                        wage_fit,
                        (w.teams[buyer].academy as f32 / 5.0).clamp(0.0, 1.0),
                        ((p.pot.saturating_sub(p.ovr)) as f32 / 40.0).clamp(0.0, 1.0),
                        1.0 - (rungs as f32 / max_rungs.max(1) as f32),
                        rep_gap,
                    ];
                    let (scout_score, scout_audit) = scout_model.audit(
                        "prospect",
                        &format!(
                            "{} from {} -> {}",
                            p.name, w.teams[seller].name, w.teams[buyer].name
                        ),
                        &scout_inputs,
                    );
                    net::append_audit(&scout_audit);
                    let ask = (raw_ask as f32 * (0.85 + scout_score * 0.30)).round() as i64;
                    // Never more than 40% of the window's money on one kid,
                    // and the squad-cost gate (35% of revenue) applies too.
                    if ask > (w.teams[buyer].budget as f32 * 0.40) as i64 {
                        continue;
                    }
                    if ask as f32 > w.teams[buyer].revenue_last as f32 * 0.35 {
                        continue;
                    }
                    if best.map_or(true, |(bpi, _, _)| p.pot > w.pool[bpi].pot) {
                        best = Some((pi, seller, ask));
                    }
                }
            }
            let Some((pi, seller, ask)) = best else {
                continue;
            };
            // The minnow's decision — the "stuck on a long deal" problem and
            // its price: sell only when the fee transforms the club (~a third
            // of a season's revenue), when the kid has clearly outgrown the
            // level at real money, or when the books demand it.
            let base_now = market_value(w.pool[pi].ovr, w.pool[pi].age, w.pool[pi].pos) as f32;
            let transformative = ask as f32 >= w.teams[seller].revenue_last as f32 * 0.33;
            let outgrown =
                w.pool[pi].ovr >= 74 && ask >= (base_now * 1.8) as i64 && w.pool[pi].age >= 19;
            // A triggered below-market clause on an elite outlier is not a
            // negotiation the seller can refuse; the market has already
            // priced the exit. Treat it as an outgrown sale.
            let clause_sale = is_clause_opportunity(&w.pool[pi]) && w.pool[pi].ovr >= 88;
            let distressed = w.teams[seller].cash < 0;
            if !transformative && !outgrown && !clause_sale && !distressed {
                continue;
            }
            let agent = (ask as f32 * w.params.agent_fee) as i64;
            if w.teams[buyer].budget < ask + agent {
                continue;
            }
            if wage_bill(w, buyer) + w.pool[pi].wage
                > wage_cap(w.teams[buyer].revenue_last, &w.params)
            {
                continue;
            }
            // Execute: same bookkeeping as any permanent move, plus a cash
            // windfall the selling board actually feels (funds the rebuild).
            let old_team = w.pool[pi].team;
            w.teams[seller].players.retain(|&x| x != pi);
            w.teams[seller].budget += ask;
            w.teams[seller].cash += ask / 2;
            w.teams[buyer].players.push(pi);
            w.teams[buyer].budget -= ask + agent;
            w.teams[buyer].net_spend += ask + agent;
            w.teams[seller].net_spend -= ask;
            agent_paid += agent;
            w.pool[pi].team = buyer;
            w.pool[pi].homegrown = false;
            w.pool[pi].locked = 3 + (rng.gen::<u8>() % 3);
            w.pool[pi].release_clause = None;
            w.pool[pi].value = (market_value(w.pool[pi].ovr, w.pool[pi].age, w.pool[pi].pos) as f32
                * w.inflation) as i64;
            w.pool[pi].wage = ((w.pool[pi].value as f32 * 2.0 / 5.0) as i64).max(1);
            w.pool[pi].morale = 75;
            w.minds[pi].requested = false;
            w.minds[pi].unhappy = false;
            moved.insert(pi);
            // Career cascade record: seed with the origin club on first move.
            if w.pool[pi].path.is_empty() {
                let origin = w.teams[old_team].name.clone();
                w.pool[pi].path.push(origin);
            }
            w.pool[pi].path.push(w.teams[buyer].name.clone());
            if w.pool[pi].path.len() > 8 {
                w.pool[pi].path.drain(0..1);
            }
            deals_done += 1;
            if ask > max_fee {
                max_fee = ask;
            }
            let line = format!(
                "{} ({}, pot {}) scouted: {} -> {} ({}m)",
                w.pool[pi].name,
                w.pool[pi].age,
                w.pool[pi].pot,
                w.teams[old_team].name,
                w.teams[buyer].name,
                rl(ask)
            );
            if ask >= w.params.log_fee.round() as i64 {
                log.push((ask, line));
            } else {
                star_lines.push(line);
            }
        }
    }
    let mut order: Vec<usize> = (0..w.teams.len()).collect();
    // Shuffled acting order: no global turn sequence, each manager moves when
    // their number comes up. Ambition sets attempts, not priority.
    order.shuffle(rng);
    let manager_fallback = net::default_manager_weights();
    let manager_model =
        net::manager_net(if w.params.manager_policy.len() == net::MANAGER_WEIGHTS {
            &w.params.manager_policy
        } else {
            &manager_fallback
        });

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
                let elite_visible = (w.pool[pi].ovr >= 90 && w.pool[pi].age <= 24)
                    || (w.pool[pi].pot >= 95 && w.pool[pi].age <= 21);
                if w.teams[ti].league_idx == buyer_league || elite_visible || rng.gen_bool(0.40) {
                    known.insert(pi);
                }
            }
        }
        let buyer_wage_bill = wage_bill(w, buyer);
        let buyer_cap = wage_cap(w.teams[buyer].revenue_last, &w.params).max(1);
        let buyer_wage_ratio =
            (buyer_wage_bill as f32 / w.teams[buyer].revenue_last.max(1) as f32).clamp(0.0, 1.0);
        let buyer_prospect_ratio = if w.teams[buyer].players.is_empty() {
            0.0
        } else {
            w.teams[buyer]
                .players
                .iter()
                .filter(|&&i| w.pool[i].age <= 21)
                .count() as f32
                / w.teams[buyer].players.len() as f32
        };
        let buyer_average_age = if w.teams[buyer].players.is_empty() {
            0.5
        } else {
            w.teams[buyer]
                .players
                .iter()
                .map(|&i| w.pool[i].age as f32 / 45.0)
                .sum::<f32>()
                / w.teams[buyer].players.len() as f32
        };
        let manager_inputs = [
            (1.0 - buyer_wage_bill as f32 / buyer_cap as f32).clamp(0.0, 1.0),
            buyer_average_age.clamp(0.0, 1.0),
            (w.teams[buyer].title_drought as f32 / 20.0).clamp(0.0, 1.0),
            buyer_prospect_ratio.clamp(0.0, 1.0),
            (w.teams[buyer].wealth as f32 / 100.0).clamp(0.0, 1.0),
            w.teams[buyer].willingness.clamp(0.0, 1.0),
            w.teams[buyer].manager.security.clamp(0.0, 1.0),
            (w.teams[buyer].manager.ambition / 1.5).clamp(0.0, 1.0),
            w.teams[buyer].manager.youth_trust.clamp(0.0, 1.0),
            (w.teams[buyer].rep as f32 / 100.0).clamp(0.0, 1.0),
            w.teams[buyer].manager.quality.clamp(0.0, 1.0),
            ((w.teams[buyer].w as f32) / 38.0).clamp(0.0, 1.0),
        ];
        let (manager_score, manager_audit) = manager_model.audit(
            "transfer_attempts",
            &format!("{} manager", w.teams[buyer].name),
            &manager_inputs,
        );
        net::append_audit(&manager_audit);
        let attempts = ((1.0 + w.teams[buyer].manager.ambition * 2.0)
            * (0.75 + manager_score * 0.5))
            .round() as i32; // 2..4
        for _ in 0..attempts {
            let (need_pos, incumbent) = weakest_slot(&w.teams[buyer].players, &w.pool);
            let buyer_rep = w.teams[buyer].rep;

            // Find the best willing upgrade at the position of need, among players
            // this manager actually knows (same league fully scouted, ~40% of
            // the wider world). Unknown players can't be bid for.
            let mut target: Option<(usize, i64, f32, f32)> = None;
            // (idx, ask, selection score, incumbent at candidate's position)
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
                    // Remaining contract years are not an immovable lock: if a
                    // buyer is making a real offer, transfer_willing decides
                    // whether the player is open to moving up (or a clause /
                    // request applies). Locked players are priced, not hidden.
                    if moved.contains(&pi) || !known.contains(&pi) || w.pool[pi].on_loan {
                        continue;
                    }
                    let p = &w.pool[pi];
                    if p.retired {
                        continue;
                    }
                    let rating = p.attrs.overall(p.pos) as f32;
                    let Some(candidate_incumbent) = acquisition_incumbent(
                        &w.teams[buyer].players,
                        &w.pool,
                        p,
                        need_pos,
                        incumbent,
                    ) else {
                        continue;
                    };
                    // A below-market clause on a true elite outlier is itself
                    // a market inefficiency: even if the buyer's weakest
                    // starter is already 98, a 95+ cheap-clause star is worth
                    // pursuing as a future-proof investment.
                    let improvement_ok = rating >= candidate_incumbent + w.params.min_improve
                        || (is_clause_opportunity(p) && rating >= 95.0);
                    if !improvement_ok {
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
                    // Bosman: an expired contract is a free move — no fee
                    // changes hands; the destination buys the wages, not the
                    // player (and fights for backups elsewhere, not for him).
                    let bosman = p.locked == 0;
                    if bosman {
                        ask = 0;
                    }
                    if w.minds[pi].requested {
                        ask = (ask as f32 * w.params.req_discount) as i64;
                    }
                    // Zero asking only valid for a Bosman (free at will).
                    if ask < 1 && !bosman {
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
                        buyer_rep,
                        seller_rep,
                        w.params.gap_clause,
                        w.params.gap_starter,
                        w.pool[pi].locked,
                        w.pool[pi].ovr,
                        w.minds[pi].contract_happiness,
                    ) || nemesis_between(w, "player", pi, "club", ti) > 0.60;
                    if !willing {
                        continue;
                    }
                    // Veteran penalty in target selection: at equal raw ratings
                    // a 25yo prospect beats a 31yo backup by a clear margin —
                    // clubs buy futures, not pasts (no more Karius-to-PSG
                    // seasons). Thirty is the cliff edge, then 2 points a year.
                    // A deep-undervalue clause on a true elite is the one
                    // market inefficiency that should dominate the board.
                    let mut score = rating - (p.age.saturating_sub(30) as f32) * 2.0;
                    if is_clause_opportunity(p) && rating >= 95.0 {
                        score += 25.0;
                    } else {
                        score -= p.age as f32 * 0.02;
                    }
                    if target.map_or(true, |(_, _, best_score, _)| score > best_score) {
                        target = Some((pi, ask, score, candidate_incumbent));
                    }
                }
            }

            let (pi, ask, _rating, target_incumbent) = match target {
                Some(t) => t,
                None => break, // nobody worth signing at the need position
            };

            // Squad registration has teeth at acquisition time: a full 25-man
            // squad must move someone out before buying — sell-to-buy for
            // space, exactly like the wage check below does for money.
            // Exception: a deep-undervalue clause on an elite is a mandatory
            // market inefficiency; the buyer can register him at the next
            // registration even if no clean sell-to-buy destination exists.
            let active_here = w.teams[buyer]
                .players
                .iter()
                .filter(|&&i| !w.pool[i].retired)
                .count();
            if active_here >= MAX_REGISTERED && sell_surplus(w, buyer, &mut moved).is_none() {
                let bargain_clause = w.pool[pi]
                    .release_clause
                    .is_some_and(|rc| ask >= rc && ask <= w.pool[pi].value / 2)
                    && w.pool[pi].attrs.overall(w.pool[pi].pos) >= 95;
                if !bargain_clause {
                    continue; // no room and nothing movable — deal dead
                }
            }

            // Bilateral negotiation between the two clubs' agents. The buyer
            // opens below ask (trying a deal, openness varies); the seller
            // accepts at their threshold — 95%, or lower for transfer requests
            // and cash-strapped sellers. On rejection the buyer counters once
            // at full ask when affordable (a seller never turns down their own
            // asking price); otherwise the deal is dead this window.
            let seller = w.pool[pi].team;
            let seller_threshold = if w.minds[pi].requested || w.teams[seller].budget < 20 {
                w.params.sell_distress
            } else {
                w.params.sell_base
            };
            let clause_deal = w.pool[pi]
                .release_clause
                .is_some_and(|clause| ask >= clause);
            let Some(fee) = agreed_transfer_fee(
                ask,
                w.teams[buyer].budget,
                seller_threshold,
                w.params.open_pct,
                clause_deal,
            ) else {
                continue;
            };

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
                if bound && (w.pool[pi].value as f32 * ransom_mult) as i64 > fee.max(ask) {
                    continue;
                }
            }

            // The buying manager's policy network has the final say. Inputs are
            // the same signals a manager weighs — need, value, wage room, own
            // ambition, reputation gap, request status — so identical deals pass
            // for one individual and fail for another. Genuine need (a big gap
            // at the position) forces even a sceptical network's hand.
            let mut need = ((w.pool[pi].attrs.overall(w.pool[pi].pos) as f32 - target_incumbent)
                / 10.0)
                .clamp(0.0, 1.0);
            // A cheap-clause elite is an investment, not merely a one-slot
            // upgrade: managers do not need a 10-point gap to pursue him.
            if clause_deal && w.pool[pi].attrs.overall(w.pool[pi].pos) >= 95 {
                need = need.max(0.55);
            }
            let value_fit =
                (1.0 - fee as f32 / (w.teams[buyer].budget.max(1)) as f32).clamp(0.0, 1.0);
            let wage_room = (1.0
                - wage_bill(w, buyer) as f32
                    / (wage_cap(w.teams[buyer].revenue_last, &w.params).max(1)) as f32)
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
                        log.push((
                            sale.0,
                            format!(
                                "{} sold by {} to {} ({}m)",
                                sale.1,
                                w.teams[buyer].name,
                                to,
                                rl(sale.0)
                            ),
                        ));
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
                        log.push((
                            sale.0,
                            format!(
                                "{} sold by {} to {} ({}m)",
                                sale.1,
                                w.teams[buyer].name,
                                to,
                                rl(sale.0)
                            ),
                        ));
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
            // Career cascade record: seed with the origin club on first move.
            if w.pool[pi].path.is_empty() {
                let origin = w.teams[seller].name.clone();
                w.pool[pi].path.push(origin);
            }
            w.pool[pi].path.push(w.teams[buyer].name.clone());
            if w.pool[pi].path.len() > 8 {
                w.pool[pi].path.drain(0..1);
            }
            // A permanent move away ends club-trained status here.
            w.pool[pi].homegrown = false;
            moved.insert(pi);
            let signing_bonus = (fee as f32 * (0.05 + w.minds[pi].greed * 0.10)).round() as i64;
            // New multi-year contract: commit the player and drop the easy
            // release clause, so stars settle instead of moving every summer.
            w.pool[pi].locked = 3 + (rng.gen::<u8>() % 3); // 3..=5 year deal
            w.pool[pi].signing_bonus += signing_bonus;
            w.teams[buyer].budget -= signing_bonus;
            w.teams[buyer].cash -= signing_bonus;
            w.teams[buyer].net_spend += signing_bonus;
            w.pool[pi].career_earnings += signing_bonus;
            w.pool[pi].contract_option = w.pool[pi].ovr >= 80 && rng.gen_bool(0.30);
            w.pool[pi].release_clause = None;
            let (ovr, age) = (w.pool[pi].ovr, w.pool[pi].age);
            w.pool[pi].value = (market_value(ovr, age, w.pool[pi].pos) as f32 * w.inflation) as i64;
            // Greedy players negotiate a premium on top of the standard wage.
            w.pool[pi].wage =
                (((w.pool[pi].value * 2 / 5).max(1) as f32) * w.minds[pi].wage_mult()) as i64;
            let departing_request = w.minds[pi].requested;
            // A completed move settles the player: request cleared, mood restored.
            w.minds[pi].requested = false;
            w.minds[pi].unhappy = false;
            w.minds[pi].contract_happiness = (0.55 + w.minds[pi].loyalty * 0.30).clamp(0.0, 1.0);
            w.pool[pi].morale = 75;
            // Long memory: a star sold to a close domestic rival, or a
            // requested move after a rejected contract, leaves a personal
            // wound. It feeds later contract moods and resilience against
            // the seller.
            if departing_request || w.teams[seller].nation == w.teams[buyer].nation {
                let opening: f32 = if departing_request { 0.45 } else { 0.25 };
                let story = format!(
                    "{} left {} for {} and carried the grudge forward",
                    w.pool[pi].name, w.teams[seller].name, w.teams[buyer].name
                );
                w.pool[pi].morale = (w.pool[pi].morale as i32 - ((opening * 20.0).round() as i32))
                    .clamp(30, 90) as u8;
                record_nemesis(w, "player", pi, "club", seller, opening, story);
                if buyer != seller {
                    record_nemesis(
                        w,
                        "club",
                        buyer,
                        "club",
                        seller,
                        opening * 0.7,
                        format!(
                            "{} bought {} from {}",
                            w.teams[buyer].name, w.pool[pi].name, w.teams[seller].name
                        ),
                    );
                }
            }
            if fee >= w.params.log_fee.round() as i64 {
                log.push((
                    fee,
                    format!(
                        "{} -> {} ({}m, +{}m agent)",
                        w.pool[pi].name,
                        w.teams[buyer].name,
                        rl(fee),
                        rl(agent)
                    ),
                ));
            }
            // Star moves are always traceable, whatever the fee: a sub-threshold
            // 82+ move must never become a ghost in the career tables.
            if ovr >= 82 && fee < w.params.log_fee.round() as i64 {
                star_lines.push(format!(
                    "{} -> {} ({}m)",
                    w.pool[pi].name,
                    w.teams[buyer].name,
                    rl(fee)
                ));
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

/// Annual performance bonus for a player who actually produced.
fn season_performance_bonus(p: &WP) -> i64 {
    if p.retired {
        return 0;
    }
    let production = p.overall_goals() + p.overall_assists();
    let mut rate = 0.0f32;
    if p.pos == Position::GK {
        if p.clean_sheets >= 12 {
            rate = 0.10;
        }
    } else if p.ovr >= 82 {
        rate = 0.12;
    } else if p.ovr >= 75 && production >= 20 {
        rate = 0.08;
    }
    if p.apps >= 25 && p.avg_rating() >= 8.15 {
        rate += 0.05;
    }
    (p.wage as f32 * rate).round() as i64
}

struct ContractOffer {
    accepted: bool,
    wage: i64,
    signing_bonus: i64,
    contract_years: u8,
    club_option: bool,
    contract_happiness: f32,
}

/// A club's offer and the player's acceptance probability. The decision is
/// deliberately not "highest wage wins": it prices ask premium, the club's
/// wage room, manager ambition, stage/reputation, the player's motives, and
/// whether he already wants out. The existing tiny policy network is the
/// player-decision model (one hidden layer, sigmoid output); this maps
/// contract situations into the same input language used for transfers.
fn build_contract_offer(
    p: &WP,
    mind: &PlayerMind,
    team: &Team,
    params: &Params,
    current_wage_bill: i64,
    rng: &mut Pcg64Mcg,
) -> ContractOffer {
    let base_from_value = (p.value * 2 / 5).max(1);
    let market_wage = (base_from_value as f32 * mind.wage_mult()).round() as i64;
    let retention_need = if p.ovr >= 90 {
        0.90
    } else if p.ovr >= 82 {
        0.75
    } else if p.ovr >= 75 {
        0.45
    } else {
        0.25
    };
    let cap = wage_cap(team.revenue_last, params);
    let wage_room = (1.0
        - if cap > 0 {
            current_wage_bill as f32 / cap as f32
        } else {
            1.0
        })
    .clamp(0.0, 1.0);
    let elite_retention = p.ovr >= 82;
    let urgent = mind.requested || mind.unhappy;
    let demand = p.wage.max(market_wage);
    // The club tries to keep its man: an elite star, a transfer request, or a
    // financially valuable prospect gets a genuine improvement on the table --
    // not a token from the table, not symbolic.
    let premium = if elite_retention && urgent {
        1.10 + mind.greed * 0.22
    } else if elite_retention {
        1.03 + mind.greed * 0.15
    } else {
        1.0 + mind.greed * 0.10
    };
    let wage = (demand as f32 * premium * (1.0 + (team.willingness - 0.5) * 0.10)).round() as i64;
    let ask = demand.max(1);
    let offer_ratio = (wage as f32 / ask as f32).clamp(0.0, 1.35);
    let club_stage = (team.rep as f32 / 100.0).clamp(0.0, 1.0);
    let player_inputs = vec![
        (p.age as f32 / 45.0).clamp(0.0, 1.0),
        (p.ovr as f32 / 100.0).clamp(0.0, 1.0),
        (p.attrs.pace as f32 / 100.0).clamp(0.0, 1.0),
        (p.attrs.stamina as f32 / 100.0).clamp(0.0, 1.0),
        ((p.pot.saturating_sub(p.ovr)) as f32 / 50.0).clamp(0.0, 1.0),
        offer_ratio,
        club_stage,
        (p.morale as f32 / 100.0).clamp(0.0, 1.0),
        mind.ambition,
        mind.greed,
        mind.loyalty,
        mind.contract_happiness,
    ];
    let fallback = net::default_player_weights();
    let player_model = net::player_net(if params.player_policy.len() == net::PLAYER_WEIGHTS {
        &params.player_policy
    } else {
        &fallback
    });
    let (accept_prob, audit) = player_model.audit(
        "contract",
        &format!("{} at {}", p.name, team.name),
        &player_inputs,
    );
    net::append_audit(&audit);
    let accepted = rng.gen_bool(accept_prob as f64);
    let signing_bonus = if accepted {
        let multiplier = 0.04 + mind.greed * 0.10 + if p.ovr >= 90 { 0.04 } else { 0.0 };
        (p.value as f32 * multiplier).round() as i64
    } else {
        0
    };
    let contract_happiness = if accepted {
        (0.45
            + mind.loyalty * 0.30
            + (offer_ratio - 0.9).max(0.0) * 0.35
            + (club_stage - 0.5) * 0.18
            - if mind.requested { 0.20 } else { 0.0 })
        .clamp(0.0, 1.0)
    } else {
        (0.25 + mind.loyalty * 0.20 - if mind.requested { 0.20 } else { 0.0 }).clamp(0.0, 1.0)
    };
    let club_option = accepted && p.ovr >= 80 && rng.gen_bool(0.30);
    let contract_years = if p.ovr >= 90 {
        5
    } else if p.ovr >= 85 {
        4 + (rng.gen::<u8>() % 2)
    } else if p.ovr >= 82 {
        4
    } else {
        3 + (rng.gen::<u8>() % 3)
    };
    ContractOffer {
        accepted,
        wage,
        signing_bonus,
        contract_years,
        club_option,
        contract_happiness,
    }
}

/// A club's wage capacity as a share of revenue (tuned ratio; real wage-to-turnover
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
        let brand_floor = revenue(w.teams[t].heritage) * 0.55;
        let base = revenue(w.teams[t].wealth).max(brand_floor) * infl;
        let revenue = (base + (league_prize + ucl_prize + el_prize) * infl) as i64;
        let mut performance_bonus_cost = 0i64;
        for &pi in w.teams[t].players.clone().iter() {
            if w.pool[pi].retired {
                continue;
            }
            let bonus = season_performance_bonus(&w.pool[pi]);
            w.pool[pi].bonus_paid += bonus;
            w.pool[pi].career_earnings += w.pool[pi].wage + bonus;
            performance_bonus_cost += bonus;
        }
        let wages = wage_bill(w, t) + performance_bonus_cost;
        let operating = (base * 0.25) as i64;
        w.teams[t].revenue_last = revenue;
        // Audit trail: the season's money, stream by stream (nominal — the
        // report deflates by inflation for honest cross-era comparison).
        w.teams[t].rev_base = base as i64;
        w.teams[t].prize_league = (league_prize * infl) as i64;
        w.teams[t].prize_euro = ((ucl_prize + el_prize) * infl) as i64;
        // Unspent budgets mostly revert — boards reclaim war chests, so piles
        // can't ratchet forever on rollover alone.
        w.teams[t].cash =
            (w.teams[t].cash + (w.teams[t].budget as f32 * 0.3) as i64).clamp(-500, 1500);
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
        let national_cap = federation_academy_cap(&w.teams[t].nation);
        let sustained_breakthrough = rev > 700 && w.teams[t].wealth >= 90 && rng.gen_bool(0.01);
        let allowed_cap = if sustained_breakthrough {
            (national_cap + 1).min(5)
        } else {
            national_cap
        };
        if rev > 450 && ac < allowed_cap && rng.gen_bool(0.18) {
            w.teams[t].academy += 1;
        } else if rev < 120 && ac > 1 && rng.gen_bool(0.20) {
            w.teams[t].academy -= 1;
        } else if rev >= 120 && ac < allowed_cap && rng.gen_bool(0.03) {
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
        // A surplus man can still be parted with under a mutual
        // termination/buyout when he is not in the XI and is not strongly
        // attached: requested, unhappy with his deal, or low-loyalty players
        // are sellable; a content, loyal contracted player is not dumped.
        let mutual_exit = w.minds[pi].requested
            || w.minds[pi].contract_happiness < 0.55
            || w.minds[pi].loyalty < 0.45;
        if xi.contains(&pi)
            || moved.contains(&pi)
            || w.pool[pi].retired
            || w.pool[pi].on_loan
            || (w.pool[pi].homegrown && w.pool[pi].age <= 21)
            || (w.pool[pi].locked > 0 && !mutual_exit)
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

fn run_cup(
    field: &[usize],
    comp: Comp,
    teams: &mut [Team],
    pool: &mut [WP],
    p: &Params,
    rng: &mut Pcg64Mcg,
) -> Option<usize> {
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
        let hmd = matchday_squad(
            &asel,
            pool,
            teams[a].shape,
            &teams[a].manager.clone(),
            comp,
            rng,
        );
        let amd = matchday_squad(
            &bsel,
            pool,
            teams[b].shape,
            &teams[b].manager.clone(),
            comp,
            rng,
        );
        let winner = if hmd.is_empty() || amd.is_empty() {
            if rng.gen::<f32>() < 0.5 {
                a
            } else {
                b
            }
        } else {
            let axi = build_lineup(&hmd, pool);
            let bxi = build_lineup(&amd, pool);
            let abench = build_lineup(&hmd[11.min(hmd.len())..], pool);
            let bbench = build_lineup(&amd[11.min(amd.len())..], pool);
            let mut side_a = Side {
                players: &axi,
                ambition: teams[a].manager.ambition,
                quality: teams[a].manager.quality,
                threat: 1.0,
                shape: Some(team_shape(&teams[a])),
                width: 50,
                defensive_line: def_line(&teams[a]),
                bench: &abench,
                crowd: 1.0,
            };
            let mut side_b = Side {
                players: &bxi,
                ambition: teams[b].manager.ambition,
                quality: teams[b].manager.quality,
                threat: 1.0,
                shape: Some(team_shape(&teams[b])),
                width: 50,
                defensive_line: def_line(&teams[b]),
                bench: &bbench,
                crowd: 1.0,
            };
            let (ta, tb) = matchsim::threat_ratio(&side_a, &side_b);
            side_a.threat = ta;
            side_b.threat = tb;
            let o = matchsim::simulate(&side_a, &side_b, Venue::Neutral, (0, 0), rng);
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

fn build_nations(
    pool: &[WP],
    kind: IntlKind,
    host_nations: &[String],
    friendly: bool,
    states: &std::collections::HashMap<String, NationalState>,
) -> Vec<Nation> {
    let euro_only = kind == IntlKind::Euros;
    let asian_only = kind == IntlKind::AsianCup;
    let confed_only = match kind {
        IntlKind::CopaAmerica => Some(crate::world_model::Confederation::CONMEBOL),
        IntlKind::Afcon => Some(crate::world_model::Confederation::CAF),
        IntlKind::GoldCup => Some(crate::world_model::Confederation::CONCACAF),
        IntlKind::None | IntlKind::WorldCup | IntlKind::Euros | IntlKind::AsianCup => None,
    };
    let wc_field = kind == IntlKind::WorldCup;
    let mut by_nation: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, w) in pool.iter().enumerate() {
        if w.retired || w.intl_retired {
            continue;
        }
        let n = w.nation.trim();
        if n.len() < 2 || !valid_nationality(n) {
            continue;
        }
        if euro_only && !EUROPE.iter().any(|e| e.eq_ignore_ascii_case(n)) {
            continue;
        }
        if asian_only {
            // AFC Asian Cup: only AFC confederation nations (2nd leg of the
            // international calendar for the non-European world — qualification
            // is by pool depth, so any AFC nation with 15+ active players).
            let c = crate::world_model::confed_of(n);
            if c != crate::world_model::Confederation::AFC {
                continue;
            }
        }
        if let Some(c) = confed_only {
            // Continental championships outside the AFC: full confederation
            // fields, so Argentina/Brazil dominate Copa América by merit and
            // CAF's deep bench competes in AFCON, not just in friendlies.
            if crate::world_model::confed_of(n) != c {
                continue;
            }
        }
        by_nation.entry(n.to_string()).or_default().push(i);
    }
    let mut nations: Vec<Nation> = Vec::new();
    for (name, mut idxs) in by_nation {
        idxs.sort_by(|&a, &b| pool[b].ovr.cmp(&pool[a].ovr));
        idxs.truncate(23);
        // Major tournaments want a real 15+ pool; the friendly calendar is
        // deliberately broader (neighbors and low-level nations with a bare
        // XI) so small federations still get matches.
        let min_pool = if friendly { 11 } else { 15 };
        if idxs.len() < min_pool {
            continue;
        }
        let mut ov: Vec<f32> = idxs.iter().map(|&i| pool[i].ovr as f32).collect();
        ov.sort_by(|a, b| b.partial_cmp(a).unwrap());
        // Two numbers that actually matter for a national side:
        //   actual XI overall — the mean of the eleven strongest FC overalls
        //     (the real, match-day strength of the nation), and
        //   squad depth signal — the rest of the 23, which is where real
        //     giants separate from minnow pools.
        // Qualifying/ranking uses `xi_overall` (the actual Eleven) directly;
        // the XI overall is then used for draw math, so a France with a
        // killer genuine XI behaves like one.
        let xi_overall = ov.iter().take(11).sum::<f32>() / 11.0;
        let depth_avg = ov.iter().skip(11).sum::<f32>() / ov.len().saturating_sub(11).max(1) as f32;
        let depth_edge = if depth_avg >= 80.0 {
            1.0
        } else if depth_avg >= 76.0 {
            0.985
        } else if depth_avg >= 72.0 {
            0.97
        } else if depth_avg >= 68.0 {
            0.95
        } else {
            0.92
        };
        let programme_edge = 0.90 + federation_capacity(&name) * 0.10;
        // Persisted federation programme: chemistry and familiarity carry
        // across seasons (shared fixtures build them), so a settled side
        // plays sharper than a one-off all-star roster.
        let state = states.get(&name).cloned().unwrap_or_default();
        nations.push(Nation {
            name,
            players: idxs,
            strength: xi_overall,
            lineup: Vec::new(),
            state,
            edge: depth_edge * programme_edge,
        });
    }
    // Deterministic order: strength desc, then name asc. The HashMap above
    // iterates in per-process random order; without the name tie-break,
    // equal-strength nations would shuffle between runs and the whole
    // world (and every focal save) would be irreproducible.
    nations.sort_by(|a, b| {
        b.strength
            .partial_cmp(&a.strength)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.name.cmp(&b.name))
    });
    if wc_field {
        // 2026 allocation: hosts + confederation slots + playoffs (see
        // qualify_wc_field). The persisted federation state travels with
        // the nation so chemistry survives the qualification round-trip.
        let indexed: HashMap<String, (Vec<usize>, f32, f32, NationalState)> = nations
            .iter()
            .map(|n| {
                (
                    n.name.clone(),
                    (n.players.clone(), n.strength, n.edge, n.state.clone()),
                )
            })
            .collect();
        return qualify_wc_field(pool, &indexed, host_nations);
    }
    if !friendly {
        let cap = if euro_only { 24 } else { 32 };
        nations.truncate(cap);
    }
    nations
}

/// A no-name XI for a federation that has no real pool here (too thin to
/// field a side). These are deliberate "irrelevant regens": generically
/// competent journeymen who occasionally contain a decent player, but who
/// leave no career record — friendlies only, never the majors.
fn guest_xi(base: f32, rng: &mut Pcg64Mcg) -> Vec<MatchPlayer> {
    fn attr(center: f32, rng: &mut Pcg64Mcg) -> u8 {
        (center + rng.gen_range(-8.0..=8.0))
            .round()
            .clamp(28.0, 97.0) as u8
    }
    let shape: [(Position, usize); 4] = [
        (Position::GK, 1),
        (Position::DEF, 4),
        (Position::MID, 3),
        (Position::FWD, 3),
    ];
    let mut xi = Vec::with_capacity(11);
    for (pos, count) in shape {
        for _ in 0..count {
            let id = 1_000_000 + xi.len();
            let attrs = Attributes {
                pace: attr(base, rng),
                shooting: attr(base, rng),
                passing: attr(base, rng),
                dribbling: attr(base, rng),
                tackling: attr(base, rng),
                heading: attr(base, rng),
                strength: attr(base, rng),
                stamina: attr(base, rng),
                vision: attr(base, rng),
                composure: attr(base, rng),
                goalkeeping: attr(base, rng),
                first_touch: attr(base, rng),
            };
            let line = line_of(pos);
            let (role, _) = best_role_for(&attrs, pos, line);
            xi.push(MatchPlayer {
                id,
                pos,
                role,
                attrs,
                start_fatigue: 0.0,
                form: 1.0,
                traits: Vec::new(),
                weak_foot: 3,
                foot: crate::model::Foot::Right,
            });
        }
    }
    xi
}

/// Play one friendly: a real nation (credited) against a guest XI
/// (anonymous). The match result itself is discarded — only caps, goals
/// and ratings for the real side's pool players are kept.
fn play_guest_friendly(
    real: usize,
    nations: &[Nation],
    guest_base: f32,
    pool: &mut [WP],
    p: &Params,
    rng: &mut Pcg64Mcg,
) {
    let axi = nation_xi(&nations[real].players, pool);
    if axi.len() < 7 {
        return;
    }
    let bxi = guest_xi(guest_base, rng);
    let side_a = Side {
        players: &axi,
        ambition: 1.0,
        quality: 1.0,
        threat: 1.0,
        shape: None,
        width: 50,
        defensive_line: 50,
        bench: &[],
        crowd: 1.0,
    };
    let side_b = Side {
        players: &bxi,
        ambition: 1.0,
        quality: 1.0,
        threat: 1.0,
        shape: None,
        width: 50,
        defensive_line: 50,
        bench: &[],
        crowd: 1.0,
    };
    let (ta, tb) = matchsim::threat_ratio(&side_a, &side_b);
    let sharp = std::env::var("FM_SHARP")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(2.0);
    let mut side_a = side_a;
    let mut side_b = side_b;
    side_a.threat = ta.powf(sharp) * nations[real].edge;
    side_b.threat = tb.powf(sharp);
    let o = matchsim::simulate(&side_a, &side_b, Venue::Neutral, (0, 0), rng);
    apply_match(&o, Comp::Intl, pool, p);
}

fn nation_xi(players: &[usize], pool: &[WP]) -> Vec<MatchPlayer> {
    // Best available XI for the national side: a keeper, four defenders, three
    // midfielders and three forwards. Line quotas are respected, but inside
    // each line the player with the highest *actual overall* goes in (role fit
    // only nudges ties) — a national team fields its best eleven, it doesn't
    // average functionalism over natural quality. This is why deep-elite
    // nations look elite: France's actual XI is a killer eleven, not a
    // statistical blend.
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
            traits: Vec::new(),
            weak_foot: 3,
            foot: crate::model::Foot::Right,
        }
    };
    for (line, count) in shape {
        let mut cands: Vec<(f32, usize)> = (0..players.len())
            .filter(|&s| !used[s])
            .filter(|&s| !(line == 0 && pool[players[s]].pos != Position::GK))
            .map(|s| {
                let w = &pool[players[s]];
                // Actual overall drives national-team selection; matching the
                // line is a strong nudge, role fit only breaks ties — a
                // national team fields its best eleven, not its most
                // functional one.
                let line_bonus = if line_of(w.pos) == line { 15.0 } else { 0.0 };
                (
                    w.ovr as f32 + line_bonus + best_role_for(&w.attrs, w.pos, line).1 * 0.02,
                    s,
                )
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

/// Reserves for a national match: up to 8 more players beyond the XI, best
/// available per line (a keeper first, then the strongest unused bodies),
/// so tournaments and friendlies actually have substitutes to use.
fn nation_bench(players: &[usize], pool: &[WP], xi: &[MatchPlayer]) -> Vec<MatchPlayer> {
    let in_xi = |pi: usize| xi.iter().any(|m| m.id == pi);
    let mut rest: Vec<usize> = players.iter().copied().filter(|&pi| !in_xi(pi)).collect();
    rest.sort_by(|&a, &b| pool[b].ovr.cmp(&pool[a].ovr));
    let mut bench = Vec::with_capacity(8);
    // Line coverage first: one reserve per line in turn, then best remaining.
    for want in [0usize, 1, 2, 3] {
        for &pi in &rest {
            if bench.len() >= 8 {
                break;
            }
            if line_of(pool[pi].pos) != want {
                continue;
            }
            if bench.iter().any(|m: &MatchPlayer| m.id == pi) {
                continue;
            }
            let w = &pool[pi];
            let line = line_of(w.pos);
            let (role, _) = best_role_for(&w.attrs, w.pos, line);
            let m = 1.0 + (w.morale as f32 - 65.0) * 0.002;
            bench.push(MatchPlayer {
                id: pi,
                pos: w.pos,
                role,
                attrs: w.attrs,
                start_fatigue: (w.fatigue as f32 / 100.0).clamp(0.0, 1.0),
                form: m.max(0.6),
                traits: Vec::new(),
                weak_foot: 3,
                foot: crate::model::Foot::Right,
            });
        }
    }
    for &pi in &rest {
        if bench.len() >= 8 {
            break;
        }
        if bench.iter().any(|m: &MatchPlayer| m.id == pi) {
            continue;
        }
        let w = &pool[pi];
        let line = line_of(w.pos);
        let (role, _) = best_role_for(&w.attrs, w.pos, line);
        let m = 1.0 + (w.morale as f32 - 65.0) * 0.002;
        bench.push(MatchPlayer {
            id: pi,
            pos: w.pos,
            role,
            attrs: w.attrs,
            start_fatigue: (w.fatigue as f32 / 100.0).clamp(0.0, 1.0),
            form: m.max(0.6),
            traits: Vec::new(),
            weak_foot: 3,
            foot: crate::model::Foot::Right,
        });
    }
    bench
}

/// Shared-fixture chemistry: every match together nudges both federations'
/// chemistry and familiarity up. A settled side plays sharper than a
/// one-off all-star roster.
fn bump_chem(nations: &mut [Nation], a: usize, b: usize) {
    for &i in [a, b].iter() {
        let st = &mut nations[i].state;
        st.caps_together = st.caps_together.saturating_add(1);
        st.chemistry = (st.chemistry + 0.05).min(1.0);
        st.familiarity = (st.familiarity + 0.05).min(1.0);
    }
}

/// Build every national side in the pool for the World Cup field, then apply
/// the 2026 qualification structure: hosts (USA/CAN/MEX) auto-qualified, then
/// regional slots — UEFA 16, CAF 9, AFC 8, CONMEBOL 6, CONCACAF 3 extra,
/// OFC 1 (New Zealand when present) — and a two-team intercontinental playoff
/// (best non-qualified CAF + AFC nations). Slots unfilled when a confed has
/// no pool depth (OFC without New Zealand, thin CAF pools, etc.), so the
/// field degrades gracefully towards 48 without holes.
fn qualify_wc_field(
    _pool: &[WP],
    by_name: &HashMap<String, (Vec<usize>, f32, f32, NationalState)>,
    host_nations: &[String],
) -> Vec<Nation> {
    use crate::world_model::Confederation as C;
    let mut by_confed: HashMap<u8, Vec<(String, Vec<usize>, f32)>> = HashMap::new();
    let mut idx_of: HashMap<String, usize> = HashMap::new();
    let mut all: Vec<Nation> = Vec::new();
    // Deterministic: iterate the map in name order, not HashMap order.
    let mut sorted_names: Vec<&String> = by_name.keys().collect();
    sorted_names.sort();
    for name in sorted_names {
        let (idxs, strength, edge, state) = &by_name[name];
        let confed = crate::world_model::confed_of(name);
        let code = match confed {
            C::UEFA => 1,
            C::CAF => 2,
            C::AFC => 3,
            C::CONMEBOL => 4,
            C::CONCACAF => 5,
            C::OFC => 6,
        };
        idx_of.insert(name.clone(), all.len());
        by_confed
            .entry(code)
            .or_default()
            .push((name.clone(), idxs.clone(), *strength));
        all.push(Nation {
            name: name.clone(),
            players: idxs.clone(),
            strength: *strength,
            lineup: Vec::new(),
            state: state.clone(),
            edge: *edge,
        });
    }
    for pool in by_confed.values_mut() {
        pool.sort_by(|a, b| {
            b.2.partial_cmp(&a.2)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
    }
    // Qualification floor + combined ranking score. Pure XI-strength
    // ordering is what let FIFA-rank-~100 India (str 68.3, but only 7
    // players within 3 of XI level — a depth of a pub side) take a direct
    // AFC slot simply because the database has fewer than eight deep AFC
    // pools. Real qualification weighs world standing and squad depth, so
    // does this: qscore = XI strength, docked for FIFA-rank distance past
    // the top 55 (0.08/pt, max 6) and for a thin pool. India: 68.5 − ~3.6
    // (rank 100) ≈ 64.9; China (rank 70): 68.6 − 1.2 ≈ 67.4. A genuinely
    // stronger XI still wins on merit — the dock is a tiebreaker, not a
    // wall (see the two-tier gate below).
    // Hosts are exempt by protocol; the OFC's single slot is the world
    // champion's path — the best OFC side qualifies even if its numbers
    // are low, because there is no other OFC benchmark.
    let rank_of = |name: &str| -> i32 {
        crate::world_model::nations()
            .into_iter()
            .find(|n| n.name == name)
            .map(|n| n.ranking as i32)
            .unwrap_or(150)
    };
    let qscore = |name: &String| -> f32 {
        let (idxs, s, _, _) = &by_name[name];
        let rank = rank_of(name);
        let rank_dock = (0.08 * ((rank - 55).max(0) as f32)).min(6.0);
        let pool = idxs.len() as f32;
        let depth_dock = if pool < 18.0 {
            (18.0 - pool) * 0.3
        } else {
            0.0
        };
        *s - rank_dock - depth_dock
    };
    // World-standing gate, two tiers. In the curated ranking table every
    // known nation is < 150; unknown nations sit at 150+. A *known* rank
    // past ~85 (India 100, Thailand 107, Indonesia 123) cannot take a
    // direct regional slot even if its database XI lifts it past 65 —
    // that is the line between "AFC's 8th best on paper in a thin
    // database" and an actual World Cup side. BUT it is not a forever
    // ban: a rank-100 side whose XI genuinely reaches 70 (a World-Cup
    // calibre XI — today's 48th seed sits near 67 — earned over years of
    // federation investment, the Aditya arc) can still enter via the
    // open merit fill. Direct slots stay rank-gated; merit is
    // strength-gated for everyone.
    // Unknown-rank nations stay strength-judged, so real-but-uncurated
    // qualifiers (Venezuela etc.) are not punished for a missing entry.
    let strength_of =
        |name: &String| -> f32 { by_name.get(name).map(|(_, s, _, _)| *s).unwrap_or(0.0) };
    let direct_eligible = |name: &String| -> bool {
        if host_nations.iter().any(|h| h == name) {
            return true;
        }
        let rank = rank_of(name);
        if rank < 150 && rank > 85 {
            return false;
        }
        strength_of(name) >= 65.0
    };
    let eligible = |name: &String| -> bool {
        if host_nations.iter().any(|h| h == name) {
            return true;
        }
        let rank = rank_of(name);
        if rank < 150 && rank > 85 && strength_of(name) < 69.5 {
            return false;
        }
        strength_of(name) >= 65.0
    };
    // Re-sort each confederation's queue by combined qualification score,
    // not raw XI strength (see above).
    for pool in by_confed.values_mut() {
        pool.sort_by(|a, b| {
            qscore(&b.0)
                .partial_cmp(&qscore(&a.0))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
    }
    let mut chosen: Vec<String> = Vec::new();
    // Hosts first: they qualify automatically, inside the regional allocation.
    for h in host_nations {
        if by_name.contains_key(h) && !chosen.contains(h) {
            chosen.push(h.clone());
        }
    }
    // Regional slots by confederation. Only eligible sides (strength
    // floor) may take a direct slot — a slot nobody qualifies for stays
    // empty here and is reallocated on merit below. Exception: OFC.
    for (code, slots) in [
        (1u8, 16usize), // UEFA
        (2u8, 9),       // CAF
        (3u8, 8),       // AFC
        (4u8, 6),       // CONMEBOL
        (5u8, 3),       // CONCACAF extra (hosts already in)
        (6u8, 1),       // OFC
    ] {
        if let Some(list) = by_confed.get(&code) {
            let mut taken = 0;
            for (name, _, _) in list {
                if taken >= slots {
                    break;
                }
                if chosen.contains(name) {
                    continue;
                }
                let ok = if code == 6 {
                    true // OFC champion path
                } else {
                    direct_eligible(name)
                };
                if ok {
                    chosen.push(name.clone());
                    taken += 1;
                }
            }
        }
    }
    // Intercontinental playoffs: best non-direct CAF + AFC nations. These
    // are the classic earned-it routes (AFC's 9th-best side, the CAF
    // playoff change-up), so they use the merit gate, not direct: a
    // rank-100 federation whose XI has actually reached World-Cup class
    // (69.5+) can fight its way in through the playoff. Year one it cannot
    // — the XI is still 68-69 and stays out.
    let direct: Vec<String> = chosen.clone();
    for code in [2u8, 3u8] {
        if let Some(list) = by_confed.get(&code) {
            for (name, _, _) in list {
                if !direct.contains(name) && !chosen.contains(name) && eligible(name) {
                    chosen.push(name.clone());
                    break;
                }
            }
        }
    }
    // Merit fill: any regional slot a confederation couldn't fill is
    // reallocated to the best remaining nations worldwide by combined
    // qualification score (not raw strength — that is what kept rank-100
    // India out of the field instead of back in through the side door),
    // so the field is always the full 48 (the real rulebook's fallback).
    if chosen.len() < 48 {
        let mut rest: Vec<String> = by_name
            .iter()
            .filter(|(name, _)| !chosen.contains(*name) && eligible(name))
            .map(|(name, _)| name.clone())
            .collect();
        rest.sort_by(|a, b| {
            qscore(b)
                .partial_cmp(&qscore(a))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.cmp(b))
        });
        for name in rest.iter().take(48 - chosen.len()) {
            chosen.push(name.clone());
        }
    }
    // Last resort only: if the eligible world still cannot fill 48
    // (an artificially thin database, never a real year), backfill by
    // qscore so the tournament keeps its 48-team format. Never reached
    // when 48 eligible nations exist.
    if chosen.len() < 48 {
        let mut rest: Vec<String> = by_name
            .iter()
            .filter(|(name, _)| !chosen.contains(*name))
            .map(|(name, _)| name.clone())
            .collect();
        rest.sort_by(|a, b| {
            qscore(b)
                .partial_cmp(&qscore(a))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.cmp(b))
        });
        for name in rest.iter().take(48 - chosen.len()) {
            chosen.push(name.clone());
        }
    }

    // Forensic: near-miss audit for the two nations players always ask
    // about (stderr only when FM_DEBUG_QUAL=1, so normal runs stay quiet).
    if std::env::var("FM_DEBUG_QUAL").ok().as_deref() == Some("1") {
        for suspect in ["India", "China PR", "Qatar", "Indonesia"] {
            if let Some((_, s, _, _)) = by_name.get(suspect) {
                eprintln!(
                    "QUAL-DEBUG {} str={:.1} rank={} qscore={:.1} eligible={} direct={} chosen={}",
                    suspect,
                    *s,
                    rank_of(suspect),
                    qscore(&suspect.to_string()),
                    eligible(&suspect.to_string()),
                    direct_eligible(&suspect.to_string()),
                    chosen.contains(&suspect.to_string())
                );
            }
        }
        eprintln!("QUAL-DEBUG chosen={}/48", chosen.len());
    }

    let mut out: Vec<Nation> = Vec::new();
    for name in &chosen {
        if let Some(&i) = idx_of.get(name) {
            out.push(all[i].clone());
        }
    }
    out.sort_by(|a, b| {
        b.strength
            .partial_cmp(&a.strength)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.name.cmp(&b.name))
    });
    out
}

/// Bid vote for the next World Cup's hosting, run after each tournament for
/// the next race four years hence. FIFA-style committee voting: strong,
/// well-resourced federations weigh more; recently-hosted nations are
/// disqualified (rotation policy); winners occasionally bring co-hosts from
/// their own confederation (the modern multi-country format — Spain/Portugal
/// style bundles emerge here too). The hosts picked get an automatic slot and
/// the home-crowd edge during the tournament.
fn decide_next_hosts(w: &mut World, rng: &mut Pcg64Mcg) -> Vec<String> {
    let mut nations = build_nations(&w.pool, IntlKind::None, &[], false, &w.national_states);
    nations.sort_by(|a, b| {
        b.strength
            .partial_cmp(&a.strength)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Candidates: eligible nations (not on the recent-hosts cooldown).
    let mut cand: Vec<&Nation> = nations
        .iter()
        .filter(|n| !w.recent_hosts.contains(&n.name))
        .collect();
    if cand.is_empty() {
        return vec!["United States".into(), "Canada".into(), "Mexico".into()];
    }
    cand.truncate(12);
    // Weighted committee vote (rank-weighted randomness layered on strength).
    let mut weights: Vec<f32> = cand
        .iter()
        .map(|n| (n.strength * n.strength * (1.0 + rng.gen_range(0.0..0.6))).max(1.0))
        .collect();
    let winner_idx = weighted_pick_idx(&weights, rng);
    let mut hosts = vec![cand[winner_idx].name.clone()];
    // Feasibility: a winner whose pool is thin gets bumped in favor of the
    // runner-up (the committee guards the stage).
    if hosts[0].trim().len() < 3 {
        hosts[0] = cand[(winner_idx + 1) % cand.len()].name.clone();
    }
    // Co-hosts: the modern world wants multi-country tournaments about a
    // third of the time — same-confederation shortlist partners join in.
    if rng.gen::<f32>() < 0.35 && cand.len() > 1 {
        let winner_confed = crate::world_model::confed_of(&hosts[0]);
        let mut partners: Vec<&Nation> = cand
            .iter()
            .copied()
            .filter(|n| {
                n.name != hosts[0] && crate::world_model::confed_of(&n.name) == winner_confed
            })
            .collect();
        partners.sort_by(|a, b| {
            b.strength
                .partial_cmp(&a.strength)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut rng2 = rng;
        for p in partners.into_iter().take(2) {
            if rng2.gen::<f32>() < 0.6 {
                hosts.push(p.name.clone());
            }
        }
        let _ = &mut weights;
    }
    hosts
}

fn weighted_pick_idx(weights: &[f32], rng: &mut Pcg64Mcg) -> usize {
    let tot: f32 = weights.iter().sum();
    let mut r = rng.gen::<f32>() * tot.max(0.01);
    for (i, &w) in weights.iter().enumerate() {
        if r < w {
            return i;
        }
        r -= w;
    }
    weights.len().saturating_sub(1)
}

/// The 2026 FIFA World Cup format (48 teams):
/// - 12 groups of 4, pot-seeded (Pot 1 = hosts + top 9 by nation strength),
///   one team per pot per group and confederation diversity (≤2 UEFA per
///   group; others ≤1 where the field allows).
/// - Top 2 per group + the 8 best third-placed teams advance (32 through).
/// - Knockout: Round of 32 → Round of 16 → QF → SF → Final (with a
///   third-place playoff), single legs — the champion plays 8 matches
///   (3 group + 5 knockouts), exactly like the real thing.
/// Tie-breakers: points, then goal difference, then goals scored, then final
/// FIFA ranking (nation strength here) — head-to-head and fair play are
/// folded in via goals/strength, a deliberate simplification documented here.
fn run_world_cup_2026(
    year: u16,
    nations: &mut [Nation],
    hosts: &[String],
    pool: &mut [WP],
    p: &Params,
    rng: &mut Pcg64Mcg,
    mut sink: IntlSink,
) -> Option<String> {
    use crate::world_model::Confederation as C;
    let n = nations.len();
    if n < 24 {
        return None;
    }
    // Forensic log: the tournament's full audit trail — seeded field (with
    // pool depth and champion-drag edge), every knockout result, the final.
    // Written to sim_dump/wc_{year}.log so any "how did X win?" question is
    // answerable from data, not speculation.
    let mut wclog: Vec<String> = Vec::new();
    wclog.push(format!("== World Cup {} ==", year));
    wclog.push(format!("hosts: {}", hosts.join(", ")));
    // Tournament-only per-player accumulator (fresh each World Cup).
    let mut wc_agg: HashMap<usize, WcAgg> = HashMap::new();
    // Order by strength, split into 4 pots (hosts first in pot 1 by strength of
    // their XI — the three hosts are bottom-of-pot-1? in the real structure
    // Pot 1 contains the hosts regardless; ordering by strength within pot 1.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        nations[b]
            .strength
            .partial_cmp(&nations[a].strength)
            .unwrap()
    });
    for (k, &i) in order.iter().enumerate() {
        let xi_level = nations[i].strength as i32;
        let depth = pool
            .iter()
            .filter(|p| !p.retired && p.nation == nations[i].name && p.ovr as i32 >= xi_level - 3)
            .count();
        wclog.push(format!(
            "{:>2}. {:<24} str={:5.1} depth={:<3} edge={:.3}",
            k + 1,
            nations[i].name,
            nations[i].strength,
            depth,
            nations[i].edge
        ));
        // Show the actual selected lineup (shape quotas + bench) the way a
        // real match sheet reads, not the top-11 by raw overall.
        let xi = nation_xi(&nations[i].players, pool);
        let bench = nation_bench(&nations[i].players, pool, &xi);
        let pos_tag = |mp: &MatchPlayer| match mp.pos {
            crate::model::Position::GK => "GK",
            crate::model::Position::DEF => "DF",
            crate::model::Position::MID => "MF",
            crate::model::Position::FWD => "FW",
        };
        let roster = xi
            .iter()
            .map(|mp| format!("{}({} {})", pool[mp.id].name, pos_tag(mp), pool[mp.id].ovr))
            .collect::<Vec<_>>()
            .join(", ");
        wclog.push(format!("    XI: {}", roster));
        let subs = bench
            .iter()
            .map(|mp| format!("{}({} {})", pool[mp.id].name, pos_tag(mp), pool[mp.id].ovr))
            .collect::<Vec<_>>()
            .join(", ");
        if !subs.is_empty() {
            wclog.push(format!("    SUBS: {}", subs));
        }
    }
    let pot_size = (n / 4).max(1);
    let mut pots: Vec<Vec<usize>> = vec![Vec::new(); 4];
    // Hosts go in Pot 1 (per the real protocol), ranked by strength inside it.
    let mut ordered: Vec<usize> = Vec::new();
    for &i in &order {
        if hosts.iter().any(|h| nations[i].name == *h) {
            ordered.push(i);
        }
    }
    for &i in &order {
        if !hosts.iter().any(|h| nations[i].name == *h) {
            ordered.push(i);
        }
    }
    for (k, &i) in ordered.iter().enumerate() {
        pots[(k / pot_size).min(3)].push(i);
    }
    let groups_n = (n / 4).max(1);
    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); groups_n];
    // Greedy pot-by-pot placement with confederation diversity. The draw
    // itself is random within each pot (as in real draws): without the
    // shuffle, every year seeded the same and every strong side took the
    // same group stage down to the last group.
    for poti in 0..4 {
        let mut pot = pots[poti].clone();
        pot.shuffle(rng);
        for &t in &pot {
            let my_confed = crate::world_model::confed_of(&nations[t].name);
            let seen = |c: C, g: &[usize]| {
                g.iter()
                    .filter(|&&x| crate::world_model::confed_of(&nations[x].name) == c)
                    .count()
            };
            let mut best: Option<usize> = None;
            for gi in 0..groups.len() {
                let g = &groups[gi];
                let ok = match my_confed {
                    C::UEFA => seen(C::UEFA, g) < 2,
                    _ => seen(my_confed, g) == 0,
                };
                if ok && (best.is_none() || groups[best.unwrap()].len() > g.len()) {
                    best = Some(gi);
                }
            }
            let gid = best.unwrap_or_else(|| {
                groups
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, g)| g.len())
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            });
            groups[gid].push(t);
        }
    }
    // Tidy degenerate single-member groups in case of small fields.
    groups.retain(|g| !g.is_empty());

    // Group stage: round-robin (3/1/0), rank by points → GD → GF → strength.
    let mut advancers: Vec<usize> = Vec::new();
    let mut thirds: Vec<(i32, i32, u32, f32, usize)> = Vec::new();
    // Host crowd edge as a plain fn (no closure capture) so the WC loop
    // can mutate nation chemistry between fixtures.
    fn host_crowd(team: &Nation, hosts: &[String]) -> f32 {
        if hosts.iter().any(|h| team.name == *h) {
            1.18 // home crowd at a hosted tournament
        } else {
            1.0
        }
    }
    for g in &groups {
        let mut pts: HashMap<usize, i32> = g.iter().map(|&t| (t, 0)).collect();
        let mut gd: HashMap<usize, i32> = g.iter().map(|&t| (t, 0)).collect();
        let mut gf: HashMap<usize, u32> = g.iter().map(|&t| (t, 0)).collect();
        for a in 0..g.len() {
            for b in (a + 1)..g.len() {
                let ca = host_crowd(&nations[g[a]], hosts);
                let cb = host_crowd(&nations[g[b]], hosts);
                let (hg, ag, wstats, outcome) = play_nation_crowd(g[a], g[b], nations, pool, p, rng, ca, cb);
                if let Some(f) = sink.as_mut() {
                    f(outcome, &nations[g[a]].name, &nations[g[b]].name, "World Cup", "Group Stage");
                }
                absorb_wc_stats(&wstats, &mut wc_agg);
                for s in &wstats {
                    if s.id < pool.len() && pool[s.id].name.contains("Messi") {
                        let opp = if s.side_a {
                            nations[g[b]].name.clone()
                        } else {
                            nations[g[a]].name.clone()
                        };
                        wclog.push(format!(
                            "  MESSI vs {} [GS {}-{}]: mins={} G={} A={} shots={} onT={} xG={:.2} KP={} BCC={} passes={}/{} dribbles={}/{} tackles={} INT={} fouls={} rating={:.2}",
                            opp, hg, ag, s.minutes, s.goals, s.assists, s.shots,
                            s.shots_on_target, s.xg, s.key_passes,
                            s.big_chances_created, s.passes_cmp, s.passes_att,
                            s.dribbles_cmp, s.dribbles_att, s.tackles,
                            s.interceptions, s.fouls, s.rating
                        ));
                    }
                }
                bump_chem(nations, g[a], g[b]);
                wclog.push(format!(
                    "GS {} {}-{} {}",
                    nations[g[a]].name, hg, ag, nations[g[b]].name
                ));
                *gd.get_mut(&g[a]).unwrap() += hg as i32 - ag as i32;
                *gd.get_mut(&g[b]).unwrap() += ag as i32 - hg as i32;
                *gf.get_mut(&g[a]).unwrap() += hg;
                *gf.get_mut(&g[b]).unwrap() += ag;
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
        ranked.sort_by(|x, y| {
            pts[y]
                .cmp(&pts[x])
                .then(gd[y].cmp(&gd[x]))
                .then(gf[y].cmp(&gf[x]))
                .then(
                    nations[*y]
                        .strength
                        .partial_cmp(&nations[*x].strength)
                        .unwrap(),
                )
        });
        wclog.push(format!(
            "GROUP: {}",
            ranked
                .iter()
                .map(|&t| format!("{} {}pts {:+}gd", nations[t].name, pts[&t], gd[&t]))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
        for (k, &t) in ranked.iter().enumerate() {
            if k < 2 {
                advancers.push(t);
            } else if k == 2 {
                thirds.push((pts[&t], gd[&t], gf[&t], nations[t].strength, t));
            }
        }
    }
    // Best 8 third-placed teams (points → GD → GF → strength).
    thirds.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.cmp(&a.1))
            .then(b.2.cmp(&a.2))
            .then(b.3.partial_cmp(&a.3).unwrap())
    });
    for (_, _, _, _, t) in thirds.iter().take(8) {
        advancers.push(*t);
    }
    for &t in &advancers {
        for &pi in &nations[t].players {
            if pool[pi].intl_result < 1 {
                pool[pi].intl_result = 1;
            }
        }
    }
    // Seeded knockout bracket (R32 → RO16 → QF → SF → Final): top seed meets
    // the bottom seed first, winners advance in bracket order — no shuffle.
    let mut sorted = advancers.clone();
    sorted.sort_by(|&a, &b| {
        nations[b]
            .strength
            .partial_cmp(&nations[a].strength)
            .unwrap()
    });
    let mut bracket: Vec<usize> = Vec::with_capacity(sorted.len());
    let half = sorted.len() / 2;
    // Cross the seeds (strongest vs weakest) but randomize which weak side
    // each strong side draws — otherwise the same two nations meet in the
    // same round every single cycle.
    let mut bottom: Vec<usize> = sorted.iter().skip(sorted.len() - half).copied().collect();
    bottom.shuffle(rng);
    for i in 0..half {
        bracket.push(sorted[i]);
        bracket.push(bottom[i]);
    }
    if sorted.len() % 2 == 1 {
        bracket.push(sorted[half]);
    }
    let mut round = bracket;
    let mut tag = 2u8;
    let mut sf_losers: Vec<usize> = Vec::new();
    while round.len() > 1 {
        let is_final = round.len() == 2;
        let is_semi = round.len() == 4;
        let round_label = match round.len() {
            32 => "R32",
            16 => "R16",
            8 => "QF",
            4 => "SF",
            2 => "F",
            _ => "KO",
        };
        let mut next = Vec::new();
        // Odd brackets (a degraded field) must never silently drop a
        // seeded team: the strongest remaining side gets a logged bye.
        let mut i = 0;
        if round.len() % 2 == 1 {
            let bye = round[0];
            next.push(bye);
            wclog.push(format!("BYE {}", nations[bye].name));
            i = 1;
        }
        while i + 1 < round.len() {
            let (a, b) = (round[i], round[i + 1]);
            let ca = host_crowd(&nations[a], hosts);
            let cb = host_crowd(&nations[b], hosts);
            let (ga, gb, wstats, outcome) = play_nation_crowd(a, b, nations, pool, p, rng, ca, cb);
            {
                let stage_full = match round_label {
                    "R32" => "Round of 32",
                    "R16" => "Round of 16",
                    "QF" => "Quarter-final",
                    "SF" => "Semi-final",
                    "F" => "Final",
                    _ => "Knockout",
                };
                if let Some(f) = sink.as_mut() {
                    f(outcome, &nations[a].name, &nations[b].name, "World Cup", stage_full);
                }
            }
            absorb_wc_stats(&wstats, &mut wc_agg);
            for s in &wstats {
                if s.id < pool.len() && pool[s.id].name.contains("Messi") {
                    let opp = if s.side_a {
                        nations[b].name.clone()
                    } else {
                        nations[a].name.clone()
                    };
                    wclog.push(format!(
                        "  MESSI vs {} [{} {}-{}]: mins={} G={} A={} shots={} onT={} xG={:.2} KP={} BCC={} passes={}/{} dribbles={}/{} tackles={} INT={} fouls={} rating={:.2}",
                        opp, round_label, ga, gb, s.minutes, s.goals, s.assists,
                        s.shots, s.shots_on_target, s.xg, s.key_passes,
                        s.big_chances_created, s.passes_cmp, s.passes_att,
                        s.dribbles_cmp, s.dribbles_att, s.tackles, s.interceptions,
                        s.fouls, s.rating
                    ));
                }
            }
            bump_chem(nations, a, b);
            let (win, lose) = if ga > gb {
                (a, b)
            } else if gb > ga {
                (b, a)
            } else {
                // Level after 90': a shootout is nerve, not class — keeper
                // quality and composure under pressure decide it. A pure
                // strength coin let disciplined grinding sides farm drawn
                // ties forever (6-for-6 across half a century in testing);
                // the pitch's eleven should settle it, exactly like the
                // Euros path.
                let axi = nation_xi(&nations[a].players, pool);
                let bxi = nation_xi(&nations[b].players, pool);
                let nerve = |xi: &[MatchPlayer]| -> f32 {
                    xi.iter()
                        .map(|mp| {
                            mp.attrs.goalkeeping as f32 * 0.3 + mp.attrs.composure as f32 * 0.7
                        })
                        .sum::<f32>()
                };
                let (na, nb) = (nerve(&axi), nerve(&bxi));
                let pa = na / (na + nb).max(1.0);
                if rng.gen::<f32>() < pa {
                    (a, b)
                } else {
                    (b, a)
                }
            };
            wclog.push(format!(
                "{} {} {}-{} {} -> {}",
                round_label, nations[a].name, ga, gb, nations[b].name, nations[win].name
            ));
            if is_semi {
                sf_losers.push(lose);
            }
            for &pi in &nations[win].players {
                let r = if is_final { 5 } else { tag };
                if pool[pi].intl_result < r {
                    pool[pi].intl_result = r;
                }
            }
            if is_final {
                if win == a {
                    for &pi in &nations[b].players {
                        pool[pi].intl_result = pool[pi].intl_result.max(4);
                    }
                } else {
                    for &pi in &nations[a].players {
                        pool[pi].intl_result = pool[pi].intl_result.max(4);
                    }
                }
            }
            next.push(win);
            i += 2;
        }
        round = next;
        tag += 1;
    }
    // Third-place playoff between the two semifinal losers.
    if sf_losers.len() == 2 {
        let [third, fourth] = [sf_losers[0], sf_losers[1]];
        let ca = host_crowd(&nations[third], hosts);
        let cb = host_crowd(&nations[fourth], hosts);
        let (g1, g2, wstats, outcome) = play_nation_crowd(third, fourth, nations, pool, p, rng, ca, cb);
        if let Some(f) = sink.as_mut() {
            f(outcome, &nations[third].name, &nations[fourth].name, "World Cup", "Third-place Play-off");
        }
        absorb_wc_stats(&wstats, &mut wc_agg);
        for s in &wstats {
            if s.id < pool.len() && pool[s.id].name.contains("Messi") {
                let opp = if s.side_a {
                    nations[fourth].name.clone()
                } else {
                    nations[third].name.clone()
                };
                wclog.push(format!(
                    "  MESSI vs {} [3rd {}-{}]: mins={} G={} A={} shots={} onT={} xG={:.2} KP={} BCC={} passes={}/{} dribbles={}/{} tackles={} INT={} fouls={} rating={:.2}",
                    opp, g1, g2, s.minutes, s.goals, s.assists, s.shots,
                    s.shots_on_target, s.xg, s.key_passes, s.big_chances_created,
                    s.passes_cmp, s.passes_att, s.dribbles_cmp, s.dribbles_att,
                    s.tackles, s.interceptions, s.fouls, s.rating
                ));
            }
        }
        bump_chem(nations, third, fourth);
        wclog.push(format!(
            "3rd {} {}-{} {}",
            nations[third].name, g1, g2, nations[fourth].name
        ));
        for &pi in &nations[third].players {
            if g1 >= g2 {
                pool[pi].intl_result = pool[pi].intl_result.max(3);
            } else {
                pool[pi].intl_result = pool[pi].intl_result.max(2);
            }
        }
        for &pi in &nations[fourth].players {
            if g2 >= g1 {
                pool[pi].intl_result = pool[pi].intl_result.max(3);
            } else {
                pool[pi].intl_result = pool[pi].intl_result.max(2);
            }
        }
    }
    // ── Tournament player summary (auditable, WC-only, no friendlies) ──
    // Messi first, in full — the 39-year-old Indian-summer use case.
    {
        let mut leaders: Vec<(&usize, &WcAgg)> =
            wc_agg.iter().filter(|(_, a)| a.apps > 0).collect();
        leaders.sort_by(|a, b| {
            b.1.goals
                .cmp(&a.1.goals)
                .then(b.1.assists.cmp(&a.1.assists))
                .then(b.1.apps.cmp(&a.1.apps))
        });
        for (id, a) in &leaders {
            if **id < pool.len() && pool[**id].name.contains("Messi") {
                let avg = if a.apps > 0 {
                    a.rating_sum / a.apps as f32
                } else {
                    0.0
                };
                let pass_pct = if a.passes_att > 0 {
                    100.0 * a.passes_cmp as f32 / a.passes_att as f32
                } else {
                    0.0
                };
                wclog.push(format!(
                    "MESSI WC {} FINAL: apps={} G={} A={} shots={} onT={} xG={:.2} KP={} BCC={} passes={}/{} ({:.1}%) dribbles={}/{} tackles={} INT={} fouls={} avg_rating={:.2} ovr={} age={} peak_seasons={}",
                    year,
                    a.apps,
                    a.goals,
                    a.assists,
                    a.shots,
                    a.sot,
                    a.xg,
                    a.kp,
                    a.bcc,
                    a.passes_cmp,
                    a.passes_att,
                    pass_pct,
                    a.dribbles_cmp,
                    a.dribbles_att,
                    a.tackles,
                    a.interceptions,
                    a.fouls,
                    avg,
                    pool[**id].ovr,
                    pool[**id].age,
                    pool[**id].peak
                ));
            }
        }
        // Full Argentina squad table (everyone who played a WC minute).
        let arg_players: Vec<usize> = pool
            .iter()
            .enumerate()
            .filter(|(i, p)| p.nation == "Argentina" && wc_agg.contains_key(i))
            .map(|(i, _)| i)
            .collect();
        if !arg_players.is_empty() {
            wclog.push("--- Argentina WC squad (tournament-only) ---".to_string());
            let mut rows: Vec<(usize, &WcAgg)> =
                arg_players.iter().map(|&i| (i, &wc_agg[&i])).collect();
            rows.sort_by(|a, b| b.1.apps.cmp(&a.1.apps).then(b.1.goals.cmp(&a.1.goals)));
            for (id, a) in rows {
                let avg = if a.apps > 0 {
                    a.rating_sum / a.apps as f32
                } else {
                    0.0
                };
                wclog.push(format!(
                    "  {} (ovr {} age {} peak {}): apps={} G={} A={} KP={} BCC={} avg={:.2}",
                    pool[id].name,
                    pool[id].ovr,
                    pool[id].age,
                    pool[id].peak,
                    a.apps,
                    a.goals,
                    a.assists,
                    a.kp,
                    a.bcc,
                    avg
                ));
            }
        }
        // Golden Boot (tournament) top 5, for context.
        wclog.push("--- WC scoring leaders (tournament-only) ---".to_string());
        for (id, a) in leaders.iter().take(5) {
            if **id < pool.len() {
                wclog.push(format!(
                    "  {} ({}): apps={} G={} A={}",
                    pool[**id].name, pool[**id].nation, a.apps, a.goals, a.assists
                ));
            }
        }
    }
    let champion = round.first().map(|&t| nations[t].name.clone());
    if let Some(c) = &champion {
        wclog.push(format!("CHAMPION: {}", c));
    }
    let _ = std::fs::create_dir_all("sim_dump");
    let _ = std::fs::write(format!("sim_dump/wc_{}.log", year), wclog.join("\n"));
    champion
}

/// An international fixture, now played through the same possession engine as
/// club football (neutral venue, single leg). Nation quality separates strong
/// sides from weak ones through the eleven on the pitch, so cup runs are less
/// of a lottery than the old strength-swing draw. `crowd_a`/`crowd_b` scale
/// each side for the home crowd: 1.0 neutral, ~1.15 for a host nation playing
/// in front of its own people — the edge real hosts ride all tournament.
fn play_nation(
    a: usize,
    b: usize,
    nations: &[Nation],
    pool: &mut [WP],
    p: &Params,
    rng: &mut Pcg64Mcg,
) -> (u32, u32, Vec<matchsim::MatchStats>, matchsim::MatchOutcome) {
    return play_nation_crowd(a, b, nations, pool, p, rng, 1.0, 1.0);
}

fn play_nation_crowd(
    a: usize,
    b: usize,
    nations: &[Nation],
    pool: &mut [WP],
    p: &Params,
    rng: &mut Pcg64Mcg,
    crowd_a: f32,
    crowd_b: f32,
) -> (u32, u32, Vec<matchsim::MatchStats>, matchsim::MatchOutcome) {
    let axi = nation_xi(&nations[a].players, pool);
    let bxi = nation_xi(&nations[b].players, pool);
    if axi.len() < 7 || bxi.len() < 7 {
        return (
            0,
            0,
            Vec::new(),
            matchsim::MatchOutcome::default(),
        );
    }
    // Real squads, not bare elevens: reserves take the bench so
    // substitutions and rotation actually happen.
    let abench = nation_bench(&nations[a].players, pool, &axi);
    let bbench = nation_bench(&nations[b].players, pool, &bxi);
    let chem = |i: usize| 0.95 + 0.10 * nations[i].state.chemistry;
    let mut side_a = Side {
        players: &axi,
        ambition: 1.0,
        quality: 1.0,
        threat: 1.0,
        shape: None,
        width: 50,
        defensive_line: 50,
        bench: &abench,
        crowd: 1.0,
    };
    let mut side_b = Side {
        players: &bxi,
        ambition: 1.0,
        quality: 1.0,
        threat: 1.0,
        shape: None,
        width: 50,
        defensive_line: 50,
        bench: &bbench,
        crowd: 1.0,
    };
    let (ta, tb) = matchsim::threat_ratio(&side_a, &side_b);
    // Nations meet too rarely for ordinary league variance: a 7-overall gap
    // between two squads of pooled national pros is worth far more in a final
    // than mid-season form suggests. Sharpen the gap (×1.5 on the ratio) so
    // the class difference is priced — favorites become *favorites* (70%+),
    // while a proper minnow run still needs repeated, genuinely lucky nights.
    let sharp = std::env::var("FM_SHARP")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(2.0);
    // `edge` is the tournament-sharpness multiplier (defending-champion drag,
    // federation-programme form). It scales how the same eleven performs on
    // the day without touching their underlying class.
    side_a.threat = ta.powf(sharp) * crowd_a * nations[a].edge * chem(a);
    side_b.threat = tb.powf(sharp) * crowd_b * nations[b].edge * chem(b);
    let o = matchsim::simulate(&side_a, &side_b, Venue::Neutral, (0, 0), rng);
    let stats = o.stats.clone();
    apply_match(&o, Comp::Intl, pool, p);
    (o.goals_a as u32, o.goals_b as u32, stats, o)
}

fn run_international(
    nations: &mut [Nation],
    pool: &mut [WP],
    p: &Params,
    rng: &mut Pcg64Mcg,
    label: &str,
    mut sink: IntlSink,
) -> Option<String> {
    let n = nations.len();
    if n < 4 {
        return None;
    }
    let groups_n = (n / 4).max(2);
    let mut seeded: Vec<usize> = (0..n).collect();
    seeded.sort_by(|&a, &b| {
        nations[b]
            .strength
            .partial_cmp(&nations[a].strength)
            .unwrap()
    });
    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); groups_n];
    for (i, &t) in seeded.iter().enumerate() {
        groups[i % groups_n].push(t);
    }
    let mut advancing: Vec<usize> = Vec::new();
    let mut thirds: Vec<(i32, i32, f32, usize)> = Vec::new();
    for g in &groups {
        let mut pts: HashMap<usize, i32> = g.iter().map(|&t| (t, 0)).collect();
        let mut gd: HashMap<usize, i32> = g.iter().map(|&t| (t, 0)).collect();
        for a in 0..g.len() {
            for b in (a + 1)..g.len() {
                let (hg, ag, _, outcome) = play_nation(g[a], g[b], nations, pool, p, rng);
                if let Some(f) = sink.as_mut() {
                    f(outcome, &nations[g[a]].name, &nations[g[b]].name, label, "Group Stage");
                }
                bump_chem(nations, g[a], g[b]);
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
        for (k, &t) in ranked.iter().enumerate() {
            if k < 2 {
                advancing.push(t);
            } else if k == 2 {
                thirds.push((pts[&t], gd[&t], nations[t].strength, t));
            }
        }
    }
    // Fill the bracket to the next power of two with the best
    // third-placed teams (the real Euro format: 12 + 4 thirds = 16).
    thirds.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.cmp(&a.1))
            .then(b.2.partial_cmp(&a.2).unwrap())
    });
    let target = advancing.len().next_power_of_two();
    if advancing.len() < target {
        advancing.extend(thirds.iter().take(target - advancing.len()).map(|x| x.3));
    }
    for &t in &advancing {
        for &pi in &nations[t].players {
            if pool[pi].intl_result < 1 {
                pool[pi].intl_result = 1;
            }
        }
    }
    // Seeded bracket, not a shuffle: group winners/runners-up are ranked by
    // final group position (winners ahead of runners-up, then by strength) and
    // crossed as 1v16, 8v9, 4v13, 5v12… so the top seeds meet the bottom of
    // the field first and the strongest nations can actually reach finals.
    // Random draws flat-out rewarded minnows with straight-to-the-final luck
    // paths; a seeded bracket is what makes France vs Greece a fair coin of
    // outcomes instead of Greece marching through six coin flips.
    let mut bracket: Vec<usize> = Vec::with_capacity(advancing.len());
    let mut sorted = advancing.clone();
    sorted.sort_by(|&a, &b| {
        nations[b]
            .strength
            .partial_cmp(&nations[a].strength)
            .unwrap()
    });
    let half = sorted.len() / 2;
    let mut bottom: Vec<usize> = sorted.iter().skip(sorted.len() - half).copied().collect();
    bottom.shuffle(rng);
    for i in 0..half {
        bracket.push(sorted[i]);
        bracket.push(bottom[i]);
    }
    if sorted.len() % 2 == 1 {
        bracket.push(sorted[half]);
    }
    let mut round = bracket;
    let mut tag = 2u8;
    while round.len() > 1 {
        let mut next = Vec::new();
        // Degraded odd brackets give the top remaining seed a bye
        // (logged nowhere but never dropped silently).
        let mut i = 0;
        if round.len() % 2 == 1 {
            next.push(round[0]);
            i = 1;
        }
        while i + 1 < round.len() {
            let (a, b) = (round[i], round[i + 1]);
            let (ga, gb, _, outcome) = play_nation(a, b, nations, pool, p, rng);
            let stage_label = match round.len() {
                2 => "Final",
                4 => "Semi-final",
                8 => "Quarter-final",
                16 => "Round of 16",
                32 => "Round of 32",
                _ => "Knockout",
            };
            if let Some(f) = sink.as_mut() {
                f(outcome, &nations[a].name, &nations[b].name, label, stage_label);
            }
            bump_chem(nations, a, b);
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
        let belite = w
            .pool
            .iter()
            .find(|p| p.name == o.ballon.0)
            .map(|p| p.goals >= 20 && (p.league_won || p.ucl_result >= 2 || p.intl_result >= 3))
            .unwrap_or(false);
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

pub fn simulate_full_season(db: &Database, seed: u64) -> SeasonReport {
    let mut rng = Pcg64Mcg::new(seed as u128 | 1);
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
    // Same Big-5-only filter as the award itself (run_one_season); sorting
    // the whole pool here used to leak Saudi/MLS scorers into the table.
    let mut boot: Vec<usize> = (0..w.pool.len())
        .filter(|&i| {
            w.pool[i].team < w.teams.len() && w.teams[w.pool[i].team].league_idx < BIG5.len()
        })
        .collect();
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
    let elig: Vec<usize> = (0..w.pool.len())
        .filter(|&i| w.pool[i].rating_apps > 0)
        .collect();
    let outfield: Vec<usize> = elig
        .iter()
        .copied()
        .filter(|&i| w.pool[i].pos != Position::GK)
        .collect();
    let elig_pass: Vec<usize> = outfield
        .iter()
        .copied()
        .filter(|&i| w.pool[i].passes_att >= 600)
        .collect();
    let elig_rate: Vec<usize> = elig
        .iter()
        .copied()
        .filter(|&i| w.pool[i].rating_apps >= 15)
        .collect();
    let gks: Vec<usize> = elig
        .iter()
        .copied()
        .filter(|&i| w.pool[i].pos == Position::GK)
        .collect();
    top10(
        &mut lines,
        "Goals",
        "G (A)",
        &w.pool,
        &w.teams,
        &elig,
        |p| p.overall_goals() as f32,
        |p| format!("{} ({})", p.overall_goals(), p.overall_assists()),
    );
    top10(
        &mut lines,
        "Assists",
        "A (G)",
        &w.pool,
        &w.teams,
        &elig,
        |p| p.overall_assists() as f32,
        |p| format!("{} ({})", p.overall_assists(), p.overall_goals()),
    );
    top10(
        &mut lines,
        "Big chances created",
        "BCC",
        &w.pool,
        &w.teams,
        &elig,
        |p| p.big_chances as f32,
        |p| format!("{}", p.big_chances),
    );
    top10(
        &mut lines,
        "Key passes",
        "KP",
        &w.pool,
        &w.teams,
        &elig,
        |p| p.key_passes as f32,
        |p| format!("{}", p.key_passes),
    );
    top10(
        &mut lines,
        "Dribbles completed",
        "Drb",
        &w.pool,
        &w.teams,
        &elig,
        |p| p.dribbles_cmp as f32,
        |p| format!("{}", p.dribbles_cmp),
    );
    top10(
        &mut lines,
        "Tackles",
        "Tkl",
        &w.pool,
        &w.teams,
        &elig,
        |p| p.tackles as f32,
        |p| format!("{}", p.tackles),
    );
    top10(
        &mut lines,
        "Interceptions",
        "Int",
        &w.pool,
        &w.teams,
        &elig,
        |p| p.interceptions as f32,
        |p| format!("{}", p.interceptions),
    );
    top10(
        &mut lines,
        "Pass completion (outfield, ≥600 passes)",
        "Cmp%",
        &w.pool,
        &w.teams,
        &elig_pass,
        |p| p.passes_cmp as f32 / p.passes_att.max(1) as f32,
        |p| {
            format!(
                "{:.1}% ({}/{})",
                p.passes_cmp as f32 / p.passes_att.max(1) as f32 * 100.0,
                p.passes_cmp,
                p.passes_att
            )
        },
    );
    top10(
        &mut lines,
        "Goalkeeper saves",
        "Sv",
        &w.pool,
        &w.teams,
        &gks,
        |p| p.saves as f32,
        |p| format!("{} ({} clean sheets)", p.saves, p.clean_sheets),
    );
    top10(
        &mut lines,
        "Clean sheets",
        "CS",
        &w.pool,
        &w.teams,
        &elig,
        |p| p.clean_sheets as f32,
        |p| format!("{}", p.clean_sheets),
    );
    top10(
        &mut lines,
        "Average rating (≥15 apps)",
        "Avg",
        &w.pool,
        &w.teams,
        &elig_rate,
        |p| p.avg_rating(),
        |p| format!("{:.2}", p.avg_rating()),
    );

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
    lines.push(format!(
        "- Champions League: **{}** (top scorer {} {}g)",
        out.ucl_winner, out.ucl_top.0, out.ucl_top.1
    ));
    lines.push(format!("- Europa League: **{}**", out.europa_winner));
    lines.push(format!("- AFC Champions League: **{}**", out.afc_winner));
    lines.push(format!(
        "- CONCACAF Champions Cup: **{}**",
        out.concacaf_winner
    ));
    lines.push(format!(
        "- CONMEBOL Libertadores: **{}**",
        out.conmebol_winner
    ));
    if out.club_world_cup_winner != "-" {
        lines.push(format!(
            "- FIFA Club World Cup: **{}**",
            out.club_world_cup_winner
        ));
    }
    lines.push(String::new());

    // Non-UEFA top flights: their own tables, printed right after the Big 5.
    for li in (BIG5.len() + 1)..w.league_teams.len() {
        let table = rank(&w.league_teams[li], &w.teams);
        if table.is_empty() {
            continue;
        }
        // The CSV files most CONMEBOL clubs under the continent's cup label;
        // as a domestic table that must not read as a cup, so display the
        // group as a premier division (logic and lookups are untouched).
        let label = if w.league_meta[li].label == "CONMEBOL Libertadores" {
            "CONMEBOL Premier Division"
        } else {
            w.league_meta[li].label.as_str()
        };
        lines.push(format!("## {}", label));
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
        lines.push(String::new());
    }

    // UCL scoring chart with appearances, so a 6-goal tally over 15 games reads
    // differently from 6 in 8 — the rate is the story, not just the total.
    lines.push("### Champions League scoring chart".to_string());
    lines.push(String::new());
    lines.push("| # | Player | Pos | Club | G | Apps |".to_string());
    lines.push("|--:|--------|:--:|------|:-:|:----:|".to_string());
    let mut ucl: Vec<usize> = (0..w.pool.len())
        .filter(|&i| w.pool[i].ucl_goals > 0)
        .collect();
    ucl.sort_by(|&a, &b| {
        w.pool[b]
            .ucl_goals
            .cmp(&w.pool[a].ucl_goals)
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
        let hg = tm
            .registered
            .iter()
            .filter(|&&i| w.pool[i].homegrown)
            .count();
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
        IntlKind::AsianCup => "AFC Asian Cup",
        IntlKind::CopaAmerica => "Copa América",
        IntlKind::Afcon => "Africa Cup of Nations",
        IntlKind::GoldCup => "CONCACAF Gold Cup",
        IntlKind::None => "No major tournament",
    };
    lines.push(format!("## International: {}", intl_label));
    lines.push(String::new());
    lines.push(format!("- Winner: **{}**", out.intl_winner));
    lines.push(String::new());

    lines.push("## Ballon d'Or".to_string());
    lines.push(String::new());
    let mut bd: Vec<(usize, f32)> = (0..w.pool.len())
        .filter(|&i| w.teams[w.pool[i].team].league_idx < BIG5.len())
        .map(|i| {
            let team = &w.teams[w.pool[i].team];
            let lf = league_factor_for(team.league_idx, &w.league_meta);
            (i, ballon_score(&w.pool[i], team.rep, BASE_YEAR, lf))
        })
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
            let (_, parts) = ballon_breakdown(
                p,
                w.teams[p.team].rep,
                BASE_YEAR,
                league_factor_for(w.teams[p.team].league_idx, &w.league_meta),
            );
            for (label, detail, pts) in parts {
                if pts.abs() > 0.001 {
                    lines.push(format!(
                        "- {} ({}): {:+} pts",
                        label,
                        detail,
                        pts.round() as i32
                    ));
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
    let mut aditya_history: Vec<serde_json::Value> = Vec::new();

    for y in 0..years {
        let year = BASE_YEAR + y;
        let mut out = run_one_season(&mut w, year, &mut rng);
        if let Some(i) = w.pool.iter().position(|p| p.name == "Aditya Singh") {
            let p = &w.pool[i];
            let team = &w.teams[p.team];
            let mind = &w.minds[i];
            aditya_history.push(serde_json::json!({
                "focal_player": p.name,
                "year": year,
                "age": p.age,
                "club": team.name,
                "club_nation": team.nation,
                "league": w.league_meta.get(team.league_idx).map(|m| m.label.clone()).unwrap_or_default(),
                "team_rep": team.rep,
                "team_strength": team.strength,
                "team_record": format!("{}-{}-{} GF{} GA{}", team.w, team.d, team.l, team.gf, team.ga),
                "team_shape": format!("{}-{}-{}", team.shape.0, team.shape.1, team.shape.2),
                "team_academy": team.academy,
                "overall": p.ovr,
                "potential": p.pot,
                "peak_seasons": p.peak,
                "peak_strength": p.peak_str,
                "effective_rating": p.effective(),
                "effective_fresh_rating": p.effective_fresh(),
                "pace": p.attrs.pace,
                "shooting": p.attrs.shooting,
                "passing": p.attrs.passing,
                "dribbling": p.attrs.dribbling,
                "tackling": p.attrs.tackling,
                "heading": p.attrs.heading,
                "strength": p.attrs.strength,
                "stamina": p.attrs.stamina,
                "vision": p.attrs.vision,
                "composure": p.attrs.composure,
                "goalkeeping": p.attrs.goalkeeping,
                "first_touch": p.attrs.first_touch,
                "apps": p.apps,
                "league_goals": p.goals,
                "league_assists": p.assists,
                "el_apps": p.el_apps,
                "el_goals": p.el_goals,
                "el_assists": p.el_assists,
                "ucl_apps": p.ucl_apps,
                "ucl_goals": p.ucl_goals,
                "ucl_assists": p.ucl_assists,
                "intl_apps": p.intl_apps,
                "intl_goals": p.intl_goals,
                "intl_assists": p.intl_assists,
                "season_overall_goals": p.overall_goals(),
                "season_overall_assists": p.overall_assists(),
                "shots": p.shots,
                "shots_on_target": p.sot,
                "on_target_pct": if p.shots > 0 { p.sot as f32 / p.shots as f32 } else { 0.0 },
                "key_passes": p.key_passes,
                "big_chances": p.big_chances,
                "passes_att": p.passes_att,
                "passes_cmp": p.passes_cmp,
                "pass_completion_pct": if p.passes_att > 0 { p.passes_cmp as f32 / p.passes_att as f32 } else { 0.0 },
                "dribbles_att": p.dribbles_att,
                "dribbles_cmp": p.dribbles_cmp,
                "dribble_success_pct": if p.dribbles_att > 0 { p.dribbles_cmp as f32 / p.dribbles_att as f32 } else { 0.0 },
                "tackles": p.tackles,
                "interceptions": p.interceptions,
                "fouls": p.fouls,
                "saves": p.saves,
                "clean_sheets": p.clean_sheets,
                "average_rating": p.avg_rating(),
                "morale": p.morale,
                "fatigue": p.fatigue,
                "adapt": p.adapt,
                "on_loan": p.on_loan,
                "homegrown": p.homegrown,
                "retired": p.retired,
                "intl_retired": p.intl_retired,
                "intl_retire_age": p.intl_retire_age,
                "club_retire_age": p.club_retire_age,
                "market_value": p.value,
                "release_clause": p.release_clause,
                "wage": p.wage,
                "career_earnings": p.career_earnings,
                "signing_bonus": p.signing_bonus,
                "bonus_paid": p.bonus_paid,
                "contract_option": p.contract_option,
                "contract_years": p.locked,
                "transfer_requested": mind.requested,
                "contract_happiness": mind.contract_happiness,
                "league_won": p.league_won,
                "ucl_result": p.ucl_result,
                "el_result": p.el_result,
                "intl_result": p.intl_result,
                "ballons": p.ballons,
                "manager": team.manager.name,
                "club_path": p.path,
            }));
        }
        // Attach the summer window that preceded this season.
        out.transfers = std::mem::take(&mut pending_transfers);
        history.push(out);
        if y + 1 < years {
            age_and_regen(&mut w, &mut rng);
            let (lines, _, _, _) = transfer_window(&mut w, &mut rng);
            pending_transfers = lines;
        }
    }

    if !aditya_history.is_empty() {
        let _ = std::fs::create_dir_all("sim_dump");
        if let Ok(s) = serde_json::to_string_pretty(&aditya_history) {
            let _ = std::fs::write("sim_dump/aditya_singh_career.json", s);
        }
        // Focal-point mode: a deterministic, player-centric dossier with the
        // season ledger, every transfer line that mentions the player, and
        // every durable Nemesis relationship involving him.
        if let Some(focal_idx) = w.pool.iter().position(|p| p.name == "Aditya Singh") {
            let label = |kind: &str, id: usize| -> String {
                if kind == "player" {
                    w.pool
                        .get(id)
                        .map(|p| p.name.clone())
                        .unwrap_or_else(|| "unknown".into())
                } else {
                    w.teams
                        .get(id)
                        .map(|t| t.name.clone())
                        .unwrap_or_else(|| "unknown".into())
                }
            };
            let relationships: Vec<serde_json::Value> = w
                .nemesis
                .iter()
                .filter(|r| {
                    (r.actor_kind == "player" && r.actor_id == focal_idx)
                        || (r.target_kind == "player" && r.target_id == focal_idx)
                })
                .map(|r| {
                    serde_json::json!({
                        "actor": label(r.actor_kind, r.actor_id),
                        "actor_kind": r.actor_kind,
                        "actor_id": r.actor_id,
                        "target": label(r.target_kind, r.target_id),
                        "target_kind": r.target_kind,
                        "target_id": r.target_id,
                        "tension": r.tension,
                        "memory": r.memory,
                        "story": r.story,
                    })
                })
                .collect();
            let transfer_lines: Vec<String> = history
                .iter()
                .flat_map(|o| {
                    o.transfers
                        .iter()
                        .filter(|line| line.contains("Aditya Singh"))
                        .cloned()
                })
                .collect();
            let focal = serde_json::json!({
                "focal_mode": true,
                "player": "Aditya Singh",
                "nation": w.pool[focal_idx].nation,
                "position": w.pool[focal_idx].pos.short(),
                "starting_overall": aditya_history.first().and_then(|s| s.get("overall")),
                "starting_potential": aditya_history.first().and_then(|s| s.get("potential")),
                "seasons": aditya_history.clone(),
                "relationships": relationships,
                "transfer_lines": transfer_lines,
            });
            if let Ok(s) = serde_json::to_string_pretty(&focal) {
                let _ = std::fs::write("sim_dump/aditya_singh_focal.json", s);
            }
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
        p.career_rating_sum += p.rating_sum;
        p.career_rating_apps += p.rating_apps as u32;
    }

    // ── Per-player career dump (sim_dump/careers.json) ─────────────────
    // The forensic record for post-sim analysis: every player's full career
    // line plus their club path — the data behind "how did nation X build a
    // World Cup winner" and "which cascades did scouting create".
    {
        let esc = |s: &str| -> String { s.replace('"', "'").replace('\\', "/") };
        let _ = std::fs::create_dir_all("sim_dump");
        let mut j = String::with_capacity(64 * 1024 * 1024 / 8);
        j.push_str("[\n");
        let last = w.pool.len().saturating_sub(1);
        for (i, p) in w.pool.iter().enumerate() {
            let club = if p.retired {
                "retired".to_string()
            } else if let Some(t) = w.teams.get(p.team) {
                t.name.clone()
            } else {
                "free agent".to_string()
            };
            let path: Vec<String> = p.path.iter().map(|c| format!("\"{}\"", esc(c))).collect();
            j.push_str(&format!(
                "  {{\"name\":\"{}\",\"nation\":\"{}\",\"pos\":\"{}\",\"age\":{},\"intl_retired\":{},\"intl_retire_age\":{},\"club_retire_age\":{},\"club\":\"{}\",\"ovr\":{},\"pot\":{},\"wage\":{},\"locked\":{},\"career_earnings\":{},\"signing_bonus\":{},\"bonus_paid\":{},\"contract_option\":{},\"apps\":{},\"goals\":{},\"assists\":{},\"kp\":{},\"ballons\":{},\"path\":[{}]}}{}\n",
                esc(&p.name),
                esc(&p.nation),
                p.pos.short(),
                p.age,
                p.intl_retired,
                p.intl_retire_age,
                p.club_retire_age,
                esc(&club),
                p.ovr,
                p.pot,
                p.wage,
                p.locked,
                p.career_earnings,
                p.signing_bonus,
                p.bonus_paid,
                p.contract_option,
                p.career_apps,
                p.career_goals,
                p.career_assists,
                p.career_kp,
                p.ballons,
                path.join(","),
                if i < last { "," } else { "" }
            ));
        }
        j.push_str("]\n");
        let _ = std::fs::write("sim_dump/careers.json", j);
        let ml_specs = crate::net::export_ml_models(&w.params);
        if let Ok(s) = serde_json::to_string_pretty(&ml_specs) {
            let _ = std::fs::write("sim_dump/ml_models.json", s);
        }
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "# {}-Year Simulation ({}–{})",
        years,
        BASE_YEAR,
        BASE_YEAR + years - 1
    ));
    lines.push(String::new());

    if !aditya_history.is_empty() {
        lines.push("## Aditya Singh prodigy experiment".to_string());
        lines.push(String::new());
        lines.push(
            "| Year | Age | Club | League | OVR | Apps | G | A | INT G | UCL G | Contract |"
                .to_string(),
        );
        lines.push("|---:|---:|------|--------|---:|---:|---:|---:|---:|---:|---:|".to_string());
        for s in &aditya_history {
            lines.push(format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                s["year"],
                s["age"],
                trunc(s["club"].as_str().unwrap_or(""), 18),
                trunc(s["league"].as_str().unwrap_or(""), 16),
                s["overall"],
                s["apps"],
                s["league_goals"],
                s["league_assists"],
                s["intl_goals"],
                s["ucl_goals"],
                s["contract_years"],
            ));
        }
        lines.push(String::new());
    }

    lines.push("## Champions by year".to_string());
    lines.push(String::new());
    lines.push("| Year | Premier League | La Liga | Serie A | Bundesliga | Ligue 1 | UCL | Europa | Intl | AFC CL | CONCACAF | Libertadores | CWC |".to_string());
    lines.push("|------|----------------|---------|--------|------------|---------|-----|--------|------|--------|----------|--------------|-----|".to_string());
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
            IntlKind::AsianCup => format!("AC: {}", trunc(&o.intl_winner, 13)),
            IntlKind::CopaAmerica => format!("Copa: {}", trunc(&o.intl_winner, 11)),
            IntlKind::Afcon => format!("AFCON: {}", trunc(&o.intl_winner, 11)),
            IntlKind::GoldCup => format!("Gold: {}", trunc(&o.intl_winner, 11)),
            IntlKind::None => "-".to_string(),
        };
        let cwc = if o.club_world_cup_winner != "-" {
            trunc(&o.club_world_cup_winner, 13)
        } else {
            "-".to_string()
        };
        lines.push(format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            o.year,
            ch("Premier League"),
            ch("La Liga"),
            ch("Serie A"),
            ch("Bundesliga"),
            ch("Ligue 1"),
            trunc(&o.ucl_winner, 17),
            trunc(&o.europa_winner, 17),
            intl,
            trunc(&o.afc_winner, 17),
            trunc(&o.concacaf_winner, 17),
            trunc(&o.conmebol_winner, 17),
            cwc,
        ));
    }
    let mut host_lines: Vec<String> = Vec::new();
    for o in &history {
        if o.intl_kind == IntlKind::WorldCup && !o.wc_hosts.is_empty() {
            host_lines.push(format!(
                "- **{} World Cup** — hosted by {}",
                o.year,
                o.wc_hosts.join(", ")
            ));
        }
    }
    if !host_lines.is_empty() {
        lines.push(String::new());
        lines.extend(host_lines);
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
                    o.overall_ties
                        .iter()
                        .map(|n| trunc(n, 16))
                        .collect::<Vec<_>>()
                        .join(" & ")
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
            if pts.abs() > 0.001 {
                lines.push(format!(
                    "- {} ({}): {:+} pts",
                    label,
                    detail,
                    pts.round() as i32
                ));
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
        let cups: Vec<String> = o.cup_winners.iter().map(|(_, c)| trunc(c, 16)).collect();
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
    lines
        .push("| Club | Wealth | Revenue | Cash | Will. | Budget | Reg | HG | Loans |".to_string());
    lines
        .push("|------|-------:|--------:|-----:|:-----:|-------:|----:|---:|:-----:|".to_string());
    let dd = w.inflation.max(1.0);
    let dreal = |v: i64| (v as f32 / dd) as i64;
    let mut by_money: Vec<usize> = (0..w.teams.len()).collect();
    by_money.sort_by(|&a, &b| w.teams[b].revenue_last.cmp(&w.teams[a].revenue_last));
    for &t in by_money.iter().take(16) {
        let tm = &w.teams[t];
        let hg = tm
            .registered
            .iter()
            .filter(|&&i| w.pool[i].homegrown)
            .count();
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
    lines.push(
        "## Money flows, last season (base | league merit | Europe | preseason net spend)"
            .to_string(),
    );
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
    let mut by_titles: Vec<usize> = (0..w.teams.len())
        .filter(|&t| w.teams[t].league_titles > 0)
        .collect();
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
    let mut by_ucl: Vec<usize> = (0..w.teams.len())
        .filter(|&t| w.teams[t].ucl_titles > 0)
        .collect();
    by_ucl.sort_by(|&a, &b| w.teams[b].ucl_titles.cmp(&w.teams[a].ucl_titles));
    let mut shown_ucl = 0usize;
    for &t in &by_ucl {
        if shown_ucl >= 6 && w.teams[t].ucl_titles < w.teams[by_ucl[shown_ucl - 1]].ucl_titles {
            break;
        }
        lines.push(format!(
            "- {} — {}",
            trunc(&w.teams[t].name, 24),
            w.teams[t].ucl_titles
        ));
        shown_ucl += 1;
    }
    lines.push(String::new());

    lines.push("## Multiple Ballon d'Or winners".to_string());
    lines.push(String::new());
    let mut multi: Vec<usize> = (0..w.pool.len())
        .filter(|&i| w.pool[i].ballons > 1)
        .collect();
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
            .then(w.teams[b].manager.seasons.cmp(&w.teams[a].manager.seasons))
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
    let career: Vec<usize> = (0..w.pool.len())
        .filter(|&i| w.pool[i].career_apps > 0)
        .collect();
    let career_rate = |p: &WP| -> f32 {
        if p.career_rating_apps == 0 {
            0.0
        } else {
            p.career_rating_sum / p.career_rating_apps as f32
        }
    };
    // A composite "mark" so an all-round great, not just a poacher, tops it.
    // Stated exactly: career goals + 0.8 per assist + 40 per Ballon d'Or.
    top10(
        &mut lines,
        "Biggest mark (career G + 0.8×A + 40×BdOr)",
        "G / A / BdOr",
        &w.pool,
        &w.teams,
        &career,
        |p| p.career_goals as f32 + p.career_assists as f32 * 0.8 + p.ballons as f32 * 40.0,
        |p| format!("{} / {} / {}", p.career_goals, p.career_assists, p.ballons),
    );
    top10(
        &mut lines,
        "Career goals",
        "G",
        &w.pool,
        &w.teams,
        &career,
        |p| p.career_goals as f32,
        |p| format!("{}", p.career_goals),
    );
    top10(
        &mut lines,
        "Career assists",
        "A",
        &w.pool,
        &w.teams,
        &career,
        |p| p.career_assists as f32,
        |p| format!("{}", p.career_assists),
    );
    top10(
        &mut lines,
        "Career big chances created",
        "BCC",
        &w.pool,
        &w.teams,
        &career,
        |p| p.career_bcc as f32,
        |p| format!("{}", p.career_bcc),
    );
    top10(
        &mut lines,
        "Career key passes",
        "KP",
        &w.pool,
        &w.teams,
        &career,
        |p| p.career_kp as f32,
        |p| format!("{}", p.career_kp),
    );
    top10(
        &mut lines,
        "Highest career earnings",
        "Earnings (2026m) / bonus",
        &w.pool,
        &w.teams,
        &career,
        |p| p.career_earnings as f32 / w.inflation.max(1.0),
        |p| {
            format!(
                "{}m / {}m",
                (p.career_earnings as f32 / w.inflation.max(1.0)) as i64,
                ((p.signing_bonus + p.bonus_paid) as f32 / w.inflation.max(1.0)) as i64
            )
        },
    );

    lines.push("## Nemesis ledger (persistent grudges that reshape futures)".to_string());
    lines.push(String::new());
    if w.nemesis.is_empty() {
        lines.push("- No durable Nemesis relationships yet.".to_string());
    } else {
        let mut rels = w.nemesis.clone();
        rels.sort_by(|a, b| {
            b.tension
                .partial_cmp(&a.tension)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for r in rels.iter().take(16) {
            let label = |kind: &str, id: usize| -> String {
                if kind == "player" {
                    w.pool
                        .get(id)
                        .map(|p| p.name.clone())
                        .unwrap_or_else(|| "unknown".into())
                } else {
                    w.teams
                        .get(id)
                        .map(|t| t.name.clone())
                        .unwrap_or_else(|| "unknown".into())
                }
            };
            lines.push(format!(
                "- {} -> {}: tension {:.2}, memory {} — {}",
                label(r.actor_kind, r.actor_id),
                label(r.target_kind, r.target_id),
                r.tension,
                r.memory,
                r.story
            ));
        }
        let mut json = String::new();
        for r in &rels {
            json.push_str(
                &serde_json::json!({
                    "actor_kind": r.actor_kind,
                    "actor_id": r.actor_id,
                    "target_kind": r.target_kind,
                    "target_id": r.target_id,
                    "tension": r.tension,
                    "memory": r.memory,
                    "story": r.story
                })
                .to_string(),
            );
            json.push('\n');
        }
        let _ = std::fs::create_dir_all("sim_dump");
        let _ = std::fs::write("sim_dump/nemesis.jsonl", json);
    }
    lines.push(String::new());

    // ── Scouting cascades: the pathways the scout system built. ──
    // Careers that climbed the pyramid club by club — the Monaco→PSG→Madrid
    // and Bryne→Molde→Salzburg→Dortmund→City ladders, found by scouts at
    // every rung: potential priced, minnows paid, steps disciplined.
    lines.push("## Scouting cascades (careers that climbed the pyramid)".to_string());
    lines.push(String::new());
    let cascade: Vec<usize> = career
        .iter()
        .copied()
        .filter(|&i| w.pool[i].path.len() >= 3)
        .collect();
    if cascade.is_empty() {
        lines.push("- No multi-club cascades developed this era.".to_string());
    } else {
        let mark = |p: &WP| -> f32 {
            p.career_goals as f32 + p.career_assists as f32 * 0.8 + p.ballons as f32 * 40.0
        };
        let mut cc = cascade;
        cc.sort_by(|&a, &b| {
            mark(&w.pool[b])
                .partial_cmp(&mark(&w.pool[a]))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for &i in cc.iter().take(10) {
            let p = &w.pool[i];
            lines.push(format!(
                "- **{}** ({}, {}): {} — {} apps, {}g {}a{}",
                p.name,
                p.pos.short(),
                p.nation,
                p.path.join(" → "),
                p.career_apps,
                p.career_goals,
                p.career_assists,
                if p.ballons > 0 {
                    format!(", {}× Ballon d'Or", p.ballons)
                } else {
                    String::new()
                }
            ));
        }
    }
    lines.push(String::new());
    // Siege effect, stated plainly: sides that defend for their lives make
    // more tackles — so career ball-winning is led by relegation-battlers,
    // exactly as real tackle charts are. Totals show apps and per-game rate
    // so longevity never reads as godliness.
    top10(
        &mut lines,
        "Career tackles + interceptions",
        "T+I (apps, per game)",
        &w.pool,
        &w.teams,
        &career,
        |p| p.career_ti as f32,
        |p| {
            format!(
                "{} in {} ({:.1}/g)",
                p.career_ti,
                p.career_apps,
                if p.career_apps > 0 {
                    p.career_ti as f32 / p.career_apps as f32
                } else {
                    0.0
                }
            )
        },
    );
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
        .filter(|&i| {
            w.pool[i].pos == Position::DEF && w.pool[i].career_apps >= 80 && !is_fb(&w.pool[i])
        })
        .collect();
    let fb_career: Vec<usize> = career
        .iter()
        .copied()
        .filter(|&i| {
            w.pool[i].pos == Position::DEF && w.pool[i].career_apps >= 80 && is_fb(&w.pool[i])
        })
        .collect();
    let cb_score = |p: &WP| -> f32 {
        let apps = p.career_apps.max(1) as f32;
        p.career_cs as f32 * 2.0
            + (career_rate(p) - 6.5).max(0.0) * 70.0
            + (p.career_ti as f32 / apps) * 8.0
    };
    let fb_score = |p: &WP| -> f32 {
        let apps = p.career_apps.max(1) as f32;
        let going_forward = (p.career_assists as f32 + p.career_kp as f32 * 0.5) / apps;
        p.career_cs as f32 * 1.6
            + (career_rate(p) - 6.5).max(0.0) * 60.0
            + (p.career_ti as f32 / apps) * 5.0
            + going_forward * 60.0
    };
    top10(
        &mut lines,
        "Best centre-backs (career: clean sheets + rating + duels)",
        "CS / avg / T+I·g / role",
        &w.pool,
        &w.teams,
        &cb_career,
        cb_score,
        |p| {
            format!(
                "{} CS, {:.2}, {:.1}/g, {}",
                p.career_cs,
                career_rate(p),
                p.career_ti as f32 / p.career_apps.max(1) as f32,
                pos_tag(p)
            )
        },
    );
    top10(
        &mut lines,
        "Best full-backs & wing-backs (career: solidity + end product)",
        "CS / avg / A+KP / role",
        &w.pool,
        &w.teams,
        &fb_career,
        fb_score,
        |p| {
            format!(
                "{} CS, {:.2}, {}A {}KP, {}",
                p.career_cs,
                career_rate(p),
                p.career_assists,
                p.career_kp,
                pos_tag(p)
            )
        },
    );
    let career_rate200: Vec<usize> = career
        .iter()
        .copied()
        .filter(|&i| w.pool[i].career_rating_apps >= 100)
        .collect();
    top10(
        &mut lines,
        "Career average rating (≥100 apps)",
        "Avg",
        &w.pool,
        &w.teams,
        &career_rate200,
        |p| career_rate(p),
        |p| format!("{:.2}", career_rate(p)),
    );

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
        out.push(format!(
            "Final-season champion reached {} pts (Juventus' 102 is the real record).",
            max_pts
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

    let oldest = w
        .pool
        .iter()
        .filter(|p| !p.retired)
        .map(|p| p.age)
        .max()
        .unwrap_or(0);
    if oldest > 42 {
        out.push(format!(
            "A player is still active at age {} (retirement cap broken).",
            oldest
        ));
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
            ties.iter()
                .map(|n| trunc(n, 16))
                .collect::<Vec<_>>()
                .join(" & "),
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
        lines.push(
            "- No great shocks: every league broadly followed the money this year.".to_string(),
        );
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
        let rep = w
            .teams
            .iter()
            .find(|t| &t.name == club)
            .map(|t| t.rep)
            .unwrap_or(0);
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
            lines.push(format!(
                "- **{}** produced the most Ballon d'Or winners ({}) — a golden generation.",
                nat, n
            ));
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
    idx.sort_by(|&a, &b| {
        val(&pool[b])
            .partial_cmp(&val(&pool[a]))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
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

pub mod web_api {
    use super::*;
    use rand::Rng;
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    /// A stored detailed domestic-league result for the interactive/web
    /// season. `MatchOutcome` contains every starter line and event used by
    /// the browser; ownership stays in memory for the API process.
    struct WebMatchRecord {
        id: usize,
        week: u32,
        /// Calendar year of the season this match belongs to (matches are
        /// retained across rollovers; per-season views filter on this).
        season: u16,
        league_idx: usize,
        round: usize,
        home: usize,
        away: usize,
        comp: Comp,
        /// Human-readable stage for cup matches ("League Phase", "Round of
        /// 16 · 1st leg", "World Cup · Final"...). Empty for domestic league
        /// rounds.
        stage: String,
        /// National-team fixtures have no club indexes: the two sides are
        /// named directly and home/away carry a sentinel.
        home_name: Option<String>,
        away_name: Option<String>,
        outcome: matchsim::MatchOutcome,
    }

    struct LeagueSchedule {
        rounds: Vec<Vec<(usize, usize)>>,
    }

    /// Pseudo-league id used by the browser for European cup matches. Every
    /// real domestic league has a small index, so a dedicated sentinel keeps
    /// `/api/league/<id>` and the match list grouping unambiguous.
    const UCL_LEAGUE_ID: usize = 1_000_000;
    /// Pseudo-league id for international fixtures (nations, not clubs).
    const INTL_LEAGUE_ID: usize = 1_000_001;
    /// The 2024+ Champions League: a 36-team single league phase.
    const UCL_FIELD: usize = 36;
    /// League-phase matchdays per team (4 home + 4 away).
    const UCL_PHASE_ROUNDS: usize = 8;
    /// Top 8 in the league phase go straight to the Round of 16.
    const UCL_DIRECT: usize = 8;
    /// Places 9-24 contest a two-legged knockout play-off.
    const UCL_PLAYOFF: usize = 16;

    /// One knockout tie's result: first leg, optional second leg, and the
    /// aggregate winner (decided on aggregate, then strength-weighted
    /// penalties when level).
    #[derive(Clone, Copy)]
    struct TieResult {
        leg1: (u8, u8),
        leg2: Option<(u8, u8)>,
        winner: usize,
    }

    struct KnockoutRound {
        stage: &'static str,
        /// Zero-based week during which the round is played (both legs of a
        /// two-legged tie land in the same week).
        week: u32,
        ties: Vec<(usize, usize)>,
        winners: Vec<usize>,
        /// Per-tie leg scores, aligned with `ties`; filled as played.
        results: Vec<Option<TieResult>>,
        done: bool,
    }

    impl KnockoutRound {
        fn new(stage: &'static str, week: u32) -> Self {
            Self {
                stage,
                week,
                ties: Vec::new(),
                winners: Vec::new(),
                results: Vec::new(),
                done: false,
            }
        }
    }

    /// Interactive Champions League, real 2024+ format: one 36-team league
    /// phase (8 matchdays, 4 home + 4 away), top 8 straight to the Round of
    /// 16, places 9-24 into a two-legged knockout play-off, then two-legged
    /// Round of 16 / quarter-final / semi-final and a single-leg final. A
    /// champion therefore plays 17 matches (via the play-off) or 15
    /// (straight from the top 8).
    struct UclCup {
        /// The 36 clubs, strongest first.
        field: Vec<usize>,
        /// [matchday] fixture pairs (home, away) for the whole league phase.
        phase_pairs: Vec<Vec<(usize, usize)>>,
        /// Zero-based weeks for the 8 league-phase matchdays.
        phase_weeks: Vec<u32>,
        /// Final league-phase order (team ids, best first). Empty until the
        /// eighth matchday has been played.
        standings: Vec<usize>,
        knockout: Vec<KnockoutRound>,
    }

    struct WebState {
        world: World,
        /// The complete `data/players.csv` roster, retained separately from the
        /// selected world pool. This is what makes database-only prospects,
        /// youth players and dormant source profiles searchable through the
        /// player laboratory even when they were not promoted into the active
        /// domestic world at build time.
        database: Database,
        /// Database position for every source `Player.id`.
        database_index: std::collections::HashMap<PlayerId, usize>,
        /// Source club position for every database player, reverse-derived
        /// from `Club.squad` so CSV profiles always expose their club/league
        /// even when the `Player.club` field itself is not materialized.
        source_club_by_player: std::collections::HashMap<PlayerId, usize>,
        /// Live world-pool index for every loaded source player whose exact
        /// source ID survived the initial world build.
        world_by_database: std::collections::HashMap<PlayerId, usize>,
        rng: Pcg64Mcg,
        week: u32,
        /// Calendar year of the current season (2026-27 season = 2026).
        year: u16,
        /// Completed seasons since the world was created.
        seasons_done: u32,
        /// Matches played in banked earlier seasons (the per-season `matches`
        /// list is cleared at each rollover so ids and stage tables stay
        /// unambiguous; this keeps the headline count honest).
        total_matches: usize,
        /// Last summer's international headline: (year, tournament, winner).
        last_intl: Option<(u16, String, String)>,
        /// Completed-season archives (one JSON blob per rolled-over season):
        /// champions, tables, top scorers, UCL and international outcomes.
        /// Built at rollover before the season stats reset, so history
        /// survives the wipe.
        archives: Vec<Value>,
        schedules: Vec<LeagueSchedule>,
        /// European cup state for the interactive season. `None` when the
        /// world has no European clubs.
        ucl: Option<UclCup>,
        matches: Vec<WebMatchRecord>,
        seed: u64,
        began: String,
    }

    fn make_schedule(team_ids: &[usize]) -> Vec<Vec<(usize, usize)>> {
        let n = team_ids.len();
        if n < 2 {
            return Vec::new();
        }
        let even_n = if n % 2 == 1 { n - 1 } else { n };
        if even_n < 2 {
            return Vec::new();
        }
        let mut order = team_ids[..even_n].to_vec();
        let mut rounds = Vec::new();
        for leg in 0..2u8 {
            for _ in 0..even_n.saturating_sub(1) {
                let mut pairs = Vec::with_capacity(even_n / 2);
                for i in 0..even_n / 2 {
                    let a = order[i];
                    let b = order[even_n - 1 - i];
                    pairs.push(if leg == 0 { (a, b) } else { (b, a) });
                }
                rounds.push(pairs);
                let last = order.pop().unwrap();
                order.insert(1, last);
            }
        }
        rounds
    }

    /// Build the interactive Champions League the real 2024+ way: the 36
    /// strongest UEFA clubs in one field, a Swiss-style league phase (8
    /// matchdays, circle-method pairing over a strength-snaked ring so
    /// everyone meets eight different opponents, ~4 home + 4 away), then the
    /// spring knockout calendar (play-off, R16, QF, SF, final).
    fn build_ucl(world: &World, _rng: &mut Pcg64Mcg) -> Option<UclCup> {
        use crate::world_model::Confederation;
        let mut teams: Vec<usize> = world
            .league_teams
            .iter()
            .enumerate()
            .filter(|(li, _)| *li != 5)
            .filter(|(li, _)| {
                world
                    .league_meta
                    .get(*li)
                    .and_then(|m| m.confed)
                    == Some(Confederation::UEFA)
            })
            .flat_map(|(_, ids)| ids.iter().copied())
            .filter(|&ti| ti < world.teams.len())
            .collect();
        teams.sort_by(|&a, &b| {
            world.teams[b]
                .rep
                .partial_cmp(&world.teams[a].rep)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        teams.dedup();
        if teams.len() < UCL_FIELD {
            return None;
        }
        teams.truncate(UCL_FIELD);
        // Snake the strength order (1st, last, 2nd, second-last...) so early
        // rounds pair giants with outsiders, then run the circle method: the
        // first 8 rotations are everyone's 8 distinct league-phase fixtures.
        let mut ring: Vec<usize> = Vec::with_capacity(teams.len());
        let (mut lo, mut hi) = (0usize, teams.len());
        let mut take_top = true;
        while lo < hi {
            if take_top {
                ring.push(teams[lo]);
                lo += 1;
            } else {
                hi -= 1;
                ring.push(teams[hi]);
            }
            take_top = !take_top;
        }
        let n = ring.len();
        let mut phase_pairs: Vec<Vec<(usize, usize)>> = Vec::new();
        for round in 0..UCL_PHASE_ROUNDS {
            let mut pairs = Vec::with_capacity(n / 2);
            for i in 0..n / 2 {
                let (a, b) = (ring[i], ring[n - 1 - i]);
                // Alternate home advantage by round: ~4 home, ~4 away each.
                pairs.push(if round % 2 == 0 { (a, b) } else { (b, a) });
            }
            phase_pairs.push(pairs);
            // Rotate all but the first entry (standard circle method).
            let last = ring.pop().unwrap();
            ring.insert(1, last);
        }
        let knockout = vec![
            KnockoutRound::new("Knockout Play-off", 20),
            KnockoutRound::new("Round of 16", 23),
            KnockoutRound::new("Quarter-final", 26),
            KnockoutRound::new("Semi-final", 29),
            KnockoutRound::new("Final", 32),
        ];
        Some(UclCup {
            field: teams,
            phase_pairs,
            phase_weeks: vec![3, 5, 7, 9, 11, 13, 15, 17],
            standings: Vec::new(),
            knockout,
        })
    }

    /// League-phase table for the current season over the whole 36-team
    /// field from played "League Phase" matches: points, GD, GF, then club
    /// id for stability.
    fn league_phase_rows(
        state: &WebState,
        field: &[usize],
    ) -> Vec<(usize, u16, u16, u16, u16, u16, u16, u16)> {
        let mut rows: Vec<(usize, u16, u16, u16, u16, u16, u16, u16)> = field
            .iter()
            .map(|&t| (t, 0, 0, 0, 0, 0, 0, 0))
            .collect();
        for m in state.matches.iter().filter(|m| m.season == state.year) {
            if m.comp != Comp::Ucl || !m.stage.starts_with("League Phase") {
                continue;
            }
            let gh = m.outcome.goals_a as u16;
            let ga = m.outcome.goals_b as u16;
            for (team, gf, gaa) in [(m.home, gh, ga), (m.away, ga, gh)] {
                if let Some(row) = rows.iter_mut().find(|r| r.0 == team) {
                    row.1 += 1;
                    row.5 += gf;
                    row.6 += gaa;
                    if gf > gaa {
                        row.2 += 1;
                        row.7 += 3;
                    } else if gf == gaa {
                        row.3 += 1;
                        row.7 += 1;
                    } else {
                        row.4 += 1;
                    }
                }
            }
        }
        rows.sort_by(|a, b| {
            b.7.cmp(&a.7)
                .then((b.5 as i32 - b.6 as i32).cmp(&(a.5 as i32 - a.6 as i32)))
                .then(b.5.cmp(&a.5))
                .then(a.0.cmp(&b.0))
        });
        rows
    }

    /// Winner of a single-leg cup tie, with a strength-weighted shootout
    /// when the score is level.
    fn cup_winner(
        home: usize,
        away: usize,
        gh: u8,
        ga: u8,
        teams: &[Team],
        rng: &mut Pcg64Mcg,
    ) -> usize {
        if gh > ga {
            return home;
        }
        if ga > gh {
            return away;
        }
        let sa = teams[home].strength;
        let sb = teams[away].strength;
        if rng.gen::<f32>() < sa / (sa + sb) {
            home
        } else {
            away
        }
    }

    /// Play one two-legged tie inside the interactive cup: leg one at a's
    /// ground, leg two at b's with the aggregate on the line, both legs
    /// recorded as separate matches (stage tagged with the leg). Aggregate
    /// winner; level ties go to a strength-weighted shootout.
    #[allow(clippy::too_many_arguments)]
    fn play_tie(
        world: &mut World,
        rng: &mut Pcg64Mcg,
        matches: &mut Vec<WebMatchRecord>,
        a: usize,
        b: usize,
        week: u32,
        season: u16,
        stage: &str,
        round_no: usize,
    ) -> Option<TieResult> {
        let (g1a, g1b) = play_web_cup(
            world,
            rng,
            matches,
            a,
            b,
            week,
            season,
            UCL_LEAGUE_ID,
            round_no,
            format!("{} · 1st leg", stage),
            Comp::Ucl,
            (0, 0),
        )?;
        let (g2b, g2a) = play_web_cup(
            world,
            rng,
            matches,
            b,
            a,
            week,
            season,
            UCL_LEAGUE_ID,
            round_no,
            format!("{} · 2nd leg", stage),
            Comp::Ucl,
            (g1b, g1a),
        )?;
        let (ta, tb) = (g1a + g2a, g1b + g2b);
        let winner = if ta > tb {
            a
        } else if tb > ta {
            b
        } else {
            let sa = world.teams[a].strength;
            let sb = world.teams[b].strength;
            if rng.gen::<f32>() < sa / (sa + sb) {
                a
            } else {
                b
            }
        };
        Some(TieResult {
            leg1: (g1a, g1b),
            leg2: Some((g2a, g2b)),
            winner,
        })
    }

    impl WebState {
        fn new(seed: u64) -> Self {
            let db = Database::load();
            let mut rng = Pcg64Mcg::new(seed as u128 | 1);
            let params = Params::load().unwrap_or_default();
            let mut world = build_world(&db, params, &mut rng);
            seed_minds(&mut world, &mut rng);
            reset_season(&mut world);
            let schedules = world
                .league_teams
                .iter()
                .map(|teams| LeagueSchedule {
                    rounds: make_schedule(teams),
                })
                .collect::<Vec<_>>();
            let ucl = build_ucl(&world, &mut rng);
            let database_index = db
                .players()
                .iter()
                .enumerate()
                .map(|(idx, p)| (p.id, idx))
                .collect::<std::collections::HashMap<_, _>>();
            let source_club_by_player = db
                .clubs()
                .iter()
                .enumerate()
                .flat_map(|(club_idx, club)| club.squad.iter().map(move |pid| (*pid, club_idx)))
                .collect::<std::collections::HashMap<_, _>>();
            let world_by_database = world
                .pool
                .iter()
                .enumerate()
                .filter_map(|(idx, p)| p.db_id.map(|id| (id, idx)))
                .collect::<std::collections::HashMap<_, _>>();
            Self {
                world,
                database: db,
                database_index,
                source_club_by_player,
                world_by_database,
                rng,
                week: 0,
                year: BASE_YEAR,
                seasons_done: 0,
                total_matches: 0,
                last_intl: None,
                archives: Vec::new(),
                schedules,
                ucl,
                matches: Vec::new(),
                seed,
                began: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| format!("+{}s", d.as_secs()))
                    .unwrap_or_default(),
            }
        }

        fn total_rounds(&self) -> usize {
            self.schedules
                .iter()
                .map(|s| s.rounds.len())
                .max()
                .unwrap_or(0)
        }

        fn remaining_rounds(&self) -> usize {
            self.total_rounds().saturating_sub(self.week as usize)
        }

        fn league_name(&self, li: usize) -> String {
            self.world
                .league_meta
                .get(li)
                .map(|m| m.label.clone())
                .unwrap_or_else(|| format!("League {}", li))
        }

        fn league_confed(&self, li: usize) -> String {
            self.world
                .league_meta
                .get(li)
                .and_then(|m| m.confed)
                .map(|c| format!("{:?}", c))
                .unwrap_or_default()
        }

        fn simulate_one_week(&mut self) -> usize {
            let season_len = self.total_rounds();
            let ucl_done = self
                .ucl
                .as_ref()
                .map(|c| c.knockout.last().map(|r| r.done).unwrap_or(true))
                .unwrap_or(true);
            // Season complete: every domestic round played and the Champions
            // League final settled. The next step rolls the world forward
            // instead of freezing at 38/38 — summer internationals (a World
            // Cup in 2026/2030…), aging and regens, the transfer window, then
            // a fresh season.
            if season_len > 0 && self.week as usize >= season_len && ucl_done {
                return self.rollover_season();
            }
            let round_idx = self.week as usize;
            let mut played = 0usize;
            for sched_idx in 0..self.schedules.len() {
                let Some(round) = self.schedules[sched_idx].rounds.get(round_idx).cloned() else {
                    continue;
                };
                if round.is_empty() {
                    continue;
                }
                let ids = self.world.league_teams[sched_idx].clone();
                for (home, away) in round {
                    let Some(outcome) = play_detailed(
                        home,
                        away,
                        Comp::League,
                        Venue::HomeA,
                        (0, 0),
                        &mut self.world.teams,
                        &mut self.world.pool,
                        &self.world.params,
                        &mut self.rng,
                    ) else {
                        continue;
                    };
                    record_result(
                        &mut self.world.teams,
                        home,
                        away,
                        outcome.goals_a as u32,
                        outcome.goals_b as u32,
                    );
                    let id = self.matches.len() + 1;
                    self.matches.push(WebMatchRecord {
                        id,
                        week: self.week + 1,
                        season: self.year,
                        league_idx: sched_idx,
                        round: round_idx + 1,
                        home,
                        away,
                        comp: Comp::League,
                        stage: String::new(),
                        home_name: None,
                        away_name: None,
                        outcome,
                    });
                    played += 1;
                }
                let ids_clone = ids.clone();
                weekly_recovery(
                    &ids_clone,
                    &self.world.teams,
                    &mut self.world.pool,
                    &mut self.world.minds,
                    &self.world.params,
                );
            }
            played += self.simulate_ucl_week();
            played += self.simulate_intl_week();
            // Advance the week when anything happened, or when the Champions
            // League still has rounds scheduled beyond the domestic season
            // (so small-league worlds don't stall before the final).
            let ucl_pending = self
                .ucl
                .as_ref()
                .map(|c| c.knockout.iter().any(|r| !r.done && r.week >= self.week))
                .unwrap_or(false);
            if played > 0 || ucl_pending {
                self.week += 1;
            }
            played
        }

        /// Mid-season international breaks (the real FIFA calendar shape):
        /// friendly windows 1-2 in the autumn break, 3-4 in the spring break.
        /// Every friendly is recorded like any other match, so caps, goals
        /// and ratings are visible on player pages and the match wire.
        fn simulate_intl_week(&mut self) -> usize {
            const AUTUMN_BREAK: u32 = 11; // week 12, one-based
            const SPRING_BREAK: u32 = 24; // week 25, one-based
            let rounds: &[u32] = if self.week == AUTUMN_BREAK {
                &[0, 1]
            } else if self.week == SPRING_BREAK {
                &[2, 3]
            } else {
                return 0;
            };
            let week = self.week + 1;
            let season = self.year;
            let mut recorded = 0usize;
            let WebState {
                world,
                matches,
                rng,
                ..
            } = self;
            let mut rec = |o: matchsim::MatchOutcome,
                           home: &str,
                           away: &str,
                           comp: &str,
                           stage: &str| {
                let id = matches.len() + 1;
                matches.push(WebMatchRecord {
                    id,
                    week,
                    season,
                    league_idx: INTL_LEAGUE_ID,
                    round: 0,
                    home: usize::MAX,
                    away: usize::MAX,
                    comp: Comp::Intl,
                    stage: format!("{} · {}", comp, stage),
                    home_name: Some(home.to_string()),
                    away_name: Some(away.to_string()),
                    outcome: o,
                });
                recorded += 1;
            };
            let sink: IntlSink = Some(&mut rec);
            friendly_windows(world, rng, rounds, sink);
            recorded
        }

        /// Close the finished season and open the next one: the summer major
        /// tournament first (World Cup / Euros / continental cups — real
        /// caps, goals and international runs land on every national-team
        /// player, and every fixture is recorded in the match list), then
        /// aging, retirements, regens and the transfer market, then a reset
        /// world with fresh schedules and a new Champions League draw.
        /// Match history is retained with a season tag so player pages keep
        /// showing last season's run.
        fn rollover_season(&mut self) -> usize {
            let year = self.year;
            let kind = intl_kind_for(year);
            let week_base = self.total_rounds() as u32 + 1;
            let season = self.year;
            let WebState {
                world,
                matches,
                rng,
                ..
            } = self;
            let mut week_counter = week_base;
            let mut rec = |o: matchsim::MatchOutcome,
                           home: &str,
                           away: &str,
                           comp: &str,
                           stage: &str| {
                let id = matches.len() + 1;
                matches.push(WebMatchRecord {
                    id,
                    week: week_counter,
                    season,
                    league_idx: INTL_LEAGUE_ID,
                    round: 0,
                    home: usize::MAX,
                    away: usize::MAX,
                    comp: Comp::Intl,
                    stage: format!("{} · {}", comp, stage),
                    home_name: Some(home.to_string()),
                    away_name: Some(away.to_string()),
                    outcome: o,
                });
                week_counter += 1;
            };
            let sink: IntlSink = Some(&mut rec);
            let winner = summer_major_tournament(world, year, rng, sink);
            let kind_label = match kind {
                IntlKind::WorldCup => "World Cup".to_string(),
                IntlKind::Euros => "UEFA Euros".to_string(),
                IntlKind::AsianCup => "AFC Asian Cup".to_string(),
                IntlKind::CopaAmerica => "Copa América".to_string(),
                IntlKind::Afcon => "Africa Cup of Nations".to_string(),
                IntlKind::GoldCup => "Gold Cup".to_string(),
                IntlKind::None => "Internationals".to_string(),
            };
            if winner != "-" {
                self.last_intl = Some((year, kind_label, winner));
            }
            // Bank the finished season before the reset wipes the tables:
            // champions, full league tables, top scorers, the UCL final and
            // the international tournament all survive as an archive.
            let archive = self.build_season_archive(year);
            self.archives.push(archive);
            let (world, rng) = (&mut self.world, &mut self.rng);
            age_and_regen(world, rng);
            let _ = transfer_window(world, rng);
            reset_season(&mut self.world);
            self.week = 0;
            self.year += 1;
            self.seasons_done += 1;
            self.schedules = self
                .world
                .league_teams
                .iter()
                .map(|teams| LeagueSchedule {
                    rounds: make_schedule(teams),
                })
                .collect::<Vec<_>>();
            self.ucl = build_ucl(&self.world, &mut self.rng);
            0
        }

        /// Snapshot a completed season: every league's champion, final table
        /// and top scorer, the Champions League final and its top scorer,
        /// and the summer international tournament with its leading men.
        /// Called at rollover BEFORE the season stats reset.
        fn build_season_archive(&self, year: u16) -> Value {
            let w = &self.world;
            let mut leagues = Vec::new();
            for li in 0..w.league_teams.len() {
                if li == 5 || w.league_teams[li].is_empty() {
                    continue;
                }
                let name = self.league_name(li);
                let order = rank(&w.league_teams[li], &w.teams);
                let table: Vec<Value> = order
                    .iter()
                    .enumerate()
                    .map(|(pos, &t)| {
                        let tm = &w.teams[t];
                        json!({
                            "position": pos + 1,
                            "id": t,
                            "name": tm.name,
                            "played": tm.p, "won": tm.w, "drawn": tm.d, "lost": tm.l,
                            "gf": tm.gf, "ga": tm.ga,
                            "gd": tm.gf as i32 - tm.ga as i32,
                            "pts": tm.w * 3 + tm.d,
                        })
                    })
                    .collect();
                // League top scorer (domestic goals only).
                let mut scorers: Vec<usize> = (0..w.pool.len())
                    .filter(|&i| {
                        w.pool[i].team < w.teams.len()
                            && w.teams[w.pool[i].team].league_idx == li
                            && w.pool[i].goals > 0
                    })
                    .collect();
                scorers.sort_by(|&a, &b| {
                    w.pool[b]
                        .goals
                        .cmp(&w.pool[a].goals)
                        .then(w.pool[b].assists.cmp(&w.pool[a].assists))
                });
                let top_scorer = scorers.first().map(|&i| {
                    json!({
                        "name": w.pool[i].name,
                        "id": i,
                        "club": w.teams[w.pool[i].team].name,
                        "goals": w.pool[i].goals,
                        "assists": w.pool[i].assists,
                    })
                });
                leagues.push(json!({
                    "league_idx": li,
                    "name": name,
                    "champion": order.first().map(|&t| json!({"id": t, "name": w.teams[t].name})),
                    "top_scorer": top_scorer,
                    "table": table,
                }));
            }
            // Champions League final + top scorer.
            let ucl = self.ucl.as_ref().and_then(|c| {
                let tr = c.knockout.last()?.results.iter().flatten().next()?;
                let (h, a) = c.knockout.last()?.ties.first().copied()?;
                Some(json!({
                    "winner": {"id": tr.winner, "name": w.teams[tr.winner].name},
                    "runner_up": {"id": if tr.winner == h { a } else { h },
                                  "name": w.teams[if tr.winner == h { a } else { h }].name},
                    "score": [tr.leg1.0, tr.leg1.1],
                }))
            });
            let mut ucl_scorers: Vec<usize> = (0..w.pool.len())
                .filter(|&i| w.pool[i].ucl_goals > 0)
                .collect();
            ucl_scorers.sort_by(|&a, &b| {
                w.pool[b]
                    .ucl_goals
                    .cmp(&w.pool[a].ucl_goals)
                    .then(w.pool[b].ucl_assists.cmp(&w.pool[a].ucl_assists))
            });
            let ucl_top_scorer = ucl_scorers.first().map(|&i| {
                json!({
                    "name": w.pool[i].name, "id": i,
                    "club": w.teams.get(w.pool[i].team).map(|t| t.name.clone()).unwrap_or_default(),
                    "goals": w.pool[i].ucl_goals, "assists": w.pool[i].ucl_assists,
                })
            });
            // International tournament (summer major + the friendly windows):
            // headline + leading scorers from the live international counters.
            let intl = self.last_intl.as_ref().map(|(y, kind, winner)| {
                let mut intl_scorers: Vec<usize> = (0..w.pool.len())
                    .filter(|&i| w.pool[i].intl_goals > 0)
                    .collect();
                intl_scorers.sort_by(|&a, &b| {
                    w.pool[b]
                        .intl_goals
                        .cmp(&w.pool[a].intl_goals)
                        .then(w.pool[b].intl_assists.cmp(&w.pool[a].intl_assists))
                });
                json!({
                    "year": y,
                    "kind": kind,
                    "winner": winner,
                    "top_scorers": intl_scorers.iter().take(10).map(|&i| json!({
                        "name": w.pool[i].name, "id": i,
                        "club": w.teams.get(w.pool[i].team).map(|t| t.name.clone()).unwrap_or_default(),
                        "nation": w.pool[i].nation,
                        "apps": w.pool[i].intl_apps,
                        "goals": w.pool[i].intl_goals,
                        "assists": w.pool[i].intl_assists,
                    })).collect::<Vec<_>>(),
                })
            });
            json!({
                "year": year,
                "season": format!("{}-{}", year, (year + 1) % 100),
                "leagues": leagues,
                "ucl": {"final": ucl, "top_scorer": ucl_top_scorer},
                "international": intl,
            })
        }

        /// Play any Champions League fixtures scheduled for the current week.
        /// The eight league-phase matchdays run through the autumn; in spring
        /// the knockout play-off (9th-24th), Round of 16, quarter-finals and
        /// semi-finals are two-legged ties played in one week, and the final
        /// is a single leg. Results are stored alongside league matches so
        /// the same match list, player pages and team pages show them.
        fn simulate_ucl_week(&mut self) -> usize {
            let Some(mut cup) = self.ucl.take() else {
                return 0;
            };
            let mut played = 0usize;
            if let Some(md) = cup
                .phase_weeks
                .iter()
                .position(|&w| w == self.week)
            {
                let pairs = cup.phase_pairs.clone();
                let Some(round) = pairs.get(md) else {
                    self.ucl = Some(cup);
                    return played;
                };
                let stage = "League Phase".to_string();
                for &(home, away) in round {
                    if play_web_cup(
                        &mut self.world,
                        &mut self.rng,
                        &mut self.matches,
                        home,
                        away,
                        self.week + 1,
                        self.year,
                        UCL_LEAGUE_ID,
                        md + 1,
                        stage.clone(),
                        Comp::Ucl,
                        (0, 0),
                    )
                    .is_some()
                    {
                        played += 1;
                    }
                }
                // After the final matchday, freeze the league-phase table.
                if md + 1 == cup.phase_weeks.len() {
                    cup.standings = league_phase_rows(self, &cup.field)
                        .into_iter()
                        .map(|r| r.0)
                        .collect();
                }
            }
            if let Some(ki) = cup
                .knockout
                .iter()
                .position(|r| r.week == self.week && !r.done)
            {
                if cup.knockout[ki].ties.is_empty() {
                    if ki == 0 {
                        // Knockout play-off: places 9-24, seeded so the best
                        // runners-up meet the weakest survivors (9v24, 10v23…).
                        let standings = if cup.standings.is_empty() {
                            league_phase_rows(self, &cup.field)
                                .into_iter()
                                .map(|r| r.0)
                                .collect::<Vec<_>>()
                        } else {
                            cup.standings.clone()
                        };
                        let direct = UCL_DIRECT.min(standings.len());
                        let playoff = UCL_PLAYOFF.min(standings.len().saturating_sub(direct));
                        // Reaching a knockout stage is the first honour code.
                        for &t in standings.iter().take(direct + playoff) {
                            set_cup_result(&mut self.world, t, 1);
                        }
                        let mut ties = Vec::new();
                        for i in 0..playoff / 2 {
                            let hi = standings[direct + i];
                            let lo = standings[direct + playoff - 1 - i];
                            ties.push((hi, lo));
                        }
                        cup.knockout[ki].ties = ties;
                    } else if ki == 1 {
                        // Round of 16: the top 8 are seeded against the
                        // play-off winners (best seed at home in the second
                        // leg is approximated by playing the tie from the
                        // seed's ground first).
                        let standings = cup.standings.clone();
                        let prev = cup.knockout[ki - 1].winners.clone();
                        let direct = UCL_DIRECT.min(standings.len());
                        let mut ties = Vec::new();
                        for i in 0..direct.min(prev.len()) {
                            ties.push((standings[direct - 1 - i], prev[i]));
                        }
                        cup.knockout[ki].ties = ties;
                    } else {
                        let prev = cup.knockout[ki - 1].winners.clone();
                        let ties = prev
                            .chunks(2)
                            .filter(|c| c.len() == 2)
                            .map(|c| (c[0], c[1]))
                            .collect::<Vec<_>>();
                        cup.knockout[ki].ties = ties;
                    }
                }
                let ties = cup.knockout[ki].ties.clone();
                let stage = cup.knockout[ki].stage.to_string();
                let round = ki + 1;
                let is_final = ki + 1 == cup.knockout.len();
                let final_tie = if is_final { ties.first().copied() } else { None };
                let mut winners = Vec::new();
                let mut results: Vec<Option<TieResult>> = Vec::new();
                for &(home, away) in &ties {
                    if is_final {
                        // The final is a single leg at a neutral venue.
                        if let Some((gh, ga)) = play_web_cup(
                            &mut self.world,
                            &mut self.rng,
                            &mut self.matches,
                            home,
                            away,
                            self.week + 1,
                            self.year,
                            UCL_LEAGUE_ID,
                            100 + round,
                            stage.clone(),
                            Comp::Ucl,
                            (0, 0),
                        ) {
                            let winner = cup_winner(
                                home,
                                away,
                                gh,
                                ga,
                                &self.world.teams,
                                &mut self.rng,
                            );
                            winners.push(winner);
                            results.push(Some(TieResult {
                                leg1: (gh, ga),
                                leg2: None,
                                winner,
                            }));
                            played += 1;
                        }
                    } else if let Some(tr) = play_tie(
                        &mut self.world,
                        &mut self.rng,
                        &mut self.matches,
                        home,
                        away,
                        self.week + 1,
                        self.year,
                        &stage,
                        100 + round,
                    ) {
                        winners.push(tr.winner);
                        results.push(Some(tr));
                        played += 2;
                    }
                }
                if is_final {
                    // Winning the final is code 5; the runner-up reached the
                    // final, which is code 4.
                    if let (Some(&wn), Some(&(h, a))) = (winners.first(), final_tie.as_ref()) {
                        set_cup_result(&mut self.world, wn, 5);
                        let loser = if wn == h { a } else { h };
                        set_cup_result(&mut self.world, loser, 4);
                    }
                } else {
                    // Play-off winners reach the R16 (1), R16 winners the QF
                    // (2), QF winners the SF (3), SF winners the final (4).
                    let tag = (ki + 1) as u8;
                    for &wn in &winners {
                        set_cup_result(&mut self.world, wn, tag);
                    }
                }
                cup.knockout[ki].winners = winners;
                cup.knockout[ki].results = results;
                cup.knockout[ki].done = true;
            }
            self.ucl = Some(cup);
            played
        }
    }

    /// Record the furthest Champions League stage reached by every player in
    /// a team's squad. Codes are 0 none, 1 R16, 2 QF, 3 SF, 4 runner-up, 5
    /// winner, matching the CLI accounting.
    fn set_cup_result(world: &mut World, ti: usize, r: u8) {
        let squad = match world.teams.get(ti) {
            Some(t) => t.players.clone(),
            None => return,
        };
        for pi in squad {
            if let Some(p) = world.pool.get_mut(pi) {
                if p.ucl_result < r {
                    p.ucl_result = r;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn play_web_cup(
        world: &mut World,
        rng: &mut Pcg64Mcg,
        matches: &mut Vec<WebMatchRecord>,
        home: usize,
        away: usize,
        week: u32,
        season: u16,
        league_idx: usize,
        round: usize,
        stage: String,
        comp: Comp,
        agg: (u8, u8),
    ) -> Option<(u8, u8)> {
        let outcome = play_detailed(
            home,
            away,
            comp,
            Venue::HomeA,
            agg,
            &mut world.teams,
            &mut world.pool,
            &world.params,
            rng,
        )?;
        let (gh, ga) = (outcome.goals_a, outcome.goals_b);
        // Cup results are deliberately NOT folded into the domestic W/D/L
        // records: league tables must reflect league matches only.
        let id = matches.len() + 1;
        matches.push(WebMatchRecord {
            id,
            week,
            season,
            league_idx,
            round,
            home,
            away,
            comp,
            stage,
            home_name: None,
            away_name: None,
            outcome,
        });
        Some((gh, ga))
    }

    fn team_json(w: &World, ti: usize) -> Value {
        let t = &w.teams[ti];
        json!({
            "id": ti,
            "name": t.name,
            "nation": t.nation,
            "league_idx": t.league_idx,
            "league": w.league_meta.get(t.league_idx).map(|m| m.label.as_str()).unwrap_or(""),
            "reputation": t.rep,
            "strength": t.strength,
            "squad": t.players.len(),
            "table": {
                "played": t.p,
                "won": t.w,
                "drawn": t.d,
                "lost": t.l,
                "gf": t.gf,
                "ga": t.ga,
                "gd": t.gd(),
                "pts": t.pts(),
            },
        })
    }

    fn player_matches_json(state: &WebState, world_idx: usize) -> Value {
        let mut rows = Vec::new();
        for m in state.matches.iter().rev() {
            for s in &m.outcome.stats {
                if s.id == world_idx {
                    let mut row = match_summary(state, m);
                    row["player_stats"] = json!({
                        "minutes": s.minutes,
                        "goals": s.goals,
                        "assists": s.assists,
                        "shots": s.shots,
                        "rating": s.rating,
                        "yellows": s.yellows,
                        "sent_off": s.sent_off,
                    });
                    rows.push(row);
                    break;
                }
            }
            if rows.len() >= 20 {
                break;
            }
        }
        rows.reverse();
        json!({"player": player_detail_json(state, world_idx), "matches": rows})
    }

    fn team_recent_json(state: &WebState, ti: usize) -> Value {
        let mut rows = Vec::new();
        for m in state.matches.iter().rev() {
            if m.home == ti || m.away == ti {
                let mut row = match_summary(state, m);
                row["is_home"] = json!(m.home == ti);
                row["result"] = json!(if m.outcome.goals_a as i32 == m.outcome.goals_b as i32 {
                    "D"
                } else if (m.outcome.goals_a as i32 > m.outcome.goals_b as i32) == (ti == m.home) {
                    "W"
                } else {
                    "L"
                });
                rows.push(row);
                if rows.len() >= 12 {
                    break;
                }
            }
        }
        rows.reverse();
        json!({"team": team_json(&state.world, ti), "recent": rows})
    }

    fn manager_team_json(state: &WebState, ti: usize) -> Value {
        let w = &state.world;
        let Some(t) = w.teams.get(ti) else {
            return json!({"error": "team not found"});
        };
        let table = league_table(state, t.league_idx);
        let rank = table.iter().position(|&x| x == ti).map(|i| i + 1);
        let mut squad = t.players.clone();
        squad.sort_by(|&a, &b| {
            w.pool[b]
                .ovr
                .cmp(&w.pool[a].ovr)
                .then(w.pool[a].name.cmp(&w.pool[b].name))
        });
        let players = squad
            .iter()
            .map(|&i| {
                let mut p = player_detail_json(state, i);
                let mind = w.minds.get(i);
                p["motives"] = json!({
                    "ambition": mind.map(|m| m.ambition).unwrap_or(0.0),
                    "loyalty": mind.map(|m| m.loyalty).unwrap_or(0.0),
                    "greed": mind.map(|m| m.greed).unwrap_or(0.0),
                    "professionalism": mind.map(|m| m.professionalism).unwrap_or(0.0),
                    "unhappy": mind.map(|m| m.unhappy).unwrap_or(false),
                    "requested": mind.map(|m| m.requested).unwrap_or(false),
                    "contract_happiness": mind.map(|m| m.contract_happiness).unwrap_or(0.65),
                    "morale_target": mind.map(|m| m.morale_target()).unwrap_or(65),
                });
                p
            })
            .collect::<Vec<_>>();
        let next_round = state.week as usize;
        let next_fixtures = state.schedules[t.league_idx]
            .rounds
            .get(next_round)
            .filter(|pairs| pairs.iter().any(|(h, a)| *h == ti || *a == ti))
            .map(|pairs| {
                pairs
                    .iter()
                    .filter(|(h, a)| *h == ti || *a == ti)
                    .map(|(h, a)| json!({"home": w.teams[*h].name, "away": w.teams[*a].name, "league": state.league_name(t.league_idx), "week": state.week + 1}))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        json!({
            "team": team_json(w, ti),
            "rank": rank,
            "shape": {"def": t.shape.0, "mid": t.shape.1, "fwd": t.shape.2},
            "finance": {"budget": t.budget, "cash": t.cash, "wealth": t.wealth, "revenue_last": t.revenue_last, "wage_cap": t.budget.saturating_sub(t.cash)},
            "mini_table": table.iter().take(8).map(|&idx| team_json(w, idx)).collect::<Vec<_>>(),
            "next_fixtures": next_fixtures,
            "players": players,
            "actions": ["team_talk", "rest", "boost", "promise", "train"],
        })
    }

    fn manager_action_json(state: &mut WebState, body: &Value) -> Value {
        let team_idx = body.get("team").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let action = body
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("team_talk");
        let player_idx = body
            .get("player")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        if team_idx >= state.world.teams.len() {
            return json!({"error": "team not found"});
        }
        let squad = state.world.teams[team_idx].players.clone();
        let mut affected = 0usize;
        match action {
            "team_talk" => {
                for &i in &squad {
                    let p = &mut state.world.pool[i];
                    p.morale = p.morale.min(85).saturating_add(5);
                    affected += 1;
                }
                for &i in &squad {
                    if let Some(mind) = state.world.minds.get_mut(i) {
                        if state.world.pool[i].morale >= 60 {
                            mind.unhappy = false;
                        }
                    }
                }
            }
            "rest" => {
                for &i in &squad {
                    let p = &mut state.world.pool[i];
                    p.fatigue = p.fatigue.saturating_sub(25);
                    p.morale = p.morale.min(85).saturating_add(2);
                    affected += 1;
                }
            }
            "shape" => {
                let def = body.get("def").and_then(|v| v.as_u64()).unwrap_or(4) as usize;
                let mid = body.get("mid").and_then(|v| v.as_u64()).unwrap_or(4) as usize;
                let fwd = body.get("fwd").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
                if def + mid + fwd != 10 || def == 0 || mid == 0 || fwd == 0 {
                    return json!({"error": "formation must be 10 outfielders"});
                }
                state.world.teams[team_idx].shape = (def, mid, fwd);
                affected = 1;
            }
            "boost" | "promise" | "train" => {
                let Some(pi) = player_idx else {
                    return json!({"error": "player required for this action"});
                };
                if !squad.contains(&pi) {
                    return json!({"error": "player not in selected squad"});
                }
                let p = &mut state.world.pool[pi];
                match action {
                    "boost" => {
                        p.morale = p.morale.min(90).saturating_add(10);
                        affected = 1;
                    }
                    "promise" => {
                        p.morale = p.morale.min(90).saturating_add(8);
                        if let Some(mind) = state.world.minds.get_mut(pi) {
                            mind.unhappy = false;
                            mind.requested = false;
                            mind.contract_happiness = (mind.contract_happiness + 0.2).min(1.0);
                        }
                        affected = 1;
                    }
                    _ => {
                        p.fatigue = p.fatigue.saturating_add(8).min(100);
                        p.morale = p.morale.min(88).saturating_add(4);
                        affected = 1;
                    }
                }
            }
            _ => return json!({"error": "unknown manager action"}),
        }
        let mut out = manager_team_json(state, team_idx);
        out["action"] = json!({"performed": action, "affected": affected, "team": team_idx});
        out
    }

    fn player_json(w: &World, i: usize) -> Value {
        let p = &w.pool[i];
        let is_database_player = p.db_id.is_some();
        let club = w
            .teams
            .get(p.team)
            .map(|t| t.name.as_str())
            .unwrap_or("free agent");
        let league = w
            .teams
            .get(p.team)
            .and_then(|t| w.league_meta.get(t.league_idx))
            .map(|m| m.label.as_str())
            .unwrap_or("");
        json!({
            "id": i,
            "handle": format!("wp:{}", i),
            "world_id": i,
            "database_id": p.db_id,
            "source": if is_database_player {
                "live-database"
            } else {
                "generated-player"
            },
            "in_active_world": true,
            "name": p.name,
            "nation": p.nation,
            "position": p.pos.short(),
            "club": club,
            "club_index": p.team,
            "league": league,
            "age": p.age,
            "ovr": p.ovr,
            "pot": p.pot,
            "apps": p.apps,
            "goals": p.goals,
            "assists": p.assists,
            "overall_apps": p.overall_apps(),
            "overall_goals": p.overall_goals(),
            "overall_assists": p.overall_assists(),
            "league_idx": w.teams.get(p.team).map(|t| t.league_idx),
            "rating": if p.rating_apps == 0 { 0.0 } else { p.rating_sum / p.rating_apps as f32 },
            "shots": p.shots,
            "sot": p.sot,
            "key_passes": p.key_passes,
            "big_chances": p.big_chances,
            "passes": format!("{}/{}", p.passes_cmp, p.passes_att),
            "dribbles": format!("{}/{}", p.dribbles_cmp, p.dribbles_att),
            "tackles": p.tackles,
            "interceptions": p.interceptions,
            "fouls": p.fouls,
            "saves": p.saves,
            "clean_sheets": p.clean_sheets,
            "fatigue": p.fatigue,
            "morale": p.morale,
            "ban_games": p.ban_games,
            "intl_goals": p.intl_goals,
            "intl_assists": p.intl_assists,
            "intl_apps": p.intl_apps,
            "ucl_apps": p.ucl_apps,
            "ucl_goals": p.ucl_goals,
            "ucl_assists": p.ucl_assists,
            "el_apps": p.el_apps,
            "el_goals": p.el_goals,
            "el_assists": p.el_assists,
            "league_won": p.league_won,
            "ucl_result": p.ucl_result,
            "el_result": p.el_result,
            "intl_result": p.intl_result,
            "career": {
                "apps": p.career_apps,
                "goals": p.career_goals,
                "assists": p.career_assists,
                "kp": p.career_kp,
                "ballons": p.ballons,
                "path": p.path,
            },
            "attrs": {
                "pace": p.attrs.pace,
                "shooting": p.attrs.shooting,
                "passing": p.attrs.passing,
                "dribbling": p.attrs.dribbling,
                "tackling": p.attrs.tackling,
                "heading": p.attrs.heading,
                "strength": p.attrs.strength,
                "stamina": p.attrs.stamina,
                "vision": p.attrs.vision,
                "composure": p.attrs.composure,
                "goalkeeping": p.attrs.goalkeeping,
                "first_touch": p.attrs.first_touch,
            }
        })
    }

    fn attrs_json(attrs: &Attributes) -> Value {
        json!({
            "pace": attrs.pace,
            "shooting": attrs.shooting,
            "passing": attrs.passing,
            "dribbling": attrs.dribbling,
            "tackling": attrs.tackling,
            "heading": attrs.heading,
            "strength": attrs.strength,
            "stamina": attrs.stamina,
            "vision": attrs.vision,
            "composure": attrs.composure,
            "goalkeeping": attrs.goalkeeping,
            "first_touch": attrs.first_touch,
        })
    }

    fn source_club<'a>(state: &'a WebState, p: &Player) -> Option<&'a Club> {
        p.club
            .and_then(|cid| state.database.clubs().iter().find(|c| c.id == cid))
            .or_else(|| {
                state
                    .source_club_by_player
                    .get(&p.id)
                    .and_then(|&idx| state.database.clubs().get(idx))
            })
    }

    fn source_profile_json(state: &WebState, db_idx: usize) -> Value {
        let p = &state.database.players()[db_idx];
        let club = source_club(state, p);
        json!({
            "database_id": p.id,
            "database_index": db_idx,
            "name": p.name,
            "age": p.age,
            "nation": p.nation,
            "position": p.position.short(),
            "overall": p.overall(),
            "seen_ovr": p.seen_ovr,
            "potential": p.potential,
            "attributes": attrs_json(&p.attrs),
            "source_club": club.map(|c| c.name.as_str()).unwrap_or("unattached"),
            "source_club_short": club.map(|c| c.short.as_str()).unwrap_or(""),
            "source_league": club.map(|c| c.league.as_str()).unwrap_or(""),
            "source_club_id": club.map(|c| c.id),
            "value": p.value,
            "wage": p.contract.wage,
            "contract_years": p.contract.years,
            "contract_expiry": p.contract_expiry,
            "release_clause": p.release_clause,
            "transfer_status": p.transfer_status,
            "morale": p.morale,
            "fitness": p.fitness,
            "form": p.form,
            "condition": p.condition,
            "sharpness": p.sharpness,
            "injury_days": p.injury_days,
            "yellows": p.yellows,
            "suspension": p.suspension,
            "happiness": p.happiness,
            "scout_knowledge": p.scout_knowledge,
            "personality": p.personality.label(),
            "foot": format!("{:?}", p.foot),
            "weak_foot": p.weak_foot,
            "skill_moves": p.skill_moves,
            "traits": p.traits.iter().map(|t| t.label()).collect::<Vec<_>>(),
            "height_cm": p.height_cm,
            "season": {
                "apps": p.stats.apps,
                "goals": p.stats.goals,
                "assists": p.stats.assists,
                "rating": p.stats.avg_rating(),
            },
        })
    }

    /// Full live player detail. A database-backed world player carries its
    /// original `data/players.csv` contract/personality/profile alongside the
    /// evolving in-world attributes and statistics.
    fn player_detail_json(state: &WebState, world_idx: usize) -> Value {
        let mut out = player_json(&state.world, world_idx);
        out["honours"] = player_honours_json(state, world_idx);
        if let Some(db_id) = state.world.pool[world_idx].db_id {
            if let Some(&db_idx) = state.database_index.get(&db_id) {
                out["profile"] = source_profile_json(state, db_idx);
                out["source_attrs"] = source_profile_json(state, db_idx)["attributes"].clone();
            }
        }
        out
    }

    /// Human label for a competition result code. Codes are 0 none, 1 first
    /// knockout round, 2 quarter-final, 3 semi-final, 4 runner-up, 5 winner.
    fn result_label(code: u8) -> &'static str {
        match code {
            0 => "Not involved",
            1 => "Round of 16",
            2 => "Quarter-final",
            3 => "Semi-final",
            4 => "Runner-up",
            5 => "Winner",
            _ => "—",
        }
    }

    /// Individual accolades and honours for one player: award counts, live
    /// scoring races, club position and the furthest stage reached in each
    /// cup this season. Computed from the same world tables the rest of the
    /// site reads so nothing drifts out of sync.
    fn player_honours_json(state: &WebState, world_idx: usize) -> Value {
        let w = &state.world;
        let p = &w.pool[world_idx];
        let league_idx = w.teams.get(p.team).map(|t| t.league_idx);
        let mut league_rank: Option<usize> = None;
        let mut league_leader_goals = 0u16;
        let mut league_position: Option<usize> = None;
        let mut league_name = String::new();
        if let Some(li) = league_idx {
            league_name = state.league_name(li);
            league_position = league_table(state, li)
                .iter()
                .position(|&t| t == p.team)
                .map(|i| i + 1);
            let mut scorers: Vec<usize> = (0..w.pool.len())
                .filter(|&i| {
                    w.teams
                        .get(w.pool[i].team)
                        .map(|t| t.league_idx == li)
                        .unwrap_or(false)
                        && w.pool[i].goals > 0
                })
                .collect();
            scorers.sort_by(|&a, &b| {
                w.pool[b]
                    .goals
                    .cmp(&w.pool[a].goals)
                    .then(w.pool[b].assists.cmp(&w.pool[a].assists))
            });
            league_leader_goals = scorers.first().map(|&i| w.pool[i].goals).unwrap_or(0);
            if p.goals > 0 {
                league_rank = scorers.iter().position(|&i| i == world_idx).map(|r| r + 1);
            }
        }
        let mut ucl: Vec<usize> = (0..w.pool.len())
            .filter(|&i| w.pool[i].ucl_goals > 0)
            .collect();
        ucl.sort_by(|&a, &b| {
            w.pool[b]
                .ucl_goals
                .cmp(&w.pool[a].ucl_goals)
                .then(w.pool[b].ucl_assists.cmp(&w.pool[a].ucl_assists))
        });
        let ucl_leader_goals = ucl.first().map(|&i| w.pool[i].ucl_goals).unwrap_or(0);
        let ucl_rank = if p.ucl_goals > 0 {
            ucl.iter().position(|&i| i == world_idx).map(|r| r + 1)
        } else {
            None
        };
        let ucl_stage = if p.ucl_result == 0 {
            if p.ucl_apps > 0 { "Group stage" } else { "Not involved" }
        } else {
            result_label(p.ucl_result)
        };
        let europa_stage = if p.el_result == 0 {
            if p.el_apps > 0 { "Group stage" } else { "Not involved" }
        } else {
            result_label(p.el_result)
        };
        let international_stage = if p.intl_result == 0 {
            if p.intl_apps > 0 { "Group stage" } else { "Not involved" }
        } else {
            result_label(p.intl_result)
        };
        json!({
            "ballons": p.ballons,
            "league": league_name,
            "league_position": league_position,
            "league_title": p.league_won,
            "league_top_scorer_rank": league_rank,
            "league_top_scorer_goals": p.goals,
            "league_leader_goals": league_leader_goals,
            "ucl_top_scorer_rank": ucl_rank,
            "ucl_top_scorer_goals": p.ucl_goals,
            "ucl_leader_goals": ucl_leader_goals,
            "ucl_assists": p.ucl_assists,
            "ucl_stage": ucl_stage,
            "europa_stage": europa_stage,
            "international_stage": international_stage,
            "season_trophies": {
                "league": p.league_won,
                "champions_league": p.ucl_result == 5,
                "europa_league": p.el_result == 5,
                "international": p.intl_result == 5,
            },
        })
    }

    /// A database player in exactly the JSON shape used by the player
    /// laboratory. When the same source player entered the live world, the
    /// top-level stats are the live world stats and `live` exposes the whole
    /// world profile verbatim. Otherwise the source row is returned with zero
    /// in-world stats and the untouched source profile.
    fn database_player_json(state: &WebState, db_idx: usize) -> Value {
        let p = &state.database.players()[db_idx];
        let world_idx = state.world_by_database.get(&p.id).copied();
        let profile = source_profile_json(state, db_idx);
        if let Some(widx) = world_idx {
            let mut out = player_detail_json(state, widx);
            out["id"] = json!(p.id);
            out["handle"] = json!(format!("db:{}", p.id));
            out["database_id"] = json!(p.id);
            out["source"] = json!("database-player");
            out["source_ovr"] = json!(p.overall());
            out["source_club"] = profile["source_club"].clone();
            out["source_league"] = profile["source_league"].clone();
            out["live"] = out.clone();
            out["profile"] = profile;
            out
        } else {
            let club = profile["source_club"].clone();
            let league = profile["source_league"].clone();
            json!({
                "id": p.id,
                "handle": format!("db:{}", p.id),
                "database_id": p.id,
                "database_index": db_idx,
                "world_id": Value::Null,
                "in_active_world": false,
                "source": "database-player",
                "name": p.name,
                "nation": p.nation,
                "position": p.position.short(),
                "club": club,
                "source_club": profile["source_club"].clone(),
                "league": league,
                "source_league": profile["source_league"].clone(),
                "age": p.age,
                "ovr": p.overall(),
                "source_ovr": p.overall(),
                "pot": p.potential,
                "apps": 0,
                "goals": 0,
                "assists": 0,
                "rating": 0.0,
                "shots": 0,
                "sot": 0,
                "key_passes": 0,
                "big_chances": 0,
                "passes": "0/0",
                "dribbles": "0/0",
                "tackles": 0,
                "interceptions": 0,
                "fouls": 0,
                "saves": 0,
                "clean_sheets": 0,
                "fatigue": 0,
                "morale": p.morale,
                "intl_goals": 0,
                "intl_assists": 0,
                "career": {
                    "apps": 0,
                    "goals": 0,
                    "assists": 0,
                    "kp": 0,
                    "ballons": 0,
                    "path": [],
                },
                "attrs": profile["attributes"].clone(),
                "profile": profile,
                "live": Value::Null,
            })
        }
    }

    /// International football view: the season's fixtures grouped by stage
    /// (friendly windows and the summer tournament), the tournament winner
    /// and leading scorers, plus the history of past tournaments. Uses the
    /// archive once the season has rolled over (live counters are reset),
    /// live pool stats for the current season.
    fn internationals_json(state: &WebState, season: Option<u16>) -> Value {
        let year = season.unwrap_or(state.year);
        let w = &state.world;
        let matches: Vec<&WebMatchRecord> = state
            .matches
            .iter()
            .filter(|m| m.comp == Comp::Intl && m.season == year)
            .collect();
        // Tournament facts: from the archive when the season is complete,
        // live (winner unknown until played) otherwise.
        let archived = state.archives.iter().find(|a| {
            a.get("year").and_then(|v| v.as_u64()).map(|y| y as u16) == Some(year)
        });
        // Tournament-only scorers, folded straight from the recorded match
        // sheets (friendlies excluded): a World Cup run is 8 games at most —
        // 3 group + R32/R16/QF/SF/F — never the friendly windows on top.
        let mut agg: std::collections::HashMap<usize, (u16, u16, u16)> =
            std::collections::HashMap::new();
        for m in state
            .matches
            .iter()
            .filter(|m| m.comp == Comp::Intl && m.season == year)
            .filter(|m| !m.stage.starts_with("Friendly"))
        {
            for s in &m.outcome.stats {
                if s.id >= w.pool.len() {
                    continue;
                }
                let e = agg.entry(s.id).or_insert((0, 0, 0));
                e.0 += 1; // apps: one match sheet = one appearance
                e.1 += s.goals as u16;
                e.2 += s.assists as u16;
            }
        }
        let mut order: Vec<usize> = agg.keys().copied().collect();
        order.sort_by(|&a, &b| {
            agg[&b]
                .1
                .cmp(&agg[&a].1)
                .then(agg[&b].2.cmp(&agg[&a].2))
                .then(agg[&a].0.cmp(&agg[&b].0))
        });
        let top_scorers: Value = order
            .iter()
            .take(10)
            .map(|&i| {
                let (apps, goals, assists) = agg[&i];
                json!({
                    "name": w.pool[i].name, "id": i,
                    "club": w.teams.get(w.pool[i].team).map(|t| t.name.clone()).unwrap_or_default(),
                    "nation": w.pool[i].nation,
                    "apps": apps,
                    "goals": goals,
                    "assists": assists,
                })
            })
            .collect::<Vec<_>>()
            .into();
        let (kind, winner) = if let Some(a) = archived {
            let intl = a.get("international").cloned().unwrap_or(Value::Null);
            (
                intl.get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                intl.get("winner")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            )
        } else {
            let kind = match intl_kind_for(year) {
                IntlKind::WorldCup => "World Cup",
                IntlKind::Euros => "UEFA Euros",
                IntlKind::AsianCup => "AFC Asian Cup",
                IntlKind::CopaAmerica => "Copa América",
                IntlKind::Afcon => "Africa Cup of Nations",
                IntlKind::GoldCup => "Gold Cup",
                IntlKind::None => "Friendlies",
            };
            let winner = state
                .last_intl
                .as_ref()
                .filter(|(y, _, _)| *y == year)
                .map(|(_, _, wn)| wn.clone())
                .unwrap_or_default();
            (kind.to_string(), winner)
        };
        // Group the season's internationals by stage, in played order.
        let mut stages: Vec<(String, Vec<Value>)> = Vec::new();
        for m in &matches {
            let row = match_summary(state, m);
            match stages.iter_mut().find(|(s, _)| s == &m.stage) {
                Some((_, rows)) => rows.push(row),
                None => stages.push((m.stage.clone(), vec![row])),
            }
        }
        let history: Vec<Value> = state
            .archives
            .iter()
            .filter_map(|a| {
                let intl = a.get("international")?;
                Some(json!({
                    "year": a.get("year"),
                    "kind": intl.get("kind"),
                    "winner": intl.get("winner"),
                }))
            })
            .collect();
        // Group-stage field: recover the groups from who played whom (teams
        // that met in the group stage form a 4-team clique), then table them
        // by points/GD/GF. This is the tournament's 48-team field.
        let mut groups_out: Vec<Value> = Vec::new();
        {
            let group_matches: Vec<&WebMatchRecord> = matches
                .iter()
                .filter(|m| m.stage.ends_with("Group Stage"))
                .copied()
                .collect();
            if !group_matches.is_empty() {
                // Union-find over nation names.
                fn uf_find(
                    p: &mut std::collections::HashMap<String, String>,
                    x: &str,
                ) -> String {
                    let mut root = x.to_string();
                    while let Some(up) = p.get(&root).cloned() {
                        if up == root {
                            break;
                        }
                        root = up;
                    }
                    p.insert(x.to_string(), root.clone());
                    root
                }
                let mut parent: std::collections::HashMap<String, String> =
                    std::collections::HashMap::new();
                for m in &group_matches {
                    for n in [&m.home_name, &m.away_name] {
                        let name = n.clone().unwrap_or_default();
                        parent.entry(name.clone()).or_insert_with(|| name.clone());
                    }
                    let (ra, rb) = (
                        uf_find(&mut parent, &m.home_name.clone().unwrap_or_default()),
                        uf_find(&mut parent, &m.away_name.clone().unwrap_or_default()),
                    );
                    if ra != rb {
                        parent.insert(ra, rb);
                    }
                }
                let mut by_group: std::collections::HashMap<String, Vec<String>> =
                    std::collections::HashMap::new();
                for name in parent.keys().cloned().collect::<Vec<_>>() {
                    let root = uf_find(&mut parent, &name);
                    by_group.entry(root).or_default().push(name.clone());
                }
                let mut grouped: Vec<Vec<String>> = by_group.into_values().collect();
                grouped.sort_by(|a, b| {
                    a.first()
                        .cmp(&b.first())
                });
                for (gi, members) in grouped.iter().enumerate() {
                    let letter = (b'A' + (gi as u8 % 26)) as char;
                    let mut rows: Vec<(String, u16, u16, u16, u16, u16, u16, i32, u16)> = members
                        .iter()
                        .map(|n| (n.clone(), 0, 0, 0, 0, 0, 0, 0, 0))
                        .collect();
                    for m in &group_matches {
                        let (h, a) = (
                            m.home_name.clone().unwrap_or_default(),
                            m.away_name.clone().unwrap_or_default(),
                        );
                        let (gh, ga) = (m.outcome.goals_a as u16, m.outcome.goals_b as u16);
                        for (team, gf, gaa) in [(h.clone(), gh, ga), (a.clone(), ga, gh)] {
                            if let Some(row) = rows.iter_mut().find(|r| r.0 == team) {
                                row.1 += 1;
                                row.5 += gf;
                                row.6 += gaa;
                                if gf > gaa {
                                    row.2 += 1;
                                    row.8 += 3;
                                } else if gf == gaa {
                                    row.3 += 1;
                                    row.8 += 1;
                                } else {
                                    row.4 += 1;
                                }
                            }
                        }
                    }
                    rows.sort_by(|a, b| {
                        b.8.cmp(&a.8)
                            .then((b.5 as i32 - b.6 as i32).cmp(&(a.5 as i32 - a.6 as i32)))
                            .then(b.5.cmp(&a.5))
                    });
                    groups_out.push(json!({
                        "name": format!("Group {}", letter),
                        "teams": rows.into_iter().enumerate().map(|(pos, r)| json!({
                            "position": pos + 1,
                            "name": r.0,
                            "played": r.1, "won": r.2, "drawn": r.3, "lost": r.4,
                            "gf": r.5, "ga": r.6, "gd": r.5 as i32 - r.6 as i32, "pts": r.8,
                        })).collect::<Vec<_>>(),
                    }));
                }
            }
        }
        json!({
            "competition": "Internationals",
            "year": year,
            "season": format!("{}-{}", year, (year + 1) % 100),
            "kind": kind,
            "winner": if winner.is_empty() { Value::Null } else { json!(winner) },
            "top_scorers": top_scorers,
            "groups": groups_out,
            "stages": stages.into_iter().map(|(name, rows)| json!({
                "name": name,
                "matches": rows,
            })).collect::<Vec<_>>(),
            "history": history,
            "archived": archived.is_some(),
        })
    }

    fn match_summary(state: &WebState, m: &WebMatchRecord) -> Value {
        let w = &state.world;
        let intl = m.comp == Comp::Intl;
        let league = if intl {
            "Internationals".to_string()
        } else if m.comp == Comp::Ucl {
            "UEFA Champions League".to_string()
        } else {
            w.league_meta
                .get(m.league_idx)
                .map(|x| x.label.clone())
                .unwrap_or_default()
        };
        json!({
            "id": m.id,
            "week": m.week,
            "season": m.season,
            "round": m.round,
            "league_idx": m.league_idx,
            "league": league,
            "competition": if intl { "Internationals" } else if m.comp == Comp::Ucl { "UEFA Champions League" } else { "League" },
            "stage": m.stage,
            "home_team": m.home,
            "away_team": m.away,
            "home": m.home_name.clone().unwrap_or_else(|| w.teams[m.home].name.clone()),
            "away": m.away_name.clone().unwrap_or_else(|| w.teams[m.away].name.clone()),
            "home_goals": m.outcome.goals_a,
            "away_goals": m.outcome.goals_b,
            "xg": [m.outcome.xg_a, m.outcome.xg_b],
            "shots": [m.outcome.shots_a, m.outcome.shots_b],
            "possession": m.outcome.poss_a,
            "corners": [m.outcome.corners_a, m.outcome.corners_b],
            "set_piece_goals": [m.outcome.sp_goals_a, m.outcome.sp_goals_b],
        })
    }

    fn event_kind_text(kind: matchsim::EventKind) -> &'static str {
        match kind {
            matchsim::EventKind::Goal => "goal",
            matchsim::EventKind::SetPiece => "set piece",
            matchsim::EventKind::Injury => "injury",
            matchsim::EventKind::Penalty => "penalty",
            matchsim::EventKind::RedCard => "red card",
            matchsim::EventKind::YellowCard => "yellow card",
            matchsim::EventKind::SubOff => "substitution off",
            matchsim::EventKind::SubOn => "substitution on",
        }
    }

    fn query_value(query: &str, key: &str) -> Option<String> {
        for part in query.split('&').filter(|x| !x.is_empty()) {
            let mut kv = part.splitn(2, '=');
            let k = url_decode(kv.next().unwrap_or_default());
            if k == key {
                return Some(url_decode(kv.next().unwrap_or_default()));
            }
        }
        None
    }

    fn url_decode(s: &str) -> String {
        let mut out = Vec::with_capacity(s.len());
        let mut chars = s.bytes().peekable();
        while let Some(b) = chars.next() {
            match b {
                b'%' => {
                    let hi = chars.next().unwrap_or(b'0');
                    let lo = chars.next().unwrap_or(b'0');
                    let h = char::from(hi).to_digit(16).unwrap_or(0);
                    let l = char::from(lo).to_digit(16).unwrap_or(0);
                    out.push(((h << 4) | l) as u8);
                }
                b'+' => out.push(b' '),
                _ => out.push(b),
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn league_table(state: &WebState, li: usize) -> Vec<usize> {
        if li >= state.world.league_teams.len() {
            return Vec::new();
        }
        rank(&state.world.league_teams[li], &state.world.teams)
    }

    fn status_json(state: &WebState) -> Value {
        let total_rounds = state.total_rounds();
        json!({
            "ok": true,
            "service": "football-api",
            "version": "0.1.0",
            "year": state.year,
            "season": format!("{}-{}", state.year, (state.year + 1) % 100),
            "seasons_done": state.seasons_done,
            "total_matches": state.matches.len(),
            "international": state.last_intl.as_ref().map(|(y, k, w)| json!({"year": y, "kind": k, "winner": w})),
            "week": state.week,
            "next_week": state.week + 1,
            "total_rounds": total_rounds,
            "remaining_rounds": state.remaining_rounds(),
            "matches_played": state.matches.len(),
            "database_players": state.database.players().len(),
            "database_clubs": state.database.clubs().len(),
            "seed": state.seed,
            "started": state.began,
        })
    }

    fn overview_json(state: &WebState) -> Value {
        let w = &state.world;
        let played = state.matches.len();
        let top: Vec<usize> = {
            let mut idx: Vec<usize> = (0..w.pool.len()).filter(|&i| w.pool[i].apps > 0).collect();
            idx.sort_by(|&a, &b| {
                w.pool[b]
                    .goals
                    .cmp(&w.pool[a].goals)
                    .then(w.pool[b].assists.cmp(&w.pool[a].assists))
            });
            idx.into_iter().take(12).collect()
        };
        let recent = state
            .matches
            .iter()
            .rev()
            .take(10)
            .map(|m| match_summary(state, m))
            .collect::<Vec<_>>();
        json!({
            "world": {
                "year": state.year,
                "season": format!("{}-{}", state.year, (state.year + 1) % 100),
                "seasons_done": state.seasons_done,
                "international": state.last_intl.as_ref().map(|(y, k, w)| json!({"year": y, "kind": k, "winner": w})),
                "week": state.week,
                "next_week": state.week + 1,
                "players": w.pool.len(),
                "database_players": state.database.players().len(),
                "database_clubs": state.database.clubs().len(),
                "teams": w.teams.len(),
                "leagues": w.league_teams.len(),
                "total_rounds": state.total_rounds(),
                "remaining_rounds": state.remaining_rounds(),
                "matches_played": played,
                "seed": state.seed,
            },
            "top_scorers": top
                .into_iter()
                .map(|i| player_detail_json(state, i))
                .collect::<Vec<_>>(),
            "recent_matches": recent,
        })
    }

    fn leagues_json(state: &WebState) -> Vec<Value> {
        let w = &state.world;
        let mut out = Vec::new();
        for li in 0..w.league_teams.len() {
            if li == 5 {
                continue;
            }
            let table = league_table(state, li);
            let leader = table
                .first()
                .map(|&t| w.teams[t].name.clone())
                .unwrap_or_default();
            out.push(json!({
                "id": li,
                "name": w.league_meta.get(li).map(|m| m.label.as_str()).unwrap_or(""),
                "confederation": state.league_confed(li),
                "teams": w.league_teams[li].len(),
                "leader": leader,
                "rounds": state.schedules[li].rounds.len(),
                "finished": state.schedules[li].rounds.len() > 0 && state.week as usize >= state.schedules[li].rounds.len(),
            }));
        }
        out
    }

    fn league_detail_json(state: &WebState, li: usize) -> Value {
        let w = &state.world;
        let table = league_table(state, li);
        let mut scorers: Vec<usize> = (0..w.pool.len())
            .filter(|&i| w.teams[w.pool[i].team].league_idx == li)
            .collect();
        scorers.sort_by(|&a, &b| {
            w.pool[b]
                .goals
                .cmp(&w.pool[a].goals)
                .then(w.pool[b].assists.cmp(&w.pool[a].assists))
        });
        let next_round = state.week as usize;
        let fixtures = state.schedules[li]
            .rounds
            .get(next_round)
            .map(|pairs| {
                pairs
                    .iter()
                    .map(|&(h, a)| {
                        json!({
                            "home": w.teams[h].name,
                            "away": w.teams[a].name,
                            "home_goals": Value::Null,
                            "away_goals": Value::Null,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        json!({
            "league": w.league_meta.get(li).map(|m| m.label.as_str()).unwrap_or(""),
            "confederation": state.league_confed(li),
            "week": state.week,
            "table": table.iter().enumerate().map(|(i, &t)| {
                let mut x = team_json(w, t);
                x["position"] = json!(i + 1);
                x
            }).collect::<Vec<_>>(),
            "scorers": scorers
                .into_iter()
                .take(20)
                .map(|i| player_detail_json(state, i))
                .collect::<Vec<_>>(),
            "next_fixtures": fixtures,
            "recent": state.matches.iter().rev()
                .filter(|m| m.league_idx == li)
                .take(20)
                .map(|m| match_summary(state, m))
                .collect::<Vec<_>>(),
        })
    }

    /// Full Champions League view: the single 36-team league-phase table
    /// (with top-8 / play-off / eliminated status), the knockout bracket
    /// (both legs and aggregates) and the competition scoring chart. Returned
    /// by `/api/league/1000000` so the normal league links and match rows can
    /// open it.
    fn ucl_detail_json(state: &WebState) -> Value {
        let w = &state.world;
        let Some(cup) = state.ucl.as_ref() else {
            return json!({"league": "UEFA Champions League", "confederation": "UEFA", "week": state.week, "groups": [], "knockout": [], "table": [], "scorers": [], "recent": []});
        };
        let phase_done = cup.standings.len() == cup.field.len();
        // League-phase table (current shape) + a legacy "groups" payload with
        // the same rows so older deployed frontends (which render group
        // tables) still show the single 36-team table instead of "not drawn".
        let phase_rows = league_phase_rows(state, &cup.field);
        let table = phase_rows
            .iter()
            .enumerate()
            .map(|(pos, &(team, p, won, drawn, lost, gf, ga, pts))| {
                let status = if !phase_done {
                    "League phase in progress"
                } else if pos < UCL_DIRECT.min(cup.field.len()) {
                    "Direct to Round of 16"
                } else if pos < (UCL_DIRECT + UCL_PLAYOFF).min(cup.field.len()) {
                    "Knockout play-off"
                } else {
                    "Eliminated"
                };
                json!({
                    "id": team,
                    "name": w.teams[team].name,
                    "position": pos + 1,
                    "played": p,
                    "won": won,
                    "drawn": drawn,
                    "lost": lost,
                    "gf": gf,
                    "ga": ga,
                    "gd": gf as i32 - ga as i32,
                    "pts": pts,
                    "status": status,
                })
            })
            .collect::<Vec<_>>();
        let groups = vec![json!({
            "name": "League Phase",
            "teams": table,
        })];
        let knockout = cup
            .knockout
            .iter()
            .map(|round| {
                json!({
                    "stage": round.stage,
                    "week": round.week + 1,
                    "teams": round.ties.iter().zip(round.results.iter()).map(|(&(h, a), res)| {
                        let mut x = json!({
                            "home_id": h,
                            "away_id": a,
                            "home": w.teams[h].name,
                            "away": w.teams[a].name,
                            "home_goals": Value::Null,
                            "away_goals": Value::Null,
                            "leg1": Value::Null,
                            "leg2": Value::Null,
                            "agg_home": Value::Null,
                            "agg_away": Value::Null,
                            "winner_id": Value::Null,
                            "winner": Value::Null,
                        });
                        if let Some(tr) = res {
                            // Legs are stored from the tie's first-leg home
                            // side (h) perspective. The legacy score fields
                            // carry the AGGREGATE for two-legged ties (and
                            // the single-leg score for the final) so every
                            // consumer shows the tie result, not leg one.
                            x["leg1"] = json!({"home_goals": tr.leg1.0, "away_goals": tr.leg1.1});
                            if let Some((l2h, l2a)) = tr.leg2 {
                                x["leg2"] = json!({"home_goals": l2a, "away_goals": l2h});
                                x["home_goals"] = json!(tr.leg1.0 + l2h);
                                x["away_goals"] = json!(tr.leg1.1 + l2a);
                                x["agg_home"] = json!(tr.leg1.0 + l2h);
                                x["agg_away"] = json!(tr.leg1.1 + l2a);
                            } else {
                                x["home_goals"] = json!(tr.leg1.0);
                                x["away_goals"] = json!(tr.leg1.1);
                                x["agg_home"] = json!(tr.leg1.0);
                                x["agg_away"] = json!(tr.leg1.1);
                            }
                            x["winner_id"] = json!(tr.winner);
                            x["winner"] = json!(w.teams[tr.winner].name);
                        }
                        x
                    }).collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>();
        let mut scorers: Vec<usize> = (0..w.pool.len())
            .filter(|&i| w.pool[i].ucl_goals > 0 || w.pool[i].ucl_assists > 0)
            .collect();
        scorers.sort_by(|&a, &b| {
            w.pool[b]
                .ucl_goals
                .cmp(&w.pool[a].ucl_goals)
                .then(w.pool[b].ucl_assists.cmp(&w.pool[a].ucl_assists))
        });
        json!({
            "league": "UEFA Champions League",
            "confederation": "UEFA",
            "week": state.week,
            "format": "36-club league phase (8 matchdays) · top 8 direct to the Round of 16 · 9th-24th two-legged play-off · two-legged knockouts · single-leg final",
            "groups": groups,
            "knockout": knockout,
            "table": table,
            "scorers": scorers.into_iter().take(20).map(|i| player_detail_json(state, i)).collect::<Vec<_>>(),
            "recent": state.matches.iter().rev()
                .filter(|m| m.comp == Comp::Ucl)
                .take(20)
                .map(|m| match_summary(state, m))
                .collect::<Vec<_>>(),
        })
    }

    fn fixtures_json(state: &WebState, week_one_based: Option<usize>) -> Vec<Value> {        let week = week_one_based.unwrap_or(state.week.saturating_add(1) as usize);
        let idx = week.saturating_sub(1);
        let mut out = Vec::new();
        for (li, sched) in state.schedules.iter().enumerate() {
            if li == 5 {
                continue;
            }
            if let Some(pairs) = sched.rounds.get(idx) {
                for &(h, a) in pairs {
                    out.push(json!({
                        "league_idx": li,
                        "league": state.world.league_meta.get(li).map(|m| m.label.as_str()).unwrap_or(""),
                        "home": state.world.teams[h].name,
                        "away": state.world.teams[a].name,
                        "week": week,
                    }));
                }
            }
        }
        out
    }

    fn players_json(state: &WebState, query: &str) -> Value {
        #[derive(Clone)]
        enum PlayerCandidate {
            World(usize),
            Database(usize),
        }

        fn metric(state: &WebState, candidate: &PlayerCandidate) -> Value {
            // Sort metrics are normalized to the same fields for the live
            // world and the source database. Database-only players therefore
            // sort by their untouched source profile instead of acquiring
            // synthetic season output merely to become visible.
            match candidate {
                PlayerCandidate::World(i) => {
                    let p = &state.world.pool[*i];
                    json!({
                        "goals": p.goals,
                        "assists": p.assists,
                        "rating": p.avg_rating(),
                        "apps": p.apps,
                        "kp": p.key_passes,
                        "created": p.big_chances,
                        "tackles": p.tackles + p.interceptions,
                        "ovr": p.ovr,
                        "pot": p.pot,
                        "name": p.name,
                        "source_rank": 0,
                    })
                }
                PlayerCandidate::Database(i) => {
                    let p = &state.database.players()[*i];
                    json!({
                        "goals": p.stats.goals,
                        "assists": p.stats.assists,
                        "rating": p.stats.avg_rating(),
                        "apps": p.stats.apps,
                        "kp": 0,
                        "created": 0,
                        "tackles": 0,
                        "ovr": p.overall(),
                        "pot": p.potential,
                        "name": p.name,
                        "source_rank": 1,
                    })
                }
            }
        }

        let w = &state.world;
        let q = query_value(query, "q")
            .or_else(|| query_value(query, "search"))
            .unwrap_or_default()
            .to_lowercase();
        let nation = query_value(query, "nation")
            .unwrap_or_default()
            .to_lowercase();
        let league_name = query_value(query, "league")
            .unwrap_or_default()
            .to_lowercase();
        let position = query_value(query, "position")
            .unwrap_or_default()
            .to_uppercase();
        let min_ovr = query_value(query, "min_ovr")
            .and_then(|s| s.parse::<u8>().ok())
            .unwrap_or(0);
        let sort = query_value(query, "sort").unwrap_or_else(|| "goals".to_string());
        let scope = query_value(query, "scope")
            .unwrap_or_else(|| "all".to_string())
            .to_lowercase();
        let include_world = scope == "all" || scope == "world";
        let include_database = scope == "all" || scope == "database";
        let limit = query_value(query, "limit")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(250)
            .clamp(1, 2000);
        let offset = query_value(query, "offset")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);

        let world_ids: Vec<usize> = if include_world {
            w.pool
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    (q.is_empty()
                        || p.name.to_lowercase().contains(&q)
                        || p.nation.to_lowercase().contains(&q)
                        || p.db_id
                            .map(|id| id.to_string())
                            .as_deref()
                            .unwrap_or("")
                            .contains(&q))
                        && (nation.is_empty() || p.nation.to_lowercase().contains(&nation))
                        && (league_name.is_empty()
                            || w.teams
                                .get(p.team)
                                .and_then(|t| w.league_meta.get(t.league_idx))
                                .map(|m| m.label.to_lowercase())
                                .map(|l| l.contains(&league_name))
                                .unwrap_or(false))
                        && (position.is_empty() || p.pos.short().eq_ignore_ascii_case(&position))
                        && p.ovr >= min_ovr
                })
                .map(|(i, _)| i)
                .collect()
        } else {
            Vec::new()
        };
        let world_total = world_ids.len();

        let database_ids: Vec<usize> = if include_database {
            state
                .database
                .players()
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    let club = source_club(state, p);
                    let club_name = club.map(|c| c.name.to_lowercase()).unwrap_or_default();
                    let club_league = club.map(|c| c.league.to_lowercase()).unwrap_or_default();
                    (q.is_empty()
                        || p.name.to_lowercase().contains(&q)
                        || p.nation.to_lowercase().contains(&q)
                        || club_name.contains(&q)
                        || club_league.contains(&q)
                        || p.id.to_string().contains(&q))
                        && (nation.is_empty() || p.nation.to_lowercase().contains(&nation))
                        && (league_name.is_empty()
                            || club_name.contains(&league_name)
                            || club_league.contains(&league_name))
                        && (position.is_empty()
                            || p.position.short().eq_ignore_ascii_case(&position))
                        && p.overall() >= min_ovr
                })
                .map(|(i, _)| i)
                .collect()
        } else {
            Vec::new()
        };
        let database_total = database_ids.len();
        let database_active_total = database_ids
            .iter()
            .filter(|&&i| {
                state
                    .world_by_database
                    .contains_key(&state.database.players()[i].id)
            })
            .count();
        let database_only_total = database_total.saturating_sub(database_active_total);

        let mut candidates: Vec<PlayerCandidate> = Vec::with_capacity(world_total + database_total);
        let active_world_ids: std::collections::HashSet<usize> =
            world_ids.iter().copied().collect();
        candidates.extend(world_ids.into_iter().map(PlayerCandidate::World));
        for i in database_ids {
            let id = state.database.players()[i].id;
            // In `all`, the live world representation already carries the
            // exact source profile through `profile`; avoid a duplicate row
            // for the same database identity. If the live candidate was
            // filtered out by a world-specific filter, retain the database
            // representation so the player remains discoverable.
            if !scope.eq_ignore_ascii_case("database")
                && state
                    .world_by_database
                    .get(&id)
                    .is_some_and(|world_idx| active_world_ids.contains(world_idx))
            {
                continue;
            }
            candidates.push(PlayerCandidate::Database(i));
        }

        candidates.sort_by(|a, b| {
            let x = metric(state, a);
            let y = metric(state, b);
            let ord = match sort.as_str() {
                "rating" => y["rating"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .partial_cmp(&x["rating"].as_f64().unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal),
                "apps" => y["apps"]
                    .as_u64()
                    .unwrap_or(0)
                    .cmp(&x["apps"].as_u64().unwrap_or(0)),
                "assists" => y["assists"]
                    .as_u64()
                    .unwrap_or(0)
                    .cmp(&x["assists"].as_u64().unwrap_or(0))
                    .then(
                        y["goals"]
                            .as_u64()
                            .unwrap_or(0)
                            .cmp(&x["goals"].as_u64().unwrap_or(0)),
                    ),
                "kp" => y["kp"]
                    .as_u64()
                    .unwrap_or(0)
                    .cmp(&x["kp"].as_u64().unwrap_or(0)),
                "created" => y["created"]
                    .as_u64()
                    .unwrap_or(0)
                    .cmp(&x["created"].as_u64().unwrap_or(0)),
                "tackles" => y["tackles"]
                    .as_u64()
                    .unwrap_or(0)
                    .cmp(&x["tackles"].as_u64().unwrap_or(0)),
                "future" => y["pot"]
                    .as_u64()
                    .unwrap_or(0)
                    .cmp(&x["pot"].as_u64().unwrap_or(0))
                    .then(
                        y["ovr"]
                            .as_u64()
                            .unwrap_or(0)
                            .cmp(&x["ovr"].as_u64().unwrap_or(0)),
                    ),
                "ovr" | "overall" => y["ovr"]
                    .as_u64()
                    .unwrap_or(0)
                    .cmp(&x["ovr"].as_u64().unwrap_or(0))
                    .then(
                        y["pot"]
                            .as_u64()
                            .unwrap_or(0)
                            .cmp(&x["pot"].as_u64().unwrap_or(0)),
                    ),
                "name" => x["name"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(y["name"].as_str().unwrap_or("")),
                _ => y["goals"]
                    .as_u64()
                    .unwrap_or(0)
                    .cmp(&x["goals"].as_u64().unwrap_or(0))
                    .then(
                        y["assists"]
                            .as_u64()
                            .unwrap_or(0)
                            .cmp(&x["assists"].as_u64().unwrap_or(0)),
                    ),
            };
            ord.then(
                x["ovr"]
                    .as_u64()
                    .unwrap_or(0)
                    .cmp(&y["ovr"].as_u64().unwrap_or(0)),
            )
            .then(
                x["source_rank"]
                    .as_u64()
                    .unwrap_or(99)
                    .cmp(&y["source_rank"].as_u64().unwrap_or(99)),
            )
            .then(
                x["name"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(y["name"].as_str().unwrap_or("")),
            )
        });

        let total = candidates.len();
        let page = candidates
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|candidate| match candidate {
                PlayerCandidate::World(i) => player_detail_json(state, i),
                PlayerCandidate::Database(i) => database_player_json(state, i),
            })
            .collect::<Vec<_>>();
        json!({
            "total": total,
            "world_total": world_total,
            "database_total": database_total,
            "database_active_total": database_active_total,
            "database_only_total": database_only_total,
            "offset": offset,
            "limit": limit,
            "scope": scope,
            "sort": sort,
            "players": page,
        })
    }

    fn match_detail_json(state: &WebState, id: usize) -> Option<Value> {
        let m = state.matches.iter().find(|x| x.id == id)?;
        let w = &state.world;
        let lines = |side_a: bool| {
            m.outcome
                .stats
                .iter()
                .filter(|s| s.side_a == side_a && s.started)
                .map(|s| {
                    let p = &w.pool[s.id];
                    json!({
                        "id": s.id,
                        "name": p.name,
                        "nation": p.nation,
                        "position": p.pos.short(),
                        "ovr": p.ovr,
                        "rating": s.rating,
                        "minutes": s.minutes,
                        "goals": s.goals,
                        "assists": s.assists,
                        "shots": s.shots,
                        "on_target": s.shots_on_target,
                        "xg": s.xg,
                        "key_passes": s.key_passes,
                        "big_chances_created": s.big_chances_created,
                        "big_chances_missed": s.big_chances_missed,
                        "passes": format!("{}/{}", s.passes_cmp, s.passes_att),
                        "dribbles": format!("{}/{}", s.dribbles_cmp, s.dribbles_att),
                        "tackles": s.tackles,
                        "interceptions": s.interceptions,
                        "fouls": s.fouls,
                        "yellows": s.yellows,
                        "sent_off": s.sent_off,
                        "saves": s.saves,
                        "clean_sheet": s.clean_sheet,
                        "injury_days": s.injury_days,
                    })
                })
                .collect::<Vec<_>>()
        };
        // Substitutes: everyone who appeared without starting. `on_minute`
        // is when he entered the pitch and `replaced` the man he took over
        // from — both straight from the SubOn event.
        let subs = |side_a: bool| {
            let mut rows: Vec<Value> = m
                .outcome
                .stats
                .iter()
                .filter(|s| s.side_a == side_a && !s.started)
                .map(|s| {
                    let p = &w.pool[s.id];
                    let sub_on = m
                        .outcome
                        .events
                        .iter()
                        .find(|e| e.kind == matchsim::EventKind::SubOn && e.actor == s.id);
                    json!({
                        "id": s.id,
                        "name": p.name,
                        "nation": p.nation,
                        "position": p.pos.short(),
                        "ovr": p.ovr,
                        "rating": s.rating,
                        "minutes": s.minutes,
                        "on_minute": sub_on.map(|e| e.minute),
                        "replaced": sub_on
                            .and_then(|e| e.assist)
                            .and_then(|a| w.pool.get(a))
                            .map(|rp| rp.name.clone()),
                        "goals": s.goals,
                        "assists": s.assists,
                    })
                })
                .collect::<Vec<_>>();
            rows.sort_by_key(|r| r.get("on_minute").and_then(|v| v.as_u64()).unwrap_or(0));
            rows
        };
        let events = m
            .outcome
            .events
            .iter()
            .map(|e| {
                json!({
                    "minute": e.minute,
                    "side": if e.side_a { "home" } else { "away" },
                    "type": event_kind_text(e.kind),
                    "player": w.pool.get(e.actor).map(|p| p.name.clone()).unwrap_or_default(),
                    "assist": e.assist.and_then(|a| w.pool.get(a)).map(|p| p.name.clone()),
                })
            })
            .collect::<Vec<_>>();
        Some(json!({
            "match": match_summary(state, m),
            "home_lineup": lines(true),
            "away_lineup": lines(false),
            "home_subs": subs(true),
            "away_subs": subs(false),
            "events": events,
        }))
    }

    fn write_response(mut stream: TcpStream, status: u16, body: &str, content_type: &str) {
        let reason = match status {
            200 => "OK",
            204 => "No Content",
            400 => "Bad Request",
            404 => "Not Found",
            _ => "Error",
        };
        let response = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            status, reason, content_type, body.len(), body
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    }

    fn json_response(stream: TcpStream, status: u16, value: &Value) {
        write_response(
            stream,
            status,
            &serde_json::to_string(value).unwrap_or_default(),
            "application/json",
        );
    }

    fn read_http_request(mut stream: TcpStream) -> Option<(String, String, String, String)> {
        let mut reader = BufReader::new(stream.try_clone().ok()?);
        let mut request_line = String::new();
        reader.read_line(&mut request_line).ok()?;
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("GET").to_string();
        let target = parts.next().unwrap_or("/").to_string();
        let mut content_length = 0usize;
        loop {
            let mut header = String::new();
            let n = reader.read_line(&mut header).ok()?;
            if n == 0 || header == "\r\n" || header == "\n" {
                break;
            }
            if let Some((k, v)) = header.split_once(':') {
                if k.trim().eq_ignore_ascii_case("content-length") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = Vec::new();
        if content_length > 0 {
            body.resize(content_length, 0);
            reader.read_exact(&mut body).ok()?;
        }
        let mut split = target.splitn(2, '?');
        // Percent-decode the path so browser-encoded stable handles such as
        // `db%3A123` and `wp%3A45` route identically to `db:123`/`wp:45`.
        let path = url_decode(split.next().unwrap_or("/"));
        let query = split.next().unwrap_or_default().to_string();
        Some((
            method,
            path,
            query,
            String::from_utf8_lossy(&body).into_owned(),
        ))
    }

    fn static_path(request_path: &str) -> Option<PathBuf> {
        let normalized = request_path.trim_start_matches('/');
        let candidate = if normalized.is_empty() || !normalized.contains('.') {
            PathBuf::from("web/index.html")
        } else {
            PathBuf::from("web").join(normalized)
        };
        let canonical = candidate.canonicalize().ok()?;
        let web_root = PathBuf::from("web").canonicalize().ok()?;
        if canonical.starts_with(&web_root) && canonical.is_file() {
            Some(canonical)
        } else {
            None
        }
    }

    fn content_type(path: &Path) -> &'static str {
        match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
            "html" | "htm" => "text/html; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "js" | "mjs" => "text/javascript; charset=utf-8",
            "json" => "application/json",
            "svg" => "image/svg+xml",
            "png" => "image/png",
            "ico" => "image/x-icon",
            _ => "application/octet-stream",
        }
    }

    fn handle_stream(mut stream: TcpStream, state: Arc<Mutex<WebState>>) {
        let Ok(stream_clone) = stream.try_clone() else {
            return;
        };
        let Some((method, path, query, body)) = read_http_request(stream_clone) else {
            return;
        };

        if method.eq_ignore_ascii_case("OPTIONS") {
            write_response(stream, 204, "", "text/plain");
            return;
        }

        if !path.starts_with("/api/") {
            if let Some(file) = static_path(&path) {
                if let Ok(bytes) = std::fs::read(&file) {
                    let _ = stream.write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            content_type(&file),
                            bytes.len()
                        )
                        .as_bytes(),
                    );
                    let _ = stream.write_all(&bytes);
                    let _ = stream.flush();
                }
            } else {
                json_response(stream, 404, &json!({"error": "not found"}));
            }
            return;
        }

        let result = if path == "/api/health" {
            json!({"ok": true, "service": "football-api", "version": "0.1.0", "world_ready": true})
        } else if path == "/api/overview" {
            let s = state.lock().unwrap();
            overview_json(&s)
        } else if path == "/api/leagues" {
            let s = state.lock().unwrap();
            json!({"leagues": leagues_json(&s)})
        } else if path.starts_with("/api/league/") {
            let idx = path
                .trim_start_matches("/api/league/")
                .parse::<usize>()
                .unwrap_or(0);
            let s = state.lock().unwrap();
            if idx == UCL_LEAGUE_ID {
                ucl_detail_json(&s)
            } else if idx == INTL_LEAGUE_ID {
                let season = query_value(&query, "season").and_then(|v| v.parse::<u16>().ok());
                internationals_json(&s, season)
            } else if idx >= s.world.league_teams.len() || idx == 5 {
                json!({"error": "league not available"})
            } else {
                league_detail_json(&s, idx)
            }
        } else if path == "/api/status" || path == "/api/week/status" {
            let s = state.lock().unwrap();
            status_json(&s)
        } else if path == "/api/fixtures" || path == "/api/fixtures/next" {
            let s = state.lock().unwrap();
            let target = query_value(&query, "week").and_then(|v| v.parse::<usize>().ok());
            json!({"week": target.unwrap_or((s.week + 1) as usize), "fixtures": fixtures_json(&s, target)})
        } else if path == "/api/matches" {
            let s = state.lock().unwrap();
            let week_filter = query_value(&query, "week");
            let season_filter =
                query_value(&query, "season").and_then(|v| v.parse::<u16>().ok());
            let league_filter = query_value(&query, "league").and_then(|v| v.parse::<usize>().ok());
            let matches = s
                .matches
                .iter()
                .rev()
                .filter(|m| {
                    season_filter
                        .map(|sy| m.season == sy)
                        .unwrap_or(true)
                })
                .filter(|m| match &week_filter {
                    Some(v) if v != "all" => week_filter
                        .as_ref()
                        .and_then(|x| x.parse::<u32>().ok())
                        .map(|w| m.week == w)
                        .unwrap_or(true),
                    _ => true,
                })
                .filter(|m| league_filter.map(|li| m.league_idx == li).unwrap_or(true))
                .map(|m| match_summary(&s, m))
                .collect::<Vec<_>>();
            json!({"matches": matches, "count": s.matches.len()})
        } else if path.starts_with("/api/match/") {
            let id = path
                .trim_start_matches("/api/match/")
                .parse::<usize>()
                .unwrap_or(0);
            let s = state.lock().unwrap();
            match_detail_json(&s, id).unwrap_or_else(|| json!({"error": "match not found"}))
        } else if path == "/api/players" || path == "/api/players/search" {
            let s = state.lock().unwrap();
            players_json(&s, &query)
        } else if path.starts_with("/api/player/") {
            let rest = path.trim_start_matches("/api/player/");
            let s = state.lock().unwrap();
            if rest.ends_with("/matches") {
                let handle = rest.trim_end_matches("/matches");
                let world_idx = handle
                    .strip_prefix("world:")
                    .or_else(|| handle.strip_prefix("wp:"))
                    .or_else(|| handle.strip_prefix("world/"))
                    .or_else(|| handle.strip_prefix("wp/"))
                    .and_then(|v| v.parse::<usize>().ok());
                match world_idx {
                    Some(i) if i < s.world.pool.len() => player_matches_json(&s, i),
                    _ => json!({"error": "player not found"}),
                }
            } else if let Some(database_id) = rest
                .strip_prefix("db:")
                .or_else(|| rest.strip_prefix("db/"))
                .and_then(|v| v.parse::<PlayerId>().ok())
            {
                match s.database_index.get(&database_id) {
                    Some(&db_idx) => database_player_json(&s, db_idx),
                    None => json!({"error": "database player not found"}),
                }
            } else if let Some(world_id) = rest
                .strip_prefix("world:")
                .or_else(|| rest.strip_prefix("wp:"))
                .or_else(|| rest.strip_prefix("world/"))
                .or_else(|| rest.strip_prefix("wp/"))
                .and_then(|v| v.parse::<usize>().ok())
            {
                if world_id < s.world.pool.len() {
                    player_detail_json(&s, world_id)
                } else {
                    json!({"error": "player not found"})
                }
            } else if let Ok(id) = rest.parse::<usize>() {
                if id < s.world.pool.len() {
                    player_detail_json(&s, id)
                } else {
                    json!({"error": "player not found"})
                }
            } else {
                json!({"error": "player not found"})
            }
        } else if path.starts_with("/api/team/") && path.ends_with("/recent") {
            let raw = path
                .trim_start_matches("/api/team/")
                .trim_end_matches("/recent");
            let idx = raw.parse::<usize>().unwrap_or(0);
            let s = state.lock().unwrap();
            if idx >= s.world.teams.len() {
                json!({"error": "team not found"})
            } else {
                team_recent_json(&s, idx)
            }
        } else if path.starts_with("/api/player/") && path.ends_with("/matches") {
            let raw = path
                .trim_start_matches("/api/player/")
                .trim_end_matches("/matches");
            let world_idx = raw
                .strip_prefix("world:")
                .or_else(|| raw.strip_prefix("wp:"))
                .or_else(|| raw.strip_prefix("world/"))
                .or_else(|| raw.strip_prefix("wp/"))
                .and_then(|v| v.parse::<usize>().ok());
            let s = state.lock().unwrap();
            match world_idx {
                Some(i) if i < s.world.pool.len() => player_matches_json(&s, i),
                _ => json!({"error": "player not found"}),
            }
        } else if path == "/api/manager/clubs" {
            let s = state.lock().unwrap();
            let clubs = (0..s.world.teams.len())
                .map(|i| team_json(&s.world, i))
                .collect::<Vec<_>>();
            json!({"clubs": clubs, "count": clubs.len()})
        } else if path == "/api/manager/team" {
            let team = query_value(&query, "team")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            let s = state.lock().unwrap();
            manager_team_json(&s, team)
        } else if path == "/api/manager/action" && method.eq_ignore_ascii_case("POST") {
            let payload = serde_json::from_str::<Value>(&body).unwrap_or(json!({}));
            let mut s = state.lock().unwrap();
            manager_action_json(&mut s, &payload)
        } else if path == "/api/seasons" {
            let s = state.lock().unwrap();
            let mut seasons: Vec<Value> = s
                .archives
                .iter()
                .map(|a| {
                    json!({
                        "year": a.get("year"),
                        "season": a.get("season"),
                        "champions": a.get("leagues").map(|ls| {
                            Value::Array(ls.as_array().map(|arr| {
                                arr.iter()
                                    .map(|l| {
                                        json!({
                                            "league": l.get("name"),
                                            "champion": l.get("champion"),
                                        })
                                    })
                                    .collect::<Vec<_>>()
                            }).unwrap_or_default())
                        }).unwrap_or(json!([])),
                        "ucl_winner": a.pointer("/ucl/final/winner/name"),
                        "international": a.get("international").map(|i| json!({
                            "kind": i.get("kind"), "winner": i.get("winner"),
                        })).unwrap_or(Value::Null),
                    })
                })
                .collect();
            seasons.push(json!({
                "year": s.year,
                "season": format!("{}-{}", s.year, (s.year + 1) % 100),
                "champions": [],
                "ucl_winner": Value::Null,
                "international": Value::Null,
                "current": true,
            }));
            json!({"seasons": seasons, "current_year": s.year})
        } else if let Some(year_s) = path.strip_prefix("/api/seasons/") {
            let year = year_s.parse::<u16>().ok();
            let s = state.lock().unwrap();
            match year.filter(|y| *y < s.year) {
                Some(y) => s
                    .archives
                    .iter()
                    .find(|a| a.get("year").and_then(|v| v.as_u64()) == Some(y as u64))
                    .cloned()
                    .unwrap_or_else(|| json!({"error": "season not found"})),
                _ => json!({"error": "season not found"}),
            }
        } else if path == "/api/careers" {
            match std::fs::read_to_string("sim_dump/careers.json") {
                Ok(text) => serde_json::from_str::<Value>(&text).unwrap_or(json!({"raw": text})),
                Err(e) => json!({"error": e.to_string()}),
            }
        } else if path == "/api/simulate/week" && method.eq_ignore_ascii_case("POST") {
            let weeks = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v.get("weeks")
                        .and_then(|x| x.as_u64())
                        .or_else(|| v.get("count").and_then(|x| x.as_u64()))
                })
                .unwrap_or(1)
                .clamp(1, 10) as usize;
            let mut s = state.lock().unwrap();
            let started = s.matches.len();
            let mut played = 0usize;
            for _ in 0..weeks {
                played += s.simulate_one_week();
            }
            let played = played.max(s.matches.len().saturating_sub(started));
            json!({"advanced": weeks, "matches_played": played, "week": s.week, "remaining_rounds": s.remaining_rounds()})
        } else if path == "/api/world/reset" && method.eq_ignore_ascii_case("POST") {
            let seed = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v.get("seed")
                        .and_then(|x| x.as_u64())
                        .or_else(|| v.get("world_seed").and_then(|x| x.as_u64()))
                })
                .unwrap_or(20_260_101);
            let mut s = state.lock().unwrap();
            *s = WebState::new(seed);
            status_json(&s)
        } else {
            json!({"error": "unknown endpoint", "path": path})
        };

        let status = if result.get("error").is_some() {
            404
        } else {
            200
        };
        json_response(stream, status, &result);
    }

    pub fn serve_web_api() {
        // Stable default seed: a restart reproduces the same world, so the
        // league tables and the Champions League draw are auditable. Pin a
        // different world with FM_API_SEED; the dashboard's "New world" button
        // still generates a fresh random one.
        let seed = std::env::var("FM_API_SEED")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(20_260_101);
        let port = std::env::var("PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(8080);
        let started = Instant::now();
        let state = Arc::new(Mutex::new(WebState::new(seed)));
        let listener = TcpListener::bind(("0.0.0.0", port)).expect("could not bind API port");
        println!(
            "FM Observatory web API + dashboard: http://localhost:{}/ (ready in {:?})",
            port,
            started.elapsed()
        );
        for stream in listener.incoming() {
            match stream {
                Ok(s) => {
                    let st = Arc::clone(&state);
                    std::thread::spawn(move || handle_stream(s, st));
                }
                Err(_) => continue,
            }
        }
    }
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
    fn mkp(name: &str, pos: Position, v: u8, age: u8, pot: u8, team: usize, homegrown: bool) -> WP {
        WP {
            db_id: None,
            name: name.to_string(),
            nation: "Testland".to_string(),
            pos,
            attrs: attrs_all(v),
            ovr: v,
            pot,
            age,
            team,
            retired: false,
            intl_retired: false,
            intl_retire_age: 0,
            club_retire_age: 0,
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
        ban_games: 0,
            shots: 0,
            sot: 0,
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
            career_earnings: 0,
            signing_bonus: 0,
            bonus_paid: 0,
            contract_option: false,
            locked: 0,
            path: Vec::new(),
            career_goals: 0,
            career_assists: 0,
            career_kp: 0,
            career_bcc: 0,
            career_ti: 0,
            career_cs: 0,
            career_apps: 0,
            career_rating_sum: 0.0,
            career_rating_apps: 0,
            ballons: 0,
            peak: 0,
            peak_str: 1,
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
            nation: "Testland".to_string(),
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
            consec_titles: 0,
            momentum: 0,
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
            big.push(push(mkp(
                &format!("Big Def {}", k),
                Position::DEF,
                80,
                27,
                80,
                0,
                false,
            )));
        }
        for k in 0..4 {
            big.push(push(mkp(
                &format!("Big Mid {}", k),
                Position::MID,
                80,
                26,
                80,
                0,
                false,
            )));
        }
        for k in 0..2 {
            big.push(push(mkp(
                &format!("Big Fwd {}", k),
                Position::FWD,
                80,
                25,
                80,
                0,
                false,
            )));
        }
        let prospect = push(mkp("Kid Prospect", Position::MID, 70, 19, 85, 0, true));
        big.push(prospect);
        let mut small: Vec<usize> = Vec::new();
        small.push(push(mkp(
            "Small Keeper",
            Position::GK,
            65,
            29,
            65,
            1,
            false,
        )));
        for k in 0..4 {
            small.push(push(mkp(
                &format!("Small Def {}", k),
                Position::DEF,
                65,
                28,
                65,
                1,
                false,
            )));
        }
        for k in 0..2 {
            small.push(push(mkp(
                &format!("Small Mid {}", k),
                Position::MID,
                65,
                27,
                65,
                1,
                false,
            )));
        }
        small.push(push(mkp(
            "Small Weakmid",
            Position::MID,
            55,
            30,
            55,
            1,
            false,
        )));
        for k in 0..3 {
            small.push(push(mkp(
                &format!("Small Fwd {}", k),
                Position::FWD,
                65,
                26,
                65,
                1,
                false,
            )));
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
            league_meta: vec![
                LeagueMeta {
                    label: "Premier League".into(),
                    confed: Some(crate::world_model::Confederation::UEFA),
                },
                LeagueMeta {
                    label: "La Liga".into(),
                    confed: Some(crate::world_model::Confederation::UEFA),
                },
            ],
            nation_pool: Vec::new(),
            next_regen: 1,
            elite_cap: 90,
            inflation: 1.0,
            loans: Vec::new(),
            nemesis: Vec::new(),
            continent_hist: Vec::new(),
            host_queue: Vec::new(),
            recent_hosts: Vec::new(),
            wc_hist: Vec::new(),
            free_agents: Vec::new(),
            national_states: HashMap::new(),
            used_names: HashSet::new(),
            params: Params::default(),
        }
    }

    #[test]
    fn loans_flow_to_starters_and_return() {
        let mut rng = Pcg64Mcg::new(3);
        let mut w = mini_world(&mut rng);
        let prospect = w
            .pool
            .iter()
            .position(|p| p.name == "Kid Prospect")
            .unwrap();
        transfer_window(&mut w, &mut rng);
        // The prospect must be on loan at the small club (a starter there),
        // not sold, not retired, with the loan recorded and a fee paid.
        assert_eq!(
            w.loans.len(),
            1,
            "expected exactly one loan, got {:?}",
            w.loans.len()
        );
        let loan = w.loans[0];
        assert_eq!((loan.player, loan.home, loan.away), (prospect, 0, 1));
        assert!(w.pool[prospect].on_loan);
        assert_eq!(w.pool[prospect].team, 1);
        assert!(w.teams[1].players.contains(&prospect));
        assert!(!w.teams[0].players.contains(&prospect));
        assert!(!w.pool[prospect].retired);
        assert!(
            w.pool[prospect].homegrown,
            "loans must keep club-trained status"
        );
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
            w.pool.push(mkp(
                &format!("Filler {}", k),
                Position::MID,
                78,
                24,
                78,
                0,
                false,
            ));
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
        let prospect = w
            .pool
            .iter()
            .position(|p| p.name == "Kid Prospect")
            .unwrap();
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

        let s_dm = ballon_score(&dm, rep, year, 1.0);
        let s_modest = ballon_score(&modest, rep, year, 1.0);
        let s_elite = ballon_score(&elite, rep, year, 1.0);
        assert!(
            s_dm > s_modest,
            "all-round DM {:.0} should beat a modest forward {:.0}",
            s_dm,
            s_modest
        );
        assert!(
            s_elite > s_dm,
            "a prolific forward {:.0} should still edge the DM {:.0}",
            s_elite,
            s_dm
        );
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
        assert!(
            reg.contains(&kid),
            "homegrown floor must rescue the last-ranked prospect"
        );
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
        assert_eq!(
            super_cup_winner(&[0], &w.teams, &w.params, &mut rng),
            Some(0)
        );
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
        assert!(
            fullbacks > 30,
            "almost no full-backs generated ({}/200)",
            fullbacks
        );
        assert!(
            centre > 30,
            "almost no centre-backs generated ({}/200)",
            centre
        );
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
            develop_player(&mut pool[0], 1.0, 1.1, 5, &mut rng);
        }
        let s = pool[0].attrs.shooting;
        assert!(s >= 82, "wonderkid must actually develop, got {}", s);
        assert!(s <= 96, "wonderkid must not stroll to 99, got {}", s);
        let ovr = pool[0].attrs.overall(pool[0].pos);
        assert!(ovr <= 95, "overall must stay GOAT-plausible, got {}", ovr);
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
                    + t.revenue_last as f32 * effective_will * w.params.budget_ratio)
                    as i64
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
        w.pool
            .push(mkp("Deadwood", Position::MID, 70, 24, 70, 0, false));
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
        assert!(
            !w.pool[deadwood].retired,
            "surplus man destroyed instead of sold"
        );
    }

    #[test]
    fn transfer_willing_requested_goes_lateral() {
        // A requesting settled starter may leave laterally (soft clause) but
        // never downward for money: dynasties rot through exits, not freefalls.
        assert!(transfer_willing(
            false, false, true, 0.9, 0.5, 95, 99, 4.0, 8.0, 0, 70, 0.8
        ));
        assert!(transfer_willing(
            false, false, true, 0.9, 0.5, 99, 99, 4.0, 8.0, 0, 70, 0.8
        ));
        assert!(!transfer_willing(
            false, false, true, 0.9, 0.5, 70, 99, 4.0, 8.0, 0, 70, 0.8
        ));
        // Unrequested settled starters need a clearly bigger club.
        assert!(!transfer_willing(
            false, false, false, 0.9, 0.5, 95, 99, 4.0, 8.0, 0, 70, 0.8
        ));
        assert!(transfer_willing(
            false, false, false, 0.9, 0.5, 99, 90, 4.0, 8.0, 0, 70, 0.8
        ));
        // Surplus men circulate within 15 rep points; ambitious ones refuse
        // even smaller drops, while low-ambition veterans wind down freely.
        assert!(transfer_willing(
            true, false, false, 0.9, 0.5, 80, 90, 4.0, 8.0, 0, 70, 0.8
        ));
        assert!(!transfer_willing(
            true, false, false, 0.9, 0.5, 70, 90, 4.0, 8.0, 0, 70, 0.8
        ));
        assert!(transfer_willing(
            true, false, false, 0.3, 0.5, 76, 90, 4.0, 8.0, 0, 70, 0.8
        ));
        assert!(!transfer_willing(
            true, false, false, 0.3, 0.5, 70, 90, 4.0, 8.0, 0, 70, 0.8
        ));
        // Contract control no longer means a hard lock: a bigger club's
        // credible offer can be accepted when the player is open to moving
        // up. Downward or lateral desertion still needs a request or
        // desperation, and elite men do not drop a tier while content.
        assert!(!transfer_willing(
            true, false, false, 0.9, 0.5, 88, 90, 4.0, 8.0, 4, 86, 0.8
        ));
        assert!(!transfer_willing(
            true, false, false, 0.9, 0.5, 76, 90, 4.0, 8.0, 1, 86, 0.8
        ));
        assert!(transfer_willing(
            true, false, true, 0.9, 0.5, 88, 90, 4.0, 8.0, 4, 86, 0.25
        ));
        assert!(transfer_willing(
            true, true, false, 0.9, 0.5, 88, 90, 4.0, 8.0, 5, 89, 0.9
        ));
        // A long deal is settleable for a genuine move up: an ambitious,
        // not-very-loyal prodigy at a small club can leave for a giant.
        assert!(transfer_willing(
            false, false, false, 0.92, 0.55, 99, 60, 4.0, 8.0, 5, 94, 0.72
        ));
        // The same offer does not move a loyal, content player.
        assert!(!transfer_willing(
            false, false, false, 0.92, 0.95, 99, 60, 4.0, 8.0, 5, 94, 0.72
        ));
        // And an open player still does not drop down a tier on a long deal.
        assert!(!transfer_willing(
            false, false, false, 0.92, 0.55, 60, 99, 4.0, 8.0, 5, 94, 0.72
        ));
    }

    #[test]
    fn below_market_clause_creates_opportunistic_target() {
        let mut pool = Vec::new();
        let mut buyer = Vec::new();
        for (pos, count, rating) in [
            (Position::GK, 1, 90),
            (Position::DEF, 4, 70),
            (Position::MID, 3, 90),
            (Position::FWD, 3, 90),
        ] {
            for n in 0..count {
                let i = pool.len();
                pool.push(mkp(
                    &format!("Starter-{pos:?}-{n}"),
                    pos,
                    rating,
                    25,
                    rating,
                    0,
                    false,
                ));
                buyer.push(i);
            }
        }
        let (need_pos, incumbent) = weakest_slot(&buyer, &pool);
        assert_eq!(need_pos, Position::DEF);

        let mut prodigy = mkp("Clause prodigy", Position::FWD, 94, 15, 99, 1, false);
        prodigy.value = 225;
        prodigy.release_clause = Some(5);
        prodigy.locked = 5;
        let replacement = acquisition_incumbent(&buyer, &pool, &prodigy, need_pos, incumbent);
        assert_eq!(replacement, Some(90.0));
        assert!(94.0 >= replacement.unwrap() + 2.0);

        prodigy.release_clause = None;
        assert_eq!(
            acquisition_incumbent(&buyer, &pool, &prodigy, need_pos, incumbent),
            None
        );
        prodigy.release_clause = Some(300);
        assert!(!is_clause_opportunity(&prodigy));
    }

    #[test]
    fn release_clause_must_be_paid_in_full() {
        assert_eq!(agreed_transfer_fee(5, 100, 0.75, 0.80, true), Some(5));
        assert_eq!(agreed_transfer_fee(5, 4, 0.75, 0.80, true), None);
        // An ordinary distressed sale may still accept the opening offer.
        assert_eq!(agreed_transfer_fee(5, 100, 0.75, 0.80, false), Some(4));
    }

    #[test]
    fn federation_infrastructure_beats_raw_population_volume() {
        assert!(federation_capacity("Japan") > federation_capacity("China PR"));
        assert!(federation_capacity("China PR") > federation_capacity("India"));
        assert_eq!(federation_academy_cap("Japan"), 5);
        assert_eq!(federation_academy_cap("China PR"), 4);
        assert_eq!(federation_academy_cap("India"), 3);
        assert!(federation_player_cap("Japan", 90) > federation_player_cap("China PR", 90));
        assert!(federation_player_cap("China PR", 90) > federation_player_cap("India", 90));
        assert!(!valid_nationality("South America"));
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
            let moves = post_season_evolution(&mut w, &tables, &mut rng);
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
            let moves = post_season_evolution(&mut w, &tables, &mut rng);
            if moves.iter().any(|m| m.contains("retires")) {
                seen = true;
                break;
            }
        }
        assert!(
            seen,
            "a 71-year-old manager never retired across 20 seasons"
        );
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
    fn international_retirement_precedes_club_career() {
        let mut rng = Pcg64Mcg::new(41);

        let mut old = mkp("Old Veteran", Position::MID, 72, 35, 72, 0, false);
        old.attrs = attrs_all(72);
        old.attrs.pace = 42;
        old.attrs.stamina = 42;
        assert!(
            intl_retirement_prob(&old, 76) > 0.35,
            "a 35-year-old who has fallen below his federation's call-up floor should be a strong international-retirement candidate"
        );

        let mut prime = mkp("Prime Mid", Position::MID, 82, 29, 85, 0, false);
        prime.attrs = attrs_all(82);
        assert_eq!(
            intl_retirement_prob(&prime, 80),
            0.0,
            "international careers should not end at 29 merely because of ordinary age"
        );

        let mut timeless = mkp("Timeless Star", Position::FWD, 89, 36, 89, 0, false);
        timeless.attrs = attrs_all(89);
        assert!(
            intl_retirement_prob(&timeless, 82) < intl_retirement_prob(&old, 76),
            "a genuine elite should be far less likely to leave the national team than a declining veteran"
        );

        let mut keeper = mkp("Old Keeper", Position::GK, 75, 35, 75, 0, false);
        keeper.attrs = attrs_all(75);
        keeper.attrs.goalkeeping = 64;
        assert!(
            intl_retirement_prob(&keeper, 78) > 0.15,
            "goalkeeper decline should also end international selection"
        );

        let _ = rng;
        assert_eq!(retire_prob(33, false, 75, false), 0.0);
        assert_eq!(retire_prob(35, false, 75, false), 0.0);
        assert!(retire_prob(38, false, 75, false) > 0.5);
        assert!(retire_prob(41, false, 75, false) < retire_prob(38, false, 75, false));
        assert_eq!(retire_prob(45, false, 75, false), 1.0);
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
