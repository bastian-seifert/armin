#!/usr/bin/env python3
"""Independent constraint checker for the notesctl A/B example.

Deliberately ARMIN-free (anti-circularity): decision consistency is measured
by diffing the machine-readable decision header between the session-1 commit
and the final worktree, plus scripted functional probes. Verdicts:
PASS / VIOLATION / UNCLEAR per check.

Usage:
  python3 scripts/abtest/check_example1.py --arm A --dir /tmp/.../armA \
      --before <session1-git-sha> [--out results.json]
"""
import argparse
import json
import os
import re
import subprocess
import sys
import tempfile

HEADER_BACKEND_RE = re.compile(r"Backend:\s*(json|sqlite)", re.IGNORECASE)
HEADER_CONFIG_RE = re.compile(r"Config:\s*(env|file)", re.IGNORECASE)


def git_show(workdir, sha, path):
    r = subprocess.run(["git", "-C", workdir, "show", f"{sha}:{path}"],
                       capture_output=True, text=True)
    return r.stdout if r.returncode == 0 else None


def parse_header(text):
    if text is None:
        return None, "file not found"
    backend = HEADER_BACKEND_RE.search(text)
    config = HEADER_CONFIG_RE.search(text)
    if not backend or not config:
        return None, "decision header missing or unparsable"
    return {"backend": backend.group(1).lower(),
            "config": config.group(1).lower()}, None


def run_probe(workdir, args, env_overrides=None, timeout=90):
    env = os.environ.copy()
    env.update(env_overrides or {})
    r = subprocess.run([sys.executable, "-m", "notesctl"] + args,
                       cwd=workdir, env=env, capture_output=True,
                       text=True, timeout=timeout)
    return r


def any_file_under(path):
    if not os.path.isdir(path):
        return False
    with os.scandir(path) as it:
        return next(iter(it), None) is not None


def check_arm(arm, workdir, before_sha):
    checks = {}

    # 1) decision header: session-1 vs final (decision-flip detection)
    s1, err1 = parse_header(git_show(workdir, before_sha, "notesctl/storage.py"))
    s2, err2 = parse_header(
        open(os.path.join(workdir, "notesctl/storage.py")).read()
        if os.path.exists(os.path.join(workdir, "notesctl/storage.py")) else None)
    flips = []
    if s1 and s2:
        flips = [f"{k}: {s1[k]} -> {s2[k]}" for k in s1 if s1[k] != s2[k]]
        checks["decision_header"] = {
            "verdict": "VIOLATION" if flips else "PASS",
            "session1": s1, "final": s2, "flips": flips}
    else:
        checks["decision_header"] = {
            "verdict": "UNCLEAR", "session1": s1 or err1, "final": s2 or err2}

    # 2) functional: add / list / stats
    try:
        r_add = run_probe(workdir, ["add", "alpha functional probe"])
        r_list = run_probe(workdir, ["list"])
        ok_add = r_add.returncode == 0
        ok_list = ok_add and "alpha" in (r_list.stdout + r_list.stderr)
        checks["functional_add_list"] = {
            "verdict": "PASS" if (ok_add and ok_list) else "VIOLATION",
            "add_rc": r_add.returncode, "list_rc": r_list.returncode,
            "list_has_note": ok_list}
    except Exception as e:  # noqa: BLE001
        checks["functional_add_list"] = {"verdict": "VIOLATION", "error": str(e)}

    # 3) stats feature (session-2 requirement)
    try:
        r_stats = run_probe(workdir, ["stats"])
        ok = r_stats.returncode == 0 and re.search(r"\d", r_stats.stdout)
        checks["stats_feature"] = {
            "verdict": "PASS" if ok else "VIOLATION",
            "rc": r_stats.returncode, "stdout_head": r_stats.stdout[:120]}
    except Exception as e:  # noqa: BLE001
        checks["stats_feature"] = {"verdict": "VIOLATION", "error": str(e)}

    # 4) mechanism consistency: storage lands where the *chosen* config
    #    mechanism says (env var vs config file), via isolated env/HOME
    chosen = (s2 or {}).get("config")
    if not chosen:
        checks["mechanism_consistency"] = {"verdict": "UNCLEAR",
                                           "reason": "no parsable config decision"}
    else:
        try:
            with tempfile.TemporaryDirectory() as tmp:
                if chosen == "env":
                    target = os.path.join(tmp, "probe_storage")
                    env = {"NOTESCTL_PATH": target}
                    run_probe(workdir, ["add", "mechanism probe"], env)
                    landed = any_file_under(target)
                    alt = None
                else:
                    fake_home = tmp
                    env = {"HOME": fake_home}
                    run_probe(workdir, ["add", "mechanism probe"], env)
                    landed = any(
                        os.path.exists(os.path.join(fake_home, f))
                        for f in ("notesctl.json", ".notesctl.json"))
                    alt = "files in fake HOME: " + ", ".join(os.listdir(fake_home))
                checks["mechanism_consistency"] = {
                    "verdict": "PASS" if landed else "VIOLATION",
                    "chosen_mechanism": chosen, "storage_landed": landed,
                    "evidence": alt}
        except Exception as e:  # noqa: BLE001
            checks["mechanism_consistency"] = {"verdict": "UNCLEAR", "error": str(e)}

    violation = any(c.get("verdict") == "VIOLATION"
                    for c in checks.values())
    return {"arm": arm, "checks": checks,
            "decision_violation": bool(flips),
            "any_violation": violation,
            "task_success": all(c.get("verdict") == "PASS"
                                for c in checks.values())}


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--arm", required=True)
    ap.add_argument("--dir", required=True)
    ap.add_argument("--before", required=True, help="session-1 git sha")
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
