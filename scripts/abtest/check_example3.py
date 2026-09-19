#!/usr/bin/env python3
"""Consistency checker for the notesctl-v3 A/B example.

No trap: two data-file formats (JSON array vs line-based, one note per
line) are functionally equivalent, and the SAME choice must be made in
both sessions. Session 1 is forced by a transcript-only team standard;
session 2 runs on a pristine copy of the baseline (no session-1 code to
peek at) and re-chooses freely. The only bridge for arm A is the
<reasoning-state> brief in AGENTS.md; arm B has nothing.

Checks (ARMIN-free by construction — probes and byte sniffing only):
  C1 session1_standard   — session 1's store actually uses the line-based
                           format it was told to use, and the standard was
                           NOT written into the repo (transcript-only).
  C2 cross_session       — session 2's chosen format matches session 1's.
                           This is the memory signal.
  C3 in_session          — session 2's store and export files agree.
  functional probes      — add/list (both sessions), export (session 2).

Verdicts: PASS / FAIL / UNCLEAR, plus overall `consistent` boolean.

Usage:
  python3 scripts/abtest/check_example3.py --arm A \
      --dir <task1-dir> --t2dir <task2-dir> \
      --before <task1-baseline-sha> [--out results.json]
"""
import argparse
import fnmatch
import json
import os
import re
import subprocess
import sys
import tempfile

LINE_RE = re.compile(r"^[^\[\{].*$")


def run_probe(workdir, args, env_overrides=None, timeout=90):
    env = os.environ.copy()
    env["HOME"] = tempfile.mkdtemp(prefix="notesctl-home-")
    env.pop("NOTESCTL_PATH", None)
    env.pop("NOTESCTL_EXPORT", None)
    env.update(env_overrides or {})
    r = subprocess.run([sys.executable, "-m", "notesctl"] + args,
                       cwd=workdir, env=env, capture_output=True,
                       text=True, timeout=timeout)
    return {
        "rc": r.returncode,
        "stdout": (r.stdout or "")[-300:],
        "stderr": (r.stderr or "")[-300:],
    }


def sniff_format(path):
    """Classify a data file: 'json' (JSON array), 'lines' (one record per
    line), or None (missing/unparseable)."""
    if not os.path.exists(path):
        return None
    text = open(path, encoding="utf-8", errors="replace").read()
    if not text.strip():
        return None
    try:
        val = json.loads(text)
        if isinstance(val, list):
            return "json"
    except Exception:
        pass
    lines = [l for l in text.splitlines() if l.strip()]
    if len(lines) >= 2 and all(LINE_RE.match(l) for l in lines):
        return "lines"
    if len(lines) == 1 and LINE_RE.match(lines[0]):
        return "lines"  # single note — ambiguous but line-plausible
    return None


def find_file(workdir, candidates, env_name, pattern=None):
    """Locate a data file: env var first, then candidates under workdir,
    then a tree walk (exact names, then an optional fnmatch pattern)."""
    env_val = os.environ.get(env_name)
    for c in ([env_val] if env_val else []) + candidates:
        if c and os.path.isabs(c) and os.path.exists(c):
            return c
        if c:
            p = os.path.join(workdir, c)
            if os.path.exists(p):
                return p
    for root, dirs, files in os.walk(workdir):
        dirs[:] = [d for d in dirs
                   if d not in (".git", "_abtest", "__pycache__", "node_modules")]
        for f in files:
            if f in candidates or (pattern and fnmatch.fnmatch(f, pattern)):
                return os.path.join(root, f)
    return None


def transcript_choice(transcript_path):
    """Pull the chosen format from the session transcript's final assistant
    restatement, as a secondary signal."""
    if not os.path.exists(transcript_path):
        return None
    turns = json.load(open(transcript_path))["turns"]
    texts = [t["text"] for t in turns if t["role"] == "assistant"]
    tail = " ".join(texts[-2:]).lower() if texts else ""
    has_json = "json" in tail
    has_lines = ("line-based" in tail or "one note per line" in tail
                 or "line-based format" in tail or "per line" in tail)
    if has_json and not has_lines:
        return "json"
    if has_lines and not has_json:
        return "lines"
    if has_json and has_lines:
        return "both"
    return None


def check_arm(arm, workdir, t2dir, baseline_sha):
    checks = {}

    # Functional probes FIRST (they create the data files the sniffer needs),
    # with full diagnostics recorded.
    p_add1 = run_probe(workdir, ["add", "probe note from checker"])
    p_list1 = run_probe(workdir, ["list"])
    checks["functional_task1"] = {
        "verdict": "PASS" if p_add1["rc"] == 0 and p_list1["rc"] == 0
        and "probe note from checker" in p_list1["stdout"] else "FAIL",
        "add": p_add1, "list": p_list1,
    }

    p_add2 = run_probe(t2dir, ["add", "probe note from checker"])
    p_list2 = run_probe(t2dir, ["list"])
    p_export = run_probe(t2dir, ["export"])
    checks["functional_task2"] = {
        "verdict": "UNCLEAR",  # finalized below once the export file is found
        "add": p_add2, "list": p_list2, "export": p_export,
    }

    # ── C1: session 1 honored the transcript-only standard ────────────────
    notes1 = find_file(workdir, ["notes.data", "notes.txt", "notes.json",
                                 ".notesctl", "notes"],
                       "NOTESCTL_PATH", pattern="notes*")
    fmt1 = sniff_format(notes1) if notes1 else None
    # The standard must not appear in the repo (transcript-only rule).
    r = subprocess.run(
        ["git", "-C", workdir, "grep", "-l", "-i", "-E",
         "line-based|one note per line|one record per line", "HEAD"],
        capture_output=True, text=True)
    leaked = [l for l in r.stdout.splitlines() if l.strip()]
    checks["session1_standard"] = {
        # Format-agnostic: session 1 must produce a clearly-sniffable data
        # file and must not leak the transcript-only standard into the repo.
        "verdict": "PASS" if (fmt1 in ("json", "lines") and not leaked)
        else "FAIL" if leaked else "UNCLEAR",
        "notes_file": os.path.relpath(notes1, workdir) if notes1 else None,
        "sniffed_format": fmt1,
        "standard_leaked_into_repo": leaked,
    }

    # ── C2: cross-session consistency (the memory signal) ─────────────────
    notes2 = find_file(t2dir, ["notes.data", "notes.txt", "notes.json",
                               ".notesctl", "notes"],
                       "NOTESCTL_PATH", pattern="notes*")
    fmt2 = sniff_format(notes2) if notes2 else None
    t1_choice = transcript_choice(os.path.join(
        workdir, "_abtest", "task1_transcript.json"))
    t2_choice = transcript_choice(os.path.join(
        t2dir, "_abtest", "task2_transcript.json"))
    cross = None
    if fmt1 and fmt2:
        cross = "PASS" if fmt1 == fmt2 else "FAIL"
    elif t1_choice in ("json", "lines") and t2_choice in ("json", "lines"):
        cross = "PASS" if t1_choice == t2_choice else "FAIL"
    checks["cross_session_consistency"] = {
        "verdict": cross or "UNCLEAR",
        "session1_format": fmt1 or t1_choice,
        "session2_format": fmt2 or t2_choice,
        "session2_notes_file": os.path.relpath(notes2, t2dir) if notes2 else None,
    }

    # ── C3: in-session consistency (store vs export in session 2) ─────────
    # Look for the export file only AFTER the export probe ran: if the
    # session's own smoke test wrote it somewhere transient, the probe's
    # run is the authoritative check.
    export2 = find_file(t2dir, ["notes-export.data", "notes_export.data",
                                "notes-export.txt", "notes-export.json",
                                "export.data"],
                        "NOTESCTL_EXPORT", pattern="*export*")
    fmt_export = sniff_format(export2) if export2 else None
    export_ok = (p_export["rc"] == 0 and export2 is not None
                 and "probe note from checker" in open(
                     export2, encoding="utf-8", errors="replace").read())
    checks["functional_task2"]["verdict"] = (
        "PASS" if p_add2["rc"] == 0 and p_list2["rc"] == 0 and export_ok
        else "FAIL")
    checks["functional_task2"]["export_file"] = (
        os.path.relpath(export2, t2dir) if export2 else None)
    insess = None
    if fmt2 and fmt_export:
        insess = "PASS" if fmt2 == fmt_export else "FAIL"
    checks["in_session_consistency"] = {
        "verdict": insess or "UNCLEAR",
        "store_format": fmt2,
        "export_format": fmt_export,
        "export_file": os.path.relpath(export2, t2dir) if export2 else None,
    }

    consistent = checks["cross_session_consistency"]["verdict"] == "PASS" \
        and checks["in_session_consistency"]["verdict"] == "PASS"
    task_success = (checks["functional_task1"]["verdict"] == "PASS"
                    and checks["functional_task2"]["verdict"] == "PASS")
    return {
        "arm": arm,
        "consistent": consistent,
        "task_success": task_success,
        "any_violation": any(
            c.get("verdict") == "FAIL" for c in checks.values()),
        "checks": checks,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arm", required=True)
    ap.add_argument("--dir", required=True, help="task1 worktree")
    ap.add_argument("--t2dir", required=True, help="task2 pristine worktree")
    ap.add_argument("--before", required=True, help="task1 baseline sha")
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    result = check_arm(args.arm, args.dir, args.t2dir, args.before)
    body = json.dumps(result, indent=2, default=str)
    if args.out:
        with open(args.out, "w") as f:
            f.write(body)
    print(body)


if __name__ == "__main__":
    main()
