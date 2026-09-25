#!/usr/bin/env bash
# Install ARMIN: build (or fetch) the armin-engine sidecar, install it to
# ~/.local/bin, register the opencode plugin globally, and verify.
#
# Usage:
#   scripts/install.sh              # build from source (needs cargo)
#   scripts/install.sh --skip-build # assume the binary is already installed
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${ARMIN_BIN_DIR:-$HOME/.local/bin}"
BIN="$BIN_DIR/armin-engine"

echo "── ARMIN install ──────────────────────────────────────────────"

# 1. Engine binary.
if [[ "${1:-}" == "--skip-build" ]]; then
    echo "skipping build (--skip-build)"
elif [[ -x "$BIN" ]] && [[ -x "$REPO/armin-core/target/release/armin-engine" ]] && \
     [[ "$BIN" -nt "$REPO/armin-core/extraction/src" ]]; then
    echo "engine binary up to date: $BIN"
else
    if ! command -v cargo >/dev/null 2>&1; then
        echo "error: cargo not found. Install Rust (https://rustup.rs) or download"
        echo "       a prebuilt armin-engine from https://github.com/bastian-seifert/armin/releases"
        exit 1
    fi
    echo "building armin-engine (release) ..."
    (cd "$REPO/armin-core" && cargo build --release -p armin-engine)
    mkdir -p "$BIN_DIR"
    cp "$REPO/armin-core/target/release/armin-engine" "$BIN"
    echo "installed: $BIN ($("$BIN" --version 2>/dev/null || echo 'ok'))"
fi

if [[ ! -x "$BIN" ]]; then
    echo "error: $BIN not found and --skip-build given"
    exit 1
fi

# 2. Extraction backend: jev (System One judgments) is the default. Ask
#    which relay to use and for the matching key; it goes into the opencode
#    config — the plugin passes it to the sidecar. A provider detected from
#    the environment skips the prompt entirely.
TS_KEY_INPUT=""
OR_KEY_INPUT=""
WRITE_JEV_PROVIDER=""
if [[ -n "${ARMIN_JEV_PROVIDER:-}" ]]; then
    provider="${ARMIN_JEV_PROVIDER}"
elif [[ -n "${TYPESAFE_AI_API_KEY:-}" ]]; then
    provider="typesafe"
elif [[ -n "${OPENROUTER_API_KEY:-}" ]]; then
    provider="openrouter"
else
    provider=""
fi

if [[ -t 0 ]]; then
    if [[ -z "$provider" ]]; then
        echo
        echo "Extraction backend (jev is the default — verbatim nodes, no generative LLM):"
        echo "  1) TypeSafe System One (typesafe.ai)  [default]"
        echo "  2) Jev via OpenRouter (openrouter.ai — same API, OpenRouter billing)"
        echo "  3) skip (LLM fallback / deterministic-only)"
        read -r -p "Choose [1/2/3, Enter=1]: " BACKEND_CHOICE || BACKEND_CHOICE=""
        case "${BACKEND_CHOICE:-1}" in
            2) provider="openrouter" ;;
            3) provider="skip" ;;
            *) provider="typesafe" ;;
        esac
    fi
    if [[ "$provider" == "typesafe" && -z "${TYPESAFE_AI_API_KEY:-}" ]]; then
        read -r -p "TypeSafe System One API key — paste, or Enter to skip: " \
            TS_KEY_INPUT || TS_KEY_INPUT=""
    elif [[ "$provider" == "openrouter" && -z "${OPENROUTER_API_KEY:-}" ]]; then
        read -r -p "OpenRouter API key (openrouter.ai/settings/keys) — paste, or Enter to skip: " \
            OR_KEY_INPUT || OR_KEY_INPUT=""
    fi
fi

# 2b. Engine port: the sidecar auto-assigns a free loopback port per session
#     (ARMIN_PORT= handshake on stdout). A fixed port can be pinned here for
#     firewall rules or monitoring — it is written to the opencode config as
#     "armin": { "port": N }. A busy pinned port auto-falls back to a free
#     port at session start. ARMIN_PORT in the environment is the runtime
#     escape hatch and skips the prompt.
PORT_INPUT=""
if [[ -n "${ARMIN_PORT:-}" ]]; then
    PORT_INPUT="$ARMIN_PORT"
elif [[ -t 0 ]]; then
    echo
    read -r -p "Fixed engine port (Enter = auto-assign per session): " \
        PORT_INPUT || PORT_INPUT=""
fi
if [[ -n "${PORT_INPUT:-}" ]]; then
    if [[ "$PORT_INPUT" =~ ^[0-9]+$ ]] && (( PORT_INPUT >= 1024 && PORT_INPUT <= 65535 )); then
        echo "engine port: pinned to $PORT_INPUT (auto-falls back to a free port if busy)"
    else
        echo "ignoring invalid port \"$PORT_INPUT\" (need 1024-65535) — engine port: auto-assign"
        PORT_INPUT=""
    fi
fi

# The provider choice is persisted only when the user explicitly picked
# OpenRouter in the prompt (env-detected providers drive the sidecar via
# their env vars; writing nothing avoids overriding that later).
if [[ "${BACKEND_CHOICE:-}" == "2" ]]; then
    WRITE_JEV_PROVIDER="openrouter"
fi

# 3. Register the plugin in the global opencode config so EVERY project
#    gets ARMIN (the plugin itself is opt-in per environment via
#    ARMIN_ENABLED=1).
#
#    Dev/source layout: the file:// form only works here because the plugin
#    sits next to the repo's .opencode/node_modules (where opencode installs
#    the plugin SDK). It must never be used for the npm-installed package —
#    `armin install` writes the npm tuple form instead.
#
#    The config is the user's, not ours: register_plugin() patches only the
#    members it changes, so comments, indentation and line endings survive, and
#    it keeps a one-time <config>.armin-bak of whatever it first overwrote.
PLUGIN_PATH="$REPO/.opencode/plugins/armin.ts"
CFG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/opencode"
CFG_JSON="$CFG_DIR/opencode.json"
CFG_JSONC="$CFG_DIR/opencode.jsonc"

# Installed opencode generation: 2 registers under "plugins" with a
# { package, options } entry, 1 under "plugin" with the file:// entry.
# Unknown → the v1 form, which opencode v2 still normalizes.
opencode_major() {
    if [[ -n "${ARMIN_OPENCODE_MAJOR:-}" ]]; then
        printf '%s' "$ARMIN_OPENCODE_MAJOR"
        return
    fi
    if command -v opencode >/dev/null; then
        opencode --version 2>/dev/null | grep -oE '^[0-9]+' | head -1
    fi
}

register_plugin() {
    local cfg="$1"
    TS_KEY_INPUT="$TS_KEY_INPUT" OR_KEY_INPUT="$OR_KEY_INPUT" \
    WRITE_JEV_PROVIDER="$WRITE_JEV_PROVIDER" PORT_INPUT="$PORT_INPUT" \
    OPENCODE_MAJOR="$(opencode_major)" \
    python3 - "$cfg" "$PLUGIN_PATH" <<'PYEOF'
import json, os, re, sys

cfg, plugin = sys.argv[1], sys.argv[2]
# v2 reads "plugins": [{ package, options }]; v1 reads "plugin": ["file://…"].
major = (os.environ.get("OPENCODE_MAJOR") or "").strip()
v2 = major.isdigit() and int(major) >= 2
key, other = ("plugins", "plugin") if v2 else ("plugin", "plugins")
existed = os.path.exists(cfg)
text = open(cfg).read() if existed else ""

# ── Surgical JSONC editing ───────────────────────────────────────────────────
# The config is the user's, not ours: comments, indentation and line endings
# outside the members we touch must survive byte-for-byte. There is no JSONC
# library for Python and pip-installing one would break this script's "no
# extra tooling" contract, so the scanning lives here. JSON strings cannot
# contain a raw newline, which is what makes the one-pass tokenizer sound.
import json
import re

TOKEN = re.compile(r'"(?:[^"\\]|\\.)*"|//[^\n]*|/\*.*?\*/', re.S)


def _scan(text, start=0):
    """Yield (index, token) for every string, comment and structural char,
    so callers never step into trivia or into the middle of a string."""
    i, n = start, len(text)
    while i < n:
        m = TOKEN.match(text, i)
        if m:
            yield i, m.group(0)
            i = m.end()
            continue
        yield i, text[i]
        i += 1


def _drop_trailing_commas(blanked):
    """Blank out commas that directly precede a closing bracket."""
    out = list(blanked)
    i, n = 0, len(out)
    while i < n:
        ch = out[i]
        if ch == '"':
            i += 1
            while i < n:
                if out[i] == "\\":
                    i += 2
                    continue
                if out[i] == '"':
                    break
                i += 1
        elif ch == ",":
            j = i + 1
            while j < n and out[j] in " \t\r\n":
                j += 1
            if j < n and out[j] in "}]":
                out[i] = " "
        i += 1
    return "".join(out)


def parse(text):
    """Parse JSONC by blanking comments (a space per character, so byte
    offsets survive) and then dropping trailing commas. Raises on bad input."""
    pieces, pos = [], 0
    for m in TOKEN.finditer(text):
        chunk = m.group(0)
        if chunk[0] == '"':
            continue
        pieces.append(text[pos:m.start()])
        pieces.append(re.sub(r"[^\n]", " ", chunk))
        pos = m.end()
    pieces.append(text[pos:])
    blanked = _drop_trailing_commas("".join(pieces))
    return json.loads(blanked) if blanked.strip() else {}


def top_members(text):
    """[(key, member_start, value_start, value_end)] for the depth-1
    properties of the root object, in source order."""
    out = []
    depth = 0
    key = None
    member_start = value_start = value_end = None
    vdepth = 0
    for pos, tok in _scan(text):
        if len(tok) > 1 and tok[0] == '"':
            if depth == 1 and key is None and member_start is None:
                key = json.loads(tok)
                member_start = pos
            continue
        if key is not None and value_start is None and tok == ":":
            value_start, vdepth = pos + 1, 0
            continue
        if value_start is not None and value_end is None:
            if tok in "{[":
                vdepth += 1
            elif tok in "}]":
                if vdepth == 0:
                    value_end = pos
                    out.append((key, member_start, value_start, value_end))
                    key = member_start = value_start = value_end = None
                else:
                    vdepth -= 1
            elif tok == "," and vdepth == 0:
                value_end = pos
                out.append((key, member_start, value_start, value_end))
                key = member_start = value_start = value_end = None
            continue
        if tok == "{":
            depth += 1
        elif tok == "}":
            depth -= 1
            if depth == 0:
                break
    return out


def detect_indent(text):
    """The indentation of the first indented line — a good enough proxy for
    the file's own indent unit, and never worse than assuming two spaces."""
    m = re.search(r"\n([ \t]+)\S", text)
    return m.group(1) if m else "  "


def dumps_member(value, base_indent, indent, eol):
    """Serialize `value` as it would sit at `base_indent` in this document,
    reusing the file's own indent unit and line ending."""
    lines = json.dumps(value, indent=len(indent)).split("\n")
    return eol.join([lines[0]] + [base_indent + l for l in lines[1:]])


def _member_indent(text, offset, indent):
    """The leading whitespace of the line `offset` starts on, or the file's
    indent unit if that line has other content in front of it."""
    line_start = text.rfind("\n", 0, offset) + 1
    base = text[line_start:offset]
    return base if not base.strip() else indent


def _root_close(text):
    """Offset of the root object's closing brace."""
    depth = 0
    for _pos, tok in _scan(text):
        if tok == "{":
            depth += 1
        elif tok == "}":
            depth -= 1
            if depth == 0:
                return _pos
    return len(text)


def set_member(text, key, value):
    """Replace the value of the top-level member `key`, creating the member if
    it is absent. Comments, indentation and line endings elsewhere survive."""
    indent = detect_indent(text)
    eol = "\r\n" if "\r\n" in text else "\n"
    members = top_members(text)

    for name, m_start, v_start, v_end in members:
        if name != key:
            continue
        # v_start sits right after the colon, so the gap before the value is
        # part of what we replace; re-emit it.
        while v_start < v_end and text[v_start] in " \t":
            v_start += 1
        # Trailing trivia — the newline and indent before the next member or
        # the closing brace — is inside the replaced span too, and a serialized
        # value never ends in a newline, so put it back verbatim.
        trail = v_end
        while trail > v_start and text[trail - 1] in " \t\r\n":
            trail -= 1
        literal = dumps_member(value, _member_indent(text, m_start, indent), indent, eol)
        return text[:v_start] + literal + text[trail:]

    # Absent: append before the root's closing brace at the member indent.
    base = _member_indent(text, members[0][1], indent) if members else indent
    literal = json.dumps(key) + ": " + dumps_member(value, base, indent, eol)
    if "{" not in text:
        return "{" + eol + base + literal + eol + "}" + eol
    close = _root_close(text)
    head = text[:close].rstrip()
    if not head.strip():
        return head + eol + base + literal + eol + text[close:]
    sep = "" if head.endswith("{") else ","
    return head + sep + eol + base + literal + eol + text[close:]


def delete_member(text, key):
    """Remove the top-level member `key` and one adjacent comma, taking the
    line it sat on with it. Trivia in front of the member is left alone, so a
    comment above it survives."""
    for name, m_start, _v_start, v_end in top_members(text):
        if name != key:
            continue
        i = v_end
        while i < len(text) and text[i] in " \t":
            i += 1
        end = i + 1 if i < len(text) and text[i] == "," else v_end

        j = m_start
        while j > 0 and text[j - 1] in " \t":
            j -= 1
        if j > 0 and text[j - 1] == "\n":
            j -= 1
        k = j
        while k > 0 and text[k - 1] in " \t":
            k -= 1
        if k > 0 and text[k - 1] == ",":
            j = k - 1
        return text[:j] + text[end:]
    return text


def append_member_item(text, key, value):
    """Append to the array at top-level member `key`, leaving the existing
    elements — and any comments between them — byte-identical. Returns None
    when the member is missing or is not an array, so the caller can fall back
    to replacing the whole value."""
    indent = detect_indent(text)
    eol = "\r\n" if "\r\n" in text else "\n"
    target = next((m for m in top_members(text) if m[0] == key), None)
    if target is None:
        return None
    _name, m_start, v_start, v_end = target
    open_at = v_start
    while open_at < v_end and text[open_at] in " \t":
        open_at += 1
    if text[open_at] != "[":
        return None

    depth, close = 0, None
    for pos, tok in _scan(text, open_at):
        if tok == "[":
            depth += 1
        elif tok == "]":
            depth -= 1
            if depth == 0:
                close = pos
                break
    if close is None:
        return None

    lines = json.dumps(value, indent=len(indent)).split("\n")
    base = _member_indent(text, m_start, indent)
    literal = eol.join(base + indent + l for l in lines)

    inner = text[open_at + 1 : close]
    if not inner.strip():
        # Empty array: give the bracket a line of its own at the member indent.
        return text[: open_at + 1] + eol + literal + eol + base + text[close:]

    # Insert after the last element, keeping the closing bracket on its own
    # line with whatever indentation it already had.
    p = close
    while p > open_at and text[p - 1] in " \t":
        p -= 1
    if p > open_at and text[p - 1] == "\n":
        return text[: p - 1] + "," + eol + literal + eol + text[p:close] + text[close:]
    return text[:close] + ", " + literal.strip() + text[close:]


def has_attached_comment(text, key):
    """True when a comment sits between the previous member (or the opening
    brace) and the start of `key` — the trivia a deletion would swallow."""
    members = top_members(text)
    prev = text.index("{")
    for name, m_start, _v, _e in members:
        if name == key:
            return "//" in text[prev:m_start] or "/*" in text[prev:m_start]
        prev = _e
        while prev < len(text) and text[prev] != ",":
            prev += 1
    return False

def data_updated(data, pending):
    """The parsed value `out` is expected to be equivalent to."""
    out = dict(data)
    for member, op, value in pending:
        if op == "delete":
            out.pop(member, None)
        elif op == "append":
            out[member] = list(out.get(member) or []) + [value]
        else:
            out[member] = value
    return out


try:
    data = parse(text)
except Exception as exc:
    if v2:
        print(f'could not parse {cfg} ({exc}) — register the plugin manually:\n  add {{ "plugins": [{{ "package": "file://{plugin}", "options": {{ "enabled": true }} }}] }}')
    else:
        print(f'could not parse {cfg} ({exc}) — register the plugin manually:\n  add "plugin": ["file://{plugin}"]')
    sys.exit(0)

plugins = data.get(key) if isinstance(data.get(key), list) else []
entry = "file://" + plugin
armin = data.get("armin") or {}
# The members to rewrite, as (key, op, value) triples where op is "set",
# "append" or "delete". Each is patched independently, so anything we do not
# change is never touched.
pending = []
changed = False

# Replace any previous armin plugin entries (stale file:// forms from older
# installers, or npm entries written by `armin install`) with the dev file://
# entry; duplicates would double-activate the plugin.
def is_armin(p):
    if isinstance(p, str):
        return p.startswith("armin-opencode") or (p.startswith("file://") and p.endswith("/plugins/armin.ts"))
    if isinstance(p, list):
        return len(p) == 2 and isinstance(p[0], str) and p[0].startswith("armin-opencode")
    if isinstance(p, dict):
        return isinstance(p.get("package"), str) and p["package"].startswith("armin-opencode")
    return False

if any(is_armin(p) and p != entry for p in plugins):
    pending.append((key, "set", [entry] + [p for p in plugins if not is_armin(p)]))
    changed = True
elif entry not in plugins:
    # Append rather than rewrite, so comments between existing entries stay.
    pending.append((key, "append", entry))
    changed = True

# A stale armin entry under the other generation's key double-activates.
other_entries = data.get(other)
if isinstance(other_entries, list):
    kept = [p for p in other_entries if not is_armin(p)]
    if len(kept) != len(other_entries):
        pending.append((other, "delete" if not kept else "set", kept or None))
        changed = True

ts_input = os.environ.get("TS_KEY_INPUT", "").strip()
if ts_input and not os.environ.get("TYPESAFE_AI_API_KEY") and armin.get("typesafeKey") != ts_input:
    armin["typesafeKey"] = ts_input
    changed = True
or_input = os.environ.get("OR_KEY_INPUT", "").strip()
if or_input and not os.environ.get("OPENROUTER_API_KEY") and armin.get("openrouterKey") != or_input:
    armin["openrouterKey"] = or_input
    changed = True
provider = os.environ.get("WRITE_JEV_PROVIDER", "").strip()
if provider and armin.get("jevProvider") != provider:
    armin["jevProvider"] = provider
    changed = True
port_input = os.environ.get("PORT_INPUT", "").strip()
if port_input.isdigit() and 1024 <= int(port_input) <= 65535 and armin.get("port") != int(port_input):
    armin["port"] = int(port_input)
    changed = True

if changed:
    pending.append(("armin", "set", armin))
    try:
        out = text
        effective = []
        for member, op, value in pending:
            if op == "append":
                appended = append_member_item(out, member, value)
                if appended is None:
                    op = "set"  # not an array, or absent: replace the value
                    value = list(data.get(member) or []) + [value]
            if op == "delete":
                # A deletion takes the comment directly above the member with
                # it. For the plugin arrays that comment is usually about the
                # config as a whole, so blank the member instead — an empty
                # array is inert and opencode reads it like an absent key.
                # "armin" is always removed: its comment describes the
                # settings being migrated away.
                if member != "armin" and has_attached_comment(out, member):
                    op, value = "set", []
            effective.append((member, op, value))
            if op == "append":
                out = appended
            elif op == "delete":
                out = delete_member(out, member)
            else:
                out = set_member(out, member, value)
        # Never write a config we cannot read back to the value we intended.
        if parse(out) != data_updated(data, effective):
            raise ValueError("edit did not round-trip")
    except Exception as exc:
        # Fall back to the old whole-file rewrite: correct, but it does not
        # preserve comments or formatting. Never worse than not trying.
        print(f"could not patch {cfg} in place ({exc}) — rewriting it, comments and formatting are not preserved")
        out = json.dumps(data_updated(data, pending), indent=2)
    os.makedirs(os.path.dirname(cfg), exist_ok=True)
    if existed:
        backup = cfg + ".armin-bak"
        if not os.path.exists(backup):
            try:
                import shutil
                shutil.copyfile(cfg, backup)
                print(f"original config backed up to {backup}")
            except Exception as exc:
                print(f"could not back up {cfg}: {exc}")
    with open(cfg, "w") as f:
        f.write(out)
    print(f"plugin registered in {cfg}")
else:
    print(f"plugin already registered in {cfg}")
PYEOF
}

if [[ -f "$CFG_JSONC" && ! -f "$CFG_JSON" ]]; then
    register_plugin "$CFG_JSONC"
else
    register_plugin "$CFG_JSON"
fi

# 4. Health check: spawn the engine briefly and verify the handshake. With a
#    pinned port this also proves the port is actually bindable right now.
echo "verifying engine ..."
TMPDIR_ENGINE="$(mktemp -d)"
HEALTH_PORT="${PORT_INPUT:-0}"
ARMIN_DB_DIR="$TMPDIR_ENGINE" "$BIN" --port "$HEALTH_PORT" >"$TMPDIR_ENGINE/log" 2>&1 &
ENGINE_PID=$!
HEALTH="unknown"
for _ in $(seq 1 30); do
    PORT=$(grep -o 'ARMIN_PORT=[0-9]*' "$TMPDIR_ENGINE/log" 2>/dev/null | cut -d= -f2 || true)
    if [[ -n "$PORT" ]]; then
        if curl -sf "http://127.0.0.1:$PORT/api/v1/health" >/dev/null 2>&1; then
            HEALTH="ok"
            break
        fi
    fi
    sleep 0.3
done
kill "$ENGINE_PID" 2>/dev/null || true
wait "$ENGINE_PID" 2>/dev/null || true
rm -rf "$TMPDIR_ENGINE"
if [[ "$HEALTH" == "ok" ]]; then
    echo "engine health: ok"
else
    echo "warning: engine health check did not complete (it may still work)"
    if [[ -n "${PORT_INPUT:-}" ]]; then
        echo "  if the pinned port is already in use, ARMIN auto-falls back to a"
        echo "  free port at session start"
    fi
fi

# 5. Extraction-mode status: jev (default) needs a Typesafe or OpenRouter key.
echo "checking extraction setup ..."
if [[ -n "${TYPESAFE_AI_API_KEY:-}" || -n "${TS_KEY_INPUT:-}" ]]; then
    echo "extraction: jev (TypeSafe System One) — ready"
elif [[ -n "${OPENROUTER_API_KEY:-}" || -n "${OR_KEY_INPUT:-}" ]]; then
    echo "extraction: jev via OpenRouter — ready"
elif [[ -n "${ANTHROPIC_API_KEY:-}" || -n "${OPENAI_API_KEY:-}" ]]; then
    echo "extraction: LLM fallback — jev is the default but no key for it is set."
    echo "  Set TYPESAFE_AI_API_KEY (typesafe.ai) or OPENROUTER_API_KEY (openrouter.ai),"
    echo "  or put \"armin\": { \"typesafeKey\": \"...\" } / { \"jevProvider\": \"openrouter\","
    echo '  "openrouterKey": "..." } in your opencode config.'
else
    echo "extraction: NONE (deterministic-only) — import, capture and unverified-edit"
    echo "  warnings work; prose extraction does not."
    echo "  Set TYPESAFE_AI_API_KEY (typesafe.ai) or OPENROUTER_API_KEY (openrouter.ai)"
    echo "  for jev extraction — the default — or ANTHROPIC_API_KEY / OPENAI_API_KEY"
    echo "  for LLM fallback."
fi

cat <<EOF

── done ────────────────────────────────────────────────────────
Enable ARMIN per environment:

    export ARMIN_ENABLED=1

Optional knobs:
    ARMIN_EXTRACTION_MODE=jev      System One extraction (default; needs
                                   TYPESAFE_AI_API_KEY or OPENROUTER_API_KEY)
    ARMIN_JEV_PROVIDER=openrouter  route jev via OpenRouter (auto-detected
                                   when only that key is set)
    ARMIN_JEV_MODEL=<model>        jev model override (default jev-1.13.0,
                                   or jev-latest via OpenRouter)
    ARMIN_MODEL=<model>            extraction model override (llm mode)
    ARMIN_DB_DIR=~/.opencode/armin graph storage (per project)
    ARMIN_PORT=<port>              fixed engine port (default: auto-assign;
                                   env wins over the config value)
    ARMIN_DEBUG=1                  verbose plugin logging

Start any opencode session in a git project — the reasoning-state brief
appears in your context, and the graph grows as you work. Inspect it via
the engine's /ui page (the plugin prints the URL with ARMIN_DEBUG=1).
EOF
