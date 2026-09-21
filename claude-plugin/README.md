# ARMIN for Claude Code

**Queryable decision memory for your agent sessions.** ARMIN watches Claude Code work, keeps a small graph of the decisions, rules, and open questions that bind the project, and pushes a compact reasoning-state brief into context every turn — so the *why* survives compaction and reaches the next session.

```
Claude Code ──hooks──► armin-hook (bundled)
                         │ ensures a per-project daemon on loopback
                         ▼
                    armin-engine (bundled, Rust sidecar)
                    capture ──► graph (sled, keyed by git origin)
                    brief   ──► injected every turn + after compaction
```

## What it does

| Hook event | What ARMIN does |
|---|---|
| `PostToolUse` (async) | Captures the tool call + touched files. Never blocks the turn. |
| `UserPromptSubmit` | Captures your prompt, flushes new assistant prose from the transcript, and injects the reasoning-state brief when it changed. |
| `SessionStart` | Ensures the engine is warm, imports `CLAUDE.md`/`AGENTS.md` into an empty graph, and injects the brief. |
| `SessionStart` after compaction (`source: compact`) | Re-injects the brief — the decisions and open items survive the compaction. |
| `PostCompact` (async) | Ingests the compaction summary into the graph. |

Everything runs on `127.0.0.1` with a per-session bearer token; no data leaves the machine. The engine serves a live graph view at `http://127.0.0.1:<port>/ui` (the port is logged with debug enabled).

## Install

From any Claude Code session:

```
/plugin marketplace add bastian-seifert/armin
/plugin install armin@armin
```

The plugin bundles the engine binary for Linux x64, macOS x64, and macOS arm64 — nothing else to install. On the first prompt you should see a `reasoning-state` reminder appear in context once memory exists.

## Extraction backends

Capture, unverified-edit warnings, and `CLAUDE.md` import work with **no keys at all**. Prose extraction (turning conversation into durable decision/rule/open-item nodes) uses, in order:

1. **jev** (System One via typesafe.ai) — default; verbatim-node extraction, hallucination-proof
2. **jev via OpenRouter** — same API, OpenRouter billing
3. **Generative LLM fallback** (Anthropic/OpenAI keys)
4. Without any key: deterministic-only (capture + import still work)

Set a key via the plugin's config prompt (stored as a sensitive option) or the standard `TYPESAFE_AI_API_KEY` / `OPENROUTER_API_KEY` environment variables — env vars always win.

## Environment overrides

| Variable | Effect |
|---|---|
| `ARMIN_ENABLED=0` | Disable ARMIN without uninstalling |
| `ARMIN_ENGINE_BIN=...` | Use a custom engine binary |
| `ARMIN_PORT=4545` | Pin the engine port (auto-falls back when busy) |
| `ARMIN_IDLE_EXIT=1800` | Seconds of inactivity before the daemon exits |
| `ARMIN_MODEL=...` | Extraction model override |
| `ARMIN_BATCH_MS` / `ARMIN_BATCH_EVENTS` | Extraction batching knobs |
| `ARMIN_DEBUG=1` | Verbose logging |

## Debugging

- Run `claude --debug` and look for `armin-hook` lines.
- Daemon logs: `~/.local/state/armin/claude/<project-slug>/engine.log`
- Graph view: `http://127.0.0.1:<port>/ui`
- Session progress files (transcript cursor, recent files): same state dir.

## Privacy & security notes

- The engine binds to `127.0.0.1` only, with a per-session random bearer token; requests require it.
- Tool-call capture sends text to the local engine only. Extraction calls out to your configured extraction provider (typesafe.ai / OpenRouter / Anthropic / OpenAI).
- Graph databases live per project (keyed by the git origin remote) under `~/.local/share/armin/claude/`.

## How this relates to the OpenCode plugin

Same engine, same brief, same event loop — ARMIN also ships for OpenCode (`npx armin-opencode`). The Claude Code plugin is the first port of the shared hook contract; Codex CLI is next.

## License

Apache-2.0
