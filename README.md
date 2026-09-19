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
| `OpenItem` | What is known broken, unfinished, or unresolved? |

Everything else a session produces is *episodic*: it lives in pipeline scratch (in-memory, per session), feeds the live debt view, and either gets promoted into a durable node or dies with the session. Tool calls are never nodes — read tools become provenance (`event_id`, files, commit) attached to durable knowledge; mutating and verifying calls land in scratch.

Edges: `Supersedes`, `Refutes`, `Resolves`, `RelatesTo`. Debt detection is graph-internal (`UnresolvedOpenItem`) plus cross-layer checks against session activity (`UnverifiedChange`, `FailedVerification` — scratch layer, in progress).

The taxonomy is deliberately minimal and measured: `Rule` (binding conventions), `Fact` (verified gotchas) and `Lesson` (tried-and-failed) are documented future extensions — see `armin-core/graph/src/types.rs` and the backtest evidence in `data/backtest/`.

---

## How it works

```
opencode session
    │
    ▼
plugin (.opencode/plugins/armin.ts)
├── CAPTURE  tool calls + prose → engine /ingest (no added latency)
├── EXTRACT  background batches: TypeSafe System One judgments (jev-native,
│            verbatim sentences, select-don't-generate) or a cheap LLM
├── PUSH     reasoning-state brief every turn + into compaction context
└── ENGINE   per-project graph DB (sled), keyed by git origin remote —
             memory follows the project, not the checkout
```

Three crates:

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

# 2. Enable it per environment (the plugin itself is global, the engine opt-in)
export ARMIN_ENABLED=1
# optional knobs:
# ARMIN_ENGINE_BIN=~/.local/bin/armin-engine
# ARMIN_MODEL=...                extraction model override
# ARMIN_EXTRACTION_MODE=llm|jev  (default: llm; jev needs TYPESAFE_AI_API_KEY)
# ARMIN_BATCH_MS=15000           extraction debounce window
# ARMIN_BATCH_EVENTS=10          events per extraction call
# ARMIN_DB_DIR=~/.opencode/armin graph storage (per-project, keyed by git origin)
# ARMIN_DEBUG=1                  verbose plugin logging
```

First run in a project: if an `AGENTS.md` or `CLAUDE.md` exists, ARMIN imports it into the graph as `Rule`/`Decision`/`OpenItem` nodes — you get a useful brief on session 1, before any extraction has run.

**Reasoning survives compaction**: the plugin injects decisions and open items into the compaction prompt, so a compacted session retains its "why".

**Inspect the graph**: the engine serves a live status page at `http://127.0.0.1:<port>/ui` (the plugin logs the URL at startup with `ARMIN_DEBUG=1`).

Cost/latency model: tool calls are captured with **zero** LLM calls; prose is batched (one cheap call per 15s window by default); the brief is computed in Rust (<10ms). The agent's turns never wait on the graph. Without any API key you still get capture, unverified-edit/failing-check warnings and agent writes — everything else needs extraction.

**Editing with memory**: the brief gains a "Binding here" section — decisions and rules scoped to the files you have been editing — and a standing instruction: if a new request conflicts with a remembered decision, say so explicitly before deviating.

---

## The reasoning-state brief

Every turn, the engine renders a compact block that is injected into the agent's system context:

```
<reasoning-state nodes="14" edges="6">
Decisions (4):
- [Validated] Use JWT for the auth gateway
- ...
Open items (2):
- Open item 'Refresh-token rotation needed?' raised by agent has not been resolved
</reasoning-state>
```

It carries only the durable set — settled decisions and unresolved threads — which is what the A/B study below tested.

---

## Does it work? The consistency study

`scripts/abtest/` contains a paired A/B harness with an ARMIN-free checker (git diffs + functional probes + byte-level format sniffing — no self-grading). The fixture: two *equivalently valuable* data-file formats; the same choice must be made twice, in different sessions and different files; session 2 runs on a pristine checkout, so memory is the only bridge.

Result (N=6 paired middleware runs, alternating which option the session-1 standard forces):

| | arm A (memory) | arm B (no memory) |
|---|---|---|
| cross-session consistency | **6/6 (100%)** | 4/6 (67%) |

Without memory, agents reverted to their natural preference exactly when the remembered choice was anti-prior — and never noticed the conflict. With the brief, sessions cited it as evidence ("a prior decision already tracks this"). Full protocol, per-pair results, and the power analysis: **`scripts/abtest/README.md`**.

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

Events can be pushed via `POST /ingest` instead of `--stream`. Provider selection: `LLM_PROVIDER=openai` + `OPENAI_API_KEY`, or Anthropic by default. `LLM_MODEL` overrides the model.

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
| `GET /api/v1/state/brief` | Compact reasoning-state markdown for prompt injection |
| `POST /api/v1/query` | Deterministic graph query (BM25 + trace + template answer) |
| `POST /api/v1/agent/decision` / `question` / `resolve` / `invalidate` | Agent writes |
| `GET /api/v1/debt` `/decisions` `/risks` `/summary` `/communities` | Analytics |
| `GET /api/v1/metrics` | Pipeline counters |
| `GET /api/v1/health` | Liveness + mode |

Handshake: the engine prints `ARMIN_PORT=<port>` on stdout line 1; graceful SIGTERM with sled flush.

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
