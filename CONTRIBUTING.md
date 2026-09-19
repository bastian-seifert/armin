# Contributing to ARMIN

Thanks for looking at ARMIN. This doc covers the layout, the invariants,
and how to get a PR merged.

## Layout

```
armin-core/            Rust workspace
  graph/               durable graph: types, store, debt, decisions, retrieval
  extraction/          jev-native + LLM extraction paths, prompts
  ingest/              event model (EventRecord)
  engine/              opencode middleware sidecar (axum): capture, scratch,
                       import, brief, /ui status page
  server/              standalone demo server (WebSocket + MCP + frontend)
frontend/              React demo UI for the standalone server
.opencode/plugins/     the opencode plugin (TypeScript, Bun)
scripts/               install, backtest, A/B study harness
docs/                  announcement drafts, demo script
data/backtest/         extraction corpus + gold + current result artifact
```

## The one invariant

**The graph holds only durable knowledge.** A future session must need a
node to keep the codebase consistent (`Rule`, `Decision`, `OpenItem`).
Everything episodic — narration, verification outcomes, tool calls —
lives in the in-memory scratch layer and dies with the process. If your
change puts session state into petgraph, it's wrong; if it makes the
brief bigger without making it more useful, it's wrong.

No node type without three things: a variant, an extraction/import path,
and a consumer (debt detector, brief section, or scoped query).

## Building & testing

```bash
cd armin-core && cargo test --workspace   # Rust
cd frontend && npm ci && npm run build    # frontend
bunx tsc --noEmit --strict --types bun --moduleResolution bundler \
  --module es2022 --target es2022 --skipLibCheck .opencode/plugins/armin.ts
```

CI runs all three. Clippy must be clean (`-D warnings`).

## Extraction changes

`jev` criteria strings are prompt-sensitive — if you touch the wording in
`armin-core/extraction/src/jev.rs`, treat the thresholds as untuned and
re-run `scripts/backtest.py`. The LLM path prompts live in
`armin-core/extraction/src/prompts/`.

## Studying claims

If you change what the brief contains, re-run the A/B harness
(`scripts/abtest/` — see its README). Don't widen a claim without a
measurement behind it.

## PRs

- Small, one concern per PR.
- Tests for behavior changes (the engine integration test spawns the real
  sidecar — extend it for pipeline changes).
- Describe the user-visible effect in the description.
