//! Core domain model: players, clubs, leagues, fixtures, finances.
use serde::{Deserialize, Serialize};

pub type PlayerId = u32;
pub type ClubId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Position {
    #[default]
    GK,
    DEF,
    MID,
    FWD,
}

impl Position {
    pub fn short(&self) -> &'static str {
        match self {
            Position::GK => "GK",
            Position::DEF => "DEF",
            Position::MID => "MID",
            Position::FWD => "FWD",
        }
    }
}

/// Raw attributes on a 1..=99 scale, as in classic football management games.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Attributes {
    pub pace: u8,
    pub shooting: u8,
    pub passing: u8,
    pub dribbling: u8,
    pub tackling: u8,
    pub heading: u8,
    pub strength: u8,
    pub stamina: u8,
    pub vision: u8,
    pub composure: u8,
    pub goalkeeping: u8,
    pub first_touch: u8,
}

impl Attributes {
    /// Position-weighted overall ability (1..=99).
    pub fn overall(&self, pos: Position) -> u8 {
        let a = self;
        let v: f32 = match pos {
            Position::GK => {
                a.goalkeeping as f32 * 0.55
                    + a.composure as f32 * 0.15
                    + a.strength as f32 * 0.10
                    + a.passing as f32 * 0.08
                    + a.pace as f32 * 0.07
                    + a.first_touch as f32 * 0.05
            }
            Position::DEF => {
                a.tackling as f32 * 0.28
                    + a.heading as f32 * 0.18
                    + a.strength as f32 * 0.16
                    + a.pace as f32 * 0.14
                    + a.composure as f32 * 0.12
                    + a.passing as f32 * 0.10
                    + a.first_touch as f32 * 0.02
            }
            Position::MID => {
                a.passing as f32 * 0.24
                    + a.vision as f32 * 0.20
                    + a.stamina as f32 * 0.16
                    + a.dribbling as f32 * 0.14
                    + a.tackling as f32 * 0.12
                    + a.composure as f32 * 0.12
                    + a.first_touch as f32 * 0.02
            }
            Position::FWD => {
                a.shooting as f32 * 0.32
                    + a.dribbling as f32 * 0.20
                    + a.pace as f32 * 0.18
                    + a.composure as f32 * 0.14
                    + a.heading as f32 * 0.08
                    + a.vision as f32 * 0.06
                    + a.first_touch as f32 * 0.02
            }
        };
        v.round().clamp(1.0, 99.0) as u8
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Contract {
    /// Weekly wage in currency units.
    pub wage: u32,
    /// Remaining seasons on the contract.
    pub years: u8,
}

/// Season-scoped performance counters.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct SeasonStats {
    pub apps: u16,
    pub goals: u16,
    pub assists: u16,
    /// Sum of per-match ratings (0..=10 scaled by 10 => 0..=100) for averaging.
    pub rating_sum: u32,
    pub rating_apps: u16,
}

impl SeasonStats {
    pub fn avg_rating(&self) -> f32 {
        if self.rating_apps == 0 {
            0.0
        } else {
            self.rating_sum as f32 / self.rating_apps as f32 / 10.0
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Player {
    pub id: PlayerId,
    pub name: String,
    pub age: u8,
    pub position: Position,
    /// Player nationality (first listed). Empty when unknown.
    #[serde(default)]
    pub nation: String,
    pub attrs: Attributes,
    /// Latent ceiling (1..=99); attributes grow toward this while young.
    pub potential: u8,
    pub contract: Contract,
    /// 0..=100 morale.
    pub morale: u8,
    /// 0..=100 match sharpness / fitness.
    pub fitness: u8,
    /// -100..=100 recent form.
    pub form: i8,
    pub value: u64,
    pub stats: SeasonStats,
    pub club: Option<ClubId>,
    /// 0..=100 condition (drains with minutes, restored by rest). Default 100.
    #[serde(default = "default_condition")]
    pub condition: u8,
    /// 0..=100 match sharpness (needs minutes). Default 70.
    #[serde(default = "default_sharpness")]
    pub sharpness: u8,
    /// Injury layoff in days remaining. 0 = fit.
    #[serde(default)]
    pub injury_days: u16,
    /// Accumulated yellow cards this season.
    #[serde(default)]
    pub yellows: u8,
    /// Matches remaining on a red-card suspension.
    #[serde(default)]
    pub suspension: u8,
    /// 0..=100 happiness (promises, game time, results). Default 70.
    #[serde(default = "default_happiness")]
    pub happiness: u8,
    /// Season year the contract expires (e.g. 2026). Default 0 = unknown.
    #[serde(default)]
    pub contract_expiry: u16,
    /// Minimum-fee release clause, if any.
    #[serde(default)]
    pub release_clause: Option<u64>,
    /// Transfer status: 0 = settled, 1 = listed, 2 = transfer-requested.
    #[serde(default)]
    pub transfer_status: u8,
    /// Knowledge 0..=100 the scouting network has on this player (for non-squad players).
    #[serde(default)]
    pub scout_knowledge: u8,
    /// Personality archetype driving development speed. Default Balanced.
    #[serde(default = "default_personality")]
    pub personality: Personality,
}

fn default_personality() -> Personality {
    Personality::Balanced
}

/// Simplified personality archetype. Original grouping inspired by the kinds
/// of personalities FM models (professional/driven vs casual/slack ends).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Personality {
    ModelCitizen,
    Professional,
    Driven,
    Resolute,
    Balanced,
    Jovial,
    Casual,
    Temperamental,
    Unambitious,
    Slack,
}

impl Personality {
    pub fn label(&self) -> &'static str {
        match self {
            Personality::ModelCitizen => "Model Citizen",
            Personality::Professional => "Professional",
            Personality::Driven => "Driven",
            Personality::Resolute => "Resolute",
            Personality::Balanced => "Balanced",
            Personality::Jovial => "Jovial",
            Personality::Casual => "Casual",
            Personality::Temperamental => "Temperamental",
            Personality::Unambitious => "Unambitious",
            Personality::Slack => "Slack",
        }
    }
    /// Development-speed multiplier. Original tuning.
    pub fn growth_mult(&self) -> f32 {
        match self {
            Personality::ModelCitizen => 1.15,
            Personality::Professional => 1.1,
            Personality::Driven => 1.1,
            Personality::Resolute => 1.05,
            Personality::Balanced => 1.0,
            Personality::Jovial => 1.0,
            Personality::Casual => 0.9,
            Personality::Temperamental => 0.9,
            Personality::Unambitious => 0.85,
            Personality::Slack => 0.8,
        }
    }
}

fn default_condition() -> u8 {
    100
}

fn default_sharpness() -> u8 {
    70
}

fn default_happiness() -> u8 {
    70
}

impl Player {
    pub fn overall(&self) -> u8 {
        self.attrs.overall(self.position)
    }

    /// Effective ability for the match engine: overall modulated by fitness/morale/form.
    pub fn effective(&self) -> f32 {
        let base = self.overall() as f32;
        let fit = 0.75 + 0.25 * (self.fitness as f32 / 100.0);
        let mor = 0.92 + 0.08 * (self.morale as f32 / 100.0);
        let frm = 1.0 + (self.form as f32 / 100.0) * 0.10;
        base * fit * mor * frm
    }

    /// Recompute market value from ability, potential, age.
    pub fn recompute_value(&mut self) {
        let ovr = self.overall() as f64;
        let pot = self.potential as f64;
        let age = self.age as f64;
        // Prime around 24-27; young high-potential players carry a premium.
        let age_factor = if age <= 27.0 {
            1.0 + (27.0 - age) * 0.04
        } else {
            (1.0 - (age - 27.0) * 0.07).max(0.15)
        };
        let ceiling_bonus = ((pot - ovr).max(0.0)) * 0.03 * age_factor;
        // Ability is on the 0-100 scale; the divisor keeps the value curve in
        // the same money band it occupied on the old compressed scale.
        let base = (ovr / 167.0).powf(3.4) * 1_500_000.0;
        self.value = ((base * age_factor) * (1.0 + ceiling_bonus)).max(25_000.0) as u64;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Formation {
    F442,
    F433,
    F352,
    F4231,
    F532,
}

impl Formation {
    pub fn all() -> &'static [Formation] {
        &[
            Formation::F442,
            Formation::F433,
            Formation::F352,
            Formation::F4231,
            Formation::F532,
        ]
    }
    pub fn label(&self) -> &'static str {
        match self {
            Formation::F442 => "4-4-2",
            Formation::F433 => "4-3-3",
            Formation::F352 => "3-5-2",
            Formation::F4231 => "4-2-3-1",
            Formation::F532 => "5-3-2",
        }
    }
    /// Outfield line counts (def, mid, fwd); GK always 1.
    pub fn lines(&self) -> (usize, usize, usize) {
        match self {
            Formation::F442 => (4, 4, 2),
            Formation::F433 => (4, 3, 3),
            Formation::F352 => (3, 5, 2),
            Formation::F4231 => (4, 5, 1), // 2 DM + 3 AM modelled as 5 mid
            Formation::F532 => (5, 3, 2),
        }
    }
    /// Natural position line for each of the 11 slots, in order (slot 0 = GK).
    /// Used by the lineup editor to know what each slot asks for.
    pub fn slot_positions(&self) -> Vec<Position> {
        let (d, m, f) = self.lines();
        let mut v = Vec::with_capacity(11);
        v.push(Position::GK);
        v.extend(std::iter::repeat(Position::DEF).take(d));
        v.extend(std::iter::repeat(Position::MID).take(m));
        v.extend(std::iter::repeat(Position::FWD).take(f));
        v
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mentality {
    VeryDefensive,
    Defensive,
    Cautious,
    Balanced,
    Positive,
    Attacking,
    VeryAttacking,
}

impl Mentality {
    pub fn all() -> &'static [Mentality] {
        &[
            Mentality::VeryDefensive,
            Mentality::Defensive,
            Mentality::Cautious,
            Mentality::Balanced,
            Mentality::Positive,
            Mentality::Attacking,
            Mentality::VeryAttacking,
        ]
    }
    pub fn label(&self) -> &'static str {
        match self {
            Mentality::VeryDefensive => "Very Defensive",
            Mentality::Defensive => "Defensive",
            Mentality::Cautious => "Cautious",
            Mentality::Balanced => "Balanced",
            Mentality::Positive => "Positive",
            Mentality::Attacking => "Attacking",
            Mentality::VeryAttacking => "Very Attacking",
        }
    }
    /// (attack multiplier, defense multiplier). Original tuning, not FM values.
    pub fn modifiers(&self) -> (f32, f32) {
        match self {
            Mentality::VeryDefensive => (0.78, 1.24),
            Mentality::Defensive => (0.88, 1.14),
            Mentality::Cautious => (0.94, 1.07),
            Mentality::Balanced => (1.00, 1.00),
            Mentality::Positive => (1.08, 0.93),
            Mentality::Attacking => (1.16, 0.86),
            Mentality::VeryAttacking => (1.26, 0.76),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tactic {
    pub formation: Formation,
    pub mentality: Mentality,
    #[serde(default)]
    pub instructions: TeamInstructions,
    /// Per-slot (slot_index, role, duty). Slot 0 is always GK.
    #[serde(default)]
    pub roles: Vec<(usize, Role, Duty)>,
    /// Ordered penalty/set-piece taker player ids.
    #[serde(default)]
    pub takers: Vec<PlayerId>,
}

impl Default for Tactic {
    fn default() -> Self {
        Tactic {
            formation: Formation::F442,
            mentality: Mentality::Balanced,
            instructions: TeamInstructions::default(),
            roles: Vec::new(),
            takers: Vec::new(),
        }
    }
}

/// Player duty: defensive, balanced, or attacking interpretation of a role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Duty {
    Defend,
    Support,
    Attack,
}

impl Duty {
    pub fn all() -> &'static [Duty] {
        &[Duty::Defend, Duty::Support, Duty::Attack]
    }
    pub fn label(&self) -> &'static str {
        match self {
            Duty::Defend => "Defend",
            Duty::Support => "Support",
            Duty::Attack => "Attack",
        }
    }
    /// (attack weight, defense weight) applied on top of mentality. Original tuning.
    pub fn modifiers(&self) -> (f32, f32) {
        match self {
            Duty::Defend => (0.92, 1.10),
            Duty::Support => (1.00, 1.00),
            Duty::Attack => (1.10, 0.92),
        }
    }
}

/// Simplified player role. Original grouping inspired by the *kinds* of roles
/// FM models (keeper, defender, midfielder, wide, forward variants).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Goalkeeper,
    SweeperKeeper,
    CentralDefender,
    BallPlayingDefender,
    FullBack,
    WingBack,
    Anchor,
    DeepLyingPlaymaker,
    BoxToBox,
    Winger,
    InsideForward,
    AdvancedPlaymaker,
    ShadowStriker,
    TargetMan,
    Poacher,
    FalseNine,
}

impl Role {
    pub fn all() -> &'static [Role] {
        &[
            Role::Goalkeeper,
            Role::SweeperKeeper,
            Role::CentralDefender,
            Role::BallPlayingDefender,
            Role::FullBack,
            Role::WingBack,
            Role::Anchor,
            Role::DeepLyingPlaymaker,
            Role::BoxToBox,
            Role::Winger,
            Role::InsideForward,
            Role::AdvancedPlaymaker,
            Role::ShadowStriker,
            Role::TargetMan,
            Role::Poacher,
            Role::FalseNine,
        ]
    }
    pub fn label(&self) -> &'static str {
        match self {
            Role::Goalkeeper => "Goalkeeper",
            Role::SweeperKeeper => "Sweeper Keeper",
            Role::CentralDefender => "Central Defender",
            Role::BallPlayingDefender => "Ball-Playing Defender",
            Role::FullBack => "Full Back",
            Role::WingBack => "Wing Back",
            Role::Anchor => "Anchor",
            Role::DeepLyingPlaymaker => "Deep-Lying Playmaker",
            Role::BoxToBox => "Box-to-Box",
            Role::Winger => "Winger",
            Role::InsideForward => "Inside Forward",
            Role::AdvancedPlaymaker => "Advanced Playmaker",
            Role::ShadowStriker => "Shadow Striker",
            Role::TargetMan => "Target Man",
            Role::Poacher => "Poacher",
            Role::FalseNine => "False Nine",
        }
    }
    /// Short positional tag (CB, DM, AP, ST…) for stat tables, the way Fbref
    /// or a scouting report abbreviates where a player actually operates.
    pub fn pos_label(&self) -> &'static str {
        match self {
            Role::Goalkeeper | Role::SweeperKeeper => "GK",
            Role::CentralDefender => "CB",
            Role::BallPlayingDefender => "BPD",
            Role::FullBack => "FB",
            Role::WingBack => "WB",
            Role::Anchor => "DM",
            Role::DeepLyingPlaymaker => "DLP",
            Role::BoxToBox => "CM",
            Role::Winger => "W",
            Role::InsideForward => "IF",
            Role::AdvancedPlaymaker => "AP",
            Role::ShadowStriker => "SS",
            Role::TargetMan => "TM",
            Role::Poacher => "ST",
            Role::FalseNine => "F9",
        }
    }
}

/// Simplified team instructions. Each field is a small dial; all default to 50.
/// Original design: values bias attack vs defense output of the sim.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct TeamInstructions {
    pub width: u8,
    pub tempo: u8,
    pub directness: u8,
    pub pressing: u8,
    pub defensive_line: u8,
    pub time_wasting: u8,
}

impl Default for TeamInstructions {
    fn default() -> Self {
        TeamInstructions {
            width: 50,
            tempo: 50,
            directness: 50,
            pressing: 50,
            defensive_line: 50,
            time_wasting: 50,
        }
    }
}

impl TeamInstructions {
    /// Net (attack_mult, defense_mult) from the dials. Original tuning.
    pub fn modifiers(&self) -> (f32, f32) {
        let atk = 1.0 + (self.tempo as f32 - 50.0) * 0.001
            + (self.directness as f32 - 50.0) * 0.001
            + (self.width as f32 - 50.0) * 0.0005
            - (self.time_wasting as f32 - 50.0) * 0.001;
        let def = 1.0 + (self.pressing as f32 - 50.0) * 0.0015
            + (self.defensive_line as f32 - 50.0) * 0.001
            - (self.tempo as f32 - 50.0) * 0.0005;
        (atk.clamp(0.85, 1.20), def.clamp(0.85, 1.20))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Club {
    pub id: ClubId,
    pub name: String,
    pub short: String,
    pub squad: Vec<PlayerId>,
    pub balance: i64,
    /// 1..=100 club reputation, drives AI generosity and youth quality.
    pub reputation: u8,
    pub stadium_capacity: u32,
    pub tactic: Tactic,
    pub is_human: bool,
    #[serde(default = "default_morale")]
    pub morale: u8,
    /// Transfer budget available for fees. Default 0 (derived at game start).
    #[serde(default)]
    pub transfer_budget: i64,
    /// Weekly wage budget. Default 0 (derived at game start).
    #[serde(default)]
    pub wage_budget: i64,
    /// Board confidence 0..=100. Default 60.
    #[serde(default = "default_confidence")]
    pub board_confidence: u8,
    /// Fan confidence 0..=100. Default 60.
    #[serde(default = "default_confidence")]
    pub fan_confidence: u8,
    /// Board-expected final league position (1 = champions). Default 10.
    #[serde(default = "default_expectation")]
    pub board_expectation: u8,
    /// Training facilities level 1..=5. Default 3.
    #[serde(default = "default_facility")]
    pub training_facilities: u8,
    /// Youth facilities level 1..=5. Default 3.
    #[serde(default = "default_facility")]
    pub youth_facilities: u8,
    /// Season-ticket / base ticket price. Default 30.
    #[serde(default = "default_ticket")]
    pub ticket_price: u32,
    /// Competition/league name (e.g. "La Liga"). Empty when unknown.
    #[serde(default)]
    pub league: String,
    /// Manager-selected starting XI: ordered player ids, one per formation slot
    /// (slot 0 = GK). Empty means auto-pick the best-fitting XI. Slots may hold
    /// a player out of their natural position; role fit handles the penalty.
    #[serde(default)]
    pub lineup: Vec<PlayerId>,
}

fn default_confidence() -> u8 {
    60
}

fn default_expectation() -> u8 {
    10
}

fn default_facility() -> u8 {
    3
}

fn default_ticket() -> u32 {
    30
}

fn default_morale() -> u8 {
    75
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct TableRow {
    pub club: ClubId,
    pub played: u16,
    pub won: u16,
    pub drawn: u16,
    pub lost: u16,
    pub gf: u16,
    pub ga: u16,
}

impl TableRow {
    pub fn points(&self) -> u16 {
        self.won * 3 + self.drawn
    }
    pub fn gd(&self) -> i32 {
        self.gf as i32 - self.ga as i32
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Fixture {
    pub round: u16,
    pub home: ClubId,
    pub away: ClubId,
    pub played: bool,
    pub home_goals: u8,
    pub away_goals: u8,
}

/// One logged match event for the minute ticker / match report.
/// Original design: minute, side (true = home), description, xG credit.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatchEvent {
    pub minute: u8,
    pub home_side: bool,
    pub text: String,
    pub xg: f32,
}

/// A scouting assignment: watch a position/age bracket for N weeks, then a
/// report matures. Original design.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScoutAssignment {
    pub id: u32,
    pub position: Option<Position>,
    pub max_age: u8,
    pub weeks_left: u8,
    pub weeks_total: u8,
}

/// Simplified staff member. Original design: one row per hire with a role,
/// a quality rating (1..=20, like judging ability), and a weekly wage.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Staff {
    pub name: String,
    pub role: StaffRole,
    pub quality: u8,
    pub wage: u32,
}

/// Staff roles modelled (subset of FM's backroom).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StaffRole {
    Assistant,
    Coach,
    Scout,
    Physio,
    Analyst,
}

impl StaffRole {
    pub fn all() -> &'static [StaffRole] {
        &[
            StaffRole::Assistant,
            StaffRole::Coach,
            StaffRole::Scout,
            StaffRole::Physio,
            StaffRole::Analyst,
        ]
    }
    pub fn label(&self) -> &'static str {
        match self {
            StaffRole::Assistant => "Assistant Manager",
            StaffRole::Coach => "Coach",
            StaffRole::Scout => "Scout",
            StaffRole::Physio => "Physio",
            StaffRole::Analyst => "Analyst",
        }
    }
}
