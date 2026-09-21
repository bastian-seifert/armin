# ARMIN on Claude Code

Status: **v1 plugin shipped** (`claude-plugin/`), published via the repo's own marketplace; community-marketplace submission pending.

- Install: `/plugin marketplace add bastian-seifert/armin` → `/plugin install armin@armin`
- User-facing docs: [`claude-plugin/README.md`](../claude-plugin/README.md)
- Reference implementation (OpenCode, in-process): [`.opencode/plugins/armin.ts`](../.opencode/plugins/armin.ts)

This document records the port: the verified hook contract, the mapping, the engine changes the port required, and what was deliberately left out.

---

## The four-event loop, mapped to Claude Code hooks

The harness requirement for the ARMIN loop is: a plugin can see every tool call and inject text into the next turn without forking. Claude Code's hook contract satisfies this:

| ARMIN need | Claude Code hook | Notes |
|---|---|---|
| post-tool capture | `PostToolUse` with `"async": true` | Background process; the turn never waits. Input carries `tool_name`, `tool_input`, `tool_response`. |
| pre-turn injection | `UserPromptSubmit` | Plain stdout is added to Claude's context (capped at 10,000 chars). The hook blocks the turn, so it must stay fast — ARMIN only does a local HTTP call. |
| pre-compact | `PreCompact` | **Notify-only.** See the deviation below. |
| session start / after compaction | `SessionStart` (matcher sources: `startup`, `resume`, `clear`, `compact`, `fork`) | Fires again with `source: "compact"` after every compaction — this is the rehydration point. |
| (bonus) | `PostCompact` | Delivers the generated `compact_summary`; ARMIN ingests it. |

### Deviation from the plan comment: PreCompact cannot contribute

The original loop sketch assumed the brief could enter the compaction prompt via a `PreCompact` hook. In Claude Code's actual contract (verified Sept 2026), `PreCompact` hooks *receive* `trigger` and `custom_instructions` but can only **block** compaction or no-op — they cannot append to the compaction instructions or replace the transcript. `PostCompact` is purely observational.

The injection-plus-rehydration design still works:

1. The brief is injected per-turn via `UserPromptSubmit` and saved into the session transcript, so the compactor sees it as conversation context.
2. After compaction, `SessionStart(source: compact)` injects a *fresh* brief computed from the graph — the durable set is rehydrated from the DB, not from whatever the summarizer happened to keep.
3. `PostCompact` feeds the summary back into the graph for extraction.

What is still missing is vendor support for *shaping* the compaction prompt itself. That is worth a feature request against Claude Code (draft: [`docs/feature-requests.md`](feature-requests.md)); it is not worth a fork.

### Deviation: assistant prose is not exposed to hooks

OpenCode pushes assistant message events to the plugin; Claude Code hooks never see the model's output text. The adapter recovers it by reading the tail of the session transcript (`transcript_path` in every hook input) with a byte-offset cursor in the hook state directory. Sidechain entries are skipped, partially written final lines are deferred, and a shrunken transcript resets the cursor. A feature request to expose assistant text to hooks would remove the workaround (same draft doc).

## Architecture: hooks + daemon

Hooks are short-lived processes, unlike OpenCode's in-process plugin. Spawning the Axum sidecar per hook invocation would (a) cost ~200–400 ms on the prompt path and (b) lose the sled graph lock. So `armin-hook` (new crate) manages a detached per-project daemon:

```
Claude Code ──hooks──► armin-hook (short-lived process, bundled in bin/)
                          │ flock(~/.local/state/armin/claude/<slug>/.engine.lock)
                          │ probe recorded endpoint (engine.json: port/token/pid)
                          │ spawn detached if needed: bundled engine, --port 0,
                          │ --auth-token <random>, --idle-exit 1800
                          ▼
                     armin-engine daemon (per project, keyed by git origin)
                     POST /ingest ◄─ tool calls, prompts, assistant prose
                     GET  /state/brief?files=…  → stdout injection
                     POST /import               ← CLAUDE.md cold start
```

- One daemon per project → multiple concurrent sessions share one graph, and the db (sled) has exactly one owner.
- `SessionStart` pre-warms the daemon so the first `UserPromptSubmit` is a cheap local call.
- The daemon exits gracefully (sled flush) after `--idle-exit` seconds without any request; the next hook starts a fresh one. This is the only engine change the port needed.
- State: `~/.local/state/armin/claude/<project-slug>/` (`engine.json`, `session-<sid>.json` with transcript cursor + recent-files ring + last brief hash); graphs under `~/.local/share/armin/claude/<project>.db`.
- Project identity mirrors the OpenCode plugin: git origin remote, else the working directory path (with a short hash suffix to prevent slug collisions).

## What v1 does not include (deliberately)

- **MCP tools** (`query_graph`, `record_decision`, …): the engine only exposes MCP over HTTP today, and in Claude Code the daemon owns the sled lock. A v1.1 bridge (engine `/mcp` route + stdio bridge subcommand) is planned; the core loop does not need it.
- **Skills**: the brief carries a standing instruction; a `skills/armin/SKILL.md` teaching `query_graph`/`record_decision` usage ships with the MCP bridge.
- **A/B harness on Claude Code**: `scripts/abtest/` drives OpenCode sessions; extending it to `claude -p` is follow-up work to prove parity.
- **Windows**: the dispatcher exits 0 and ARMIN stays invisible.

## Testing

- Unit tests in `armin-core/hook/src/*` (26): transcript parsing, file extraction, response summarization, state round-trips, project keying.
- Integration tests spawn the real `armin-engine`: ensure → idempotent re-use → capture → CLAUDE.md import → brief; pinned-port-busy fallback; graceful inert mode when the binary is missing. Run: `cargo test -p armin-hook` (workspace build provides the engine binary).
- Catalog checks: all three JSON files parse and agree on the plugin name; hook commands map to the five subcommands.

## Publishing status

| Step | Status |
|---|---|
| Own marketplace in repo (`.claude-plugin/marketplace.json`, archive source pinned to release assets) | done — active after the first `v*` tag |
| Release CI builds hook+engine per platform, zips the plugin, uploads, pins catalog to `main` | done (`.github/workflows/release.yml`) |
| `claude plugin validate ./claude-plugin --strict` gate | run locally before release (no Claude CLI in CI yet) |
| Community marketplace (`anthropics/claude-plugins-community`) | pending submission — guide: [`docs/community-submission.md`](community-submission.md) |
| Official marketplace (`claude-plugins-official`) | curated by Anthropic, no application process |

## Local development

```bash
cd armin-core && cargo build --release -p armin-hook -p armin-engine
cp target/release/armin-hook ../claude-plugin/bin/armin-hook-linux-x64
cp target/release/armin-engine ../claude-plugin/bin/armin-engine-linux-x64
# then, in a scratch project:
claude --plugin-dir /path/to/armin/claude-plugin
```

Debug with `claude --debug` (look for `armin-hook` lines) and check the daemon log in `~/.local/state/armin/claude/<project-slug>/engine.log`.
