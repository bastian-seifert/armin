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

# 2. Register the plugin in the global opencode config so EVERY project
#    gets ARMIN (the plugin itself is opt-in per environment via
#    ARMIN_ENABLED=1).
PLUGIN_PATH="$REPO/.opencode/plugins/armin.ts"
CFG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/opencode"
CFG_JSON="$CFG_DIR/opencode.json"
CFG_JSONC="$CFG_DIR/opencode.jsonc"

register_plugin() {
    local cfg="$1"
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
if entry in plugins:
    print(f"plugin already registered in {cfg}")
else:
    plugins.append(entry)
    data["plugin"] = plugins
    with open(cfg, "w") as f:
        json.dump(data, f, indent=2)
    print(f"plugin registered in {cfg}")
PYEOF
}

if [[ -f "$CFG_JSONC" && ! -f "$CFG_JSON" ]]; then
    register_plugin "$CFG_JSONC"
else
    register_plugin "$CFG_JSON"
fi

# 3. Health check: spawn the engine briefly and verify the handshake.
echo "verifying engine ..."
TMPDIR_ENGINE="$(mktemp -d)"
ARMIN_DB_DIR="$TMPDIR_ENGINE" "$BIN" --port 0 >"$TMPDIR_ENGINE/log" 2>&1 &
ENGINE_PID=$!
HEALTH="unknown"
for _ in $(seq 1 30); do
    PORT=$(grep -o 'ARMIN_PORT=[0-9]*' "$TMPDIR_ENGINE/log" 2>/dev/null | cut -d= -f2)
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
fi

cat <<EOF

── done ────────────────────────────────────────────────────────
Enable ARMIN per environment:

    export ARMIN_ENABLED=1

Optional knobs:
    ARMIN_EXTRACTION_MODE=jev      TypeSafe System One (needs TYPESAFE_AI_API_KEY)
    ARMIN_MODEL=<model>            extraction model override (llm mode)
    ARMIN_DB_DIR=~/.opencode/armin graph storage (per project)
    ARMIN_DEBUG=1                  verbose plugin logging

Start any opencode session in a git project — the reasoning-state brief
appears in your context, and the graph grows as you work. Inspect it via
the engine's /ui page (the plugin prints the URL with ARMIN_DEBUG=1).
EOF
