# Football Manager CLI ⚽

A fast, robust, end-to-end command-line football management game written in Rust.

It loads one real dataset — the 2026 players database (`data/players.csv`, an
EA FC-style dump, men's football only, ~19,800 rows with 110 attributes) — into
a seeded world with club squads drawn from real rosters and a free-agent market.
Falls back to procedural generation when a CSV is absent. Drop a newer
same-schema dump at `data/players.csv` to upgrade ratings.

## Features

- **New Game** — Pick a real league (La Liga, Premier League, Serie A, MLS…), then a club within it; opponents are that division only
- **Match Centre** — Team talks, per-shot xG engine (goals track chance quality), 7 mentalities, instructions, roles/duties, minute events
- **Starting XI & Positions** — Pick your eleven and field any player anywhere:
  drop a striker into a False Nine, push a midfielder to centre-back, invert a
  full-back. Each slot shows a live role-fit rating (per-attribute, with an
  out-of-position penalty) so the trade-off is visible, and the choice feeds
  straight into match strength. Auto-pick fills the best-fitting XI. Thirteen
  formations are modelled as real 11-slot pitch geometry (`tactics.rs`): a
  narrow diamond genuinely concedes the flanks, a high line invites through
  balls a pacey runner exploits, wing-backs create genuine wide overloads.
- **Development is environment-first** — a youngster grows under his club's
  academy tier (1–5★). The environment walls him in: academy 1 caps ~62, 2
  caps ~75, 3 caps ~82, 4 caps ~88, only a 5★ academy grows players to the
  full 95. At the wall growth all but stops; the only way past it is
  transferring up to a stronger academy — the real-world pipeline that takes
  decades of investment. Weak leagues produce strong *regional* players
  (ISL dynasties, Mohun Bagan's 5 titles), never world-class ones.
- **Actual FC overall everywhere** — club XI selection (`select_xi`) and club
  match strength (`match_strength`) now use the real database overall (synced
  from attributes every year) — the metric of record — instead of blended
  "role fit × suitability × form" math. Squads are picked the way the display
  rating says, with positional preference as a small bonus. Full decade
  regression after the switch: titles, tournaments, Ballons all sane.
- **Ballon d'Or with a Big-5 bias factor** — voters only really watch the top-5 leagues, so a player outside them is graded at a discount: UEFA outsiders ×0.90, CONMEBOL ×0.84, CONCACAF ×0.78, AFC ×0.72. Only a genuinely historic season (a 40-goal Port-city masterclass, a Saudi-era blasting) still breaks through — ordinary fine seasons elsewhere don't edge out Europe's monsters the way they used to. The world also produces the occasional true outsider winner when numbers are undeniable.
- **Full international calendar** — World Cup + Euros (unchanged), an **AFC Asian Cup** every 4 years (2027, 2031, 2035…) with its own winner, plus **annual friendlies** for every built nation (ISL, Saudi, MLS players eat caps and goals on the top-level stage continuously, so outsider stars actually develop as internationals worth voting for via `International goals/assists/run` components in the Ballon).
- **Player traits & body** — The 2026 dump's FC playstyles (Finesse Shot, Rapid, Dead Ball, Incisive Pass…) are parsed into internal traits that bias finishing, creation, dribbling and set pieces, and preferred foot / weak foot penalise finishing on the wrong side — all emerging from duels, not dice.
- **The non-UEFA world** — The sandbox now simulates AFC (ISL, Saudi, K League, CSL, A-League), CONMEBOL (Argentina + the pooled south-american Libertadores clubs) and CONCACAF (MLS, Liga MX) top flights with their own tables, domestic cups and **confederation cups** (AFC Champions League, CONCACAF Champions Cup, CONMEBOL Libertadores).
- **FIFA Club World Cup (every 4 years)** — 32-team tournament (2029, 2033…): UEFA 12, CONMEBOL 6, AFC 4, CONCACAF 4, OFC (Auckland FC) 1 + a rotating host (USA/Mexico). Slots earned like the real thing: the 4-year cycle of continental champions qualify directly, with a coefficient ladder (titles, then strength) and a two-clubs-per-country cap (champion-rich nations exempt).
- **FIFA World Cup 2026 format** — the real 48-team tournament: hosts (USA/CAN/MEX) plus UEFA 16, CAF 9, AFC 8, CONMEBOL 6, CONCACAF 3, OFC 1 and two intercontinental playoff winners qualify; 12 groups of 4 with pot seeding and confederation balance; top 2 plus the best 8 thirds advance; a Round of 32 then R16/QF/SF/Final (plus third-place playoff). Knockout brackets are properly seeded and national-team matchups price class properly — world champions are elite nations again, not Greece flukes.
- **Bid-appointed World Cup hosting** — after each World Cup, a FIFA-style committee vote (strength-weighted, recent hosts disqualified) awards the next tournament to single or co-host nations worldwide: 2026 = USA/Canada/Mexico, 2030 = England, 2034 = Senegal in the verification run. The host nation(s) qualify automatically and get a home-crowd edge in every match they play at the tournament. National strength is measured depth-weighted (55% stars, 45% squad depth) so giants separate from thin-pool nations at tournament time, and favoriters behave like favorites in knockouts.
- **Full match realism** — Real substitutions (bench of 7, exhaustion/chasing/
  cover triggers), cards with second-yellow and straight-red sendings-off (a
  team can finish a man down), stoppage time derived from fouls/injuries/subs,
  crowd lift scaled by the `crowd` dial, and — for your own career — the full
  possession engine runs *your* matches, so your XI, shape and mentality shape
  the game and scorers/assists/cards come off real events.
- **Squad Management** — View squads, inspect details (condition/sharpness/cards/injury, best-fit roles), sell players
- **Transfer Market** — Buy players from the 2026 database using real market values
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
- **Headless season sim (sandbox)** — Headless sim of the Big 5 leagues + the
  top non-Big-5 UEFA clubs (Porto, Benfica, Ajax, Celtic, Brugge, Galatasaray,
  PSV…) fielded with their *real* squads from the database as the "outsider"
  group contesting the Champions League (league_idx 5), plus Europa League,
  domestic cups + international tournaments (World Cup every 4 years, Euros in
  between), for one season (detailed) or up to 100 years (dynasty). Every club
  match is played by a **possession engine** (`matchsim`): ~190 possessions
  where 22 individuals contest build-up, progression and the final third from
  their own attributes *and their formation coordinates* — inverted
  full-backs add midfield control, full-backs create width overloads, deep
  blocks double up — so passes, dribbles, key passes, big chances, shots,
  tackles, saves, goals, assists, ratings and even injuries all emerge from
  real play (no flat per-event chances). Shot-taking follows role tendency
  (finishers hunt, holders recycle) with a hot hand when a man scores and
  defensive attention when he shoots all game; chance quality emerges from the
  creative duel. Corners and dangerous free kicks are full aerial contests
  decided by heading × strength, and deep-sitting sides genuinely double up
  while gung-ho sides overload. Managers read the scoreline — including a
  two-legged tie's aggregate — and shift mentality mid-match, and the opponent
  reacts. The UCL runs the real Swiss format (8 games, top 16 through,
  single-leg neutral final).
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

See `docs/FM2023_PROBE.md` for the mechanics research log and world-model accounting.

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
  main.rs       — CLI game loop, all screens, interactive match
  model.rs      — Core domain: Player, Club, Attributes, Formation, Tactic, Fixture
  matchsim.rs   — Possession-phase match engine (22 individuals, per-player stats)
  tactics.rs    — Spatial formation geometry: 11 slot coordinates per shape; width &
                  defensive-line warp the shape, flank coverage + high-line physics
                  feed the duel engine
  traits.rs     — Player traits parsed from the FC dump's playstyles (Finesse Shot,
                  Rapid, Dead Ball…) plus weak-foot physics
  world_model.rs— World registry: ~49 real leagues + 164 nations with confid
                  erations, tiers and UEFA-style coefficients
  observe.rs    — Spectator mode: live match ticker + league-season browser
  season.rs     — Season/dynasty sandbox (drives matches through matchsim);
                  loads real squads for the Big 5 **and** top non-Big-5 UEFA
                  clubs (Porto, Ajax, Celtic, Galatasaray…) as the UCL field
  gen.rs        — Procedural world generation (names, squads, fallback data)
  data.rs       — CSV loader (BOM-aware, quote-aware parser) + procedural fallback
  cup.rs        — Domestic cups, European competitions, super cups
  engine.rs     — Shared simulation engine helpers
  tune.rs       — Evolutionary calibration of sim parameters
  deals.rs      — Transfers, loans, contracts
  managers.rs   — AI manager lifecycle (hire/sack/retire)
  players.rs    — Player development, aging, regens
  net.rs        — Savegame persistence helpers
  dev.rs        — Dev/debug utilities
data/
  players.csv — 2026 players database (single EA FC-style dump)
docs/
  FM2023_PROBE.md — Mechanics research + world-model accounting (probe log)
```

## Performance

Release profile uses `lto`, `opt-level=3`, `codegen-units=1`, and `panic=abort`
for a fast, small binary. The full ~19,800-row database loads in well under a second.
