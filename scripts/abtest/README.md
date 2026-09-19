# Tier-1 paired multi-session A/B harness

Tests the actual product claim: **does injecting ARMIN's reasoning-state
brief into an agent's context measurably improve cross-session decision
consistency — without hurting task success or cost?**

## Design

- **Paired arms**, identical fixture, identical scripted prompts, fresh
  opencode sessions:
  - **arm A**: after session 1, a `<reasoning-state>` block (Jev-native
    extraction of that arm's own session-1 transcript) is written to
    `AGENTS.md` — the same system-prompt injection channel the middleware
    uses — plus an instruction to treat the decisions as settled.
  - **arm B**: identical `AGENTS.md` with a content-free placeholder
    (same wrapper, similar length — controls for prompt-length effects).
- **Session 1** forces two binding decisions (storage backend json/sqlite,
  config mechanism env/file) and requires a machine-readable decision header.
- **Session 2** (fresh context) adds a feature *through the existing storage
  layer* and an override "consistent with existing decisions" — the trap is
  to add a second mechanism or silently flip a session-1 decision.
- **Scoring is ARMIN-free** (`check_example1.py`): decision-flip detection by
  diffing the header between the session-1 commit and the final worktree,
  functional probes (add/list/stats), and a mechanism-consistency probe in an
  isolated HOME/env. Verdicts PASS / VIOLATION / UNCLEAR.

## Phases

    uv run scripts/abtest/run_pilot.py --phase stage
    uv run scripts/abtest/run_pilot.py --phase task1  --arms A
    uv run scripts/abtest/run_pilot.py --phase task1  --arms B
    uv run scripts/abtest/run_pilot.py --phase brief
    uv run scripts/abtest/run_pilot.py --phase task2  --arms A
    uv run scripts/abtest/run_pilot.py --phase task2  --arms B
    uv run scripts/abtest/run_pilot.py --phase check

or `--phase all`. State lives in `<workdir>/abtest-state.json`; transcripts,
briefs, and the final `pilot-summary.json` are kept under the workdir.

## Notes

- Brief built with `--native-threshold 0.5` (the human-gold calibration
  found 0.85 too conservative for recall-oriented context injection).
- Anti-circularity: ARMIN never grades its own output — the checker uses
  git diffs and scripted probes only.
- Pilot = 1 task pair × 2 arms, single seed (directional signal only).
  The powered study repeats over N fixture variants with K seeds per arm;
  paired analysis (per-task differences, bootstrap CI) detects ~25–30pp
  deltas at N=20–30.
- Cost per pilot: 4 opencode sessions (default model) + 1 small Jev-native
  scan per arm (cached in `data/external/.jev-cache-abtest.json`).

## Pilot result — example 1 (jev-native brief, 2026-09-17)

Setup: notesctl fixture, arm A = Jev-native brief in AGENTS.md, arm B =
content-free placeholder. 4 opencode sessions, checker = git diff + probes.

| | arm A (brief) | arm B (placeholder) |
|---|---|---|
| decision flips | 0 | 0 |
| functional (add/list/stats/mechanism) | PASS ×4 | PASS ×4 |
| task2 diff | 3 files, +52 | 4 files, +81 |
| session2 mentioned brief/decisions | 1× | 0× |

Findings (this is why one runs a pilot):

1. **The Jev-native brief missed its own payload.** Of 9 "Decisions" in
   arm A's brief, most were echoes of the task *instructions* ("You must
   commit to exactly one storage backend...") or process talk ("Let me check
   the remaining files..."); the actual settled choices (`Backend: json`,
   `Config: env`) — which only appear in the committed `storage.py` header,
   i.e. in tool events — never made it into the brief. The prose-only scan
   under-includes tool-event content, exactly the Tier-0 faithfulness risk.
   → Fix: feed tool events (file writes) into the scan, as the middleware
   would; or let the brief builder carry deterministic header/decision
   extraction from changed files.
2. **The fixture was too easy**: session-1 decisions are self-documenting
   in the repo header, so arm B stayed consistent without any memory.
   Powered-study fixtures must place decision constraints *only in the
   transcript* (verbal, no code footprint) so the control arm genuinely
   lacks them.
3. Harness bugs found and fixed: `opencode run` resolves the workspace from
   the inherited `PWD` env var (now pinned to the arm dir) and attaches to
   the user's running workspace unless `XDG_DATA_HOME`/`XDG_CONFIG_HOME` are
   sandboxed per arm (auth is copied in; sessions land in a sandbox DB).
   Two stray "abtest-task1-A" sessions from the failed attempts remain in
   the user's global opencode DB (harmless, uncommitted).

Conclusion: harness works end-to-end (stage → sessions → Jev-native brief →
checker, ~25 min, ~12k Jev tokens); example 1 is uninformative for the A/B
effect but produced two concrete treatment/fixture fixes for the powered run.

## Pilot results — example 2, minimal v2 taxonomy (2026-09-19)

Taxonomy stripped to `{Decision, OpenItem}` per the backtest evidence
(see `data/backtest/`); jev criteria: 3 choices, one judgment per sentence
(no noul pass), gate 0.85. Two runs of the full sequence
(`--example 2 --native-threshold 0.85`, workdirs `abtest-pilot-v2/-v3`).

**Run 1** (task2 asked for a "robust embedded database engine" for the
export): both arms imported sqlite3 → single_backend VIOLATION, everything
else PASS. Uninformative A/B, but two strong findings:

1. **The brief now carries its payload verbatim.** Arm A's AGENTS.md held
   exactly 5 decisions: the two binding transcript-only constraints ("never
   print timestamps", "everything persistent goes through the single
   backend") plus the settled choices. The old pilot's failure #1
   (instruction echoes, missed payload) is fixed by the minimal criteria.
2. **Agents obey explicit new user instructions over remembered
   constraints.** Naming a mechanism in the new prompt ("embedded database
   engine") reads as an explicit override — the brief's "unless the user
   explicitly overrides" escape hatch fired. Product lever: the brief
   injection should tell the agent to *flag the conflict* rather than
   silently switch when a new request touches a settled decision.

**Run 2** (fixture fix: crash-safe export without naming a mechanism):
both arms fully consistent (6/6 PASS each). Arm A (json backend) and arm B
(sqlite backend, chosen legitimately in task 1) both honored their
session-1 decisions — arm B without any memory. Conclusion: atomic
replace-on-write is idiomatic enough that the trap does not tempt a
capable model; the no-conflict fixture cannot discriminate arms.

**Net status:** extraction side measured and improved (brief payload
correct in both runs, ~10-15 jev calls per arm, sessions stable end to
end). The bottleneck for the powered study is fixture design, unchanged
from the example-1 finding: the control arm must *genuinely* lack the
constraints AND the temptation must be reachable by a plausible-but-wrong
solution path. Candidate: constraints that force a *non-idiomatic* choice
(e.g., "no atomic-rename tricks: the export must be a single write call"),
so the brief is the only thing steering the agent away from the default.

## Pilot results — example 3, cross-session consistency (2026-09-19)

New fixture design per the no-trap requirement: two *equivalently valuable*
data-file formats (single JSON array vs line-based), the SAME choice made
twice — session 1 picks the notes-file format, session 2 (fresh session)
must pick one format for BOTH its store and export files. Session 2 runs
on a **pristine fixture copy**, so nothing in session-1 code can leak the
choice; the only bridge is memory (arm A: brief; arm B: none). Session 1's
choice is forced by a transcript-only team standard; both options are
functionally equal, so there is nothing to "trap" — the measured signal is
purely cross-session consistency (`check_example3.py`, ARMIN-free).

**Prior calibration matters.** Run v5 forced the line-based format — which
turned out to be the model's *natural* choice (arm B's free session-2
choice: line-based, "Unix-y plain-text ethos") → both arms consistent, no
discrimination. Fix: force session 1 to the **anti-prior** option (JSON).

**Proxy mode (v6, forced JSON):** arm A (brief in AGENTS.md) chose JSON in
session 2 "consistent with the project's previously settled design
decision" → consistent=True. Arm B (no memory) drifted to line-based →
consistent=False. Both arms task_success=True. **Clean 1/1 paired
discrimination with zero functional failures.**

**Middleware mode (v8, real plugin + engine):** first run found a product
bug — `dbPathFor()` keys the engine DB on the *worktree path*, so session
2's fresh checkout got an empty graph and arm A drifted (v7). Bridged at
the harness level by copying the sled dir under the t2 worktree slug; the
real fix is project-identity keys (e.g. git remote) instead of paths.
With the bridge: arm A consistent=True — its transcript explicitly cites
the injected reasoning state ("The reasoning graph shows the binding team
standard … and a prior decision already tracks 'Pick JSON array format for
notes.data'"). Arm B drifted, consistent=False. **The full middleware path
(capture → jev extraction → engine brief push) now measurably produces the
same behavior as the static brief.**

| example-3 runs | arm A (memory) | arm B (no memory) |
|---|---|---|
| v5 forced line-based (= prior) | consistent | consistent (uninformative) |
| v6 proxy, forced JSON (anti-prior) | **consistent** | **drifted** |
| v8 middleware, forced JSON + DB bridge | **consistent** | **drifted** |

Follow-ups: (1) per-worktree DB identity → project-identity key in the
plugin; (2) powered study = N seeds × both forced assignments per arm
(the v5 anti-calibration shows assignment must be anti-prior per fixture);
(3) conflict-flagging brief wording from the example-2 finding.

## Identity fix follow-up (v9) — cross-checkout memory

The per-worktree DB bug is fixed in the plugin: `projectKeyFor()` keys the
engine database on the **git origin remote URL** (falling back to the
worktree path for non-git directories), so different checkouts of the same
project share one graph while different projects stay isolated. The
harness now gives each arm a fake origin (`notesctl-arm{A,B}.git`) —
shared by that arm's task1 and task2 worktrees, distinct across arms —
and the DB-copy bridge is gone. Validated in v9 (middleware, forced=json,
no bridge): arm A consistent=True citing the reasoning graph from the
remote-keyed DB, arm B drifted. One DB per project on disk:
`https-abtest-local-notesctl-armA.db`.

## Powered-study protocol (`run_study.py`)

**Design.** N independent paired runs of the full example-3 sequence; each
pair in its own workdir (fresh sessions, fresh engine DBs, isolated
sandboxes). Within a pair, arms differ only in memory (arm A: middleware
brief; arm B: none). Between pairs, the task-1 team standard **alternates
assignment** (`json` odd pairs, `lines` even pairs). Alternation is the
honest protocol: when the forced standard matches the model's prior the
pair is uninformative (both arms consistent — real teams' standards often
do match agent defaults); when it is anti-prior only memory keeps arm A
consistent. Per-pair verdicts come from `check_example3.py`
(ARMIN-free: byte-sniffed formats + functional probes; `d = A - B ∈
{1, 0, -1}`).

**Analysis.** Consistency rates per arm, mean paired delta, bootstrap 95%
CI over pairs (10k resamples), exact sign test on discordant pairs, plus
the anti-prior subset (`forced=json`) broken out — that subset is where
discrimination happens per the v5/v6 calibration.

**Run** (resume-safe: completed pairs are skipped):

    uv run scripts/abtest/run_study.py --pairs 6 --mode middleware \
        --workdir /tmp/opencode/abtest-study

Cost: one pair ≈ 4 opencode sessions + ~10–20 jev calls (engine path)
≈ 10–15 min wall. `--native-threshold 0.85`.

**Results — N=6, middleware mode (2026-09-19, study-summary.json):**

| pair | forced | arm A (memory) | arm B (no memory) | d |
|---|---|---|---|---|
| 1 | json | consistent | consistent | 0 |
| 2 | lines | consistent | consistent | 0 |
| 3 | json | consistent | **drifted** | +1 |
| 4 | lines | consistent | consistent | 0 |
| 5 | json | consistent | **drifted** | +1 |
| 6 | lines | consistent | consistent | 0 |

- Consistency: **arm A 6/6 (100%) vs arm B 4/6 (67%)**
- Mean paired delta **+0.33**, bootstrap 95% CI [+0.00, +0.67]
- Discordant pairs 2–0 for A; sign test p = 0.50 (N far too small for
  significance — this is a directional signal, as designed for a pilot)
- Anti-prior subset (n=3): mean delta **+0.67** — arm B drifted 2/3 times
  when the standard contradicted its natural choice; arm A never drifted.
- The three prior-aligned pairs behaved as predicted (uninformative),
  validating the assignment-alternation design.

**Power notes.** To detect a 33pp delta (100% vs 67%) at α=0.05 / power
0.8 (two-proportion or paired sign framework) ≈ 25–30 pairs — consistent
with the original powered-study sizing. Two scaling options:
`--pairs 30` on the current mixture design (interprets the mixture
effect), or run the informative subset only by fixing
`--assignment json` for all pairs (maximum discrimination per session
cost; slightly overstates the real-world effect since prior-aligned
standards need no memory). Continue the same workdir to accumulate:
resume skips finished pairs.
