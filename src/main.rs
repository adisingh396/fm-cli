//! # Football Manager CLI — End-to-End CLI Football Management Game
//!
//! A fast, robust, Rust-driven command-line football management game built on the
//! Football Manager 2023 data schema (~2,828 real-world players with 100+ attributes).
//!
//! Features:
//! - **New Game** — Pick a club, name your team, set difficulty
//! - **Season Dashboard** — Overview of current season, league position, key events
//! - **Match Centre** — View and simulate matches with full tactical controls
//! - **Squad Management** — Select lineups, manage formations and tactics
//! - **Transfer Market** — Buy/sell players using FM2023 data with market values
//! - **Training** — Improve player attributes through focused training
//! - **Scouting** — Discover and evaluate new players from the FM2023 database
//! - **Finances** — Track wages, revenue, transfer budget
//! - **League Table** — Full standings with promotion/relegation
//! - **Club Info** — Stadium, reputation, youth academy
//! - **News/Events** — Dynamic match reports, transfers, injuries
//! - **Save/Load** — Persistent game state via JSON serialization
//!
//! Built on the `FM 2023.csv` dataset from Sports Interactive/Sega's Football Manager 2023.

mod cup;
mod data;
mod deals;
mod dev;
mod engine;
mod gen;
mod managers;
mod matchsim;
mod model;
mod net;
mod observe;
mod players;
mod season;
mod tune;

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::time::SystemTime;

use rand::Rng;
use rand_pcg::Pcg64Mcg;
use serde::{Deserialize, Serialize};

use data::Database;
use gen::Gen;
use model::*;

// ── Game State ───────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct GameState {
    season: u16,
    week: u8,
    club: Club,
    league: Vec<Club>,
    fixtures: Vec<Fixture>,
    fixture_idx: usize,
    players: Vec<Player>,
    next_player_id: PlayerId,
    news: Vec<String>,
    difficulty: Difficulty,
    saved: bool,
    #[serde(default)]
    staff: Vec<Staff>,
    /// Season year the last youth intake was generated.
    #[serde(default)]
    last_intake_season: u16,
    /// Active scouting assignments (reports mature via the weekly tick).
    #[serde(default)]
    assignments: Vec<ScoutAssignment>,
    /// Scouted shortlist: copies of database players with knowledge attached.
    #[serde(default)]
    scouted: Vec<Player>,
    #[serde(default)]
    next_assignment_id: u32,
    /// Loan deals involving the human club (both directions).
    #[serde(default)]
    loans: Vec<deals::LoanOffer>,
    /// Day of the season (1-based). Advanced by daily ticks and End Week.
    #[serde(default = "default_day")]
    day: u16,
    /// Domestic knockout cup for the current season.
    #[serde(default)]
    cup: Option<cup::Cup>,
}

fn default_day() -> u16 {
    1
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
enum Difficulty {
    Amateur,
    Intermediate,
    Professional,
    Elite,
}

impl Difficulty {
    fn label(&self) -> &'static str {
        match self {
            Difficulty::Amateur => "Amateur",
            Difficulty::Intermediate => "Intermediate",
            Difficulty::Professional => "Professional",
            Difficulty::Elite => "Elite",
        }
    }
    fn match_boost(&self) -> f32 {
        match self {
            Difficulty::Amateur => 1.15,
            Difficulty::Intermediate => 1.0,
            Difficulty::Professional => 0.9,
            Difficulty::Elite => 0.8,
        }
    }
}

impl GameState {
    fn new(club: Club, league: Vec<Club>, players: Vec<Player>, difficulty: Difficulty) -> Self {
        // Fixtures are (re)generated once the full league is known; see generate_fixtures.
        let fixtures = Vec::new();
        GameState {
            season: 2024,
            week: 1,
            club,
            league,
            fixtures,
            fixture_idx: 0,
            next_player_id: players.len() as PlayerId + 1,
            players,
            news: Vec::new(),
            difficulty,
            saved: false,
            staff: Vec::new(),
            last_intake_season: 0,
            assignments: Vec::new(),
            scouted: Vec::new(),
            next_assignment_id: 1,
            loans: Vec::new(),
            day: 1,
            cup: None,
        }
    }

    fn current_club(&self) -> &Club {
        &self.club
    }

    fn current_club_mut(&mut self) -> &mut Club {
        &mut self.club
    }

    fn next_fixture(&self) -> Option<&Fixture> {
        self.fixtures.iter().find(|f| !f.played && (f.home == self.club.id || f.away == self.club.id))
    }

    fn add_news(&mut self, text: String) {
        self.news.push(format!("[Week {}] {}", self.week, text));
        if self.news.len() > 50 {
            self.news.remove(0);
        }
    }
}

// ── Display Helpers ──────────────────────────────────────────────────────────

fn clear_screen() {
    print!("\x1B[2J\x1B[H");
    io::stdout().flush().unwrap();
}

fn print_header(title: &str) {
    clear_screen();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║          ⚽  FOOTBALL MANAGER CLI  🏆                       ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!("  {}", title);
    println!("  ───────────────────────────────────────────────────────────");
}

fn print_menu(options: &[&str]) {
    for (i, opt) in options.iter().enumerate() {
        println!("  {}. {}", i + 1, opt);
    }
    print!("\n  Choose: ");
    io::stdout().flush().unwrap();
}

fn read_choice() -> usize {
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    input.trim().parse().unwrap_or(0)
}

fn read_text(prompt: &str) -> String {
    print!("  {}", prompt);
    io::stdout().flush().unwrap();
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    input.trim().to_string()
}

fn press_enter() {
    println!("\n  Press Enter to continue...");
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
}

// ── Main Menu ────────────────────────────────────────────────────────────────

enum MenuAction {
    NewGame,
    Quit,
}

fn main_menu() -> MenuAction {
    loop {
        print_header("MAIN MENU");
        println!("\n  1.  New Game");
        println!("  2.  Load Game");
        println!("  3.  Simulate Season(s) — 1 season or up to 100 years (dynasty)");
        println!("  4.  Train AI — evolve parameters against real-world targets");
        println!("  5.  Observe / Spectate — watch matches live, browse a league");
        println!("  6.  Quit");
        print!("\n  Choose: ");
        io::stdout().flush().unwrap();
        let choice = read_choice();
        match choice {
            1 => return MenuAction::NewGame,
            2 => {
                if let Some(state) = load_game() {
                    game_loop(state);
                } else {
                    println!("\n  No saved game found!");
                    press_enter();
                }
            }
            3 => run_season_sandbox(),
            4 => run_calibration(),
            5 => observe::run(&Database::load()),
            6 => return MenuAction::Quit,
            _ => println!("\n  Invalid choice!"),
        }
    }
}

fn select_difficulty() -> Difficulty {    print_header("SELECT DIFFICULTY");
    println!("\n  1.  Amateur       (boosted performance)");
    println!("  2.  Intermediate  (balanced)");
    println!("  3.  Professional  (realistic challenge)");
    println!("  4.  Elite         (extreme challenge)");
    print_menu(&["", "", "", ""]);
    match read_choice() {
        4 => Difficulty::Elite,
        3 => Difficulty::Professional,
        2 => Difficulty::Intermediate,
        _ => Difficulty::Amateur,
    }
}

/// Headless season sandbox: simulate one season (detailed) or a decade
/// (dynasty report), print it, and write it to season_1yr.md / season_Nyr.md.
fn run_season_sandbox() {
    print_header("SIMULATE SEASON(S)");
    println!("\n  How many years to simulate? (1 = detailed single season,");
    let years = read_number("  up to 100 for a dynasty report) [default 10]: ");
    let years = if years <= 0 { 10 } else { years.min(100) } as u16;
    println!("\n  Loading database and simulating {} season(s) (~{} matches)...",
        years, years as u32 * 2100);
    io::stdout().flush().unwrap();
    let db = Database::load();
    // FM_SEED pins the run so a fix can be A/B'd against the exact same
    // history it is meant to correct; unset, the sim stays time-seeded.
    let seed = match std::env::var("FM_SEED") {
        Ok(s) if !s.trim().is_empty() => s.trim().parse().unwrap_or_else(|_| {
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        }),
        _ => SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    };
    println!("  (seed {})", seed);
    let report = if years == 1 {
        season::simulate_full_season(&db, seed)
    } else {
        season::simulate_decade(&db, years, seed)
    };
    report.print();
    let text = report.as_text();
    let fname = if years == 1 {
        "season_1yr.md".to_string()
    } else {
        format!("season_{}yr.md", years)
    };
    if std::fs::write(&fname, &text).is_ok() {
        println!("\n  (Full report saved to {})", fname);
    }
    press_enter();
}

/// Evolutionary training: evolve simulation parameters (including the transfer
/// policy network weights) against real-world targets. Each generation every
/// candidate simulates seasons; fitness is closeness to reality; elites
/// survive and mutate. Best vector is saved to tuned_params.json, which the
/// simulator loads automatically on subsequent runs.
fn run_calibration() {
    print_header("TRAIN AI (EVOLUTIONARY CALIBRATION)");
    println!("\n  Each generation, every candidate simulates seasons and is scored");
    println!("  against real-world targets (champion points, golden boots, UCL");
    println!("  winner quality, Ballon d'Or profiles, transfer volume/fees).");
    let gens = read_number("  Generations [default 3]: ");
    let gens = if gens <= 0 { 3 } else { gens.min(20) } as usize;
    println!("\n  Loading database and training (population 6, 3 seasons each)...");
    io::stdout().flush().unwrap();
    let db = Database::load();
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let best = tune::calibrate(&db, gens, 6, 3, seed);
    match best.save() {
        Ok(()) => println!("\n  Best parameters saved to tuned_params.json — future sims use them."),
        Err(e) => println!("\n  Could not save tuned_params.json: {}", e),
    }
    press_enter();
}

// ── New Game Setup ───────────────────────────────────────────────────────────

fn new_game() -> Option<GameState> {
    print_header("NEW GAME");

    let db = Database::load();

    // Group clubs by real competition (FC 26 league labels). Only leagues
    // with enough clubs to field a division are playable — this keeps the
    // world coherent (no La Liga side drawn against a German 2. Liga club).
    let mut by_league: HashMap<String, Vec<ClubId>> = HashMap::new();
    for c in db.clubs() {
        // Skip mis-parsed junk club names (numeric or single char).
        let junk = c.name.trim().len() < 2 || c.name.trim().chars().all(|ch| ch.is_ascii_digit());
        if !c.league.trim().is_empty() && !junk {
            by_league.entry(c.league.clone()).or_default().push(c.id);
        }
    }
    let mut leagues: Vec<(String, usize)> = by_league
        .iter()
        .filter(|(_, v)| v.len() >= 8)
        .map(|(k, v)| (k.clone(), v.len()))
        .collect();
    leagues.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    if leagues.is_empty() {
        println!("\n  No league data found. Ensure data/players.csv is present.");
        press_enter();
        return None;
    }

    print_header("SELECT LEAGUE");
    println!();
    let shown = leagues.len().min(24);
    for (i, (name, n)) in leagues.iter().take(shown).enumerate() {
        println!("  {:>2}. {} ({} clubs)", i + 1, name, n);
    }
    print!("\n  Choose league (1-{}): ", shown);
    io::stdout().flush().unwrap();
    let lchoice = read_choice();
    if lchoice == 0 || lchoice > shown {
        println!("  Invalid choice!");
        press_enter();
        return None;
    }
    let league_name = leagues[lchoice - 1].0.clone();

    // Clubs in the chosen league, strongest first.
    let mut league_clubs: Vec<Club> = by_league[&league_name]
        .iter()
        .filter_map(|id| db.clubs().iter().find(|c| c.id == *id).cloned())
        .collect();
    league_clubs.sort_by(|a, b| b.reputation.cmp(&a.reputation).then(a.name.cmp(&b.name)));
    // Cap to a sane division size (even number → clean round-robin).
    if league_clubs.len() > 24 {
        league_clubs.truncate(24);
    }
    if league_clubs.len() % 2 == 1 {
        league_clubs.pop();
    }

    print_header(&format!("SELECT CLUB — {}", league_name));
    println!();
    for (i, c) in league_clubs.iter().enumerate() {
        println!("  {:>2}. {} (Rep: {})", i + 1, c.name, c.reputation);
    }
    print!("\n  Choose club (1-{}): ", league_clubs.len());
    io::stdout().flush().unwrap();
    let cchoice = read_choice();
    if cchoice == 0 || cchoice > league_clubs.len() {
        println!("  Invalid choice!");
        press_enter();
        return None;
    }

    let mut club = league_clubs[cchoice - 1].clone();
    let renamed = read_text(&format!("  Rename your club (Enter to keep '{}'):", club.name));
    if !renamed.is_empty() {
        club.name = renamed;
    }
    let human_src_id = league_clubs[cchoice - 1].id;

    // World squads come from the database pool (real clubs, latest ratings),
    // topped up with generated players where a source roster runs short.
    let mut rng = Pcg64Mcg::new(SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as u128);
    let db_index: HashMap<PlayerId, usize> =
        db.players().iter().enumerate().map(|(i, p)| (p.id, i)).collect();
    let mut used_db: HashSet<PlayerId> = HashSet::new();
    let mut players = Vec::new();
    let mut next_id: PlayerId = 1;

    for mut p in pull_db_squad(&db, &db_index, human_src_id, 25, &mut used_db) {
        p.id = next_id;
        next_id += 1;
        p.club = Some(club.id);
        players.push(p);
    }
    // Fill to a full 25-man squad with generated players.
    while players.len() < 25 {
        let pos = match rng.gen_range(0..5) {
            0 => Position::GK,
            1 => Position::DEF,
            2 => Position::MID,
            _ => Position::FWD,
        };
        let strength = rng.gen_range(10..=30) as i32;
        let mut player = Gen::new(rng.gen()).player(pos, strength);
        player.id = next_id;
        player.club = Some(club.id);
        player.recompute_value();
        players.push(player);
        next_id += 1;
    }

    // Set club squad
    club.squad = players.iter().map(|p| p.id).collect();

    let difficulty = select_difficulty();

    let mut state = GameState::new(club, vec![], players, difficulty);

    // The division is the real league the chosen club plays in.
    state.league = league_clubs;
    state.club.is_human = true;
    state.club.league = league_name.clone();
    // Mark our entry as the human club and sync the renamed squad/tactics.
    if let Some(c) = state.league.iter_mut().find(|c| c.id == state.club.id) {
        c.is_human = true;
        c.name = state.club.name.clone();
        c.squad = state.club.squad.clone();
        c.tactic = state.club.tactic.clone();
    } else {
        state.league.insert(0, state.club.clone());
    }
    // Build world squads from the database pool so the match sim rates both
    // sides on real attributes (original logic). Human squad (built above)
    // already exists; continue numbering from there.
    let mut next_id = state.next_player_id;
    for i in 0..state.league.len() {
        let club_id = state.league[i].id;
        if club_id == state.club.id {
            continue;
        }
        let rep = state.league[i].reputation as i32;
        let strength = (rep / 4).clamp(8, 28);
        let mut squad_ids = Vec::new();
        for mut p in pull_db_squad(&db, &db_index, club_id, 23, &mut used_db) {
            p.id = next_id;
            next_id += 1;
            p.club = Some(club_id);
            squad_ids.push(p.id);
            state.players.push(p);
        }
        // Top up short lines to a playable 18 (2-6-6-4) with generated players.
        let mut have = [0usize; 4];
        for pid in &squad_ids {
            if let Some(p) = state.players.iter().find(|p| p.id == *pid) {
                have[pos_slot(p.position)] += 1;
            }
        }
        for (slot, (pos, want)) in
            [(Position::GK, 2), (Position::DEF, 6), (Position::MID, 6), (Position::FWD, 4)]
                .into_iter()
                .enumerate()
        {
            while have[slot] < want && squad_ids.len() < 23 {
                let mut p = Gen::new(rng.gen()).player(pos, strength);
                p.id = next_id;
                next_id += 1;
                p.club = Some(club_id);
                squad_ids.push(p.id);
                state.players.push(p);
                have[slot] += 1;
            }
        }
        state.league[i].squad = squad_ids;
    }
    // Seed the free-agent pool from the cheapest unused catalog rows so the
    // parallel market has stock from day one. (Catalog screens may still list
    // the same template; signing it twice creates two world copies.)
    let mut cheap: Vec<PlayerId> = {
        let mut rows: Vec<(u64, PlayerId)> = db
            .players()
            .iter()
            .filter(|p| !used_db.contains(&p.id))
            .map(|p| (p.value, p.id))
            .collect();
        rows.sort_unstable();
        rows.into_iter().take(200).map(|(_, id)| id).collect()
    };
    for db_id in cheap.drain(..) {
        if let Some(&idx) = db_index.get(&db_id) {
            let mut p = db.players()[idx].clone();
            p.id = next_id;
            next_id += 1;
            p.club = None;
            state.players.push(p);
        }
    }
    state.next_player_id = next_id;
    // Budgets + board expectation derived from reputation (original rules).
    {
        let weekly_wages: i64 = state.players.iter()
            .filter(|p| p.club == Some(state.club.id))
            .map(|p| p.contract.wage as i64)
            .sum();
        state.club.transfer_budget = (state.club.balance.max(0)) / 3;
        state.club.wage_budget = weekly_wages * 60;
        state.club.board_expectation = match state.club.reputation {
            81..=100 => 4,
            61..=80 => 6,
            41..=60 => 10,
            _ => 14,
        };
    }
    // Reassign fixtures with the full league
    state.fixtures = generate_fixtures(&state.league);

    // Add opening news
    state.add_news(format!("{} season starts! Good luck managing {}.", state.season, state.club.name));
    state.add_news("Pre-season transfers completed. Squad assembled.".to_string());

    press_enter();
    Some(state)
}

/// Clone up to `max_n` database players of one club for the world pool.
/// Takes the BEST players by overall, not the first rows: the FM and FC
/// datasets use different attribute scales (and sometimes both list the same
/// club), so positional order would randomly field weak slices for big clubs
/// whose names happen to match in both sets.
/// Caller assigns world ids + club. Consumed source ids are tracked so no
/// database row enters the world twice.
fn pull_db_squad(
    db: &Database,
    db_index: &HashMap<PlayerId, usize>,
    club_id: ClubId,
    max_n: usize,
    used: &mut HashSet<PlayerId>,
) -> Vec<Player> {
    let mut out = Vec::new();
    let Some(club) = db.clubs().iter().find(|c| c.id == club_id) else {
        return out;
    };
    let mut cands: Vec<(u8, PlayerId)> = club
        .squad
        .iter()
        .filter(|pid| !used.contains(*pid))
        .filter_map(|pid| {
            db_index
                .get(pid)
                .and_then(|&idx| db.players().get(idx))
                .map(|p| (p.overall(), *pid))
        })
        .collect();
    cands.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, pid) in cands.into_iter().take(max_n) {
        if let Some(&idx) = db_index.get(&pid) {
            if let Some(p) = db.players().get(idx) {
                used.insert(pid);
                out.push(p.clone());
            }
        }
    }
    out
}

fn pos_slot(pos: Position) -> usize {
    match pos {
        Position::GK => 0,
        Position::DEF => 1,
        Position::MID => 2,
        Position::FWD => 3,
    }
}

fn generate_fixtures(league: &[Club]) -> Vec<Fixture> {
    // Proper double round-robin (circle method). Each pair meets twice,
    // once at each ground. Requires an even number of teams; a dummy
    // "bye" is added for odd leagues and its fixtures skipped.
    let mut ids: Vec<ClubId> = league.iter().map(|c| c.id).collect();
    if ids.len() < 2 {
        return Vec::new();
    }
    if ids.len() % 2 == 1 {
        ids.push(0); // bye
    }
    let n = ids.len();
    let rounds = n - 1;
    let mut first_leg: Vec<Vec<(ClubId, ClubId)>> = Vec::with_capacity(rounds);
    let mut ring = ids.clone();
    for _ in 0..rounds {
        let mut round_pairs = Vec::with_capacity(n / 2);
        for i in 0..n / 2 {
            let a = ring[i];
            let b = ring[n - 1 - i];
            if a != 0 && b != 0 {
                round_pairs.push((a, b));
            }
        }
        first_leg.push(round_pairs);
        // Rotate all but the first entry.
        let last = ring.pop().unwrap();
        ring.insert(1, last);
    }
    let mut fixtures = Vec::new();
    for (r, pairs) in first_leg.iter().enumerate() {
        for (idx, &(a, b)) in pairs.iter().enumerate() {
            // Alternate home/away to balance venues; second leg reverses.
            let (home, away) = if (r + idx) % 2 == 0 { (a, b) } else { (b, a) };
            fixtures.push(Fixture {
                round: (r + 1) as u16,
                home,
                away,
                played: false,
                home_goals: 0,
                away_goals: 0,
            });
        }
    }
    for (r, pairs) in first_leg.iter().enumerate() {
        for (idx, &(a, b)) in pairs.iter().enumerate() {
            let (away, home) = if (r + idx) % 2 == 0 { (a, b) } else { (b, a) };
            fixtures.push(Fixture {
                round: (rounds + r + 1) as u16,
                home,
                away,
                played: false,
                home_goals: 0,
                away_goals: 0,
            });
        }
    }
    fixtures
}

// ── Main Game Loop ───────────────────────────────────────────────────────────

fn game_loop(mut state: GameState) {
    loop {
        print_header(&format!("SEASON {} · WEEK {} · DAY {} · {}", state.season, state.week, state.day, state.club.name));
        println!("\n  💰 Budget: {}   ⭐ Rep: {}   🏟️  Capacity: {}",
            format_currency(state.club.balance),
            state.club.reputation,
            state.club.stadium_capacity);
        println!("  📋 Squad Size: {}   📊 League Pos: {}",
            state.club.squad.len(),
            get_league_position(&state));

        println!("\n  ── MAIN MENU ──");
        println!("  1.  Match Centre (Play Next Match)");
        println!("  2.  Squad (View/Manage)");
        println!("  3.  Transfers");
        println!("  4.  Training");
        println!("  5.  Scouting");
        println!("  6.  Finances");
        println!("  7.  League Table");
        println!("  8.  Club & Board");
        println!("  9.  News & Events");
        println!("  10. Save Game");
        println!("  11. Staff");
        println!("  12. Medical Centre");
        println!("  13. Youth Intake");
        println!("  14. End Week");
        println!("  15. Advance Day (world ticks in parallel)");
        println!("  16. Quit to Menu");

        print!("\n  Choose: ");
        io::stdout().flush().unwrap();
        let choice = read_choice();

        match choice {
            1 => match_centre(&mut state),
            2 => squad_manager(&mut state),
            3 => transfer_market(&mut state),
            4 => training(&mut state),
            5 => scouting(&mut state),
            6 => finances(&mut state),
            7 => league_table(&state),
            8 => club_board(&state),
            9 => news_screen(&state),
            10 => save_game(&state),
            11 => staff_screen(&mut state),
            12 => medical_screen(&mut state),
            13 => youth_screen(&mut state),
            14 => end_week(&mut state),
            15 => advance_day(&mut state),
            16 => return,
            _ => println!("  Invalid choice!"),
        }
    }
}

// ── Match Centre ─────────────────────────────────────────────────────────────

fn match_centre(state: &mut GameState) {
    let fixture = match state.next_fixture() {
        Some(f) => *f,
        None => {
            println!("\n  No upcoming fixtures! All matches played this round.");
            press_enter();
            return;
        }
    };

    let home_name = state.league.iter().find(|c| c.id == fixture.home).map(|c| c.name.clone()).unwrap_or_default();
    let away_name = state.league.iter().find(|c| c.id == fixture.away).map(|c| c.name.clone()).unwrap_or_default();

    print_header("MATCH CENTRE");
    println!("\n  Round {}: {} vs {}", fixture.round, home_name, away_name);
    println!("  Venue: {}", home_name);
    println!("\n  1.  Simulate Match (Auto)");
    println!("  2.  View Team Sheet");
    println!("  3.  Set Tactics");
    println!("  4.  Select Starting XI (positions & roles)");
    println!("  5.  Back");

    print!("\n  Choose: ");
    io::stdout().flush().unwrap();
    let choice = read_choice();

    match choice {
        1 => {
            simulate_match(state, &fixture);
        }
        2 => view_team_sheet(state, fixture.home, fixture.away),
        3 => set_tactics(state, state.club.id),
        4 => select_starting_xi(state),
        _ => {}
    }
}

fn simulate_match(state: &mut GameState, fixture: &Fixture) {
    let home_tactic = club_tactic(state, fixture.home);
    let away_tactic = club_tactic(state, fixture.away);
    let home_line = club_lineup(state, fixture.home);
    let away_line = club_lineup(state, fixture.away);
    let home_name = state.league.iter().find(|c| c.id == fixture.home).map(|c| c.name.clone()).unwrap_or_default();
    let away_name = state.league.iter().find(|c| c.id == fixture.away).map(|c| c.name.clone()).unwrap_or_default();
    let involves_human = fixture.home == state.club.id || fixture.away == state.club.id;

    // Pre-match team talk (original morale mechanic): only for human matches.
    let mut talk_mult = 1.0f32;
    if involves_human {
        print_header("TEAM TALK");
        println!("\n  {} vs {}", home_name, away_name);
        println!("  1. Encourage (+attack, +morale if win)");
        println!("  2. Demand more (+attack, -morale if lose)");
        println!("  3. Stay calm (+defense)");
        println!("  4. No talk");
        print!("\n  Choose: ");
        io::stdout().flush().unwrap();
        match read_choice() {
            1 => talk_mult = 1.05,
            2 => talk_mult = 1.08,
            3 => talk_mult = 0.97,
            _ => {}
        }
        // Store the talk choice effect as a small morale swing applied below.
    }

    let mut rng = Pcg64Mcg::new(SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as u128 ^ fixture.round as u128);

    // Apply difficulty boost
    let diff_boost = state.difficulty.match_boost();

    // Both sides rated from the world player pool (fixed: AI squads now exist).
    let home_players: Vec<Player> = state.players.iter().filter(|p| p.club == Some(fixture.home)).cloned().collect();
    let away_players: Vec<Player> = state.players.iter().filter(|p| p.club == Some(fixture.away)).cloned().collect();
    let home_refs: Vec<&Player> = home_players.iter().collect();
    let away_refs: Vec<&Player> = away_players.iter().collect();

    // Strengths from attributes; engine owns home advantage (no extra multiplier).
    let mut home_strength = calculate_team_strength_from_players(&home_refs, &home_tactic, &home_line);
    let mut away_strength = calculate_team_strength_from_players(&away_refs, &away_tactic, &away_line);
    if involves_human {
        if fixture.home == state.club.id {
            home_strength *= diff_boost * talk_mult;
        } else {
            away_strength *= diff_boost * talk_mult;
        }
    }

    // Shots, xG and goals from the engine so ticker and score agree.
    let (home_goals, away_goals, shots_h, shots_a, xg_h, xg_a) =
        engine::simulate_score(&mut rng, home_strength, away_strength);

    // Minute-by-minute event ticker: goals spread across 90 minutes, each
    // carrying an equal share of the side's xG so the ticker sums correctly.
    let mut events: Vec<MatchEvent> = Vec::new();
    let xg_each_h = if home_goals > 0 { xg_h / home_goals as f32 } else { 0.0 };
    let xg_each_a = if away_goals > 0 { xg_a / away_goals as f32 } else { 0.0 };
    for _ in 0..home_goals {
        events.push(MatchEvent {
            minute: rng.gen_range(1..=90) as u8,
            home_side: true,
            text: format!("GOAL {}!", home_name),
            xg: xg_each_h,
        });
    }
    for _ in 0..away_goals {
        events.push(MatchEvent {
            minute: rng.gen_range(1..=90) as u8,
            home_side: false,
            text: format!("GOAL {}!", away_name),
            xg: xg_each_a,
        });
    }
    events.sort_by_key(|e| e.minute);
    // Display the engine's real chance quality, not a per-goal reattribution,
    // so a goalless game can still read (e.g.) xG 1.3 - 0.9.
    let home_xg = xg_h;
    let away_xg = xg_a;

    let result_fix = Fixture {
        round: fixture.round,
        home: fixture.home,
        away: fixture.away,
        played: true,
        home_goals: home_goals.min(255) as u8,
        away_goals: away_goals.min(255) as u8,
    };

    // Update fixture in state
    for f in &mut state.fixtures {
        if f.round == fixture.round && f.home == fixture.home && f.away == fixture.away {
            *f = result_fix.clone();
            break;
        }
    }

    // Post-match tick for both squads (shared with AI-vs-AI fixtures).
    tick_club_after_match(
        state,
        fixture.home,
        home_goals,
        away_goals,
        home_tactic.instructions.pressing,
        &mut rng,
    );
    tick_club_after_match(
        state,
        fixture.away,
        away_goals,
        home_goals,
        away_tactic.instructions.pressing,
        &mut rng,
    );

    // Add news
    if home_goals > away_goals {
        state.add_news(format!("{} beat {} {}-{} (xG {:.1}-{:.1}).", home_name, away_name, home_goals, away_goals, home_xg, away_xg));
    } else if away_goals > home_goals {
        state.add_news(format!("{} won {}-{} at {} (xG {:.1}-{:.1}).", away_name, away_goals, home_goals, home_name, away_xg, home_xg));
    } else {
        state.add_news(format!("{} and {} drew {}-{} (xG {:.1}-{:.1}).", home_name, away_name, home_goals, away_goals, home_xg, away_xg));
    }

    // Prize money / wage costs
    let prize_money = 50000 + (home_goals.max(away_goals) as u64) * 25000 + (if home_goals != away_goals { 100000 } else { 0 });
    state.club.balance += prize_money as i64 / 2; // Home team gets more

    // Advance week
    state.week += 1;

    // Display result
    print_header("MATCH RESULT");
    println!("\n  {} {} - {} {}", home_name, home_goals, away_goals, away_name);
    println!("  xG: {:.1} - {:.1}   Shots: {} - {}", home_xg, away_xg, shots_h, shots_a);
    println!("\n  ── Events ──");
    if events.is_empty() {
        println!("  No goals. Goalless draw.");
    } else {
        for e in &events {
            println!("  {}' {}", e.minute, e.text);
        }
    }
    println!("\n  Prize Money: {} (Home)", format_currency(prize_money as i64));
    println!("  Wage Costs:  -{}", format_currency(25000));
    println!("\n  New Balance: {}", format_currency(state.club.balance));

    // Morale + board/fan confidence swing for the human club.
    let human_won = (fixture.home == state.club.id && home_goals > away_goals)
        || (fixture.away == state.club.id && away_goals > home_goals);
    let human_lost = (fixture.home == state.club.id && home_goals < away_goals)
        || (fixture.away == state.club.id && away_goals < home_goals);
    if involves_human {
        if human_won {
            println!("\n  Victory! Morale and confidence rise.");
            state.club.morale = (state.club.morale as i16 + 5).clamp(0, 100) as u8;
            state.club.board_confidence = (state.club.board_confidence as i16 + 3).clamp(0, 100) as u8;
            state.club.fan_confidence = (state.club.fan_confidence as i16 + 4).clamp(0, 100) as u8;
        } else if human_lost {
            println!("\n  Defeat. Morale and confidence dip.");
            state.club.morale = (state.club.morale as i16 - 5).clamp(0, 100) as u8;
            state.club.board_confidence = (state.club.board_confidence as i16 - 3).clamp(0, 100) as u8;
            state.club.fan_confidence = (state.club.fan_confidence as i16 - 4).clamp(0, 100) as u8;
        } else {
            println!("\n  Draw. Morale steady.");
        }
    }

    // The rest of the round resolves as a unit (AI vs AI quick sim),
    // so the table, news, and awards all move every matchday.
    simulate_rest_of_round(state, fixture.round, &mut rng);

    press_enter();
}

/// Post-match world tick for one club (original rules): condition drains,
/// sharpness rises with minutes, cards accumulate, suspensions are served
/// without playing, small injury chance, apps/goals/ratings recorded.
fn tick_club_after_match(
    state: &mut GameState,
    club_id: ClubId,
    goals: u32,
    conceded: u32,
    pressing: u8,
    rng: &mut Pcg64Mcg,
) {
    let ids: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|p| p.club == Some(club_id))
        .take(18)
        .map(|p| p.id)
        .collect();
    let mut gid = 0u32;
    for pid in ids {
        if let Some(p) = state.players.iter_mut().find(|p| p.id == pid) {
            // Suspended/injured players serve the ban / sit out without playing.
            if p.suspension > 0 {
                p.suspension -= 1;
                continue;
            }
            if p.injury_days > 0 {
                continue;
            }
            // Condition drain scales with stamina (fitter players fade less).
            let drain = (30u8.saturating_sub(p.attrs.stamina.min(20))) + rng.gen_range(0..=8);
            p.condition = p.condition.saturating_sub(drain);
            p.sharpness = (p.sharpness + rng.gen_range(1..=4)).min(100);
            p.stats.apps += 1;
            p.stats.rating_apps += 1;
            // Share goals across outfield scorers (very rough).
            let mut scored = 0u8;
            if gid < goals && p.position != Position::GK && rng.gen_bool(0.35) {
                p.stats.goals += 1;
                scored = 1;
                gid += 1;
            }
            // Cards via the engine's rates; 5 yellows = 1 ban, any red = 1 ban.
            let mut card = 0u8;
            if rng.gen_bool(engine::YELLOW_P) {
                card = 1;
                p.yellows += 1;
                if p.yellows >= 5 {
                    p.yellows = 0;
                    p.suspension = 1;
                }
            }
            if rng.gen_bool(engine::RED_P) {
                card = 2;
                p.suspension = 1;
            }
            let clean = conceded == 0;
            let is_def = matches!(p.position, Position::GK | Position::DEF);
            let r = engine::rating(scored, 0, clean, is_def, 0, card);
            p.stats.rating_sum += (r * 10.0).round() as u32;
            p.form = ((p.form as i16 + ((r - 6.7) * 8.0) as i16).clamp(-100, 100)) as i8;
            // Injuries scale with condition, sharpness and pressing.
            if rng.gen_bool(engine::injury_probability(p.condition, p.sharpness, pressing)) {
                p.injury_days = rng.gen_range(3..=21);
            }
        }
    }
}

/// Resolve every other fixture of the same round (AI vs AI quick sim), drift
/// AI morale/confidence, run the sack cycle, and post a roundup. Original
/// orchestration: one round advances as a unit, like FM's Continue.
fn simulate_rest_of_round(state: &mut GameState, round: u16, rng: &mut Pcg64Mcg) {
    let pending: Vec<(ClubId, ClubId)> = state
        .fixtures
        .iter()
        .filter(|f| f.round == round && !f.played)
        .map(|f| (f.home, f.away))
        .collect();
    if pending.is_empty() {
        return;
    }
    let mut lines: Vec<String> = Vec::new();
    for (home_id, away_id) in pending {
        let ht = club_tactic(state, home_id);
        let at = club_tactic(state, away_id);
        let hl = club_lineup(state, home_id);
        let al = club_lineup(state, away_id);
        let hp: Vec<Player> = state
            .players
            .iter()
            .filter(|p| p.club == Some(home_id))
            .cloned()
            .collect();
        let ap: Vec<Player> = state
            .players
            .iter()
            .filter(|p| p.club == Some(away_id))
            .cloned()
            .collect();
        let hrefs: Vec<&Player> = hp.iter().collect();
        let arefs: Vec<&Player> = ap.iter().collect();
        let hs = calculate_team_strength_from_players(&hrefs, &ht, &hl) * 1.15;
        let aws = calculate_team_strength_from_players(&arefs, &at, &al);
        let (hg, ag, _, _, _, _) = engine::simulate_score(rng, hs, aws);
        for f in state.fixtures.iter_mut() {
            if f.round == round && f.home == home_id && f.away == away_id && !f.played {
                f.played = true;
                f.home_goals = hg.min(255) as u8;
                f.away_goals = ag.min(255) as u8;
                break;
            }
        }
        tick_club_after_match(state, home_id, hg, ag, ht.instructions.pressing, &mut *rng);
        tick_club_after_match(state, away_id, ag, hg, at.instructions.pressing, &mut *rng);
        // AI morale/confidence drift + sack cycle (original rules).
        let (hw, aw) = (hg > ag, ag > hg);
        for (cid, won, lost) in [(home_id, hw, aw), (away_id, aw, hw)] {
            if cid == state.club.id {
                continue;
            }
            if let Some(c) = state.league.iter_mut().find(|c| c.id == cid) {
                if won {
                    c.morale = (c.morale as i16 + 3).clamp(0, 100) as u8;
                    c.board_confidence = (c.board_confidence as i16 + 2).clamp(0, 100) as u8;
                    c.fan_confidence = (c.fan_confidence as i16 + 3).clamp(0, 100) as u8;
                } else if lost {
                    c.morale = (c.morale as i16 - 3).clamp(0, 100) as u8;
                    c.board_confidence = (c.board_confidence as i16 - 2).clamp(0, 100) as u8;
                    c.fan_confidence = (c.fan_confidence as i16 - 3).clamp(0, 100) as u8;
                }
            }
        }
        let hn = state.league.iter().find(|c| c.id == home_id).map(|c| c.name.clone()).unwrap_or_default();
        let an = state.league.iter().find(|c| c.id == away_id).map(|c| c.name.clone()).unwrap_or_default();
        let hr = state.league.iter().find(|c| c.id == home_id).map(|c| c.reputation).unwrap_or(50);
        let ar = state.league.iter().find(|c| c.id == away_id).map(|c| c.reputation).unwrap_or(50);
        lines.push(format!("{} {}-{} {}", hn, hg, ag, an));
        // Upset: winner rated 15+ reputation below the loser. Rout: margin 4+.
        if (hw && hr + 15 < ar) || (aw && ar + 15 < hr) {
            let (w, l) = if hw { (hn.clone(), an.clone()) } else { (an.clone(), hn.clone()) };
            state.add_news(format!("UPSET: {} beat {} {}-{}.", w, l, hg.max(ag), hg.min(ag)));
        } else if hg.max(ag) >= hg.min(ag) + 4 && hg != ag {
            let (w, l) = if hw { (hn.clone(), an.clone()) } else { (an.clone(), hn.clone()) };
            state.add_news(format!("Rout: {} thrashed {} {}-{}.", w, l, hg.max(ag), hg.min(ag)));
        }
        // Sack cycle: confidence collapse after 5+ played games -> new manager,
        // new tactics, reset confidence (original merry-go-round, names omitted).
        for cid in [home_id, away_id] {
            if cid == state.club.id {
                continue;
            }
            let played = state.fixtures.iter().filter(|f| f.played && (f.home == cid || f.away == cid)).count();
            let conf = state.league.iter().find(|c| c.id == cid).map(|c| c.board_confidence).unwrap_or(60);
            if conf < 25 && played >= 5 {
                if let Some(c) = state.league.iter_mut().find(|c| c.id == cid) {
                    let nf = Formation::all()[rng.gen_range(0..Formation::all().len())];
                    c.tactic.formation = nf;
                    c.tactic.mentality = Mentality::Balanced;
                    c.tactic.roles.clear();
                    c.board_confidence = 55;
                    c.morale = 70;
                    let nm = c.name.clone();
                    state.add_news(format!("{} sack their manager after {} games; new boss switches to {}.", nm, played, nf.label()));
                }
            }
        }
    }
    state.add_news(format!("Round {} elsewhere: {}.", round, lines.join(" · ")));
}

/// A player's form/condition multiplier: fitness, morale, form, condition and
/// sharpness combined, on top of raw ability. Mirrors `Player::effective`'s
/// modulation so a role-fit number can be scaled the same way.
fn form_multiplier(p: &Player) -> f32 {
    let fit = 0.75 + 0.25 * (p.fitness as f32 / 100.0);
    let mor = 0.92 + 0.08 * (p.morale as f32 / 100.0);
    let frm = 1.0 + (p.form as f32 / 100.0) * 0.10;
    let cond = 0.85 + 0.15 * (p.condition as f32 / 100.0);
    let sharp = 0.90 + 0.10 * (p.sharpness as f32 / 100.0);
    fit * mor * frm * cond * sharp
}

/// A player's effective quality for auto-selecting a slot on a formation line:
/// their best role fit for that line, scaled by current form.
fn slot_fit(p: &Player, line: usize) -> f32 {
    engine::best_role_for(&p.attrs, p.position, line).1 * form_multiplier(p)
}

/// Resolve the ordered starting XI for a club as (player, role, duty) triples.
///
/// The manager's saved `lineup` is honoured slot-by-slot (any slot may hold a
/// player out of their natural position — that's the whole point of a False
/// Nine or a midfielder at the back). Empty or invalid slots are auto-filled
/// with the best available fit for that slot's line. Per-slot role/duty come
/// from the tactic when set, otherwise from the player's best-fitting role.
fn resolve_lineup<'a>(
    players: &[&'a Player],
    tactic: &Tactic,
    lineup: &[PlayerId],
) -> Vec<(&'a Player, Role, Duty)> {
    let slots = tactic.formation.slot_positions();
    let available = |p: &Player| p.injury_days == 0 && p.suspension == 0;
    let mut chosen: Vec<Option<&Player>> = vec![None; slots.len()];
    let mut used: std::collections::HashSet<PlayerId> = std::collections::HashSet::new();

    // 1. Honour the manager's explicit picks.
    for (slot, &pid) in lineup.iter().enumerate() {
        if slot >= slots.len() {
            break;
        }
        if used.contains(&pid) {
            continue;
        }
        if let Some(p) = players.iter().find(|p| p.id == pid && available(p)) {
            chosen[slot] = Some(*p);
            used.insert(pid);
        }
    }

    // 2. Auto-fill any empty slot with the best remaining fit for its line.
    for slot in 0..slots.len() {
        if chosen[slot].is_some() {
            continue;
        }
        let line = engine::line_of(slots[slot]);
        let pick = players
            .iter()
            .filter(|p| available(p) && !used.contains(&p.id))
            .max_by(|a, b| {
                slot_fit(a, line)
                    .partial_cmp(&slot_fit(b, line))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        if let Some(p) = pick {
            chosen[slot] = Some(*p);
            used.insert(p.id);
        }
    }

    // 3. Attach role + duty for each filled slot.
    let mut xi = Vec::with_capacity(slots.len());
    for slot in 0..slots.len() {
        if let Some(p) = chosen[slot] {
            let line = engine::line_of(slots[slot]);
            let (role, duty) = tactic
                .roles
                .iter()
                .find(|(s, _, _)| *s == slot)
                .map(|(_, r, d)| (*r, *d))
                .unwrap_or_else(|| (engine::best_role_for(&p.attrs, p.position, line).0, Duty::Support));
            xi.push((p, role, duty));
        }
    }
    xi
}

/// Team strength from a resolved XI: each slot contributes its role fit
/// (per-attribute rating × out-of-position penalty), scaled by form and duty,
/// then lifted by the tactic's attack/defence multipliers. Because role fit is
/// per-attribute, tactical choices bite: a poacher with no vision makes a poor
/// False Nine, a technical midfielder makes a fine one.
fn team_strength(xi: &[(&Player, Role, Duty)], tactic: &Tactic) -> f32 {
    if xi.is_empty() {
        return 10.0;
    }
    let (atk_mult, def_mult) = engine::team_multipliers(tactic);
    let combined = (atk_mult + def_mult) / 2.0;
    let mut total_eff = 0.0f32;
    for (p, role, duty) in xi {
        let mut eff = engine::role_fit(&p.attrs, *role, p.position) * form_multiplier(p);
        let (d_atk, d_def) = duty.modifiers();
        eff *= (d_atk + d_def) / 2.0;
        total_eff += eff;
    }
    (total_eff / xi.len() as f32) * combined
}

fn calculate_team_strength_from_players(
    players: &[&Player],
    tactic: &Tactic,
    lineup: &[PlayerId],
) -> f32 {
    if players.is_empty() {
        return 10.0;
    }
    let xi = resolve_lineup(players, tactic, lineup);
    team_strength(&xi, tactic)
}

/// The tactic in force for a club, preferring the human club's live copy over
/// the (only-synced-at-kickoff) league mirror.
fn club_tactic(state: &GameState, id: ClubId) -> Tactic {
    if id == state.club.id {
        state.club.tactic.clone()
    } else {
        state
            .league
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.tactic.clone())
            .unwrap_or_default()
    }
}

/// The starting XI selection for a club, preferring the human club's live copy.
fn club_lineup(state: &GameState, id: ClubId) -> Vec<PlayerId> {
    if id == state.club.id {
        state.club.lineup.clone()
    } else {
        state
            .league
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.lineup.clone())
            .unwrap_or_default()
    }
}

fn view_team_sheet(state: &mut GameState, home_id: ClubId, away_id: ClubId) {
    print_header("TEAM SHEET");
    let home = state.league.iter().find(|c| c.id == home_id).cloned();
    let away = state.league.iter().find(|c| c.id == away_id).cloned();
    if let Some(home) = home {
        println!("\n  {} Squad:", home.name);
        for pid in &home.squad {
            if let Some(p) = state.players.iter().find(|p| p.id == *pid) {
                println!("    #{} {} ({}|{}) - OVR: {}", p.id, p.name, p.position.short(), p.age, p.overall());
            }
        }
    }
    if let Some(away) = away {
        println!("\n  {} Squad:", away.name);
        for pid in &away.squad {
            if let Some(p) = state.players.iter().find(|p| p.id == *pid) {
                println!("    #{} {} ({}|{}) - OVR: {}", p.id, p.name, p.position.short(), p.age, p.overall());
            }
        }
    }
    press_enter();
}

fn set_tactics(state: &mut GameState, club_id: ClubId) {
    loop {
        let club_name = state.league.iter().find(|c| c.id == club_id).map(|c| c.name.clone())
            .unwrap_or_else(|| state.club.name.clone());
        let (formation, mentality, instr) = if state.club.id == club_id {
            (state.club.tactic.formation, state.club.tactic.mentality, state.club.tactic.instructions)
        } else {
            match state.league.iter().find(|c| c.id == club_id) {
                Some(c) => (c.tactic.formation, c.tactic.mentality, c.tactic.instructions),
                None => return,
            }
        };
        print_header("SET TACTICS");
        println!("\n  {}: {} ({})", club_name, formation.label(), mentality.label());
        println!("  Instructions: tempo {} / direct {} / pressing {} / line {} / width {} / waste {}",
            instr.tempo, instr.directness, instr.pressing, instr.defensive_line, instr.width, instr.time_wasting);
        println!("\n  1.  Formation");
        println!("  2.  Mentality (7 levels)");
        println!("  3.  Instructions (tempo/direct/press/line/width/waste 0-100)");
        println!("  4.  Player role (slot + role + duty)");
        println!("  5.  Back");

        print!("\n  Choose: ");
        io::stdout().flush().unwrap();
        match read_choice() {
            1 => {
                println!("\n  Formations:");
                for (i, f) in Formation::all().iter().enumerate() {
                    println!("  {}. {}", i + 1, f.label());
                }
                print!("  Set formation (1-{}): ", Formation::all().len());
                io::stdout().flush().unwrap();
                let c = read_choice();
                if c >= 1 && c <= Formation::all().len() {
                    apply_tactic(state, club_id, |t| t.formation = Formation::all()[c - 1]);
                }
            }
            2 => {
                println!("\n  Mentalities:");
                for (i, m) in Mentality::all().iter().enumerate() {
                    println!("  {}. {} (ATK:{:.2} DEF:{:.2})", i + 1, m.label(), m.modifiers().0, m.modifiers().1);
                }
                print!("  Set mentality (1-{}): ", Mentality::all().len());
                io::stdout().flush().unwrap();
                let c = read_choice();
                if c >= 1 && c <= Mentality::all().len() {
                    apply_tactic(state, club_id, |t| t.mentality = Mentality::all()[c - 1]);
                }
            }
            3 => {
                let v = read_text("  Enter 6 values tempo direct pressing line width waste (0-100, space separated):");
                let nums: Vec<u8> = v.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                if nums.len() == 6 {
                    apply_tactic(state, club_id, |t| {
                        t.instructions.tempo = nums[0].min(100);
                        t.instructions.directness = nums[1].min(100);
                        t.instructions.pressing = nums[2].min(100);
                        t.instructions.defensive_line = nums[3].min(100);
                        t.instructions.width = nums[4].min(100);
                        t.instructions.time_wasting = nums[5].min(100);
                    });
                } else {
                    println!("  Need exactly 6 numbers.");
                    press_enter();
                }
            }
            4 => {
                println!("\n  Roles:");
                for (i, r) in Role::all().iter().enumerate() {
                    println!("  {}. {}", i + 1, r.label());
                }
                print!("  Slot index (0-10), role (1-{}), duty 1=Def 2=Sup 3=Att, space separated: ", Role::all().len());
                io::stdout().flush().unwrap();
                let v = read_text("");
                let nums: Vec<usize> = v.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                if nums.len() == 3 && nums[0] <= 10 && nums[1] >= 1 && nums[1] <= Role::all().len() && nums[2] >= 1 && nums[2] <= 3 {
                    let role = Role::all()[nums[1] - 1];
                    let duty = Duty::all()[nums[2] - 1];
                    apply_tactic(state, club_id, |t| {
                        t.roles.retain(|(s, _, _)| *s != nums[0]);
                        t.roles.push((nums[0], role, duty));
                    });
                } else {
                    println!("  Invalid role input.");
                    press_enter();
                }
            }
            _ => return,
        }
    }
}

fn apply_tactic(state: &mut GameState, club_id: ClubId, f: impl FnOnce(&mut Tactic)) {
    if state.club.id == club_id {
        f(&mut state.club.tactic);
    } else if let Some(club) = state.league.iter_mut().find(|c| c.id == club_id) {
        f(&mut club.tactic);
    }
}

/// Truncate a string to `n` display columns (ASCII-safe for this game's data).
fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    }
}

/// Pad the human club's lineup to exactly one entry per formation slot, using 0
/// (auto) for any slot the manager hasn't picked. Keeps edits index-stable even
/// after formation changes.
fn normalize_lineup(state: &mut GameState) {
    let n = state.club.tactic.formation.slot_positions().len();
    let lineup = &mut state.club.lineup;
    if lineup.len() < n {
        lineup.resize(n, 0);
    } else if lineup.len() > n {
        lineup.truncate(n);
    }
}

// ── Starting XI / Positions ──────────────────────────────────────────────────
//
// A manager should be able to field any player anywhere and see the cost. This
// screen lets you drop a striker into a False Nine, push a midfielder to
// centre-back, or invert a full-back, and shows the role fit so the trade-off
// is visible before kickoff. Selection feeds straight into match strength.

fn select_starting_xi(state: &mut GameState) {
    loop {
        normalize_lineup(state);
        let formation = state.club.tactic.formation;
        let slots = formation.slot_positions();
        let squad: Vec<Player> = state
            .players
            .iter()
            .filter(|p| p.club == Some(state.club.id))
            .cloned()
            .collect();
        let refs: Vec<&Player> = squad.iter().collect();
        let xi = resolve_lineup(&refs, &state.club.tactic, &state.club.lineup);

        print_header("SELECT STARTING XI");
        println!(
            "\n  {}  ·  {}  ·  {}",
            state.club.name,
            formation.label(),
            state.club.tactic.mentality.label()
        );
        println!("  Field anyone anywhere — Fit shows how their attributes suit the");
        println!("  chosen role (out-of-position picks are marked *).\n");
        println!("  ┌────┬─────┬──────────────────────┬─────┬───────────────────┬─────┬──────┐");
        println!("  │ Sl │ Ln  │ Player               │ Nat │ Role              │ Dut │ Fit  │");
        println!("  ├────┼─────┼──────────────────────┼─────┼───────────────────┼─────┼──────┤");
        for (slot, pos) in slots.iter().enumerate() {
            match xi.get(slot) {
                Some((p, role, duty)) => {
                    let eff = engine::role_fit(&p.attrs, *role, p.position) * form_multiplier(p);
                    let flag = if engine::line_of(p.position) != engine::line_of(*pos) {
                        "*"
                    } else {
                        " "
                    };
                    let name = format!("{}{}", flag, clip(&p.name, 19));
                    let dut: String = duty.label().chars().take(3).collect();
                    println!(
                        "  │ {:>2} │ {:>3} │ {:<20} │ {:>3} │ {:<17} │ {:<3} │ {:>4.0} │",
                        slot,
                        pos.short(),
                        name,
                        p.position.short(),
                        clip(role.label(), 17),
                        dut,
                        eff.round()
                    );
                }
                None => {
                    println!(
                        "  │ {:>2} │ {:>3} │ {:<20} │ {:>3} │ {:<17} │ {:<3} │ {:>4} │",
                        slot,
                        pos.short(),
                        "(no eligible player)",
                        "-",
                        "-",
                        "-",
                        "-"
                    );
                }
            }
        }
        println!("  └────┴─────┴──────────────────────┴─────┴───────────────────┴─────┴──────┘");
        let strength = team_strength(&xi, &state.club.tactic);
        println!("  Projected XI strength: {:.1}", strength);

        println!("\n  1.  Assign a player to a slot");
        println!("  2.  Set role & duty for a slot");
        println!("  3.  Auto-pick best XI");
        println!("  4.  Clear selection (revert to auto-pick)");
        println!("  5.  Back");
        print!("\n  Choose: ");
        io::stdout().flush().unwrap();

        match read_choice() {
            1 => assign_slot_player(state),
            2 => set_slot_role(state),
            3 => {
                // Reset to the engine's best-fit XI with best-fit roles.
                let mut probe = state.club.tactic.clone();
                probe.roles.clear();
                let auto = resolve_lineup(&refs, &probe, &[]);
                state.club.lineup = auto.iter().map(|(p, _, _)| p.id).collect();
                state.club.tactic.roles.clear();
                normalize_lineup(state);
                println!("\n  Auto-picked the best-fitting XI.");
                press_enter();
            }
            4 => {
                state.club.lineup.clear();
                println!("\n  Selection cleared. The best available XI will be auto-picked.");
                press_enter();
            }
            _ => return,
        }
    }
}

/// Show the squad (with per-line hints) and drop a chosen player into a slot.
fn assign_slot_player(state: &mut GameState) {
    let slot_count = state.club.tactic.formation.slot_positions().len();
    println!("\n  Squad:");
    println!("  ┌─────┬─────────────────────┬─────┬──────┬──────┐");
    println!("  │ ID  │ Name                │ Nat │ Age  │ OVR  │");
    println!("  ├─────┼─────────────────────┼─────┼──────┼──────┤");
    for p in &state.players {
        if p.club == Some(state.club.id) {
            let mut nat = p.position.short().to_string();
            if p.injury_days > 0 {
                nat = format!("{}!", nat);
            } else if p.suspension > 0 {
                nat = format!("{}x", nat);
            }
            println!(
                "  │ {:>3} │ {:<20}│ {:>3} │ {:>4} │ {:>4} │",
                p.id,
                clip(&p.name, 20),
                nat,
                p.age,
                p.overall()
            );
        }
    }
    println!("  └─────┴─────────────────────┴─────┴──────┴──────┘");
    println!("  (! injured, x suspended — they won't take the pitch)");

    let v = read_text(&format!(
        "  Enter: slot (0-{}) and player ID, space separated:",
        slot_count - 1
    ));
    let nums: Vec<u32> = v.split_whitespace().filter_map(|s| s.parse().ok()).collect();
    if nums.len() != 2 || nums[0] as usize >= slot_count {
        println!("  Invalid input.");
        press_enter();
        return;
    }
    let slot = nums[0] as usize;
    let pid = nums[1] as PlayerId;
    let in_squad = state
        .players
        .iter()
        .any(|p| p.id == pid && p.club == Some(state.club.id));
    if !in_squad {
        println!("  That player isn't in your squad.");
        press_enter();
        return;
    }
    normalize_lineup(state);
    // If the player was already in another slot, free it (no clones on the pitch).
    for s in state.club.lineup.iter_mut() {
        if *s == pid {
            *s = 0;
        }
    }
    state.club.lineup[slot] = pid;
}

/// Assign a role and duty to a formation slot (stored on the tactic). This is
/// what turns a forward slot into a False Nine, a full-back into a wing-back,
/// and so on — the role re-weights which attributes the slot rewards.
fn set_slot_role(state: &mut GameState) {
    let slots = state.club.tactic.formation.slot_positions();
    println!("\n  Roles:");
    let all = Role::all();
    for (i, r) in all.iter().enumerate() {
        println!("  {:>2}. {}", i + 1, r.label());
    }
    let v = read_text(&format!(
        "  Enter: slot (0-{}), role (1-{}), duty (1=Def 2=Sup 3=Att), space separated:",
        slots.len() - 1,
        all.len()
    ));
    let nums: Vec<usize> = v.split_whitespace().filter_map(|s| s.parse().ok()).collect();
    if nums.len() != 3
        || nums[0] >= slots.len()
        || nums[1] < 1
        || nums[1] > all.len()
        || nums[2] < 1
        || nums[2] > 3
    {
        println!("  Invalid input.");
        press_enter();
        return;
    }
    let slot = nums[0];
    let role = all[nums[1] - 1];
    let duty = Duty::all()[nums[2] - 1];
    // Nudge the manager when a role sits off its natural line, but allow it.
    if !engine::roles_for_line(engine::line_of(slots[slot])).contains(&role) {
        println!(
            "  Note: {} is unusual for a {} slot — fit will reflect the mismatch.",
            role.label(),
            slots[slot].short()
        );
    }
    state.club.tactic.roles.retain(|(s, _, _)| *s != slot);
    state.club.tactic.roles.push((slot, role, duty));
}

// ── Squad Manager ────────────────────────────────────────────────────────────

fn squad_manager(state: &mut GameState) {
    loop {
        print_header("SQUAD MANAGER");
        println!("\n  Squad for {} (Budget: {})", state.club.name, format_currency(state.club.balance));
        println!("  ┌─────┬────────────────────┬─────┬──────┬──────┬──────────┐");
        println!("  │ ID  │ Name               │ Pos │ Age  │ OVR  │ Value    │");
        println!("  ├─────┼────────────────────┼─────┼──────┼──────┼──────────┤");
        for p in &state.players {
            if p.club == Some(state.club.id) {
                let val = format_currency(p.value as i64);
                println!("  │ {:>3} │ {:<18} │ {:>3} │ {:>4} │ {:>4} │ {:>8} │", p.id, p.name, p.position.short(), p.age, p.overall(), val);
            }
        }
        println!("  └─────┴────────────────────┴─────┴──────┴──────┴──────────┘");

        println!("\n  1.  View Player Details");
        println!("  2.  Select Starting XI");
        println!("  3.  Sell Player");
        println!("  4.  Renew Contract");
        println!("  5.  Back");

        print!("\n  Choose: ");
        io::stdout().flush().unwrap();
        let choice = read_choice();

        match choice {
            1 => {
                print!("  Enter player ID: ");
                io::stdout().flush().unwrap();
                let pid: PlayerId = read_choice() as PlayerId;
                if let Some(p) = state.players.iter().find(|p| p.id == pid && p.club == Some(state.club.id)) {
                    show_player_details(p);
                } else {
                    println!("  Player not found!");
                    press_enter();
                }
            }
            2 => select_starting_xi(state),
            3 => sell_player(state),
            4 => renew_contract(state),
            _ => break,
        }
    }
}

fn show_player_details(p: &Player) {
    print_header(&format!("PLAYER DETAILS - {}", p.name));
    println!("  Position: {}  |  Age: {}  |  Nationality: Gen", p.position.short(), p.age);
    println!("  Overall: {}  |  Potential: {}  |  Personality: {}", p.overall(), p.potential, p.personality.label());
    println!("  Effective: {:.1}", p.effective());
    println!("\n  ── Attributes ──");
    println!("  Pace:{}, Shooting:{}, Passing:{}, Dribbling:{}", p.attrs.pace, p.attrs.shooting, p.attrs.passing, p.attrs.dribbling);
    println!("  Tackling:{}, Heading:{}, Strength:{}, Stamina:{}", p.attrs.tackling, p.attrs.heading, p.attrs.strength, p.attrs.stamina);
    println!("  Vision:{}, Composure:{}, GK:{}, FT:{}", p.attrs.vision, p.attrs.composure, p.attrs.goalkeeping, p.attrs.first_touch);
    println!("\n  ── Best Roles (attribute fit, out-of-position cost included) ──");
    let mut fits: Vec<(Role, f32)> = Role::all()
        .iter()
        .map(|&r| (r, engine::role_fit(&p.attrs, r, p.position)))
        .collect();
    fits.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (r, f) in fits.iter().take(4) {
        println!("  {:<24} {:>3.0}", r.label(), f.round());
    }
    println!("\n  ── Form ──");
    println!("  Morale: {}%  |  Fitness: {}%  |  Form: {}", p.morale, p.fitness, p.form);
    println!("  Condition: {}%  |  Sharpness: {}%  |  Happiness: {}%", p.condition, p.sharpness, p.happiness);
    println!("  Injury: {} days  |  Yellows: {}  |  Suspension: {} match(es)", p.injury_days, p.yellows, p.suspension);
    println!("  Contract expiry: {}  |  Release clause: {}", p.contract_expiry,
        p.release_clause.map(|v| format_currency(v as i64)).unwrap_or_else(|| "-".to_string()));
    println!("  Contract: {} weeks/yr, {} years left", format_currency(p.contract.wage as i64), p.contract.years);
    println!("  Value: {}", format_currency(p.value as i64));
    println!("\n  ── Season Stats ──");
    println!("  Apps: {}, Goals: {}, Assists: {}", p.stats.apps, p.stats.goals, p.stats.assists);
    println!("  Avg Rating: {:.1}", p.stats.avg_rating());
    press_enter();
}

fn sell_player(state: &mut GameState) {
    print!("  Enter player ID to sell: ");
    io::stdout().flush().unwrap();
    let pid: PlayerId = read_choice() as PlayerId;
    let idx = state.players.iter().position(|p| p.id == pid && p.club == Some(state.club.id));
    if let Some(i) = idx {
        let name = state.players[i].name.clone();
        let value = state.players[i].value as i64;
        println!("\n  Selling {} for {}?", name, format_currency(value));
        print!("  Confirm (y/n): ");
        io::stdout().flush().unwrap();
        let mut confirm = String::new();
        io::stdin().read_line(&mut confirm).unwrap();
        if confirm.trim().eq_ignore_ascii_case("y") {
            state.club.balance += value;
            state.club.squad.retain(|&id| id != pid);
            state.players[i].club = None;
            state.add_news(format!("{} sold to another club for {}!", name, format_currency(value)));
            println!("  Sold! New balance: {}", format_currency(state.club.balance));
        }
    } else {
        println!("  Player not found or not in squad!");
    }
    press_enter();
}

fn read_number(prompt: &str) -> i64 {
    print!("  {}", prompt);
    io::stdout().flush().unwrap();
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    input.trim().parse().unwrap_or(0)
}

fn renew_contract(state: &mut GameState) {
    print!("  Enter player ID to renew: ");
    io::stdout().flush().unwrap();
    let pid: PlayerId = read_choice() as PlayerId;
    let idx = state.players.iter().position(|p| p.id == pid && p.club == Some(state.club.id));
    let Some(i) = idx else {
        println!("  Player not found or not in squad!");
        press_enter();
        return;
    };
    let name = state.players[i].name.clone();
    let current = state.players[i].contract.wage;
    let demand = ((current as f64 * 1.1) as u32).max(1500);
    println!("\n  {} earns {}/wk. Agent demands {}/wk.", name,
        format_currency(current as i64), format_currency(demand as i64));
    let wage = read_number("  Wage offer per week (0 = walk away): ") as u32;
    if wage == 0 || wage < demand * 9 / 10 {
        println!("  Talks break down. {} is unhappy.", name);
        state.players[i].happiness = state.players[i].happiness.saturating_sub(15);
        state.players[i].morale = state.players[i].morale.saturating_sub(10);
        press_enter();
        return;
    }
    let years = read_number("  Years (1-5): ").clamp(1, 5) as u8;
    let agent_fee = wage as i64 * 2;
    state.club.balance -= agent_fee;
    state.players[i].contract.wage = wage;
    state.players[i].contract.years = years;
    state.players[i].contract_expiry = state.season + years as u16;
    state.players[i].morale = (state.players[i].morale + 8).min(100);
    state.players[i].happiness = (state.players[i].happiness + 10).min(100);
    state.add_news(format!("{} signs a new {}-year deal ({}k/wk).", name, years, wage / 1000));
    println!("  Renewed! Agent fee {}.", format_currency(agent_fee));
    press_enter();
}

// ── Transfer Market ──────────────────────────────────────────────────────────

/// Negotiate and sign a player template (from the database or shortlist).
/// Original flow: window gate -> budget/squad gates -> bid/counter loop (max
/// 3 rounds) -> contract talks -> completed signing.
fn negotiate_signing(state: &mut GameState, template: &Player) {
    if !deals::window_open(state.week) {
        println!("\n  Transfer window is CLOSED (open weeks 1-6 and 20-23).");
        press_enter();
        return;
    }
    if state.club.squad.len() >= 25 {
        println!("\n  Squad full! (25/25) Sell first.");
        press_enter();
        return;
    }
    let months_left = template.contract_expiry.saturating_sub(state.season) as i64 * 12;
    let bosman = deals::is_bosman(template.age, template.contract_expiry, state.season);
    let asking = if bosman {
        0
    } else {
        deals::asking_price(
            template.value,
            false,
            template.transfer_status == 1,
            months_left,
        )
    };
    println!("\n  Target: {} ({} age {}) OVR {} value {}",
        template.name, template.position.short(), template.age,
        template.overall(), format_currency(template.value as i64));
    if bosman {
        println!("  Out of contract: FREE transfer (wages + signing bonus only).");
    } else {
        println!("  Asking price: {} (months left on deal: {})", format_currency(asking), months_left);
    }
    println!("  Your transfer kitty: {}", format_currency(state.club.transfer_budget));

    let mut offer = deals::TransferOffer::new(template.id, 0, state.club.id);
    if bosman {
        offer.base_fee = 0;
        offer.send();
        offer.accept_club();
    } else {
        let bid = read_number("  Opening bid (0 = walk away): ");
        if bid <= 0 {
            return;
        }
        if bid > state.club.transfer_budget {
            println!("  Board blocks the bid: exceeds the transfer kitty.");
            press_enter();
            return;
        }
        offer.base_fee = bid;
        offer.send();
        loop {
            match deals::ai_evaluate(
                offer.total_cash(),
                asking,
                months_left,
                template.transfer_status == 1,
                template.transfer_status == 2,
                false,
            ) {
                deals::AiResponse::Accept => {
                    println!("  Seller ACCEPTS {}.", format_currency(offer.total_cash()));
                    offer.accept_club();
                    break;
                }
                deals::AiResponse::Counter(price) => {
                    if offer.rounds >= deals::TransferOffer::max_rounds() {
                        println!("  Seller walks away. Deal expires.");
                        offer.expire();
                        break;
                    }
                    println!("  Seller counters at {}.", format_currency(price));
                    let reply = read_text("  [a]ccept counter / [r]aise / [w]alk: ");
                    match reply.to_lowercase().as_str() {
                        "a" => {
                            offer.counter(price);
                            offer.accept_club();
                            break;
                        }
                        "r" => {
                            let nb = read_number("  New bid: ");
                            if nb <= 0 || !offer.counter(nb) {
                                offer.withdraw();
                                break;
                            }
                        }
                        _ => {
                            offer.withdraw();
                            println!("  You walk away.");
                            break;
                        }
                    }
                }
                deals::AiResponse::Reject => {
                    println!("  Seller REJECTS the bid outright.");
                    offer.reject();
                    break;
                }
            }
        }
    }
    if offer.state != deals::OfferState::AcceptedClub {
        press_enter();
        return;
    }

    // Contract talks: one round against the agent's demand.
    offer.to_talks();
    let demand = ((template.contract.wage as f64 * 1.2) as u32).max(2000);
    println!("\n  Contract talks: agent demands {}/wk, {} years.", format_currency(demand as i64), offer.years);
    let wage = read_number("  Your wage offer per week (0 = walk away): ") as u32;
    if wage == 0 || wage < (demand * 9 / 10) {
        println!("  Player rejects the terms.");
        offer.fail();
        press_enter();
        return;
    }
    let years = read_number("  Years (1-5): ").clamp(1, 5) as u8;
    let agent_fee = (wage as i64) * 4;
    let total_fee = if bosman { 0 } else { offer.total_cash() };
    if total_fee > state.club.transfer_budget {
        println!("  Board blocks the deal: fee exceeds the kitty.");
        press_enter();
        return;
    }
    offer.years = years;
    offer.wage_offer = wage;
    offer.complete();

    let mut new_player = template.clone();
    new_player.id = state.next_player_id;
    state.next_player_id += 1;
    new_player.club = Some(state.club.id);
    new_player.contract.wage = wage;
    new_player.contract.years = years;
    new_player.contract_expiry = state.season + years as u16;
    new_player.morale = 80;
    new_player.happiness = 80;
    let new_id = new_player.id;
    let new_name = new_player.name.clone();
    state.players.push(new_player);
    state.club.squad.push(new_id);
    state.club.balance -= total_fee + agent_fee;
    state.club.transfer_budget -= total_fee;
    state.add_news(format!("{} joined {} for {} ({}k/wk)!", new_name, state.club.name,
        format_currency(total_fee), wage / 1000));
    println!("\n  Signed {}! Fee {} + agent fee {}.", new_name,
        format_currency(total_fee), format_currency(agent_fee));
    press_enter();
}

// ── Loans ────────────────────────────────────────────────────────────────────
// Original loan model: fee up front, weekly wage split by wage_pct, monthly
// countdown on the End Week tick, optional recall clause and future fee.

fn loan_player_name(state: &GameState, pid: PlayerId) -> String {
    state.players.iter().find(|p| p.id == pid)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| format!("#{}", pid))
}

fn loans_screen(state: &mut GameState) {
    loop {
        print_header("LOANS");
        println!("\n  Window: {} (loans register inside the window)",
            if deals::window_open(state.week) { "OPEN" } else { "CLOSED" });
        let active: Vec<usize> = state.loans.iter().enumerate()
            .filter(|(_, l)| l.state == deals::OfferState::Active)
            .map(|(i, _)| i)
            .collect();
        if active.is_empty() {
            println!("\n  No active loans.");
        } else {
            println!("\n  Active loans:");
            for (n, &i) in active.iter().enumerate() {
                let l = &state.loans[i];
                let dir = if l.parent_club == state.club.id { "OUT" } else { "IN " };
                let wage = state.players.iter().find(|p| p.id == l.player_id)
                    .map(|p| p.contract.wage).unwrap_or(0);
                let weekly = deals::loan_weekly_cost(wage, l.wage_pct, l.parent_club == state.club.id);
                println!("  {}. [{}] {} - {} mo left, {}/wk cost to you{}{}",
                    n + 1, dir, loan_player_name(state, l.player_id), l.months_left,
                    format_currency(weekly),
                    if l.recall { ", recall clause" } else { "" },
                    l.future_fee.map(|f| format!(", buy option {}", format_currency(f))).unwrap_or_default());
            }
        }
        let awaiting: Vec<usize> = state.loans.iter().enumerate()
            .filter(|(_, l)| matches!(l.state, deals::OfferState::Completed | deals::OfferState::Recalled)
                && l.parent_club == state.club.id
                && state.players.iter().any(|p| p.id == l.player_id && p.club.is_none()))
            .map(|(i, _)| i)
            .collect();
        if !awaiting.is_empty() {
            println!("\n  Awaiting registration (squad was full on return):");
            for (n, &i) in awaiting.iter().enumerate() {
                println!("  {}. {}", n + 1, loan_player_name(state, state.loans[i].player_id));
            }
        }

        println!("\n  1.  Loan Out (squad player)");
        println!("  2.  Loan In (database player)");
        println!("  3.  Recall (clause required)");
        println!("  4.  Register Returnee");
        println!("  5.  Activate Buy Option");
        println!("  6.  Back");
        print!("\n  Choose: ");
        io::stdout().flush().unwrap();
        match read_choice() {
            1 => loan_out(state),
            2 => loan_in(state),
            3 => recall_loan(state),
            4 => register_returnee(state),
            5 => buy_option(state),
            _ => return,
        }
    }
}

fn loan_out(state: &mut GameState) {
    if !deals::window_open(state.week) {
        println!("\n  Transfer window is CLOSED (open weeks 1-6 and 20-23).");
        press_enter();
        return;
    }
    println!("\n  Squad players:");
    for pid in &state.club.squad.clone() {
        if let Some(p) = state.players.iter().find(|p| p.id == *pid) {
            println!("  #{} {} ({} age {} {}/wk)", p.id, p.name, p.position.short(), p.age,
                format_currency(p.contract.wage as i64));
        }
    }
    let pid = read_number("  Player ID to loan out (0 = back): ") as PlayerId;
    if pid == 0 || !state.club.squad.contains(&pid) {
        return;
    }
    let fee = read_number("  Loan fee up front (0 = none): ").max(0);
    let wage_pct = read_number("  Wage % paid by loanee (0-100): ").clamp(0, 100) as u8;
    let months = read_number("  Months (1-12): ").clamp(1, 12) as u8;
    let recall = read_text("  Recall clause? (y/n): ").to_lowercase() == "y";
    let loanee = state.league.iter().find(|c| c.id != state.club.id).map(|c| c.id).unwrap_or(0);

    let mut offer = deals::LoanOffer::new_loan(pid, state.club.id, loanee, fee, wage_pct, months, recall, None);
    offer.send();
    offer.activate();
    state.club.balance += fee;
    state.club.squad.retain(|&id| id != pid);
    if let Some(p) = state.players.iter_mut().find(|p| p.id == pid) {
        p.club = None;
        p.morale = (p.morale as i16 - 3).clamp(0, 100) as u8;
    }
    let name = loan_player_name(state, pid);
    state.loans.push(offer);
    state.add_news(format!("{} loaned out ({} fee, {}% wages, {} mo{}).", name,
        format_currency(fee), wage_pct, months, if recall { ", recall clause" } else { "" }));
    println!("\n  {} loaned out. Fee {} banked.", name, format_currency(fee));
    press_enter();
}

fn loan_in(state: &mut GameState) {
    if !deals::window_open(state.week) {
        println!("\n  Transfer window is CLOSED (open weeks 1-6 and 20-23).");
        press_enter();
        return;
    }
    if state.club.squad.len() >= 25 {
        println!("\n  Squad full! (25/25) Sell or loan out first.");
        press_enter();
        return;
    }
    let db = Database::load();
    let available = db.players();
    println!("\n  Enter a database player ID (1-{}, 0 = back): ", available.len());
    let pid = read_number("  Choose: ") as PlayerId;
    if pid == 0 || pid as usize > available.len() {
        return;
    }
    let template = available[pid as usize - 1].clone();
    println!("\n  Target: {} ({} age {}) OVR {} wage {}/wk",
        template.name, template.position.short(), template.age, template.overall(),
        format_currency(template.contract.wage as i64));
    let fee = read_number("  Your loan fee offer: ").max(0);
    let wage_pct = read_number("  Wage % you will pay (0-100): ").clamp(0, 100) as u8;
    let months = read_number("  Months (1-12): ").clamp(1, 12) as u8;
    let recall = read_text("  Accept recall clause? (y/n): ").to_lowercase() == "y";
    let ff = read_number("  Future fee / buy option (0 = none): ").max(0);
    let future_fee = if ff > 0 { Some(ff) } else { None };

    if !deals::ai_accept_loan(fee, template.value, wage_pct) {
        println!("\n  Parent club rejects: fee below ~1% of value and wage share under 60%.");
        press_enter();
        return;
    }
    if fee > state.club.transfer_budget {
        println!("\n  Board blocks the fee: exceeds the transfer kitty.");
        press_enter();
        return;
    }
    let mut offer = deals::LoanOffer::new_loan(template.id, 0, state.club.id, fee, wage_pct, months, recall, future_fee);
    offer.send();
    offer.activate();
    let mut new_player = template.clone();
    new_player.id = state.next_player_id;
    state.next_player_id += 1;
    new_player.club = Some(state.club.id);
    new_player.contract_expiry = state.season + 3;
    new_player.morale = 75;
    new_player.happiness = 75;
    let new_id = new_player.id;
    let new_name = new_player.name.clone();
    state.players.push(new_player);
    state.club.squad.push(new_id);
    state.club.balance -= fee;
    state.club.transfer_budget -= fee;
    // Rewrite the offer to reference the world copy.
    offer.player_id = new_id;
    state.loans.push(offer);
    state.add_news(format!("{} joined on loan ({} fee, {}% wages, {} mo{}).", new_name,
        format_currency(fee), wage_pct, months,
        future_fee.map(|f| format!(", buy option {}", format_currency(f))).unwrap_or_default()));
    println!("\n  {} signed on loan!", new_name);
    press_enter();
}

fn recall_loan(state: &mut GameState) {
    let recallable: Vec<usize> = state.loans.iter().enumerate()
        .filter(|(_, l)| l.state == deals::OfferState::Active && l.recall && l.parent_club == state.club.id)
        .map(|(i, _)| i)
        .collect();
    if recallable.is_empty() {
        println!("\n  No recallable loans (need an active loan-out with a clause).");
        press_enter();
        return;
    }
    for (n, &i) in recallable.iter().enumerate() {
        println!("  {}. {}", n + 1, loan_player_name(state, state.loans[i].player_id));
    }
    let n = read_number("  Recall # (0 = back): ");
    if n >= 1 && (n as usize) <= recallable.len() {
        let i = recallable[n as usize - 1];
        state.loans[i].recall();
        let name = loan_player_name(state, state.loans[i].player_id);
        state.add_news(format!("{} recalled from loan.", name));
        resolve_loan_return(state, i);
        press_enter();
    }
}

fn resolve_loan_return(state: &mut GameState, idx: usize) {
    let pid = state.loans[idx].player_id;
    let name = loan_player_name(state, pid);
    if state.club.squad.len() >= 25 {
        state.add_news(format!("{} returned but the squad is full - register from Loans.", name));
        println!("\n  {} is back but the squad is full. Use Register Returnee.", name);
        return;
    }
    if let Some(p) = state.players.iter_mut().find(|p| p.id == pid) {
        p.club = Some(state.club.id);
        p.sharpness = p.sharpness.max(80); // senior minutes keep them sharp
        p.condition = 100;
        p.morale = (p.morale as i16 + 3).clamp(0, 100) as u8;
    }
    if !state.club.squad.contains(&pid) {
        state.club.squad.push(pid);
    }
    state.add_news(format!("{} returned from loan and rejoins the squad.", name));
    println!("\n  {} rejoins the squad (sharpness topped up from senior minutes).", name);
}

fn register_returnee(state: &mut GameState) {
    let awaiting: Vec<usize> = state.loans.iter().enumerate()
        .filter(|(_, l)| matches!(l.state, deals::OfferState::Completed | deals::OfferState::Recalled)
            && l.parent_club == state.club.id
            && state.players.iter().any(|p| p.id == l.player_id && p.club.is_none()))
        .map(|(i, _)| i)
        .collect();
    if awaiting.is_empty() {
        println!("\n  Nobody waiting for registration.");
        press_enter();
        return;
    }
    if state.club.squad.len() >= 25 {
        println!("\n  Squad still full! Sell or release someone first.");
        press_enter();
        return;
    }
    for (n, &i) in awaiting.iter().enumerate() {
        println!("  {}. {}", n + 1, loan_player_name(state, state.loans[i].player_id));
    }
    let n = read_number("  Register # (0 = back): ");
    if n >= 1 && (n as usize) <= awaiting.len() {
        resolve_loan_return(state, awaiting[n as usize - 1]);
        press_enter();
    }
}

fn buy_option(state: &mut GameState) {
    let options: Vec<usize> = state.loans.iter().enumerate()
        .filter(|(_, l)| l.state == deals::OfferState::Active
            && l.loanee_club == state.club.id && l.future_fee.is_some())
        .map(|(i, _)| i)
        .collect();
    if options.is_empty() {
        println!("\n  No active loan-ins with a buy option.");
        press_enter();
        return;
    }
    for (n, &i) in options.iter().enumerate() {
        println!("  {}. {} - {}", n + 1, loan_player_name(state, state.loans[i].player_id),
            format_currency(state.loans[i].future_fee.unwrap_or(0)));
    }
    let n = read_number("  Activate # (0 = back): ");
    if n < 1 || (n as usize) > options.len() {
        return;
    }
    let i = options[n as usize - 1];
    let fee = state.loans[i].future_fee.unwrap_or(0);
    if fee > state.club.transfer_budget {
        println!("\n  Board blocks the fee: exceeds the transfer kitty.");
        press_enter();
        return;
    }
    let pid = state.loans[i].player_id;
    state.loans[i].complete();
    state.club.balance -= fee;
    state.club.transfer_budget -= fee;
    if let Some(p) = state.players.iter_mut().find(|p| p.id == pid) {
        p.contract.years = 3;
        p.contract_expiry = state.season + 3;
        p.morale = 82;
        p.happiness = 82;
    }
    let name = loan_player_name(state, pid);
    state.add_news(format!("Buy option activated: {} signs permanently for {}.", name, format_currency(fee)));
    println!("\n  {} signs permanently for {}!", name, format_currency(fee));
    press_enter();
}

/// Weekly loan tick: wage-split costs, monthly countdown, expiry/recall
/// resolution. Runs inside End Week before the week counter advances.
fn tick_loans(state: &mut GameState) {
    let human = state.club.id;
    let mut expired: Vec<usize> = Vec::new();
    for (i, l) in state.loans.iter_mut().enumerate() {
        if l.state != deals::OfferState::Active {
            continue;
        }
        let wage = state.players.iter().find(|p| p.id == l.player_id)
            .map(|p| p.contract.wage).unwrap_or(0);
        let cost = deals::loan_weekly_cost(wage, l.wage_pct, l.parent_club == human);
        state.club.balance -= cost;
        if l.tick_week() {
            l.complete();
            expired.push(i);
        }
    }
    for i in expired {
        let (pid, out) = (state.loans[i].player_id, state.loans[i].parent_club == human);
        let name = loan_player_name(state, pid);
        if out {
            state.add_news(format!("Loan expired: {} returns.", name));
            resolve_loan_return(state, i);
        } else {
            state.club.squad.retain(|&id| id != pid);
            state.players.retain(|p| p.id != pid);
            state.add_news(format!("Loan expired: {} returns to the parent club.", name));
            println!("\n  {} returns to the parent club.", name);
        }
    }
}

fn transfer_market(state: &mut GameState) {
    let db = Database::load();
    let available = db.players();

    loop {
        print_header("TRANSFER MARKET");
        println!("\n  Window: {} (open weeks 1-6, 20-23)", if deals::window_open(state.week) { "OPEN" } else { "CLOSED" });
        println!("  Available Players from FM 2023 Database ({} players)", available.len());
        println!("  Kitty: {}   Squad: {}/25   Shortlist: {}", format_currency(state.club.transfer_budget), state.club.squad.len(), state.scouted.len());
        println!("\n  ┌─────┬────────────────────┬─────┬──────┬──────────┬──────────┐");
        println!("  │ ID  │ Name               │ Pos │ Age  │ OVR    │ Value    │");
        println!("  ├─────┼────────────────────┼─────┼──────┼──────────┼──────────┤");

        for (i, p) in available.iter().enumerate().take(15) {
            let pos = p.position.short();
            let val = format_currency(p.value as i64);
            println!("  |{:>4}| {} {:<18} |{:>3}|{:>4}|{:>8}|{:>8}|",
                i + 1, "", p.name, pos, p.age, p.overall().to_string(), val);
        }
        println!("  └─────┴────────────────────┴─────┴──────┴──────────┴──────────┘");

        println!("\n  1.  Make an Offer (negotiate)");
        println!("  2.  View Player Details");
        println!("  3.  Scouted Shortlist");
        println!("  4.  Loans (in / out, recall, buy option)");
        println!("  5.  Back");

        print!("\n  Choose: ");
        io::stdout().flush().unwrap();
        let choice = read_choice();

        match choice {
            1 => {
                print!("  Enter player ID to bid for: ");
                io::stdout().flush().unwrap();
                let pid: PlayerId = read_choice() as PlayerId;
                if pid >= 1 && pid <= available.len() as PlayerId {
                    let template = available[pid as usize - 1].clone();
                    negotiate_signing(state, &template);
                } else {
                    println!("  Invalid ID.");
                    press_enter();
                }
            }
            2 => {
                print!("  Enter player ID: ");
                io::stdout().flush().unwrap();
                let pid: PlayerId = read_choice() as PlayerId;
                if pid >= 1 && pid <= available.len() as PlayerId {
                    show_player_details(&available[pid as usize - 1]);
                }
            }
            3 => {
                print_header("SCOUTED SHORTLIST");
                if state.scouted.is_empty() {
                    println!("\n  Empty. Assign scouts from the Scouting screen.");
                    press_enter();
                } else {
                    for (i, p) in state.scouted.iter().enumerate() {
                        println!("  {}. {} ({} age {} OVR {} knowledge {}%) - {}",
                            i + 1, p.name, p.position.short(), p.age, p.overall(),
                            p.scout_knowledge, format_currency(p.value as i64));
                    }
                    let n = read_number("\n  Bid for shortlist # (0 = back): ");
                    if n >= 1 && (n as usize) <= state.scouted.len() {
                        let template = state.scouted[n as usize - 1].clone();
                        negotiate_signing(state, &template);
                    }
                }
            }
            4 => loans_screen(state),
            _ => break,
        }
    }
}

// ── Training ─────────────────────────────────────────────────────────────────

fn training(state: &mut GameState) {
    print_header("TRAINING");
    let coach: u8 = state.staff.iter()
        .filter(|s| matches!(s.role, StaffRole::Coach | StaffRole::Assistant))
        .map(|s| s.quality).max().unwrap_or(10);
    let physio: u8 = state.staff.iter()
        .filter(|s| matches!(s.role, StaffRole::Physio))
        .map(|s| s.quality).max().unwrap_or(10);
    println!("\n  Coach quality: {}/20   Physio: {}/20   Facilities: {}/5",
        coach, physio, state.club.training_facilities);
    println!("  Growth follows age curves: teens grow fast, 30+ decline.");
    println!("  Heavy intensity grows faster but risks injury and drains condition.");

    println!("\n  Focus:");
    println!("  1.  Pace");
    println!("  2.  Shooting");
    println!("  3.  Passing");
    println!("  4.  Dribbling");
    println!("  5.  Tackling");
    println!("  6.  Heading");
    println!("  7.  Strength");
    println!("  8.  Stamina");
    println!("  9.  Vision");
    println!("  10. Composure");
    println!("  11. Goalkeeping");
    println!("  12. General (all attributes, slower)");
    println!("  13. Back");

    print!("\n  Choose: ");
    io::stdout().flush().unwrap();
    let choice = read_choice();
    if choice < 1 || choice > 12 {
        return;
    }

    println!("\n  Intensity: 1. Light (8k)  2. Normal (15k)  3. Heavy (25k)");
    print!("  Choose: ");
    io::stdout().flush().unwrap();
    let intensity = read_choice().clamp(1, 3) as u8 - 1; // 0, 1, 2
    let training_cost: i64 = [8000, 15000, 25000][intensity as usize];
    if state.club.balance < training_cost {
        println!("  Insufficient funds for training!");
        press_enter();
        return;
    }

    let mut rng = Pcg64Mcg::new(SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as u128);
    let attr_name = match choice {
        1 => "Pace", 2 => "Shooting", 3 => "Passing", 4 => "Dribbling",
        5 => "Tackling", 6 => "Heading", 7 => "Strength", 8 => "Stamina",
        9 => "Vision", 10 => "Composure", 11 => "Goalkeeping", _ => "All",
    };
    let coach_mult = 0.7 + coach as f32 * 0.03; // 10 -> 1.0, 20 -> 1.3
    let facility_mult = 0.8 + state.club.training_facilities as f32 * 0.08;
    let cond_drain = [0u8, 4, 8][intensity as usize];

    let mut improved = 0;
    let mut declined = 0;
    let mut hurt = Vec::new();
    let squad_ids = state.club.squad.clone();
    for pid in &squad_ids {
        if let Some(p) = state.players.iter_mut().find(|p| p.id == *pid) {
            if p.injury_days > 0 {
                continue; // injured players don't train
            }
            let morale_mult = 0.8 + p.morale as f32 / 500.0;
            let minutes_frac = (p.sharpness as f32 / 100.0).clamp(0.2, 1.0);
            // Focused attribute gets full focus; general spreads thin.
            let apply = |v: &mut u8, focused: bool| -> i8 {
                let base = if choice == 12 { 0.4 } else { 0.8 + intensity as f32 * 0.3 };
                let g = dev::weekly_gain(base, dev::age_growth_mult(p.age), minutes_frac,
                    focused, p.personality.growth_mult(), morale_mult, coach_mult * facility_mult, false);
                let pts = g.round().clamp(-1.0, 2.0) as i8;
                if pts > 0 {
                    *v = (*v + pts as u8).min(20);
                } else if pts < 0 {
                    *v = v.saturating_sub(1);
                }
                pts
            };
            let focused = |slot: usize| choice == slot || choice == 12;
            let mut delta = 0i8;
            delta += apply(&mut p.attrs.pace, focused(1));
            delta += apply(&mut p.attrs.shooting, focused(2));
            delta += apply(&mut p.attrs.passing, focused(3));
            delta += apply(&mut p.attrs.dribbling, focused(4));
            delta += apply(&mut p.attrs.tackling, focused(5));
            delta += apply(&mut p.attrs.heading, focused(6));
            delta += apply(&mut p.attrs.strength, focused(7));
            delta += apply(&mut p.attrs.stamina, focused(8));
            delta += apply(&mut p.attrs.vision, focused(9));
            delta += apply(&mut p.attrs.composure, focused(10));
            delta += apply(&mut p.attrs.goalkeeping, focused(11));
            p.attrs.first_touch = if delta > 0 {
                (p.attrs.first_touch + 1).min(20)
            } else {
                p.attrs.first_touch
            };
            if delta > 0 {
                improved += 1;
            } else if delta < 0 {
                declined += 1;
            }
            p.recompute_value();
            p.condition = p.condition.saturating_sub(cond_drain);
            // Training injuries: per-player share of the session risk.
            if rng.gen_bool(dev::training_injury_prob(intensity, p.condition as f32, physio) * 0.15) {
                p.injury_days = rng.gen_range(3..=14);
                hurt.push(p.name.clone());
            }
        }
    }

    state.club.balance -= training_cost;
    state.add_news(format!("Training ({} focus): {} improved, {} declined, {} injured.",
        attr_name, improved, declined, hurt.len()));
    println!("\n  {} improved, {} declined with age.", improved, declined);
    for name in hurt.iter().take(5) {
        println!("  INJURED in training: {}", name);
    }
    println!("\n  Cost: {} | Balance: {}", format_currency(training_cost), format_currency(state.club.balance));
    press_enter();
}

// ── Scouting ─────────────────────────────────────────────────────────────────

fn scouting(state: &mut GameState) {
    loop {
        print_header("SCOUTING");
        println!("\n  Scouting Network: Searching the FM 2023 Database for new talent...");
        println!("  Your Reputation: {} | Instant search: {} | Assignment: {}/week",
            state.club.reputation, format_currency(5000), format_currency(2000));
        println!("\n  Active assignments: {}   Shortlist: {}", state.assignments.len(), state.scouted.len());

        println!("\n  1.  Instant Search (Cost: {})", format_currency(5000));
        println!("  2.  Search by Position");
        println!("  3.  New Assignment (report matures in 1-4 weeks)");
        println!("  4.  Assignments + Shortlist");
        println!("  5.  Back");

        print!("\n  Choose: ");
        io::stdout().flush().unwrap();
        let choice = read_choice();

        match choice {
            1 => {
                let db = Database::load();
                let available = db.players();
                if state.club.balance < 5000 {
                    println!("  Insufficient funds for scouting!");
                    press_enter();
                    continue;
                }
                state.club.balance -= 5000;
                // Show top 15 highest-rated players not already in squad
                let mut candidates: Vec<&Player> = available.iter().filter(|p| {
                    !state.players.iter().any(|sp| sp.name == p.name && sp.club.is_some())
                }).collect();
                candidates.sort_by(|a, b| b.overall().cmp(&a.overall()));

                println!("\n  Top Scouting Results:");
                for p in candidates.iter().take(15) {
                    println!("  {} (Age {}, OVR {}) - {} | Value: {}", p.name, p.age, p.overall(), p.position.short(), format_currency(p.value as i64));
                }
                println!("\n  Total scouting cost: {}", format_currency(5000));
                press_enter();
            }
            2 => {
                let db = Database::load();
                let available = db.players();
                if state.club.balance < 5000 {
                    println!("  Insufficient funds for scouting!");
                    press_enter();
                    continue;
                }
                state.club.balance -= 5000;
                println!("\n  Select Position:");
                for (i, pos) in [Position::GK, Position::DEF, Position::MID, Position::FWD].iter().enumerate() {
                    println!("  {}. {}", i + 1, pos.short());
                }
                print!("  Choose: ");
                io::stdout().flush().unwrap();
                let pos_choice = read_choice();
                let pos = match pos_choice {
                    1 => Position::GK,
                    2 => Position::DEF,
                    3 => Position::MID,
                    4 => Position::FWD,
                    _ => Position::MID,
                };
                let mut candidates: Vec<&Player> = available.iter().filter(|p| p.position == pos).collect();
                candidates.sort_by(|a, b| b.overall().cmp(&a.overall()));
                println!("\n  Top {} ({}):", pos.short(), candidates.len());
                for p in candidates.iter().take(15) {
                    println!("  {} (Age {}, OVR {}) - Value: {}", p.name, p.age, p.overall(), format_currency(p.value as i64));
                }
                press_enter();
            }
            3 => {
                println!("\n  Position: 0. Any  1. GK  2. DEF  3. MID  4. FWD");
                let pc = read_number("  Choose: ");
                let position = match pc {
                    1 => Some(Position::GK),
                    2 => Some(Position::DEF),
                    3 => Some(Position::MID),
                    4 => Some(Position::FWD),
                    _ => None,
                };
                let max_age = read_number("  Max age (16-35): ").clamp(16, 35) as u8;
                let weeks = read_number("  Weeks (1-4): ").clamp(1, 4) as u8;
                let cost = 2000 * weeks as i64;
                if state.club.balance < cost {
                    println!("  Insufficient funds!");
                    press_enter();
                    continue;
                }
                state.club.balance -= cost;
                let id = state.next_assignment_id;
                state.next_assignment_id += 1;
                state.assignments.push(ScoutAssignment {
                    id,
                    position,
                    max_age,
                    weeks_left: weeks,
                    weeks_total: weeks,
                });
                state.add_news(format!("Scout assignment #{} started ({} weeks).", id, weeks));
                println!("  Assignment #{} started.", id);
                press_enter();
            }
            4 => {
                print_header("ASSIGNMENTS + SHORTLIST");
                if state.assignments.is_empty() {
                    println!("\n  No active assignments.");
                } else {
                    for a in &state.assignments {
                        let pos = a.position.map(|p| p.short()).unwrap_or("ANY");
                        println!("  #{} {} U{} - {} week(s) left", a.id, pos, a.max_age, a.weeks_left);
                    }
                }
                println!("\n  Shortlist ({}):", state.scouted.len());
                for (i, p) in state.scouted.iter().enumerate() {
                    println!("  {}. {} ({} age {} OVR {} knowledge {}%) - {}",
                        i + 1, p.name, p.position.short(), p.age, p.overall(),
                        p.scout_knowledge, format_currency(p.value as i64));
                }
                press_enter();
            }
            _ => break,
        }
    }
}

/// Tick scouting assignments: countdown, and mature finished ones into
/// shortlist reports (top 3 matching database players, knowledge 100).
fn tick_assignments(state: &mut GameState) {
    if state.assignments.is_empty() {
        return;
    }
    let db = Database::load();
    let mut matured = Vec::new();
    for a in state.assignments.iter_mut() {
        a.weeks_left = a.weeks_left.saturating_sub(1);
        if a.weeks_left == 0 {
            matured.push(a.clone());
        }
    }
    state.assignments.retain(|a| a.weeks_left > 0);
    for a in matured {
        let mut cands: Vec<&Player> = db.players().iter()
            .filter(|p| {
                (a.position.map_or(true, |pos| p.position == pos))
                    && p.age <= a.max_age
                    && !state.scouted.iter().any(|s| s.name == p.name)
                    && !state.players.iter().any(|sp| sp.name == p.name && sp.club.is_some())
            })
            .collect();
        cands.sort_by(|x, y| y.overall().cmp(&x.overall()));
        let mut names = Vec::new();
        for c in cands.into_iter().take(3) {
            let mut cp = c.clone();
            cp.scout_knowledge = 100;
            names.push(format!("{} ({} OVR {})", cp.name.clone(), cp.position.short(), cp.overall()));
            state.scouted.push(cp);
        }
        state.add_news(if names.is_empty() {
            format!("Scout report #{}: no new targets found.", a.id)
        } else {
            format!("Scout report #{}: {}.", a.id, names.join(", "))
        });
    }
}

// ── Finances ─────────────────────────────────────────────────────────────────

fn finances(state: &mut GameState) {
    print_header("FINANCES");
    let wage_total: u64 = state.players.iter()
        .filter(|p| p.club == Some(state.club.id))
        .map(|p| p.contract.wage as u64)
        .sum();
    let weekly_revenue: i64 = 30000 + (state.club.stadium_capacity as i64 / 1000) * 5 + (state.club.reputation as i64) * 500;
    let season_wage = wage_total as i64 * 38;
    let season_revenue = weekly_revenue * 38;

    println!("\n  ── Financial Summary ──");
    println!("  Balance:      {}", format_currency(state.club.balance as i64));
    println!("  Weekly Revenue: {} (Matchday + Sponsorship + TV)", format_currency(weekly_revenue));
    println!("  Weekly Wages:   {} ({} players)", format_currency(wage_total as i64), state.club.squad.len());
    println!("  Season Revenue: {}", format_currency(season_revenue));
    println!("  Season Wages:   {}", format_currency(season_wage));
    println!("  Season Profit:  {}", format_currency(season_revenue - season_wage));
    println!("\n  ── Revenue Breakdown ──");
    println!("  Matchday:     {}", format_currency((state.club.stadium_capacity as i64 / 1000) * 5 + 10000));
    println!("  Sponsorship:  {}", format_currency(state.club.reputation as i64 * 500 + 5000));
    println!("  TV Rights:    {}", format_currency(20000));
    println!("\n  ── Wage Bill ──");
    let mut sorted_wages: Vec<_> = state.players.iter()
        .filter(|p| p.club == Some(state.club.id))
        .collect();
    sorted_wages.sort_by(|a, b| b.contract.wage.cmp(&a.contract.wage));
    for p in sorted_wages.iter().take(10) {
        println!("  {}  -  {}/wk", format_currency(p.contract.wage as i64), p.name);
    }
    press_enter();
}

// ── League Table ─────────────────────────────────────────────────────────────

fn league_table(state: &GameState) {
    print_header("LEAGUE TABLE");
    let n = state.league.len();

    // Build standings
    let mut rows: HashMap<ClubId, TableRow> = HashMap::new();
    for club in &state.league {
        rows.insert(club.id, TableRow {
            club: club.id,
            played: 0,
            won: 0,
            drawn: 0,
            lost: 0,
            gf: 0,
            ga: 0,
        });
    }

    for f in &state.fixtures {
        if f.played {
            let row = rows.get_mut(&f.home).unwrap();
            row.played += 1;
            row.gf += f.home_goals as u16;
            row.ga += f.away_goals as u16;
            if f.home_goals > f.away_goals {
                row.won += 1;
            } else if f.home_goals == f.away_goals {
                row.drawn += 1;
            } else {
                row.lost += 1;
            }
            let row = rows.get_mut(&f.away).unwrap();
            row.played += 1;
            row.gf += f.away_goals as u16;
            row.ga += f.home_goals as u16;
            if f.away_goals > f.home_goals {
                row.won += 1;
            } else if f.away_goals == f.home_goals {
                row.drawn += 1;
            } else {
                row.lost += 1;
            }
        }
    }

    let mut standings: Vec<_> = rows.values().cloned().collect();
    standings.sort_by(|a, b| {
        b.points().cmp(&a.points())
            .then(b.gd().cmp(&a.gd()))
            .then(b.gf.cmp(&a.gf))
    });

    println!("\n  ┌──────┬──────────────────┬────┬────┬────┬────┬────┬────┬────┐");
    println!("  │ Rank │ Club             │ P  │ W  │ D  │ L  │ GF │ GA │ PTS│");
    println!("  ├──────┼──────────────────┼────┼────┼────┼────┼────┼────┼────┤");
    for (i, row) in standings.iter().enumerate() {
        let club_name = state.league.iter().find(|c| c.id == row.club).map(|c| c.name.clone()).unwrap_or_else(|| "Unknown".to_string());
        let highlight = if row.club == state.club.id { " >>>" } else { "" };
        println!("  |{:>5}| {:<16}{:>4} |{:>3}|{:>3}|{:>3}|{:>3}|{:>3}|{:>3}|{:>4}|",
            i + 1, club_name, highlight, row.played, row.won, row.drawn, row.lost, row.gf, row.ga, row.points());
    }
    println!("  └──────┴──────────────────┴────┴────┴────┴────┴────┴────┴────┘");
    println!("\n  {} is in {}th position (of {})", state.club.name, standings.iter().position(|r| r.club == state.club.id).unwrap_or(0) + 1, n);
    press_enter();
}

// ── Club & Board ─────────────────────────────────────────────────────────────

fn club_board(state: &GameState) {
    print_header("CLUB & BOARD");
    println!("\n  {} ({}):", state.club.name, state.club.short);
    println!("  Reputation:    {} / 100", state.club.reputation);
    println!("  Stadium Cap:   {} (ticket {})", state.club.stadium_capacity, format_currency(state.club.ticket_price as i64));
    println!("  Balance:       {}", format_currency(state.club.balance));
    println!("  Transfer kitty:{}   Wage budget: {}/wk", format_currency(state.club.transfer_budget), format_currency(state.club.wage_budget / 52));
    println!("  Morale:        {}%", state.club.morale);
    println!("  Formation:     {} ({})", state.club.tactic.formation.label(), state.club.tactic.mentality.label());
    println!("  Squad Size:    {}/25", state.club.squad.len());
    println!("\n  ── Board ──");
    println!("  Expectation:   finish top {}", state.club.board_expectation);
    println!("  Board confidence: {}%   Fan confidence: {}%", state.club.board_confidence, state.club.fan_confidence);
    println!("  Position:      {} (board {})", get_league_position(state),
        if get_league_position(state) as u8 <= state.club.board_expectation { "satisfied" } else { "concerned" });
    println!("\n  ── Facilities ──");
    println!("  Training: {} / 5   Youth: {} / 5", state.club.training_facilities, state.club.youth_facilities);
    println!("\n  ── Youth Academy ──");
    println!("  Next Intake: Season {}", state.season + 1);
    press_enter();
}

// ── Staff ────────────────────────────────────────────────────────────────────

fn staff_screen(state: &mut GameState) {
    loop {
        print_header("STAFF");
        if state.staff.is_empty() {
            println!("\n  No backroom staff hired. Hire coaches, scouts, physios, analysts.");
        } else {
            for (i, s) in state.staff.iter().enumerate() {
                println!("  {}. {} - {} (quality {}/20, {}/wk)", i + 1, s.name, s.role.label(), s.quality, format_currency(s.wage as i64));
            }
        }
        let coach_bonus: u8 = state.staff.iter().filter(|s| matches!(s.role, StaffRole::Coach | StaffRole::Assistant)).map(|s| s.quality).max().unwrap_or(10);
        let physio_bonus: u8 = state.staff.iter().filter(|s| matches!(s.role, StaffRole::Physio)).map(|s| s.quality).max().unwrap_or(10);
        println!("\n  Training quality (best coach): {}/20   Medical quality (best physio): {}/20", coach_bonus, physio_bonus);
        println!("\n  1. Hire (cost: signing wage only)");
        println!("  2. Release");
        println!("  3. Back");
        print!("\n  Choose: ");
        io::stdout().flush().unwrap();
        match read_choice() {
            1 => {
                println!("\n  Roles:");
                for (i, r) in StaffRole::all().iter().enumerate() {
                    println!("  {}. {}", i + 1, r.label());
                }
                print!("  Role (1-5): ");
                io::stdout().flush().unwrap();
                let r = read_choice();
                if !(1..=5).contains(&r) {
                    continue;
                }
                let name = read_text("  Staff name:");
                if name.is_empty() {
                    continue;
                }
                let mut rng = Pcg64Mcg::new(SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as u128 ^ state.staff.len() as u128);
                let quality = rng.gen_range(8..=18);
                let wage = (quality as u32) * 400;
                state.staff.push(Staff { name, role: StaffRole::all()[r - 1], quality, wage });
                state.add_news("Backroom staff appointment confirmed.".to_string());
            }
            2 => {
                print!("  Number to release: ");
                io::stdout().flush().unwrap();
                let n = read_choice();
                if n >= 1 && n <= state.staff.len() {
                    let s = state.staff.remove(n - 1);
                    state.add_news(format!("{} left the backroom staff.", s.name));
                }
            }
            _ => return,
        }
    }
}

// ── Medical Centre ───────────────────────────────────────────────────────────

fn medical_screen(state: &mut GameState) {
    print_header("MEDICAL CENTRE");
    let physio: u8 = state.staff.iter().filter(|s| matches!(s.role, StaffRole::Physio)).map(|s| s.quality).max().unwrap_or(10);
    println!("\n  Physio quality: {}/20 (higher = faster recovery)", physio);
    let mut rows: Vec<&Player> = state.players.iter().filter(|p| p.club == Some(state.club.id)).collect();
    rows.sort_by_key(|p| std::cmp::Reverse(p.injury_days));
    println!("\n  Injured / suspended / exhausted:");
    let mut any = false;
    for p in &rows {
        if p.injury_days > 0 || p.suspension > 0 || p.condition < 80 {
            any = true;
            println!("  #{} {} - injury {}d, susp {}m, cond {}%, sharp {}%",
                p.id, p.name, p.injury_days, p.suspension, p.condition, p.sharpness);
        }
    }
    if !any {
        println!("  Squad fully healthy.");
    }
    println!("\n  1. Rest squad (+condition, -sharpness edge)");
    println!("  2. Back");
    print!("\n  Choose: ");
    io::stdout().flush().unwrap();
    if read_choice() == 1 {
        for pid in state.club.squad.clone() {
            if let Some(p) = state.players.iter_mut().find(|p| p.id == pid) {
                p.condition = (p.condition + 12).min(100);
                if p.injury_days > 0 {
                    let heal = 2 + (physio / 8) as u16;
                    p.injury_days = p.injury_days.saturating_sub(heal);
                }
            }
        }
        state.add_news("Squad given recovery time; condition improved.".to_string());
        println!("  Recovery done.");
        press_enter();
    }
}

// ── Youth ────────────────────────────────────────────────────────────────────

fn youth_screen(state: &mut GameState) {
    print_header("YOUTH INTAKE");
    println!("\n  Youth facilities: {} / 5 (higher = better newgens)", state.club.youth_facilities);
    println!("  Last intake season: {}", if state.last_intake_season == 0 { "-".to_string() } else { state.last_intake_season.to_string() });
    let youths: Vec<&Player> = state.players.iter()
        .filter(|p| p.club == Some(state.club.id) && p.age <= 18)
        .collect();
    println!("\n  Prospects in squad ({}):", youths.len());
    for p in youths.iter().take(10) {
        println!("  #{} {} ({} age {}) OVR {} POT {}", p.id, p.name, p.position.short(), p.age, p.overall(), p.potential);
    }
    println!("\n  Intake arrives automatically at season rollover.");
    press_enter();
}

fn run_youth_intake(state: &mut GameState) {
    // Original intake rule: 2 + facilities bonus prospects, ages 15-17,
    // strength scaled by reputation + youth facilities.
    let mut rng = Pcg64Mcg::new(SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as u128 ^ state.season as u128);
    let count = 2 + (state.club.youth_facilities as usize / 2);
    let strength = ((state.club.reputation as i32) / 5 + state.club.youth_facilities as i32 * 2).clamp(8, 28);
    let mut names = Vec::new();
    for _ in 0..count {
        let pos = [Position::GK, Position::DEF, Position::MID, Position::FWD][rng.gen_range(0..4)];
        let mut p = Gen::new(rng.gen()).player(pos, strength);
        p.age = rng.gen_range(15..=17) as u8;
        p.id = state.next_player_id;
        state.next_player_id += 1;
        p.club = Some(state.club.id);
        p.contract_expiry = state.season + 3;
        names.push(format!("{} ({} OVR {})", p.name.clone(), p.position.short(), p.overall()));
        state.club.squad.push(p.id);
        state.players.push(p);
    }
    state.last_intake_season = state.season;
    state.add_news(format!("Youth intake: {} graduate(s) - {}.", count, names.join(", ")));
}

// ── News & Events ────────────────────────────────────────────────────────────

fn news_screen(state: &GameState) {
    print_header("NEWS & EVENTS");
    if state.news.is_empty() {
        println!("\n  No news this week.");
    } else {
        for (i, news) in state.news.iter().rev().enumerate() {
            println!("  {}. {}", i + 1, news);
        }
    }
    println!("\n  ── Upcoming Fixtures ──");
    let upcoming: Vec<_> = state.fixtures.iter().filter(|f| !f.played && (f.home == state.club.id || f.away == state.club.id)).take(5).collect();
    if upcoming.is_empty() {
        println!("  No upcoming fixtures.");
    } else {
        for f in upcoming {
            let home = state.league.iter().find(|c| c.id == f.home).map(|c| c.name.clone()).unwrap_or_default();
            let away = state.league.iter().find(|c| c.id == f.away).map(|c| c.name.clone()).unwrap_or_default();
            println!("  Round {}: {} vs {}", f.round, home, away);
        }
    }
    press_enter();
}

// ── Save/Load ────────────────────────────────────────────────────────────────

fn save_game(state: &GameState) {
    let json = serde_json::to_string_pretty(state).unwrap();
    if let Err(e) = std::fs::write("savegame.json", &json) {
        println!("  Error saving: {}", e);
    } else {
        println!("  Game saved to savegame.json!");
    }
    press_enter();
}

fn load_game() -> Option<GameState> {
    match std::fs::read_to_string("savegame.json") {
        Ok(json) => match serde_json::from_str(&json) {
            Ok(state) => {
                println!("  Game loaded!");
                Some(state)
            }
            Err(e) => {
                println!("  Error loading: {}", e);
                None
            }
        },
        Err(_) => None,
    }
}

// ── Daily World Tick ─────────────────────────────────────────────────────────
// Parallelism model (original): every lap (day) each club acts — AI players
// recover, youths grow, veterans decline, AI clubs sign free agents, and
// morale drifts to baseline. End Week runs 7 of these; Advance Day runs one.

fn tick_world_day(state: &mut GameState, rng: &mut Pcg64Mcg) {
    state.day += 1;
    let human = state.club.id;
    let surge = deals::window_open(state.week) && (state.week == 6 || state.week == 23);

    // Human squad: light daily recovery + morale drift toward 65.
    for pid in state.club.squad.clone() {
        if let Some(p) = state.players.iter_mut().find(|p| p.id == pid) {
            p.condition = (p.condition + 1).min(100);
            if p.morale < 65 {
                p.morale += 1;
            } else if p.morale > 65 {
                p.morale -= 1;
            }
        }
    }

    // Rest of the world: recovery, growth/decline, morale drift.
    for p in state.players.iter_mut() {
        if p.club == Some(human) {
            continue;
        }
        if p.injury_days > 0 {
            p.injury_days -= 1;
            continue;
        }
        p.condition = (p.condition + 2).min(100);
        if p.morale < 65 {
            p.morale += 1;
        } else if p.morale > 65 {
            p.morale -= 1;
        }
        if p.club.is_none() {
            continue; // free agents don't train
        }
        if p.age <= 21 && p.overall() < p.potential && rng.gen_bool(0.05) {
            let attr = match p.position {
                Position::GK => {
                    if rng.gen_bool(0.5) {
                        &mut p.attrs.goalkeeping
                    } else {
                        &mut p.attrs.composure
                    }
                }
                Position::DEF => match rng.gen_range(0..3) {
                    0 => &mut p.attrs.tackling,
                    1 => &mut p.attrs.heading,
                    _ => &mut p.attrs.strength,
                },
                Position::MID => match rng.gen_range(0..3) {
                    0 => &mut p.attrs.passing,
                    1 => &mut p.attrs.vision,
                    _ => &mut p.attrs.stamina,
                },
                Position::FWD => match rng.gen_range(0..3) {
                    0 => &mut p.attrs.shooting,
                    1 => &mut p.attrs.dribbling,
                    _ => &mut p.attrs.pace,
                },
            };
            *attr = (*attr + 1).min(99);
        } else if p.age >= 33 && rng.gen_bool(0.03) {
            if rng.gen_bool(0.5) {
                p.attrs.pace = p.attrs.pace.saturating_sub(1).max(1);
            } else {
                p.attrs.stamina = p.attrs.stamina.saturating_sub(1).max(1);
            }
        }
    }

    // AI transfer activity: short squads sign affordable free agents.
    // Options are gathered fresh per club so a signing is never double-sold.
    let mut digest: Vec<String> = Vec::new();
    // AI clearout first: bloated squads release their lowest-rated player,
    // which keeps the free pool (and the market) alive in parallel.
    for i in 0..state.league.len() {
        let cid = state.league[i].id;
        if cid == human {
            continue;
        }
        if state.league[i].squad.len() <= 20 || !rng.gen_bool(0.03) {
            continue;
        }
        let mut worst: Option<(PlayerId, u8)> = None;
        for pid in state.league[i].squad.clone() {
            if let Some(p) = state.players.iter().find(|p| p.id == pid) {
                let o = p.overall();
                if worst.map_or(true, |(_, w)| o < w) {
                    worst = Some((pid, o));
                }
            }
        }
        if let Some((pid, _)) = worst {
            state.league[i].squad.retain(|&id| id != pid);
            if let Some(p) = state.players.iter_mut().find(|p| p.id == pid) {
                if p.club == Some(cid) {
                    p.club = None;
                }
                p.morale = p.morale.saturating_sub(5);
            }
        }
    }
    for i in 0..state.league.len() {
        let cid = state.league[i].id;
        if cid == human {
            continue;
        }
        let short = state.league[i].squad.len() < 25;
        let budget = state.league[i].balance;
        if !short || budget < 1_000_000 {
            continue;
        }
        if !rng.gen_bool(if surge { 0.12 } else { 0.04 }) {
            continue;
        }
        let cap = (budget / 8).max(0) as u64;
        let options: Vec<PlayerId> = state
            .players
            .iter()
            .filter(|p| p.club.is_none() && p.age <= 32 && p.value <= cap)
            .map(|p| p.id)
            .collect();
        if options.is_empty() {
            continue;
        }
        let fid = options[rng.gen_range(0..options.len())];
        let fee = state.players.iter().find(|p| p.id == fid).map(|p| p.value).unwrap_or(0);
        if let Some(p) = state.players.iter_mut().find(|p| p.id == fid) {
            p.club = Some(cid);
            p.morale = 75;
        }
        state.league[i].squad.push(fid);
        state.league[i].balance -= fee as i64;
        let nm = state.players.iter().find(|p| p.id == fid).map(|p| p.name.clone()).unwrap_or_default();
        let cn = state.league[i].name.clone();
        digest.push(format!("{} signed {} ({})", cn, nm, format_currency(fee as i64)));
    }
    // One digest line per day so parallel activity is visible without spam.
    match digest.len() {
        0 => {}
        1..=3 => state.add_news(format!("Elsewhere: {}.", digest.join("; "))),
        n => state.add_news(format!(
            "Elsewhere: {} AI deals completed, biggest: {}.",
            n,
            digest.into_iter().take(2).collect::<Vec<_>>().join("; ")
        )),
    }
    if surge {
        state.add_news("Transfer deadline day looms: expect a surge of AI bids.".to_string());
    }
}

fn advance_day(state: &mut GameState) {
    let mut rng = Pcg64Mcg::new(
        SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as u128
            ^ state.day as u128,
    );
    tick_world_day(state, &mut rng);
    println!("\n  Advanced to Day {} (Week {}).", state.day, state.week);
    press_enter();
}

// ── End Week ──────────────────────────────────────────────────────────────────

fn end_week(state: &mut GameState) {
    let prev_window = deals::window_open(state.week);
    // Weekly recovery tick (original rules): small condition restore,
    // injury countdown, suspended players serve bans only via matches.
    for pid in state.club.squad.clone() {
        if let Some(p) = state.players.iter_mut().find(|p| p.id == pid) {
            p.condition = (p.condition + 6).min(100);
            if p.injury_days > 0 {
                p.injury_days -= 1;
            }
        }
    }
    // Seven daily world laps: every club recovers, develops, and deals.
    let mut rng = Pcg64Mcg::new(
        SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as u128
            ^ state.day as u128,
    );
    for _ in 0..7 {
        tick_world_day(state, &mut rng);
    }
    state.week += 1;
    // Transfer-window transitions and scouting ticks run every week.
    let now_open = deals::window_open(state.week);
    if now_open && !prev_window {
        state.add_news("The transfer window OPENS. Deals can be registered.".to_string());
    } else if !now_open && prev_window {
        state.add_news("The transfer window SHUTS. Pending bids expire.".to_string());
    }
    tick_assignments(state);
    tick_loans(state);
    if state.week > 38 {
        // Season rollover (original pipeline): awards -> aging -> contracts ->
        // intake -> reset fixtures/stats.
        season_awards(state);
        state.season += 1;
        state.week = 1;
        state.day = 1;
        for p in state.players.iter_mut() {
            p.age = p.age.saturating_add(1);
            p.stats = SeasonStats::default();
            p.yellows = 0;
            p.condition = 100;
            // Retirements: 35+ with rising odds; released to free agency.
            // (Kept in pool as free agents rather than deleted.)
        }
        // Expired contracts: release players aged 33+ or low morale, else renew 1yr.
        // Loaned-in players on active deals are protected from release.
        let human = state.club.id;
        let protected: Vec<PlayerId> = state.loans.iter()
            .filter(|l| l.state == deals::OfferState::Active && l.loanee_club == human)
            .map(|l| l.player_id)
            .collect();
        let mut released = Vec::new();
        for p in state.players.iter_mut().filter(|p| p.club == Some(state.club.id)) {
            if p.contract_expiry <= state.season && !protected.contains(&p.id) {
                if p.age >= 33 || p.morale < 40 {
                    released.push(p.id);
                } else {
                    p.contract_expiry = state.season + 1;
                }
            }
        }
        for pid in released {
            if let Some(p) = state.players.iter_mut().find(|p| p.id == pid) {
                p.club = None;
            }
            state.club.squad.retain(|&id| id != pid);
        }
        run_youth_intake(state);
        state.fixtures = generate_fixtures(&state.league);
        state.add_news(format!("Season {} begins! Board expects top {}.", state.season, state.club.board_expectation));
    } else {
        state.add_news("Week completed. Preparing for next match.".to_string());
    }
    println!("\n  Advanced to Week {} Season {}!", state.week, state.season);
    press_enter();
}

fn season_awards(state: &mut GameState) {
    // Original awards: league-wide Golden Boot from recorded goals (now that
    // every round is simulated, AI players score too), announced via news.
    let mut top: Option<(String, String, u16)> = None;
    for p in state.players.iter().filter(|p| p.club.is_some()) {
        let beats = match &top {
            None => true,
            Some((_, _, g)) => p.stats.goals > *g,
        };
        if beats {
            let club = state.league.iter().find(|c| Some(c.id) == p.club)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| state.club.name.clone());
            top = Some((p.name.clone(), club, p.stats.goals));
        }
    }
    if let Some((name, club, goals)) = top {
        if club == state.club.name {
            state.club.fan_confidence = (state.club.fan_confidence as i16 + 2).clamp(0, 100) as u8;
        }
        state.add_news(format!("Season awards: {} ({}) wins the Golden Boot ({} goals).", name, club, goals));
    }
}

// ── Utility ──────────────────────────────────────────────────────────────────

fn format_currency(amount: i64) -> String {
    let neg = amount < 0;
    let amount = amount.unsigned_abs();
    let s = if amount >= 1_000_000 {
        format!("{}.{}m", amount / 1_000_000, (amount % 1_000_000) / 100_000)
    } else if amount >= 1_000 {
        format!("{}.{}k", amount / 1_000, (amount % 1_000) / 100)
    } else {
        amount.to_string()
    };
    if neg { format!("-{}", s) } else { s }
}

fn get_league_position(state: &GameState) -> usize {
    let mut rows: HashMap<ClubId, TableRow> = HashMap::new();
    for club in &state.league {
        rows.insert(club.id, TableRow {
            club: club.id, played: 0, won: 0, drawn: 0, lost: 0, gf: 0, ga: 0,
        });
    }
    for f in &state.fixtures {
        if f.played {
            if let Some(row) = rows.get_mut(&f.home) {
                row.played += 1; row.gf += f.home_goals as u16; row.ga += f.away_goals as u16;
                if f.home_goals > f.away_goals { row.won += 1; } else if f.home_goals == f.away_goals { row.drawn += 1; } else { row.lost += 1; }
            }
            if let Some(row) = rows.get_mut(&f.away) {
                row.played += 1; row.gf += f.away_goals as u16; row.ga += f.home_goals as u16;
                if f.away_goals > f.home_goals { row.won += 1; } else if f.away_goals == f.home_goals { row.drawn += 1; } else { row.lost += 1; }
            }
        }
    }
    let mut standings: Vec<_> = rows.values().cloned().collect();
    standings.sort_by(|a, b| b.points().cmp(&a.points()).then(b.gd().cmp(&a.gd())));
    standings.iter().position(|r| r.club == state.club.id).unwrap_or(0) + 1
}

fn main() {
    println!("\n  ╔══════════════════════════════════════════╗");
    println!("  ║   ⚽  FOOTBALL MANAGER CLI  🏆          ║");
    println!("  ║   Built with Rust & FM 2023 Data        ║");
    println!("  ╚══════════════════════════════════════════╝");
    
    loop {
        match main_menu() {
            MenuAction::NewGame => {
                if let Some(state) = new_game() {
                    game_loop(state);
                }
            }
            MenuAction::Quit => break,
        }
    }
    println!("\n  Thanks for playing Football Manager CLI! ⚽\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_club(id: ClubId, name: &str, human: bool) -> Club {
        Club {
            id,
            name: name.to_string(),
            short: "TST".to_string(),
            squad: Vec::new(),
            balance: 10_000_000,
            reputation: 60,
            stadium_capacity: 30_000,
            tactic: Tactic::default(),
            is_human: human,
            morale: 75,
            transfer_budget: 3_000_000,
            wage_budget: 1_000_000,
            board_confidence: 60,
            fan_confidence: 60,
            board_expectation: 10,
            training_facilities: 3,
            youth_facilities: 3,
            ticket_price: 30,
            league: "Test League".to_string(),
            lineup: Vec::new(),
        }
    }

    #[test]
    fn world_tick_ai_signs_over_time() {
        let mut rng = Pcg64Mcg::new(999);
        let mut human = test_club(1, "Human", true);
        let mut ai = test_club(2, "AI United", false);
        let mut players = Vec::new();
        let mut pid: PlayerId = 1;
        let mut hp = Gen::new(1).player(Position::MID, 20);
        hp.id = pid;
        hp.club = Some(1);
        human.squad.push(pid);
        players.push(hp);
        pid += 1;
        for i in 0..18 {
            let mut p = Gen::new(100 + i).player(Position::MID, 20);
            p.id = pid;
            p.club = Some(2);
            p.value = 500_000;
            ai.squad.push(pid);
            players.push(p);
            pid += 1;
        }
        for i in 0..5 {
            let mut p = Gen::new(200 + i).player(Position::FWD, 15);
            p.id = pid;
            p.club = None;
            p.value = 100_000;
            p.age = 25;
            players.push(p);
            pid += 1;
        }
        let mut state = GameState {
            season: 2024,
            week: 1,
            club: human,
            league: vec![ai],
            fixtures: Vec::new(),
            fixture_idx: 0,
            players,
            next_player_id: pid,
            news: Vec::new(),
            difficulty: Difficulty::Amateur,
            saved: false,
            staff: Vec::new(),
            last_intake_season: 0,
            assignments: Vec::new(),
            scouted: Vec::new(),
            next_assignment_id: 1,
            loans: Vec::new(),
            day: 1,
            cup: None,
        };
        for _ in 0..300 {
            tick_world_day(&mut state, &mut rng);
        }
        assert!(
            state.league[0].squad.len() > 18,
            "AI club signed nobody in 300 ticks (squad {})",
            state.league[0].squad.len()
        );
        assert!(
            state.news.iter().any(|n| n.contains("Elsewhere")),
            "no digest news posted: {:?}",
            state.news.iter().take(3).collect::<Vec<_>>()
        );
    }

    fn squad_of(n_per_line: usize) -> Vec<Player> {
        // A balanced pool: n keepers/def/mid/fwd, ids 1.. .
        let mut out = Vec::new();
        let mut id: PlayerId = 1;
        for (pos, seed0) in [
            (Position::GK, 0u64),
            (Position::DEF, 100),
            (Position::MID, 200),
            (Position::FWD, 300),
        ] {
            for k in 0..n_per_line {
                let mut p = Gen::new(seed0 + k as u64).player(pos, 20);
                p.id = id;
                p.club = Some(1);
                out.push(p);
                id += 1;
            }
        }
        out
    }

    #[test]
    fn resolve_lineup_honours_manager_pick() {
        let squad = squad_of(4);
        let refs: Vec<&Player> = squad.iter().collect();
        let tactic = Tactic::default(); // 4-4-2

        // Force a forward (last id) into the GK slot: an extreme experiment.
        let fwd_id = squad.iter().rev().find(|p| p.position == Position::FWD).unwrap().id;
        let mut lineup = vec![0u32; 11];
        lineup[0] = fwd_id;
        let xi = resolve_lineup(&refs, &tactic, &lineup);
        assert_eq!(xi[0].0.id, fwd_id, "manager's slot-0 pick was ignored");
        // Every slot filled, no player used twice.
        assert_eq!(xi.len(), 11);
        let mut ids: Vec<PlayerId> = xi.iter().map(|(p, _, _)| p.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 11, "a player was fielded in two slots");
    }

    #[test]
    fn empty_lineup_auto_picks_a_full_xi() {
        let squad = squad_of(4);
        let refs: Vec<&Player> = squad.iter().collect();
        let xi = resolve_lineup(&refs, &Tactic::default(), &[]);
        assert_eq!(xi.len(), 11);
        // The auto GK slot should be filled by an actual keeper when available.
        assert_eq!(xi[0].0.position, Position::GK);
    }

    #[test]
    fn injured_players_are_not_auto_selected() {
        let mut squad = squad_of(4);
        // Injure every forward; a 4-4-2 still needs two up top from elsewhere.
        for p in squad.iter_mut() {
            if p.position == Position::FWD {
                p.injury_days = 7;
            }
        }
        let refs: Vec<&Player> = squad.iter().collect();
        let xi = resolve_lineup(&refs, &Tactic::default(), &[]);
        assert!(
            xi.iter().all(|(p, _, _)| p.injury_days == 0),
            "an injured player was auto-selected"
        );
    }
}




