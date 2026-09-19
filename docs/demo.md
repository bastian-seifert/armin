# Demo script — ARMIN in 60 seconds

A repeatable, screen-recordable demo that shows the whole loop without an
LLM API key (deterministic mode: import + capture + warnings).

## Setup (once)

```bash
scripts/install.sh --skip-build   # or plain scripts/install.sh
```

## Script

1. **Import (cold start → session-1 value).** In a test project with an
   `AGENTS.md` containing constraints ("- You must never print timestamps
   ..."), start any opencode session with `ARMIN_ENABLED=1 ARMIN_DEBUG=1`.
   The log shows `imported AGENTS.md: N node(s)` and the /ui URL.

2. **Live status page.** Open `http://127.0.0.1:<port>/ui` — Rules,
   Decisions, Open items, and the exact brief text the agent sees.

3. **Cross-layer warning.** Let the agent edit a file without running
   tests. The /ui debt card and the brief show:
   `Unverified edits (1): Edited 'src/cli.rs' via edit — no test/lint/build
   run covered it afterwards`

4. **Warning clears.** Agent runs the test suite → the brief drops the
   warning (the debt card shows nothing outstanding).

5. **Compaction survival.** In a long session, trigger compaction — the
   compaction prompt carries decisions and open items (the "why").

## Recording tips

- 1280x720, zoom the /ui cards to ~125%.
- Record steps 2–4 as one continuous take (~45s) — the warning appearing
  and then clearing is the money shot.
- Caption each step; no audio needed.
