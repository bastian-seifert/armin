#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["typesafe-sdk"]
# ///
"""Powered paired study over the example-3 consistency fixture.

Runs N independent pairs of the full example-3 sequence (stage → task1 →
brief → task2 → check), each pair in its own workdir, alternating which
data format the task-1 team standard forces (`json` / `lines`). The
alternation is the point: when the forced option matches the model's
natural choice, the pair is uninformative (both arms consistent); when it
is anti-prior, only memory keeps arm A consistent. Reality contains both
kinds of standard, so the study measures the memory effect as a mixture.

Analysis (ARMIN-free; reads each pair's pilot-summary.json):
  per-pair: consistent_A, consistent_B, d = A - B  ∈ {1, 0, -1}
  aggregate: consistency rates, mean paired delta, bootstrap 95% CI over
  pairs, sign test on discordant pairs (exact binomial, two-sided).

Usage:
  uv run scripts/abtest/run_study.py --pairs 6 --mode middleware \
      --workdir /tmp/opencode/abtest-study [--parallel-arms]

Resume-safe: pairs whose pilot-summary.json exists are skipped.
"""
import argparse
import json
import math
import os
import random
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
PY = sys.executable


def run_pair(pair_idx: int, args, assignment: str) -> dict:
    wd = os.path.join(args.workdir, f"pair{pair_idx:02d}-{assignment}")
    summary_path = os.path.join(wd, "pilot-summary.json")
    if os.path.exists(summary_path):
        print(f"== pair {pair_idx} ({assignment}) already done, skipping ==")
        return json.load(open(summary_path))
    print(f"\n{'=' * 64}\n== pair {pair_idx}: forced={assignment}, workdir={wd} ==")
    cmd = [
        "uv", "run", os.path.join(HERE, "run_pilot.py"),
        "--phase", "all", "--example", "3",
        "--mode", args.mode,
        "--assignment", assignment,
        "--native-threshold", str(args.native_threshold),
        "--workdir", wd,
    ]
    r = subprocess.run(cmd, capture_output=True, text=True)
    tail = "\n".join((r.stdout or "").splitlines()[-8:])
    print(tail)
    if r.returncode != 0 or not os.path.exists(summary_path):
        return {"error": (r.stderr or "")[-400:], "pair": pair_idx,
                "assignment": assignment}
    return json.load(open(summary_path))


def pair_verdicts(summary: dict) -> tuple | None:
    try:
        a = summary["A"]["consistent"]
        b = summary["B"]["consistent"]
        return (bool(a), bool(b))
    except Exception:
        return None


def bootstrap_ci(deltas: list[int], iters: int = 10_000, seed: int = 13):
    rng = random.Random(seed)
    n = len(deltas)
    if n == 0:
        return (0.0, 0.0)
    means = []
    for _ in range(iters):
        sample = [deltas[rng.randrange(n)] for _ in range(n)]
        means.append(sum(sample) / n)
    means.sort()
    return (means[int(0.025 * iters)], means[int(0.975 * iters) - 1])


def sign_test(discordant_plus: int, discordant_minus: int) -> float:
    """Exact two-sided binomial sign test on discordant pairs."""
    k, n = min(discordant_plus, discordant_minus), discordant_plus + discordant_minus
    if n == 0:
        return 1.0
    p = sum(math.comb(n, i) for i in range(0, k + 1)) / 2 ** n * 2
    return min(1.0, p)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--pairs", type=int, default=6,
                    help="number of paired runs (each = 4 opencode sessions)")
    ap.add_argument("--mode", default="middleware", choices=["proxy", "middleware"])
    ap.add_argument("--native-threshold", type=float, default=0.85)
    ap.add_argument("--workdir", default="/tmp/opencode/abtest-study")
    ap.add_argument("--bootstrap-iters", type=int, default=10_000)
    args = ap.parse_args()

    os.makedirs(args.workdir, exist_ok=True)

    pairs = []
    for i in range(1, args.pairs + 1):
        assignment = "json" if i % 2 == 1 else "lines"
        summary = run_pair(i, args, assignment)
        verdicts = pair_verdicts(summary)
        entry = {"pair": i, "assignment": assignment,
                 "consistent_A": verdicts[0] if verdicts else None,
                 "consistent_B": verdicts[1] if verdicts else None}
        if "error" in summary:
            entry["error"] = summary["error"]
        pairs.append(entry)

    valid = [p for p in pairs if p["consistent_A"] is not None]
    deltas = [int(p["consistent_A"]) - int(p["consistent_B"]) for p in valid]
    a_rate = sum(1 for p in valid if p["consistent_A"]) / len(valid) if valid else 0
    b_rate = sum(1 for p in valid if p["consistent_B"]) / len(valid) if valid else 0
    mean_d = sum(deltas) / len(deltas) if deltas else 0
    ci = bootstrap_ci(deltas, args.bootstrap_iters)
    d_plus = sum(1 for d in deltas if d > 0)
    d_minus = sum(1 for d in deltas if d < 0)
    p_value = sign_test(d_plus, d_minus)

    # Anti-prior subset (forced=json): the informative pairs per calibration.
    anti = [p for p in valid if p["assignment"] == "json"]
    anti_deltas = [int(p["consistent_A"]) - int(p["consistent_B"]) for p in anti]

    report = {
        "mode": args.mode,
        "pairs_requested": args.pairs,
        "pairs_valid": len(valid),
        "per_pair": pairs,
        "consistency_rate_A": round(a_rate, 3),
        "consistency_rate_B": round(b_rate, 3),
        "mean_paired_delta": round(mean_d, 3),
        "bootstrap_95_ci_mean_delta": [round(ci[0], 3), round(ci[1], 3)],
        "discordant": {"A_consistent_B_not": d_plus, "B_consistent_A_not": d_minus},
        "sign_test_p": round(p_value, 4),
        "anti_prior_subset": {
            "n": len(anti),
            "mean_delta": round(sum(anti_deltas) / len(anti_deltas), 3)
            if anti_deltas else None,
        },
    }
    out = os.path.join(args.workdir, "study-summary.json")
    with open(out, "w") as f:
        json.dump(report, f, indent=2)
    print("\n" + "=" * 64)
    print(f"pairs (valid/total): {len(valid)}/{len(pairs)}")
    for p in pairs:
        if p["consistent_A"] is None:
            print(f"  pair {p['pair']:2d} [{p['assignment']:5}] ERROR")
        else:
            mark = "+" if int(p["consistent_A"]) > int(p["consistent_B"]) else \
                "-" if int(p["consistent_A"]) < int(p["consistent_B"]) else "="
            print(f"  pair {p['pair']:2d} [{p['assignment']:5}] "
                  f"A={'consistent' if p['consistent_A'] else 'DRIFTED':<10} "
                  f"B={'consistent' if p['consistent_B'] else 'DRIFTED':<10} {mark}")
    print(f"consistency rate: arm A {a_rate:.2f} vs arm B {b_rate:.2f}")
    print(f"mean paired delta: {mean_d:+.2f} (95% CI {ci[0]:+.2f}..{ci[1]:+.2f})")
    print(f"discordant pairs: A>B {d_plus}, B>A {d_minus}; sign test p={p_value:.3f}")
    if anti:
        print(f"anti-prior subset (forced=json): n={len(anti)}, "
              f"mean delta {report['anti_prior_subset']['mean_delta']:+.2f}")
    print(f"study summary -> {out}")


if __name__ == "__main__":
    main()
