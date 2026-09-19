# Show HN draft

Title: **Show HN: ARMIN — decision memory for AI coding agents (Rust, open source)**

Body:

ARMIN is a small Rust sidecar that gives coding agents cross-session memory of *why* the code is the way it is. It watches an agent work (opencode plugin, passive capture — no added latency), distills durable knowledge into a tiny typed graph — Decisions, Rules, OpenItems — and pushes a compact "reasoning-state brief" into the agent's context: every turn, after compaction, and scoped to the files being edited.

The problem it targets is specific. We ran paired A/B sessions where a design decision exists only in session 1's conversation (never in the code), and session 2 must re-make the same choice in a new file. Without memory, the agent reverted to its own default preference 2/3 times — re-deriving a different choice with confident, reasonable-sounding arguments, never noticing the conflict. With the brief injected: 6/6 consistent, and the agent explicitly cited the memory as evidence.

Things that turned out to matter more than expected:

- **The brief must be tiny and durable-only.** Early versions extracted the full "argument graph" of the session (claims, evidence, assumptions...). That filled the context with instruction echoes and narration, and the useful stuff drowned. The taxonomy is now three node types, everything else is deliberately ephemeral.
- **Extraction is select-don't-generate.** Sentences are judged (kept or dropped), nodes are verbatim — hallucinated memory is impossible by construction. Runs on a cheap model, ~0.3% of session cost; the graph core is ~3k lines of Rust with sub-10ms brief computation.
- **Memory should follow the project, not the checkout.** The graph DB is keyed by git origin, so a fresh clone keeps its history. (We found the path-keyed version of this the hard way — an A/B arm silently lost its memory across a fresh checkout.)

Honest limits: the A/B pilot is N=6 pairs (directional; the study protocol and checker are in the repo and resume-safe for N≥25). The failure mode we haven't solved: when a *new* user instruction conflicts with a remembered decision, agents currently obey the new instruction — we want conflict-*flagging* instead of silent overrides.

Repo (Apache-2.0): https://github.com/bastian-seifert/armin — `scripts/install.sh` installs the sidecar and registers the opencode plugin; there's a live status page at the engine's /ui. Feedback welcome, especially on the brief format and on memory systems people actually keep enabled past week one.
