# Football Manager CLI ⚽

A fast, robust, end-to-end command-line football management game written in Rust,
built on the **Football Manager 2023** data schema.

It probes and loads two real datasets — the FM 2023 database (~8,400 players)
from `data/fm2023.csv`
([source](https://github.com/dannytharris/FM)) and the FC 26 ratings set
(~18,400 men, 110 attributes) from `data/fc26.csv`
([source](https://github.com/ismailoksuz/EAFC26-DataHub)) — 26,800+ players
total, merged into one pool with club squads drawn from real rosters and a
seeded free-agent market. No FC 27 bulk dump exists publicly; FC 26 is the
latest downloadable set. Falls back to procedural generation when a CSV is
absent. Drop a newer same-schema dump at `data/fc.csv` to upgrade ratings.

## Features

- **New Game** — Pick a real league (La Liga, Premier League, Serie A, MLS…), then a club within it; opponents are that division only
- **Match Centre** — Team talks, per-shot xG engine (goals track chance quality), 7 mentalities, instructions, roles/duties, minute events
- **Starting XI & Positions** — Pick your eleven and field any player anywhere:
  drop a striker into a False Nine, push a midfielder to centre-back, invert a
  full-back. Each slot shows a live role-fit rating (per-attribute, with an
  out-of-position penalty) so the trade-off is visible, and the choice feeds
  straight into match strength. Auto-pick fills the best-fitting XI.
- **Squad Management** — View squads, inspect details (condition/sharpness/cards/injury, best-fit roles), sell players
- **Transfer Market** — Buy players from the FM 2023 database using real market values
- **Loans** — Loan out/in with fees, wage split, monthly countdown, recall clauses, buy options
- **Training** — Improve player attributes (per-attribute or general sessions)
- **Scouting** — Search the database for talent, filter by position
- **Finances** — Transfer/wage budgets, wage bill, matchday/sponsorship/TV revenue
- **League Table** — Proper double round-robin standings (20 clubs)
- **Club & Board** — Board expectation/confidence, fan confidence, facilities, tickets
- **Staff** — Hire/release coaches, scouts, physios, analysts (quality-gated bonuses)
- **Medical Centre** — Injuries, suspensions, condition; rest and physio recovery
- **Youth Intake** — Annual newgen intake scaled by youth facilities, plus season awards and rollover (aging, contracts, retirements)
- **News & Events** — Dynamic match reports (with xG) and transfer news
- **Advance Day** — Daily parallel world lap: every club recovers, youths grow,
  veterans decline, AI clubs sign/release with a digest, deadline-day surge
- **Observe / Spectate** — Watch any match unfold minute by minute (goals with
  scorer + assister, injuries, and managers shifting mentality live as the
  scoreline changes), then read full per-player rating sheets. Or spectate a
  whole league season and browse the final table, top scorers/assisters, top
  ratings, chances created, and any club's full squad stat lines.
- **Simulate Season(s) (sandbox)** — Headless sim of the Big 5 leagues + Champions
  League + Europa League + domestic cups + international tournaments (World Cup every
  4 years, Euros in between), for one season (detailed) or up to 100 years (dynasty).
  Every club match is played by a **possession engine** (`matchsim`): ~190 possessions
  where 22 individuals contest build-up, progression and the final third from their own
  attributes, so passes, dribbles, key passes, big chances, shots, tackles, saves,
  goals, assists, ratings and even injuries all emerge from real play (no flat
  per-event chances). Shot-taking follows role tendency (finishers hunt, holders
  recycle) with a hot hand when a man scores and defensive attention when he shoots
  all game; chance quality emerges from the creative duel. Corners and dangerous
  free kicks are full aerial contests decided by heading × strength, and deep-sitting
  sides genuinely double up while gung-ho sides overload. Managers read the scoreline
  — including a two-legged tie's aggregate — and shift mentality mid-match, and the
  opponent reacts. The UCL runs the real Swiss format (8 games, top 16 through,
  single-leg neutral final) with outsiders (Porto, Ajax, Celtic…) in the field.
  Each nation also contests a **domestic super cup** curtain-raiser: Spain and Italy
  as the modern Final Four (champion, league runner-up, cup winner and cup runner-up
  across two semis and a final), England, Germany and France as a single champion-vs-
  cup-winner match, with the Double winner meeting the league runner-up. International
  **assists and caps count everywhere** — in the all-competition tallies, the Ballon
  d'Or (its own assist line plus an all-comps appearances count), and player
  development, so a season carrying club and country improves a youngster, not just
  his league games.
  Players develop/decline per-attribute with age (pace fades first, technique lingers),
  retire, and are replaced through a real **academy pipeline**: academy quality (1–5,
  built slowly from revenue) drives intake strength and wonderkid odds, teenagers
  grow on minutes under trusting managers, and generational careers emerge organically.
  Defenders are generated as **centre-backs or full-backs/wing-backs** (pacey,
  high-stamina, crossing types), so the stat tables aren't wall-to-wall CBs, and the
  **best-defenders board ranks on quality** — clean sheets kept at strong defences,
  average rating, and duels per game — so elite-club defenders lead it, not whoever
  racked up the most tackles battling relegation.
  The **club-finance loop** keeps money honest: slow-moving wealth earns revenue,
  revenue plus league/European prize money flows into cash, and each summer's budget
  is revenue × board willingness plus carried cash — never reputation alone — with a
  revenue-derived wage cap (UEFA-style 70%), agent fees, multi-year contracts with
  market-repriced release clauses, sell-to-buy, and talent cascading down. All money
  is nominal and tracks football inflation (revenue, wages, fees inflate together,
  reports deflate to 2026 money), so late-era billions never parade as real wealth;
  insolvent (cash-negative) clubs get zero allocation and run sell-to-buy only, so
  debt cannot finance galácticos. Boards must also win leagues: a stature-85+
  club six-plus years without its league title starts rolling the axe every season,
  cups included — no half-century of cup runs survives. Managers are **individuals
  with a lifecycle**: each carries an age and retires through their 60s (hard cap at
  72), boards lose patience at random regardless of results (the De Laurentiis
  factor, every club big or small), and a trophyless league drought is measured
  against the *current* manager's tenure — so there are no 40- or 50-season reigns and
  no club's whole history pinned on a man who just arrived. Clubs mean-revert toward
  their own **heritage** (starting stature), not a flat average, and a brand revenue
  floor keeps a fallen giant (a Man United, an Arsenal) solvent enough to rebuild —
  so historic clubs aren't shut out for 50 years, while a club with no history can
  enjoy an era but not reign forever. The **Ballon d'Or** is voted, not computed:
  on the on-pitch maths sit a media/reputation bias (coverage follows the badge), a
  per-player narrative swing, and small all-round credits — ball-winning, clean sheets,
  build-up passing and carrying, plus a clean-sweep (league + UCL) and an underdog-run
  bonus — weighted so a prolific forward still usually wins, but a monumental holding
  midfielder (a Rodri/Modric treble season) or a title-winning defender can break
  through, and a lower-rep club's hero on a deep European run is rewarded over a
  big-club bias. Scoring itself favours **agile forwards**: most generated strikers are
  inside-forward/false-nine/poacher types (target men are the minority), and set-piece
  deliveries look for a goal threat by heading *and* finishing — so corners no longer
  funnel every goal to one 6'4" target man and the scoring charts read like modern
  football. A **marked-man brake** keeps the scoring charts honest: once a player
  passes ~22 goals the whole league keys on him — his form dips as defenders swarm
  the hot scorer, volume shooting tires his legs within a match, and open-play
  conversion is capped — so freak 60-goal seasons can't happen while a normal 25-35
  goal campaign is barely touched. Career ball-winning shows
  apps and per-game rate with the siege effect stated (relegation-battlers tackle
  more, as in reality), so longevity never reads as godliness. Regen **names** are
  culture-aware and procedurally expanded, with the most recognisable real-superstar
  surnames curated out and native-script duplicates stripped from source data, so no
  regen is born "Luka Gvardiol" or "Milos Milos Popovic". Season-long
  loans develop prospects on smaller clubs' minutes (fees, split wages, summer
  returns), and 25-man squad registration with a club-trained floor governs who may
  actually play. Each club
  has its own manager profile (rotation tendency + ambition), so squads are rotated
  across the season (but the talisman always plays, and full-strength sides turn up
  in Europe) and hundreds of AI managers behave individually. Reports (separate
  `season_1yr.md` / `season_10yr.md`) carry top-10s per department with positions,
  a UCL scoring chart with appearances, academy watch, club finance books, anomaly
  breakdowns, decade legends and trend narratives, Ballon d'Or breakdowns, and
  auto-detected realism glitches.
- **Train AI (evolutionary calibration)** — Learns simulation parameters (including
  the transfer policy network weights) from real-world targets: champion points,
  golden boots, elite UCL winners, Ballon d'Or profiles, transfer volume/fees.
  Saves the winner to `tuned_params.json`, which all future sims load automatically.
- **Save/Load** — Persistent JSON game state (`savegame.json`)

## Data Model

The starting world is loaded from a single **2026 players database** (an EA FC-style
dump, men's football only) at `data/players.csv`. Each row's current ratings, position
and birthdate are the player's starting stats, position and age; club reputation is
derived from squad quality (the strongest squads are the biggest clubs — no per-club
constant is hardcoded). The detailed FC attributes map onto the internal model as:

| CSV field(s) | Internal attribute |
|---|---|
| movement_acceleration, movement_sprint_speed | pace |
| attacking_finishing, power_shot_power, power_long_shots, attacking_volleys | shooting |
| attacking_short_passing, skill_long_passing, mentality_vision, attacking_crossing | passing |
| skill_dribbling, skill_ball_control, movement_agility, movement_balance | dribbling |
| defending_standing_tackle, defending_sliding_tackle, mentality_interceptions, defending_awareness | tackling |
| attacking_heading_accuracy | heading |
| power_strength | strength |
| power_stamina | stamina |
| mentality_vision | vision |
| mentality_composure | composure |
| goalkeeping_diving/handling/kicking/positioning/reflexes | goalkeeping |
| skill_ball_control | first_touch |
| overall_rating | current overall (potential = overall + youth headroom, capped at 95) |
| birthdate | age at the 2026 snapshot |
| position / alternate_positions | role bucket (GK/DEF/MID/FWD) |
| league | Big-5 division (FC labels normalised to Premier League / La Liga / Serie A / Bundesliga / Ligue 1) |

See `docs/FM2023_PROBE.md` for the earlier dataset probe report.

## Build & Run

This project requires a working Rust linker. On Windows without MSVC build tools,
use the GNU toolchain:

```sh
# Place the 2026 players database at data/players.csv
# (procedural generation is the fallback if it's missing)

cargo +stable-x86_64-pc-windows-gnu run --release
```

On Linux/macOS or Windows-MSVC:

```sh
cargo run --release
```

## Architecture

```
src/
  main.rs     — CLI game loop, all screens, interactive match
  model.rs    — Core domain: Player, Club, Attributes, Formation, Tactic, Fixture
  matchsim.rs — Possession-phase match engine (22 individuals, per-player stats)
  observe.rs  — Spectator mode: live match ticker + league-season browser
  season.rs   — Season/dynasty sandbox (drives matches through matchsim)
  gen.rs      — Procedural world generation (names, squads, fallback data)
  data.rs     — CSV loader (BOM-aware, quote-aware parser) + procedural fallback
  cup.rs      — Domestic cups, European competitions, super cups
  engine.rs   — Shared simulation engine helpers
  tune.rs     — Evolutionary calibration of sim parameters
  deals.rs    — Transfers, loans, contracts
  managers.rs — AI manager lifecycle (hire/sack/retire)
  players.rs  — Player development, aging, regens
  net.rs      — Savegame persistence helpers
  dev.rs      — Dev/debug utilities
data/
  players.csv — 2026 players database (FM 2023 + FC 26 merged pool)
docs/
  FM2023_PROBE.md — Dataset schema probe report
```

## Performance

Release profile uses `lto`, `opt-level=3`, `codegen-units=1`, and `panic=abort`
for a fast, small binary. The full 8,400-player database loads in well under a second.
