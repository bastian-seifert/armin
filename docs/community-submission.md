# Submitting the ARMIN plugin to Anthropic's community marketplace

The community marketplace (`anthropics/claude-plugins-community`) is the realistic "verified by Anthropic" tier: plugins there have **passed Anthropic's automated validation and safety screening**, are pinned to a commit SHA, and auto-sync nightly. The official marketplace (`claude-plugins-official`) is curated at Anthropic's discretion with no application process.

## Preconditions

1. **Validation green.** From the repo root:
   ```bash
   claude plugin validate ./claude-plugin --strict
   ```
   The review pipeline runs the same check plus safety screening. Fix errors; `--strict` warnings also fail.
2. **A real release exists.** Tag `v0.1.0` (or later) so the release workflow has produced `armin-claude-plugin-vX.Y.Z.zip` with its sha256, and `marketplace.json` is pinned. CI does this automatically on tag push.
3. **Bundled binaries are expected by the manifest.** `hooks/hooks.json` references only `${CLAUDE_PLUGIN_ROOT}/bin/*` — no runtime downloads, no network beyond localhost. This matters for safety screening; keep it that way.

## Submission

Use the Console form (authors without a claude.ai Team/Enterprise org):

1. Go to **platform.claude.com/plugins/submit**.
2. Fill in the repository (`bastian-seifert/armin`), the plugin path (`claude-plugin`), and description. Suggested description:
   > ARMIN — queryable decision memory for Claude Code. Passively captures tool calls and prose into a local reasoning graph (decisions / rules / open items, keyed by git origin), pushes a compact reasoning-state brief into context every turn, and rehydrates it after compaction. Fully local (loopback, bearer token); extraction is opt-in via your own API key.
3. Attach/screenshot the `validate --strict` output if asked for evidence.

claude.ai's form (`claude.ai/admin-settings/directory/submissions/plugins/new`) requires a Team/Enterprise organization with directory access; individuals should use the Console form.

## After approval

- The catalog pins the plugin to a specific commit SHA; CI bumps the pin as you push (the review pipeline syncs nightly, so allow up to a day).
- Users install with `/plugin marketplace add anthropics/claude-plugins-community` then `/plugin install armin@claude-community`.
- Keep the in-repo marketplace as the primary channel (auto-update on by default there); the community listing is a discovery boost.

## Before you submit — review checklist

- [ ] `README.md` documents exactly what the hooks do (it does — check it still does after changes).
- [ ] No network calls except: local engine, and the user's configured extraction provider. No telemetry.
- [ ] Sensitive `userConfig` options (API keys) are marked `sensitive: true` — they are.
- [ ] No `bin/`-directory requirement violated: marketplace distribution is fine; claude.ai org-settings distribution would forbid `bin/` (not our channel).
- [ ] Version bumped in `claude-plugin/.claude-plugin/plugin.json` and catalog entry.
- [ ] CHANGELOG/README updated for the release being submitted.
