//! Spatial formation geometry — Track 3 (the physics layer).
//!
//! Until now a formation was only a (def, mid, fwd) line count, so the engine
//! could not tell a 4-3-3's holding midfielder from a 4-2-3-1's playmaker —
//! both were just "MID". This module gives every slot an (x, y) coordinate on
//! a 100×100 tactical pitch plus a phase model, so *where* a player stands and
//! *where* the opponent leaves space drive which duels happen. The physics is
//! geometry-driven, not scripted: a narrow shape genuinely concedes the flanks,
//! a high line genuinely rewards a runner's pace, wing-backs really do create
//! wide overloads against a back three.
//!
//! Conventions:
//! - x: 0 = own-left touchline … 50 = centre … 100 = own-right touchline.
//! - y: 0 = own goal line … 100 = opponent's goal line (the team attacks +y).
//! - Slot 0 is the goalkeeper at (50, 4).

use serde::{Deserialize, Serialize};

use crate::model::{Formation, Position, TeamInstructions};

/// A slot's coordinate on the tactical pitch.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Slot {
    /// Width: 0 = left touchline, 50 = centre, 100 = right touchline.
    pub x: u8,
    /// Depth: 0 = own goal, 100 = opponent's goal.
    pub y: u8,
}

impl Slot {
    pub const fn new(x: u8, y: u8) -> Slot {
        Slot { x, y }
    }
    /// How far out wide this slot plays (0 = central, 50 = on the line).
    pub fn widthness(&self) -> u8 {
        (self.x as i16 - 50).unsigned_abs() as u8
    }
}

/// The 11 starting coordinates for a shape, in lineup order (slot 0 = GK).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    pub slots: [Slot; 11],
}

/// Broad vertical zone a slot lives in. Used to weight which players take part
/// in each possession phase (build-up / progression / final third).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    Defence,
    Midfield,
    Attack,
}

/// Vertical phase bands (y-centre of the zone the phase is fought over).
/// Build-up happens deep, progression through the middle, the final third high.
pub const BUILDUP_Y: u8 = 24;
pub const PROGRESSION_Y: u8 = 48;
pub const FINAL_THIRD_Y: u8 = 76;

/// Zone a y-coordinate belongs to (for coarse phase tagging).
pub fn zone_of(y: u8) -> Zone {
    if y < 34 {
        Zone::Defence
    } else if y < 63 {
        Zone::Midfield
    } else {
        Zone::Attack
    }
}

/// How strongly a slot participates in a phase, from the distance between its
/// depth and the phase's centre band. Falls off with a ~28-yard half-width so a
/// striker still occasionally helps the build-up and a centre-back can step in,
/// but each phase is dominated by the line that lives there.
pub fn phase_weight(slot_y: u8, phase_y: u8) -> f32 {
    let d = (slot_y as i32 - phase_y as i32).abs() as f32;
    (1.0 - d / 28.0).clamp(0.05, 1.0)
}

// ---------------------------------------------------------------------------
// Formation tables. Each is an explicit, reviewable [Slot; 11] in lineup order:
// GK, then defenders (left→right), midfielders (deep→advanced, left→right),
// forwards (left→right). These are *base* coordinates; width & defensive-line
// instructions shift them at runtime.
// ---------------------------------------------------------------------------

const GK: Slot = Slot::new(50, 4);

/// 4-4-2: flat back four, flat midfield four, front two.
const SHAPE_442: Shape = Shape {
    slots: [
        GK,
        Slot::new(14, 18),
        Slot::new(38, 17),
        Slot::new(62, 17),
        Slot::new(86, 18), // back 4
        Slot::new(14, 46),
        Slot::new(38, 44),
        Slot::new(62, 44),
        Slot::new(86, 46), // mid 4
        Slot::new(40, 74),
        Slot::new(60, 74), // front 2
    ],
};

/// 4-3-3: back four, single pivot + two 8s, wide front three.
const SHAPE_433: Shape = Shape {
    slots: [
        GK,
        Slot::new(14, 18),
        Slot::new(38, 17),
        Slot::new(62, 17),
        Slot::new(86, 18),
        Slot::new(50, 34),
        Slot::new(36, 46),
        Slot::new(64, 46), // DM + 2 CM
        Slot::new(18, 72),
        Slot::new(50, 78),
        Slot::new(82, 72), // LW ST RW
    ],
};

/// 4-3-3 (flat): the three central midfielders in a line — less between-lines
/// threat than the pivot shape but a more even spread of bodies.
const SHAPE_433F: Shape = Shape {
    slots: [
        GK,
        Slot::new(14, 18),
        Slot::new(38, 17),
        Slot::new(62, 17),
        Slot::new(86, 18),
        Slot::new(28, 42),
        Slot::new(50, 42),
        Slot::new(72, 42), // flat 3
        Slot::new(18, 72),
        Slot::new(50, 78),
        Slot::new(82, 72),
    ],
};

/// 4-1-4-1: anchor screening the back four, bank of four, lone striker.
const SHAPE_4141: Shape = Shape {
    slots: [
        GK,
        Slot::new(14, 18),
        Slot::new(38, 17),
        Slot::new(62, 17),
        Slot::new(86, 18),
        Slot::new(50, 32), // anchor
        Slot::new(16, 50),
        Slot::new(40, 50),
        Slot::new(60, 50),
        Slot::new(84, 50), // mid 4
        Slot::new(50, 76), // lone ST
    ],
};

/// 4-4-1-1: 4-4-2 with a #10 off the main striker rather than a second forward.
const SHAPE_4411: Shape = Shape {
    slots: [
        GK,
        Slot::new(14, 18),
        Slot::new(38, 17),
        Slot::new(62, 17),
        Slot::new(86, 18),
        Slot::new(14, 46),
        Slot::new(38, 44),
        Slot::new(62, 44),
        Slot::new(86, 46),
        Slot::new(50, 62), // #10
        Slot::new(50, 78), // striker
    ],
};

/// 4-4-2 diamond: narrow — DM, two box-to-box shuttlers, a #10, two strikers.
/// Genuinely concedes the flanks (no wide mids), which the engine punishes.
const SHAPE_442D: Shape = Shape {
    slots: [
        GK,
        Slot::new(16, 18),
        Slot::new(38, 17),
        Slot::new(62, 17),
        Slot::new(84, 18),
        Slot::new(50, 34), // DM (point)
        Slot::new(38, 48),
        Slot::new(62, 48), // shuttlers
        Slot::new(50, 60), // #10
        Slot::new(40, 76),
        Slot::new(60, 76), // front 2
    ],
};

/// 3-5-2: back three, wing-backs, three central mids, front two.
const SHAPE_352: Shape = Shape {
    slots: [
        GK,
        Slot::new(28, 17),
        Slot::new(50, 16),
        Slot::new(72, 17), // back 3
        Slot::new(10, 44), // LWB
        Slot::new(40, 44),
        Slot::new(50, 42),
        Slot::new(60, 44), // 3 CM
        Slot::new(90, 44), // RWB
        Slot::new(42, 74),
        Slot::new(58, 74), // front 2
    ],
};

/// 3-5-2 (attacking): wing-backs pushed higher, one mid more advanced.
const SHAPE_352A: Shape = Shape {
    slots: [
        GK,
        Slot::new(28, 17),
        Slot::new(50, 16),
        Slot::new(72, 17),
        Slot::new(10, 54), // LWB high
        Slot::new(38, 44),
        Slot::new(50, 40),
        Slot::new(62, 44),
        Slot::new(90, 54), // RWB high
        Slot::new(50, 64), // advanced mid
        Slot::new(50, 78), // striker
    ],
};

/// 3-4-2-1: back three, two wing-backs + two mids, two #10s behind a striker.
const SHAPE_3421: Shape = Shape {
    slots: [
        GK,
        Slot::new(28, 17),
        Slot::new(50, 16),
        Slot::new(72, 17),
        Slot::new(10, 46), // LWB
        Slot::new(40, 44),
        Slot::new(60, 44), // 2 CM
        Slot::new(90, 46), // RWB
        Slot::new(34, 64),
        Slot::new(66, 64), // two #10s
        Slot::new(50, 78), // striker
    ],
};

/// 3-4-3: back three, wing-backs, two mids, front three.
const SHAPE_343: Shape = Shape {
    slots: [
        GK,
        Slot::new(28, 17),
        Slot::new(50, 16),
        Slot::new(72, 17),
        Slot::new(10, 46),
        Slot::new(40, 44),
        Slot::new(60, 44),
        Slot::new(90, 46),
        Slot::new(20, 72),
        Slot::new(50, 78),
        Slot::new(80, 72), // front 3
    ],
};

/// 4-2-3-1: double pivot, three attacking mids, lone striker.
const SHAPE_4231: Shape = Shape {
    slots: [
        GK,
        Slot::new(14, 18),
        Slot::new(38, 17),
        Slot::new(62, 17),
        Slot::new(86, 18),
        Slot::new(42, 36),
        Slot::new(58, 36), // double pivot
        Slot::new(20, 58),
        Slot::new(50, 60),
        Slot::new(80, 58), // 3 AM
        Slot::new(50, 78), // striker
    ],
};

/// 5-3-2: a flat back five (the wing-backs sit deep, y<34 so they read as
/// defenders), three central mids, two strikers.
const SHAPE_532: Shape = Shape {
    slots: [
        GK,
        Slot::new(10, 26),
        Slot::new(30, 17),
        Slot::new(50, 16),
        Slot::new(70, 17),
        Slot::new(90, 26), // WB CB CB CB WB
        Slot::new(38, 46),
        Slot::new(50, 44),
        Slot::new(62, 46), // 3 CM
        Slot::new(42, 74),
        Slot::new(58, 74), // front 2
    ],
};

/// 5-4-1: back five, four mids, lone striker — a low block by construction.
const SHAPE_541: Shape = Shape {
    slots: [
        GK,
        Slot::new(10, 26),
        Slot::new(30, 17),
        Slot::new(50, 16),
        Slot::new(70, 17),
        Slot::new(90, 26), // back 5
        Slot::new(20, 47),
        Slot::new(40, 45),
        Slot::new(60, 45),
        Slot::new(80, 47), // mid 4
        Slot::new(50, 76), // lone ST
    ],
};

/// Base shape for a formation before width/defensive-line instructions shift it.
fn base_shape(f: Formation) -> Shape {
    match f {
        Formation::F442 => SHAPE_442,
        Formation::F433 => SHAPE_433,
        Formation::F433Flat => SHAPE_433F,
        Formation::F4141 => SHAPE_4141,
        Formation::F4411 => SHAPE_4411,
        Formation::F442Diamond => SHAPE_442D,
        Formation::F352 => SHAPE_352,
        Formation::F352Attacking => SHAPE_352A,
        Formation::F3421 => SHAPE_3421,
        Formation::F343 => SHAPE_343,
        Formation::F4231 => SHAPE_4231,
        Formation::F532 => SHAPE_532,
        Formation::F541 => SHAPE_541,
    }
}

/// The runtime shape for a tactic: the base formation warped by the team's
/// width and defensive-line instructions. This is where instructions stop being
/// abstract multipliers and become geometry the duel engine can read.
///
/// - `width` (0..100) spreads or compresses x-coords about the centre, so a
///   narrow shape crowds the middle but leaves the flanks exposed — the trade
///   the engine then exploits rather than assumes.
/// - `defensive_line` (0..100) shifts every outfield slot's depth, so a high
///   line genuinely squeezes the pitch (more compression, more space in behind
///   for a pace runner) while a low block sits deep and concedes territory.
pub fn shape(f: Formation, instr: &TeamInstructions) -> Shape {
    let mut s = base_shape(f);
    let width_shift = (instr.width as i32 - 50) as f32 / 50.0; // -1 .. +1
    let line_shift = (instr.defensive_line as i32 - 50) as f32 / 50.0; // -1 .. +1

    for slot in s.slots.iter_mut().skip(1) {
        // Width: push wide slots further out / pull them in. Central slots are
        // anchored near 50 so a "narrow" instruction compresses the whole shape.
        let x_dev = slot.x as i32 - 50;
        let nx = 50 + (x_dev as f32 * (1.0 + 0.28 * width_shift)).round() as i32;
        slot.x = nx.clamp(2, 98) as u8;

        // Defensive line: the *whole* block moves. A high line compresses the
        // space between lines toward the opposition half; a low line sits deep.
        // We move non-GK slots but weight the shift so the back line moves most
        // (defenders set the line) and the striker least.
        let y_dev = slot.y as i32;
        let depth_weight = if y_dev < 34 {
            1.0 // defenders take the full instruction
        } else if y_dev < 63 {
            0.7 // mids follow
        } else {
            0.45 // forwards drift a little
        };
        let ny = y_dev as f32 + line_shift * 10.0 * depth_weight;
        slot.y = ny.round().clamp(2.0, 96.0) as u8;
    }
    s
}

/// The widest non-GK slot on a side — used to decide whether the flanks are
/// covered. A narrow diamond or a back three has a small widest-x, leaving the
/// channels a winger attacks.
pub fn widest_outfield(s: &Shape) -> u8 {
    s.slots
        .iter()
        .skip(1)
        .map(|sl| sl.widthness())
        .max()
        .unwrap_or(50)
}

/// Coverage the defending side offers at width `attacker_x`: the distance from
/// the widest defender's x to the attack line. A positive result = an
/// uncovered channel (the physics of "exposed flanks" falls out of geometry).
pub fn flank_coverage(def_shape: &Shape, attacker_x: u8) -> i32 {
    // Find the deepest wide-ish defender to compare against.
    let def_widest = def_shape
        .slots
        .iter()
        .skip(1)
        .filter(|sl| sl.y < 40)
        .map(|sl| sl.widthness())
        .max()
        .unwrap_or(30) as i32;
    (attacker_x as i32 - 50).abs() as i32 - def_widest
}

/// Neutral default shape (flat 4-4-2) for sides that supply no explicit shape —
/// legacy observers and the inline tests keep their old, symmetric behaviour.
pub const SHAPE_DEFAULT: Shape = SHAPE_442;

/// Whether a formation is fundamentally back-three based (informs whether
/// wing-backs or full-backs provide width, and how the engine tags roles).
pub fn is_back_three(f: Formation) -> bool {
    matches!(
        f,
        Formation::F352
            | Formation::F352Attacking
            | Formation::F3421
            | Formation::F343
            | Formation::F532
            | Formation::F541
    )
}

/// Whether a formation plays with genuine width from wide attackers/mids
/// (as opposed to relying on wing-backs or being narrow).
pub fn has_wide_attackers(f: Formation) -> bool {
    matches!(
        f,
        Formation::F442
            | Formation::F433
            | Formation::F433Flat
            | Formation::F343
            | Formation::F4411
            | Formation::F352
            | Formation::F352Attacking
            | Formation::F3421
            | Formation::F532
            | Formation::F541
            | Formation::F4141
    )
}
