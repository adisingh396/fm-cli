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

## T5 — Real European outsiders (`season.rs`)- `build_world` loads the top 8 UEFA tier-1 (non-Big-5) clubs by reputation
  with real database squads (dedupe pre-pass + team build both include them),
  assigned `league_idx: 5` so they contest the UCL/Europa and enter the
  transfer market. The 8 procedural fillers remain only as a fallback if the
  database has no real outsiders.

## T5b — Non-UEFA world + Club World Cup + Ballon bias (`season.rs`, `world_model.rs`)
- `build_world` now loads every other tier-1 men's league in the database as a
  *simulated* domestic league: AFC (ISL, Saudi, K League, CSL, A-League),
  CONMEBOL (Liga Profesional + the pooled `CONMEBOL Libertadores` clubs — the
  dump files non-Argentine SA clubs under the competition label, including
  Peñarol, Nacional, Sporting Cristal, Cerro Porteño, LDU Quito…), CONCACAF
  (MLS, Liga MX). Registered in `world_model.rs` as a CONMEBOL top flight.
- Each extra league gets `LeagueMeta` (label + confederation), its own table,
  champions, domestic cup, and a **confederation cup** (AFC CL, CONCACAF
  Champions Cup, CONMEBOL Libertadores) from the top-4 finishers per league.
- **Ballon d'Or league-prestige bias**: `ballon_breakdown` takes a
  `league_factor` (Big-5 1.00, UEFA outsiders 0.90, CONMEBOL 0.84, CONCACAF
  0.78, AFC 0.72) and ends with a printed bias line (e.g. −85 pts) so the
  totals reconcile. Genuine outsider winners remain reachable but only via
  undeniable numbers — Driussi won the 2028 Ballon from River Plate through
  the 0.84 discount; the Saudi-era stars (Oh, Bardghji) still barely survive
  it via historic goal volumes.
- **FIFA Club World Cup** (2029, 2033, every 4y): qualification from
  `World::continent_hist` (each season's UCL/AFC/CONCACAF/CONMEBOL winners).
  UEFA 12, CONMEBOL 6, AFC 4, CONCACAF 4, OFC 1 (Auckland FC) + rotating host
  (MLS/Liga MX of the year) = 28 teams. Continental champions qualify
  directly; the ladder fills on European silverware then strength; cap is
  2 per `Team.nation` unless that country produced 3+ continental champions
  in the cycle. Verified winners: Real Madrid (2029), Spurs (2031-ish 2033).

## FIFA World Cup 2026 (48-team structure) + UK-style expectations (`season.rs`)
- The World Cup is now the real format: **48 teams** qualified via the 2026
  allocation (hosts USA/CAN/MEX, UEFA 16, CAF 9, AFC 8, CONMEBOL 6,
  CONCACAF 3 more, OFC 1, intercontinental playoffs 2) — `qualify_wc_field`.
- **12 groups of 4**, pot-seeded and confederation-balanced (≤2 UEFA per
  group, ≤1 others where the field allows), top-2 + best-8 thirds → 32.
- Knockout: **Round of 32 → R16 → QF → SF → Final** with a third-place
  playoff — the champion plays exactly 8 matches (3 group + 5 knockout).
- Tie-breakers: points → GD → GF → nation strength (FIFA ranking analogue);
  head-to-head and fair play folded in via goals/strength, documented.
- Bracket is seeded properly (1v32…), and national-team matches sharpen the
  threat gap (×1.5) since pooled national XI quality is compressed in the
  dump — favorites finally behave like favorites.
- Verified decade: World Cup winners England (2026, 2030), France (2034);
  England also won both Euros of the decade; no more mid-tier-heavy champions.
- The European Championship (24) and AFC Asian Cup (6 AFC nations) reuse the
  same group+seeded-knockout machinery.
- **Actual overall for national teams**: `Nation.strength` is the best-XI
  actual overall (mean of the eleven strongest FC overalls) — no blended
  "adjusted" measures — and `nation_xi` now picks the actual highest-overall
  players per line (role-fit only ties). Measured run: WC winners
  Belgium/Argentina/Denmark (strong natural squads, not flukes), Euro kings
  Denmark, and India winning the 2035 Asian Cup as a legit regional favorite.
- **AFC Asian Cup** (2027, 2031, 2035): `IntlKind::AsianCup`, `build_nations`
  filters `world_model::confed_of(nation) == AFC`. `run_international`
  threshold lowered to 4 (only 6 AFC nations clear the 15-player pool filter:
  Japan, Korea, Saudi, Australia, China, India).
- **Friendlies**: every non-World-Cup year a one-round friendly window over all
  built nations (16 pairs) so every outsider-league footballer gets real caps
  and goals — feeding Ballon international components.
- `Team` gained `nation: String` (for the CWC cap); `World` gained
  `league_meta: Vec<LeagueMeta>` and `continent_hist`.

## Environment-first development (the India fix)
- Root cause of "India won the 2034 WC": `develop_player` gave every ≤21yo
  1–3 growth steps/yr everywhere, and regen ceilings were global
  (`w.elite_cap`), so weak leagues generated 90-ceiling wonderkids and grew
  them at full pace — 8 seasons to world-class.
- Fix: each youth now develops under his club's academy tier (1–5).
  `env_cap` walls growth at 62 (1★), 75 (2★), 82 (3★), 88 (4★), 95 (5★);
  `env_mult` scales steps at 0.25/0.45/0.65/0.85/1.0. At the wall, growth
  nearly stops (15% of one step) — only transferring up (the real-world
  pipeline) raises it further. Same cap in the 22–25 branch. Regen ceilings
  are `min(rolled, env_cap(academy))`, so a wonderkid at a weak club tops
  out at the local ceiling (the country's future star, not the next Messi);
  `elite_cap` only ever shows up at academy 4–5.
- Consequence: the revolutionary leap is now what it should be — nearly
  unattainable without decades of academy investment. Verification sim:
  ISL clubs still rule their own league (Mohun Bagan 5 titles) but their
  best XI tops ~75 and no longer contests a World Cup; new world-class
  players (Kaiser-class regens) emerge only from academy 4–5 clubs.

## The 90 wall (earned elite progression)
- Pushing past overall 90 is *astronomical* — development past 90 only unlocks
  when the player **proves** it that fresh season via `proof_to_push_past_90`:
  real production (20+ goal involvements, or an 8.15+ rating over 25+ apps)
  **and** deep big-stage success (UCL/EL semi-final or better, international
  semi, or a league title plus European knockout run). Without proof the wall
  is absolute even in a 5★ academy; with proof, at most ONE growth step per
  season (~90→94 needs four proven seasons, if the pot cooperates). Applies to
  both the ≤21 growth phase and the 22–25 mid-twenties phase.
  Career check after the wall: World Cup winners Argentina/Turkey/Greece/
  Senegal; Ballons only to elite names; Cherki's back-to-back wins from
  River Plate are the blast-through case the bias factor allows.

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
