//! Possession-phase match engine — 22 individuals interacting, not a team-strength dice roll.
//!
//! A match is a sequence of ~190 possessions. Each possession, the side in
//! control tries to progress through three phases — build-up, progression,
//! final third — and every phase is a contest between a *specific* attacker and
//! a *specific* defender, resolved from their attributes (plus tactics, venue,
//! fatigue and the current scoreline). Nothing here is a flat constant unmoored
//! from the players: pass completion, dribbles, chances, shots, saves, fouls and
//! even injuries all fall out of attribute contests.
//!
//! Emergent properties we rely on instead of special-casing them:
//! - A squad of pure poachers with nobody to supply the final ball creates few
//!   chances, because the final-third phase picks a *creator* weighted by real
//!   creativity — "all poachers is chaotic" falls out of the mechanics.
//! - Assists are credited to whoever actually played the killer pass for *this*
//!   goal, so a 40-goal striker who also creates gets real assists, and
//!   playmakers rack them up. The 59g/3a artefact cannot happen here.
//! - A losing manager pushes the mentality up (scaled by ambition and time
//!   left); the opponent reads the same scoreline and drops off to counter.
//!   Two-legged ties feed the aggregate in, so 3-0 down away starts the return
//!   leg on the front foot.

use rand::Rng;

use crate::model::{Attributes, Position, Role};

/// A player as the match engine sees them. `id` is the caller's own index
/// (e.g. into the season pool); the engine only reports it back.
#[derive(Clone)]
pub struct MatchPlayer {
    pub id: usize,
    pub pos: Position,
    pub role: Role,
    pub attrs: Attributes,
    /// Starting fatigue, 0.0 fresh .. 1.0 spent (from the season timeline).
    pub start_fatigue: f32,
    /// Morale/adaptation multiplier on ability, ~0.8 .. 1.15.
    pub form: f32,
}

/// One side's team sheet plus the two manager dials the engine actually uses.
pub struct Side<'a> {
    pub players: &'a [MatchPlayer],
    /// Manager ambition 0.6 (pragmatic) .. 1.5 (gung-ho): sets the resting
    /// mentality and how hard they chase a deficit.
    pub ambition: f32,
    /// Manager quality ~0.82 .. 1.20: a small edge on every contest.
    pub quality: f32,
    /// Own XI strength over the opponent's (1.0 = even), read off the two
    /// lineups' attributes — never reputation. Drives the pre-match tactical
    /// read (underdogs sit off, favourites press on).
    pub threat: f32,
}

pub enum Venue {
    HomeA,
    HomeB,
    Neutral,
}

/// Per-player match line. Every counter is accrued from real events.
#[derive(Clone, Debug, Default)]
pub struct MatchStats {
    pub id: usize,
    pub side_a: bool,
    pub minutes: u8,
    pub goals: u8,
    pub assists: u8,
    pub shots: u8,
    pub shots_on_target: u8,
    /// Expected goals accumulated from this player's shots (chance quality).
    pub xg: f32,
    /// Expected assists: the xG of the chances this player's final ball
    /// produced for others — creation scored on chance quality, not just on
    /// whether the finisher happened to bury it.
    pub xa: f32,
    /// Aerial duels won/lost (headers challenged at set pieces and crosses),
    /// the currency centre-backs and target men are actually judged on.
    pub aerials_won: u8,
    pub aerials_lost: u8,
    pub big_chances_created: u8,
    pub big_chances_missed: u8,
    pub key_passes: u8,
    pub passes_att: u16,
    pub passes_cmp: u16,
    pub dribbles_att: u8,
    pub dribbles_cmp: u8,
    pub tackles: u8,
    pub interceptions: u8,
    pub fouls: u8,
    /// Goalkeeper saves (on-target shots kept out).
    pub saves: u8,
    /// Kept a clean sheet (GK/defenders of a side that conceded nothing).
    pub clean_sheet: bool,
    pub rating: f32,
    pub injury_days: u16,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EventKind {
    Goal,
    /// A goal from a dead ball (corner or free kick) — tracked separately so
    /// set-piece threat shows up in reports and the observer ticker.
    SetPiece,
    Injury,
    Penalty,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub minute: u8,
    pub side_a: bool,
    pub kind: EventKind,
    /// Caller id of the primary actor (scorer / injured player).
    pub actor: usize,
    /// Caller id of the assister, when there is one.
    pub assist: Option<usize>,
}

pub struct MatchOutcome {
    pub goals_a: u8,
    pub goals_b: u8,
    pub poss_a: u8,
    pub shots_a: u16,
    pub shots_b: u16,
    pub xg_a: f32,
    pub xg_b: f32,
    pub corners_a: u16,
    pub corners_b: u16,
    pub sp_goals_a: u8,
    pub sp_goals_b: u8,
    /// All 22 starters' lines (side_a flag distinguishes teams).
    pub stats: Vec<MatchStats>,
    pub events: Vec<Event>,
    /// Final mentality each manager settled on (−3 very defensive .. 3 very attacking).
    pub end_mentality_a: i8,
    pub end_mentality_b: i8,
}

fn wv(v: u8) -> f32 {
    v as f32
}

/// Pre-match strength ratio between two lineups, read purely off attributes
/// (midfield control + overall quality): (a/b, b/a), clamped to a sane band.
/// Callers set both sides' `threat` from this before simulating, so tactical
/// reads never touch reputation — only the eleven on the pitch.
pub fn threat_ratio(a: &Side, b: &Side) -> (f32, f32) {
    let ra = Rt::new(a, true);
    let rb = Rt::new(b, false);
    let ea = (ra.control() + ra.quality_index()).max(1.0);
    let eb = (rb.control() + rb.quality_index()).max(1.0);
    ((ea / eb).clamp(0.25, 4.0), (eb / ea).clamp(0.25, 4.0))
}

/// The manager's in-match "read": resting mentality set by ambition, then
/// bent by the scoreline (including any first-leg aggregate) and how much time
/// is left. A gung-ho manager 2-0 down at 70' throws bodies forward; a
/// pragmatic one nursing a lead shuts up shop. `threat` is own/opp squad
/// strength: underdogs start cagier (parking the bus against superior sides is
/// read off the matchup, not assigned), favourites start on the front foot.
/// Returns −3..3.
pub fn decide_mentality(ambition: f32, goal_diff: i32, minute: u8, quality: f32, threat: f32) -> i8 {
    // Resting stance: ambitious managers sit a notch higher.
    let mut m = (ambition - 1.0) * 2.0 + (quality - 1.0) * 0.5;
    // Pre-match read of the matchup: half as strong parks the bus, twice as
    // strong comes out to attack. Logarithmic so mismatches saturate. Kept
    // modest — it should tilt cageyness, not lock the whole league in.
    m += 0.5 * (threat.max(0.05).ln() / 2.0f32.ln()).clamp(-1.5, 1.5);
    // How urgent is the clock? Ramps after the hour, peaks at the death.
    let urgency = ((minute as f32 - 45.0) / 45.0).clamp(0.0, 1.0);
    if goal_diff < 0 {
        // Chasing: push up with the deficit, the clock, and ambition.
        let deficit = (-goal_diff) as f32;
        m += (0.9 + urgency * 1.8) * deficit.min(3.0) * (0.6 + ambition * 0.5);
    } else if goal_diff > 0 {
        // Protecting: drop off, harder late and for cautious managers.
        let lead = goal_diff as f32;
        m -= (0.5 + urgency * 1.3) * lead.min(3.0) * (1.4 - ambition * 0.4);
    }
    m.round().clamp(-3.0, 3.0) as i8
}

/// Runtime state for one side. Static per-skill arrays are precomputed once;
/// their *effective* value each contest folds in form and live fatigue.
struct Rt {
    p: Vec<MatchPlayer>,
    st: Vec<MatchStats>,
    fat: Vec<f32>,
    out: Vec<bool>,
    passing: Vec<f32>,
    dribble: Vec<f32>,
    creation: Vec<f32>,
    finish: Vec<f32>,
    defend: Vec<f32>,
    press: Vec<f32>,
    aerial: Vec<f32>,
    gk: Vec<f32>,
    /// How often the player shoots when a chance falls his way — from his role
    /// (which itself comes from his attributes). Finishers hunt shots, deep
    /// playmakers and anchors recycle instead. This is what stops a Rodri-type
    /// topping a scoring chart while keeping Bellingham-types chipping in.
    shoot_tend: Vec<f32>,
    ambition: f32,
    quality: f32,
    goals: u8,
    corners_won: u16,
    sp_goals: u8,
    /// This side's most dangerous finisher (set pre-match): the man the
    /// opposition's marker will shadow into every duel.
    danger: Option<usize>,
    /// This side's designated stopper (set pre-match): shadows the
    /// opponent's danger man. Shadowing only works if he's good enough.
    marker: Option<usize>,
}

/// How much a role actually competes for the ball. The anchor and the
/// centre-backs live in the duels, the full-backs join in, and a poacher or an
/// advanced playmaker is almost never the man who makes the challenge. Weighting
/// every ball-winning duel by role is what puts tackles and interceptions on the
/// players who win them in real life — without it the midfield five share every
/// turnover evenly, and an attacking playmaker out-tackles N'Golo Kanté.
fn role_defence_w(role: Role) -> f32 {
    match role {
        Role::Anchor => 1.45,
        Role::CentralDefender | Role::BallPlayingDefender => 1.30,
        Role::BoxToBox => 1.15,
        Role::FullBack | Role::WingBack => 1.10,
        Role::DeepLyingPlaymaker => 1.05,
        Role::Winger => 0.55,
        Role::FalseNine | Role::InsideForward | Role::AdvancedPlaymaker => 0.45,
        Role::TargetMan => 0.35,
        Role::ShadowStriker => 0.30,
        Role::Poacher => 0.25,
        Role::Goalkeeper | Role::SweeperKeeper => 0.0,
    }
}

/// Shooting tendency by role: the shot-hunting of a poacher vs the restraint
/// of a holding midfielder, emerging from where each role operates.
fn shoot_tendency(role: Role) -> f32 {
    match role {
        Role::Poacher
        | Role::ShadowStriker
        | Role::TargetMan
        | Role::InsideForward
        | Role::FalseNine
        | Role::Winger => 1.15,
        Role::AdvancedPlaymaker | Role::BoxToBox => 0.9,
        Role::WingBack => 0.5,
        Role::DeepLyingPlaymaker => 0.5,
        Role::FullBack => 0.35,
        Role::Anchor => 0.35,
        Role::CentralDefender | Role::BallPlayingDefender => 0.3,
        Role::Goalkeeper | Role::SweeperKeeper => 0.0,
    }
}

impl Rt {
    fn new(side: &Side, side_a: bool) -> Rt {
        let n = side.players.len();
        let mut rt = Rt {
            p: side.players.to_vec(),
            st: Vec::with_capacity(n),
            fat: vec![0.0; n],
            out: vec![false; n],
            passing: Vec::with_capacity(n),
            dribble: Vec::with_capacity(n),
            creation: Vec::with_capacity(n),
            finish: Vec::with_capacity(n),
            defend: Vec::with_capacity(n),
            press: Vec::with_capacity(n),
            aerial: Vec::with_capacity(n),
            gk: Vec::with_capacity(n),
            shoot_tend: Vec::with_capacity(n),
            ambition: side.ambition,
            quality: side.quality,
            goals: 0,
            corners_won: 0,
            sp_goals: 0,
            danger: None,
            marker: None,
        };
        for mp in side.players {
            let a = &mp.attrs;
            rt.st.push(MatchStats {
                id: mp.id,
                side_a,
                minutes: 90,
                rating: 6.0,
                ..Default::default()
            });
            rt.passing.push(
                wv(a.passing) * 0.42 + wv(a.vision) * 0.24 + wv(a.composure) * 0.14 + wv(a.first_touch) * 0.20,
            );
            rt.dribble
                .push(wv(a.dribbling) * 0.46 + wv(a.pace) * 0.30 + wv(a.first_touch) * 0.24);
            rt.creation
                .push(wv(a.vision) * 0.40 + wv(a.passing) * 0.35 + wv(a.dribbling) * 0.25);
            rt.finish
                .push(wv(a.shooting) * 0.55 + wv(a.composure) * 0.25 + wv(a.pace) * 0.20);
            rt.defend.push(
                wv(a.tackling) * 0.40 + wv(a.strength) * 0.20 + wv(a.pace) * 0.15 + wv(a.vision) * 0.25,
            );
            rt.press
                .push(wv(a.stamina) * 0.40 + wv(a.tackling) * 0.35 + wv(a.pace) * 0.25);
            rt.aerial.push(wv(a.heading) * 0.6 + wv(a.strength) * 0.4);
            rt.gk.push(wv(a.goalkeeping) * 0.82 + wv(a.composure) * 0.18);
            rt.shoot_tend.push(shoot_tendency(mp.role));
        }
        rt
    }

    /// Effective value of a base skill for player `i`: ability × form, docked by
    /// however gassed they are (start fatigue + what they've burned in-match).
    fn eff(&self, i: usize, base: &[f32]) -> f32 {
        let f = (self.p[i].start_fatigue + self.fat[i]).min(0.75);
        base[i] * self.p[i].form * (1.0 - f * 0.42)
    }

    fn is_gk(&self, i: usize) -> bool {
        self.p[i].pos == Position::GK
    }

    /// Position weight for a phase group (how much this line takes part).
    fn pos_w(&self, i: usize, fwd: f32, mid: f32, def: f32, gkw: f32) -> f32 {
        match self.p[i].pos {
            Position::FWD => fwd,
            Position::MID => mid,
            Position::DEF => def,
            Position::GK => gkw,
        }
    }

    /// Candidate (index, weight) list for a phase, skipping injured players.
    fn group(&self, base: &[f32], fwd: f32, mid: f32, def: f32, gkw: f32) -> Vec<(usize, f32)> {
        (0..self.p.len())
            .filter(|&i| !self.out[i])
            .map(|i| (i, self.eff(i, base).max(1.0) * self.pos_w(i, fwd, mid, def, gkw)))
            .filter(|&(_, w)| w > 0.0)
            .collect()
    }

    /// The same pool, but for a genuine ball-winning duel: candidates are
    /// additionally weighted by how much their ROLE defends, so the man picked
    /// to make the challenge is the man who would actually be there to make it.
    /// This is the difference between a season tackle chart led by a destroyer
    /// and one led by whichever playmaker happened to have high vision.
    fn duel_group(&self, base: &[f32], fwd: f32, mid: f32, def: f32, gkw: f32) -> Vec<(usize, f32)> {
        (0..self.p.len())
            .filter(|&i| !self.out[i])
            .map(|i| {
                (
                    i,
                    self.eff(i, base).max(1.0)
                        * self.pos_w(i, fwd, mid, def, gkw)
                        * role_defence_w(self.p[i].role),
                )
            })
            .filter(|&(_, w)| w > 0.0)
            .collect()
    }

    /// The man the opposition must stop: best effective finishing among
    /// outfield players. Keepers are never the danger man.
    fn danger_man(&self) -> Option<usize> {
        (0..self.p.len())
            .filter(|&i| !self.out[i] && !self.is_gk(i))
            .max_by(|&a, &b| {
                self.eff(a, &self.finish)
                    .partial_cmp(&self.eff(b, &self.finish))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    /// The designated stopper: best effective defending among defenders and
    /// midfielders. Forwards don't man-mark.
    fn stopper(&self) -> Option<usize> {
        (0..self.p.len())
            .filter(|&i| {
                !self.out[i] && matches!(self.p[i].pos, Position::DEF | Position::MID)
            })
            .max_by(|&a, &b| {
                self.eff(a, &self.defend)
                    .partial_cmp(&self.eff(b, &self.defend))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    /// Who gets on the end of a chance: finishing × line × role tendency, so a
    /// poacher leads, an advanced playmaker gambles his share, and a holding
    /// midfielder stays home. Plus a mild hot hand: a man who has scored today
    /// gets fed — braces happen. Against it, defensive attention: a defense
    /// that has watched one man shoot all game keys on him (man-marking the
    /// hot man is real), so each further shot comes harder. The two oppose —
    /// stars still lead, nobody eats half his team's shots every week.
    /// `marked`: this side's danger man, when the opponent assigned a shadow —
    /// his team looks elsewhere a little more, feeding the others instead.
    fn shoot_candidates(&self, marked: Option<usize>) -> Vec<(usize, f32)> {
        (0..self.p.len())
            .filter(|&i| !self.out[i] && self.shoot_tend[i] > 0.0)
            .map(|i| {
                let hot = 1.0 + 0.2 * (self.st[i].goals.min(2) as f32);
                let attention =
                    1.0 / (1.0 + 0.42 * (self.st[i].shots.min(10) as f32 - 3.0).max(0.0));
                let shadowed = if Some(i) == marked { 0.92 } else { 1.0 };
                (
                    i,
                    self.eff(i, &self.finish).max(1.0)
                        * self.pos_w(i, 1.25, 0.95, 0.25, 0.0)
                        * self.shoot_tend[i]
                        * hot
                        * attention
                        * shadowed,
                )
            })
            .filter(|&(_, w)| w > 0.0)
            .collect()
    }

    /// Team control index: the technical level of the players who dominate
    /// midfield, so a side that is out-passed genuinely sees less of the ball.
    fn control(&self) -> f32 {
        let mut vals: Vec<f32> = (0..self.p.len())
            .filter(|&i| !self.out[i])
            .map(|i| self.eff(i, &self.passing) * self.pos_w(i, 0.7, 1.0, 0.6, 0.1))
            .collect();
        vals.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        let take = vals.iter().take(5);
        let n = take.len().max(1) as f32;
        vals.iter().take(5).sum::<f32>() / n
    }

    /// Overall squad quality on the pitch: the mean effective ability of the
    /// XI, each judged in the phase their line lives in. This is what makes a
    /// genuinely stronger squad dominate the ball (and so the result) instead
    /// of every game drifting toward a coin-flip on compressed attributes.
    fn quality_index(&self) -> f32 {
        let mut sum = 0.0f32;
        let mut n = 0.0f32;
        for i in 0..self.p.len() {
            if self.out[i] {
                continue;
            }
            let s = match self.p[i].pos {
                Position::GK => self.eff(i, &self.gk),
                Position::DEF => self.eff(i, &self.defend),
                Position::MID => (self.eff(i, &self.passing) + self.eff(i, &self.defend)) * 0.5,
                Position::FWD => (self.eff(i, &self.finish) + self.eff(i, &self.creation)) * 0.5,
            };
            sum += s;
            n += 1.0;
        }
        if n > 0.0 {
            sum / n
        } else {
            70.0
        }
    }

    fn gk_index(&self) -> Option<usize> {
        (0..self.p.len()).find(|&i| self.is_gk(i) && !self.out[i])
    }

    fn add_fatigue(&mut self, i: usize, base: f32) {
        // Low-stamina players tire faster; the effect compounds late on.
        let stam = self.p[i].attrs.stamina as f32;
        self.fat[i] += base * (1.0 + (200.0 - stam).max(0.0) / 300.0);
    }
}

fn logistic(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Per-hard-foul injury probability, entirely a function of the challenge
/// intensity, the victim's robustness (strength + stamina), and how spent the
/// victim already is. No fixed "0.1 chance" anywhere — a fresh, powerful player
/// shrugs off challenges a slight, exhausted one cannot.
pub fn injury_chance(intensity: f32, robust: f32, victim_fatigue: f32) -> f32 {
    logistic((intensity - robust) / 87.0 + victim_fatigue * 1.2 - 3.2).clamp(0.0, 0.4)
}

/// Attribute contest → success probability, clamped away from certainty so
/// upsets remain possible. `bias` nudges in raw skill points. The raw ability
/// gap saturates slowly (even +20 mismatches win phases, not whole matches —
/// otherwise dominant sides manufacture 30-shot games), while the tactical
/// bias stays sharp, so mentality and pressing still swing duels.
fn contest(att: f32, def: f32, bias: f32) -> f32 {
    logistic((att - def) / 110.0 + bias / 55.0).clamp(0.03, 0.97)
}

/// Isolated 1v1 contest for man-marked duels: the marker shadows his man away
/// from help, so the raw quality gap decides steeply — an elite stopper holds
/// a star to scraps while a weak one gets destroyed, exactly as it should be.
/// Even matchups resolve near coin-flips, same as the zonal version.
fn contest_iso(att: f32, def: f32, bias: f32) -> f32 {
    logistic((att - def) / 45.0 + bias / 55.0).clamp(0.03, 0.97)
}

/// Not every lost ball is a *credited* defensive action by a named player —
/// most are just misplaced passes. Better readers/tacklers claim a bigger
/// share, so the season leaderboard lands where real data sits (a top ball-
/// winner around 120-180 combined, not 400+).
fn credited(defender_skill: f32, rng: &mut impl Rng) -> bool {
    let p = logistic((defender_skill - 153.0) / 67.0).clamp(0.12, 0.6);
    rng.gen::<f32>() < p
}

fn wpick(cands: &[(usize, f32)], rng: &mut impl Rng) -> Option<usize> {
    let tot: f32 = cands.iter().map(|&(_, w)| w).sum();
    if tot <= 0.0 {
        return None;
    }
    let mut r = rng.gen::<f32>() * tot;
    for &(i, w) in cands {
        if r < w {
            return Some(i);
        }
        r -= w;
    }
    cands.last().map(|&(i, _)| i)
}

/// Simulate one match. `agg` is the aggregate carried in from a previous leg
/// (a_prev, b_prev); pass (0, 0) for a one-off. Managers read the running
/// aggregate, so second legs of ties play to the tie, not just the day.
pub fn simulate(a: &Side, b: &Side, venue: Venue, agg: (u8, u8), rng: &mut impl Rng) -> MatchOutcome {
    let mut ra = Rt::new(a, true);
    let mut rb = Rt::new(b, false);

    let home_bias_a = match venue {
        Venue::HomeA => 0.28,
        Venue::HomeB => -0.28,
        Venue::Neutral => 0.0,
    };

    let total_drives: usize = 190;
    let mut poss_a: u16 = 0;
    let (mut shots_a, mut shots_b) = (0u16, 0u16);
    let (mut xg_a, mut xg_b) = (0.0f32, 0.0f32);
    let mut events: Vec<Event> = Vec::new();

    // Pre-match game-planning, read off the two XIs: each side identifies the
    // opponent's danger man and assigns its best stopper to shadow him all
    // game. And if the mismatch is stark (a monster against an overmatched
    // back line), the exposed side starts a notch more cautious — managers
    // game-plan around individuals, not just team strength.
    ra.danger = ra.danger_man();
    rb.danger = rb.danger_man();
    ra.marker = ra.stopper();
    rb.marker = rb.stopper();
    let caution_a = match (rb.danger, ra.marker) {
        (Some(m), Some(k)) => {
            let gap = rb.eff(m, &rb.finish) - ra.eff(k, &ra.defend);
            if gap > 30.0 {
                -2
            } else if gap > 18.0 {
                -1
            } else {
                0
            }
        }
        _ => 0,
    };
    let caution_b = match (ra.danger, rb.marker) {
        (Some(m), Some(k)) => {
            let gap = ra.eff(m, &ra.finish) - rb.eff(k, &rb.defend);
            if gap > 30.0 {
                -2
            } else if gap > 18.0 {
                -1
            } else {
                0
            }
        }
        _ => 0,
    };

    let mut ment_a: i8 = (decide_mentality(ra.ambition, 0, 1, ra.quality, a.threat) + caution_a).clamp(-3, 3);
    let mut ment_b: i8 = (decide_mentality(rb.ambition, 0, 1, rb.quality, b.threat) + caution_b).clamp(-3, 3);

    for d in 0..total_drives {
        let minute = ((d as f32) * 96.0 / total_drives as f32) as u8 + 1;

        // Managers re-read the game every ~5 minutes, aggregate included. The
        // pre-match caution for a mismatched danger man persists underneath.
        if d % 10 == 0 {
            let gd_a = (ra.goals as i32 + agg.0 as i32) - (rb.goals as i32 + agg.1 as i32);
            ment_a = (decide_mentality(ra.ambition, gd_a, minute, ra.quality, a.threat) + caution_a).clamp(-3, 3);
            ment_b = (decide_mentality(rb.ambition, -gd_a, minute, rb.quality, b.threat) + caution_b).clamp(-3, 3);
        }

        // Possession is decided by who is better on the ball *and* by overall
        // squad quality — a stronger side sees more of the ball, so it takes
        // more shots and wins more often (fewer coin-flip draws, fewer minnows
        // lifting cups). Attributes are compressed in the data, so the quality
        // gap is deliberately amplified to restore real separation; none of it
        // is a per-team constant — it all comes off the eleven on the pitch.
        // The total edge saturates: real possession caps around 75/25 no matter
        // how vast the gulf (even Guardiola's sides rarely top 72%), so runaway
        // 85%+ ball-hoarding — and the 30-shot games it manufactures — cannot
        // happen.
        let ctrl = ((ra.control() - rb.control()) / 37.0
            + (ra.quality_index() - rb.quality_index()) / 38.0
            + home_bias_a
            + (ment_a - ment_b) as f32 * 0.05
            + (ra.quality - rb.quality) * 0.7)
            .clamp(-1.1, 1.1);
        let attacking_a = rng.gen::<f32>() < logistic(ctrl);
        if attacking_a {
            poss_a += 1;
            drive(&mut ra, &mut rb, ment_a, ment_b, minute, true, &mut shots_a, &mut xg_a, &mut events, rng);
        } else {
            drive(&mut rb, &mut ra, ment_b, ment_a, minute, false, &mut shots_b, &mut xg_b, &mut events, rng);
        }
    }

    finalize_ratings(&mut ra, rb.goals);
    finalize_ratings(&mut rb, ra.goals);

    let poss = ((poss_a as f32 / total_drives as f32) * 100.0).round() as u8;
    let mut stats = ra.st;
    stats.extend(rb.st);

    MatchOutcome {
        goals_a: ra.goals,
        goals_b: rb.goals,
        poss_a: poss,
        shots_a,
        shots_b,
        xg_a,
        xg_b,
        corners_a: ra.corners_won,
        corners_b: rb.corners_won,
        sp_goals_a: ra.sp_goals,
        sp_goals_b: rb.sp_goals,
        stats,
        events,
        end_mentality_a: ment_a,
        end_mentality_b: ment_b,
    }
}

/// One attacking possession by `atk` against `def`. Progresses through three
/// attribute contests; a turnover ends it, reaching the box makes a shot.
#[allow(clippy::too_many_arguments)]
fn drive(
    atk: &mut Rt,
    def: &mut Rt,
    atk_ment: i8,
    def_ment: i8,
    minute: u8,
    atk_is_a: bool,
    shots: &mut u16,
    xg_total: &mut f32,
    events: &mut Vec<Event>,
    rng: &mut impl Rng,
) {
    // A higher mentality means more direct, riskier progression (more end
    // product but easier to dispossess); the defending side's willingness to
    // press hard scales with how attacking *they* are.
    let direct = atk_ment as f32 * 1.1;
    let press = 1.5 + def_ment as f32 * 0.9;

    // Circulation: before the incisive play, the side recycles the ball with a
    // few safe passes. These complete at a high, passing-driven rate and are
    // pure retention — they don't end the possession — which pulls a team's
    // pass-completion into the real 72-92% band and its volume into the
    // hundreds, emergent from passing quality rather than a hard-coded 85%.
    let circ = 3 + (rng.gen::<f32>() * 4.0) as u32; // 3..=6 safe passes
    for _ in 0..circ {
        let Some(cp) = wpick(&atk.group(&atk.passing, 0.8, 1.1, 1.1, 0.4), rng) else { break };
        let Some(cd2) = wpick(&def.group(&def.press, 1.0, 0.9, 0.4, 0.0), rng) else { break };
        atk.st[cp].passes_att += 1;
        // Gentle dependence on passing vs the press: elite passers reach the
        // low 90s, poor ones the high 70s, as in real completion data.
        let p_keep = logistic((atk.eff(cp, &atk.passing) - def.eff(cd2, &def.press)) / 100.0 + 2.4)
            .clamp(0.6, 0.985);
        if rng.gen::<f32>() < p_keep {
            atk.st[cp].passes_cmp += 1;
        }
    }

    // Phase 1 — build-up.
    let Some(ba) = wpick(&atk.group(&atk.passing, 0.7, 1.0, 1.0, 0.2), rng) else { return };
    let Some(bd) = wpick(&def.group(&def.press, 1.0, 1.0, 0.5, 0.0), rng) else { return };
    atk.st[ba].passes_att += 1;
    atk.add_fatigue(ba, 0.004);
    def.add_fatigue(bd, 0.005 + (press.max(0.0)) * 0.001);
    if rng.gen::<f32>() < contest(atk.eff(ba, &atk.passing), def.eff(bd, &def.press), 7.5 - press) {
        atk.st[ba].passes_cmp += 1;
    } else {
        if credited(def.eff(bd, &def.press), rng) {
            def.st[bd].interceptions += 1;
        }
        return;
    }

    // Phase 2 — progression through midfield (pass or carry).
    let Some(pa) = wpick(&atk.group(&atk.passing, 0.9, 1.2, 0.5, 0.0), rng) else { return };
    let Some(mut pd) = wpick(&def.duel_group(&def.defend, 0.5, 1.0, 1.0, 0.0), rng) else { return };
    // Man-marking: the danger man gets no zonal duel — his shadow takes him
    // one-on-one instead of the picked defender, and all downstream credits,
    // fouls and turnovers follow the marker naturally. Shadowing tires him.
    // Execution depends on the defending manager: elite coaches' markers lock
    // on (full value), average ones get dragged out of shape — stopping stars
    // is coaching as much as personnel.
    let man_marked = Some(pa) == atk.danger
        && def.marker.map_or(false, |k| !def.out[k]);
    if man_marked {
        pd = def.marker.unwrap();
        def.add_fatigue(pd, 0.003);
    }
    let exec = 0.75 + 0.25 * ((def.quality - 0.82) / 0.38).clamp(0.0, 1.0);
    let pres = def.eff(pd, &def.defend) * if man_marked { exec } else { 1.0 };
    // Isolated duels resolve steeply on the raw gap (no help to hide behind);
    // zonal duels stay saturated.
    // Whether he takes the man on or moves it on is a WEIGHTED CHOICE, not a
    // switch: a player leans toward whichever skill he owns, but nobody goes a
    // whole season without carrying the ball once. Before this, anyone whose
    // passing out-rated his dribbling simply never dribbled — which is how a
    // midfielder could finish a 50-game, 2,600-pass season on zero dribbles.
    let drib = atk.eff(pa, &atk.dribble);
    let pass = atk.eff(pa, &atk.passing);
    let p_carry = (0.40 + (drib - pass) / 100.0).clamp(0.15, 0.70);
    let carry = rng.gen::<f32>() < p_carry;
    if carry {
        atk.st[pa].dribbles_att += 1;
        atk.add_fatigue(pa, 0.006);
        let p = if man_marked {
            contest_iso(atk.eff(pa, &atk.dribble), pres, direct)
        } else {
            contest(atk.eff(pa, &atk.dribble), pres, direct)
        };
        if rng.gen::<f32>() < p {
            atk.st[pa].dribbles_cmp += 1;
        } else {
            resolve_turnover(def, pd, atk, pa, minute, !atk_is_a, events, rng);
            return;
        }
    } else {
        atk.st[pa].passes_att += 1;
        atk.add_fatigue(pa, 0.004);
        let p = if man_marked {
            contest_iso(atk.eff(pa, &atk.passing), pres, direct)
        } else {
            contest(atk.eff(pa, &atk.passing), pres, direct)
        };
        if rng.gen::<f32>() < p {
            atk.st[pa].passes_cmp += 1;
        } else {
            resolve_turnover(def, pd, atk, pa, minute, !atk_is_a, events, rng);
            return;
        }
    }

    // Phase 3 — final third: a creator tries to unlock a set defence. Creation
    // is chiefly a midfield/wide job (playmakers, wingers, overlapping
    // full-backs), so those roles — not just the striker — rack up the key
    // passes, assists and big chances, exactly as real chance-creation charts.
    let Some(ca) = wpick(&atk.group(&atk.creation, 0.95, 1.35, 0.45, 0.0), rng) else { return };
    let Some(mut cd) = wpick(&def.duel_group(&def.defend, 0.3, 0.7, 1.2, 0.0), rng) else { return };
    atk.st[ca].passes_att += 1;
    atk.add_fatigue(ca, 0.005);
    // Numbers in the final third emerge from mentalities: a side pouring
    // forward (mentality +2 or more) overloads with extra runners, while a
    // deep-sitting side (mentality -1 or less) doubles up — the covering
    // defender helps, so it is genuinely one-vs-two, not a flat constant.
    let mut extra_man = 0.0;
    if atk_ment >= 2 {
        extra_man += 4.0;
    }
    // Man-marking: the danger man gets no zonal duel here either — his shadow
    // takes him one-on-one, isolated from help. Against a deep block he must
    // then beat the covering defender too: two clean wins to unlock, which is
    // why parked buses with elite stoppers strangle even 99-rated monsters
    // while weak markers get skinned twice.
    let man_marked = Some(ca) == atk.danger
        && def.marker.map_or(false, |k| !def.out[k]);
    if man_marked {
        cd = def.marker.unwrap();
        def.add_fatigue(cd, 0.003);
    }
    let mut resistance = def.eff(cd, &def.defend);
    if def_ment <= -1 {
        // The covering man is picked from the DEFENDERS, not from anyone with a
        // decent defend score — a real deep block is marshalled by a centre-back
        // stepping across, not by the nearest attacker.
        let cover = def
            .duel_group(&def.defend, 0.15, 0.5, 1.3, 0.0)
            .into_iter()
            .filter(|(i, _)| *i != cd)
            .map(|(i, _)| def.eff(i, &def.defend))
            .fold(0.0f32, f32::max);
        // Cover helps, but a second man never doubles the duel — overloaded
        // defenses still concede, just less often.
        resistance = resistance.max(cover * 0.82);
    }
    let beaten = if man_marked {
        // Isolated against his shadow: no zonal help, no cover blended in.
        // Execution scales with the defending manager's quality.
        let exec = 0.75 + 0.25 * ((def.quality - 0.82) / 0.38).clamp(0.0, 1.0);
        let p = contest_iso(
            atk.eff(ca, &atk.creation),
            def.eff(cd, &def.defend) * exec,
            direct - 1.0 + extra_man,
        );
        if rng.gen::<f32>() >= p {
            true
        } else if def_ment <= -1 {
            // Beat his man into a set wall: the cover steps out for a second
            // isolated duel. Two wins or no chance.
            let cover = def
                .duel_group(&def.defend, 0.15, 0.5, 1.3, 0.0)
                .into_iter()
                .filter(|(i, _)| *i != cd)
                .map(|(i, _)| def.eff(i, &def.defend))
                .fold(0.0f32, f32::max);
            rng.gen::<f32>() >= contest_iso(atk.eff(ca, &atk.creation), cover.max(1.0), direct - 1.0 + extra_man)
        } else {
            false
        }
    } else {
        rng.gen::<f32>()
            >= contest(
                atk.eff(ca, &atk.creation),
                resistance,
                direct - 1.0 + extra_man,
            )
    };
    if beaten {
        if credited(def.eff(cd, &def.defend), rng) {
            def.st[cd].interceptions += 1;
        }
        return;
    }
    // Beaten on the edge of the box, a physical defender may take the tactical
    // foul instead — his aggression against his composure decides, and the
    // victim's set-piece threat is born from a real duel, not a dice table.
    let intensity = wv(def.p[cd].attrs.tackling) * 0.6 + wv(def.p[cd].attrs.strength) * 0.4;
    let foul_p =
        logistic((intensity - wv(def.p[cd].attrs.composure) - 10.0) / 14.0).clamp(0.02, 0.22);
    if rng.gen::<f32>() < foul_p {
        def.st[cd].fouls += 1;
        take_free_kick(atk, def, minute, atk_is_a, shots, events, rng);
        return;
    }
    atk.st[ca].passes_cmp += 1;

    // Shot: whoever gets on the end of it. Forwards lead, but midfielders take
    // roughly their real share (~30% of team shots) and defenders chip in from
    // set pieces and distance — so one talisman takes ~22%, not ~60%, of his
    // side's shots. Conversion still rewards finishing, so the elite lead the
    // charts on merit, just never 70-goal merit.
    let Some(sh) = wpick(&atk.shoot_candidates(if def.marker.is_some() { atk.danger } else { None }), rng) else { return };
    atk.st[sh].shots += 1;
    *shots += 1;
    // A player who keeps shooting tires from the effort: the fatigue cost of
    // each shot grows with how many he has already taken this match, so a
    // volume shooter's legs (and conversion) fade late — another brake on the
    // freak high-volume games that build 60-goal seasons.
    atk.add_fatigue(sh, 0.006 + 0.0015 * (atk.st[sh].shots.min(14) as f32 - 1.0).max(0.0));
    // A key pass is a pass that sets up a shot *for a team-mate*; a solo effort
    // (the creator shooting themselves) is not one, which keeps season key-pass
    // tallies in the real ~90-110 range instead of double-counting solo runs.
    if ca != sh {
        atk.st[ca].key_passes += 1;
    }

    let pressure = def.eff(cd, &def.defend);
    let quality = atk.eff(sh, &atk.finish);

    // Chance TYPE sets the xG spread — most efforts are speculative, a few are
    // gilt-edged — the way real shot maps look. WHAT kind of chance emerges
    // from the creative duel just won: a dominant playmaker facing a weak
    // defense carves clear-cut openings, while a set elite defense concedes
    // only half-chances. The shooter's finishing then nudges the quality.
    let create_gap = atk.eff(ca, &atk.creation) - def.eff(cd, &def.defend);
    let roll = (rng.gen::<f32>() - (create_gap / 150.0).clamp(-0.08, 0.09)).clamp(0.0, 0.999);
    let base = if roll < 0.020 {
        0.75 // won a penalty / rounded the keeper
    } else if roll < 0.14 {
        rng.gen_range(0.30..0.55) // clear-cut
    } else if roll < 0.42 {
        rng.gen_range(0.12..0.24) // decent look
    } else {
        rng.gen_range(0.02..0.09) // speculative / long shot
    };
    let edge = ((quality - pressure) / 133.0).clamp(-0.4, 0.6);
    let xg = (base * (1.0 + edge)).clamp(0.02, 0.95);
    *xg_total += xg;
    atk.st[sh].xg += xg;
    let big = xg >= 0.30;

    // The creator of the chance: the man who played the killer ball, or the
    // shooter themselves if it was a solo effort.
    let assister = if ca != sh { Some(ca) } else { None };
    let creator = assister.unwrap_or(sh);
    if big {
        atk.st[creator].big_chances_created += 1;
    }
    // Expected assists: the final ball is credited with the quality of the
    // chance it manufactured, whether or not the finisher buried it — so a
    // playmaker whose strikers fluff gilt-edged openings still shows up on the
    // creation chart, the way xA does in real tracking data.
    if let Some(a) = assister {
        atk.st[a].xa += xg;
    }

    let on_target;
    // xG *is* the chance quality; a good keeper saves a slice. On top of that,
    // *finishing quality* decides conversion: an elite finisher (Haaland-tier)
    // buries chances a journeyman fluffs, so the golden boot goes to the best
    // strikers on the best teams, not a mid-table forward who happens to be his
    // club's focal point. Centred so a league-average forward is neutral.
    let gk_save = def.gk_index().map(|g| def.eff(g, &def.gk)).unwrap_or(70.0);
    let gk_stop = logistic((gk_save - quality + 27.0) / 53.0);
    let finish_factor = (quality / 79.0).powf(1.15).clamp(0.6, 1.15);
    let raw = xg * (1.0 - gk_stop * 0.35) * finish_factor;
    // No open-play shot exceeds ~26%: even the most gilt-edged chance needs
    // the finish, and nobody sustains better than roughly a quarter across a
    // season. And tired legs snatch at volume: past a normal workload, each
    // further shot in a match converts progressively worse — the steeper bite
    // falls on the gorging high-volume games (10+ shots) that build freak
    // 55-goal seasons, while a normal 4-6 shot game is barely touched. Both
    // bind almost only on chances farmed from overmatched defenses. Penalties
    // (base 0.75) keep their real conversion.
    let volume = 1.0 / (1.0 + 0.09 * (atk.st[sh].shots as f32 - 4.0).max(0.0));
    let scored_p = if base >= 0.6 {
        raw
    } else {
        (raw.min(0.26) * volume).min(0.26)
    };
    let scored = rng.gen::<f32>() < scored_p;
    if scored {
        on_target = true;
    } else {
        // Missed efforts are on target a fraction of the time (blocked/saved).
        on_target = rng.gen::<f32>() < (0.30 + 0.30 * xg);
    }
    if on_target {
        atk.st[sh].shots_on_target += 1;
        // An on-target shot that isn't scored was kept out — credit the keeper,
        // and nearly half the time it is pushed behind for a corner.
        if !scored {
            if let Some(g) = def.gk_index() {
                def.st[g].saves += 1;
            }
            if rng.gen::<f32>() < 0.45 {
                take_corner(atk, def, minute, atk_is_a, shots, events, rng);
            }
        }
    } else if !scored && rng.gen::<f32>() < 0.25 {
        // Blocked behind — corner.
        take_corner(atk, def, minute, atk_is_a, shots, events, rng);
    }
    if scored {
        atk.st[sh].goals += 1;
        atk.goals += 1;
        if let Some(aidx) = assister {
            atk.st[aidx].assists += 1;
        }
        events.push(Event {
            minute,
            side_a: atk_is_a,
            kind: EventKind::Goal,
            actor: atk.st[sh].id,
            assist: assister.map(|i| atk.st[i].id),
        });
    } else if big {
        atk.st[sh].big_chances_missed += 1;
    }
}

/// A dead-ball delivery (corner or free-kick cross): the taker's delivery finds
/// the best header of the ball against the best marker and the keeper. Aerial
/// monsters decide these — heading × strength on both sides, nothing else.
#[allow(clippy::too_many_arguments)]
fn header_attempt(
    atk: &mut Rt,
    def: &mut Rt,
    taker: usize,
    minute: u8,
    atk_is_a: bool,
    shots: &mut u16,
    events: &mut Vec<Event>,
    rng: &mut impl Rng,
) {
    // The delivery looks for a goal threat in the box — not only the tallest
    // man. Aerial ability leads, but a sharp finisher who gambles on the
    // half-chance competes too (near-post flicks, volleys off a cross), so
    // set-piece goals spread across complete forwards and poachers instead of
    // piling onto one 6'4" target man. Blend heading with finishing.
    let hq = |rt: &Rt, i: usize| rt.eff(i, &rt.aerial) * 0.55 + rt.eff(i, &rt.finish) * 0.45;
    let cands: Vec<(usize, f32)> = (0..atk.p.len())
        .filter(|&i| !atk.out[i] && !atk.is_gk(i))
        .map(|i| (i, hq(atk, i) * atk.pos_w(i, 1.2, 0.8, 0.7, 0.0)))
        .collect();
    let Some(target) = wpick(&cands, rng) else { return };
    // The marker is a NAME, not just a number: whoever actually goes up with
    // him wins or loses the duel, so aerial contests build a real won/lost
    // column for the defenders and strikers who contest them.
    let mark_cand = (0..def.p.len())
        .filter(|&i| !def.out[i] && !def.is_gk(i))
        .map(|i| (i, def.eff(i, &def.aerial) * def.pos_w(i, 0.3, 0.6, 1.2, 0.0)))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let mark_i = mark_cand.map(|(i, _)| i);
    let mark = mark_cand.map(|(_, v)| v).unwrap_or(0.0);
    let keeper = def.gk_index().map(|g| def.eff(g, &def.gk)).unwrap_or(70.0);
    let threat = hq(atk, target);
    atk.st[taker].passes_att += 1;
    atk.st[taker].passes_cmp += 1;
    atk.add_fatigue(taker, 0.003);
    if rng.gen::<f32>() >= contest(threat, mark.max(keeper * 0.75), 2.0) {
        // Dealt with — but somebody still won the header. When the marker was
        // the man the delivery had to beat, it goes on his ledger; when the
        // keeper's claim was the higher obstacle, he simply plucked it out.
        if let Some(mi) = mark_i.filter(|_| mark >= keeper * 0.75) {
            def.st[mi].aerials_won += 1;
            atk.st[target].aerials_lost += 1;
        }
        return;
    }
    // A won header: on target or not, then the keeper.
    atk.st[target].aerials_won += 1;
    if let Some(mi) = mark_i {
        def.st[mi].aerials_lost += 1;
    }
    atk.st[target].shots += 1;
    *shots += 1;
    atk.st[taker].key_passes += 1;
    let xg = (0.085 * (1.0 + (threat - mark) / 100.0)).clamp(0.04, 0.24);
    atk.st[taker].xa += xg;
    let gk_stop = logistic((keeper - threat + 10.0) / 40.0);
    let scored = rng.gen::<f32>() < xg * (1.0 - gk_stop * 0.3);
    let on_target = scored || rng.gen::<f32>() < 0.45;
    if on_target {
        atk.st[target].shots_on_target += 1;
        if !scored {
            if let Some(g) = def.gk_index() {
                def.st[g].saves += 1;
            }
        }
    }
    if scored {
        atk.st[target].goals += 1;
        atk.goals += 1;
        atk.sp_goals += 1;
        atk.st[taker].assists += 1;
        events.push(Event {
            minute,
            side_a: atk_is_a,
            kind: EventKind::SetPiece,
            actor: atk.st[target].id,
            assist: Some(atk.st[taker].id),
        });
    }
}

/// A corner: the best crossing midfielder delivers, the corner is counted, and
/// the header decides it. No recycled corners — it is scored or cleared.
fn take_corner(
    atk: &mut Rt,
    def: &mut Rt,
    minute: u8,
    atk_is_a: bool,
    shots: &mut u16,
    events: &mut Vec<Event>,
    rng: &mut impl Rng,
) {
    let cands: Vec<(usize, f32)> = (0..atk.p.len())
        .filter(|&i| !atk.out[i] && atk.p[i].pos == Position::MID)
        .map(|i| (i, atk.eff(i, &atk.passing)))
        .collect();
    let taker = wpick(&cands, rng).or_else(|| {
        wpick(
            &(0..atk.p.len())
                .filter(|&i| !atk.out[i] && !atk.is_gk(i))
                .map(|i| (i, atk.eff(i, &atk.passing)))
                .collect::<Vec<_>>(),
            rng,
        )
    });
    let Some(taker) = taker else { return };
    atk.corners_won += 1;
    header_attempt(atk, def, taker, minute, atk_is_a, shots, events, rng);
}

/// A dangerous free kick on the edge of the box: a genuine striker of the ball
/// (shooting 78+) goes direct; otherwise the taker crosses and it becomes an
/// aerial contest like a corner.
#[allow(clippy::too_many_arguments)]
fn take_free_kick(
    atk: &mut Rt,
    def: &mut Rt,
    minute: u8,
    atk_is_a: bool,
    shots: &mut u16,
    events: &mut Vec<Event>,
    rng: &mut impl Rng,
) {
    let cands: Vec<(usize, f32)> = (0..atk.p.len())
        .filter(|&i| !atk.out[i] && !atk.is_gk(i))
        .map(|i| {
            let a = &atk.p[i].attrs;
            (i, wv(a.shooting) * 0.5 + wv(a.passing) * 0.3 + wv(a.composure) * 0.2)
        })
        .collect();
    let Some(taker) = wpick(&cands, rng) else { return };
    if atk.p[taker].attrs.shooting >= 78 {
        // Direct: beating wall and keeper from distance is hard — conversion
        // sits where real direct free kicks do (one in fifteen-ish).
        atk.st[taker].shots += 1;
        *shots += 1;
        atk.add_fatigue(taker, 0.004);
        let fk = wv(atk.p[taker].attrs.shooting) * 0.5
            + wv(atk.p[taker].attrs.passing) * 0.3
            + wv(atk.p[taker].attrs.composure) * 0.2;
        let keeper = def.gk_index().map(|g| def.eff(g, &def.gk)).unwrap_or(70.0);
        let xg = (0.05 + (fk - 70.0) / 500.0).clamp(0.03, 0.12);
        let gk_stop = logistic((keeper - fk + 12.0) / 30.0);
        let scored = rng.gen::<f32>() < xg * (1.0 - gk_stop * 0.4);
        let on_target = scored || rng.gen::<f32>() < 0.40;
        if on_target {
            atk.st[taker].shots_on_target += 1;
            if !scored {
                if let Some(g) = def.gk_index() {
                    def.st[g].saves += 1;
                }
                // Pushed behind — corner.
                if rng.gen::<f32>() < 0.30 {
                    take_corner(atk, def, minute, atk_is_a, shots, events, rng);
                }
            }
        }
        if scored {
            atk.st[taker].goals += 1;
            atk.goals += 1;
            atk.sp_goals += 1;
            events.push(Event {
                minute,
                side_a: atk_is_a,
                kind: EventKind::SetPiece,
                actor: atk.st[taker].id,
                assist: None,
            });
        }
    } else {
        header_attempt(atk, def, taker, minute, atk_is_a, shots, events, rng);
    }
}

/// A dispossession in midfield: the defender may go through the man. If the
/// challenge is heavy, the injury roll is decided by the victim's robustness
/// and how gassed they are — not a fixed chance.
fn resolve_turnover(
    def: &mut Rt,
    pd: usize,
    atk: &mut Rt,
    victim: usize,
    minute: u8,
    def_is_a: bool,
    events: &mut Vec<Event>,
    rng: &mut impl Rng,
) {
    // A dispossession in a duel is usually a genuine tackle; a strong ball-
    // winner claims most of them, so an elite destroyer (Kanté-tier) racks up
    // 100+ tackles a season while a weak presser wins few.
    let tackle_p = (0.45 + logistic((def.eff(pd, &def.defend) - 70.0) / 30.0) * 0.4).clamp(0.35, 0.85);
    if rng.gen::<f32>() < tackle_p {
        def.st[pd].tackles += 1;
    }
    // Aggression of the challenge from the tackler's physical profile.
    let intensity = wv(def.p[pd].attrs.tackling) * 0.6 + wv(def.p[pd].attrs.strength) * 0.4;
    let control = wv(def.p[pd].attrs.composure);
    let foul = rng.gen::<f32>() < logistic((intensity - control - 18.0) / 16.0).clamp(0.02, 0.5);
    if !foul {
        return;
    }
    def.st[pd].fouls += 1;
    // Injury: victim robustness (strength + stamina) vs the challenge, worsened
    // by how tired the victim already is. Every term is the players'. A single
    // hard foul rarely injures a fresh, robust player; a spent, slight one is
    // far more exposed.
    let victim_fat = (atk.p[victim].start_fatigue + atk.fat[victim]).min(1.0);
    let robust = wv(atk.p[victim].attrs.strength) * 0.5 + wv(atk.p[victim].attrs.stamina) * 0.5;
    let inj_p = injury_chance(intensity, robust, victim_fat);
    if rng.gen::<f32>() < inj_p {
        // Severity scales with the mismatch; fitter players shrug off more.
        let sev = ((intensity - robust) * 0.12 + 6.0).clamp(3.0, 40.0);
        let days = (sev + rng.gen::<f32>() * sev) as u16;
        atk.st[victim].injury_days = days;
        atk.st[victim].minutes = minute;
        atk.out[victim] = true;
        events.push(Event {
            minute,
            side_a: !def_is_a,
            kind: EventKind::Injury,
            actor: atk.st[victim].id,
            assist: None,
        });
    }
}

/// Turn a match line into a 1-10 rating from what the player actually did.
/// Attackers are rewarded for end product, midfielders for control and
/// creation, defenders for duels and clean sheets — so a poacher who scores and
/// a playmaker who creates and a centre-back who keeps a clean sheet can all
/// rate highly in the same match, as in reality.
///
/// The scale is centred so an unremarkable outing sits BELOW the 6.5 an average
/// player is supposed to rate: the baseline the Ballon d'Or arithmetic uses has
/// to be the population's actual mean, or every vote is quietly inflated. That
/// means the misses charge as loudly as the hits — shots off target, passes
/// astray, dribbles lost, chances fluffed and fouls given away all cost — and
/// pass volume no longer buys a seven on its own. Only what a player really
/// contributes to the result lifts him.
fn finalize_ratings(rt: &mut Rt, conceded: u8) {
    let clean = conceded == 0;
    for i in 0..rt.p.len() {
        let s = &mut rt.st[i];
        let is_def = matches!(rt.p[i].pos, Position::DEF | Position::GK);
        let is_gk = rt.p[i].pos == Position::GK;
        let mut r = 5.8f32;
        r += s.passes_cmp as f32 * 0.0045;
        r += s.dribbles_cmp as f32 * 0.08;
        r += s.key_passes as f32 * 0.15;
        r += s.shots_on_target as f32 * 0.10;
        r += s.goals as f32 * 1.00;
        r += s.assists as f32 * 0.75;
        r += s.big_chances_created as f32 * 0.12;
        r += s.saves as f32 * 0.14; // shot-stopping is a keeper's end product
        r += s.aerials_won as f32 * 0.05; // duels actually won, not just contests
        let tackle_w = if is_def { 0.16 } else { 0.09 };
        r += s.tackles as f32 * tackle_w;
        r += s.interceptions as f32 * (tackle_w * 0.8);
        // The cost side, which is where a rating is really earned.
        r -= (s.passes_att.saturating_sub(s.passes_cmp)) as f32 * 0.013;
        r -= (s.shots.saturating_sub(s.shots_on_target)) as f32 * 0.05;
        r -= (s.dribbles_att.saturating_sub(s.dribbles_cmp)) as f32 * 0.06;
        r -= s.fouls as f32 * 0.09;
        r -= s.big_chances_missed as f32 * 0.14;
        if is_def {
            s.clean_sheet = clean;
            if clean {
                r += if is_gk { 0.7 } else { 0.4 };
            } else {
                r -= conceded as f32 * (if is_gk { 0.12 } else { 0.05 });
            }
        }
        s.rating = r.clamp(4.0, 10.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_pcg::Pcg64Mcg;

    fn base_attrs(v: u8) -> Attributes {
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

    fn mp(id: usize, pos: Position, role: Role, v: u8) -> MatchPlayer {
        MatchPlayer {
            id,
            pos,
            role,
            attrs: base_attrs(v),
            start_fatigue: 0.0,
            form: 1.0,
        }
    }

    /// A balanced 4-3-3 of uniform quality `v`.
    fn team(base_id: usize, v: u8) -> Vec<MatchPlayer> {
        let mut t = vec![mp(base_id, Position::GK, Role::Goalkeeper, v)];
        for k in 0..4 {
            t.push(mp(base_id + 1 + k, Position::DEF, Role::CentralDefender, v));
        }
        for k in 0..3 {
            t.push(mp(base_id + 5 + k, Position::MID, Role::BoxToBox, v));
        }
        for k in 0..3 {
            t.push(mp(base_id + 8 + k, Position::FWD, Role::Poacher, v));
        }
        t
    }

    fn side(players: &[MatchPlayer]) -> Side {
        Side {
            players,
            ambition: 1.0,
            quality: 1.0,
            threat: 1.0,
        }
    }

    #[test]
    fn even_match_scores_and_shots_are_realistic() {
        let mut rng = Pcg64Mcg::new(42);
        let (mut gsum, mut ssum, mut n) = (0u32, 0u32, 0u32);
        for _ in 0..300 {
            let a = team(0, 60);
            let b = team(100, 60);
            let o = simulate(&side(&a), &side(&b), Venue::HomeA, (0, 0), &mut rng);
            gsum += (o.goals_a + o.goals_b) as u32;
            ssum += (o.shots_a + o.shots_b) as u32;
            n += 1;
            assert_eq!(o.stats.len(), 22);
            assert!(o.poss_a <= 100);
        }
        let gpg = gsum as f32 / n as f32;
        let spg = ssum as f32 / n as f32 / 2.0;
        assert!((1.8..3.8).contains(&gpg), "goals/game {:.2} unrealistic", gpg);
        assert!((7.0..20.0).contains(&spg), "shots/team {:.2} unrealistic", spg);
    }

    #[test]
    fn stronger_side_wins_more_often() {
        let mut rng = Pcg64Mcg::new(7);
        let (mut strong_wins, mut weak_wins) = (0u32, 0u32);
        for _ in 0..300 {
            let a = team(0, 78);
            let b = team(100, 55);
            let o = simulate(&side(&a), &side(&b), Venue::Neutral, (0, 0), &mut rng);
            if o.goals_a > o.goals_b {
                strong_wins += 1;
            } else if o.goals_b > o.goals_a {
                weak_wins += 1;
            }
        }
        assert!(strong_wins > weak_wins * 2, "strong {} weak {}", strong_wins, weak_wins);
    }

    #[test]
    fn better_finisher_outscores_a_poor_one() {
        let mut rng = Pcg64Mcg::new(99);
        let mut a = team(0, 62);
        // Turn striker id=8 into an elite finisher, id=9 into a poor one.
        a[8].attrs.shooting = 95;
        a[8].attrs.composure = 90;
        a[9].attrs.shooting = 30;
        a[9].attrs.composure = 40;
        let b = team(100, 62);
        let (mut g_elite, mut g_poor) = (0u32, 0u32);
        for _ in 0..250 {
            let o = simulate(&side(&a), &side(&b), Venue::Neutral, (0, 0), &mut rng);
            for s in &o.stats {
                if s.id == 8 {
                    g_elite += s.goals as u32;
                }
                if s.id == 9 {
                    g_poor += s.goals as u32;
                }
            }
        }
        assert!(g_elite > g_poor * 2, "elite {} poor {}", g_elite, g_poor);
    }

    #[test]
    fn a_creator_produces_more_assists_than_a_poacher() {
        let mut rng = Pcg64Mcg::new(123);
        let mut a = team(0, 64);
        // Midfielder id=5 is a pure creator; forward id=8 is a pure poacher.
        a[5].attrs.vision = 96;
        a[5].attrs.passing = 96;
        a[8].attrs.vision = 30;
        a[8].attrs.passing = 30;
        a[8].attrs.shooting = 90;
        let b = team(100, 64);
        let (mut creator_a, mut poacher_a) = (0u32, 0u32);
        for _ in 0..250 {
            let o = simulate(&side(&a), &side(&b), Venue::Neutral, (0, 0), &mut rng);
            for s in &o.stats {
                if s.id == 5 {
                    creator_a += s.assists as u32;
                }
                if s.id == 8 {
                    poacher_a += s.assists as u32;
                }
            }
        }
        assert!(creator_a > poacher_a, "creator {} poacher {}", creator_a, poacher_a);
    }

    #[test]
    fn top_scorer_assist_ratio_is_sane() {
        // The 59g/3a artefact check: over a season-length sample, a team's main
        // striker should also chip in a believable number of assists, because
        // assists come from real key passes, not a decoupled lottery.
        let mut rng = Pcg64Mcg::new(2024);
        let a = team(0, 70);
        let b = team(100, 60);
        let (mut goals, mut assists) = (0u32, 0u32);
        for _ in 0..38 {
            let o = simulate(&side(&a), &side(&b), Venue::HomeA, (0, 0), &mut rng);
            for s in &o.stats {
                if s.side_a {
                    goals += s.goals as u32;
                    assists += s.assists as u32;
                }
            }
        }
        // Team-wide, assists should be a large fraction of goals (most goals are
        // assisted), never the ~5% the old engine produced for a lone poacher.
        assert!(assists * 3 > goals, "assists {} vs goals {} — ratio too low", assists, goals);
    }

    #[test]
    fn losing_manager_pushes_up_opponent_drops_off() {
        // Same scoreline, read from both dugouts late in the game.
        let chasing = decide_mentality(1.2, -2, 80, 1.0, 1.0);
        let leading = decide_mentality(1.2, 2, 80, 1.0, 1.0);
        assert!(chasing > 1, "a manager 2 down at 80' should attack, got {}", chasing);
        assert!(leading < 0, "a manager 2 up at 80' should defend, got {}", leading);
        // Ambition matters: a gung-ho boss chases harder than a pragmatic one.
        let gung_ho = decide_mentality(1.5, -1, 85, 1.0, 1.0);
        let pragmatic = decide_mentality(0.7, -1, 85, 1.0, 1.0);
        assert!(gung_ho >= pragmatic);
    }

    #[test]
    fn underdogs_park_the_bus_favourites_attack() {
        // Same even scoreline at kickoff: only the matchup read differs.
        let dog = decide_mentality(1.0, 0, 1, 1.0, 0.5);
        let even = decide_mentality(1.0, 0, 1, 1.0, 1.0);
        let fav = decide_mentality(1.0, 0, 1, 1.0, 2.0);
        assert!(dog < even, "half-strength side should start cagier: {}", dog);
        assert!(fav > even, "double-strength side should start hotter: {}", fav);
        // ...but a two-goal deficit still drags even an underdog forward.
        let dog_chasing = decide_mentality(1.0, -2, 80, 1.0, 0.5);
        assert!(dog_chasing > dog);
    }

    #[test]
    fn threat_ratio_favors_the_better_xi() {
        let strong = team(0, 78);
        let weak = team(100, 55);
        let (ts, tw) = threat_ratio(&side(&strong), &side(&weak));
        assert!(ts > 1.2, "strong side threat {} should exceed 1.2", ts);
        assert!(tw < 1.0, "weak side threat {} should sit below 1.0", tw);
        let (e1, e2) = threat_ratio(&side(&strong), &side(&strong));
        assert!((e1 - 1.0).abs() < 0.05 && (e2 - 1.0).abs() < 0.05);
    }

    #[test]
    fn marking_identifies_danger_and_stopper() {
        // A 99-finisher among 70s must be the danger man; a 95-defender among
        // 70s must be the stopper. Everything downstream keys off these two.
        let mut attackers = team(0, 70);
        attackers[8].attrs.shooting = 99;
        attackers[8].attrs.composure = 95;
        attackers[8].attrs.pace = 99;
        let mut defenders = team(100, 70);
        defenders[2].attrs.tackling = 95;
        defenders[2].attrs.strength = 92;
        let ra = Rt::new(&side(&attackers), true);
        let rb = Rt::new(&side(&defenders), false);
        assert_eq!(ra.danger_man(), Some(8), "danger man must be the 99-finisher");
        assert_eq!(rb.stopper(), Some(2), "stopper must be the 95-defender");
    }

    #[test]
    fn elite_stopper_strangles_star() {
        // Same 99-finishing star, same teammates, same seed: the only
        // difference is the opposing stopper (95 vs 60 defending). If
        // man-marking is real, the star scores clearly fewer against the
        // elite stopper — emerging purely from the duel, no caps anywhere.
        fn star_side() -> Vec<MatchPlayer> {
            let mut t = team(0, 76);
            t[8].attrs.shooting = 99;
            t[8].attrs.composure = 95;
            t[8].attrs.pace = 99;
            t[8].attrs.dribbling = 90;
            t
        }
        fn opp_with_stopper(tackling: u8, strength: u8) -> Vec<MatchPlayer> {
            let mut t = team(100, 72);
            t[2].attrs.tackling = tackling;
            t[2].attrs.strength = strength;
            t
        }
        let count_star = |opp: &[MatchPlayer], seed: u64| {
            let atk = star_side();
            let mut rng = Pcg64Mcg::new(seed as u128);
            let mut g = 0u32;
            for _ in 0..100 {
                let o = simulate(&side(&atk), &side(opp), Venue::Neutral, (0, 0), &mut rng);
                for s in &o.stats {
                    if s.side_a && s.id == 8 {
                        g += s.goals as u32;
                    }
                }
            }
            g
        };
        let weak_opp = opp_with_stopper(60, 60);
        let elite_opp = opp_with_stopper(95, 92);
        let g_weak = count_star(&weak_opp, 500);
        let g_elite = count_star(&elite_opp, 500);
        assert!(
            g_elite * 10 < g_weak * 9,
            "elite stopper {} should hold the star below weak stopper {}",
            g_elite,
            g_weak
        );
    }

    #[test]
    fn injury_chance_is_attribute_driven() {
        // Fatigue raises it, robustness lowers it, and it never becomes a
        // coin-flip on a single foul.
        let fresh = injury_chance(70.0, 55.0, 0.0);
        let spent = injury_chance(70.0, 55.0, 0.9);
        assert!(spent > fresh, "fatigue should raise injury risk: {} vs {}", spent, fresh);
        let fragile = injury_chance(80.0, 30.0, 0.3);
        let sturdy = injury_chance(80.0, 80.0, 0.3);
        assert!(fragile > sturdy, "robustness should lower risk: {} vs {}", fragile, sturdy);
        for f in [0.0f32, 0.5, 1.0] {
            let p = injury_chance(99.0, 1.0, f);
            assert!((0.0..=0.4).contains(&p), "prob {} out of range", p);
        }
    }

    #[test]
    fn injuries_occur_but_are_not_epidemic() {
        // Over a season, injuries happen yet stay believable (nowhere near one
        // every game, and not zero either).
        let mut rng = Pcg64Mcg::new(88);
        let a = team(0, 62);
        let b = team(100, 62);
        let mut injuries = 0u32;
        let matches = 200;
        for _ in 0..matches {
            let o = simulate(&side(&a), &side(&b), Venue::Neutral, (0, 0), &mut rng);
            injuries += o.stats.iter().filter(|s| s.injury_days > 0).count() as u32;
        }
        let per_match = injuries as f32 / matches as f32;
        assert!(injuries > 0, "no injuries in {} matches", matches);
        assert!(per_match < 1.5, "injury rate {:.2}/match too high", per_match);
    }

    #[test]
    fn pass_completion_lands_in_a_real_band() {
        let mut rng = Pcg64Mcg::new(321);
        let mut good = team(0, 62);
        for p in good.iter_mut() {
            p.attrs.passing = 88;
            p.attrs.vision = 85;
            p.attrs.first_touch = 85;
        }
        let b = team(100, 62);
        let (mut att, mut cmp) = (0u32, 0u32);
        for _ in 0..38 {
            let o = simulate(&side(&good), &side(&b), Venue::Neutral, (0, 0), &mut rng);
            for s in &o.stats {
                if s.side_a {
                    att += s.passes_att as u32;
                    cmp += s.passes_cmp as u32;
                }
            }
        }
        let pct = cmp as f32 / att.max(1) as f32;
        assert!((0.80..0.97).contains(&pct), "team pass completion {:.3} out of real band", pct);
        assert!(att as f32 / 38.0 > 250.0, "too few passes/game: {:.0}", att as f32 / 38.0);
    }

    #[test]
    fn set_pieces_happen_at_real_rates() {
        // Corners, free-kick fouls and dead-ball goals must all occur, at
        // believable volumes: ~3-8 corners per team per match, and a visible
        // but minority share of goals from set pieces (~15-30% in reality).
        let mut rng = Pcg64Mcg::new(777);
        let a = team(0, 64);
        let b = team(100, 64);
        let (mut corners, mut sp, mut goals, mut matches) = (0u32, 0u32, 0u32, 0u32);
        for _ in 0..120 {
            let o = simulate(&side(&a), &side(&b), Venue::Neutral, (0, 0), &mut rng);
            corners += (o.corners_a + o.corners_b) as u32;
            sp += (o.sp_goals_a + o.sp_goals_b) as u32;
            goals += (o.goals_a + o.goals_b) as u32;
            matches += 1;
        }
        let cpg = corners as f32 / matches as f32 / 2.0;
        assert!(cpg > 1.5, "almost no corners ({:.2}/team/match)", cpg);
        assert!(cpg < 12.0, "corner spam ({:.2}/team/match)", cpg);
        assert!(sp > 0, "no set-piece goals in {} matches", matches);
        let share = sp as f32 / goals.max(1) as f32;
        assert!(
            share < 0.55,
            "set pieces decide too much ({:.0}% of goals)",
            share * 100.0
        );
    }

    #[test]
    fn aerial_monsters_win_dead_balls() {
        // A side of giants should outscore an otherwise identical side of
        // midgets from set pieces over a sample — heading must matter.
        let mut rng = Pcg64Mcg::new(31337);
        let mut giants = team(0, 62);
        for p in giants.iter_mut() {
            if p.pos != Position::GK {
                p.attrs.heading = 95;
                p.attrs.strength = 92;
            }
        }
        let mut midgets = team(100, 62);
        for p in midgets.iter_mut() {
            if p.pos != Position::GK {
                p.attrs.heading = 30;
                p.attrs.strength = 35;
            }
        }
        let (mut g_sp, mut m_sp) = (0u32, 0u32);
        for _ in 0..150 {
            let o = simulate(&side(&giants), &side(&midgets), Venue::Neutral, (0, 0), &mut rng);
            g_sp += o.sp_goals_a as u32;
            m_sp += o.sp_goals_b as u32;
        }
        assert!(g_sp > m_sp, "aerial giants {} should beat midgets {} from dead balls", g_sp, m_sp);
    }

    /// A maxed wonderkid (99 finishing/pace/composure) among good — not great —
    /// teammates, facing good defenses, over a full 38-game season. Guards the
    /// late-era pathology where one such player hits 60-80 league goals: even
    /// a generational talent must land under ~55 (Messi's 50 is the precedent).
    #[test]
    fn wonderkid_season_stays_inside_precedent() {
        fn star(id: usize, pos: Position, role: Role, heat: u8, craft: u8) -> MatchPlayer {
            let mut a = base_attrs(78);
            if pos == Position::FWD {
                a.shooting = heat;
                a.composure = 95;
                a.pace = heat;
                a.dribbling = 90;
                a.passing = craft;
                a.vision = craft;
            } else {
                a.passing = craft;
                a.vision = craft;
            }
            MatchPlayer { id, pos, role, attrs: a, start_fatigue: 0.0, form: 1.0 }
        }
        let mut home = vec![star(0, Position::GK, Role::Goalkeeper, 78, 78)];
        for k in 0..4 {
            home.push(star(1 + k, Position::DEF, Role::CentralDefender, 78, 78));
        }
        home.push(star(5, Position::MID, Role::Anchor, 78, 78));
        home.push(star(6, Position::MID, Role::BoxToBox, 78, 82));
        home.push(star(7, Position::MID, Role::AdvancedPlaymaker, 78, 88));
        // The wonderkid: 99 finishing/pace, elite dribbling, decent craft.
        home.push(star(8, Position::FWD, Role::Poacher, 99, 72));
        home.push(star(9, Position::FWD, Role::InsideForward, 84, 80));
        home.push(star(10, Position::FWD, Role::Winger, 80, 82));
        let away = team(100, 78);
        let mut rng = Pcg64Mcg::new(4242);
        let mut goals = 0u32;
        let mut shots = 0u32;
        for _ in 0..38 {
            let o = simulate(&side(&home), &side(&away), Venue::HomeA, (0, 0), &mut rng);
            for s in &o.stats {
                if s.side_a && s.id == 8 {
                    goals += s.goals as u32;
                    shots += s.shots as u32;
                }
            }
        }
        assert!(
            goals < 56,
            "wonderkid hit {} goals on {} shots in 38 games — 60-80-goal pathology",
            goals, shots
        );
    }

    /// Regression guard for the late-era 60-80-goal pathology: a 99-finishing
    /// wonderkid farming a weak league (dominant side) or carrying weak
    /// teammates (sole star) must stay inside precedent. If this fails, shot
    /// volume or conversion in mismatches has blown out again.
    ///
    /// Diagnostic scaffold: set PROBE_PRINT to true to print per-config
    /// tallies while hunting the next spike mechanism.
    #[test]
    fn wonderkid_environments_probe() {
        fn mk(id: usize, pos: Position, role: Role, v: u8) -> MatchPlayer {
            MatchPlayer { id, pos, role, attrs: base_attrs(v), start_fatigue: 0.0, form: 1.0 }
        }
        fn star9() -> MatchPlayer {
            let mut a = base_attrs(78);
            a.shooting = 99;
            a.composure = 95;
            a.pace = 99;
            a.dribbling = 90;
            a.passing = 72;
            a.vision = 72;
            MatchPlayer { id: 8, pos: Position::FWD, role: Role::Poacher, attrs: a, start_fatigue: 0.0, form: 1.0 }
        }
        // Dominant: 88 teammates vs 65 opposition.
        let mut dom = vec![mk(0, Position::GK, Role::Goalkeeper, 86)];
        for k in 0..4 {
            dom.push(mk(1 + k, Position::DEF, Role::CentralDefender, 86));
        }
        dom.push(mk(5, Position::MID, Role::Anchor, 86));
        dom.push(mk(6, Position::MID, Role::BoxToBox, 86));
        dom.push(mk(7, Position::MID, Role::AdvancedPlaymaker, 88));
        dom.push(star9());
        dom.push(mk(9, Position::FWD, Role::InsideForward, 86));
        dom.push(mk(10, Position::FWD, Role::Winger, 86));
        let weak = team(100, 65);
        // Sole star: 70 teammates vs 65 opposition.
        let mut solo = vec![mk(0, Position::GK, Role::Goalkeeper, 70)];
        for k in 0..4 {
            solo.push(mk(1 + k, Position::DEF, Role::CentralDefender, 70));
        }
        solo.push(mk(5, Position::MID, Role::Anchor, 70));
        solo.push(mk(6, Position::MID, Role::BoxToBox, 70));
        solo.push(mk(7, Position::MID, Role::AdvancedPlaymaker, 72));
        solo.push(star9());
        solo.push(mk(9, Position::FWD, Role::InsideForward, 70));
        solo.push(mk(10, Position::FWD, Role::Winger, 70));
        // Maxed-everything spike candidate: 99s across the board (including
        // heading, so he also hogs every dead ball), morale-pinned team vs a
        // demoralized weak league — the late-decade perfect storm.
        fn maxed(id: usize, pos: Position, role: Role, form: f32) -> MatchPlayer {
            MatchPlayer { id, pos, role, attrs: base_attrs(99), start_fatigue: 0.0, form }
        }
        let mut storm = vec![maxed(0, Position::GK, Role::Goalkeeper, 1.05)];
        for k in 0..4 {
            storm.push(maxed(1 + k, Position::DEF, Role::CentralDefender, 1.05));
        }
        storm.push(maxed(5, Position::MID, Role::Anchor, 1.05));
        storm.push(maxed(6, Position::MID, Role::BoxToBox, 1.05));
        storm.push(maxed(7, Position::MID, Role::AdvancedPlaymaker, 1.05));
        storm.push(maxed(8, Position::FWD, Role::Poacher, 1.05));
        storm.push(maxed(9, Position::FWD, Role::InsideForward, 1.05));
        storm.push(maxed(10, Position::FWD, Role::Winger, 1.05));
        let mut weak_demoralized = team(200, 62);
        for p in weak_demoralized.iter_mut() {
            p.form = 0.93;
        }
        let mut rng = Pcg64Mcg::new(99);
        for (label, xi, max_goals, max_shots) in
            [("dominant", &dom, 50u32, 1050u32), ("sole-star", &solo, 45u32, 800u32)]
        {
            let (mut g, mut sh) = (0u32, 0u32);
            let mut sha = 0u32;
            for _ in 0..38 {
                let o = simulate(&side(xi), &side(&weak), Venue::HomeA, (0, 0), &mut rng);
                sha += o.shots_a as u32;
                for s in &o.stats {
                    if s.side_a && s.id == 8 {
                        g += s.goals as u32;
                        sh += s.shots as u32;
                    }
                }
            }
            assert!(
                g < max_goals,
                "{}: wonderkid hit {} goals on {} shots — mismatch pathology",
                label, g, sh
            );
            assert!(
                sha < max_shots,
                "{}: dominant side took {} shots in 38 games — volume pathology",
                label, sha
            );
        }
        // The perfect storm must also stay sane: a maxed XI vs demoralized
        // farmers is the absolute ceiling of what the engine can produce, and
        // even that has to read like football, not pinball.
        {
            let (mut g, mut sh, mut cor, mut sp) = (0u32, 0u32, 0u32, 0u32);
            for _ in 0..38 {
                let o = simulate(&side(&storm), &side(&weak_demoralized), Venue::HomeA, (0, 0), &mut rng);
                cor += (o.corners_a + o.corners_b) as u32;
                sp += (o.sp_goals_a + o.sp_goals_b) as u32;
                for s in &o.stats {
                    if s.side_a && s.id == 8 {
                        g += s.goals as u32;
                        sh += s.shots as u32;
                    }
                }
            }
            println!("PROBE storm: star {} on {} (corners {}, sp goals {})", g, sh, cor, sp);
            assert!(
                g < 70,
                "perfect-storm star hit {} goals on {} shots — ceiling pathology",
                g, sh
            );
        }
        // Rotation asymmetry: the talisman plays every game while his fellow
        // attackers sit ~35% of the time, replaced by weaker benchwarmers.
        // This must not manufacture hog-share beyond precedent.
        {
            let (mut g, mut sh) = (0u32, 0u32);
            for _ in 0..38 {
                let mut xi = storm.clone();
                for i in [6usize, 7, 9, 10] {
                    if rng.gen::<f32>() < 0.35 {
                        xi[i].attrs = base_attrs(72);
                    }
                }
                let o = simulate(&side(&xi), &side(&weak_demoralized), Venue::HomeA, (0, 0), &mut rng);
                for s in &o.stats {
                    if s.side_a && s.id == 8 {
                        g += s.goals as u32;
                        sh += s.shots as u32;
                    }
                }
            }
            println!("PROBE rotation-asymmetry: star {} on {}", g, sh);
            assert!(
                g < 70,
                "rotation-asymmetry star hit {} goals on {} shots — share pathology",
                g, sh
            );
        }
    }

    #[test]
    fn defensive_actions_are_not_inflated() {
        // Guards the 443-tackles regression: crediting is gated, so no player
        // racks up an impossible pile over a season.
        let mut rng = Pcg64Mcg::new(654);
        let a = team(0, 60);
        let b = team(100, 60);
        let mut ti: std::collections::HashMap<(bool, usize), u32> = std::collections::HashMap::new();
        for _ in 0..38 {
            let o = simulate(&side(&a), &side(&b), Venue::Neutral, (0, 0), &mut rng);
            for s in &o.stats {
                *ti.entry((s.side_a, s.id)).or_insert(0) += (s.tackles + s.interceptions) as u32;
            }
        }
        let max = ti.values().copied().max().unwrap_or(0);
        assert!(max < 320, "top ball-winner {} tackles+interceptions over 38 games is inflated", max);
        assert!(max > 15, "defensive actions vanished ({} max)", max);
    }
}
