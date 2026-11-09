//! Player traits (fc27 playstyles) mapped to an internal enum.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trait {
    FinesseShot,
    PowerHeader,
    Acrobatic,
    Gamechanger,
    Rapid,
    QuickStep,
    TikiTaka,
    IncisivePass,
    Inventive,
    Technical,
    FirstTouch,
    LongBallPass,
    DeadBall,
    Trivela,
    LowDrivenShot,
    PrecisionHeader,
    Block,
    Anticipate,
    Bruiser,
    Trickster,
    KeeperSweep,
    KeeperRush,
    LongThrow,
    Flair,
    #[serde(other)]
    Other,
}

impl Trait {
    pub fn label(&self) -> &'static str {
        match self {
            Trait::FinesseShot => "Finesse Shot",
            Trait::PowerHeader => "Power Header",
            Trait::Acrobatic => "Acrobatic",
            Trait::Gamechanger => "Gamechanger",
            Trait::Rapid => "Rapid",
            Trait::QuickStep => "Quick Step",
            Trait::TikiTaka => "Tiki Taka",
            Trait::IncisivePass => "Incisive Pass",
            Trait::Inventive => "Inventive",
            Trait::Technical => "Technical",
            Trait::FirstTouch => "First Touch",
            Trait::LongBallPass => "Long Ball Pass",
            Trait::DeadBall => "Dead Ball",
            Trait::Trivela => "Trivela",
            Trait::LowDrivenShot => "Low Driven Shot",
            Trait::PrecisionHeader => "Precision Header",
            Trait::Block => "Block",
            Trait::Anticipate => "Anticipate",
            Trait::Bruiser => "Bruiser",
            Trait::Trickster => "Trickster",
            Trait::KeeperSweep => "Keeper Sweep",
            Trait::KeeperRush => "Keeper Rush",
            Trait::LongThrow => "Long Throw",
            Trait::Flair => "Flair",
            Trait::Other => "Other",
        }
    }
}

pub fn has(v: &[Trait], t: Trait) -> bool {
    v.contains(&t)
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn map_one(s: &str) -> Option<Trait> {
    match norm(s).as_str() {
        "finesseshot" => Some(Trait::FinesseShot),
        "powerheader" => Some(Trait::PowerHeader),
        "acrobatic" => Some(Trait::Acrobatic),
        "gamechanger" => Some(Trait::Gamechanger),
        "rapid" => Some(Trait::Rapid),
        "quickstep" => Some(Trait::QuickStep),
        "tikitaka" => Some(Trait::TikiTaka),
        "incisivepass" => Some(Trait::IncisivePass),
        "inventive" => Some(Trait::Inventive),
        "technical" => Some(Trait::Technical),
        "firsttouch" => Some(Trait::FirstTouch),
        "longballpass" => Some(Trait::LongBallPass),
        "deadball" => Some(Trait::DeadBall),
        "trivela" => Some(Trait::Trivela),
        "lowdrivenshot" => Some(Trait::LowDrivenShot),
        "precisionheader" => Some(Trait::PrecisionHeader),
        "block" => Some(Trait::Block),
        "anticipate" => Some(Trait::Anticipate),
        "bruiser" => Some(Trait::Bruiser),
        "trickster" => Some(Trait::Trickster),
        "keepersweep" => Some(Trait::KeeperSweep),
        "keeperrush" => Some(Trait::KeeperRush),
        "longthrow" => Some(Trait::LongThrow),
        "flair" => Some(Trait::Flair),
        _ => None,
    }
}

pub fn parse_playstyles(s: &str) -> Vec<Trait> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let mut t = part.trim();
        // Strip trailing '+' variants; base trait still maps.
        while t.ends_with('+') {
            t = t.trim_end_matches('+').trim();
        }
        if let Some(tr) = map_one(t) {
            if !out.contains(&tr) {
                out.push(tr);
            }
        }
    }
    out
}
