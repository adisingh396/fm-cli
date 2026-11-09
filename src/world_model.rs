//! World / league / nation registry.
//!
//! DATA + REGISTRY ONLY. This module exposes the curated league catalogue,
//! the nation registry, and a loader that groups the existing `Database`'s
//! players/clubs by league. It does not mutate the DB or touch the season
//! engine.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::data::Database;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confederation {
    UEFA,
    CONMEBOL,
    CONCACAF,
    AFC,
    CAF,
    OFC,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LeagueDef {
    pub id: u16,
    pub name: String,
    pub nation: String,
    pub confed: Confederation,
    pub tier: u8,
    pub coeff: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NationDef {
    pub name: String,
    pub confed: Confederation,
    pub ranking: u8,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorldLeagues {
    pub leagues: Vec<LeagueDef>,
    pub by_nation: BTreeMap<String, Vec<u16>>,
    /// club name -> player indices (into Database::players()) belonging to it.
    pub club_index: HashMap<String, Vec<usize>>,
    /// league id -> club names that play in it.
    pub league_clubs: BTreeMap<u16, Vec<String>>,
    /// club name -> league id.
    pub club_league: HashMap<String, u16>,
}

impl WorldLeagues {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Normalise a raw CSV `league` string to the curated display name.
pub fn normalize_league(csv_league: &str) -> Option<&'static str> {
    match csv_league.trim() {
        "1A Pro League" => Some("Belgian Pro League"),
        "3. Liga" => Some("3. Liga"),
        "Allsvenskan" => Some("Allsvenskan"),
        "Brack Super League" => Some("Swiss Super League"),
        "Bundesliga" => Some("Bundesliga"),
        "Bundesliga 2" => Some("Bundesliga 2"),
        "CSL" => Some("Chinese Super League"),
        // The 2026 dump files the non-Argentine South American clubs under the
        // continent's flagship competition label rather than a domestic league;
        // it loads as a CONMEBOL top-flight group so the Libertadores field
        // has real clubs in it (Peñarol, Cerro Porteño, Sporting Cristal…).
        "CONMEBOL Libertadores" => Some("CONMEBOL Libertadores"),
        "EFL Championship" => Some("EFL Championship"),
        "EFL League One" => Some("EFL League One"),
        "EFL League Two" => Some("EFL League Two"),
        "Ekstraklasa" => Some("Ekstraklasa"),
        "Eliteserien" => Some("Eliteserien"),
        "Eredivisie" => Some("Eredivisie"),
        "Finnliiga" => Some("Veikkausliiga"),
        "Hellas Liga" => Some("Greek Super League"),
        "ISL" => Some("Indian Super League"),
        "Isuzu UTE A League" => Some("A-League"),
        "K League 1" => Some("K League 1"),
        "LALIGA EA SPORTS" => Some("La Liga"),
        "LALIGA HYPERMOTION" => Some("Segunda División"),
        "LPF" => Some("Liga Profesional"),
        "Liga Azerbaijan" => Some("Azerbaijan Premier League"),
        "Liga BBVA MX" => Some("Liga MX"),
        "Liga Bulgaria" => Some("Bulgarian First League"),
        "Liga Colombia" => Some("Liga BetPlay"),
        "Liga Cyprus" => Some("Cypriot First Division"),
        "Liga Hrvatska" => Some("Croatian HNL"),
        "Liga Portugal" => Some("Liga Portugal"),
        "Liga do Brasil" => Some("Brasileirão"),
        "Ligue 1 McDonald's" => Some("Ligue 1"),
        "Ligue 2 BKT" => Some("Ligue 2"),
        "MLS" => Some("MLS"),
        "Magyar Liga" => Some("NB I"),
        "Metropolitan Division" => Some("US lower division"),
        "Premier League" => Some("Premier League"),
        "ROSHN Saudi League" => Some("Saudi Pro League"),
        "SSE Airtricity Men's Premier Division" => Some("League of Ireland Premier Division"),
        "SUPERLIGA" => Some("Danish Superliga"),
        "Scottish Premiership" => Some("Scottish Premiership"),
        "Serie A Enilive" => Some("Serie A"),
        "Serie BKT" => Some("Serie B"),
        "Thailand League" => Some("Thai League 1"),
        "Trendyol Süper Lig" => Some("Süper Lig"),
        "Ukrayina Liha" => Some("Ukrainian Premier League"),
        "United Emirates League" => Some("UAE Pro League"),
        "Ö. Bundesliga" => Some("Austrian Bundesliga"),
        "Česká Liga" => Some("Czech First League"),
        // already-clean pass-throughs
        "Belgian Pro League" => Some("Belgian Pro League"),
        "3. Liga" => Some("3. Liga"),
        "Swiss Super League" => Some("Swiss Super League"),
        "Chinese Super League" => Some("Chinese Super League"),
        "Greek Super League" => Some("Greek Super League"),
        "Indian Super League" => Some("Indian Super League"),
        "A-League" => Some("A-League"),
        "K League 1" => Some("K League 1"),
        "La Liga" => Some("La Liga"),
        "Segunda División" => Some("Segunda División"),
        "Liga Profesional" => Some("Liga Profesional"),
        "CONMEBOL Libertadores" => Some("CONMEBOL Libertadores"),
        "Liga MX" => Some("Liga MX"),
        "Liga BetPlay" => Some("Liga BetPlay"),
        "Cypriot First Division" => Some("Cypriot First Division"),
        "Croatian HNL" => Some("Croatian HNL"),
        "Brasileirão" => Some("Brasileirão"),
        "NB I" => Some("NB I"),
        "US lower division" => Some("US lower division"),
        "Saudi Pro League" => Some("Saudi Pro League"),
        "League of Ireland Premier Division" => Some("League of Ireland Premier Division"),
        "Danish Superliga" => Some("Danish Superliga"),
        "Serie A" => Some("Serie A"),
        "Serie B" => Some("Serie B"),
        "Thai League 1" => Some("Thai League 1"),
        "Süper Lig" => Some("Süper Lig"),
        "Ukrainian Premier League" => Some("Ukrainian Premier League"),
        "UAE Pro League" => Some("UAE Pro League"),
        "Austrian Bundesliga" => Some("Austrian Bundesliga"),
        "Czech First League" => Some("Czech First League"),
        "Veikkausliiga" => Some("Veikkausliiga"),
        "Azerbaijan Premier League" => Some("Azerbaijan Premier League"),
        "Bulgarian First League" => Some("Bulgarian First League"),
        "Allsvenskan" => Some("Allsvenskan"),
        _ => None,
    }
}

static LEAGUE_REGISTRY: OnceLock<Vec<LeagueDef>> = OnceLock::new();

fn build_catalogue() -> Vec<LeagueDef> {
    // (raw-ish key for nation lookup handled separately; here: display, nation, confed, tier, coeff)
    let rows: &[(&str, &str, Confederation, u8, f32)] = &[
        (
            "Belgian Pro League",
            "Belgium",
            Confederation::UEFA,
            1,
            0.55,
        ),
        ("3. Liga", "Germany", Confederation::UEFA, 3, 0.90),
        ("Allsvenskan", "Sweden", Confederation::UEFA, 1, 0.30),
        (
            "Swiss Super League",
            "Switzerland",
            Confederation::UEFA,
            1,
            0.36,
        ),
        ("Bundesliga", "Germany", Confederation::UEFA, 1, 0.90),
        ("Bundesliga 2", "Germany", Confederation::UEFA, 2, 0.90),
        (
            "Chinese Super League",
            "China PR",
            Confederation::AFC,
            1,
            0.0,
        ),
        ("EFL Championship", "England", Confederation::UEFA, 2, 0.98),
        ("EFL League One", "England", Confederation::UEFA, 3, 0.98),
        ("EFL League Two", "England", Confederation::UEFA, 4, 0.98),
        ("Ekstraklasa", "Poland", Confederation::UEFA, 1, 0.34),
        ("Eliteserien", "Norway", Confederation::UEFA, 1, 0.30),
        ("Eredivisie", "Netherlands", Confederation::UEFA, 1, 0.66),
        ("Veikkausliiga", "Finland", Confederation::UEFA, 1, 0.28),
        ("Greek Super League", "Greece", Confederation::UEFA, 1, 0.33),
        ("Indian Super League", "India", Confederation::AFC, 1, 0.0),
        ("A-League", "Australia", Confederation::AFC, 1, 0.0),
        ("K League 1", "Korea Republic", Confederation::AFC, 1, 0.0),
        ("La Liga", "Spain", Confederation::UEFA, 1, 1.00),
        ("Segunda División", "Spain", Confederation::UEFA, 2, 1.00),
        (
            "Liga Profesional",
            "Argentina",
            Confederation::CONMEBOL,
            1,
            0.0,
        ),
        (
            "CONMEBOL Libertadores",
            "South America",
            Confederation::CONMEBOL,
            1,
            0.0,
        ),
        (
            "Azerbaijan Premier League",
            "Azerbaijan",
            Confederation::UEFA,
            1,
            0.25,
        ),
        ("Liga MX", "Mexico", Confederation::CONCACAF, 1, 0.0),
        (
            "Bulgarian First League",
            "Bulgaria",
            Confederation::UEFA,
            1,
            0.28,
        ),
        ("Liga BetPlay", "Colombia", Confederation::CONMEBOL, 1, 0.0),
        (
            "Cypriot First Division",
            "Cyprus",
            Confederation::UEFA,
            1,
            0.32,
        ),
        ("Croatian HNL", "Croatia", Confederation::UEFA, 1, 0.38),
        ("Liga Portugal", "Portugal", Confederation::UEFA, 1, 0.62),
        ("Brasileirão", "Brazil", Confederation::CONMEBOL, 1, 0.0),
        ("Ligue 1", "France", Confederation::UEFA, 1, 0.86),
        ("Ligue 2", "France", Confederation::UEFA, 2, 0.86),
        ("MLS", "United States", Confederation::CONCACAF, 1, 0.0),
        ("NB I", "Hungary", Confederation::UEFA, 1, 0.30),
        (
            "US lower division",
            "United States",
            Confederation::CONCACAF,
            2,
            0.0,
        ),
        ("Premier League", "England", Confederation::UEFA, 1, 0.98),
        (
            "Saudi Pro League",
            "Saudi Arabia",
            Confederation::AFC,
            1,
            0.0,
        ),
        (
            "League of Ireland Premier Division",
            "Republic of Ireland",
            Confederation::UEFA,
            1,
            0.18,
        ),
        ("Danish Superliga", "Denmark", Confederation::UEFA, 1, 0.30),
        (
            "Scottish Premiership",
            "Scotland",
            Confederation::UEFA,
            1,
            0.42,
        ),
        ("Serie A", "Italy", Confederation::UEFA, 1, 0.92),
        ("Serie B", "Italy", Confederation::UEFA, 2, 0.92),
        ("Thai League 1", "Thailand", Confederation::AFC, 1, 0.0),
        ("Süper Lig", "Turkey", Confederation::UEFA, 1, 0.40),
        (
            "Ukrainian Premier League",
            "Ukraine",
            Confederation::UEFA,
            1,
            0.32,
        ),
        (
            "UAE Pro League",
            "United Arab Emirates",
            Confederation::AFC,
            1,
            0.0,
        ),
        (
            "Austrian Bundesliga",
            "Austria",
            Confederation::UEFA,
            1,
            0.38,
        ),
        (
            "Czech First League",
            "Czech Republic",
            Confederation::UEFA,
            1,
            0.32,
        ),
        // Regen leagues: first-tier competitions the 2026 dump does not
        // carry clubs/players for (the database has one Qatari, six Iranian,
        // six Uzbek men — leagues those stars should be playing in). Their
        // clubs and squads are procedurally built in `build_regen_leagues`
        // (season.rs) so the entry is metadata + pipeline, not dead text.
        ("J-League", "Japan", Confederation::AFC, 1, 0.0),
        ("Iran Pro League", "Iran", Confederation::AFC, 1, 0.0),
        ("Qatar Stars League", "Qatar", Confederation::AFC, 1, 0.0),
        (
            "Uzbekistan Super League",
            "Uzbekistan",
            Confederation::AFC,
            1,
            0.0,
        ),
        ("Jordan Pro League", "Jordan", Confederation::AFC, 1, 0.0),
        ("Indonesia Liga 1", "Indonesia", Confederation::AFC, 1, 0.0),
        ("Iraq Stars League", "Iraq", Confederation::AFC, 1, 0.0),
        (
            "UAE Pro League",
            "United Arab Emirates",
            Confederation::AFC,
            1,
            0.0,
        ),
        ("Oman Pro League", "Oman", Confederation::AFC, 1, 0.0),
        (
            "Philippines Football League",
            "Philippines",
            Confederation::AFC,
            1,
            0.0,
        ),
        (
            "Brazilian Serie A",
            "Brazil",
            Confederation::CONMEBOL,
            1,
            0.0,
        ),
        (
            "Chilean Primera Division",
            "Chile",
            Confederation::CONMEBOL,
            1,
            0.0,
        ),
        (
            "Uruguayan Primera Division",
            "Uruguay",
            Confederation::CONMEBOL,
            1,
            0.0,
        ),
        (
            "Peruvian Primera Division",
            "Peru",
            Confederation::CONMEBOL,
            1,
            0.0,
        ),
        (
            "Paraguayan Division de Honor",
            "Paraguay",
            Confederation::CONMEBOL,
            1,
            0.0,
        ),
        (
            "Bolivian Primera Division",
            "Bolivia",
            Confederation::CONMEBOL,
            1,
            0.0,
        ),
        (
            "Ecuadorian Serie A",
            "Ecuador",
            Confederation::CONMEBOL,
            1,
            0.0,
        ),
        (
            "Venezuelan Primera Division",
            "Venezuela",
            Confederation::CONMEBOL,
            1,
            0.0,
        ),
        ("Serbian SuperLiga", "Serbia", Confederation::UEFA, 1, 0.0),
        ("Romanian Liga I", "Romania", Confederation::UEFA, 1, 0.0),
        (
            "Bulgarian First League",
            "Bulgaria",
            Confederation::UEFA,
            1,
            0.0,
        ),
        (
            "Bosnian Premier League",
            "Bosnia and Herzegovina",
            Confederation::UEFA,
            1,
            0.0,
        ),
        ("Slovak Superliga", "Slovakia", Confederation::UEFA, 1, 0.0),
        (
            "Slovenian PrvaLiga",
            "Slovenia",
            Confederation::UEFA,
            1,
            0.0,
        ),
        (
            "Icelandic Besta deild",
            "Iceland",
            Confederation::UEFA,
            1,
            0.0,
        ),
        (
            "Israeli Premier League",
            "Israel",
            Confederation::UEFA,
            1,
            0.0,
        ),
        (
            "Georgian Erovnuli Liga",
            "Georgia",
            Confederation::UEFA,
            1,
            0.0,
        ),
        (
            "Albanian Kategoria Superiore",
            "Albania",
            Confederation::UEFA,
            1,
            0.0,
        ),
        (
            "Montenegrin First League",
            "Montenegro",
            Confederation::UEFA,
            1,
            0.0,
        ),
        (
            "North Macedonian First League",
            "North Macedonia",
            Confederation::UEFA,
            1,
            0.0,
        ),
        (
            "Armenian Premier League",
            "Armenia",
            Confederation::UEFA,
            1,
            0.0,
        ),
        (
            "Azerbaijani Premier League",
            "Azerbaijan",
            Confederation::UEFA,
            1,
            0.0,
        ),
        (
            "Belarusian Premier League",
            "Belarus",
            Confederation::UEFA,
            1,
            0.0,
        ),
        (
            "Kazakh Premier League",
            "Kazakhstan",
            Confederation::UEFA,
            1,
            0.0,
        ),
        (
            "Estonian Meistriliiga",
            "Estonia",
            Confederation::UEFA,
            1,
            0.0,
        ),
        ("Latvian Virsliga", "Latvia", Confederation::UEFA, 1, 0.0),
        (
            "Lithuanian A Lyga",
            "Lithuania",
            Confederation::UEFA,
            1,
            0.0,
        ),
        ("Welsh Cymru Premier", "Wales", Confederation::UEFA, 1, 0.0),
        (
            "Northern Irish Premiership",
            "Northern Ireland",
            Confederation::UEFA,
            1,
            0.0,
        ),
        (
            "Maltese Premier League",
            "Malta",
            Confederation::UEFA,
            1,
            0.0,
        ),
        (
            "Faroe Islands Betri Deildin",
            "Faroe Islands",
            Confederation::UEFA,
            1,
            0.0,
        ),
        (
            "Luxembourg National Division",
            "Luxembourg",
            Confederation::UEFA,
            1,
            0.0,
        ),
        (
            "Moldovan Super Liga",
            "Moldova",
            Confederation::UEFA,
            1,
            0.0,
        ),
        ("Kosovo Superliga", "Kosovo", Confederation::UEFA, 1, 0.0),
        (
            "Egyptian Premier League",
            "Egypt",
            Confederation::CAF,
            1,
            0.0,
        ),
        (
            "South African Premiership",
            "South Africa",
            Confederation::CAF,
            1,
            0.0,
        ),
        ("Moroccan Botola", "Morocco", Confederation::CAF, 1, 0.0),
        ("Algerian Ligue 1", "Algeria", Confederation::CAF, 1, 0.0),
        ("Tunisian Ligue 1", "Tunisia", Confederation::CAF, 1, 0.0),
        (
            "Nigerian Professional Football League",
            "Nigeria",
            Confederation::CAF,
            1,
            0.0,
        ),
        ("Ghana Premier League", "Ghana", Confederation::CAF, 1, 0.0),
        ("Angolan Girabola", "Angola", Confederation::CAF, 1, 0.0),
        (
            "Ivorian Ligue 1",
            "Côte d'Ivoire",
            Confederation::CAF,
            1,
            0.0,
        ),
        (
            "Tanzanian Mainland Premier League",
            "Tanzania",
            Confederation::CAF,
            1,
            0.0,
        ),
        ("Zambian Super League", "Zambia", Confederation::CAF, 1, 0.0),
        (
            "Zimbabwean Premier Soccer League",
            "Zimbabwe",
            Confederation::CAF,
            1,
            0.0,
        ),
        (
            "Kenyan FKF Premier League",
            "Kenya",
            Confederation::CAF,
            1,
            0.0,
        ),
        (
            "Burkinabe Premier League",
            "Burkina Faso",
            Confederation::CAF,
            1,
            0.0,
        ),
        ("Gabonese Ligue 1", "Gabon", Confederation::CAF, 1, 0.0),
        (
            "Costa Rican Primera Division",
            "Costa Rica",
            Confederation::CONCACAF,
            1,
            0.0,
        ),
        (
            "Guatemalan Liga Nacional",
            "Guatemala",
            Confederation::CONCACAF,
            1,
            0.0,
        ),
        (
            "Honduran Liga Nacional",
            "Honduras",
            Confederation::CONCACAF,
            1,
            0.0,
        ),
        (
            "Panamanian Liga Panamena de Futbol",
            "Panama",
            Confederation::CONCACAF,
            1,
            0.0,
        ),
        (
            "Salvadoran Primera Division",
            "El Salvador",
            Confederation::CONCACAF,
            1,
            0.0,
        ),
        (
            "Jamaican Premier League",
            "Jamaica",
            Confederation::CONCACAF,
            1,
            0.0,
        ),
        (
            "New Caledonia Super Ligue",
            "New Caledonia",
            Confederation::OFC,
            1,
            0.0,
        ),
    ];
    let mut out = Vec::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        out.push(LeagueDef {
            id: (i as u16) + 1,
            name: r.0.to_string(),
            nation: r.1.to_string(),
            confed: r.2,
            tier: r.3,
            coeff: r.4,
        });
    }
    out
}

fn registry() -> &'static [LeagueDef] {
    LEAGUE_REGISTRY.get_or_init(build_catalogue)
}

pub fn catalogue() -> Vec<LeagueDef> {
    registry().to_vec()
}

pub fn lookup(csv_league: &str) -> Option<&'static LeagueDef> {
    let clean = normalize_league(csv_league)?;
    registry().iter().find(|l| l.name == clean)
}

/// Continental cups (not leagues) present in the CSV — kept as a note only.
pub const CONTINENTAL_CUPS: &[&str] = &["CONMEBOL Libertadores", "CONMEBOL Sudamericana"];

pub fn confed_of(nation: &str) -> Confederation {
    match nation {
        // UEFA
        "Albania"
        | "Andorra"
        | "Armenia"
        | "Austria"
        | "Azerbaijan"
        | "Belarus"
        | "Belgium"
        | "Bosnia and Herzegovina"
        | "Bulgaria"
        | "Croatia"
        | "Cyprus"
        | "Czech Republic"
        | "Denmark"
        | "England"
        | "Estonia"
        | "Faroe Islands"
        | "Finland"
        | "France"
        | "Georgia"
        | "Germany"
        | "Gibraltar"
        | "Greece"
        | "Holland"
        | "Hungary"
        | "Iceland"
        | "Israel"
        | "Italy"
        | "Kazakhstan"
        | "Kosovo"
        | "Latvia"
        | "Liechtenstein"
        | "Lithuania"
        | "Luxembourg"
        | "Malta"
        | "Moldova"
        | "Montenegro"
        | "North Macedonia"
        | "Netherlands"
        | "Northern Ireland"
        | "Norway"
        | "Poland"
        | "Portugal"
        | "Republic of Ireland"
        | "Romania"
        | "Russia"
        | "Scotland"
        | "Serbia"
        | "Slovakia"
        | "Slovenia"
        | "Spain"
        | "Sweden"
        | "Switzerland"
        | "Turkey"
        | "Ukraine"
        | "Wales" => Confederation::UEFA,
        // CONMEBOL
        "Argentina" | "Bolivia" | "Brazil" | "Chile" | "Colombia" | "Ecuador" | "Paraguay"
        | "Peru" | "Uruguay" | "Venezuela" => Confederation::CONMEBOL,
        // CONCACAF
        "Antigua and Barbuda"
        | "Barbados"
        | "Bermuda"
        | "Canada"
        | "Costa Rica"
        | "Cuba"
        | "Curaçao"
        | "Dominican Republic"
        | "El Salvador"
        | "Grenada"
        | "Guatemala"
        | "Haiti"
        | "Honduras"
        | "Jamaica"
        | "Mexico"
        | "Montserrat"
        | "Panama"
        | "Puerto Rico"
        | "St. Kitts and Nevis"
        | "St. Lucia"
        | "Suriname"
        | "Trinidad and Tobago"
        | "United States" => Confederation::CONCACAF,
        // AFC
        "Afghanistan"
        | "Australia"
        | "Bangladesh"
        | "China PR"
        | "Chinese Taipei"
        | "Hong Kong"
        | "India"
        | "Indonesia"
        | "Iran"
        | "Iraq"
        | "Japan"
        | "Jordan"
        | "Korea DPR"
        | "Korea Republic"
        | "Lebanon"
        | "Oman"
        | "Pakistan"
        | "Palestine"
        | "Philippines"
        | "Qatar"
        | "Saudi Arabia"
        | "Sri Lanka"
        | "Syria"
        | "Thailand"
        | "United Arab Emirates"
        | "Uzbekistan"
        | "Yemen" => Confederation::AFC,
        // OFC
        "New Caledonia" | "New Zealand" | "Vanuatu" => Confederation::OFC,
        // CAF (default for the rest of Africa)
        _ => Confederation::CAF,
    }
}

pub fn nations() -> Vec<NationDef> {
    // curated top ~60 rankings (approx FIFA). Everyone else defaults to 150+.
    let curated: &[(&str, u8)] = &[
        ("Argentina", 1),
        ("France", 2),
        ("Spain", 3),
        ("England", 4),
        ("Brazil", 5),
        ("Belgium", 6),
        ("Holland", 7),
        ("Portugal", 8),
        ("Italy", 9),
        ("Croatia", 10),
        ("Morocco", 11),
        ("Switzerland", 12),
        ("Germany", 13),
        ("Japan", 14),
        ("Senegal", 15),
        ("United States", 16),
        ("Iran", 17),
        ("Denmark", 18),
        ("Mexico", 19),
        ("Netherlands", 20),
        ("Uruguay", 21),
        ("Colombia", 22),
        ("Wales", 23),
        ("Australia", 24),
        ("Scotland", 25),
        ("Ukraine", 26),
        ("Poland", 27),
        ("Korea Republic", 28),
        ("Serbia", 29),
        ("Tunisia", 30),
        ("Ecuador", 31),
        ("Hungary", 32),
        ("Turkey", 33),
        ("Austria", 34),
        ("Czech Republic", 35),
        ("Costa Rica", 36),
        ("Nigeria", 37),
        ("Egypt", 38),
        ("Cameroon", 39),
        ("Canada", 40),
        ("Saudi Arabia", 41),
        ("Qatar", 42),
        ("Ghana", 43),
        ("Ivory Coast", 44),
        ("Côte d'Ivoire", 44),
        ("Chile", 45),
        ("Peru", 46),
        ("Sweden", 47),
        ("Norway", 48),
        ("Russia", 49),
        ("Greece", 50),
        ("Algeria", 51),
        ("Mali", 52),
        ("Burkina Faso", 53),
        ("South Africa", 54),
        ("Japan", 14),
        ("China PR", 70),
        ("India", 100),
        ("Republic of Ireland", 58),
        ("Iraq", 59),
        ("Uzbekistan", 60),
        ("Jordan", 64),
        ("United Arab Emirates", 67),
        ("Oman", 79),
        ("Indonesia", 123),
        ("Thailand", 107),
    ];
    // collect all 164 nations from CSV is not available here; build from a
    // static list derived from the CSV nationality column.
    let all: &[&str] = &[
        "Afghanistan",
        "Albania",
        "Algeria",
        "Andorra",
        "Angola",
        "Antigua and Barbuda",
        "Argentina",
        "Armenia",
        "Australia",
        "Austria",
        "Azerbaijan",
        "Bangladesh",
        "Barbados",
        "Belarus",
        "Belgium",
        "Benin",
        "Bermuda",
        "Bolivia",
        "Bosnia and Herzegovina",
        "Brazil",
        "Bulgaria",
        "Burkina Faso",
        "Burundi",
        "Cameroon",
        "Canada",
        "Cape Verde Islands",
        "Central African Republic",
        "Chad",
        "Chile",
        "China PR",
        "Chinese Taipei",
        "Colombia",
        "Comoros",
        "Congo",
        "Congo DR",
        "Costa Rica",
        "Croatia",
        "Cuba",
        "Curaçao",
        "Cyprus",
        "Czech Republic",
        "Côte d'Ivoire",
        "Denmark",
        "Dominican Republic",
        "Ecuador",
        "Egypt",
        "El Salvador",
        "England",
        "Equatorial Guinea",
        "Eritrea",
        "Estonia",
        "Faroe Islands",
        "Finland",
        "France",
        "Gabon",
        "Gambia",
        "Georgia",
        "Germany",
        "Ghana",
        "Gibraltar",
        "Greece",
        "Grenada",
        "Guatemala",
        "Guinea",
        "Guinea-Bissau",
        "Guyana",
        "Haiti",
        "Holland",
        "Honduras",
        "Hong Kong",
        "Hungary",
        "Iceland",
        "India",
        "Indonesia",
        "Iran",
        "Iraq",
        "Israel",
        "Italy",
        "Jamaica",
        "Japan",
        "Jordan",
        "Kazakhstan",
        "Kenya",
        "Korea DPR",
        "Korea Republic",
        "Kosovo",
        "Latvia",
        "Lebanon",
        "Liberia",
        "Libya",
        "Liechtenstein",
        "Lithuania",
        "Luxembourg",
        "Madagascar",
        "Malawi",
        "Mali",
        "Malta",
        "Mauritania",
        "Mauritius",
        "Mexico",
        "Moldova",
        "Montenegro",
        "Montserrat",
        "Morocco",
        "Mozambique",
        "Namibia",
        "New Caledonia",
        "New Zealand",
        "Niger",
        "Nigeria",
        "North Macedonia",
        "Northern Ireland",
        "Norway",
        "Oman",
        "Pakistan",
        "Palestine",
        "Panama",
        "Paraguay",
        "Peru",
        "Philippines",
        "Poland",
        "Portugal",
        "Puerto Rico",
        "Qatar",
        "Republic of Ireland",
        "Romania",
        "Russia",
        "Rwanda",
        "Saudi Arabia",
        "Scotland",
        "Senegal",
        "Serbia",
        "Sierra Leone",
        "Slovakia",
        "Slovenia",
        "Somalia",
        "South Africa",
        "Spain",
        "Sri Lanka",
        "St. Kitts and Nevis",
        "St. Lucia",
        "Suriname",
        "Sweden",
        "Switzerland",
        "Syria",
        "São Tomé e Príncipe",
        "Tanzania",
        "Thailand",
        "Togo",
        "Trinidad and Tobago",
        "Tunisia",
        "Turkey",
        "Uganda",
        "Ukraine",
        "United Arab Emirates",
        "United States",
        "Uruguay",
        "Uzbekistan",
        "Vanuatu",
        "Venezuela",
        "Wales",
        "Yemen",
        "Zambia",
        "Zimbabwe",
    ];
    let mut rank_map: HashMap<String, u8> = HashMap::new();
    for (n, r) in curated {
        rank_map.insert(n.to_string(), *r);
    }
    let mut out = Vec::with_capacity(all.len());
    for (i, n) in all.iter().enumerate() {
        let ranking = *rank_map
            .get(*n)
            .unwrap_or(&(150u8.saturating_add(i as u8).min(254)));
        out.push(NationDef {
            name: n.to_string(),
            confed: confed_of(n),
            ranking,
        });
    }
    out
}

pub fn build_world_leagues(db: &Database) -> WorldLeagues {
    let leagues = catalogue();
    let mut by_nation: BTreeMap<String, Vec<u16>> = BTreeMap::new();
    for l in &leagues {
        by_nation.entry(l.nation.clone()).or_default().push(l.id);
    }
    let mut club_index: HashMap<String, Vec<usize>> = HashMap::new();
    let mut league_clubs: BTreeMap<u16, Vec<String>> = BTreeMap::new();
    let mut club_league: HashMap<String, u16> = HashMap::new();

    let clubs = db.clubs();
    let players = db.players();

    for (idx, p) in players.iter().enumerate() {
        let cid = match p.club {
            Some(c) => c,
            None => continue,
        };
        let club = match clubs.iter().find(|c| c.id == cid) {
            Some(c) => c,
            None => continue,
        };
        let ld = match lookup(&club.league) {
            Some(l) => l,
            None => continue,
        };
        club_index.entry(club.name.clone()).or_default().push(idx);
        let entry = league_clubs.entry(ld.id).or_default();
        if !entry.iter().any(|n| n == &club.name) {
            entry.push(club.name.clone());
        }
        club_league.insert(club.name.clone(), ld.id);
    }

    WorldLeagues {
        leagues,
        by_nation,
        club_index,
        league_clubs,
        club_league,
    }
}
