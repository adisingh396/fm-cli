//! 2026 Players Database Loader
//!
//! Loads player and club data from the 2026 players database
//! (`data/players.csv`, an EA FC-style dump), men's football only,
//! mapping its ratings onto the shared 0-99 attribute scale.
//!
//! Falls back to procedural generation if the CSV is not present.

use std::collections::HashMap;
use std::fs;

use rand::Rng;
use rand_pcg::Pcg64Mcg;

use crate::gen::Gen;
use crate::model::*;

/// The 2026 players database (all players and clubs) loaded at startup.
#[derive(Clone)]
pub struct Database {
    players: Vec<Player>,
    clubs: Vec<Club>,
    club_index: HashMap<String, usize>,
}

/// The world runs on a FIFA-style 0-100 attribute scale (elite ~88-95, 99 a
/// rare anomaly). The 2026 database ratings are already 0-99, so they map
/// straight through; generated players are scaled to match. Keeping one honest
/// 0-100 scale everywhere is what lets a report say "Pace 96", not "Pace 29".

static DB_CACHE: std::sync::OnceLock<Database> = std::sync::OnceLock::new();

impl Database {
    /// Load the 2026 players database from `data/players.csv`, or fall back to
    /// procedural generation. Cached process-wide: transfers clone players
    /// into the save, so the source pool never mutates.
    pub fn load() -> Self {
        DB_CACHE
            .get_or_init(|| {
                // Single source of truth: the 2026 players database (EA FC-style
                // dump). Its stats, position, age (from birthdate) and a derived
                // potential cap are the starting world; nothing else is merged.
                match Self::from_fc27("data/players.csv") {
                    Some(db) => {
                        eprintln!(
                            "  Loaded {} players / {} clubs from the 2026 players database.",
                            db.players.len(),
                            db.clubs.len()
                        );
                        db
                    }
                    None => {
                        eprintln!("  data/players.csv missing or unparsable — using procedural fallback.");
                        Self::procedural()
                    }
                }
            })
            .clone()
    }

    /// Normalise an FC-dump league label to the Big-5 names the sim builds its
    /// divisions from. Everything else keeps its own name (and simply isn't one
    /// of the five simulated leagues).
    fn norm_league(l: &str) -> String {
        match l.trim() {
            "LALIGA EA SPORTS" | "La Liga" => "La Liga",
            "Serie A Enilive" | "Serie A" => "Serie A",
            "Ligue 1 McDonald's" | "Ligue 1" => "Ligue 1",
            "Premier League" => "Premier League",
            "Bundesliga" => "Bundesliga",
            other => other,
        }
        .to_string()
    }

    /// Age at the 2026 snapshot from a "YYYY-MM-DD" birthdate. Falls back to 24
    /// when the field is missing or malformed.
    fn age_from_birthdate(birth: &str) -> u8 {
        let yr: i32 = birth.get(0..4).and_then(|s| s.parse().ok()).unwrap_or(0);
        if yr < 1950 || yr > 2026 {
            return 24;
        }
        (2026 - yr).clamp(15, 44) as u8
    }

    /// Load the 2026 players database (EA FC-style schema). Men's football only
    /// (the dump also carries women's players), overall ≥ 40, with every game
    /// attribute mapped from the detailed FC ratings, age from birthdate, and a
    /// potential cap derived from current ability plus a little youth headroom.
    /// Club reputations emerge from squad quality — the strongest squads are the
    /// biggest clubs — so no per-club constant is hardcoded.
    fn from_fc27(path: &str) -> Option<Self> {
        let content = fs::read_to_string(path).ok()?;
        let content = content.trim_start_matches('\u{feff}');
        let mut lines = content.lines();
        let headers: Vec<&str> = parse_csv_line(lines.next()?);
        let col = |name: &str| headers.iter().position(|h| h.eq_ignore_ascii_case(name));

        let i_common = col("common_name");
        let i_first = col("first_name");
        let i_last = col("last_name");
        let i_ovr = col("overall_rating")?;
        let i_pos = col("position")?;
        let i_alt = col("alternate_positions");
        let i_club = col("club")?;
        let i_league = col("league")?;
        let i_nat = col("nationality");
        let i_gender = col("gender");
        let i_birth = col("birthdate");
        let a = |n: &str| col(n);
        let (c_acc, c_spr) = (a("movement_acceleration"), a("movement_sprint_speed"));
        let (c_fin, c_pow, c_ls, c_vol) = (
            a("attacking_finishing"),
            a("power_shot_power"),
            a("power_long_shots"),
            a("attacking_volleys"),
        );
        let (c_sp, c_lp, c_vis, c_cro) = (
            a("attacking_short_passing"),
            a("skill_long_passing"),
            a("mentality_vision"),
            a("attacking_crossing"),
        );
        let (c_dri, c_con, c_agi, c_bal) = (
            a("skill_dribbling"),
            a("skill_ball_control"),
            a("movement_agility"),
            a("movement_balance"),
        );
        let (c_st, c_sl, c_int, c_mar) = (
            a("defending_standing_tackle"),
            a("defending_sliding_tackle"),
            a("mentality_interceptions"),
            a("defending_awareness"),
        );
        let c_head = a("attacking_heading_accuracy");
        let c_str = a("power_strength");
        let c_sta = a("power_stamina");
        let c_comp = a("mentality_composure");
        let (c_gkd, c_gkh, c_gkk, c_gkp, c_gkr) = (
            a("goalkeeping_diving"),
            a("goalkeeping_handling"),
            a("goalkeeping_kicking"),
            a("goalkeeping_positioning"),
            a("goalkeeping_reflexes"),
        );

        let get = |fields: &[String], idx: Option<usize>| -> String {
            idx.and_then(|i| fields.get(i)).cloned().unwrap_or_default()
        };
        let scaled = |fields: &[String], idx: Option<usize>| -> u8 {
            let v: f32 = get(fields, idx).trim().parse().unwrap_or(1.0);
            v.round().clamp(1.0, 99.0) as u8
        };
        let avg = |fields: &[String], idxs: &[Option<usize>]| -> u8 {
            let vals: Vec<f32> = idxs
                .iter()
                .filter_map(|i| get(fields, *i).trim().parse::<f32>().ok())
                .collect();
            if vals.is_empty() {
                return 1;
            }
            (vals.iter().sum::<f32>() / vals.len() as f32).round().clamp(1.0, 99.0) as u8
        };

        let mut players: Vec<Player> = Vec::new();
        // Club squads accumulate by lowercased name → (display name, league,
        // player indices). Reputation is derived afterwards from squad quality.
        let mut squads: HashMap<String, (String, String, Vec<usize>)> = HashMap::new();

        for line in lines {
            let fields: Vec<String> =
                parse_csv_line(line).iter().map(|s| s.to_string()).collect();
            if fields.len() < headers.len() / 2 {
                continue;
            }
            // Men's football only — the dump also lists women's players.
            if let Some(gi) = i_gender {
                if let Some(g) = fields.get(gi) {
                    if !g.trim().eq_ignore_ascii_case("Men's Football") {
                        continue;
                    }
                }
            }
            let ovr_raw: f32 =
                fields.get(i_ovr).and_then(|s| s.trim().parse().ok()).unwrap_or(0.0);
            if ovr_raw < 40.0 {
                continue; // filler reserve records
            }
            // Name: prefer the common name, else "first last".
            let common = get(&fields, i_common);
            let name = if common.trim().len() >= 2 {
                clean_name(&common)
            } else {
                clean_name(format!("{} {}", get(&fields, i_first), get(&fields, i_last)).trim())
            };
            if name.len() < 2 {
                continue;
            }
            // Primary position drives the role bucket; wingers (LW/RW/CF) count
            // as forwards through fc_position.
            let _ = i_alt;
            let pos = fc_position(&get(&fields, Some(i_pos)));
            let age = Self::age_from_birthdate(&get(&fields, i_birth));
            let attrs = Attributes {
                pace: avg(&fields, &[c_acc, c_spr]),
                shooting: avg(&fields, &[c_fin, c_pow, c_ls, c_vol]),
                passing: avg(&fields, &[c_sp, c_lp, c_vis, c_cro]),
                dribbling: avg(&fields, &[c_dri, c_con, c_agi, c_bal]),
                tackling: avg(&fields, &[c_st, c_sl, c_int, c_mar]),
                heading: scaled(&fields, c_head),
                strength: scaled(&fields, c_str),
                stamina: scaled(&fields, c_sta),
                vision: scaled(&fields, c_vis),
                composure: scaled(&fields, c_comp),
                goalkeeping: avg(&fields, &[c_gkd, c_gkh, c_gkk, c_gkp, c_gkr]),
                first_touch: scaled(&fields, c_con),
            };

            let mut player = Player {
                id: players.len() as PlayerId + 1,
                name,
                age,
                position: pos,
                nation: get(&fields, i_nat).split(',').next().unwrap_or("").trim().to_string(),
                attrs,
                potential: 0,
                contract: Contract { wage: 1000, years: 3 },
                morale: 75,
                fitness: 90,
                form: 0,
                value: 0,
                stats: SeasonStats::default(),
                club: None,
                condition: 100,
                sharpness: 70,
                injury_days: 0,
                yellows: 0,
                suspension: 0,
                happiness: 70,
                contract_expiry: 2027,
                release_clause: None,
                transfer_status: 0,
                scout_knowledge: 0,
                personality: Personality::Balanced,
            };
            let ovr = player.overall();
            // Potential cap: current ability plus a little youth headroom, hard-
            // capped at 95 (just above the dataset's 91 ceiling) so wonderkids
            // can grow into generational talents without strolling to 99.
            let youth = (23i32 - age as i32).clamp(0, 8) as u16;
            player.potential = (ovr as u16 + youth).min(95).max(ovr as u16) as u8;
            // Starting value/wage derive from ability (the sim re-prices everyone
            // from market_value on build, so this is only a seed).
            let v = (ovr as u64).pow(3);
            player.value = (v * 90).max(50_000);
            player.contract.wage = (v / 20).max(1000) as u32;

            let idx = players.len();
            let club_name = get(&fields, Some(i_club));
            players.push(player);

            if club_name.trim().len() < 2 || club_name.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let league = Self::norm_league(&get(&fields, Some(i_league)));
            squads
                .entry(club_name.to_lowercase())
                .or_insert_with(|| (club_name.clone(), league, Vec::new()))
                .2
                .push(idx);
        }

        // Build clubs: squad = best 32 by overall, reputation from the top-15
        // average overall (so the biggest clubs are the strongest squads).
        let mut clubs: Vec<Club> = Vec::new();
        for (_key, (cname, league, mut idxs)) in squads.into_iter() {
            idxs.sort_by(|&a, &b| players[b].overall().cmp(&players[a].overall()));
            idxs.truncate(32);
            if idxs.is_empty() {
                continue;
            }
            let topn = idxs.len().min(15);
            let avg_ovr: f32 =
                idxs[..topn].iter().map(|&i| players[i].overall() as f32).sum::<f32>() / topn as f32;
            let reputation = (2.24 * avg_ovr - 95.0).round().clamp(35.0, 99.0) as u8;
            let new_id = clubs.len() as ClubId + 1;
            let squad: Vec<PlayerId> = idxs.iter().map(|&i| players[i].id).collect();
            clubs.push(Club {
                id: new_id,
                name: cname.clone(),
                short: cname.chars().take(3).collect::<String>().to_uppercase(),
                squad,
                balance: 1_000_000 + (reputation as i64) * 200_000,
                reputation,
                stadium_capacity: 20_000 + (reputation as u32) * 400,
                tactic: Tactic::default(),
                is_human: false,
                morale: 75,
                transfer_budget: 0,
                wage_budget: 0,
                board_confidence: 60,
                fan_confidence: 60,
                board_expectation: 10,
                training_facilities: 3,
                youth_facilities: 3,
                ticket_price: 30,
                league,
                lineup: Vec::new(),
            });
        }
        let club_index = clubs.iter().enumerate().map(|(i, c)| (c.name.clone(), i)).collect();
        Some(Database { players, clubs, club_index })
    }

    fn procedural() -> Self {
        let mut rng = Pcg64Mcg::new(12345);
        let mut players = Vec::new();
        let mut clubs = Vec::new();

        let club_names = [
            "Manchester City", "Liverpool", "Arsenal", "Chelsea", "Tottenham Hotspur",
            "Manchester United", "Leicester City", "West Ham United", "Wolverhampton", "Crystal Palace",
            "Everton", "Aston Villa", "Newcastle United", "Brighton", "Southampton",
            "Leeds United", "Fulham", "Brentford", "Nottingham Forest", "Bournemouth",
        ];

        for (i, name) in club_names.iter().enumerate() {
            let strength = rng.gen_range(10..=30) as i32;
            let mut club = Club {
                id: i as ClubId + 1,
                name: name.to_string(),
                short: name.chars().take(3).collect::<String>().to_uppercase(),
                squad: Vec::new(),
                balance: rng.gen_range(-200_000..2_000_000),
                reputation: rng.gen_range(30..=95) as u8,
                stadium_capacity: rng.gen_range(20_000..60_000),
                tactic: Tactic::default(),
                is_human: false,
                morale: 75,
                transfer_budget: 0,
                wage_budget: 0,
                board_confidence: 60,
                fan_confidence: 60,
                board_expectation: 10,
                training_facilities: 3,
                youth_facilities: 3,
                ticket_price: 30,
                league: "Premier League".to_string(),
                lineup: Vec::new(),
            };
            let plan = [(Position::GK, 3), (Position::DEF, 8), (Position::MID, 8), (Position::FWD, 5)];
            let mut squad_ids = Vec::new();
            for (pos, count) in plan {
                for _ in 0..count {
                    let mut player = Gen::new(rng.gen()).player(pos, strength);
                    player.id = players.len() as PlayerId + 1;
                    player.club = Some(club.id);
                    player.recompute_value();
                    let pid = player.id;
                    players.push(player);
                    squad_ids.push(pid);
                }
            }
            club.squad = squad_ids;
            clubs.push(club);
        }

        let club_index = clubs.iter().enumerate().map(|(i, c)| (c.name.clone(), i)).collect();
        Database { players, clubs, club_index }
    }

    pub fn players(&self) -> &[Player] {
        &self.players
    }

    pub fn clubs(&self) -> &[Club] {
        &self.clubs
    }

    pub fn find_club(&self, name: &str) -> Option<&Club> {
        self.club_index.get(name).map(|&i| &self.clubs[i])
    }
}

// ── CSV Parsing ────────────────────────────────────────────────────────

/// Clean a raw CSV name. Some source rows carry the Latinised name immediately
/// followed, with no separator, by the native-script duplicate — e.g.
/// "Charalampos KostoulasΧαράλαμπος Κωστούλας" or a Cyrillic/CJK tail. Keep the
/// leading Latin portion (letters, accents, spaces, hyphens, apostrophes,
/// periods) and stop at the first letter from another script, which marks the
/// native-name tail. Plain Latin names (incl. accented) are returned unchanged.
pub(crate) fn clean_name(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        if c == ' ' || c == '-' || c == '\'' || c == '.' || c == ',' || c.is_ascii_alphanumeric()
        {
            out.push(c);
        } else if c.is_alphabetic() {
            // Latin and Latin-1/Extended accents sit below U+0250; anything at
            // or above (Greek U+0370, Cyrillic U+0400, Arabic, CJK…) is the
            // start of the native-script duplicate, so stop here.
            if (c as u32) < 0x0250 {
                out.push(c);
            } else {
                break;
            }
        }
        // Other symbols are dropped but scanning continues.
    }
    let cleaned = out.trim().trim_end_matches([',', '-']).trim().to_string();
    // A name that was entirely non-Latin leaves nothing — keep the original
    // rather than blanking the player out.
    if cleaned.is_empty() {
        raw.trim().to_string()
    } else {
        cleaned
    }
}

/// Split a CSV line honoring double-quoted fields (which may contain commas).
fn parse_csv_line(line: &str) -> Vec<&str> {
    // We return owned-free slices where possible, but quoted fields with
    // commas need reconstruction; for the FM dataset quoted fields are the
    // nationality/foot lists, which we don't index by column offset issues
    // because quotes wrap the whole field. Use a simple state machine.
    let mut result = Vec::new();
    let bytes = line.as_bytes();
    let mut start = 0usize;
    let mut in_quotes = false;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => in_quotes = !in_quotes,
            b',' if !in_quotes => {
                result.push(line[start..i].trim_matches('"').trim());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    result.push(line[start..].trim_matches('"').trim());
    result
}

fn fc_position(pos_str: &str) -> Position {
    // SoFIFA-style "RM, RW" — a player is a forward if ANY listed slot is an
    // attacking one (wingers who cut inside score like forwards), else fall
    // back to the primary slot. Reading only the first token misclassifies
    // wingers such as Yamal (RM, RW) as midfielders and guts their scoring.
    let tokens: Vec<String> = pos_str
        .split([',', ' '])
        .map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty())
        .collect();
    let is_fwd = |t: &str| {
        matches!(
            t,
            "ST" | "LW" | "RW" | "CF" | "LF" | "RF" | "LS" | "RS" | "S"
        )
    };
    if tokens.iter().any(|t| is_fwd(t)) {
        return Position::FWD;
    }
    match tokens.first().map(|s| s.as_str()).unwrap_or("") {
        "GK" => Position::GK,
        "LB" | "RB" | "CB" | "LCB" | "RCB" | "LWB" | "RWB" => Position::DEF,
        _ => Position::MID,
    }
}

#[cfg(test)]
mod tests {
    use super::clean_name;

    #[test]
    fn clean_name_strips_native_script_tail() {
        // The Latinised name is immediately followed by its native-script
        // duplicate with no separator — keep only the Latin portion.
        assert_eq!(
            clean_name("Charalampos KostoulasΧαράλαμπος Κωστούλας"),
            "Charalampos Kostoulas"
        );
        assert_eq!(clean_name("Andrey Santosандрей"), "Andrey Santos");
    }

    #[test]
    fn clean_name_keeps_latin_accents_and_punctuation() {
        // Accented Latin, hyphens and apostrophes are all legitimate.
        assert_eq!(clean_name("Kylian Mbappé"), "Kylian Mbappé");
        assert_eq!(clean_name("Pierre-Emerick Aubameyang"), "Pierre-Emerick Aubameyang");
        assert_eq!(clean_name("N'Golo Kanté"), "N'Golo Kanté");
        assert_eq!(clean_name("Erling Braut Håland"), "Erling Braut Håland");
    }

    #[test]
    fn clean_name_falls_back_when_all_native() {
        // A name with no Latin letters must not blank the player out.
        assert_eq!(clean_name("Χαράλαμπος"), "Χαράλαμπος");
    }
}

