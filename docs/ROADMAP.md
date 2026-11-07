# FM-CLI — Realism & World Expansion Roadmap

Master plan authored by the lead (Opus). Workers execute the individual tracks.
Tracks are ordered by architectural dependency: data → traits → physics →
world → tactics → manager depth. Each track lands as a self-contained, compiling
increment so `cargo build --release` stays green between merges.

## Current state (baseline)

- ~16.6k LOC Rust. Possession-engine `matchsim.rs` (22 individuals, ~190
  possessions, emergent assists/shots/injuries). Big-5 + UCL + Europa + cups +
  World Cup/Euro. Evolutionary tuner.
- `data/players.csv` (fc27): **19,790 rows, 164 nations, ~40 real leagues**
  (England 4 tiers, Spain/Germany/Italy/France 2-3 tiers, Eredivisie, Liga
  Portugal, Belgian, Turkish, Scottish, Saudi, MLS, Argentine, Mexican, Korean,
  Chinese, Austrian, Swiss, Irish, Scandinavian, Brazilian…). Columns include
  `league`, `playstyles` (trait list!), `preferred_foot`, `weak_foot`,
  `skill_moves`, `height_cm`, `weight_kg`, `nationality`, `playstyles+`.
- **Gaps**: only Big-5 leagues simulated; playstyles/foot/height/weak-foot/
  skill-moves ignored; formations are 5 line-count presets with no spatial
  geometry; no player traits; no substitutions; no cards-to-suspension depth in
  sim; manager mode is weekly not a living calendar; no continental competition
  beyond UCL/EL.

## Design principles (non-negotiable)

1. **Physics, not dice.** Outcomes emerge from per-duel attribute contests
   weighted by *where* the player is on the pitch and *what* the shape asks of
   him — never flat probabilities or reputation.
2. **Data over constants.** Real leagues/nations/players come from the CSV;
   procedural regen fills the long tail. No hardcoded "these clubs are big".
3. **Emergence.** A narrow 3-5-2 losing the wings, a high line beaten by pace,
   a false-nine pulling a CB out — these must *fall out* of shape+attribute
   geometry, not be scripted.
4. **Save compat.** All new model fields `#[serde(default)]`; old savegame.json
   must still load.

## Tracks

### T1 — Data layer: world database (foundation)
Extend `Database` to expose every league + nation present in the CSV, plus the
rich per-player fields we're currently discarding.
- `data.rs`: parse `preferred_foot`, `weak_foot` (1-5), `skill_moves` (1-5),
  `height_cm`, `weight_kg`, `playstyles` → new `Player` fields
  (`foot: Foot`, `weak_foot: u8`, `skill_moves: u8`, `height_cm: u8`,
  `weight_kg: u8`, `traits: Vec<Trait>`). All `#[serde(default)]`.
- `leagues.rs` (new): a `LeagueDef` registry built from the CSV — id, name,
  nation, tier, confederation (UEFA/CONMEBOL/CONCACAF/AFC/CAF/OFC), coeff.
  UEFA coefficient per nation seeds European qualification.
- `nations.rs` (new): `Nation` registry from the 164 nationalities + a
  confederation map + FIFA-style ranking derived from top-player concentration.

### T2 — Player traits & body profile (realism core)
`traits.rs` (new): map fc27 `playstyles` strings to an internal `Trait` enum
(FinesseShot, PowerHeader, Rapid, TikiTaka, LongBallPass, IncisivePass,
Acrobatic, Gamechanger, Inventive, Technical, FirstTouch, QuickStep, LowDriven,
DeadBall, Trivela/OutsideFoot, Block, Anticipate, Bruiser, Keeper*…).
- Traits feed `matchsim`: FinesseShot/Acrobatic/Gamechanger raise shot xG→goal;
  IncisivePass/Inventive raise key-pass→big-chance; Rapid/QuickStep win
  offside-line foot races; DeadBall boosts set-piece xG; Anticipate/Block boost
  interceptions; Bruiser raises duel intensity + foul risk.
- `height_cm`/`weight_kg` strengthen the existing aerial contest; `preferred_foot`
  + `weak_foot` penalise finishing/crossing on the wrong side (physics hook for T3).

### T3 — Spatial formation & tactic physics (the headline ask)
Replace flat `Formation` line counts with a **pitch-grid model**.
- `tactics.rs` (new): every formation is 11 slot coordinates on a
  (0..=100 width) × (0..=100 depth) grid + a per-slot role. Ship ~14 formations:
  4-4-2, 4-4-2-diamond, 4-2-4, 4-3-3 (holding/flat/attack variants), 4-2-3-1,
  4-1-4-1, 4-5-1, 4-2-2-2, 3-5-2, 3-4-2-1, 3-4-3, 5-3-2, 5-4-1. Each maps to
  existing `Position`/`Role` so the lineup editor keeps working.
- Feed `matchsim` a **spatial weight matrix**: build-up draws from the back-line
  + deep mids; progression from mids + advancing fullbacks; final third from
  forwards + arriving runners. Width dial shifts the effective x-coords
  (narrow → central overloads but exposed flanks; wide → crosses but stretched
  middle). Defensive_line + pressing set the *zone* where defensive duels are
  contested (high line → defenders' pace matters; low block → heading/strength).
- **Matchups**: a side's wide attackers attack the space the opponent's shape
  leaves. 3-at-the-back with wingbacks pinned = 5-v-3 wide overloads become
  real. Inverted fullbacks add midfield control. This is all geometry-driven.

### T4 — Match engine deepening (realism)
- **Substitutions**: bench of 7, AI subs on fatigue threshold / chasing a
  result / injury; user can pick subs in Match Centre. Fresh legs lower fatigue.
- **Cards & suspensions**: fouls accrue yellows (tackle intensity + Temperamental
  personality + derby flag); 2nd yellow = red → team plays a man down (players
  array already supports `out[]`), red carries a suspension into next match.
- **Injury time**: ~+2..=5 min per half weighted by time_wasting + injuries.
- **Extra time & penalties** for knockouts (cup.rs already has shootout — wire
  it into the sim for ties that need a winner).
- **Conditions**: home crowd (attendance vs capacity) lifts home side; big-match
  composure matters more on the biggest stage.

### T5 — World expansion (European importance)
- `world.rs`/`season.rs`: every UEFA league from the CSV runs its own table +
  promotion/relegation where ≥2 tiers exist (England 4→champ playoff, Spain/
  Italy/Germany/France 2-tier). UCL qualification = league position × national
  coefficient (top-4 leagues send 4, mid send 2, low send 1 + qualifiers).
- Add **Europa League** + **Conference League** as a third tier; non-Big-5
  European clubs (Porto, Ajax, Celtic, Galatasaray, Benfica…) populate properly
  from the CSV instead of a hardcoded outsider list.
- CONMEBOL: Libertadores + Sudamericana for South American leagues present
  (Argentina, Brazil, etc.). Keep MLS/LigaMX/Saudi/AFC as parallel regions with
  their own cups; they don't feed UCL but exist in the world + Club World Cup.
- **Promotion/relegation** + continental movement so the world churns.

### T6 — Managerial depth (the "career" ask)
- `calendar.rs`: real season calendar — league rounds, midweek Europe, cup ties,
  international breaks, winter breaks (Bundesliga), transfer windows.
- **Manager career**: reputation, tactical philosophy (preferred shape + style
  ratings), job moves between clubs, sackings, national-team offers. Player-
  manager relationships and dressing-room morale chemistry.
- **Contract/squad depth**: contract talks, promises, agent demands, wage
  structure friction; youth promotion; pre-season; dynamic transfer sagas.
- **Media/inbox**: reaction to results, board meetings, transfer rumours,
  milestone alerts — the stuff that makes a save feel alive.

## Execution model

- Opus (me): architecture, interfaces, cross-cutting review, the physics/tuning
  judgement calls.
- Workers: one track (or sub-track) each, self-contained prompt, writes
  `.agent-notes/<track>.md`, keeps `cargo build --release` green.
- `season.rs` is 6k LOC — world expansion (T5) is the biggest job and may need
  splitting into T5a (league registry + extra UEFA leagues) and T5b (continental
  cups + promotion/relegation wiring).
