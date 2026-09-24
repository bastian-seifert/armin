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
PLUGIN_PATH="$REPO/.opencode/plugins/armin.ts"
CFG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/opencode"
CFG_JSON="$CFG_DIR/opencode.json"
CFG_JSONC="$CFG_DIR/opencode.jsonc"

register_plugin() {
    local cfg="$1"
    TS_KEY_INPUT="$TS_KEY_INPUT" OR_KEY_INPUT="$OR_KEY_INPUT" \
    WRITE_JEV_PROVIDER="$WRITE_JEV_PROVIDER" PORT_INPUT="$PORT_INPUT" \
    python3 - "$cfg" "$PLUGIN_PATH" <<'PYEOF'
import json, re, sys, os
cfg, plugin = sys.argv[1], sys.argv[2]
text = open(cfg).read() if os.path.exists(cfg) else ""
# JSONC: strip // comments before parsing, restore later is best-effort —
# if parsing fails we do not touch the file and print manual instructions.
try:
    stripped = re.sub(r"^\s*//.*$", "", text, flags=re.M)
    data = json.loads(stripped) if stripped.strip() else {}
except Exception:
    print(f"could not parse {cfg} — register the plugin manually:")
    print(f'  add "plugin": ["file://{plugin}"] to your opencode config')
    sys.exit(0)
plugins = data.get("plugin", [])
entry = "file://" + plugin
armin = data.get("armin") or {}
changed = False
# Replace any previous armin plugin entries (stale file:// forms from older
# installers, or npm tuples written by `armin install`) with the dev file://
# entry; duplicates would double-activate the plugin.
def is_armin(p):
    if isinstance(p, str):
        return p.startswith("armin-opencode") or (p.startswith("file://") and p.endswith("/plugins/armin.ts"))
    return isinstance(p, list) and len(p) == 2 and isinstance(p[0], str) and p[0].startswith("armin-opencode")
if any(is_armin(p) and p != entry for p in plugins):
    plugins = [entry] + [p for p in plugins if not is_armin(p)]
    data["plugin"] = plugins
    changed = True
elif entry not in plugins:
    plugins.append(entry)
    data["plugin"] = plugins
    changed = True
ts_input = os.environ.get("TS_KEY_INPUT", "").strip()
if ts_input and not os.environ.get("TYPESAFE_AI_API_KEY"):
    if armin.get("typesafeKey") != ts_input:
        armin["typesafeKey"] = ts_input
        changed = True
or_input = os.environ.get("OR_KEY_INPUT", "").strip()
if or_input and not os.environ.get("OPENROUTER_API_KEY"):
    if armin.get("openrouterKey") != or_input:
        armin["openrouterKey"] = or_input
        changed = True
provider = os.environ.get("WRITE_JEV_PROVIDER", "").strip()
if provider and armin.get("jevProvider") != provider:
    armin["jevProvider"] = provider
    changed = True
port_input = os.environ.get("PORT_INPUT", "").strip()
if port_input.isdigit() and 1024 <= int(port_input) <= 65535:
    port = int(port_input)
    if armin.get("port") != port:
        armin["port"] = port
        changed = True
if changed:
    data["armin"] = armin
    os.makedirs(os.path.dirname(cfg), exist_ok=True)
    with open(cfg, "w") as f:
        json.dump(data, f, indent=2)
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
