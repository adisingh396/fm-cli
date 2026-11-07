# Implementation log — Realism & World Expansion (Tracks 1–5, 2026-11-08)

All tracks implemented, `cargo build --release` green, all 99 tests pass.
End-to-end proof: a 1-season headless sim produced Chelsea (92), Real Madrid
(101), Bayern (88), PSG (76); UCL won by Juventus with Kane top scorer;
Ballon d'Or Yan Diomande; real non-Big-5 European clubs (PSV with Dest's
passing stat line, Galatasaray with Osimhen) fielded in Europe.

## T1/T2 — Data layer + traits (`model.rs`, `traits.rs`, `data.rs`, `matchsim.rs`)
- `Player` gains `foot: Foot`, `weak_foot: u8`, `skill_moves: u8`,
  `traits: Vec<Trait>`, `height_cm: u8` (all `#[serde(default)]`).
- `traits.rs`: 24-variant `Trait` enum, tolerant `parse_playstyles`,
  `has()`, `label()`. Parse is case/punct/`+`-tolerant; unknowns skipped.
- `data.rs` parses preferred_foot(11)/weak_foot(13)/skill_moves(12)/playstyles(17).
- Engine hooks: finish +4% (Finesse/LowDriven/Gamechanger), +3% Acrobatic;
  creation +5% (IncisivePass/Inventive); passing +3% TikiTaka; dribble +5%
  (Rapid/QuickStep/Trickster); aerial +5% (PowerHeader/PrecisionHeader);
  gk +4% (KeeperSweep/KeeperRush). Weak-foot: ~25% of shots (wingers cutting
  in always) get conversion ×(0.55 + 0.09×weak_foot) — documented
  approximation.

## T1/T5 — World registry (`world_model.rs`)
- `Confederation`, `LeagueDef` (id/name/nation/confed/tier/coeff),
  `NationDef`, `WorldLeagues` + `catalogue()`, `normalize_league()`
  (mojibake handled), `lookup()`, `confed_of()`, `nations()`,
  `build_world_leagues()`, `CONTINENTAL_CUPS`.
- ~49 men's leagues catalogued vs the CSV's 164 nationalities.
  UEFA coefficient tiers incl. big-5 ~1.00–0.86, NED 0.66, POR 0.62.

## T3 — Spatial formation physics (`model.rs`, `tactics.rs`, `matchsim.rs`)
- `Formation` expanded: 13 variants (F442, F433, F433Flat, F4141, F4411,
  F442Diamond, F352, F352Attacking, F3421, F343, F4231, F532, F541).
- `tactics.rs`: `Slot`/`Shape` (100×100 pitch coords), 13 reviewable shape
  tables, `phase_weight`, `zone_of`, `BUILDUP_Y/PROGRESSION_Y/FINAL_THIRD_Y`,
  `shape()` warps base by width + defensive-line (defenders move most),
  `flank_coverage`, `widest_outfield`, `is_back_three`, `has_wide_attackers`.
- `matchsim`: `Side` gains `shape: Option<Shape>`, `width`, `defensive_line`;
  `Rt` stores per-player `slot` + full `shape`; `phase_group` scales
  participant weights by slot-depth proximity to the phase band (build-up from
  the deep zone, etc.). Flank physics: wide carrier vs uncovered channel
  (geometrically measured) gets up to +18% duel edge. High line: defensive-
  line dial amplifies pace edge on progression duels. `season.rs` maps AI
  `(d,m,f)` + ambition → `tactics::Shape` via `team_shape`/`def_line`.

## T4 — Match deepening (`matchsim.rs`, `observe.rs`)
- Substitutions: `Side.bench`, 5 max, checked ~every 9 drives. Triggers:
  exhaustion (spent starter → fresher same-line bench), chasing loss (best
  attacker on late), cover a man down. Subbed-on player gets a fresh
  `MatchStats` with his id (bug fixed); departed lines merged into final
  stats so goals/minutes survive. →
- Cards: `MatchStats.yellows`+`sent_off`, `EventKind::RedCard`. Booking
  prob from challenge intensity vs composure; second yellow or DOGSO
  last-man foul = sending off; ten men cover (team-wide fatigue drag).
- Injury time: `added_minutes` from fouls/injuries/subs (2–7 min), played out
  with mentality read from the current scoreline.
- Crowd: `Side.crowd` scales the home bias (1.0 default).
- All 18 `matchsim` tests still pass.

## Career mode (`main.rs`)
- `simulate_match` now runs the **full possession engine** instead of the
  flat Poisson score: `build_career_side` honours the manager's saved XI +
  bench of 7 best remaining; formation geometry from the saved tactic;
  ambition from the saved mentality; difficulty/team-talk scale quality.
  Events name scorers/assistants/injuries/sendings-off; `apply_career_stats`
  writes season stats/cards/suspensions/condition/form/injuries off the real
  per-player lines; suspended non-players still serve.

## T5 — Real European outsiders (`season.rs`)
- `build_world` loads the top 8 UEFA tier-1 (non-Big-5) clubs by reputation
  with real database squads (dedupe pre-pass + team build both include them),
  assigned `league_idx: 5` so they contest the UCL/Europa and enter the
  transfer market. The 8 procedural fillers remain only as a fallback if the
  database has no real outsiders.

## Not done (ROADMAP tracks deferred)
- T5b: full multi-tier league simulation (promotion/relegation across
  England's 4 tiers Spain/Germany/Italy/France's 2–3 tiers) and extra
  continental cups — the registry now exists to build on.
- T6: living calendar, manager career moves, contract/media depth. The
  career loop is weekly and single-league; expanding it is the natural next
  project.

## Verification
- `cargo build --release` — exit 0.
- `cargo test --release` — 99/99 passed.
- Headless season sim (menu 3 → 1 season) — report sane, generated
  `season_1yr.md`.
