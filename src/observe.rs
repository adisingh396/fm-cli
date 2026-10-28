//! Spectator / observer mode: watch matches unfold minute by minute and browse
//! a whole league season's tables and individual stat lines, all powered by the
//! possession engine in `matchsim` — the same 22-player simulation the sandbox
//! uses, so what you watch is what actually drives the numbers.

use std::collections::HashMap;
use std::io::{self, Write};

use rand_pcg::Pcg64Mcg;

use crate::data::Database;
use crate::engine;
use crate::matchsim::{self, EventKind, MatchPlayer, MatchStats, Side, Venue};
use crate::model::*;

fn prompt(msg: &str) -> String {
    print!("{}", msg);
    io::stdout().flush().ok();
    let mut s = String::new();
    io::stdin().read_line(&mut s).ok();
    s.trim().to_string()
}

fn read_idx(msg: &str) -> Option<usize> {
    prompt(msg).parse::<usize>().ok()
}

fn pause() {
    prompt("\n  (Enter to continue) ");
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    }
}

/// Public entry: the observer top menu.
pub fn run(db: &Database) {
    let mut rng = Pcg64Mcg::new(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    );
    loop {
        println!("\n╔══════════════════════════════════════════╗");
        println!("║           OBSERVER / SPECTATOR            ║");
        println!("╚══════════════════════════════════════════╝");
        println!("  1. Watch a single match live (minute by minute)");
        println!("  2. Spectate a full league season");
        println!("  3. Back");
        match read_idx("\n  Choose: ") {
            Some(1) => watch_match(db, &mut rng),
            Some(2) => spectate_season(db, &mut rng),
            _ => return,
        }
    }
}

// ── League / club pickers ──────────────────────────────────────────────

/// Playable leagues (≥8 clubs), strongest-labelled first, like New Game.
fn playable_leagues(db: &Database) -> Vec<(String, Vec<ClubId>)> {
    let mut by: HashMap<String, Vec<ClubId>> = HashMap::new();
    for c in db.clubs() {
        let junk = c.name.trim().len() < 2 || c.name.trim().chars().all(|ch| ch.is_ascii_digit());
        if !c.league.trim().is_empty() && !junk {
            by.entry(c.league.clone()).or_default().push(c.id);
        }
    }
    let mut v: Vec<(String, Vec<ClubId>)> = by.into_iter().filter(|(_, ids)| ids.len() >= 8).collect();
    v.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    v
}

fn pick_league(db: &Database) -> Option<(String, Vec<ClubId>)> {
    let leagues = playable_leagues(db);
    if leagues.is_empty() {
        println!("\n  No league data found (need data/players.csv).");
        pause();
        return None;
    }
    println!("\n  Leagues:");
    let shown = leagues.len().min(24);
    for (i, (name, ids)) in leagues.iter().take(shown).enumerate() {
        println!("  {:>2}. {} ({} clubs)", i + 1, name, ids.len());
    }
    let c = read_idx(&format!("\n  Choose league (1-{}): ", shown))?;
    if c == 0 || c > shown {
        return None;
    }
    Some(leagues[c - 1].clone())
}

fn club_players<'a>(db: &'a Database, club: &Club) -> Vec<&'a Player> {
    let idx: HashMap<PlayerId, usize> =
        db.players().iter().enumerate().map(|(i, p)| (p.id, i)).collect();
    club.squad
        .iter()
        .filter_map(|pid| idx.get(pid).map(|&i| &db.players()[i]))
        .collect()
}

// ── Lineup selection ───────────────────────────────────────────────────

/// Best 4-3-3 from a squad: fill each line with the players who fit its roles
/// best (a keeper in goal, the strongest defenders at the back, and so on).
/// `id_base` tags this side's caller ids so events map back to names.
fn build_xi(players: &[&Player], id_base: usize) -> (Vec<MatchPlayer>, Vec<String>) {
    let lines: [(usize, usize); 4] = [(0, 1), (1, 4), (2, 3), (3, 3)];
    let mut used = vec![false; players.len()];
    let mut xi: Vec<MatchPlayer> = Vec::with_capacity(11);
    let mut names: Vec<String> = Vec::with_capacity(11);
    let mut push = |i: usize, line: usize, xi: &mut Vec<MatchPlayer>, names: &mut Vec<String>| {
        let p = players[i];
        let (role, _) = engine::best_role_for(&p.attrs, p.position, line);
        names.push(p.name.clone());
        xi.push(MatchPlayer {
            id: id_base + xi.len(),
            pos: p.position,
            role,
            attrs: p.attrs,
            start_fatigue: 0.0,
            form: 1.0,
        });
    };
    for (line, count) in lines {
        let mut cands: Vec<(f32, usize)> = (0..players.len())
            .filter(|&i| !used[i])
            .map(|i| (engine::best_role_for(&players[i].attrs, players[i].position, line).1, i))
            .collect();
        cands.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        for (_, i) in cands.into_iter().take(count) {
            used[i] = true;
            push(i, line, &mut xi, &mut names);
        }
    }
    // Backfill if the squad was thin.
    if xi.len() < 11 {
        for i in 0..players.len() {
            if xi.len() >= 11 {
                break;
            }
            if !used[i] {
                used[i] = true;
                let line = engine::line_of(players[i].position);
                push(i, line, &mut xi, &mut names);
            }
        }
    }
    (xi, names)
}

/// A club's manager-style dials for the engine, inferred from reputation so
/// bigger clubs back themselves a touch more.
fn club_dials(rep: u8) -> (f32, f32) {
    let ambition = 0.9 + (rep as f32 / 100.0) * 0.4; // 0.9 .. 1.3
    let quality = 0.9 + (rep as f32 / 100.0) * 0.25; // 0.9 .. 1.15
    (ambition, quality)
}

// ── Live single match ──────────────────────────────────────────────────

fn watch_match(db: &Database, rng: &mut Pcg64Mcg) {
    let Some((_lname, ids)) = pick_league(db) else { return };
    let mut clubs: Vec<Club> = ids
        .iter()
        .filter_map(|id| db.clubs().iter().find(|c| c.id == *id).cloned())
        .collect();
    clubs.sort_by(|a, b| b.reputation.cmp(&a.reputation).then(a.name.cmp(&b.name)));
    println!("\n  Clubs:");
    for (i, c) in clubs.iter().enumerate() {
        println!("  {:>2}. {} (Rep {})", i + 1, clip(&c.name, 30), c.reputation);
    }
    let Some(h) = read_idx("\n  Home club #: ") else { return };
    let Some(a) = read_idx("  Away club #: ") else { return };
    if h == 0 || a == 0 || h > clubs.len() || a > clubs.len() || h == a {
        println!("  Invalid pick.");
        pause();
        return;
    }
    let home = clubs[h - 1].clone();
    let away = clubs[a - 1].clone();
    let hp = club_players(db, &home);
    let ap = club_players(db, &away);
    if hp.len() < 7 || ap.len() < 7 {
        println!("  One of these squads is too thin to field a side.");
        pause();
        return;
    }
    let (hxi, hnames) = build_xi(&hp, 0);
    let (axi, anames) = build_xi(&ap, 1000);
    let (h_amb, h_q) = club_dials(home.reputation);
    let (a_amb, a_q) = club_dials(away.reputation);
    let mut side_h = Side { players: &hxi, bench: &[], ambition: h_amb, quality: h_q, threat: 1.0 };
    let mut side_a = Side { players: &axi, bench: &[], ambition: a_amb, quality: a_q, threat: 1.0 };
    let (th, ta) = matchsim::threat_ratio(&side_h, &side_a);
    side_h.threat = th;
    side_a.threat = ta;
    let o = matchsim::simulate(&side_h, &side_a, Venue::HomeA, (0, 0), rng);

    let name_of = |id: usize| -> String {
        if id >= 1000 {
            anames.get(id - 1000).cloned().unwrap_or_default()
        } else {
            hnames.get(id).cloned().unwrap_or_default()
        }
    };

    println!("\n══════════════════════════════════════════════════════════");
    println!("  {}  vs  {}", home.name, away.name);
    println!("══════════════════════════════════════════════════════════");
    println!("  Kick-off. {} at home.\n", clip(&home.name, 24));
    let mut ev = o.events.clone();
    ev.sort_by_key(|e| e.minute);
    let (mut sh, mut sa) = (0u8, 0u8);
    for e in &ev {
        match e.kind {
            EventKind::Goal | EventKind::SetPiece => {
                if e.side_a {
                    sh += 1;
                } else {
                    sa += 1;
                }
                let who = if e.side_a { &home.name } else { &away.name };
                let assist = e
                    .assist
                    .map(|i| format!(" (assist: {})", clip(&name_of(i), 18)))
                    .unwrap_or_default();
                let dead = if e.kind == EventKind::SetPiece {
                    " (dead ball)"
                } else {
                    ""
                };
                println!(
                    "  {:>2}'  \u{26bd} GOAL{} — {} {}-{}  {}{}",
                    e.minute, dead, clip(who, 18), sh, sa, clip(&name_of(e.actor), 18), assist
                );
            }
            EventKind::Injury => {
                let side = if e.side_a { &home.name } else { &away.name };
                println!(
                    "  {:>2}'  \u{1fa79} Injury — {} ({}) forced off",
                    e.minute, clip(&name_of(e.actor), 18), clip(side, 14)
                );
            }
            EventKind::Penalty => {}
        }
    }
    if ev
        .iter()
        .all(|e| e.kind != EventKind::Goal && e.kind != EventKind::SetPiece)
    {
        println!("  A goalless watch — chances came and went.");
    }
    println!("\n  ── FULL TIME ──");
    println!("  {} {} - {} {}", clip(&home.name, 22), o.goals_a, o.goals_b, clip(&away.name, 22));
    println!(
        "  Possession {}%-{}%   Shots {}-{}   xG {:.1}-{:.1}",
        o.poss_a,
        100 - o.poss_a,
        o.shots_a,
        o.shots_b,
        o.xg_a,
        o.xg_b
    );
    println!(
        "  Final mentalities: {} {}  |  {} {}",
        clip(&home.name, 16),
        mentality_label(o.end_mentality_a),
        clip(&away.name, 16),
        mentality_label(o.end_mentality_b)
    );

    print_sheet(&home.name, true, &o.stats, &name_of);
    print_sheet(&away.name, false, &o.stats, &name_of);
    pause();
}

fn mentality_label(m: i8) -> &'static str {
    match m {
        i8::MIN..=-2 => "Very Defensive",
        -1 => "Defensive",
        0 => "Balanced",
        1 => "Positive",
        _ => "Attacking",
    }
}

fn print_sheet(team: &str, side_a: bool, stats: &[MatchStats], name_of: &dyn Fn(usize) -> String) {
    println!("\n  {} — player ratings", team);
    println!("  ┌──────────────────────┬─────┬───┬───┬────┬────┬─────┬─────┬──────┐");
    println!("  │ Player               │ Min │ G │ A │ Sh │ KP │ Drb │ Tkl │ Rate │");
    println!("  ├──────────────────────┼─────┼───┼───┼────┼────┼─────┼─────┼──────┤");
    let mut rows: Vec<&MatchStats> = stats.iter().filter(|s| s.side_a == side_a).collect();
    rows.sort_by(|a, b| b.rating.partial_cmp(&a.rating).unwrap_or(std::cmp::Ordering::Equal));
    for s in rows {
        println!(
            "  │ {:<21}│ {:>3} │ {:>1} │ {:>1} │ {:>2} │ {:>2} │ {:>3} │ {:>3} │ {:>4.1} │",
            clip(&name_of(s.id), 21),
            s.minutes,
            s.goals,
            s.assists,
            s.shots,
            s.key_passes,
            s.dribbles_cmp,
            s.tackles + s.interceptions,
            s.rating
        );
    }
    println!("  └──────────────────────┴─────┴───┴───┴────┴────┴─────┴─────┴──────┘");
}

// ── Full league season spectate ────────────────────────────────────────

#[derive(Default, Clone)]
struct Line {
    name: String,
    club: usize,
    pos: Position,
    apps: u16,
    goals: u16,
    assists: u16,
    shots: u16,
    key_passes: u16,
    big_chances: u16,
    dribbles: u16,
    tackles: u16,
    passes_att: u32,
    passes_cmp: u32,
    rating_sum: f32,
    rating_apps: u16,
}

impl Line {
    fn avg(&self) -> f32 {
        if self.rating_apps == 0 {
            0.0
        } else {
            self.rating_sum / self.rating_apps as f32
        }
    }
}

#[derive(Default, Clone)]
struct Row {
    name: String,
    rep: u8,
    p: u16,
    w: u16,
    d: u16,
    l: u16,
    gf: u16,
    ga: u16,
}

impl Row {
    fn pts(&self) -> u16 {
        self.w * 3 + self.d
    }
    fn gd(&self) -> i32 {
        self.gf as i32 - self.ga as i32
    }
}

fn spectate_season(db: &Database, rng: &mut Pcg64Mcg) {
    let Some((lname, ids)) = pick_league(db) else { return };
    // Build a compact local world: clubs + a flat player pool for this league.
    let mut clubs: Vec<Club> = ids
        .iter()
        .filter_map(|id| db.clubs().iter().find(|c| c.id == *id).cloned())
        .collect();
    clubs.sort_by(|a, b| b.reputation.cmp(&a.reputation).then(a.name.cmp(&b.name)));
    if clubs.len() % 2 == 1 {
        clubs.pop();
    }
    let n = clubs.len();
    if n < 4 {
        println!("  Not enough clubs to run a season.");
        pause();
        return;
    }

    // Flat pool: lines[club] = Vec<Line indices>. Build per-club player refs.
    let pidx: HashMap<PlayerId, usize> =
        db.players().iter().enumerate().map(|(i, p)| (p.id, i)).collect();
    let mut lines: Vec<Line> = Vec::new();
    let mut club_lines: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (ci, c) in clubs.iter().enumerate() {
        for pid in &c.squad {
            if let Some(&i) = pidx.get(pid) {
                let p = &db.players()[i];
                club_lines[ci].push(lines.len());
                lines.push(Line {
                    name: p.name.clone(),
                    club: ci,
                    pos: p.position,
                    ..Default::default()
                });
            }
        }
    }
    // A stable attr lookup by line index (mirrors `lines` order).
    let attrs: Vec<(Attributes, Position)> = {
        let mut v = Vec::with_capacity(lines.len());
        for (ci, c) in clubs.iter().enumerate() {
            let _ = ci;
            for pid in &c.squad {
                if let Some(&i) = pidx.get(pid) {
                    let p = &db.players()[i];
                    v.push((p.attrs, p.position));
                }
            }
        }
        v
    };

    let mut table: Vec<Row> = clubs
        .iter()
        .map(|c| Row { name: c.name.clone(), rep: c.reputation, ..Default::default() })
        .collect();

    println!(
        "\n  Simulating {} ({} clubs, double round-robin) with the possession engine…",
        lname, n
    );
    io::stdout().flush().ok();

    // Double round-robin via the circle method.
    let mut order: Vec<usize> = (0..n).collect();
    for leg in 0..2 {
        for _ in 0..n - 1 {
            for i in 0..n / 2 {
                let (x, y) = (order[i], order[n - 1 - i]);
                let (home, away) = if leg == 0 { (x, y) } else { (y, x) };
                play_and_record(&clubs, &club_lines, &attrs, &mut lines, &mut table, home, away, rng);
            }
            let last = order.pop().unwrap();
            order.insert(1, last);
        }
    }

    browse_season(&lname, &clubs, &club_lines, &lines, &table, db, rng);
}

#[allow(clippy::too_many_arguments)]
fn play_and_record(
    clubs: &[Club],
    club_lines: &[Vec<usize>],
    attrs: &[(Attributes, Position)],
    lines: &mut [Line],
    table: &mut [Row],
    home: usize,
    away: usize,
    rng: &mut Pcg64Mcg,
) {
    // Build XIs from each club's lines: pick the best 11 by role fit per line.
    let (hxi, hmap) = local_xi(&club_lines[home], attrs, 0);
    let (axi, amap) = local_xi(&club_lines[away], attrs, 1000);
    if hxi.len() < 7 || axi.len() < 7 {
        return;
    }
    let (h_amb, h_q) = club_dials(clubs[home].reputation);
    let (a_amb, a_q) = club_dials(clubs[away].reputation);
    let mut side_h = Side { players: &hxi, bench: &[], ambition: h_amb, quality: h_q, threat: 1.0 };
    let mut side_a = Side { players: &axi, bench: &[], ambition: a_amb, quality: a_q, threat: 1.0 };
    let (th, ta) = matchsim::threat_ratio(&side_h, &side_a);
    side_h.threat = th;
    side_a.threat = ta;
    let o = matchsim::simulate(
        &side_h,
        &side_a,
        Venue::HomeA,
        (0, 0),
        rng,
    );
    // Standings.
    table[home].p += 1;
    table[away].p += 1;
    table[home].gf += o.goals_a as u16;
    table[home].ga += o.goals_b as u16;
    table[away].gf += o.goals_b as u16;
    table[away].ga += o.goals_a as u16;
    match o.goals_a.cmp(&o.goals_b) {
        std::cmp::Ordering::Greater => {
            table[home].w += 1;
            table[away].l += 1;
        }
        std::cmp::Ordering::Less => {
            table[away].w += 1;
            table[home].l += 1;
        }
        std::cmp::Ordering::Equal => {
            table[home].d += 1;
            table[away].d += 1;
        }
    }
    // Fold per-player lines.
    for s in &o.stats {
        let line_i = if s.side_a { hmap.get(&s.id) } else { amap.get(&s.id) };
        let Some(&li) = line_i else { continue };
        let l = &mut lines[li];
        l.apps += 1;
        l.goals += s.goals as u16;
        l.assists += s.assists as u16;
        l.shots += s.shots as u16;
        l.key_passes += s.key_passes as u16;
        l.big_chances += s.big_chances_created as u16;
        l.dribbles += s.dribbles_cmp as u16;
        l.tackles += (s.tackles + s.interceptions) as u16;
        l.passes_att += s.passes_att as u32;
        l.passes_cmp += s.passes_cmp as u32;
        l.rating_sum += s.rating;
        l.rating_apps += 1;
    }
}

/// Build a best XI from a club's line indices; returns the lineup plus a map
/// from caller id → line index so results fold back to the right player.
fn local_xi(
    club_line_idxs: &[usize],
    attrs: &[(Attributes, Position)],
    id_base: usize,
) -> (Vec<MatchPlayer>, HashMap<usize, usize>) {
    let shape: [(usize, usize); 4] = [(0, 1), (1, 4), (2, 3), (3, 3)];
    let mut used = vec![false; club_line_idxs.len()];
    let mut xi = Vec::with_capacity(11);
    let mut map = HashMap::new();
    let mut add = |slot: usize, xi: &mut Vec<MatchPlayer>, map: &mut HashMap<usize, usize>| {
        let li = club_line_idxs[slot];
        let (a, pos) = attrs[li];
        let line = engine::line_of(pos);
        let (role, _) = engine::best_role_for(&a, pos, line);
        let id = id_base + xi.len();
        map.insert(id, li);
        xi.push(MatchPlayer { id, pos, role, attrs: a, start_fatigue: 0.0, form: 1.0 });
    };
    for (line, count) in shape {
        let mut cands: Vec<(f32, usize)> = (0..club_line_idxs.len())
            .filter(|&s| !used[s])
            .map(|s| {
                let (a, pos) = attrs[club_line_idxs[s]];
                (engine::best_role_for(&a, pos, line).1, s)
            })
            .collect();
        cands.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal));
        for (_, s) in cands.into_iter().take(count) {
            used[s] = true;
            add(s, &mut xi, &mut map);
        }
    }
    if xi.len() < 11 {
        for s in 0..club_line_idxs.len() {
            if xi.len() >= 11 {
                break;
            }
            if !used[s] {
                used[s] = true;
                add(s, &mut xi, &mut map);
            }
        }
    }
    (xi, map)
}

fn browse_season(
    lname: &str,
    clubs: &[Club],
    club_lines: &[Vec<usize>],
    lines: &[Line],
    table: &[Row],
    db: &Database,
    rng: &mut Pcg64Mcg,
) {
    loop {
        println!("\n  ══ {} — season complete ══", lname);
        println!("  1. Final table");
        println!("  2. Top scorers");
        println!("  3. Most assists");
        println!("  4. Top rated (≥15 apps)");
        println!("  5. Most chances created");
        println!("  6. View a club's squad stats");
        println!("  7. Watch a match live");
        println!("  8. Back");
        match read_idx("\n  Choose: ") {
            Some(1) => show_table(table),
            Some(2) => leader_board(lines, "Top scorers", |l| l.goals as f32, |l| format!("{}g {}a", l.goals, l.assists)),
            Some(3) => leader_board(lines, "Most assists", |l| l.assists as f32, |l| format!("{}a {}g", l.assists, l.goals)),
            Some(4) => {
                let mut idx: Vec<usize> = (0..lines.len()).filter(|&i| lines[i].rating_apps >= 15).collect();
                idx.sort_by(|&a, &b| lines[b].avg().partial_cmp(&lines[a].avg()).unwrap_or(std::cmp::Ordering::Equal));
                println!("\n  Top rated (≥15 apps):");
                for &i in idx.iter().take(15) {
                    let l = &lines[i];
                    println!(
                        "   {:>5.2}  {:<22} {:<20} {}g {}a",
                        l.avg(), clip(&l.name, 22), clip(&clubs[l.club].name, 20), l.goals, l.assists
                    );
                }
                pause();
            }
            Some(5) => leader_board(lines, "Most chances created", |l| l.big_chances as f32, |l| format!("{} big, {} KP", l.big_chances, l.key_passes)),
            Some(6) => view_club(clubs, club_lines, lines),
            Some(7) => watch_from_season(clubs, db, rng),
            _ => return,
        }
    }
}

fn show_table(table: &[Row]) {
    let mut idx: Vec<usize> = (0..table.len()).collect();
    idx.sort_by(|&a, &b| {
        table[b].pts().cmp(&table[a].pts())
            .then(table[b].gd().cmp(&table[a].gd()))
            .then(table[b].gf.cmp(&table[a].gf))
    });
    println!("\n  ┌────┬──────────────────────┬────┬────┬────┬────┬─────┬─────┐");
    println!("  │ #  │ Club                 │ P  │ W  │ D  │ L  │ GD  │ Pts │");
    println!("  ├────┼──────────────────────┼────┼────┼────┼────┼─────┼─────┤");
    for (rank, &i) in idx.iter().enumerate() {
        let r = &table[i];
        println!(
            "  │ {:>2} │ {:<21}│ {:>2} │ {:>2} │ {:>2} │ {:>2} │ {:>3} │ {:>3} │",
            rank + 1, clip(&r.name, 21), r.p, r.w, r.d, r.l, r.gd(), r.pts()
        );
    }
    println!("  └────┴──────────────────────┴────┴────┴────┴────┴─────┴─────┘");
    pause();
}

fn leader_board(lines: &[Line], title: &str, key: impl Fn(&Line) -> f32, extra: impl Fn(&Line) -> String) {
    let mut idx: Vec<usize> = (0..lines.len()).collect();
    idx.sort_by(|&a, &b| key(&lines[b]).partial_cmp(&key(&lines[a])).unwrap_or(std::cmp::Ordering::Equal));
    println!("\n  {}:", title);
    for &i in idx.iter().take(15) {
        let l = &lines[i];
        println!("   {:<24} {:<6.1}  avg {:.2}   {}", clip(&l.name, 24), key(l), l.avg(), extra(l));
    }
    pause();
}

fn view_club(clubs: &[Club], club_lines: &[Vec<usize>], lines: &[Line]) {
    println!("\n  Clubs:");
    for (i, c) in clubs.iter().enumerate() {
        println!("  {:>2}. {}", i + 1, clip(&c.name, 30));
    }
    let Some(c) = read_idx("\n  Club #: ") else { return };
    if c == 0 || c > clubs.len() {
        return;
    }
    let ci = c - 1;
    let mut idx = club_lines[ci].clone();
    idx.sort_by(|&a, &b| {
        (lines[b].goals + lines[b].assists).cmp(&(lines[a].goals + lines[a].assists))
    });
    println!("\n  {} — squad stats", clubs[ci].name);
    println!("  ┌──────────────────────┬─────┬────┬────┬────┬─────┬─────┬──────┐");
    println!("  │ Player               │ Pos │ Ap │ G  │ A  │ KP  │ T+I │ Rate │");
    println!("  ├──────────────────────┼─────┼────┼────┼────┼─────┼─────┼──────┤");
    for &i in &idx {
        let l = &lines[i];
        if l.apps == 0 {
            continue;
        }
        println!(
            "  │ {:<21}│ {:>3} │ {:>2} │ {:>2} │ {:>2} │ {:>3} │ {:>3} │ {:>4.2} │",
            clip(&l.name, 21),
            l.pos.short(),
            l.apps,
            l.goals,
            l.assists,
            l.key_passes,
            l.tackles,
            l.avg()
        );
    }
    println!("  └──────────────────────┴─────┴────┴────┴────┴─────┴─────┴──────┘");
    pause();
}

fn watch_from_season(clubs: &[Club], db: &Database, rng: &mut Pcg64Mcg) {
    println!("\n  Clubs:");
    for (i, c) in clubs.iter().enumerate() {
        println!("  {:>2}. {}", i + 1, clip(&c.name, 30));
    }
    let Some(h) = read_idx("\n  Home #: ") else { return };
    let Some(a) = read_idx("  Away #: ") else { return };
    if h == 0 || a == 0 || h > clubs.len() || a > clubs.len() || h == a {
        return;
    }
    let home = clubs[h - 1].clone();
    let away = clubs[a - 1].clone();
    let hp = club_players(db, &home);
    let ap = club_players(db, &away);
    if hp.len() < 7 || ap.len() < 7 {
        println!("  Squad too thin.");
        pause();
        return;
    }
    let (hxi, hnames) = build_xi(&hp, 0);
    let (axi, anames) = build_xi(&ap, 1000);
    let (h_amb, h_q) = club_dials(home.reputation);
    let (a_amb, a_q) = club_dials(away.reputation);
    let mut side_h = Side { players: &hxi, bench: &[], ambition: h_amb, quality: h_q, threat: 1.0 };
    let mut side_a = Side { players: &axi, bench: &[], ambition: a_amb, quality: a_q, threat: 1.0 };
    let (th, ta) = matchsim::threat_ratio(&side_h, &side_a);
    side_h.threat = th;
    side_a.threat = ta;
    let o = matchsim::simulate(
        &side_h,
        &side_a,
        Venue::HomeA,
        (0, 0),
        rng,
    );
    let name_of = |id: usize| -> String {
        if id >= 1000 {
            anames.get(id - 1000).cloned().unwrap_or_default()
        } else {
            hnames.get(id).cloned().unwrap_or_default()
        }
    };
    let mut ev = o.events.clone();
    ev.sort_by_key(|e| e.minute);
    let (mut sh, mut sa) = (0u8, 0u8);
    println!("\n  {} vs {}", home.name, away.name);
    for e in &ev {
        if e.kind == EventKind::Goal || e.kind == EventKind::SetPiece {
            if e.side_a {
                sh += 1;
            } else {
                sa += 1;
            }
            let who = if e.side_a { &home.name } else { &away.name };
            let assist = e.assist.map(|i| format!(" (assist {})", clip(&name_of(i), 16))).unwrap_or_default();
            let dead = if e.kind == EventKind::SetPiece { " (db)" } else { "" };
            println!("  {:>2}'  \u{26bd} {}{} {}-{}  {}{}", e.minute, clip(who, 16), dead, sh, sa, clip(&name_of(e.actor), 16), assist);
        } else if e.kind == EventKind::Injury {
            println!("  {:>2}'  \u{1fa79} {} injured", e.minute, clip(&name_of(e.actor), 16));
        }
    }
    println!("  FT: {} {}-{} {}   (xG {:.1}-{:.1}, poss {}%, corners {}-{}, set-piece goals {}-{})", clip(&home.name, 18), o.goals_a, o.goals_b, clip(&away.name, 18), o.xg_a, o.xg_b, o.poss_a, o.corners_a, o.corners_b, o.sp_goals_a, o.sp_goals_b);
    print_sheet(&home.name, true, &o.stats, &name_of);
    print_sheet(&away.name, false, &o.stats, &name_of);
    pause();
}
