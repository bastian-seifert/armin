# Feature request drafts

Two vendor requests came out of the Claude Code port. Both are filed as issues (not forks): the hook contract is otherwise sufficient, and injection-plus-rehydration is the design the vendors themselves validated.

---

## 1. Claude Code: let PreCompact hooks contribute instructions to the compaction prompt

**Repo:** anthropics/claude-code

**Title:** PreCompact hooks: allow contributing context to the compaction prompt (and optionally the PostCompact summary instructions)

**Body:**

`PreCompact` hooks currently receive `trigger` and `custom_instructions` but can only block compaction or no-op (`systemMessage`/`continue` are discarded). There is no way for a hook to append instructions to the summarizer.

**Why this matters**

We maintain memory middleware (ARMIN) that captures a session's durable set — settled decisions, binding rules, unresolved open items — into a local graph. For continuity across compaction we currently:

1. inject a short `<reasoning-state>` block every turn (it therefore sits in the transcript the compactor reads), and
2. re-inject a fresh brief on `SessionStart(source: compact)`.

That is better than losing the context entirely, but it depends on the summarizer paying attention to a system reminder rather than the user explicitly asking the compactor to preserve specific constraints. The reliable contract we want is:

> When compacting, include the text provided by PreCompact hooks in the compaction instructions — the same channel `UserPromptSubmit` already provides via `additionalContext`.

Proposed shape (JSON output from a PreCompact hook):

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreCompact",
    "additionalContext": "Preserve these decisions and open items in the summary: ..."
  }
}
```

Blocking compaction is not a substitute: skipping auto-compact only delays it to the next request, and blocking recovery compaction fails the request.

Related request from the same project: exposing assistant message text to hooks (we currently parse the transcript JSONL tail). Both features serve the same use case — passive memory that survives compaction — and hook-based solutions avoid forks of the harness.

---

## 2. Codex CLI: let PreCompact replace or annotate the compaction transcript

**Repo:** openai/codex

**Title:** Plugins/hooks: allow PreCompact to annotate or replace the compaction input

**Body:**

Codex CLI's plugin hook family (`PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `PreCompact`, `PostCompact`) already supports passive capture and unbidden injection — the official docs even list "summarize chats to create persistent memories" as a hook use case. One gap prevents that use case from being fully implemented by an unprivileged hook:

`PreCompact` can observe the transcript but cannot (a) contribute custom instructions to the compaction prompt, or (b) return replacement history.

**Concrete proposal:** allow a `PreCompact` hook to return
- `additionalContext` merged into the compaction instructions, and/or
- a replacement history that the summarizer consumes.

This is the same gap Claude Code has (filed there in parallel); the injection-plus-rehydration pattern is what we ship today, but both harnesses would benefit from letting memory tools bind constraints *at the moment of summarization* instead of hoping a prepended user blob survives attention.

---

## 3. Claude Code: expose assistant message text to hooks

**Title:** Hooks: surface assistant message text (event or input field)

**Body:**

Hooks never see the model's output. Harnesses that do expose it (e.g. OpenCode's `message.part.updated`) let memory middleware capture the assistant's reasoning, not just user prompts and tool I/O.

Workaround in use: parse the session transcript JSONL (`transcript_path`) from a `UserPromptSubmit` hook. It works but couples extensions to the transcript's internal format (sidechain flags, partial-line writes, file replacement on compact).

Suggested surface: an `assistant_message` field on `Stop`/`StopFailure` hook input, or a dedicated per-turn event carrying the final assistant text blocks.
