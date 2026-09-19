#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["typesafe-sdk"]
# ///
"""Tier-1 paired multi-session A/B pilot for ARMIN's reasoning-state brief.

Protocol (one fixture = one paired example):
  phase stage  — copy the fixture template into <workdir>/arm{A,B}, git init,
                 record baseline sha
  phase task1  — run a fresh `opencode run` session per arm that forces
                 binding decisions (storage backend, config mechanism);
                 capture the transcript from the local opencode store and
                 commit the resulting state as the "session1" sha
  phase brief  — Jev-native scan (select-don't-generate) of each arm's own
                 session-1 transcript; arm A gets the decisions as a
                 <reasoning-state> block in AGENTS.md, arm B gets a neutral
                 placeholder (same wrapper, no content)
  phase task2  — fresh session per arm (no --continue) whose correct solution
                 depends on respecting the session-1 decisions
  phase check  — run scripts/abtest/check_example1.py per arm (ARMIN-free:
                 git diff of the decision header + functional probes)

Usage:
  uv run scripts/abtest/run_pilot.py --phase stage
  uv run scripts/abtest/run_pilot.py --phase task1 --arm A
  ...
  uv run scripts/abtest/run_pilot.py --phase all          # full sequence
"""
import argparse
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, os.path.join(HERE, ".."))

from jev_eval import JevClient, native_extract  # noqa: E402
import backtest as bt  # noqa: E402

OPENCODE_DB = os.path.expanduser("~/.local/share/opencode/opencode.db")
STATE_FILE = "abtest-state.json"
PLUGIN_PATH = os.path.join(ROOT, ".opencode", "plugins", "armin.ts")
ENGINE_BIN = os.path.join(ROOT, "armin-core", "target", "release", "armin-engine")


# ── state ─────────────────────────────────────────────────────────────────────

def state_path(workdir):
    return os.path.join(workdir, STATE_FILE)


def load_state(workdir):
    return json.load(open(state_path(workdir))) if os.path.exists(state_path(workdir)) else {}


def save_state(workdir, state):
    with open(state_path(workdir), "w") as f:
        json.dump(state, f, indent=2)


# ── opencode session capture ──────────────────────────────────────────────────

def make_sandbox(arm_dir, arm=None, mode="proxy"):
    """Isolated opencode home (XDG dirs) so `opencode run` cannot attach to
    the user's running workspace: auth + config are copied in, sessions are
    recorded in the sandbox DB. In middleware mode, arm A additionally gets
    the ARMIN plugin registered in the sandbox config."""
    sb = os.path.join(arm_dir, "_abtest", "opencode-home")
    data_opencode = os.path.join(sb, "data", "opencode")
    cfg_opencode = os.path.join(sb, "config", "opencode")
    os.makedirs(data_opencode, exist_ok=True)
    real_data = os.path.expanduser("~/.local/share/opencode")
    real_cfg = os.path.expanduser("~/.config/opencode")
    auth = os.path.join(real_data, "auth.json")
    if os.path.exists(auth):
        shutil.copy(auth, os.path.join(data_opencode, "auth.json"))
    if os.path.isdir(real_cfg):
        if os.path.exists(cfg_opencode):
            shutil.rmtree(cfg_opencode)
        shutil.copytree(real_cfg, cfg_opencode)
    if mode == "middleware" and arm == "A":
        cfg = os.path.join(cfg_opencode, "opencode.jsonc")
        with open(cfg, "w") as f:
            json.dump({"$schema": "https://opencode.ai/config.json",
                       "plugin": ["file://" + PLUGIN_PATH]}, f, indent=2)
    return sb


def middleware_env(arm, arm_dir, workdir):
    """Env gate for the real ARMIN middleware (engine spawns with inherited
    env): Jev-native extraction, per-arm sled DB (OUTSIDE the worktree so the
    engine's own files don't pollute the agent's context), fast batch settle."""
    env = bt.load_env()
    db_dir = os.path.join(workdir, "armin-state", f"arm{arm}")
    out = {
        "ARMIN_ENABLED": "1",
        "ARMIN_ENGINE_BIN": ENGINE_BIN,
        "ARMIN_EXTRACTION_MODE": "jev",
        "ARMIN_DB_DIR": db_dir,
        "ARMIN_BATCH_MS": "2000",
        "ARMIN_BATCH_EVENTS": "5",
        "ARMIN_JEV_MODEL": "jev-1.13.0",
    }
    key = env.get("TYPESAFE_AI_API_KEY") or env.get("TYPESAFE_API_KEY")
    if key:
        out["TYPESAFE_AI_API_KEY"] = key
    return out


def reap_engines(db_dir):
    """The plugin's dispose does not always reap the sidecar; kill any
    armin-engine holding this arm's DB (sled lock would block the next run)."""
    try:
        out = subprocess.run(["pgrep", "-x", "armin-engine", "-a"],
                             capture_output=True, text=True).stdout
        for line in out.splitlines():
            pid, _, cmd = line.strip().partition(" ")
            if str(db_dir) in cmd:
                subprocess.run(["kill", pid], capture_output=True)
    except Exception:
        pass


def run_opencode(cwd, prompt, title, timeout=600, extra_env=None,
                 arm=None, mode="proxy"):
    """Run one headless opencode session in an isolated home; return (rc, tail)."""
    sb = make_sandbox(cwd, arm=arm, mode=mode)
    env = os.environ.copy()
    env["XDG_DATA_HOME"] = os.path.join(sb, "data")
    env["XDG_CONFIG_HOME"] = os.path.join(sb, "config")
    env["PWD"] = cwd  # opencode resolves the workspace from PWD, not getcwd()
    env.update(extra_env or {})
    r = subprocess.run(
        ["opencode", "run", "--title", title, prompt],
        cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout)
    return r.returncode, (r.stdout or "")[-2000:]


def latest_session_transcript(directory, db_path):
    """Pull user/assistant prose turns of the newest session in `directory`
    from the given opencode SQLite store (sandbox DB for isolation)."""
    if not os.path.exists(db_path):
        return None
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    sid = conn.execute(
        "select id from session where directory=? order by time_updated desc limit 1",
        (directory,)).fetchone()
    if not sid:
        conn.close()
        return None
    rows = conn.execute(
        "select m.data, p.data, p.time_created from message m "
        "join part p on p.message_id = m.id "
        "where m.session_id = ? order by m.time_created, p.time_created",
        (sid[0],)).fetchall()
    conn.close()
    turns = []
    for mdata, pdata, _t in rows:
        try:
            m, p = json.loads(mdata), json.loads(pdata)
        except Exception:
            continue
        if m.get("role") not in ("user", "assistant") or p.get("type") != "text":
            continue
        text = (p.get("text") or "").strip()
        if text:
            turns.append({"role": m["role"], "text": text})
    return {"session_id": sid[0], "turns": turns}


def save_transcript(workdir, arm, phase, transcript, target_dir=None):
    path = os.path.join(target_dir or os.path.join(workdir, f"arm{arm}"),
                        "_abtest")
    os.makedirs(path, exist_ok=True)
    out = os.path.join(path, f"{phase}_transcript.json")
    with open(out, "w") as f:
        json.dump(transcript, f, indent=2, ensure_ascii=False)
    return out


# ── phases ────────────────────────────────────────────────────────────────────

TASK1_PROMPT = (
    "Work in this repository (notesctl). Implement persistent storage for the "
    "CLI: the `add` and `list` subcommands must actually store and list notes "
    "across runs. You must commit to exactly one storage backend - a single "
    "JSON file OR an SQLite database - and exactly one path-configuration "
    "mechanism - the environment variable NOTESCTL_PATH OR a config file named "
    "notesctl.json looked up in the user's home directory. Implement the "
    "chosen mechanisms in notesctl/storage.py and wire the CLI to them. At the "
    "very top of notesctl/storage.py document your two choices in exactly this "
    "machine-readable header (as comments): 'Backend: json' or 'Backend: "
    "sqlite', and 'Config: env' or 'Config: file'. Make `python3 -m notesctl "
    "add \"text\"` and `python3 -m notesctl list` work. Standard library only; "
    "do not add third-party dependencies."
)

TASK2_PROMPT = (
    "Work in this repository (notesctl). Two new requirements: (1) Add a "
    "`stats` subcommand that prints the total number of stored notes and the "
    "title of the most recent note, reading through the existing storage "
    "layer. (2) Make the storage location overridable at runtime, consistent "
    "with how this project already handles storage-path configuration. Keep "
    "the implementation consistent with the project's existing design "
    "decisions. Standard library only."
)


def phase_stage(args, state):
    fixture = os.path.join(HERE, "fixtures", FIXTURES[args.example])
    shutil.rmtree(os.path.join(args.workdir, "armin-state"), ignore_errors=True)
    for arm in args.arms:
        for d in (os.path.join(args.workdir, f"arm{arm}"),
                  os.path.join(args.workdir, f"arm{arm}-t2")):
            if os.path.exists(d):
                shutil.rmtree(d)
        arm_dir = os.path.join(args.workdir, f"arm{arm}")
        shutil.copytree(fixture, arm_dir)
        subprocess.run(["git", "init", "-q"], cwd=arm_dir, check=True)
        subprocess.run(["git", "config", "user.email", "abtest@local"],
                       cwd=arm_dir, check=True)
        subprocess.run(["git", "config", "user.name", "abtest"],
                       cwd=arm_dir, check=True)
        subprocess.run(["git", "add", "-A"], cwd=arm_dir, check=True)
        subprocess.run(["git", "commit", "-qm", "fixture baseline"],
                       cwd=arm_dir, check=True)
        # A per-arm fake origin gives the plugin a stable project identity:
        # the arm's task1 and task2 worktrees are different checkouts of the
        # same project, but the two arms must not share memory.
        subprocess.run(["git", "remote", "add", "origin",
                        f"https://abtest.local/notesctl-arm{arm}.git"],
                       cwd=arm_dir, check=True)
        state.setdefault("arms", {})[arm] = {"dir": arm_dir, "baseline_sha": git_sha(arm_dir)}
        print(f"staged arm{arm} -> {arm_dir} (baseline {state['arms'][arm]['baseline_sha'][:8]})")
    save_state(args.workdir, state)


def git_sha(workdir):
    return subprocess.run(["git", "-C", workdir, "rev-parse", "HEAD"],
                          capture_output=True, text=True).stdout.strip()


def pristine_task2_dir(args, state, arm):
    """Fresh fixture copy for example 3's second session: no session-1 code,
    so the repeated format choice can only be consistent via memory."""
    d = os.path.join(args.workdir, f"arm{arm}-t2")
    if not os.path.exists(os.path.join(d, ".git")):
        fixture = os.path.join(HERE, "fixtures", FIXTURES[args.example])
        if os.path.exists(d):
            shutil.rmtree(d)
        shutil.copytree(fixture, d)
        subprocess.run(["git", "init", "-q"], cwd=d, check=True)
        subprocess.run(["git", "config", "user.email", "abtest@local"],
                       cwd=d, check=True)
        subprocess.run(["git", "config", "user.name", "abtest"], cwd=d, check=True)
        subprocess.run(["git", "add", "-A"], cwd=d, check=True)
        subprocess.run(["git", "commit", "-qm", "fixture baseline (task2)"],
                       cwd=d, check=True)
        subprocess.run(["git", "remote", "add", "origin",
                        f"https://abtest.local/notesctl-arm{arm}.git"],
                       cwd=d, check=True)
    return d


def phase_task(args, state, phase, prompt):
    for arm in args.arms:
        arm_dir = state["arms"][arm]["dir"]
        work_dir = arm_dir
        if args.example == 3 and phase == "task2":
            work_dir = pristine_task2_dir(args, state, arm)
            state["arms"][arm]["task2_dir"] = work_dir
            if args.mode == "proxy" and arm == "A":
                # carry the session-1 brief into the fresh worktree: for arm A
                # the AGENTS.md brief IS the memory under test.
                src = os.path.join(arm_dir, "AGENTS.md")
                if os.path.exists(src):
                    shutil.copy(src, os.path.join(work_dir, "AGENTS.md"))
        extra = {}
        if args.mode == "middleware":
            if arm == "A":
                extra = middleware_env(arm, arm_dir, args.workdir)
            # arm B: no ARMIN env — plugin absent/inert by construction
        print(f"[arm{arm}] running opencode session ({phase}) in {work_dir} ...",
              flush=True)
        rc, tail = run_opencode(work_dir, prompt, title=f"abtest-{phase}-{arm}",
                                timeout=args.session_timeout, extra_env=extra,
                                arm=arm, mode=args.mode)
        db = os.path.join(work_dir, "_abtest", "opencode-home", "data",
                          "opencode", "opencode.db")
        tr = latest_session_transcript(work_dir, db)
        n_turns = len(tr["turns"]) if tr else 0
        print(f"[arm{arm}] rc={rc}, transcript turns={n_turns}, stdout tail: {tail[-160:]!r}")
        state["arms"][arm][f"{phase}_rc"] = rc
        state["arms"][arm][f"{phase}_turns"] = n_turns
        if args.mode == "middleware" and arm == "A":
            adb = os.path.join(args.workdir, "armin-state", f"arm{arm}")
            db_bytes = 0
            if os.path.isdir(adb):
                for root, _d, fs in os.walk(adb):
                    for f in fs:
                        try:
                            db_bytes += os.path.getsize(os.path.join(root, f))
                        except OSError:
                            pass
            state["arms"][arm]["armin_db_bytes"] = db_bytes
            if phase == "task1" and db_bytes == 0:
                print(f"[arm{arm}] WARNING: ARMIN engine DB is empty — "
                      "middleware did not spawn; treatment is inert!")
        reap_engines(os.path.join(args.workdir, "armin-state", f"arm{arm}"))
        if tr:
            save_transcript(args.workdir, arm, phase, tr,
                            target_dir=work_dir)
        if phase == "task1":
            subprocess.run(["git", "add", "-A"], cwd=arm_dir, check=True)
            subprocess.run(["git", "commit", "-qm", "session1 (task1 done)"],
                           cwd=arm_dir, check=False)
            state["arms"][arm]["session1_sha"] = git_sha(arm_dir)
    save_state(args.workdir, state)


def build_brief(events, args):
    """Jev-native extraction over session-1 prose; returns grouped decisions."""
    env = bt.load_env()
    api_key = env.get("TYPESAFE_AI_API_KEY") or env.get("TYPESAFE_API_KEY")
    if not api_key:
        sys.exit("no TypeSafe API key: set TYPESAFE_AI_API_KEY in .env")
    cache = os.path.join(ROOT, "data", "external", ".jev-cache-abtest.json")
    jev = JevClient(api_key, args.model, cache)
    nodes, scan, stats = native_extract(jev, events, args)
    grouped = {}
    for n in nodes:
        grouped.setdefault(n["node_type"], []).append(n["description"])
    return grouped, stats, {"calls": jev.calls, "input_tokens": jev.input_tokens}


def extract_settled_choices(arm_dir):
    """Deterministic read of the machine-readable decision header from
    notesctl/storage.py, if the session wrote one (abtest finding #1: the
    settled choices often live in tool events/committed files, not prose —
    exactly what the middleware's deterministic path would carry)."""
    path = os.path.join(arm_dir, "notesctl", "storage.py")
    choices = []
    if os.path.exists(path):
        text = open(path, encoding="utf-8", errors="replace").read()
        for pattern, label in ((r"Backend:\s*(json|sqlite)", "Backend"),
                               (r"Config:\s*(env|file)", "Config")):
            m = re.search(pattern, text)
            if m:
                choices.append(f"{label}: {m.group(1)}")
    return choices


SECTION_LABELS = {"Decision": "Decisions", "OpenItem": "Open items"}


def phase_brief(args, state):
    if args.mode == "middleware":
        print("middleware mode: memory lives in the per-arm engine DB "
              "(ARMIN_DB_DIR) — no AGENTS.md brief is built")
        save_state(args.workdir, state)
        return
    for arm in args.arms:
        arm_dir = state["arms"][arm]["dir"]
        tp = os.path.join(arm_dir, "_abtest", "task1_transcript.json")
        turns = json.load(open(tp))["turns"]
        events = [{"id": f"{arm}-t{i}", "session_id": f"abtest-{arm}",
                   "event_kind": "utterance", "text": t["text"]}
                  for i, t in enumerate(turns)]
        grouped, stats, usage = build_brief(events, args)
        settled = extract_settled_choices(arm_dir)
        print(f"[arm{arm}] jev-native: {stats['sentences']} sentences, "
              f"{stats['kept']} kept, calls={usage['calls']}, "
              f"settled-from-code={settled}")
        state["arms"][arm]["brief"] = grouped
        state["arms"][arm]["brief_settled"] = settled
        order = ["Decision", "OpenItem"]
        if arm == "A":
            lines = ["<reasoning-state source=\"jev-native\" "
                     f"units=\"{stats['kept']}\">"]
            if settled:
                lines.append("Settled choices (from committed code):")
                lines += [f"- {c}" for c in settled]
            for key in order:
                items = grouped.get(key, [])
                if not items:
                    continue
                lines.append(f"{SECTION_LABELS[key]} ({len(items)}):")
                lines += [f"- {t}" for t in items]
            lines.append("</reasoning-state>")
            lines.append("")
            lines.append("Treat the decisions above as settled project "
                         "decisions: stay consistent with them unless the "
                         "user explicitly overrides. The open items are "
                         "unresolved threads from prior sessions.")
            body = "\n".join(lines)
        else:
            body = ("<reasoning-state source=\"none\">\n"
                    "No prior session context available.\n</reasoning-state>")
        header = ("# Reasoning state (auto-extracted from prior sessions)\n\n")
        with open(os.path.join(arm_dir, "AGENTS.md"), "w") as f:
            f.write(header + body + "\n")
        print(f"[arm{arm}] AGENTS.md written "
              f"({len(grouped.get('Decision', []))} decisions in brief)")
    save_state(args.workdir, state)


def phase_check(args, state):
    results = {}
    checker = {1: "check_example1.py", 2: "check_example2.py",
               3: "check_example3.py"}[args.example]
    for arm in args.arms:
        arm_dir = state["arms"][arm]["dir"]
        sha = state["arms"][arm].get("session1_sha")
        out = os.path.join(args.workdir, f"check-arm{arm}.json")
        cmd = [sys.executable, os.path.join(HERE, checker),
               "--arm", arm, "--dir", arm_dir, "--before", sha, "--out", out]
        if args.example == 3:
            t2 = state["arms"][arm].get("task2_dir")
            if t2:
                cmd += ["--t2dir", t2]
        r = subprocess.run(cmd, capture_output=True, text=True)
        results[arm] = json.load(open(out)) if os.path.exists(out) else {
            "error": r.stderr[-400:]}
    save_state(args.workdir, state)
    print("\n" + "=" * 64)
    for arm, res in results.items():
        if "error" in res:
            print(f"[arm{arm}] checker error: {res['error']}")
            continue
        if args.example == 3:
            print(f"[arm{arm}] consistent={res.get('consistent')} "
                  f"any_violation={res['any_violation']} "
                  f"task_success={res['task_success']}")
        else:
            print(f"[arm{arm}] decision_violation={res['decision_violation']} "
                  f"any_violation={res['any_violation']} "
                  f"task_success={res['task_success']}")
        for name, c in res["checks"].items():
            print(f"    {name:<24} {c.get('verdict')}")
    # persist comparison
    with open(os.path.join(args.workdir, "pilot-summary.json"), "w") as f:
        json.dump(results, f, indent=2, default=str)
    return results


TASK1_PROMPT_V2 = (
    "Work in this repository (notesctl). Implement persistent storage for the "
    "CLI: the `add` and `list` subcommands must actually store and list notes "
    "across runs. You must commit to exactly one storage backend - a single "
    "JSON file OR an SQLite database - and exactly one path-configuration "
    "mechanism - the environment variable NOTESCTL_PATH OR a config file named "
    "notesctl.json looked up in the user's home directory. Implement it in "
    "notesctl/storage.py and wire the CLI to it. Standard library only. "
    "IMPORTANT - two binding constraints from our team discussion that you "
    "must honor and that must NOT be written into the code, README, or "
    "comments (they live in our external design doc, not in this repo): "
    "(1) The CLI must never print timestamps anywhere in its output - the "
    "surrounding UI does its own formatting. (2) Everything persistent must "
    "go through the single storage backend chosen above - no other storage "
    "mechanism anywhere in the tool, so the tool stays auditable with "
    "standard tools. Finish by restating your two choices (backend and "
    "config mechanism) explicitly in your final summary."
)

TASK2_PROMPT_V2 = (
    "Work in this repository (notesctl). Two new requirements: (1) Add a "
    "`stats` subcommand that prints the total number of stored notes and the "
    "most recent note's title. (2) Add an `export` subcommand that writes all "
    "notes to a single output file; make the export robust against crashes - "
    "a half-written export file must never be readable as a valid export. "
    "Standard library only."
)

def task1_prompt_v3(forced: str) -> str:
    """Task 1 with the transcript-only team standard forced to `json` or
    `lines`. The powered study alternates the assignment: when the forced
    option matches the model's prior the pair is uninformative (both arms
    consistent), when it is anti-prior the memory effect discriminates."""
    assert forced in ("json", "lines")
    opt = ("the JSON format (a single JSON array of note strings)" if forced == "json"
           else "the LINE-BASED format (one note per line, UTF-8, no quoting)")
    return (
        "Work in this repository (notesctl). Implement persistent storage for the "
        "CLI: the `add` and `list` subcommands must actually store and list notes "
        "across runs. Implement it in notesctl/storage.py and wire the CLI to it. "
        "Store notes in the file given by the NOTESCTL_PATH environment variable "
        "(default notes.data in the current directory when unset). You must pick "
        "exactly ONE data format for the notes file: a single JSON array (an "
        "array of note strings, UTF-8), OR the line-based format (one note per "
        "line, UTF-8, no quoting). Both are fully equivalent for this tool's "
        "needs. IMPORTANT - one binding standard from our team discussion, which "
        "you must honor and which must NOT be written into the code, README, or "
        "comments (it lives in our external design doc, not in this repo): the "
        f"team standardized on {opt} for all notesctl data files. "
        "Standard library only. Finish by restating your chosen data format "
        "explicitly in your final summary."
    )

TASK2_PROMPT_V3 = (
    "Work in this repository (notesctl). Two things: (1) Implement persistent "
    "storage for the CLI: the `add` and `list` subcommands must actually store "
    "and list notes across runs; store notes in the file given by the "
    "NOTESCTL_PATH environment variable (default notes.data in the current "
    "directory when unset). (2) Add an `export` subcommand that writes all "
    "stored notes to a single export file (NOTESCTL_EXPORT environment "
    "variable, default notes-export.data in the current directory when unset), "
    "reading through the storage layer. You must pick exactly ONE data format "
    "and use it for BOTH files: a single JSON array (an array of note "
    "strings, UTF-8), OR the line-based format (one note per line, UTF-8, no "
    "quoting). Both are fully equivalent for "
    "this tool's needs - choose the one most consistent with this project's "
    "existing decisions. Keep the implementation consistent with the project's "
    "established conventions. Standard library only. Finish by restating your "
    "chosen data format explicitly in your final summary."
)

TASK2_PROMPT_V3 = (
    "Work in this repository (notesctl). Two things: (1) Implement persistent "
    "storage for the CLI: the `add` and `list` subcommands must actually store "
    "and list notes across runs; store notes in the file given by the "
    "NOTESCTL_PATH environment variable (default notes.data in the current "
    "directory when unset). (2) Add an `export` subcommand that writes all "
    "stored notes to a single export file (NOTESCTL_EXPORT environment "
    "variable, default notes-export.data in the current directory when unset), "
    "reading through the storage layer. You must pick exactly ONE data format "
    "and use it for BOTH files: a single JSON array (an array of note "
    "strings, UTF-8), OR the line-based format (one note per line, UTF-8, no "
    "quoting). Both are fully equivalent for this tool's needs - choose the "
    "one most consistent with this project's existing decisions. Keep the "
    "implementation consistent with the project's established conventions. "
    "Standard library only. Finish by restating your chosen data format "
    "explicitly in your final summary."
)

PROMPTS = {1: (TASK1_PROMPT, TASK2_PROMPT), 2: (TASK1_PROMPT_V2, TASK2_PROMPT_V2),
           3: (None, TASK2_PROMPT_V3)}  # task1 built via task1_prompt_for()
FIXTURES = {1: "notesctl", 2: "notesctl-v2", 3: "notesctl-v3"}

def task1_prompt_for(args):
    if args.example == 3:
        return task1_prompt_v3(args.assignment)
    return PROMPTS[args.example][0]


PHASES = {
    "stage": lambda a, s: phase_stage(a, s),
    "task1": lambda a, s: phase_task(a, s, "task1", task1_prompt_for(a)),
    "brief": lambda a, s: phase_brief(a, s),
    "task2": lambda a, s: phase_task(a, s, "task2", PROMPTS[a.example][1]),
    "check": lambda a, s: phase_check(a, s),
}


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--phase", required=True,
                    choices=list(PHASES) + ["all"])
    ap.add_argument("--example", type=int, default=1, choices=[1, 2, 3],
                    help="fixture/prompt set (2 = transcript-only constraints, "
                         "3 = cross-session consistency, no trap)")
    ap.add_argument("--mode", default="proxy", choices=["proxy", "middleware"],
                    help="treatment: static AGENTS.md brief (proxy) or the "
                         "real ARMIN opencode middleware (Jev-native engine)")
    ap.add_argument("--arms", nargs="+", default=["A", "B"])
    ap.add_argument("--workdir", default="/tmp/opencode/abtest-pilot")
    ap.add_argument("--model", default="jev-1.13.0")
    ap.add_argument("--native-threshold", type=float, default=0.85,
                    help="brief keep gate (minimal-criteria backtest 2026-09-19: "
                         "0.85 kept 22/46 sentences at ~90% hand-judged useful)")
    ap.add_argument("--assumption-threshold", type=float, default=0.6,
                    help="unused since the noul pass was dropped from the "
                         "minimal criteria (kept for CLI compat)")
    ap.add_argument("--assignment", default="json", choices=["json", "lines"],
                    help="example 3 only: which format the task1 team standard "
                         "forces (json = anti-prior per v5/v6 calibration)")
    ap.add_argument("--session-timeout", type=int, default=600)
    args = ap.parse_args()

    os.makedirs(args.workdir, exist_ok=True)
    state = load_state(args.workdir)
    seq = list(PHASES) if args.phase == "all" else [args.phase]
    for name in seq:
        print(f"\n=== phase {name} ===")
        PHASES[name](args, state)


if __name__ == "__main__":
    main()
