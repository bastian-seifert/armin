# ARMIN

**Agent Reasoning Memory & Introspection Network. Queryable decision memory for AI agents.**

ARMIN monitors AI agents as they work. It watches tool calls and prose, figures out what actually matters for future sessions, and builds a small graph of decisions and open items as the session unfolds. When the session is over — or when context compaction wipes the "why" — that reasoning survives, queryable and injectable.

---

## What problem this solves

AI agent sessions produce decisions. They also produce the context that explains those decisions — constraints nobody wrote down, questions that were raised but never answered, choices that bind future work. That context lives in the agent's context window, or in a log nobody reads, or nowhere at all.

The failure mode is specific: **agents don't drift by forgetting, they drift by reverting to their defaults.** When a fresh session re-makes a choice the previous session already made — and the remembered choice contradicts the model's natural preference — the re-derivation feels correct to the agent. Memory has to arrive as a constraint at the moment of re-decision.

ARMIN does exactly that: passive capture, a *durable-knowledge* graph, and a compact reasoning-state brief pushed into the agent's context every turn and after compaction.

---

## The graph: durable knowledge only

The graph holds only what a **future session** needs to keep the codebase consistent:

| Node | Edit-time question it answers |
|---|---|
| `Decision` | Was this decided before — and why? |
| `Rule` | What binds this file/module — conventions, constraints, requirements? |
| `OpenItem` | What is known broken, unfinished, or unresolved? |

Everything else a session produces is *episodic*: it lives in pipeline scratch (in-memory, per session), feeds the live debt view, and either gets promoted into a durable node or dies with the session. Tool calls are never nodes — read tools become provenance (`event_id`, files, commit) attached to durable knowledge; mutating and verifying calls land in scratch.

Edges: `Supersedes`, `Refutes`, `Resolves`, `RelatesTo`. Debt detection is graph-internal (`UnresolvedOpenItem`) plus cross-layer checks against session activity: `UnverifiedChange` (edited, never tested), `FailedVerification` (failing check, no fix attempted), and `RuleViolation` (failed check on files covered by a Rule).

The taxonomy is deliberately minimal and measured. `Rule` exists for deterministic paths — doc import and the scoped brief — but is *not* extracted from prose yet; `Fact` (verified gotchas) and `Lesson` (tried-and-failed) remain documented future extensions. Rationale and evidence: `armin-core/graph/src/types.rs`, `data/backtest/`.

---

## How it works

```
opencode session
    │
    ▼
plugin (.opencode/plugins/armin.ts)
├── CAPTURE  tool calls + prose → engine /ingest (no added latency)
├── EXTRACT  background batches: TypeSafe System One judgments by default
│            (jev: verbatim sentences, select-don't-generate — hallucinated
│            memory is impossible; direct via typesafe.ai or relayed through
│            OpenRouter); generative LLM is the opt-out and the automatic
│            fallback when no Jev key is set
├── PUSH     reasoning-state brief every turn + into compaction context
└── ENGINE   per-project graph DB (sled), keyed by git origin remote —
             memory follows the project, not the checkout
```

Three crates (plus the **`armin-hook`** launcher for hook-based harnesses):

- **`armin-graph`** — thread-safe `petgraph::StableDiGraph` + sled persistence, debt detection, decision status, session diff, communities, scoped retrieval (BM25 / embedding / hybrid).
- **`armin-extraction`** — provider-agnostic batch extraction: `jev-native` (typed judgments, no generative LLM, hallucination-proof verbatim nodes) or an extraction LLM (Anthropic/OpenAI).
- **`armin-engine`** — the sidecar: Axum API, auth, batched background extraction worker, resolution linking, reasoning-state brief, metrics. Handshake via `ARMIN_PORT` on stdout, graceful SIGTERM with sled flush.

A standalone **`armin-server`** (same graph, event stream + WebSocket + React frontend) is included for exploration and demos.

---

## Quickstart: opencode middleware (recommended)

```bash
# 1. Install (pick one):
npx armin-opencode install       # npm: fetches the release binary + registers
                                 # the plugin (no Rust toolchain needed)
scripts/install.sh               # or build from source (needs cargo)

# 2. Give Jev a key — jev extraction is the default and needs one.
#    Two relays serve the same System One API; pick either:
export TYPESAFE_AI_API_KEY=...        # typesafe.ai direct
# export OPENROUTER_API_KEY=...       # or via OpenRouter (billed there;
#                                     # jev routes to ~typesafe/jev-latest)
#    …or put it in the plugin options in your global opencode config instead
#    (the installer writes this form; options win over the legacy "armin"
#    section, which opencode strips from its resolved config anyway):
#    "plugin": [["armin-opencode@<version>", { "typesafeKey": "..." }]]
#    "plugin": [["armin-opencode@<version>", { "jevProvider": "openrouter",
#                                              "openrouterKey": "..." }]]
#    Pin the engine port (default: auto-assign): { "port": 4545 } options

# 3. Done — `armin install` registers
#    "plugin": [["armin-opencode@<version>", { "enabled": true, ... }]]
#    in your global opencode config (opencode installs the package with its
#    SDK into its own plugin cache on next start). Verify with `armin doctor`.
#    Escape hatch (env wins over config):
# export ARMIN_ENABLED=1              # enable without the config entry
# optional knobs:
# ARMIN_ENGINE_BIN=~/.local/bin/armin-engine
# ARMIN_MODEL=...                extraction model override
# ARMIN_EXTRACTION_MODE=llm|jev  (default: jev — System One, verbatim nodes;
#                                 set llm to opt out; without a Typesafe/
#                                 OpenRouter key the engine falls back to
#                                 LLM automatically)
# ARMIN_JEV_PROVIDER=openrouter  relay for jev: typesafe|openrouter
#                                 (default: auto from which key is set)
# ARMIN_JEV_MODEL=...            jev model override (default jev-1.13.0,
#                                 or jev-latest via OpenRouter)
# ARMIN_BATCH_MS=15000           extraction debounce window
# ARMIN_BATCH_EVENTS=10          events per extraction call
# ARMIN_DB_DIR=~/.opencode/armin graph storage (per-project, keyed by git origin)
# ARMIN_PORT=4545                fixed engine port (default: auto-assign per
#                                session; auto-falls back to a free port if busy)
# ARMIN_DEBUG=1                  verbose plugin logging
```

First run in a project: if an `AGENTS.md` or `CLAUDE.md` exists, ARMIN imports it into the graph as `Rule`/`Decision`/`OpenItem` nodes — you get a useful brief on session 1, before any extraction has run.

**Reasoning survives compaction**: the plugin injects decisions and open items into the compaction prompt, so a compacted session retains its "why".

**Inspect the graph**: the engine serves a live status page at `http://127.0.0.1:<port>/ui` (the plugin logs the URL at startup with `ARMIN_DEBUG=1`).

Cost/latency model: tool calls are captured with **zero** LLM calls; prose is batched (one cheap call per 15s window by default); the brief is computed in Rust (<10ms). The agent's turns never wait on the graph. Without any API key you still get capture, unverified-edit/failing-check warnings, agent writes and the AGENTS.md import — everything else needs extraction.

**Editing with memory**: the brief gains a "Binding here" section — decisions and rules scoped to the files you have been editing — and a standing instruction: if a new request conflicts with a remembered decision, say so explicitly before deviating.

---

## Quickstart: Claude Code plugin

```bash
claude plugin marketplace add bastian-seifert/armin
claude plugin install armin@armin
```

That's it — the plugin bundles the Rust sidecar (Linux x64, macOS x64/arm64). ARMIN hooks in passively: capture on `PostToolUse` (async, never waits), the reasoning-state brief injected from `UserPromptSubmit` stdout, re-injected on `SessionStart` after compaction, and the compaction summary fed back via `PostCompact`. Extraction is the same jev/LLM stack as the OpenCode plugin (`TYPESAFE_AI_API_KEY` / `OPENROUTER_API_KEY` env vars, or the plugin's config prompt). Port details and status: **`docs/claude-code.md`**.

---

## The reasoning-state brief

Every turn, the engine renders a compact block that is injected into the agent's system context:

```
<reasoning-state nodes="14" edges="6">
Decisions (4):
- [Validated] Use JWT for the auth gateway
- ...
Rules (1):
- You must never print timestamps in CLI output
Open items (2):
- Open item 'Refresh-token rotation needed?' raised by agent has not been resolved
</reasoning-state>
```

It carries only the durable set — settled decisions, binding rules, unresolved threads — which is what the A/B study below tested.

---

## Does it work? The consistency study

`scripts/abtest/` contains a paired A/B harness with an ARMIN-free checker (git diffs + functional probes + byte-level format sniffing — no self-grading). The fixture: two *equivalently valuable* data-file formats; the same choice must be made twice, in different sessions and different files; session 2 runs on a pristine checkout, so memory is the only bridge.

Result (N=6 paired middleware runs, alternating which option the session-1 standard forces):

| | arm A (memory) | arm B (no memory) |
|---|---|---|
| cross-session consistency | **6/6 (100%)** | 4/6 (67%) |

Without memory, agents reverted to their natural preference exactly when the remembered choice was anti-prior — and never noticed the conflict. With the brief, sessions cited it as evidence ("a prior decision already tracks this"). Full protocol, per-pair results, and the power analysis: **`scripts/abtest/README.md`**. (The study ran on the pre-v0.1 engine; a fresh middleware pair on v0.1 reproduced the discrimination exactly.)

`data/backtest/` holds the extraction backtest corpus (a real design conversation + hand annotations + the current result artifact).

---

## Standalone server + frontend

```bash
cargo run -p armin-server -- \
  --stream data/backtest/conversation.jsonl \
  --db-path data/graph.db \
  --speed 8
cd frontend && npm install && npm run dev   # http://localhost:5173
```

Events can be pushed via `POST /ingest` instead of `--stream`. Provider selection: `LLM_PROVIDER=openai` + `OPENAI_API_KEY`, or Anthropic by default. `LLM_MODEL` overrides the model. (The demo server runs generative extraction; the middleware defaults to jev.)

### Server flags

| Flag | Default | Description |
|------|---------|-------------|
| `--stream` | optional | Path to `stream.jsonl` event input |
| `--speed` | `4` | Playback multiplier (0=max, 1=realtime, 2/4/8) |
| `--bind` | `127.0.0.1` | Bind address (loopback by default — the graph is sensitive) |
| `--port` | `8080` | HTTP listen port |
| `--auth-token` | off | Require bearer token on every request |
| `--db-path` | — | Sled persistence directory (in-memory if omitted) |
| `--retriever` | `bm25` | Graph query mode: `bm25`, `embedding`, or `hybrid` |

### Engine API (sidecar)

| Endpoint | Purpose |
|----------|---------|
| `POST /api/v1/ingest` | Batch-ingest events |
| `GET /api/v1/state/brief?files=a.rs` | Reasoning-state brief; `files=` scopes a "Binding here" section |
| `POST /api/v1/import` | Deterministic AGENTS.md/CLAUDE.md import (idempotent) |
| `GET /ui` | Live status page (graph, debt, brief preview) |
| `POST /api/v1/query` | Deterministic graph query (BM25 + trace + template answer) |
| `POST /api/v1/agent/decision` / `question` / `resolve` / `invalidate` | Agent writes |
| `GET /api/v1/debt` `/decisions` `/risks` `/summary` `/communities` | Analytics |
| `GET /api/v1/metrics` | Pipeline counters |
| `GET /api/v1/health` | Liveness + mode |

Handshake: the engine prints `ARMIN_PORT=<port>` on stdout line 1; graceful SIGTERM with sled flush. The port is OS-assigned per session by default (`--port 0`); pin it with the `"port": 4545` plugin option (or the legacy `"armin": { "port": 4545 }` config section) or the `ARMIN_PORT` env var (installer prompt available) — a busy pinned port auto-falls back to a free port.

### MCP

`armin-server` exposes MCP at `/mcp` for any MCP client (stdio via `--mcp-stdio`):

```json
{ "mcpServers": { "armin": { "url": "http://localhost:8080/mcp" } } }
```

Tools: `query_graph`, `record_decision`, `raise_question`, `resolve_question`, `invalidate_assumption`, `get_decisions`, `get_risks`, `get_debt`, `get_summary`, `get_community`.

---

## Running the tests

```bash
cd armin-core && cargo test --workspace
```

The integration test in `armin-core/engine/tests/integration.rs` spawns the real `armin-engine` binary and verifies the full middleware flow: auth → ingest → agent writes → brief → query → restart persistence → idempotent ingestion.

The opencode plugin typechecks with:

```bash
bunx tsc --noEmit --strict --types bun --moduleResolution bundler \
  --module es2022 --target es2022 --skipLibCheck .opencode/plugins/armin.ts
```

---

## License

Apache-2.0 — see [LICENSE](LICENSE).
