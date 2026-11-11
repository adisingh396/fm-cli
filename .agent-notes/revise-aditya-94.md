Done: Revised the opt-in Aditya Singh scenario from 86 to 94 OVR with the requested balanced elite profile and a EUR5m-equivalent release clause (5 in the sim million unit), backed up the prior 86 career JSON, reran cargo fmt/tests and the 20-year simulation, and validated the output.

Changed files:
- src/season.rs: updated inject_aditya_prodigy profile to pace 97, shooting 94, passing 91, dribbling 96, tackling 42, heading 84, strength 82, stamina 91, vision 93, composure 92, goalkeeping 8, first_touch 96; debug_assert/market_value/ovr set to 94; release_clause Some(5); comment updated.
- sim_dump/aditya_singh_86_career.json: backup of previous 86 OVR career (new file).
- sim20_aditya_singh_94.md: new 20-year run report.
- sim_dump/aditya_singh_career.json: regenerated 94 OVR career (20 entries).

Commands/results:
- cargo fmt: pass (exit 0).
- cargo test --release: pass (107 tests, 0 failed).
- cargo build --release: pass.
- FM_ADITY_PRODIGY=1 FM_AUDIT_LOG=0 simulation with inputs 3/20 -> sim20_aditya_singh_94.md: exit 0, 74744 bytes.

Career totals (20 annual records, age 15 to 34):
- League: 619 apps, 279 goals, 107 assists.
- Continental (UCL): 34 apps, 6 goals, 7 assists.
- International: 33 apps, 7 goals, 2 assists.
- Combined: 686 apps, 292 goals, 116 assists.
- Peak OVR 94 in seasons 2026-2040 (ages 15-29); then 93, 93, 92, 91, 90.
- Ballons: 0.
- League titles: 8 (all Kerala Blasters).
- UCL best: runner-up once (2029, Kerala Blasters); no UCL title.
- International best: runner-up twice (2035, 2039, Kerala Blasters); no title.
- Club path: Kerala Blasters (ages 15-29) -> Paris SG (30-32) -> Al Ittihad (33-34).
- First transfer: 2041, age 30, Kerala Blasters to Paris SG.

Clause impact: The EUR5m-equivalent clause did NOT produce an early move. Initial locked=5 keeps the player untransferable during the first contract; by the time locked reaches 0 the clause is repriced to a market-value figure on renewal, so the EUR5m clause never triggers a transfer.

Deviations/open issues:
- No commit or push performed.
- Result-code semantics treated as round reached (UCL: 5 winner, 4 runner-up; intl: 5 winner, 4 runner-up); no title codes (5) occurred for Aditya.
- Existing untracked simulation reports left untouched.
