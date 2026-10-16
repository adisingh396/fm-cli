//! Original transfer/contract/loan negotiation logic for fm-cli.
//!
//! State machines plus the AI accept heuristic. Pure functions where
//! possible so negotiations are unit-testable. All thresholds are our own
//! tuning, informed by observable FM/community behavior (asking above value,
//! discounts for short contracts and listed players, Bosman <6 months at 23+,
//! summer weeks 1-6 / winter weeks 20-23 on a 38-week season).

use serde::{Deserialize, Serialize};

use crate::model::{ClubId, PlayerId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OfferState {
    Draft,
    Sent,
    Countered,
    AcceptedClub,
    Rejected,
    Withdrawn,
    Expired,
    PlayerTalks,
    Completed,
    Failed,
    Active,
    Recalled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransferOffer {
    pub player_id: PlayerId,
    pub from_club: ClubId,
    pub to_club: ClubId,
    pub base_fee: i64,
    pub installments: Vec<i64>,
    pub sell_on_pct: u8,
    pub wage_offer: u32,
    pub years: u8,
    pub state: OfferState,
    pub rounds: u8,
}

impl TransferOffer {
    pub fn new(player_id: PlayerId, from_club: ClubId, to_club: ClubId) -> Self {
        TransferOffer {
            player_id,
            from_club,
            to_club,
            base_fee: 0,
            installments: Vec::new(),
            sell_on_pct: 0,
            wage_offer: 0,
            years: 3,
            state: OfferState::Draft,
            rounds: 0,
        }
    }

    pub fn total_cash(&self) -> i64 {
        self.base_fee + self.installments.iter().sum::<i64>()
    }

    pub fn send(&mut self) -> bool {
        if self.state == OfferState::Draft {
            self.state = OfferState::Sent;
            self.rounds += 1;
            true
        } else {
            false
        }
    }

    pub fn counter(&mut self, new_base: i64) -> bool {
        if matches!(
            self.state,
            OfferState::Sent | OfferState::Countered
        ) && self.rounds < Self::max_rounds()
        {
            self.base_fee = new_base;
            self.state = OfferState::Countered;
            self.rounds += 1;
            true
        } else {
            false
        }
    }

    pub fn accept_club(&mut self) -> bool {
        if matches!(self.state, OfferState::Sent | OfferState::Countered) {
            self.state = OfferState::AcceptedClub;
            true
        } else {
            false
        }
    }

    pub fn to_talks(&mut self) -> bool {
        if self.state == OfferState::AcceptedClub {
            self.state = OfferState::PlayerTalks;
            true
        } else {
            false
        }
    }

    pub fn complete(&mut self) -> bool {
        if self.state == OfferState::PlayerTalks {
            self.state = OfferState::Completed;
            true
        } else {
            false
        }
    }

    pub fn reject(&mut self) -> bool {
        if matches!(self.state, OfferState::Sent | OfferState::Countered) {
            self.state = OfferState::Rejected;
            true
        } else {
            false
        }
    }

    pub fn withdraw(&mut self) -> bool {
        if matches!(
            self.state,
            OfferState::Draft | OfferState::Sent | OfferState::Countered
        ) {
            self.state = OfferState::Withdrawn;
            true
        } else {
            false
        }
    }

    pub fn expire(&mut self) -> bool {
        if matches!(self.state, OfferState::Sent | OfferState::Countered) {
            self.state = OfferState::Expired;
            true
        } else {
            false
        }
    }

    pub fn fail(&mut self) {
        self.state = OfferState::Failed;
    }

    pub fn max_rounds() -> u8 {
        3
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiResponse {
    Accept,
    Counter(i64),
    Reject,
}

/// Asking price: premium for key players, discounts for listed players and
/// short contracts. Never below half of value.
pub fn asking_price(value: u64, key_player: bool, listed: bool, months_left: i64) -> i64 {
    let mut m: f64 = 1.3;
    if key_player {
        m += 0.4;
    }
    if listed {
        m -= 0.25;
    }
    if months_left <= 6 {
        m -= 0.45;
    } else if months_left <= 12 {
        m -= 0.2;
    }
    ((value as f64 * m.max(0.5)) as i64).max(10_000)
}

/// AI seller decision from the bid/asking ratio plus context adjustments.
pub fn ai_evaluate(
    bid_total: i64,
    asking: i64,
    months_left: i64,
    listed: bool,
    unhappy: bool,
    rivalry: bool,
) -> AiResponse {
    let mut ratio = bid_total as f64 / asking.max(1) as f64;
    if months_left <= 6 {
        ratio += 0.15;
    } else if months_left <= 12 {
        ratio += 0.07;
    }
    if listed {
        ratio += 0.10;
    }
    if unhappy {
        ratio += 0.10;
    }
    if rivalry {
        ratio -= 0.20;
    }
    if ratio >= 1.0 {
        AiResponse::Accept
    } else if ratio >= 0.7 {
        AiResponse::Counter(asking)
    } else {
        AiResponse::Reject
    }
}

/// Bosman rule (simplified): 23+ with a contract expiring this season can be
/// signed for free.
pub fn is_bosman(age: u8, expiry_season: u16, season: u16) -> bool {
    age >= 23 && expiry_season <= season
}

/// Transfer windows on a 38-week season: summer weeks 1-6, winter weeks 20-23.
pub fn window_open(week: u8) -> bool {
    (1..=6).contains(&week) || (20..=23).contains(&week)
}

/// AI loanee decision (original heuristic): accept when the fee covers roughly
/// 1% of value or the wage contribution reaches 60%.
pub fn ai_accept_loan(fee: i64, value: u64, wage_pct: u8) -> bool {
    fee >= value as i64 / 100 || wage_pct >= 60
}

/// Weekly wage split for a loan. Returns the share owed by the side in
/// question: the parent retains (100 - pct), the loanee pays pct.
pub fn loan_weekly_cost(wage_weekly: u32, wage_pct: u8, is_parent: bool) -> i64 {
    let share = if is_parent {
        100 - wage_pct.min(100) as i64
    } else {
        wage_pct.min(100) as i64
    };
    wage_weekly as i64 * share / 100
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContractOffer {
    pub player_id: PlayerId,
    pub club: ClubId,
    pub wage_weekly: u32,
    pub years: u8,
    pub signing_bonus: i64,
    pub release_clause: Option<u64>,
    pub state: OfferState,
}

impl ContractOffer {
    pub fn offer(&mut self) -> bool {
        if self.state == OfferState::Draft {
            self.state = OfferState::Sent;
            true
        } else {
            false
        }
    }

    pub fn sign(&mut self) -> bool {
        if matches!(self.state, OfferState::Sent | OfferState::Countered) {
            self.state = OfferState::Completed;
            true
        } else {
            false
        }
    }

    pub fn reject(&mut self) -> bool {
        if matches!(self.state, OfferState::Sent | OfferState::Countered) {
            self.state = OfferState::Rejected;
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoanOffer {
    pub player_id: PlayerId,
    pub parent_club: ClubId,
    pub loanee_club: ClubId,
    pub fee: i64,
    pub wage_pct: u8,
    pub months: u8,
    pub recall: bool,
    pub future_fee: Option<i64>,
    pub state: OfferState,
    /// Months remaining while Active. Counts down on the weekly tick.
    #[serde(default)]
    pub months_left: u8,
}

impl LoanOffer {
    pub fn new_loan(
        player_id: PlayerId,
        parent_club: ClubId,
        loanee_club: ClubId,
        fee: i64,
        wage_pct: u8,
        months: u8,
        recall: bool,
        future_fee: Option<i64>,
    ) -> Self {
        let months = months.clamp(1, 12);
        LoanOffer {
            player_id,
            parent_club,
            loanee_club,
            fee,
            wage_pct: wage_pct.min(100),
            months,
            recall,
            future_fee,
            state: OfferState::Draft,
            months_left: months,
        }
    }

    pub fn send(&mut self) -> bool {
        if self.state == OfferState::Draft {
            self.state = OfferState::Sent;
            true
        } else {
            false
        }
    }

    /// Advance one week. Returns true when the loan expires on this tick.
    pub fn tick_week(&mut self) -> bool {
        if self.state != OfferState::Active || self.months_left == 0 {
            return false;
        }
        self.months_left -= 1;
        self.months_left == 0
    }
    pub fn activate(&mut self) -> bool {
        if matches!(self.state, OfferState::Sent | OfferState::Countered) {
            self.state = OfferState::Active;
            true
        } else {
            false
        }
    }

    pub fn recall(&mut self) -> bool {
        if self.state == OfferState::Active && self.recall {
            self.state = OfferState::Recalled;
            true
        } else {
            false
        }
    }

    pub fn complete(&mut self) -> bool {
        if self.state == OfferState::Active {
            self.state = OfferState::Completed;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer() -> TransferOffer {
        TransferOffer::new(1, 2, 3)
    }

    #[test]
    fn bid_lifecycle_transitions() {
        let mut o = offer();
        assert!(o.send());
        assert!(!o.send()); // already sent
        assert!(o.counter(100));
        assert!(o.accept_club());
        assert!(o.to_talks());
        assert!(o.complete());
        assert_eq!(o.state, OfferState::Completed);
    }

    #[test]
    fn counter_round_cap_enforced() {
        let mut o = offer();
        assert!(o.send());
        assert!(o.counter(100));
        assert!(o.counter(110)); // rounds = 3, at cap
        assert!(!o.counter(120)); // over cap
    }

    #[test]
    fn withdraw_and_expire_paths() {
        let mut o = offer();
        assert!(o.withdraw());
        assert_eq!(o.state, OfferState::Withdrawn);
        let mut o = offer();
        assert!(o.send());
        assert!(o.expire());
        assert_eq!(o.state, OfferState::Expired);
    }

    #[test]
    fn ai_accepts_at_asking() {
        assert_eq!(ai_evaluate(100, 100, 36, false, false, false), AiResponse::Accept);
    }

    #[test]
    fn ai_counters_near_miss() {
        assert_eq!(
            ai_evaluate(80, 100, 36, false, false, false),
            AiResponse::Counter(100)
        );
    }

    #[test]
    fn ai_rejects_lowball() {
        assert_eq!(ai_evaluate(50, 100, 36, false, false, false), AiResponse::Reject);
    }

    #[test]
    fn rivalry_flips_counter_to_reject() {
        // 0.8 ratio counters normally, but -0.2 rivalry drops it below 0.7.
        assert_eq!(
            ai_evaluate(80, 100, 36, false, false, true),
            AiResponse::Reject
        );
    }

    #[test]
    fn short_contract_earns_counter() {
        // 0.65 ratio rejects normally, but +0.15 short-contract lifts it.
        assert_eq!(
            ai_evaluate(65, 100, 5, false, false, false),
            AiResponse::Counter(100)
        );
    }

    #[test]
    fn asking_price_rules() {
        let base = asking_price(1_000_000, false, false, 36);
        assert!(base > 1_000_000); // premium over value
        let listed = asking_price(1_000_000, false, true, 36);
        assert!(listed < base); // listed discount
        let expiring = asking_price(1_000_000, false, false, 4);
        assert!(expiring < base); // expiring-contract discount
    }

    #[test]
    fn bosman_rule() {
        assert!(is_bosman(25, 2024, 2024));
        assert!(!is_bosman(22, 2024, 2024)); // too young
        assert!(!is_bosman(25, 2026, 2024)); // years left
    }

    #[test]
    fn transfer_windows() {
        assert!(window_open(1) && window_open(6));
        assert!(window_open(20) && window_open(23));
        assert!(!window_open(7) && !window_open(19) && !window_open(24));
    }

    #[test]
    fn loan_recall_needs_clause() {
        let mut l = LoanOffer {
            player_id: 1,
            parent_club: 2,
            loanee_club: 3,
            fee: 0,
            wage_pct: 50,
            months: 6,
            recall: false,
            future_fee: None,
            state: OfferState::Sent,
            months_left: 6,
        };
        assert!(l.activate());
        assert!(!l.recall()); // no clause
        l.recall = true;
        assert!(l.recall());
    }

    #[test]
    fn loan_tick_expires() {
        let mut l = LoanOffer::new_loan(1, 2, 3, 0, 50, 2, true, None);
        assert!(l.send());
        assert!(l.activate());
        assert!(!l.tick_week());
        assert_eq!(l.months_left, 1);
        assert!(l.tick_week());
        assert!(l.complete());
    }

    #[test]
    fn loan_accept_and_cost_rules() {
        assert!(ai_accept_loan(20_000, 1_000_000, 0)); // fee covers 1%
        assert!(ai_accept_loan(0, 1_000_000, 60)); // wage share reaches 60%
        assert!(!ai_accept_loan(0, 1_000_000, 40));
        assert_eq!(loan_weekly_cost(10_000, 70, true), 3_000); // parent retains 30%
        assert_eq!(loan_weekly_cost(10_000, 70, false), 7_000); // loanee pays 70%
    }
}
