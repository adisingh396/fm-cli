# FM 2023 Dataset - Probe Report

## Source: https://github.com/dannytharris/FM (FM 2023.csv)

## Dataset Overview
- ~2,828 players
- Each player has 100+ attributes
- Core scale: 1-99 (ca/pa), 1-20 (skill attributes)

## Attribute Categories (from CSV header):

### Basic Info
- Name, Position, Age, Nationality, Club, Height, Weight, Date of birth
- Race, Preferred Foot (Left/Right), Caps, Goals, Salary

### Core Abilities (1-99 scale)
- ca (Career Ability), pa (Potential Ability)

### Skills (1-20 scale)
- Corners, Crossing, Dribbling, Finishing, First Touch, Free Kick Taking,
  Heading, Long Shots, Long Throws, Marking, Passing, Penalty Taking,
  Tackling, Technique

### Mental Attributes (1-20 scale)
- Aggression, Anticipation, Bravery, Composure, Concentration, Vision,
  Decision, Determination, Flair, Leadership, Off The Ball, Positioning,
  Teamwork, Work Rate

### Physical Attributes (1-20 scale)
- Acceleration, Agility, Balance, Jumping Reach, Natural Fitness, Pace,
  Stamina, Strength, Stability

### GK Attributes (1-20 scale)
- Aggression, Command Of Area, Communication, Eccentricity, Handling,
  Kicking, One On Ones, Reflexes, Rushing Out, Punching, Throwing,
  Adaptation, Ambition, Argue, Loyalty, Resist to Stress, Professional,
  Sportsmanship, Emotional Control

### Defensive Roles (1-20 scale)
- DL (Defender Left), DC (Defender Centre), DR (Defender Right),
  WBL (Wing Back Left), WBR (Wing Back Right), DM, ML, MC, MR,
  AML, AMC, AMR, ST

### Market Data
- Values (market value in currency units), Current reputation,
  Domestic reputation, World reputation

### National Team
- National team appearances, Goals scored for national team,
  Rental club, UID

## Key Repositories Found:
1. dannytharris/FM - Primary CSV dataset
2. RJ030830/Projeto_Analise_FootballManager2023 - Python ETL pipeline
3. EliasBorrajo/Football-Manager - Java EE 7 web app
4. chavalvit-k/football-player-market-values-prediction-with-xgboost - ML prediction
5. nmclawrence010/fm23-position-calculator - Position optimization
6. tsubasa1123/sokrstat-fm2023 - Statistics web app
7. giangkatas/Football-Manager-2023-Scraper - Data scraper
8. raffaelow/Football_Manager2023_Scouting_Tool_Values - Scouting tool
9. papa-github/Football-Manager-2023-MoneyBall - Moneyball analysis

## FM23 Full Feature Probe (summarized in own words, 2026-09-28)

Sources consulted (public docs/guides, no FM text reproduced):
- SI community manual pages (tactics, playing a match, training, finances, club/board)
- footballmanager.com feature pages (transfers/squad building, supporter confidence, licences)
- fmscout.com, passion4fm.com, fminside.net, FM fandom wiki, Steam FM23 page
- Supplemental rule sources: UEFA.com (club competitions in FM23), premierleague.com (subs/suspensions context), Wikipedia (FFP overview), gameran/outside/givemesport/4-4-2 media guides

### 1. Tactics + Matchday
- Formations as slot maps setting starting zones in/out of possession.
- 7 mentalities (Very Defensive..Very Attacking): global risk dial shifting press height, tempo, width, directness, time-wasting.
- In-possession instructions: width, focus (flanks/middle), directness, tempo, time-wasting, crossing style, dribbling, creative freedom, through-ball risk.
- Transition: counter-press vs regroup when lost; counter vs hold shape when won; GK distribution tempo/target/method.
- Out-of-possession (FM23 rework): line of engagement (low/mid/high), defensive line height + hold/drop, pressing traps inside/outside, cross engagement, tackling intensity vs card risk, press triggers, blocking short GK distribution.
- Player roles x duties (Defend/Support/Attack): per-slot defaults for runs, risk, pressing (e.g. GK/SK, CB/BPD, FB/WB, DM anchor/DLP/BWM/regista, CM BBM/AP, wide winger/IF, AM AP/SS, ST advanced/poacher/target/false nine).
- Set pieces: per-type routines (corners L/R, free kicks, throws, pens) with rotated variants + ordered taker lists.
- Opposition instructions per slot: tight marking, pressing trigger, tackling, show-onto-foot.
- Match plans: conditional tactic switches on scoreline + minute window.
- Prematch: staff meeting, opposition report, fitness/sharpness/suspension checklist, tactical familiarity %.
- Team talks (pre/half/full, team or individual): morale/focus modifier.
- Touchline shouts (~8, ~10-min buffs, team or individual): encourage/praise vs demand/fire-up vs calm; gated by body language.
- Subs: 5 subs within 3 stoppages + half-time (2022/23 era rules).
- Condition drains with minutes/intensity; match sharpness needs minutes; low sharpness raises injury drain.
- Live stats: score, possession, shots/on-target, xG team + per-player, pass %, tackles/fouls/cards, momentum, 1-10 ratings, minute-by-minute events.

### 2. Transfers + Contracts + Loans + Scouting
- Bids: enquiry -> bid -> negotiation rounds (accept/reject/counter, deadlines); asking price above value; AI reacts to form, reputation, interest, contract expiry; bidding wars inflate price.
- Fee structure: lump sum + installments + add-ons (apps/goals/caps/promotion/continental) + sell-on % + buy-back + loan-back; player exchange possible; agent + signing-on fees separate; board transfer/wage budgets gate deals.
- Windows: registrations only inside national window; free agents outside; deadline-day countdown; work permits (post-Brexit GB points test) with appeal or loan-abroad fallback.
- Contracts: wage, length, playing-time promise (key/first-team/rotation/backup/prospect); bonuses (apps/goals/clean sheets/loyalty/promotion/title/continental/caps); clauses (min-fee release, relegation release/wage-cut, promotion rise, +1yr extension option); Bosman pre-contracts <6mo; final-year values collapse.
- Listing: transfer-list vs offer-to-clubs (set/blank price, targeted clubs); unhappy players (warnings, requests, refusal) resolved via promises.
- Loans: fee + wage-share %, duration, playing-time promise, recall clause/window + recall on broken time; future fee/buy option; youth need senior minutes at right level; loan manager tracking + weekly reports.
- Scouting: chief/scouts/analysts with ability-judging + data-analysis ratings; staff cap + budget + geographic range; recruitment focuses (position/role, age, ability, region, deal type); reports with star ratings vs club, recommendation grade, pros/cons, role fit, price/wage estimates, knowledge % growing per scouting; data-hub radars feed shortlists; agent offers; trialists; feeder affiliates.

### 3. Training + Development + Youth + Staff + Medical
- Weekly schedule grid (AM/PM/extra slots across attacking/defending/technical/tactical/physical/GK/set-piece/match-prep/rest); presets for 1- vs 2-match weeks + pre-season fitness block.
- Individual focus: role + single attribute + intensity; new-position familiarity curve; coach-taught traits; workload vs injury/unhappiness tradeoff; rest days; match-prep cohesion sessions; mentoring groups (3+, same squad) nudging personality via hierarchy/age/leadership.
- CA/PA model: hidden current vs fixed potential ceiling (flat or randomized range per save); stars are staff estimates scaled to squad; growth fastest ~16-23, peak ~24-30, pace/stamina decline first; drivers = minutes at right level, training quality, personality, morale/happiness, sharpness, coach/facility quality, injury avoidance.
- Pathways: senior tiers (star/important/regular/squad/fringe/surplus) and youth tiers (breakthrough/prospect/youngster); broken promises hurt morale + growth + trigger requests.
- Youth intake ~1x/year per nation: batch of 15-16yo with HOYD-graded report; quality levers = youth facilities, junior coaching, youth recruitment, HOYD profile, nation rating; U18/U21 squads with own staff/fixtures; loan-to-develop pathway with level/minutes/facilities fit.
- Staff roles: assistant, coaches (general/fitness/GK), U18/U21 managers, HOYD, chief/scouts, analysts, physios, sports scientists, loan manager, DoF, technical director; key ratings (coaching clusters, man-management, youth work, physio/sports-science, negotiating, adaptability, badges); workload penalties; hire/fire/renew; backroom advice + staff meetings; delegation of tasks.
- Medical: per-player injury-risk band + reason, susceptibility, history/recurring flags; slight/minor/major severities; relapse if rushed; physio shortens layoffs; sports science lowers injury odds and jadedness; condition/sharpness/jadedness trio; pre-season build-up matters.

### 4. Board + Finances + Facilities + Dynamics + Media/Fans
- Club vision: multi-year plan (league/cup/continental targets + philosophies like attacking play, youth development, domestic signings, wage discipline, solvency) with 5-year milestones.
- Board confidence per area (vision, matches, transfers, tactics, squad) on a graded scale; prolonged negatives risk sacking; takeovers/embargo/budget injections as events; affiliate network (loan feeders, first options, youth intake mix, friendly fees, scouting knowledge, marketing).
- Budgets: transfer lump + weekly wage pot with adjustable slider; income = sponsorships, TV (equal share + per-game + final-position prize), matchday (attendance x ticket + hospitality), merchandising, friendly fees, installment receipts; costs = wages, installments, loan fees/interest, upkeep, agent fees; debt = bank loans + net transfer debt; insolvency risks embargo/points deduction; FFP-style caps enforced.
- Facilities on stepped scales: training (10 labels poor->state of art, gates training speed + unlocks focuses), youth facilities (intake CA), junior coaching (intake CA/PA volume), youth recruitment (pool breadth), youth level (competition quality), data/analysis tier; board-requested upgrades take months and can decay.
- Dynamics: hierarchy pyramid (leaders/highly/influential), social cliques (tenure/language/nation) with morale contagion, happiness drivers (time vs promise, results, treatment, broken promises), personalities from hidden drivers + labels, code-of-conduct fines, team talks + team meetings.
- Media/fans: pre/post-match press conferences with tonal answers (praise/defend vs criticize unsettles), delegable to assistant; media-handling styles; journalist personalities; social-feed reactions; FM23 supporter confidence with 6 fan segments (hardcore/core/family/fair-weather/corporate/casual) tracked separately from board, weighing identity, derbies, legends, style.

### 5. Competitions + Season + Manager Career
- Leagues: round-robin 3/1/0; England sorts Pts > GD > GF (playoff for undecided title/relegation in this era); Spain/Italy use head-to-head first; Germany GD > GF; nation rules vary.
- Promotion/relegation + playoffs (England: 2 auto + 3rd-6th semis 2-leg + Wembley final); multi-tier pyramids.
- Domestic cups: open knockouts; FA Cup early replays then ET/pens, neutral semis/final; Carabao single-leg to pens (2-leg semis), Wembley final.
- Continental (first licensed in FM23): UCL/UEL/UECL 32 teams, 8x4 home/away groups, top 2 advance; 3rd-place drops down a tier via playoff; 2-leg knockouts, single final + ET/pens; away-goals abolished 2021; Super Cup + Youth League included.
- International: 32-team World Cup cycle, 24-team Euros, qualifying groups + playoffs, Nations League groups; optional dual club+country jobs.
- Season flow: pre-season friendlies/tours (fitness/sharpness/familiarity) -> windows (summer ~mid-Jun->1 Sep, winter Jan) -> congested Dec-Apr (2 games/week) -> winter break -> postponements/rearranged midweeks -> playoffs/finals -> awards -> board reset -> rollover (age+1, contracts-1yr, Bosman warnings, retirements to staff, dated youth intakes, new fixtures, promoted swaps).
- Rules: 25-man senior registration with U21 exemption + min homegrown counts (club/nation trained variants); EU non-EU caps; 18-man matchday with 7 subs (FM23 era); yellows accumulate to 1-2 game bans at thresholds, reds 1-3 games by severity; GB work-permit points + fee + appeal window; FFP loss/break-even caps with fines/embargo/exclusion.
- Manager career: badge ladder (none -> national C/B/A/pro) + playing past set coaching ratings + starting reputation; reputation gates jobs; unemployed starts common; apply -> shortlist -> interview (philosophy/expectations/budgets) -> offer/negotiate; sack/resign; AI hire/fire; per-club timeline (P/W/D, trophies); hall-of-fame score.
- Awards/records: golden boot/playmaker/glove, player/young/manager of season, team of season/week, monthly awards; all-time apps/goals/fees/attendance/streaks/youngest/oldest.

## Binary-adjacent probe (2026-09-28)

No FM23 binary exists on this machine (verified: no Steam FM23 install, no
`Documents/Sports Interactive` folder, no `.fm`/`.fmf` files). Nothing was
disassembled or decompiled, and no proprietary code, text, or data is
reproduced anywhere in this project. All game logic is original Rust.

What was probed instead (public/community sources, paraphrased):
- Install/mod layout: community mods ship as single `.fmf`/`.xml` files into
  `editor data/` and are picked at new-career setup; skins are `skin_config.xml`
  + `panels/` + `graphics/` with parent-skin fallback; saves live in per-version
  `games/` folders.
- Save format: proprietary compressed binary, not publicly parseable
  (community consensus) -> we keep our own versioned JSON saves in `saves/`.
- Scales (manual/wiki): visible attributes 1-20 in Technical/Mental/Physical/GK
  groups plus hidden drivers; CA/PA 0-200 hidden with negative `-1..-10` ranges
  (e.g. `-10` = 170-200); star ratings are staff estimates relative to the
  squad, never absolute.
- Tactic shares (structural): formation shape + mentality + per-slot role/duty
  + 3-phase instructions + ordered takers -> mirrored by our `Tactic` struct,
  which serializes as plain data.
- Engine baselines (2022/23 PL, public stats): ~2.7-2.85 goals/game, ~11.5
  shots/team-game, ~0.11 xG/shot, ~48% home wins, 6.7 rating baseline, 5 subs /
  3 stoppages / 9 bench, yellow bans at 5-before-19 and 10-before-32, reds 1-3
  by severity -> encoded as OUR constants in `src/engine.rs` (unit-tested).
- Deal/medical/training behavior above comes from observable community guides
  and real-world rules (Bosman, GBE points, windows), re-expressed as OUR
  state machines in `src/deals.rs` and tick formulas in `src/dev.rs`.

## FM Ratings Database (FC 26, probed 2026-09-28)

- No FC 27 bulk player dump exists publicly; the latest downloadable ratings
  set is FC 26: `ismailoksuz/EAFC26-DataHub` (`data/players.csv`, 18,405 men,
  110 SoFIFA-style attributes, Kaggle-sourced). Wired in as `data/fc26.csv`.
- Loader (`src/data.rs:merge_fc`) maps the 30+ SoFIFA fields onto the 12 world
  attributes, scales ratings x0.3 and values x5% into world range, matches
  clubs case-insensitively, and caches the merged pool process-wide.
- Honest limits: duplicate players can exist across the FM and FC sets
  (different club spellings), and catalog screens can list a template that is
  already seeded as a world free agent.

## FM realism probe (leagues + match engine, 2026-09-28)

Two user-visible breakages fixed after probing public FM sources (SI manual
"Add/Remove Leagues", FMInside "start a new career", FM Scout xG/finishing
threads, community match-engine writeups — paraphrased, no FM code/data used):

1. **Leagues are per-nation divisions.** FM has you pick nation -> league ->
   club; opponents are that division only. Ours built a 20-club league from a
   random slice of the merged 900+ pool, so a La Liga side faced a German 2.
   Liga club. Fix: `Club.league` now carries the FC 26 competition label
   (`src/data.rs`, disambiguated by `league_id` so the five different "Super
   League" nations don't merge), and New Game is league-first — pick a real
   division (La Liga, Premier League, Serie A, MLS...), and every opponent is
   drawn from it.
2. **Goals must track chance quality.** FM converts per shot: a clear-cut
   chance ~0.30-0.45, a decent look ~0.10-0.22, a speculative effort ~0.05, a
   penalty ~0.76; the score is the sum of converted shots, so it stays near xG
   with only football's real variance. Ours drew each side's goals from an
   independent Poisson on the xG total, so a 0.8-xG side beating a 2.5-xG side
   3-1 was common. Fix: `engine.rs` now assigns each shot a quality and rolls a
   Bernoulli per shot (binomial, under-dispersed vs Poisson). A regression test
   asserts total goals stay within 10% of total xG across 2000 games.

## CLI mapping (original logic, implemented incrementally)
- Tactics: `Tactic` gains instructions + per-slot role/duty + taker order; screens: set-mentality (7), set-instruction, set-role, setpiece takers, opposition slot flags, match plans; prematch checklist; talks/shouts as timed modifiers; 5-sub/3-window enforcement; live minute ticker with xG + ratings.
- Transfers/contracts/loans: `Offer` (base/installments/add-ons/sell-on/wage-share/loan fee/duration/time promise/recall/buy option); bid/counter/accept loop vs AI multiplier; renew with wage/years/time/release/bonus/extension; Bosman tab; loan in/out with monthly ticks + recall; board budget gates; window flag; permit check.
- Scouting: assignments with region/weeks; delayed reports; stars vs club average; knowledge %; price/wage estimate error shrinking; shortlist; trials; data-hub form table.
- Training/dev/youth/staff/medical: schedule preset + intensity + rest; individual focus + new-position familiarity; mentoring groups; CA/PA hidden with star display; annual intake event; U18/U21 as squad filters; staff rows with ratings + workload + advice; medical risk table + rest; pathway tiers + promise tracking; growth/injury weekly ticks.
- Club/finance/dynamics/media: vision + per-area board confidence + fan confidence (6 segments); transfer/wage budgets + slider; income/cost ledger + 3-season projection; ticket price; facility levels + upgrade requests; hierarchy/social/happiness screens; code-of-conduct fines; talks/meetings; press Q&A pre/post-match; news tags.
- Competitions/season/career: `Competition` (type, nation, tier, teams, promo/rel/playoff, tiebreak order); cup rounds (legs/replays/ET/pens); euro slots; season pipeline stages (preseason/window/congestion/run-in/review/rollover); registration + suspension + permit + FFP checks; manager profile (badges/exp/rep/history/fame); awards + records tables.

## World model accounting (the full hisab — everything below is original logic)

Determinism: every stochastic choice flows from one seeded RNG per run
(Pcg64Mcg). Same seed → same 10 years. Managers and players differ because
their traits are rolled per individual from that seed — not because of hidden
global rules.

Player agent (`src/players.rs`, state in parallel `World.minds`):
- Motives rolled 0-1: ambition, loyalty, greed, professionalism.
- Expected game time by squad rank: starter 95% of matches, rotation 55%,
  depth 25%. 4+ games short → unhappy + morale hit; 8+ short + loyalty < 0.45
  → transfer request (stands until sold). Meeting expectation lifts morale.
- Morale floor = 30 + professionalism*20. Requested players sit at morale 45.
- Greedy players cost 1.0-1.3x wage when signing.
- Effective rating = overall × (1 + (morale-65)*0.002) × (1 - fatigue*0.0025).
  Fatigue +22 per match involved, -25 per rest week. Selection AND team
  strength use effective rating, so tired/unhappy players cost results.

Manager agent (`src/managers.rs`, one per club, name + career record):
- Traits rolled per individual: rotation 0.20-0.49, ambition 0.6-1.5,
  quality 0.90-1.10, youth_trust/loyalty 0-1, security (starts 0.6).
- Insecure managers (security < 0.4) halve rotation and ride stars.
- Youth get a 1 + trust*0.15 selection boost; the talisman never rests.
- Transfer bids: score = w_need*need + w_value*value_fit + w_amb*(amb/1.5) +
  w_wage*room, pursue if > 0.55. Weights rolled per manager — identical deals
  pass for one individual and fail for another (unit-tested).
- Ambitious + disloyal managers accept bigger jobs; loyal ones stay.
- Security tracks results vs expectation yearly; quality regresses to 1.0.

Timeline: weekly rounds (circle-method fixtures, each club plays once), then
midweek recovery (fatigue -25, morale drift to mind target, treatment
evaluation). Cups/internationals run as tournament bursts between.

Money (grounded to Deloitte Football Money League 2024/25: Real ~€1100m,
City ~€830m, Spurs ~€673m, mid-table ~€150-280m): revenue(rep) piecewise
through those anchors; transfer budget = 25% of revenue (no rollover);
wage cap = 50% of revenue. Greedy signings cost more; bursting the cap forces
sales first. Seller agents accept at 95% of ask (85% for requests/cash needs);
ambitious buyers counter once at full ask.

## Learning loop (train, don't hand-tune)

`src/net.rs` is a from-scratch MLP policy (6 inputs → 8 tanh → sigmoid):
need, value fit, wage room, ambition, reputation gap, request status. One
learned policy, individual behavior via trait inputs — identical deals resolve
differently per manager.

`src/tune.rs` holds every tunable constant (~45, `Params`, JSON-serializable)
plus real-world targets and the fitness function: champion points ~88±7,
golden boot ~30±5, UCL winners ~always elite, Ballon winners score 20+ for
contenders, 4-10 big deals/window, record fees €80-320m, title concentration
(HHI) 0.2-0.6 per league.

Training = evolution strategies (the honest algorithm for a
non-differentiable simulator; no backprop through matches exists): a
population of parameter vectors each simulates seasons, fitness = distance to
the targets, elites survive and mutate (Gaussian, decaying sigma). Menu
option 4 runs it; best vector saves to `tuned_params.json`, auto-loaded by
all future sims. Randomness stays (seeded RNG — football has variance); what
is learned are decision policies and rates, so trends converge to reality.

