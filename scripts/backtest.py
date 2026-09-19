#!/usr/bin/env python3
"""Backtest ARMIN extraction against a real conversation.

Corpus: data/backtest/conversation.jsonl — the actual opencode-design
dialogue from this working session (14 prose turns + 6 tool calls).
Gold:   data/backtest/gold.json — hand annotations of the argumentative
        units a good extraction should find.

The script starts the real armin-engine binary with the project .env
(poe / GLM-5.3-flash), ingests the session, waits for the batched
background extraction to settle, then scores:

  - node-level precision / recall / F1 per node type (fuzzy match)
  - decision statuses as extracted
  - debt detection: are the real open questions found?
  - brief quality
  - spurious nodes (extracted but not in gold)

Usage: python3 scripts/backtest.py [--keep]
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ENGINE = os.path.join(ROOT, "armin-core", "target", "release", "armin-engine")
ENV_FILE = os.path.join(ROOT, ".env")
DATA = os.path.join(ROOT, "data", "backtest")

# Gold section -> extracted NodeType that can satisfy it.
TYPE_ALIASES = {
    "decisions": {"Decision"},
    "questions": {"Question"},
    "assumptions": {"Assumption"},
    "evidence": {"Evidence"},
    "contradictions": {"Claim"},
}

MATCH_THRESHOLD = 0.5


def load_env() -> dict:
    env = dict(os.environ)
    if os.path.exists(ENV_FILE):
        for line in open(ENV_FILE, encoding="utf-8"):
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                key, val = line.split("=", 1)
                env[key.strip()] = val.strip()
    return env


def req(port: int, method: str, path: str, body=None, timeout=30):
    url = f"http://127.0.0.1:{port}/api/v1{path}"
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(url, data=data, method=method,
                               headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(r, timeout=timeout) as resp:
        return json.loads(resp.read())


def start_engine(workdir: str, env: dict):
    args = [ENGINE, "--port", "0", "--db-path", os.path.join(workdir, "graph.db")]
    log_path = os.path.join(workdir, "engine.log")
    log_file = open(log_path, "w")
    proc = subprocess.Popen(args, stdout=log_file, stderr=subprocess.STDOUT,
                            env=env, start_new_session=True)
    # Poll the health endpoint (handshake goes to the log file).
    port = None
    deadline = time.time() + 15
    while time.time() < deadline:
        try:
            with open(log_path) as f:
                for line in f:
                    if line.startswith("ARMIN_PORT="):
                        port = int(line.split("=", 1)[1].strip())
                        break
        except FileNotFoundError:
            pass
        if port:
            break
        time.sleep(0.3)
    return proc, port, log_path


# ── Fuzzy matching ────────────────────────────────────────────────────────────

STOP = set("""the a an and or of to in for on with at by from as is are was were be been
being this that these those it its they them their we our you your i me my he she his her
will would should could can may might shall must do does did not no nor so than too very
just because but if while about what which who whom when where why how all any both each
few more most other some such only own same then once here there also into out up down
over under again further against between through during before after above below""".split())


def toks(s: str) -> set:
    return {w for w in re.findall(r"[a-z0-9]+", s.lower()) if w not in STOP}


def containment(a: str, b: str) -> float:
    ta, tb = toks(a), toks(b)
    if not ta or not tb:
        return 0.0
    return len(ta & tb) / min(len(ta), len(tb))


def score_pair(gold_label: str, ex_node: dict) -> float:
    """Containment of the gold label in the extracted label or label+description."""
    label = ex_node.get("label", "")
    desc = ex_node.get("description", "")
    return max(
        containment(gold_label, label),
        containment(gold_label, f"{label} {desc}"),
    )


def match_section(gold_items, extracted):
    """Greedy one-to-one matching: returns (matched, used_extracted_indices)."""
    matched, used = [], set()
    for g in gold_items:
        best, best_s = None, 0.0
        for i, ex in enumerate(extracted):
            if i in used:
                continue
            s = score_pair(g["label"], ex)
            if s > best_s:
                best, best_s = i, s
        if best is not None and best_s >= MATCH_THRESHOLD:
            used.add(best)
            matched.append((g, extracted[best], round(best_s, 2)))
    return matched, used


def prf(tp, fp, fn):
    p = tp / (tp + fp) if tp + fp else 0.0
    r = tp / (tp + fn) if tp + fn else 0.0
    f1 = 2 * p * r / (p + r) if p + r else 0.0
    return round(p, 3), round(r, 3), round(f1, 3)


def evaluate(gold: dict, snapshot: dict, debt: dict, decisions_api, brief: str) -> dict:
    nodes = snapshot.get("nodes", [])
    by_type: dict = {}
    for n in nodes:
        by_type.setdefault(n["node_type"], []).append(n)

    report = {"per_type": {}, "matched": [], "missed_gold": [], "spurious": []}

    total_tp = total_fp = total_fn = 0
    matched_gold_ids: set = set()

    for section, types in TYPE_ALIASES.items():
        gold_items = gold.get(section, [])
        extracted = [n for n in nodes if n["node_type"] in types]
        matched, used = match_section(gold_items, extracted)
        for g, ex, s in matched:
            matched_gold_ids.add(g["id"])
            report["matched"].append({
                "section": section, "score": s,
                "gold": g["label"], "extracted": ex["label"],
            })
        tp = len(matched)
        fp = len(extracted) - tp
        fn = len(gold_items) - tp
        p, r, f1 = prf(tp, fp, fn)
        report["per_type"][section] = {
            "gold": len(gold_items), "extracted_of_type": len(
                [n for n in nodes if n["node_type"] in types]),
            "tp": tp, "fp": fp, "fn": fn,
            "precision": p, "recall": r, "f1": f1,
        }
        total_tp, total_fp, total_fn = total_tp + tp, total_fp + fp, total_fn + fn

    report["missed_gold"] = [
        {"section": sec, "id": g["id"], "label": g["label"]}
        for sec, types in TYPE_ALIASES.items()
        for g in gold.get(sec, []) if g["id"] not in matched_gold_ids
    ]
    p, r, f1 = prf(total_tp, total_fp, total_fn)
    report["overall"] = {"tp": total_tp, "fp": total_fp, "fn": total_fn,
                         "precision": p, "recall": r, "f1": f1}

    # Decision statuses as the engine sees them.
    report["decision_status"] = decisions_api

    # Debt: real open questions found?
    report["debt"] = {
        "total_score": debt.get("total_score"),
        "open_question_items": [
            i["description"] for i in debt.get("items", [])
            if "question" in i["debt_type"].lower()
        ],
        "contradictions": [
            i["description"] for i in debt.get("items", [])
            if "contradiction" in i["debt_type"].lower()
        ],
    }

    report["brief"] = {
        "text": brief,
        "has_decisions": "Decisions" in brief,
        "has_open_questions": "Open questions" in brief,
    }
    return report


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true")
    args = ap.parse_args()

    if not os.path.exists(ENGINE):
        sys.exit("engine not built — run: cargo build --release -p armin-engine")

    env = load_env()
    print(f"provider={env.get('LLM_PROVIDER')} model={env.get('LLM_MODEL')}")

    workdir = tempfile.mkdtemp(prefix="armin-backtest-")
    events = [json.loads(l) for l in open(os.path.join(DATA, "conversation.jsonl"))
              if l.strip()]
    gold = json.load(open(os.path.join(DATA, "gold.json")))
    prose = [e for e in events if e["event_kind"] == "utterance"]
    expected_batches = -(-len(prose) // 10)

    proc, port, log_path = start_engine(workdir, env)
    if not port:
        print("FATAL: engine did not report port")
        sys.exit(1)
    print(f"engine up on :{port} — ingesting {len(events)} events "
          f"({len(prose)} prose, {len(events) - len(prose)} tool)")

    t0 = time.time()
    resp = req(port, "POST", "/ingest", events)
    print(f"ingest: {resp}")

    last, stable_ticks = None, 0
    while time.time() - t0 < 420:
        time.sleep(5)
        m = req(port, "GET", "/metrics")
        drained = (m["llm_events_extracted"] + m["llm_extraction_errors"]
                   >= m["events_queued_for_llm"])
        all_errored = (m["llm_extraction_errors"] >= expected_batches
                       and m["llm_batches"] == 0)
        sig = (m["llm_batches"], m["llm_events_extracted"],
               m["llm_extraction_errors"])
        if sig == last:
            stable_ticks += 1
        else:
            stable_ticks = 0
        last = sig
        if stable_ticks >= 2 and (drained or all_errored):
            break
    elapsed = time.time() - t0

    metrics = req(port, "GET", "/metrics")
    snapshot = req(port, "GET", "/snapshot")
    debt = req(port, "GET", "/debt")
    decisions_api = req(port, "GET", "/decisions")
    brief = req(port, "GET", "/state/brief")["brief"]

    proc.terminate()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()

    # Include the engine log tail so API errors are visible in the report.
    try:
        with open(log_path) as f:
            log_lines = f.readlines()
        report_log_tail = [l.strip()[:300] for l in log_lines[-25:]]
    except OSError:
        report_log_tail = []

    report = evaluate(gold, snapshot, debt, decisions_api, brief)
    report["runtime"] = {
        "wall_seconds": round(elapsed, 1),
        "events": len(events), "prose": len(prose),
        "expected_batches": expected_batches,
        "metrics": metrics,
        "node_count": len(snapshot.get("nodes", [])),
        "edge_count": len(snapshot.get("edges", [])),
        "engine_log_tail": report_log_tail,
    }

    out = os.path.join(DATA, f"results-{int(time.time())}.json")
    with open(out, "w") as f:
        json.dump(report, f, indent=2, default=str)

    print(json.dumps(report["per_type"], indent=2))
    print("OVERALL:", json.dumps(report["overall"]))
    print("MISSED GOLD:", json.dumps(report["missed_gold"], indent=2))
    print(f"\nresults -> {out}")
    if args.keep:
        print(f"workdir kept: {workdir}")
    else:
        shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    main()
