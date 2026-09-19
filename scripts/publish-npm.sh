#!/usr/bin/env bash
# Publish armin-opencode to npm. Stages the plugin file and syncs the
# version from the Rust workspace, then publishes.
#
# Requires: npm login (npm whoami), and version bump in armin-core/Cargo.toml
# already released as a git tag (the installer fetches that tag's binaries).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NPM_DIR="$REPO/npm"

VERSION=$(grep -m1 '^version' "$REPO/armin-core/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
echo "publishing armin-opencode@$VERSION"

node - "$NPM_DIR/package.json" "$VERSION" <<'EOF'
const fs = require("fs");
const [pkgPath, version] = process.argv.slice(2);
const pkg = JSON.parse(fs.readFileSync(pkgPath, "utf-8"));
pkg.version = version;
fs.writeFileSync(pkgPath, JSON.stringify(pkg, null, 2) + "\n");
EOF

mkdir -p "$NPM_DIR/plugins"
cp "$REPO/.opencode/plugins/armin.ts" "$NPM_DIR/plugins/armin.ts"

cd "$NPM_DIR"
npm publish --access public
echo "published armin-opencode@$VERSION — binaries come from the v$VERSION GitHub release"
