# LinkedIn post — ARMIN v0.1 announcement

> Draft. Post as-is or trim; the hook is the finding, not the tool.

---

**AI agents don't drift because they forget. They drift because they revert to their defaults.**

We measured it. Paired A/B sessions on the same codebase task:

- Session 1 commits to a design decision (forced by a "team standard" that exists *only in the conversation* — never written to the repo).
- Session 2, fresh context, has to make the same choice again for a new file. Both options are functionally identical. Nothing in the code hints at session 1.

**Without memory:** the agent reverted to its natural preference 2 times out of 3 — confidently re-deriving a different choice, with perfectly reasonable-sounding arguments. It never "noticed" the conflict.

**With ARMIN's reasoning-state brief** (decisions, rules, and open questions carried across sessions and injected into context): **6/6 consistent.** The agent cited the memory as evidence: *"a prior decision already tracks this."*

The uncomfortable part isn't the forgetting. It's that when we drift, our reasoning feels correct — to us. Consistency across sessions can't be prompted in ("be consistent!"); it has to be carried in, at the moment of re-decision.

That's why I built ARMIN (open source, Apache-2.0): a small Rust sidecar that watches your coding agent work, keeps a durable memory of decisions / rules / open questions, and pushes a compact reasoning-state brief into the agent's context on every turn, after compaction, and scoped to the files being edited. It even imports your existing AGENTS.md so session 1 is already useful.

- Zero added latency (capture is passive; the brief is computed in Rust)
- Extraction runs on a cheap background model (~0.3% of session cost)
- A/B checker is ARMIN-free: git diffs, functional probes, byte-level format sniffing — no self-grading

v0.1 is out: `scripts/install.sh` and you're running. Works with opencode today.

Repo: https://github.com/bastian-seifert/armin

Pilot scale (N=6 pairs — directional; the powered-study protocol is in the repo and repeatable). What we haven't solved: when a *new* instruction conflicts with a remembered decision, agents still obey the new one. Flagging conflicts instead of silently overriding — that's next.

#AIagents #AgentMemory #OpenSource
