//! Name pools and procedural world generation.
use crate::model::*;
use rand::Rng;
use rand_pcg::Pcg64Mcg;

const FIRST_NAMES: &[&str] = &[
    "James", "Marco", "Diego", "Luka", "Kai", "Omar", "Ethan", "Mateo", "Noah", "Leo",
    "Hugo", "Felix", "Yuki", "Andres", "Bruno", "Ivan", "Sven", "Karim", "Nico", "Tariq",
    "Sergio", "Pavel", "Jonas", "Malik", "Ryo", "Elias", "Adam", "Theo", "Ola", "Dami",
    "Rashid", "Viktor", "Sami", "Enzo", "Rui", "Gabriel", "Milan", "Aaron", "Cole", "Idris",
    "Luca", "Mario", "Pedro", "Youssef", "Amir", "Dylan", "Mason", "Finn", "Oscar", "Ruben",
    "Matteo", "Lorenzo", "Andrea", "Samuel", "Daniel", "David", "Lucas", "Alex", "Marc", "Pau",
    "Iker", "Unai", "Mikkel", "Anton", "Emil", "Lasse", "Kacper", "Filip", "Stefan", "Nikola",
    "Dejan", "Bojan", "Arda", "Emre", "Cengiz", "Hakim", "Achraf", "Bilal", "Zaid", "Musa",
    "Kwame", "Kofi", "Ibrahim", "Seydou", "Ismael", "Cheick", "Tomoki", "Sota", "Hana", "Jin",
    "Rafael", "Joao", "Tiago", "Vitor", "Andre", "Nuno", "Sacha", "Yannick", "Lars", "Timo",
];

const LAST_NAMES: &[&str] = &[
    "Silva", "Kovac", "Nakamura", "Fernandez", "Muller", "Rossi", "Andersen", "Diallo",
    "Novak", "Haddad", "Okafor", "Petrov", "Bauer", "Costa", "Larsson", "Yilmaz", "Mensah",
    "Ferrari", "Horvath", "Sato", "Moreno", "Janssen", "Hunt", "Traore", "Popov", "Nielsen",
    "Gomez", "Ricci", "Owusu", "Sarr", "Vega", "Horvat", "Tanaka", "Bianchi", "Afolabi",
    "Marino", "Sorensen", "Cruz", "Novakovic", "Diarra",
    "Almeida", "Carvalho", "Pereira", "Ramos", "Torres", "Navarro", "Iglesias", "Castro", "Ortega",
    "Romano", "Conti", "Greco", "Marchetti", "Esposito", "Lombardi", "Colombo", "Bruno",
    "Schmidt", "Wagner", "Becker", "Hoffmann", "Krause", "Werner", "Fischer", "Weber",
    "Dubois", "Laurent", "Moreau", "Girard", "Lefevre", "Mercier", "Bernard", "Rousseau",
    "Jansen", "Visser", "Bakker", "Vermeulen", "Mwangi", "Abubakar", "Osei", "Boateng", "Nkemelu",
    "Kimura", "Yoshida", "Ishikawa", "Kang", "Park", "Petkovic", "Jovanovic", "Markovic",
    "Kowalski", "Nowak", "Zielinski", "Andersson", "Johansson", "Berg", "Lindqvist", "Haugen",
    "Rahimi", "Karimi", "Hassan", "Toure", "Keita", "Camara", "Mendez", "Delgado", "Herrera",
];

/// Culture-aware name pools: a regen's name follows their nationality instead
/// of one flat global pool, so late-era squads read Brazilian, Senegalese,
/// Japanese… rather than generically synthetic. Each culture is small on
/// purpose — combinatorial expansion (below) makes the supply infinite.
macro_rules! names {
    ($name:ident, $($x:expr),*) => { const $name: &[&str] = &[$($x),*]; };
}
names!(FIRST_EN, "James","Harry","Jack","Oliver","George","Alfie","Charlie","Freddie","Archie","Teddy","Mason","Louie","Ronnie","Reggie","Sonny","Tommy","Ellis","Rhys","Callum","Ollie","Aaron","Liam","Connor","Kyle","Jayden");
names!(LAST_EN, "Smith","Jones","Taylor","Davies","Walker","Wright","Thompson","White","Hughes","Edwards","Green","Wood","Harris","Martin","Clarke","Lewis","Young","Hall","Gordon","Beckett","Fletcher","Nolan","Price","Ashworth");
names!(FIRST_ES, "Alejandro","Diego","Pablo","Hugo","Mateo","Leo","Santiago","Thiago","Bruno","Enzo","Gabriel","Lucas","Martin","Dario","Iker","Unai","Pau","Marc","Nil","Pol","Joel","Sergi","Adria","Ivan","Rafa");
names!(LAST_ES, "Garcia","Fernandez","Gonzalez","Rodriguez","Lopez","Martinez","Sanchez","Perez","Martin","Gomez","Ruiz","Diaz","Moreno","Alvarez","Romero","Navarro","Torres","Dominguez","Vega","Herrera","Castro","Ortega","Ramos","Iglesias");
names!(FIRST_FR, "Hugo","Louis","Raphael","Jules","Adam","Lucas","Liam","Gabriel","Ethan","Paul","Arthur","Nathan","Mohamed","Rayan","Ibrahim","Moussa","Yanis","Leny","Mathis","Theo","Enzo","Clement","Baptiste","Noe");
names!(LAST_FR, "Martin","Bernard","Dubois","Thomas","Robert","Petit","Durand","Leroy","Moreau","Laurent","Michel","Garcia","David","Bertrand","Roux","Vincent","Fournier","Morel","Girard","Lefevre","Mercier","Blanc","Guerin","Faure");
names!(FIRST_DE, "Lukas","Felix","Jonas","Finn","Leon","Paul","Jannik","Malik","Timo","Kai","Lars","Niklas","Florian","Marvin","Fabian","Noah","Kevin","Dennis","Robin","Mats","Nico","Tom","Emre","Deniz");
names!(LAST_DE, "Muller","Schmidt","Schneider","Fischer","Weber","Meyer","Wagner","Becker","Schulz","Hoffmann","Krause","Huber","Mayer","Frank","Berger","Kaiser","Brandt","Lehmann","Winkler","Keller","Busch","Vogel","Scholz","Hartmann");
names!(FIRST_IT, "Lorenzo","Matteo","Andrea","Gabriele","Riccardo","Tommaso","Davide","Nicolo","Samuele","Mattia","Alessandro","Federico","Cesare","Edoardo","Giorgio","Michael","Raoul","Giovanni","Marco","Simone","Luca","Elia","Pietro","Rocco");
names!(LAST_IT, "Rossi","Ferrari","Bianchi","Romano","Ricci","Conti","Greco","Marchetti","Esposito","Lombardi","Colombo","Bruno","Gallo","Costa","Fontana","Serra","Riva","Villa","Sala","Testa","Barone","Pellegrino","Sartori","Farina");
names!(FIRST_PT, "Joao","Tiago","Rafael","Miguel","Afonso","Dinis","Santiago","Tomas","Martim","Duarte","Francisco","Rodrigo","Goncalo","Nuno","Ruben","Bruno","Diogo","Pedro","Andre","Rafa","Iker","Dario");
names!(LAST_PT, "Silva","Santos","Ferreira","Pereira","Oliveira","Costa","Rodrigues","Martins","Jesus","Sousa","Fernandes","Carvalho","Gomes","Almeida","Ribeiro","Pinto","Teixeira","Moreira","Cardoso","Nogueira","Esteves","Moniz");
names!(FIRST_BR, "Gabriel","Lucas","Matheus","Pedro","Rafael","Caio","Igor","Wesley","Marcos","Thiago","Bruno","Danilo","Diego","Felipe","Murilo","Juninho","Gustavo","Joao","Leandro","Breno","Vitor","Davi","Enzo","Kaua");
names!(LAST_BR, "Silva","Souza","Oliveira","Santos","Ferreira","Almeida","Carvalho","Araujo","Barbosa","Cardoso","Teixeira","Moreira","Pereira","Ribeiro","Nascimento","Batista","Freitas","Moraes","Peixoto","Sales","Xavier","Prado","Lacerda","Queiroz");
names!(FIRST_NL, "Daan","Sem","Mees","Luuk","Finn","Milan","Levi","Noud","Teun","Gijs","Stijn","Pim","Jari","Ryan","Jeremie","Thijs","Bram","Joris","Micky","Sven","Justin","Calvin","Ruben","Daley");
names!(LAST_NL, "Jansen","Bakker","Visser","Vermeulen","Smit","Meijer","De Vries","Van Dijk","Van den Berg","Jong","Willems","Bos","Vos","Kok","Dekker","Brouwer","Mulder","Peters","Hendriks","Groot","Kuijpers","Scholten","Dijkstra","Linden");
names!(FIRST_SC, "Lukas","Emil","Oliver","Noah","Victor","William","Hugo","Elias","Axel","Viggo","Sander","Magnus","Henrik","Martin","Jens","Mikkel","Rasmus","Christian","Pierre","Joakim","Mathias","Mads","Jonas","Gustav");
names!(LAST_SC, "Andersen","Nielsen","Sorensen","Larsen","Jensen","Petersen","Holm","Lund","Bakken","Haugen","Lie","Berg","Dahl","Lindqvist","Johansson","Andersson","Nilsson","Lindgren","Isaksson","Bergstrom","Nyholm","Moen","Ruud","Sveen");
names!(FIRST_SL, "Luka","Stefan","Nikola","Marko","Ivan","Petar","Dusan","Aleksandar","Filip","Milos","Dejan","Bojan","Nemanja","Zoran","Djordje","Milan","Josip","Zlatko","Dominik","Andrej","Toni","Goran","Vedran","Mateo");
names!(LAST_SL, "Novak","Horvat","Kovac","Petrovic","Popovic","Jovanovic","Petrov","Ivanov","Kovacevic","Markovic","Nikolic","Lukic","Stankovic","Ilic","Pavlovic","Simic","Ristic","Babic","Vukovic","Djukic","Zivkovic","Savic","Tomic","Milic");
names!(FIRST_WA, "Ibrahim","Moussa","Cheick","Sekou","Mamadou","Abdoulaye","Ousseynou","Habib","Modou","Pape","Alassane","Cheikhou","Demba","Fode","Keita","Bakary","Lassana","Mohamed","Aliou","Youssouf","Amadou","Oumar","Sory","Aguibou");
names!(LAST_WA, "Diallo","Traore","Diarra","Camara","Keita","Toure","Sow","Ndiaye","Sarr","Cisse","Sylla","Bah","Conde","Doumbia","Coulibaly","Drame","Gassama","Diakhate","Faye","Badji","Diop","Niang","Owusu","Mensah");
names!(FIRST_AR, "Mohamed","Youssef","Omar","Karim","Hakim","Walid","Bilal","Rayan","Ilias","Amine","Fouad","Kamel","Lotfi","Romain","Selim","Aissa","Tarek","Jalal","Hamza","Said","Mehdi","Nabil","Yacine","Adam");
names!(LAST_AR, "Haddad","Hassan","Benali","El Amrani","Tahrat","Haddadi","Ghezali","Abdelhamid","Rahimi","Belkacem","Taleb","Belhadj","Bouzid","Messaoud","Othmani","Chaib","Fares","Meziane","Larbi","Cherif","Khelifi","Saidi","Kaddour","Zerhouni");
names!(FIRST_JA, "Yuki","Ren","Sota","Haruto","Soma","Koki","Ao","Riku","Itsuki","Hayato","Taisei","Shuto","Kaito","Daichi","Wataru","Haruki","Yuto","Kaoru","Ritsu","Daizen","Ayase","Shogo","Ko","Jun");
names!(LAST_JA, "Sato","Suzuki","Takahashi","Tanaka","Watanabe","Yamamoto","Nakamura","Kobayashi","Ito","Kato","Yoshida","Yamada","Sasaki","Yamaguchi","Matsumoto","Inoue","Kimura","Hayashi","Shimizu","Mori","Abe","Ikeda","Hashimoto","Ishikawa");
names!(FIRST_KR, "Minjun","Seojun","Hajun","Donghyun","Jihoon","Taeyang","Taehoon","Minseok","Jaesung","Wooyeong","Hyeonwoo","Seungho","Jongho","Inbeom","Jinwoo","Sungmin","Dongwon","Jaejoong","Hyukkyu","Siwoo","Yuna","Jiwon","Sua","Hana");
names!(LAST_KR, "Kim","Lee","Park","Choi","Jung","Kang","Cho","Yoon","Shin","Han","Oh","Seo","Kwon","Hwang","Ahn","Song","Hong","Jeon","Moon","Son","Hwang","Ryu","Noh","Bae");
names!(FIRST_TR, "Emre","Arda","Kenan","Semih","Yusuf","Mert","Altay","Ferdi","Ismail","Salih","Okay","Cengiz","Hakan","Merih","Samet","Zeki","Abdulkadir","Baris","Yunus","Bertug","Can","Deniz","Doruk","Efe");
names!(LAST_TR, "Yilmaz","Kaya","Demir","Sahin","Celik","Yavuz","Ozturk","Aydin","Ozdemir","Arslan","Dogan","Kilic","Aslan","Cetin","Unal","Koc","Kurt","Simsek","Turan","Tekin","Ozkan","Polat","Sari","Avci");

const CITY: &[&str] = &[
    "North", "Real", "Athletic", "Sporting", "Dynamo", "United", "City", "Wanderers",
    "Rovers", "Albion", "County", "Athletico", "Olympic", "Racing", "Inter", "Central",
];
const CITY2: &[&str] = &[
    "Riverton", "Kingsbridge", "Ashford", "Belmont", "Cordoba", "Vale", "Harbour", "Elmwood",
    "Granite", "Silverlake", "Redcliff", "Norwood", "Fairhaven", "Oakport", "Westmere",
    "Highgate", "Brookdale", "Stonemoor", "Larkfield", "Marston",
];

/// Lift factor from the compact tier space generation reasons in (~10-30) onto
/// the shared FIFA-style 0-100 attribute scale used across the whole game.
const GEN_SCALE: f32 = 3.0;

/// Map a nationality label to its naming culture. Best-effort substring match;
/// anything unrecognized falls back to the global pool.
fn culture_for(nation: &str) -> &'static str {
    let n = nation.to_lowercase();
    let has = |s: &str| n.contains(s);
    if has("england") || has("scotland") || has("wales") || n == "ireland" || has("republic of ireland") || has("northern ireland") || has("united states") || has("australia") || has("canada") {
        "english"
    } else if has("spain") || has("mexico") || has("colombia") || has("uruguay") || has("chile") || has("peru") || has("paraguay") || has("venezuela") || has("ecuador") || has("bolivia") || has("costa rica") || has("honduras") {
        "spanish"
    } else if has("argentina") {
        "spanish"
    } else if has("france") || has("belgium") || has("switzerland") {
        "french"
    } else if has("germany") || has("austria") {
        "german"
    } else if has("italy") {
        "italian"
    } else if has("portugal") {
        "portuguese"
    } else if has("brazil") {
        "brazilian"
    } else if has("netherlands") || has("holland") {
        "dutch"
    } else if has("sweden") || has("norway") || has("denmark") || has("finland") || has("iceland") {
        "scandinavian"
    } else if has("croatia") || has("serbia") || has("slovenia") || has("bosnia") || has("montenegro") || has("macedonia") || has("poland") || has("czech") || has("slovak") || has("ukraine") || has("russia") || has("georgia") || has("albania") || has("kosovo") || has("romania") || has("bulgaria") || has("hungary") || has("greece") {
        "slavic"
    } else if has("senegal") || has("mali") || has("ivory") || has("ghana") || has("nigeria") || has("cameroon") || has("guinea") || has("gambia") || has("sierra") || has("liberia") || has("burkina") || has("togo") || has("benin") || has("niger") || has("chad") || has("congo") || has("angola") || has("gabon") {
        "west_african"
    } else if has("morocco") || has("algeria") || has("tunisia") || has("egypt") {
        "arabic"
    } else if has("japan") {
        "japanese"
    } else if has("korea") {
        "korean"
    } else if has("turk") {
        "turkish"
    } else {
        "global"
    }
}

fn culture_pools(culture: &str) -> (&'static [&'static str], &'static [&'static str]) {
    match culture {
        "english" => (&FIRST_EN, &LAST_EN),
        "spanish" => (&FIRST_ES, &LAST_ES),
        "french" => (&FIRST_FR, &LAST_FR),
        "german" => (&FIRST_DE, &LAST_DE),
        "italian" => (&FIRST_IT, &LAST_IT),
        "portuguese" => (&FIRST_PT, &LAST_PT),
        "brazilian" => (&FIRST_BR, &LAST_BR),
        "dutch" => (&FIRST_NL, &LAST_NL),
        "scandinavian" => (&FIRST_SC, &LAST_SC),
        "slavic" => (&FIRST_SL, &LAST_SL),
        "west_african" => (&FIRST_WA, &LAST_WA),
        "arabic" => (&FIRST_AR, &LAST_AR),
        "japanese" => (&FIRST_JA, &LAST_JA),
        "korean" => (&FIRST_KR, &LAST_KR),
        "turkish" => (&FIRST_TR, &LAST_TR),
        _ => (&FIRST_NAMES, &LAST_NAMES),
    }
}

/// Draw one name from a culture's pools, escalating through combinatorial
/// expansion (middle name, hyphenated surname, generational suffix) until the
/// caller accepts it — the supply is effectively infinite, so late-era squads
/// never recycle identities.
fn regen_name_from(
    rng: &mut impl rand::Rng,
    culture: &str,
    is_used: &mut dyn FnMut(&str) -> bool,
) -> String {
    const SUFFIXES: &[&str] = &["Jr.", "II", "III"];
    let (firsts, lasts) = culture_pools(culture);
    fn pick<'a>(xs: &'a [&'a str], rng: &mut impl rand::Rng) -> &'a str {
        xs[rng.gen_range(0..xs.len())]
    }
    // Draw a token distinct from `avoid` so names never read "Milos Milos" or
    // "Ronnie Ronnie": a repeated adjacent token is a classic generator tell.
    fn pick_distinct<'a>(
        xs: &'a [&'a str],
        avoid: &str,
        rng: &mut impl rand::Rng,
    ) -> &'a str {
        for _ in 0..6 {
            let c = xs[rng.gen_range(0..xs.len())];
            if c != avoid {
                return c;
            }
        }
        xs[rng.gen_range(0..xs.len())]
    }
    loop {
        let f = pick(firsts, rng);
        let l = pick(lasts, rng);
        // Mostly the plain form; escalate only on collision pressure. A middle
        // name or hyphenated surname is drawn distinct from its neighbour, and
        // a generational suffix is rare (and never stacks on a compound), so
        // the output reads like a person, not "Xavi Timber IV".
        let r: f32 = rng.gen();
        let cand = if r < 0.72 {
            format!("{} {}", f, l)
        } else if r < 0.85 {
            format!("{} {} {}", f, pick_distinct(firsts, f, rng), l)
        } else if r < 0.97 {
            let l2 = pick_distinct(lasts, l, rng);
            format!("{} {}-{}", f, l, l2)
        } else {
            format!("{} {} {}", f, l, SUFFIXES[rng.gen_range(0..SUFFIXES.len())])
        };
        if !is_used(&cand) {
            return cand;
        }
    }
}

pub struct Gen {
    pub rng: Pcg64Mcg,
    next_player: PlayerId,
}

impl Gen {
    pub fn new(seed: u64) -> Self {
        Gen {
            rng: Pcg64Mcg::new(seed as u128 | 1),
            next_player: 1,
        }
    }

    fn name(&mut self) -> String {
        regen_name_from(&mut self.rng, "global", &mut |_| false)
    }

    /// A generated person's name (for managers, staff, regens off-pitch).
    pub fn person_name(rng: &mut impl rand::Rng) -> String {
        regen_name_from(rng, "global", &mut |_| false)
    }

    /// A guaranteed-distinct regen name for a nation: culture-aware pools plus
    /// combinatorial expansion (middle names, hyphenated surnames, Jr/II/III),
    /// so the supply never runs out no matter how many decades simulate.
    pub fn regen_name(
        rng: &mut impl rand::Rng,
        nation: &str,
        used: &std::collections::HashSet<String>,
    ) -> String {
        let culture = culture_for(nation);
        regen_name_from(rng, culture, &mut |n| used.contains(n))
    }

    /// Draw an attribute centred on `center` with spread, on the 0-100 scale.
    /// Generation reasons in a compact tier space (a club "strength" ~10-30);
    /// the ×GEN_SCALE lift puts the result on the same FIFA-style 0-100 axis as
    /// the FC/FM data so generated and real players sit side by side.
    fn attr(&mut self, center: i32, spread: i32) -> u8 {
        let v = center + self.rng.gen_range(-spread..=spread);
        ((v as f32 * GEN_SCALE).round() as i32).clamp(1, 99) as u8
    }

    /// Draw a value uniformly in [lo, hi], lifted to the 0-100 scale.
    fn base(&mut self, lo: i32, hi: i32) -> u8 {
        let lo = lo.max(1);
        let hi = hi.max(lo);
        let v = self.rng.gen_range(lo..=hi);
        ((v as f32 * GEN_SCALE).round() as i32).clamp(1, 99) as u8
    }

    /// Generate one player whose ability centres on `strength` (roughly the club's tier).
    pub fn player(&mut self, pos: Position, strength: i32) -> Player {
        let id = self.next_player;
        self.next_player += 1;
        let age = self.rng.gen_range(16..=36) as u8;
        let s = strength;
        let hi = s + 6;
        let lo = (s - 6).max(1);

        // Position-biased attribute profile.
        let attrs = match pos {
            Position::GK => Attributes {
                pace: self.attr(s - 15, 8),
                shooting: self.attr(s - 30, 6),
                passing: self.attr(s - 6, 8),
                dribbling: self.attr(s - 20, 6),
                tackling: self.attr(s - 12, 8),
                heading: self.attr(s - 10, 8),
                strength: self.attr(s + 2, 8),
                stamina: self.attr(s - 4, 8),
                vision: self.attr(s - 6, 8),
                composure: self.attr(s + 2, 8),
                goalkeeping: self.base(lo + 4, hi + 6),
                first_touch: self.attr(s - 5, 6),
            },
            Position::DEF => {
                // Roughly half of all defenders are full-backs / wing-backs (a
                // back four fields two of each): pacey, high-stamina, crossing
                // types with less aerial and physical presence. Generating them
                // as a distinct profile means best_role_for tags them FB/WB, so
                // the stat tables and legends boards aren't wall-to-wall
                // centre-backs — left and right backs show up as themselves.
                if self.rng.gen_bool(0.5) {
                    Attributes {
                        pace: self.base(lo + 2, hi + 5),
                        shooting: self.attr(s - 14, 8),
                        passing: self.attr(s + 2, 8),
                        dribbling: self.attr(s + 2, 8),
                        tackling: self.attr(s, 8),
                        heading: self.attr(s - 8, 8),
                        strength: self.attr(s - 6, 8),
                        stamina: self.base(lo + 3, hi + 6),
                        vision: self.attr(s - 3, 8),
                        composure: self.attr(s, 8),
                        goalkeeping: self.attr(3, 3),
                        first_touch: self.attr(s + 1, 6),
                    }
                } else {
                    Attributes {
                        pace: self.attr(s, 9),
                        shooting: self.attr(s - 18, 8),
                        passing: self.attr(s - 4, 8),
                        dribbling: self.attr(s - 10, 8),
                        tackling: self.base(lo + 4, hi + 6),
                        heading: self.attr(s + 4, 8),
                        strength: self.attr(s + 4, 8),
                        stamina: self.attr(s + 2, 8),
                        vision: self.attr(s - 6, 8),
                        composure: self.attr(s, 8),
                        goalkeeping: self.attr(3, 3),
                        first_touch: self.attr(s, 6),
                    }
                }
            }
            Position::MID => Attributes {
                pace: self.attr(s, 9),
                shooting: self.attr(s - 4, 9),
                passing: self.base(lo + 3, hi + 5),
                dribbling: self.attr(s + 2, 9),
                tackling: self.attr(s - 2, 9),
                heading: self.attr(s - 6, 8),
                strength: self.attr(s - 2, 8),
                stamina: self.base(lo + 4, hi + 6),
                vision: self.base(lo + 2, hi + 5),
                composure: self.attr(s, 8),
                goalkeeping: self.attr(3, 3),
                first_touch: self.attr(s + 2, 6),
            },
            Position::FWD => {
                // Modern football's scorers are mostly agile — inside forwards,
                // false nines, poachers who live on pace, dribbling and a quick
                // finish — with the classic aerial target man now the minority.
                // Generating ~70% agile / 30% target man (and the header engine
                // no longer handing set-pieces only to the tallest man) keeps
                // the scoring charts topped by complete forwards, not 6'4" TMs.
                if self.rng.gen_bool(0.70) {
                    Attributes {
                        pace: self.base(lo + 4, hi + 7),
                        shooting: self.base(lo + 3, hi + 6),
                        passing: self.attr(s - 2, 8),
                        dribbling: self.base(lo + 4, hi + 7),
                        tackling: self.attr(s - 16, 8),
                        heading: self.attr(s - 8, 8),
                        strength: self.attr(s - 6, 8),
                        stamina: self.attr(s, 8),
                        vision: self.attr(s, 8),
                        composure: self.base(lo + 3, hi + 6),
                        goalkeeping: self.attr(3, 3),
                        first_touch: self.attr(s + 4, 6),
                    }
                } else {
                    Attributes {
                        pace: self.attr(s - 4, 8),
                        shooting: self.base(lo + 4, hi + 6),
                        passing: self.attr(s - 6, 8),
                        dribbling: self.attr(s - 8, 8),
                        tackling: self.attr(s - 16, 8),
                        heading: self.base(lo + 3, hi + 6),
                        strength: self.base(lo + 2, hi + 6),
                        stamina: self.attr(s - 2, 8),
                        vision: self.attr(s - 4, 8),
                        composure: self.attr(s + 2, 8),
                        goalkeeping: self.attr(3, 3),
                        first_touch: self.attr(s, 6),
                    }
                }
            }
        };

        let ovr = attrs.overall(pos) as i32;
        // Younger players have headroom; potential >= current.
        let growth = if age <= 21 {
            self.rng.gen_range(4..=22)
        } else if age <= 25 {
            self.rng.gen_range(0..=8)
        } else {
            0
        };
        let potential = (ovr + growth).clamp(1, 99) as u8;

        // Wage from ability; divisor is scaled for the 0-100 attribute axis so
        // a generated star's wage stays in the same currency band as before.
        let wage = ((ovr as u32).pow(2) / 33).max(500) * 10;
        // Weighted toward balanced/positive personalities, with a tail of difficult ones.
        let personality = match self.rng.gen_range(0..10) {
            0 => Personality::ModelCitizen,
            1 => Personality::Professional,
            2 => Personality::Driven,
            3 => Personality::Resolute,
            4 | 5 | 6 => Personality::Balanced,
            7 => Personality::Jovial,
            8 => Personality::Casual,
            _ => Personality::Temperamental,
        };
        let mut p = Player {
            id,
            name: self.name(),
            age,
            position: pos,
            nation: String::new(),
            attrs,
            potential,
            contract: Contract {
                wage,
                years: self.rng.gen_range(1..=4),
            },
            morale: self.rng.gen_range(60..=90),
            fitness: self.rng.gen_range(85..=100),
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
            personality,
        };
        p.recompute_value();
        p
    }

    /// Build a full 22-25 player squad for a club of the given strength tier.
    pub fn squad(&mut self, strength: i32) -> Vec<Player> {
        let mut v = Vec::new();
        // Balanced squad composition.
        let plan = [
            (Position::GK, 3),
            (Position::DEF, 8),
            (Position::MID, 8),
            (Position::FWD, 5),
        ];
        for (pos, n) in plan {
            for _ in 0..n {
                v.push(self.player(pos, strength));
            }
        }
        v
    }

    pub fn club_name(&mut self, taken: &[String]) -> (String, String) {
        loop {
            let a = CITY[self.rng.gen_range(0..CITY.len())];
            let b = CITY2[self.rng.gen_range(0..CITY2.len())];
            let name = format!("{} {}", a, b);
            if !taken.contains(&name) {
                let short: String = b.chars().take(3).collect::<String>().to_uppercase();
                return (name, short);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regen_forwards_are_mostly_agile() {
        // Modern scorers are agile (IF/F9/poacher/shadow), with target men the
        // minority — so the scoring charts don't fill with 6'4" TMs. Judge by
        // the best-fitting forward role from attributes.
        use crate::engine::best_role_for;
        let mut g = Gen::new(13);
        let mut tm = 0;
        let total = 300;
        for _ in 0..total {
            let p = g.player(Position::FWD, 20);
            if best_role_for(&p.attrs, Position::FWD, 3).0.pos_label() == "TM" {
                tm += 1;
            }
        }
        assert!(tm * 2 < total, "target men dominate the forward pool ({}/{})", tm, total);
        assert!(tm > 0, "no target men generated at all ({}/{})", tm, total);
    }


    fn regen_names_have_no_doubled_tokens() {
        // "Milos Milos Popovic" / "Ronnie Ronnie Becker" are a generator tell:
        // no two adjacent tokens may be identical across many draws.
        let mut rng = Pcg64Mcg::new(99);
        let used = std::collections::HashSet::new();
        for culture in ["Brazil", "Serbia", "England", "Japan", "Morocco", "Netherlands"] {
            for _ in 0..400 {
                let n = Gen::regen_name(&mut rng, culture, &used);
                let toks: Vec<&str> = n.split([' ', '-']).filter(|t| !t.is_empty()).collect();
                for w in toks.windows(2) {
                    assert_ne!(w[0], w[1], "doubled adjacent token in '{}'", n);
                }
            }
        }
    }

    #[test]
    fn regen_names_avoid_famous_surnames() {
        // The curated pools no longer mint instantly-recognisable superstars
        // like "Gvardiol"/"Modric"/"Timber" — a classic recycled-name glitch.
        let mut rng = Pcg64Mcg::new(123);
        let used = std::collections::HashSet::new();
        let banned = ["Gvardiol", "Modric", "Timber", "Mahrez", "Kimmich", "Kane"];
        for culture in ["Croatia", "Netherlands", "Germany", "Morocco", "England"] {
            for _ in 0..500 {
                let n = Gen::regen_name(&mut rng, culture, &used);
                for b in banned {
                    assert!(!n.contains(b), "regen reused famous surname '{}' in '{}'", b, n);
                }
            }
        }
    }

    #[test]
    fn regen_names_never_repeat() {
        // 5000 draws against a live used-set must all be distinct — the supply
        // is effectively infinite, so no bounded-retry duplicates ever again.
        let mut rng = Pcg64Mcg::new(7);
        let mut used = std::collections::HashSet::new();
        for _ in 0..5000 {
            let n = Gen::regen_name(&mut rng, "Brazil", &used);
            assert!(!used.contains(&n), "duplicate regen name: {}", n);
            used.insert(n);
        }
    }

    #[test]
    fn regen_names_follow_culture() {
        let mut rng = Pcg64Mcg::new(11);
        let used = std::collections::HashSet::new();
        // Spot-check: Japanese and Brazilian pools must not overlap.
        let mut ja = std::collections::HashSet::new();
        for _ in 0..50 {
            ja.insert(Gen::regen_name(&mut rng, "Japan", &used));
        }
        for _ in 0..50 {
            let b = Gen::regen_name(&mut rng, "Brazil", &used);
            assert!(!ja.contains(&b), "culture pools overlap: {}", b);
        }
        // Unknown nations fall back to the global pool without panicking.
        let g = Gen::regen_name(&mut rng, "", &used);
        assert!(g.contains(' '));
    }
}

