use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CareerRow {
    pub player_id: usize,
    pub name: String,
    pub nation: String,
    pub position: String, // "GK"/"DEF"/"MID"/"FWD"
    pub age: u8,
    pub season_year: u16,
    pub club: String,
    // Attributes snapshot
    pub ovr: u8,
    pub pot: u8,
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
    // Agent state
    pub morale: u8,
    pub fatigue: u8,
    pub injury_days: u16,
    pub value: i64,
    pub wage: i64,
    // Season totals (all comps combined)
    pub apps: u16,
    pub minutes: u32,
    pub goals: u16,
    pub assists: u16,
    pub avg_rating: f32,
    // Per-competition breakdowns
    pub league_apps: u16,
    pub league_goals: u16,
    pub league_assists: u16,
    pub ucl_apps: u16,
    pub ucl_goals: u16,
    pub ucl_assists: u16,
    pub el_apps: u16,
    pub el_goals: u16,
    pub el_assists: u16,
    pub cup_apps: u16,
    pub cup_goals: u16,
    pub cup_assists: u16,
    pub intl_apps: u16,
    pub intl_goals: u16,
    pub intl_assists: u16,
    // Detailed stats
    pub shots: u16,
    pub sot: u16,
    pub xg: f32,
    pub xa: f32,
    pub key_passes: u16,
    pub big_chances: u16,
    pub passes_att: u32,
    pub passes_cmp: u32,
    pub dribbles_att: u16,
    pub dribbles_cmp: u16,
    pub tackles: u16,
    pub interceptions: u16,
    pub fouls: u16,
    pub saves: u16,
    pub clean_sheets: u16,
    pub aerials_won: u16,
    pub aerials_lost: u16,
    // Honours
    pub league_won: bool,
    pub ucl_result: u8,
    pub el_result: u8,
    pub intl_result: u8,
    pub ballon_dor: bool,
}

#[derive(Clone, Debug, Default)]
pub struct CareerDb {
    pub rows: Vec<CareerRow>,
}

impl CareerDb {
    pub fn new() -> Self {
        CareerDb { rows: Vec::new() }
    }

    pub fn push_season(&mut self, rows: Vec<CareerRow>) {
        self.rows.extend(rows);
    }
}

/// Write career_db.json and career_db.sql to the working directory.
/// `csv_path` is the path to data/players.csv for the raw attribute table.
pub fn write_career_db(db: &CareerDb, csv_path: &str) {
    // 1. JSON output
    if let Ok(json) = serde_json::to_string_pretty(&db.rows) {
        let _ = std::fs::write("career_db.json", json);
    }

    // 2. SQL output
    let mut sql = String::new();

    // Raw CSV table
    if let Ok(csv_content) = std::fs::read_to_string(csv_path) {
        let mut csv_lines = csv_content.lines();
        if let Some(header_line) = csv_lines.next() {
            let headers: Vec<&str> = header_line.split(',').collect();
            sql.push_str("CREATE TABLE IF NOT EXISTS csv_attributes (\n");
            for (i, h) in headers.iter().enumerate() {
                let col = h.trim();
                let comma = if i + 1 < headers.len() { "," } else { "" };
                if col == "player_id" {
                    sql.push_str(&format!("    {} INTEGER PRIMARY KEY{}\n", col, comma));
                } else {
                    sql.push_str(&format!("    {} TEXT{}\n", sanitize_col(col), comma));
                }
            }
            sql.push_str(");\n\n");

            for line in csv_lines {
                let fields: Vec<&str> = line.split(',').collect();
                if fields.len() < headers.len() {
                    continue;
                }
                let values: Vec<String> = fields
                    .iter()
                    .map(|f| format!("'{}'", sql_escape(f.trim())))
                    .collect();
                sql.push_str(&format!(
                    "INSERT INTO csv_attributes VALUES ({});\n",
                    values.join(", ")
                ));
            }
            sql.push('\n');
        }
    }

    // Career seasons table
    sql.push_str("CREATE TABLE IF NOT EXISTS career_seasons (\n");
    sql.push_str("    player_id INTEGER,\n");
    sql.push_str("    season_year INTEGER,\n");
    sql.push_str("    name TEXT,\n");
    sql.push_str("    nation TEXT,\n");
    sql.push_str("    position TEXT,\n");
    sql.push_str("    age INTEGER,\n");
    sql.push_str("    club TEXT,\n");
    sql.push_str("    ovr INTEGER,\n");
    sql.push_str("    pot INTEGER,\n");
    sql.push_str("    pace INTEGER,\n");
    sql.push_str("    shooting INTEGER,\n");
    sql.push_str("    passing INTEGER,\n");
    sql.push_str("    dribbling INTEGER,\n");
    sql.push_str("    tackling INTEGER,\n");
    sql.push_str("    heading INTEGER,\n");
    sql.push_str("    strength INTEGER,\n");
    sql.push_str("    stamina INTEGER,\n");
    sql.push_str("    vision INTEGER,\n");
    sql.push_str("    composure INTEGER,\n");
    sql.push_str("    goalkeeping INTEGER,\n");
    sql.push_str("    first_touch INTEGER,\n");
    sql.push_str("    morale INTEGER,\n");
    sql.push_str("    fatigue INTEGER,\n");
    sql.push_str("    injury_days INTEGER,\n");
    sql.push_str("    value INTEGER,\n");
    sql.push_str("    wage INTEGER,\n");
    sql.push_str("    apps INTEGER,\n");
    sql.push_str("    minutes INTEGER,\n");
    sql.push_str("    goals INTEGER,\n");
    sql.push_str("    assists INTEGER,\n");
    sql.push_str("    avg_rating REAL,\n");
    sql.push_str("    league_apps INTEGER,\n");
    sql.push_str("    league_goals INTEGER,\n");
    sql.push_str("    league_assists INTEGER,\n");
    sql.push_str("    ucl_apps INTEGER,\n");
    sql.push_str("    ucl_goals INTEGER,\n");
    sql.push_str("    ucl_assists INTEGER,\n");
    sql.push_str("    el_apps INTEGER,\n");
    sql.push_str("    el_goals INTEGER,\n");
    sql.push_str("    el_assists INTEGER,\n");
    sql.push_str("    cup_apps INTEGER,\n");
    sql.push_str("    cup_goals INTEGER,\n");
    sql.push_str("    cup_assists INTEGER,\n");
    sql.push_str("    intl_apps INTEGER,\n");
    sql.push_str("    intl_goals INTEGER,\n");
    sql.push_str("    intl_assists INTEGER,\n");
    sql.push_str("    shots INTEGER,\n");
    sql.push_str("    sot INTEGER,\n");
    sql.push_str("    xg REAL,\n");
    sql.push_str("    xa REAL,\n");
    sql.push_str("    key_passes INTEGER,\n");
    sql.push_str("    big_chances INTEGER,\n");
    sql.push_str("    passes_att INTEGER,\n");
    sql.push_str("    passes_cmp INTEGER,\n");
    sql.push_str("    dribbles_att INTEGER,\n");
    sql.push_str("    dribbles_cmp INTEGER,\n");
    sql.push_str("    tackles INTEGER,\n");
    sql.push_str("    interceptions INTEGER,\n");
    sql.push_str("    fouls INTEGER,\n");
    sql.push_str("    saves INTEGER,\n");
    sql.push_str("    clean_sheets INTEGER,\n");
    sql.push_str("    aerials_won INTEGER,\n");
    sql.push_str("    aerials_lost INTEGER,\n");
    sql.push_str("    league_won INTEGER,\n");
    sql.push_str("    ucl_result INTEGER,\n");
    sql.push_str("    el_result INTEGER,\n");
    sql.push_str("    intl_result INTEGER,\n");
    sql.push_str("    ballon_dor INTEGER,\n");
    sql.push_str("    PRIMARY KEY (player_id, season_year)\n");
    sql.push_str(");\n\n");

    for r in &db.rows {
        sql.push_str(&format!(
            "INSERT INTO career_seasons VALUES ({}, {}, '{}', '{}', '{}', {}, '{}', {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {:.2}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {:.2}, {:.2}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {});\n",
            r.player_id,
            r.season_year,
            sql_escape(&r.name),
            sql_escape(&r.nation),
            sql_escape(&r.position),
            r.age,
            sql_escape(&r.club),
            r.ovr,
            r.pot,
            r.pace,
            r.shooting,
            r.passing,
            r.dribbling,
            r.tackling,
            r.heading,
            r.strength,
            r.stamina,
            r.vision,
            r.composure,
            r.goalkeeping,
            r.first_touch,
            r.morale,
            r.fatigue,
            r.injury_days,
            r.value,
            r.wage,
            r.apps,
            r.minutes,
            r.goals,
            r.assists,
            r.avg_rating,
            r.league_apps,
            r.league_goals,
            r.league_assists,
            r.ucl_apps,
            r.ucl_goals,
            r.ucl_assists,
            r.el_apps,
            r.el_goals,
            r.el_assists,
            r.cup_apps,
            r.cup_goals,
            r.cup_assists,
            r.intl_apps,
            r.intl_goals,
            r.intl_assists,
            r.shots,
            r.sot,
            r.xg,
            r.xa,
            r.key_passes,
            r.big_chances,
            r.passes_att,
            r.passes_cmp,
            r.dribbles_att,
            r.dribbles_cmp,
            r.tackles,
            r.interceptions,
            r.fouls,
            r.saves,
            r.clean_sheets,
            r.aerials_won,
            r.aerials_lost,
            r.league_won as u8,
            r.ucl_result,
            r.el_result,
            r.intl_result,
            r.ballon_dor as u8,
        ));
    }

    let _ = std::fs::write("career_db.sql", sql);
}

fn sql_escape(s: &str) -> String {
    s.replace('\'', "''")
}

fn sanitize_col(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}
