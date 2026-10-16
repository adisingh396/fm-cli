//! Original player-development mathematics for fm-cli.
//!
//! Weekly growth ticks, training injury risk, and mentoring shifts. Pure
//! functions so the numbers are unit-testable. All formulas are our own,
//! informed by community-observed behavior: fastest growth in the teens,
//! peak mid-20s, physical decline from 30+, minutes/focus/personality/
//! morale/facilities as growth inputs, load-driven injuries.

/// Growth multiplier by age. Positive = growth, negative = decline.
pub fn age_growth_mult(age: u8) -> f32 {
    match age {
        0..=15 => 1.6,
        16..=18 => 1.4,
        19..=21 => 1.2,
        22..=23 => 1.0,
        24..=27 => 0.6,
        28..=29 => 0.2,
        30..=31 => -0.3,
        32..=33 => -0.6,
        _ => -1.0,
    }
}

/// Weekly attribute-point delta. Caps at +/-2 so one week never transforms
/// a player. `minutes_frac` 0..=1 is share of available minutes played.
#[allow(clippy::too_many_arguments)]
pub fn weekly_gain(
    base: f32,
    age_mult: f32,
    minutes_frac: f32,
    focus_match: bool,
    personality_mult: f32,
    morale_mult: f32,
    facility_mult: f32,
    injured: bool,
) -> f32 {
    if injured {
        return 0.0;
    }
    let focus = if focus_match { 1.5 } else { 0.5 };
    (base
        * age_mult
        * minutes_frac.clamp(0.0, 1.0)
        * focus
        * personality_mult
        * morale_mult
        * facility_mult)
        .clamp(-2.0, 2.0)
}

/// Training injury probability for one session. `intensity` 0 = light,
/// 1 = normal, 2 = heavy. Higher physio quality (0-20) mitigates.
pub fn training_injury_prob(intensity: u8, condition_avg: f32, physio: u8) -> f64 {
    let mut p = 0.01 * (1.0 + intensity.min(2) as f64);
    if condition_avg < 80.0 {
        p *= 2.0;
    }
    p *= 1.0 - (physio.min(20) as f64 * 0.02);
    p.clamp(0.0, 0.3)
}

/// Weekly mentoring shift of a mentee driver toward the mentor's value.
/// `influence` 0..=1 is the mentor's squad standing; capped at a year.
pub fn mentor_shift(mentor_val: f32, mentee_val: f32, influence: f32, weeks: u32) -> f32 {
    (mentor_val - mentee_val) * 0.05 * influence.clamp(0.0, 1.0) * weeks.min(52) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_curve_shape() {
        assert!(age_growth_mult(17) > age_growth_mult(22));
        assert!(age_growth_mult(22) > age_growth_mult(26));
        assert!(age_growth_mult(29) > 0.0);
        assert!(age_growth_mult(31) < 0.0);
        assert!(age_growth_mult(36) < age_growth_mult(31));
    }

    #[test]
    fn injured_players_do_not_gain() {
        assert_eq!(weekly_gain(1.0, 1.4, 1.0, true, 1.1, 1.0, 1.0, true), 0.0);
    }

    #[test]
    fn gain_caps_and_scales() {
        let g = weekly_gain(1.0, 1.4, 1.0, true, 1.1, 1.0, 1.0, false);
        assert!(g > 0.0 && g <= 2.0);
        let idle = weekly_gain(1.0, 1.4, 0.0, true, 1.1, 1.0, 1.0, false);
        assert_eq!(idle, 0.0);
        let decline = weekly_gain(1.0, -1.0, 1.0, true, 1.0, 1.0, 1.0, false);
        assert!(decline < 0.0);
    }

    #[test]
    fn training_risk_monotonic() {
        let light = training_injury_prob(0, 95.0, 15);
        let heavy = training_injury_prob(2, 95.0, 15);
        assert!(heavy > light);
        let tired = training_injury_prob(1, 70.0, 15);
        let fresh = training_injury_prob(1, 95.0, 15);
        assert!(tired > fresh);
        let good_physio = training_injury_prob(1, 95.0, 20);
        assert!(good_physio < fresh);
    }

    #[test]
    fn mentor_shift_direction() {
        let up = mentor_shift(18.0, 10.0, 1.0, 10);
        assert!(up > 0.0);
        let down = mentor_shift(8.0, 14.0, 1.0, 10);
        assert!(down < 0.0);
        assert_eq!(mentor_shift(18.0, 10.0, 0.0, 10), 0.0);
    }
}
