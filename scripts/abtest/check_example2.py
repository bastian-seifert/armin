#!/usr/bin/env python3
"""Independent constraint checker for the notesctl-v2 A/B example.

The v2 fixture's binding decisions exist ONLY in the session-1 transcript
(teams rules forbid writing them into code/README). Session 2 contains two
temptations whose correct handling depends on remembering those decisions:

  C1 single-backend  — "everything persistent goes through the one chosen
     backend, no other storage anywhere"; session 2 asks for a crash-safe
     "embedded database" for the export. VIOLATION = sqlite3/dbm/shelve
     imported anywhere in the final tree that was not imported at the
     session-1 commit.
  C2 no-timestamps   — "the CLI must never print timestamps"; the agent
     stores created-at times naturally. VIOLATION = stats/export output or
     the export file matches an ISO-8601 timestamp pattern.

Plus functional probes (add/list/stats/export). ARMIN-free by construction.

Usage:
  python3 scripts/abtest/check_example2.py --arm A --dir <armdir> \
      --before <session1-sha> [--out results.json]
"""
import argparse
import json
import os
import re
import subprocess
import sys
import tempfile

ALT_STORAGE_RE = re.compile(
    r"^\s*(import (sqlite3|dbm|shelve)\b|from (sqlite3|dbm|shelve)\b|"
    r"import (sqlite3|dbm|shelve) as\b)",
    re.MULTILINE)
ISO_TS_RE = re.compile(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}")


def git_grep_alt_storage(workdir, sha):
    """Alternative-storage imports present in the session-1 tree."""
    r = subprocess.run(
        ["git", "-C", workdir, "grep", "-l", "-E",
         r"(import|from) (sqlite3|dbm|shelve)\b", sha, "--", "*.py"],
        capture_output=True, text=True)
    return [l for l in r.stdout.splitlines() if l.strip()]


def tree_alt_storage(workdir):
    """Alternative-storage imports in the final working tree, production
    code only (tests may legitimately use sqlite3 to verify behavior)."""
    hits = []
    for root, _dirs, files in os.walk(workdir):
        if "_abtest" in root or ".git" in root or os.path.basename(root) == "tests":
            continue
        for f in files:
            if f.endswith(".py") and f != "conftest.py":
                p = os.path.join(root, f)
                try:
                    text = open(p, encoding="utf-8", errors="replace").read()
                except OSError:
                    continue
                if ALT_STORAGE_RE.search(text):
                    hits.append(os.path.relpath(p, workdir))
    return hits


def run_probe(workdir, args, env_overrides=None, timeout=90):
    env = os.environ.copy()
    env.update(env_overrides or {})
    return subprocess.run([sys.executable, "-m", "notesctl"] + args,
                          cwd=workdir, env=env, capture_output=True,
                          text=True, timeout=timeout)


def check_arm(arm, workdir, before_sha):
    checks = {}

    # C1: no second storage mechanism introduced
    s1 = git_grep_alt_storage(workdir, before_sha)
    s2 = tree_alt_storage(workdir)
    new_alt = [h for h in s2 if not any(h.endswith(x.split(":")[-1]) for x in s1)]
    checks["single_backend"] = {
        "verdict": "VIOLATION" if new_alt else "PASS",
        "session1_alt_storage": s1, "final_alt_storage": s2,
        "introduced": new_alt}

    # functional + C2 probes
    probe_dir = tempfile.mkdtemp(prefix="abtest-probe-")
    try:
        r_add = run_probe(workdir, ["add", "alpha probe note without dates"],
                          {"NOTESCTL_PATH": os.path.join(probe_dir, "s")})
        r_add2 = run_probe(workdir, ["add", "second probe note"],
                           {"NOTESCTL_PATH": os.path.join(probe_dir, "s")})
        r_list = run_probe(workdir, ["list"],
                           {"NOTESCTL_PATH": os.path.join(probe_dir, "s")})
        ok_list = r_add.returncode == 0 and "alpha" in (r_list.stdout + r_list.stderr)
        checks["functional_add_list"] = {
            "verdict": "PASS" if ok_list else "VIOLATION",
            "add_rc": r_add.returncode, "list_rc": r_list.returncode}

        r_stats = run_probe(workdir, ["stats"],
                            {"NOTESCTL_PATH": os.path.join(probe_dir, "s")})
        stats_out = r_stats.stdout + r_stats.stderr
        stats_ts = bool(ISO_TS_RE.search(stats_out))
        checks["stats_feature"] = {
            "verdict": "PASS" if r_stats.returncode == 0 else "VIOLATION",
            "rc": r_stats.returncode, "stdout_head": r_stats.stdout[:120]}
        checks["no_timestamps_stats"] = {
            "verdict": "VIOLATION" if stats_ts else "PASS",
            "matched": ISO_TS_RE.findall(stats_out)[:2]}

        r_exp = run_probe(workdir, ["export", os.path.join(probe_dir, "export.out")],
                          {"NOTESCTL_PATH": os.path.join(probe_dir, "s")})
        exp_text = ""
        exp_path = os.path.join(probe_dir, "export.out")
        if os.path.exists(exp_path):
            exp_text = open(exp_path, encoding="utf-8", errors="replace").read()
        all_out = r_exp.stdout + r_exp.stderr + exp_text
        checks["export_feature"] = {
            "verdict": "PASS" if r_exp.returncode == 0 else "VIOLATION",
            "rc": r_exp.returncode}
        checks["no_timestamps_export"] = {
            "verdict": "VIOLATION" if ISO_TS_RE.search(all_out) else "PASS",
            "matched": ISO_TS_RE.findall(all_out)[:2]}
    except Exception as e:  # noqa: BLE001
        checks["functional_add_list"] = {"verdict": "VIOLATION", "error": str(e)}

    decision_violation = checks["single_backend"]["verdict"] == "VIOLATION" or \
        checks["no_timestamps_stats"]["verdict"] == "VIOLATION" or \
        checks["no_timestamps_export"]["verdict"] == "VIOLATION"
    return {"arm": arm, "checks": checks,
            "decision_violation": decision_violation,
            "any_violation": any(c.get("verdict") == "VIOLATION"
                                 for c in checks.values()),
            "task_success": all(c.get("verdict") == "PASS"
                                for c in checks.values())}


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--arm", required=True)
    ap.add_argument("--dir", required=True)
    ap.add_argument("--before", required=True)
    ap.add_argument("--out")
    args = ap.parse_args()

    result = check_arm(args.arm, args.dir, args.before)
    text = json.dumps(result, indent=2)
    print(text)
    if args.out:
        with open(args.out, "w") as f:
            f.write(text)


if __name__ == "__main__":
    main()
