#!/usr/bin/env bash
# Build the armin-engine sidecar binary and install it to ~/.local/bin.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT/armin-core"

cargo build --release -p armin-engine

BIN="$ROOT/armin-core/target/release/armin-engine"
DEST="${ARMIN_BIN_DIR:-$HOME/.local/bin}/armin-engine"

mkdir -p "$(dirname "$DEST")"
cp "$BIN" "$DEST"
echo "Installed armin-engine -> $DEST"
echo
echo "Enable the plugin in any project by setting:"
echo "  export ARMIN_ENABLED=1"
