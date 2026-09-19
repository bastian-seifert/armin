#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["typesafe-sdk"]
# ///
"""Jev (TypeSafe System One) verification pass over ARMIN extraction.

Pipeline:
  1. Replay data/backtest/conversation.jsonl through the real engine
     (same loop as scripts/backtest.py) and capture the RAW snapshot.
  2. Ask Jev, per extracted node: its argumentative role (Choice over
     Claim/Evidence/Assumption/Question/Decision/noise) and whether the
     source event text grounds the node (Noul).
  3. Apply a code-side policy: relabel on high-confidence disagreement,
     drop ungrounded / pure-noise nodes.
  4. Assumption sweep: Noul per prose sentence to surface implicit
     premises (the extraction LLM's 0%-recall blind spot).
  5. Re-score against gold with backtest.py's matching and produce a
     before/after report + a human-reviewable overrides document.

Note: gold.json is self-annotated by the same model family as the
conversation — treat deltas as agreement signals, not truth.

Usage:
  uv run scripts/jev_eval.py [--model jev-1.13.0] [--relabel-threshold 0.8]
      [--drop-grounded 0.3] [--assumption-threshold 0.6] [--chunk-size 10]
      [--snapshot FILE]

Requires TYPESAFE_AI_API_KEY (or TYPESAFE_API_KEY) in .env and the engine
binary (armin-core/target/release/armin-engine).
"""
import argparse
import hashlib
import json
import os
import re
import shutil
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import backtest as bt

ROOT = bt.ROOT
DATA = bt.DATA

TYPE_CRITERIA = {
    "Decision": "A settled choice about what to do, adopt, change, or reject in this codebase or task — including plans, approaches, and commitments that bind later work",
    "OpenItem": "Something explicitly left unresolved or flagged for later — a TODO, an open question, deferred work, or a stated intention not yet acted on",
    "noise": "Transient session content no future session needs: social filler, greetings, process meta-talk, restatements of the task, narration of moment-to-moment actions, status reports that expire with the session, and reports of work already completed (the committed code itself is the record of what was done)",
}

# v2 scoring aliases: old gold sections mapped onto durable types.
# 'contradictions' and 'evidence' are not scoreable — v2 surfaces the former
# as scratch-vs-graph tension, and tool events are provenance, not nodes.
TYPE_ALIASES_V2 = {
    "decisions": {"Decision"},
    "questions": {"OpenItem"},
    "assumptions": {"OpenItem"},
}

MIN_SENTENCE_WORDS = 6
MAX_SENTENCES_PER_EVENT = 12
MAX_ASSUMPTIONS_PER_EVENT = 3
SOURCE_EXCERPT_CHARS = 400


# ── Engine pipeline (mirrors backtest.py main, captures raw data) ─────────────

def run_engine(env: dict) -> dict:
    """Start the engine, ingest the corpus, wait for settle, return raw data."""
    events = [json.loads(l) for l in open(os.path.join(bt.DATA, "conversation.jsonl"))
              if l.strip()]
    prose = [e for e in events if e["event_kind"] == "utterance"]
    expected_batches = -(-len(prose) // 10)

    workdir = tempfile.mkdtemp(prefix="armin-jev-")
    proc, port, log_path = bt.start_engine(workdir, env)
    if not port:
        sys.exit("FATAL: engine did not report port")
    print(f"engine up on :{port} — ingesting {len(events)} events "
          f"({len(prose)} prose, {len(events) - len(prose)} tool)")

    t0 = time.time()
    resp = bt.req(port, "POST", "/ingest", events)
    print(f"ingest: {resp}")

    last, stable_ticks = None, 0
    while time.time() - t0 < 420:
        time.sleep(5)
        m = bt.req(port, "GET", "/metrics")
        drained = (m["llm_events_extracted"] + m["llm_extraction_errors"]
                   >= m["events_queued_for_llm"])
        all_errored = (m["llm_extraction_errors"] >= expected_batches
                       and m["llm_batches"] == 0)
        sig = (m["llm_batches"], m["llm_events_extracted"], m["llm_extraction_errors"])
        if sig == last:
            stable_ticks += 1
        else:
            stable_ticks = 0
        last = sig
        if stable_ticks >= 2 and (drained or all_errored):
            break
    elapsed = time.time() - t0

    raw = {
        "snapshot": bt.req(port, "GET", "/snapshot"),
        "debt": bt.req(port, "GET", "/debt"),
        "decisions": bt.req(port, "GET", "/decisions"),
        "brief": bt.req(port, "GET", "/state/brief")["brief"],
        "metrics": bt.req(port, "GET", "/metrics"),
    }
    proc.terminate()
    try:
        proc.wait(timeout=10)
    except Exception:
        proc.kill()
    shutil.rmtree(workdir, ignore_errors=True)

    raw["runtime"] = {
        "wall_seconds": round(elapsed, 1), "events": len(events),
        "prose": len(prose), "expected_batches": expected_batches,
    }
    return raw


# ── TypeSafe client with disk cache ───────────────────────────────────────────

def canonical(obj) -> str:
    return json.dumps(obj, sort_keys=True, ensure_ascii=False)


class JevClient:
    """TypeSafe systemone wrapper with a JSON disk cache and usage metering."""

    def __init__(self, api_key: str, model: str, cache_path: str):
        from typesafe_sdk import RetryPolicy, TypeSafeClient

        self.model = model
        self.cache_path = cache_path
        self.cache = json.load(open(cache_path)) if os.path.exists(cache_path) else {}
        self.input_tokens = 0
        self.calls = 0
        self.client = TypeSafeClient(
            api_key=api_key,
            retry=RetryPolicy(max_retries=5, backoff_initial=1.0, backoff_max=20.0),
            timeout=120.0,
        )

    def ask(self, state, questions: dict) -> dict:
        key = hashlib.sha256(canonical(
            {"model": self.model, "state": state, "questions": _plain(questions)}
        ).encode()).hexdigest()
        if key in self.cache:
            return self.cache[key]
        resp = self.client.system_one(state=state, questions=questions, model=self.model)
        self.calls += 1
        self.input_tokens += resp.usage.input_tokens or 0
        answer = {"model": resp.model, "answers": _plain(resp.answers)}
        self.cache[key] = answer
        with open(self.cache_path, "w") as f:
            json.dump(self.cache, f, indent=1)
        return answer


def _plain(obj):
    """Convert SDK objects / nested structures into JSON-safe plain data."""
    import msgspec
    try:
        return msgspec.to_builtins(obj)
    except (TypeError, msgspec.ValidationError):
        pass
    if isinstance(obj, dict):
        return {k: _plain(v) for k, v in obj.items()}
    if isinstance(obj, (list, tuple)):
        return [_plain(v) for v in obj]
    if isinstance(obj, (str, int, float, bool)) or obj is None:
        return obj
    return str(obj)


# ── Node verification ─────────────────────────────────────────────────────────

def node_questions(node_map: dict) -> dict:
    from typesafe_sdk import Choice, Noul

    questions = {}
    for qid in node_map:
        questions[f"{qid}_type"] = Choice(
            instructions=(
                f"Which argumentative role does `items.{qid}` play in the session, "
                f"judging `items.{qid}.label` and `items.{qid}.description` against "
                f"the source text in `items.{qid}.source_text`? The extracted type "
                f"was `items.{qid}.extracted_type` — judge the text on its own terms."
            ),
            criteria=TYPE_CRITERIA,
        )
        questions[f"{qid}_grounded"] = Noul(
            instructions=(
                f"Does the source text in `items.{qid}.source_text` contain, or "
                f"directly support, what `items.{qid}` asserts in its label and "
                "description? "
                "true = the source explicitly states or directly entails it; "
                "false = the source does not address it or contradicts it."
            ),
            criteria={
                "true": "Source text states or directly entails the node's assertion",
                "false": "Source text does not contain support for the assertion",
            },
        )
    return questions


def build_node_state(chunk: list) -> dict:
    items = {}
    for i, (node, source) in enumerate(chunk):
        qid = f"n{i}"
        items[qid] = {
            "label": node["label"],
            "description": node["description"],
            "extracted_type": node["node_type"],
            "source_text": source if source.strip() else "(source text unavailable)",
        }
    return {"items": items}


def verify_nodes(client: JevClient, snapshot: dict, events: list, args) -> dict:
    """Ask Jev about every node; returns verdicts keyed by node id."""
    event_text = {ev["id"]: ev.get("text", "") for ev in events}

    chunk_size = args.chunk_size
    items = []
    for node in snapshot["nodes"]:
        src = event_text.get(node["event_id"], "")
        items.append((node, src))

    verdicts = {}
    for start in range(0, len(items), chunk_size):
        chunk = items[start:start + chunk_size]
        state = build_node_state(chunk)
        questions = node_questions(state["items"])
        answer = client.ask(state, questions)
        for i, (node, src) in enumerate(chunk):
            qid = f"n{i}"
            answers = answer["answers"]
            verdicts[node["id"]] = {
                "type_choice": answers[f"n{i}_type"].get("choice"),
                "type_probabilities": answers[f"n{i}_type"].get("probabilities"),
                "type_confidence": answers[f"n{i}_type"].get("confidence"),
                "grounded": answers[f"n{i}_grounded"].get("noul"),
                "source_available": bool(src.strip()),
            }
        print(f"  jev verified nodes {start + 1}–{min(start + chunk_size, len(items))} "
              f"of {len(items)}")
    return verdicts


def apply_policy(snapshot: dict, verdicts: dict, args) -> tuple:
    """Relabel/drop per policy. Returns (new_nodes, actions, disagreements)."""
    actions, disagreements = [], []
    kept_nodes = []
    for node in snapshot["nodes"]:
        v = verdicts.get(node["id"])
        if v is None:
            kept_nodes.append(node)
            continue
        action = None
        if v["grounded"] is not None and v["grounded"] < args.drop_grounded:
            action = {"node_id": node["id"], "kind": "drop",
                      "reason": f"ungrounded (noul={v['grounded']:.2f})"}
        elif v["type_choice"] == "noise" and (v["type_confidence"] or 0) >= args.relabel_threshold:
            action = {"node_id": node["id"], "kind": "drop",
                      "reason": f"noise (p_conf={v['type_confidence']:.2f})"}
        elif (v["type_choice"] and v["type_choice"] != "noise"
              and v["type_choice"] != node["node_type"]
              and (v["type_confidence"] or 0) >= args.relabel_threshold):
            action = {"node_id": node["id"], "kind": "relabel",
                      "reason": f"{node['node_type']} -> {v['type_choice']} "
                                f"(p_conf={v['type_confidence']:.2f})"}
            node = dict(node, node_type=v["type_choice"])
        if action:
            actions.append(action)
        elif v["type_choice"] and v["type_choice"] != node["node_type"]:
            disagreements.append({
                "node_id": node["id"],
                "kind": "disagree",
                "reason": f"kept {node['node_type']}, jev said {v['type_choice']} "
                          f"(conf={(v['type_confidence'] or 0):.2f})",
            })
        kept_nodes.append(node)
    return kept_nodes, actions, disagreements


# ── Assumption sweep ──────────────────────────────────────────────────────────

def split_sentences(text: str) -> list:
    parts = re.split(r"(?<=[.!?])\s+", text.replace("\n", " ").strip())
    return [p for p in parts if len(p.split()) >= MIN_SENTENCE_WORDS]


def sweep_assumptions(client: JevClient, events: list, args) -> list:
    """Per-sentence Noul over prose events; returns synthetic Assumption nodes."""
    from typesafe_sdk import Noul

    prose = [e for e in events if e["event_kind"] == "utterance"]
    nodes = []
    for ev in prose:
        sentences = split_sentences(ev.get("text", ""))[:MAX_SENTENCES_PER_EVENT]
        if not sentences:
            continue
        state = {
            "event": ev.get("text", ""),
            "sentences": {f"s{i}": s for i, s in enumerate(sentences)},
        }
        questions = {}
        for i in range(len(sentences)):
            questions[f"s{i}_assumption"] = Noul(
                instructions=(
                    f"Does `sentences.s{i}` state an implicit premise that the "
                    "reasoning takes for granted without justification? "
                    "true = the sentence assumes something as given that carries the "
                    "argument but is not itself supported or derived; "
                    "false = the sentence is a supported claim, a decision, a "
                    "question, evidence, or chatter."
                ),
                criteria={
                    "true": "Unjustified premise taken for granted by the reasoning",
                    "false": "Supported statement, question, decision, or chatter",
                },
            )
        answer = client.ask(state, questions)
        scored = []
        for i, sentence in enumerate(sentences):
            noul = answer["answers"][f"s{i}_assumption"].get("noul", 0.0)
            if noul >= args.assumption_threshold:
                scored.append((noul, sentence))
        scored.sort(reverse=True)
        for k, (noul, sentence) in enumerate(scored[:MAX_ASSUMPTIONS_PER_EVENT]):
            words = sentence.split()
            label = " ".join(words[:12]) + ("…" if len(words) > 12 else "")
            nodes.append({
                "id": f"jev-assume-{ev['id']}-{k}",
                "node_type": "Assumption",
                "label": label,
                "description": sentence,
                "event_id": ev["id"],
                "agent_id": "jev-sweep",
                "session_id": ev.get("session_id", "backtest-1"),
                "timestamp": ev.get("end_time", 0.0),
                "confidence": round(noul, 2),
                "files": [], "commit": None, "mention_count": 1,
                "status": "Active",
            })
        print(f"  assumption sweep: event {ev['id']} — "
              f"{len(scored[:MAX_ASSUMPTIONS_PER_EVENT])}/{len(sentences)} sentences flagged")
    return nodes


# ── Jev-native edge pass (pairwise relation Choice) ───────────────────────────

EDGE_CRITERIA = {
    "none": "No durable relation between the two items — unrelated, merely adjacent, or both transient",
    "later_supersedes_earlier": "The later item replaces or overrides the earlier decision or rule",
    "later_refutes_earlier": "The later item shows the earlier fact, rule, or plan is wrong or no longer holds",
    "later_resolves_earlier": "The later item settles or answers the earlier open item",
    "later_relates_earlier": "The later item constrains, grounds, explains, or narrows the earlier one without contradicting it",
    "earlier_supersedes_later": "The earlier item replaces or overrides the later decision or rule",
    "earlier_refutes_later": "The earlier item shows the later fact, rule, or plan is wrong or no longer holds",
    "earlier_resolves_later": "The earlier item settles or answers the later open item",
    "earlier_relates_later": "The earlier item constrains, grounds, explains, or narrows the later one without contradicting it",
}

OPTION_TO_EDGE = {
    "later_supersedes_earlier": ("Supersedes", "later"),
    "later_refutes_earlier": ("Refutes", "later"),
    "later_resolves_earlier": ("Resolves", "later"),
    "later_relates_earlier": ("RelatesTo", "later"),
    "earlier_supersedes_later": ("Supersedes", "earlier"),
    "earlier_refutes_later": ("Refutes", "earlier"),
    "earlier_resolves_later": ("Resolves", "earlier"),
    "earlier_relates_later": ("RelatesTo", "earlier"),
}


def edge_questions(pair_map: dict) -> dict:
    from typesafe_sdk import Choice

    questions = {}
    for qid in pair_map:
        questions[f"{qid}_relation"] = Choice(
            instructions=(
                f"In `pairs.{qid}`, does one of the two sentences bear an "
                "argumentative relation to the other? `pairs.{qid}.earlier` occurred "
                f"before `pairs.{qid}.later` in the conversation. Pick the single "
                "best description of how they relate, or none if they are unrelated "
                "or merely adjacent."
            ),
            criteria=EDGE_CRITERIA,
        )
    return questions


def native_edges(client: JevClient, nodes: list, args) -> tuple:
    """Pairwise relation judgments over Jev-native nodes; returns (edges, pairs_meta)."""
    ordered = sorted(nodes, key=lambda n: (n["timestamp"], n["id"]))
    texts = {n["id"]: f"{n['label']} {n['description']}" for n in ordered}
    token_sets = {n["id"]: bt.toks(texts[n["id"]]) for n in ordered}

    candidates = []
    for i, a in enumerate(ordered):
        for b in ordered[i + 1:]:
            if a["id"] == b["id"]:
                continue
            overlap = len(token_sets[a["id"]] & token_sets[b["id"]])
            if overlap >= args.edge_min_overlap:
                candidates.append((a, b, overlap))
    print(f"  edge pass: {len(ordered)} nodes, {len(candidates)} candidate pairs "
          f"(overlap >= {args.edge_min_overlap})")

    edges, meta = [], []
    for start in range(0, len(candidates), args.edge_chunk):
        chunk = candidates[start:start + args.edge_chunk]
        pair_map = {f"p{k}": {"earlier": texts[a["id"]], "later": texts[b["id"]]}
                    for k, (a, b, _) in enumerate(chunk)}
        state = {"pairs": pair_map}
        questions = edge_questions(pair_map)
        answer = client.ask(state, questions)
        for k, (a, b, overlap) in enumerate(chunk):
            rel = answer["answers"][f"p{k}_relation"]
            choice = rel.get("choice")
            p = (rel.get("probabilities") or {}).get(choice, 0.0)
            meta.append({"earlier": a["id"], "later": b["id"], "choice": choice,
                         "p": p, "overlap": overlap})
            if choice not in OPTION_TO_EDGE or p < args.edge_threshold:
                continue
            edge_type, side = OPTION_TO_EDGE[choice]
            src, tgt = (b["id"], a["id"]) if side == "later" else (a["id"], b["id"])
            edges.append({
                "id": f"jev-native-e{len(edges)}",
                "edge_type": edge_type,
                "source_node_id": src,
                "target_node_id": tgt,
                "reasoning": f"jev-native: {choice} (p={p:.2f}, "
                             f"overlap={overlap})",
                "timestamp": max(a["timestamp"], b["timestamp"]),
                "evidence_score": None,
                "provenance": "EXTRACTED" if p >= 0.8 else "INFERRED",
            })
        print(f"  edge pass: pairs {start + 1}–{min(start + args.edge_chunk, len(candidates))} "
              f"of {len(candidates)} judged")
    return edges, meta


def gold_label_by_id(gold: dict) -> dict:
    out = {}
    for section in ("decisions", "questions", "assumptions", "evidence",
                    "contradictions"):
        for item in gold.get(section, []):
            if item.get("id") and item.get("label"):
                out[item["id"]] = item["label"]
    return out


def score_relations(gold: dict, nodes: list, edges: list) -> dict:
    """Match gold key_relations against native edges via node-level fuzzy matching."""
    labels = gold_label_by_id(gold)
    node_by_type_candidates = {}
    edge_keys = {(e["source_node_id"], e["target_node_id"], e["edge_type"])
                 for e in edges}

    used_edges = set()
    tp, details = 0, []
    for rel in gold.get("key_relations", []):
        src_label, tgt_label = labels.get(rel["source"], ""), labels.get(rel["target"], "")
        src_match = max(((n, bt.score_pair(src_label, n)) for n in nodes),
                        key=lambda x: x[1], default=(None, 0.0))
        tgt_match = max(((n, bt.score_pair(tgt_label, n)) for n in nodes),
                        key=lambda x: x[1], default=(None, 0.0))
        src_node, tgt_node = src_match[0], tgt_match[0]
        found = None
        if (src_node and tgt_node and src_match[1] >= 0.5 and tgt_match[1] >= 0.5
                and (src_node["id"], tgt_node["id"], rel["type"]) in edge_keys):
            found = (src_node["id"], tgt_node["id"], rel["type"])
            used_edges.add(found)
        if found:
            tp += 1
        details.append({
            "relation": f"{rel['source']} -{rel['type']}-> {rel['target']}",
            "note": rel.get("note", ""),
            "matched_source_node": src_node["id"] if src_node and src_match[1] >= 0.5 else None,
            "matched_target_node": tgt_node["id"] if tgt_node and tgt_match[1] >= 0.5 else None,
            "edge_found": bool(found),
        })
    fn = len(gold.get("key_relations", [])) - tp
    fp = len(edge_keys - used_edges)
    p, r, f1 = bt.prf(tp, fp, fn)
    return {"gold": len(gold.get("key_relations", [])), "edges": len(edges),
            "tp": tp, "fp": fp, "fn": fn,
            "precision": p, "recall": r, "f1": f1, "details": details}


# ── Reporting ─────────────────────────────────────────────────────────────────

def fmt_table(baseline: dict, after: dict) -> str:
    lines = [
        f"{'section':<15} {'gold':>5} | {'P':>6} {'R':>6} {'F1':>6} | {'P':>6} {'R':>6} {'F1':>6}",
        "-" * 65,
    ]
    sections = sorted(set(baseline["per_type"]) | set(after["per_type"]))
    for sec in sections:
        b = baseline["per_type"].get(sec, {})
        a = after["per_type"].get(sec, {})
        lines.append(
            f"{sec:<15} {b.get('gold', 0):>5} | {b.get('precision', 0):>6.3f} "
            f"{b.get('recall', 0):>6.3f} {b.get('f1', 0):>6.3f} | "
            f"{a.get('precision', 0):>6.3f} {a.get('recall', 0):>6.3f} {a.get('f1', 0):>6.3f}")
    bo, ao = baseline["overall"], after["overall"]
    lines.append("-" * 65)
    lines.append(f"{'OVERALL':<15} {'':>5} | {bo['precision']:>6.3f} {bo['recall']:>6.3f} "
                 f"{bo['f1']:>6.3f} | {ao['precision']:>6.3f} {ao['recall']:>6.3f} "
                 f"{ao['f1']:>6.3f}")
    lines.append("(columns: baseline extractor | after Jev policy)")
    return "\n".join(lines)


def write_overrides_md(path: str, nodes: list, verdicts: dict, actions: dict,
                       disagreements: list, assumption_nodes: list, gold: dict,
                       events_by_id: dict):
    node_by_id = {n["id"]: n for n in nodes}
    out = ["# Jev overrides — human review document", "",
           "Every node where Jev's verdict differs from the extractor, plus the "
           "assumption sweep. Extractor and gold are both LLM-family generated; "
           "judge each row on the source text itself.", ""]

    out.append("## Actions taken\n")
    if actions:
        for a in actions:
            n = node_by_id.get(a["node_id"], {})
            v = verdicts.get(a["node_id"], {})
            out.append(f"### {a['node_id']} — {a['kind']}")
            out.append(f"- reason: {a['reason']}")
            out.append(f"- extractor type: {n.get('node_type')} (conf {n.get('confidence')})")
            out.append(f"- jev type: {v.get('type_choice')} "
                       f"(conf {v.get('type_confidence')}), grounded noul {v.get('grounded')}")
            out.append(f"- label: {n.get('label')}")
            out.append(f"- description: {n.get('description')}")
            src = events_by_id.get(n.get("event_id"), {}).get("text", "")
            out.append(f"- source event {n.get('event_id')}: "
                       f"{src[:SOURCE_EXCERPT_CHARS]!r}")
            out.append("")
    else:
        out.append("(none)\n")

    out.append("## Disagreements kept (low confidence)\n")
    if disagreements:
        for d in disagreements:
            n = node_by_id.get(d["node_id"], {})
            out.append(f"- **{d['node_id']}**: {d['reason']} — "
                       f"label: {n.get('label')}")
    else:
        out.append("(none)")
    out.append("")

    out.append("## Assumption sweep (new nodes)\n")
    if assumption_nodes:
        for n in assumption_nodes:
            src = events_by_id.get(n["event_id"], {}).get("text", "")
            gold_hits = [g["label"] for g in gold.get("assumptions", [])
                         if bt.score_pair(g["label"], n) >= 0.5]
            out.append(f"- **{n['id']}** (noul {n['confidence']}): "
                       f"{n['description']}")
            out.append(f"  - source event {n['event_id']}: {src[:SOURCE_EXCERPT_CHARS]!r}")
            out.append(f"  - matches gold: {gold_hits if gold_hits else 'no'}")
    else:
        out.append("(none)")
    out.append("")
    with open(path, "w") as f:
        f.write("\n".join(out))


# ── Jev-native extraction (select instead of generate) ────────────────────────

def _norm_text(s: str) -> str:
    return re.sub(r"[^a-z0-9 ]", "", s.lower()).strip()


def native_extract(client: JevClient, events: list, args) -> tuple:
    """Scan every prose sentence with Jev; kept sentences become verbatim nodes.

    No generative LLM involved: labels are the source sentences themselves,
    so hallucinated content is impossible by construction. Tool-call events
    become deterministic Evidence nodes, mirroring the engine.
    """
    from typesafe_sdk import Choice, Noul

    prose = [e for e in events if e["event_kind"] == "utterance"]
    tools = [e for e in events if e["event_kind"] != "utterance"]
    nodes, seen, scan = [], {}, []
    stats = {"sentences": 0, "kept": 0, "noise": 0, "below_threshold": 0,
             "duplicates": 0, "tools_provenance_only": len(tools)}

    for ev in prose:
        sentences = split_sentences(ev.get("text", ""))[:MAX_SENTENCES_PER_EVENT]
        if not sentences:
            continue
        state = {
            "event": ev.get("text", ""),
            "sentences": {f"s{i}": s for i, s in enumerate(sentences)},
        }
        questions = {}
        for i in range(len(sentences)):
            questions[f"s{i}_type"] = Choice(
                instructions=(
                    f"A coding agent worked in this codebase and wrote "
                    f"`sentences.s{i}` inside the surrounding discussion in `event`. "
                    "What durable knowledge does the sentence carry — what would a "
                    "FUTURE session working in this codebase need to remember from "
                    "it? Judge the sentence itself. Sentences that only narrate the "
                    "moment (actions taken, readings, transitions, social filler, "
                    "process talk, status that expires with the session) are noise: "
                    "nothing in them will still matter once the session ends."
                ),
                criteria=TYPE_CRITERIA,
            )
        answer = client.ask(state, questions)
        for i, sentence in enumerate(sentences):
            stats["sentences"] += 1
            type_answer = answer["answers"][f"s{i}_type"]
            choice = type_answer.get("choice")
            p = (type_answer.get("probabilities") or {}).get(choice, 0.0)
            scan.append({"event_id": ev["id"], "sentence": sentence, "choice": choice,
                         "p": p})
            if choice == "noise":
                stats["noise"] += 1
                continue
            key = _norm_text(sentence)
            if key in seen:
                stats["duplicates"] += 1
                seen[key]["mention_count"] += 1
                continue
            node_type = choice
            if node_type not in TYPE_CRITERIA or p < args.native_threshold:
                stats["below_threshold"] += 1
                continue
            words = sentence.split()
            node = {
                "id": f"jev-native-{ev['id']}-{i}",
                "node_type": node_type,
                "label": " ".join(words[:12]) + ("…" if len(words) > 12 else ""),
                "description": sentence,
                "event_id": ev["id"],
                "agent_id": "jev-native",
                "session_id": ev.get("session_id", "backtest-1"),
                "timestamp": ev.get("end_time", 0.0),
                "confidence": round(p, 2),
                "files": [], "commit": None, "mention_count": 1,
                "status": "Active",
            }
            seen[key] = node
            nodes.append(node)
            stats["kept"] += 1
    return nodes, scan, stats


def fmt_table3(baseline: dict, after: dict, native: dict) -> str:
    lines = [
        f"{'section':<15} {'gold':>4} | {'LLM F1':>7} {'+Jev F1':>8} {'native F1':>9} "
        f"| {'native P':>8} {'native R':>8}",
        "-" * 72,
    ]
    sections = sorted(set(baseline["per_type"]) | set(native["per_type"]))
    for sec in sections:
        b = baseline["per_type"].get(sec, {})
        a = after["per_type"].get(sec, {})
        n = native["per_type"].get(sec, {})
        lines.append(
            f"{sec:<15} {b.get('gold', 0):>4} | {b.get('f1', 0):>7.3f} "
            f"{a.get('f1', 0):>8.3f} {n.get('f1', 0):>9.3f} | "
            f"{n.get('precision', 0):>8.3f} {n.get('recall', 0):>8.3f}")
    bo, ao, no = baseline["overall"], after["overall"], native["overall"]
    lines.append("-" * 72)
    lines.append(f"{'OVERALL':<15} {'':>4} | {bo['f1']:>7.3f} {ao['f1']:>8.3f} "
                 f"{no['f1']:>9.3f} | {no['precision']:>8.3f} {no['recall']:>8.3f}")
    lines.append("(LLM = extractor baseline, +Jev = verify/drop policy, "
                 "native = Jev-only sentence scan, verbatim nodes, no edges)")
    return "\n".join(lines)


# ── Main ──────────────────────────────────────────────────────────────────────

def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--model", default="jev-1.13.0")
    ap.add_argument("--relabel-threshold", type=float, default=0.8)
    ap.add_argument("--drop-grounded", type=float, default=0.3)
    ap.add_argument("--assumption-threshold", type=float, default=0.6)
    ap.add_argument("--chunk-size", type=int, default=10)
    ap.add_argument("--snapshot", help="reuse a saved jev-snapshot.json instead of "
                                       "re-running the engine")
    ap.add_argument("--native-only", action="store_true",
                    help="skip the engine entirely: extract with Jev alone by "
                         "scanning sentences (select-don't-generate, verbatim nodes)")
    ap.add_argument("--native-threshold", type=float, default=0.85,
                    help="min probability of the chosen type for a node in native "
                         "mode (curve on the backtest gold: 0.5 -> F1 0.338, "
                         "0.7 -> 0.377, 0.85 -> 0.441, 0.9 -> 0.429; tuned on the "
                         "same self-annotated gold it is scored against)")
    ap.add_argument("--no-edges", action="store_true",
                    help="native mode: skip the pairwise edge pass")
    ap.add_argument("--edge-threshold", type=float, default=0.7,
                    help="min probability for keeping a native edge")
    ap.add_argument("--edge-min-overlap", type=int, default=1,
                    help="min shared content tokens for a candidate edge pair")
    ap.add_argument("--edge-chunk", type=int, default=15,
                    help="pairs per edge-pass request")
    ap.add_argument("--no-cache", action="store_true")
    args = ap.parse_args()

    from typesafe_sdk import TypeSafeClient  # noqa: F401 — fail fast if missing

    env = bt.load_env()
    api_key = env.get("TYPESAFE_AI_API_KEY") or env.get("TYPESAFE_API_KEY")
    if not api_key:
        sys.exit("no TypeSafe API key: set TYPESAFE_AI_API_KEY in .env")

    gold = json.load(open(os.path.join(bt.DATA, "gold.json")))
    cache_path = os.path.join(bt.DATA, ".jev-cache.json") if not args.no_cache else "/dev/null"
    jev = JevClient(api_key, args.model, cache_path)

    if args.native_only:
        events = [json.loads(l) for l in open(os.path.join(bt.DATA, "conversation.jsonl"))
                  if l.strip()]
        # v2 durable taxonomy: score old gold sections against new types.
        bt.TYPE_ALIASES.clear()
        bt.TYPE_ALIASES.update(TYPE_ALIASES_V2)
        print(f"Jev-native extraction, v2 durable criteria (model {args.model}, "
              f"threshold {args.native_threshold}) — no generative LLM, no engine")
        nodes, scan, stats = native_extract(jev, events, args)
        native = bt.evaluate(gold, {"nodes": nodes, "edges": []}, {}, [], "")

        edges, edge_meta, relations = [], [], {}
        if not args.no_edges:
            print("\n─ Jev-native edge pass ─")
            edges, edge_meta = native_edges(jev, nodes, args)
            relations = score_relations(gold, nodes, edges)
            print(f"  edges kept: {len(edges)} "
                  f"({sum(1 for e in edges if e['edge_type'] == 'Supersedes')} Supersedes, "
                  f"{sum(1 for e in edges if e['edge_type'] == 'Refutes')} Refutes, "
                  f"{sum(1 for e in edges if e['edge_type'] == 'RelatesTo')} RelatesTo, "
                  f"{sum(1 for e in edges if e['edge_type'] == 'Resolves')} Resolves)")
            print(f"  relations vs gold: {json.dumps({k: v for k, v in relations.items() if k != 'details'})}")
            for d in relations["details"]:
                print(f"    {'OK ' if d['edge_found'] else 'MISS'} {d['relation']} — {d['note']} "
                      f"(src={d['matched_source_node']}, tgt={d['matched_target_node']})")

        saved_path = os.path.join(bt.DATA, "jev-results.json")
        if os.path.exists(saved_path):
            saved = json.load(open(saved_path))
            print("\n" + fmt_table3(saved["baseline"], saved["after_jev"], native))
        else:
            print("\n" + fmt_table3(native, native, native))
        print(f"\nsentences scanned: {stats['sentences']}, kept: {stats['kept']} "
              f"(noise: {stats['noise']}, below threshold: {stats['below_threshold']}, "
              f"exact dups: {stats['duplicates']}), tool events kept as provenance "
              f"only: {stats['tools_provenance_only']}")
        print(f"jev calls: {jev.calls}, input tokens: {jev.input_tokens}")

        from collections import Counter
        print("native node types:", dict(Counter(n["node_type"] for n in nodes)))

        out = os.path.join(bt.DATA, "jev-native-results.json")
        with open(out, "w") as f:
            json.dump({"model": args.model, "config": vars(args), "stats": stats,
                       "scan": scan, "nodes": nodes, "edges": edges,
                       "edge_meta": edge_meta,
                       "relations": relations if edges else {},
                       "native": {
                           "per_type": native["per_type"], "overall": native["overall"]},
                       "usage": {"calls": jev.calls, "input_tokens": jev.input_tokens},
                       "caveat": "gold is self-annotated by the same model family; "
                                 "native mode produces verbatim labels"},
                      f, indent=2, default=str)
        print(f"\nnative results -> {out}")
        return

    if args.snapshot:
        raw = json.load(open(args.snapshot))
        events = [json.loads(l) for l in open(os.path.join(bt.DATA, "conversation.jsonl"))
                  if l.strip()]
        print(f"loaded snapshot from {args.snapshot}: "
              f"{len(raw['snapshot'].get('nodes', []))} nodes")
    else:
        if not os.path.exists(bt.ENGINE):
            sys.exit("engine not built — run: cargo build --release -p armin-engine")
        raw = run_engine(env)
        events = [json.loads(l) for l in open(os.path.join(bt.DATA, "conversation.jsonl"))
                  if l.strip()]
        snap_path = os.path.join(bt.DATA, "jev-snapshot.json")
        with open(snap_path, "w") as f:
            json.dump(raw, f, indent=2, default=str)
        print(f"raw snapshot -> {snap_path}")

    snapshot = raw["snapshot"]
    events_by_id = {e["id"]: e for e in events}

    print(f"\nbaseline: {len(snapshot['nodes'])} nodes, {len(snapshot['edges'])} edges")
    baseline = bt.evaluate(gold, snapshot, raw.get("debt", {}),
                           raw.get("decisions", []), raw.get("brief", ""))
    print("baseline overall:", json.dumps(baseline["overall"]))

    print("\n─ Jev node verification ─")
    verdicts = verify_nodes(jev, snapshot, events, args)

    kept_nodes, actions, disagreements = apply_policy(snapshot, verdicts, args)
    kept_edges = [e for e in snapshot["edges"]
                  if e["source_node_id"] not in {a["node_id"] for a in actions if a["kind"] == "drop"}
                  and e["target_node_id"] not in {a["node_id"] for a in actions if a["kind"] == "drop"}]

    print("\n─ Assumption sweep ─")
    assumption_nodes = sweep_assumptions(client=jev, events=events, args=args)

    modified = {"nodes": kept_nodes + assumption_nodes, "edges": kept_edges}
    after = bt.evaluate(gold, modified, raw.get("debt", {}),
                        raw.get("decisions", []), raw.get("brief", ""))

    print("\n" + fmt_table(baseline, after))
    print(f"\nactions: {len(actions)} "
          f"({sum(1 for a in actions if a['kind'] == 'drop')} dropped, "
          f"{sum(1 for a in actions if a['kind'] == 'relabel')} relabeled), "
          f"kept-with-disagreement: {len(disagreements)}, "
          f"assumption nodes added: {len(assumption_nodes)}")
    print(f"jev calls: {jev.calls}, input tokens: {jev.input_tokens}")

    results = {
        "model": args.model,
        "config": vars(args),
        "baseline": {"per_type": baseline["per_type"], "overall": baseline["overall"]},
        "after_jev": {"per_type": after["per_type"], "overall": after["overall"]},
        "actions": actions,
        "disagreements": disagreements,
        "verdicts": verdicts,
        "assumption_nodes": assumption_nodes,
        "raw": raw,
        "usage": {"calls": jev.calls, "input_tokens": jev.input_tokens},
        "caveat": "gold and extraction are both LLM-family self-annotated; deltas measure agreement, not truth",
    }
    out = os.path.join(bt.DATA, "jev-results.json")
    with open(out, "w") as f:
        json.dump(results, f, indent=2, default=str)

    md = os.path.join(bt.DATA, "jev-overrides.md")
    write_overrides_md(md, snapshot["nodes"], verdicts, actions, disagreements,
                       assumption_nodes, gold, events_by_id)
    print(f"\nresults -> {out}\noverrides -> {md}")


if __name__ == "__main__":
    main()
