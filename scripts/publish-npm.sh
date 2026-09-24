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

# The published package must keep "main": "plugins/armin.ts" plus the
# "exports" map (opencode v2 resolves the entry through "exports", v1 through
# "main"/"exports["./server"]") and the @opencode-ai/plugin dependency (v1
# sessions cannot resolve the SDK otherwise — file:// installs never get an
# npm install step). Fail loudly if the metadata drifts.
node - "$NPM_DIR/package.json" <<'EOF'
const fs = require("fs");
const pkg = JSON.parse(fs.readFileSync(process.argv[2], "utf-8"));
const problems = [];
if (pkg.main !== "plugins/armin.ts") problems.push(`"main" must be "plugins/armin.ts", got ${JSON.stringify(pkg.main)}`);
if (pkg.exports?.["."] !== "./plugins/armin.ts" || pkg.exports?.["./server"] !== "./plugins/armin.ts")
  problems.push(`"exports" must map "." and "./server" to ./plugins/armin.ts (opencode v2 entrypoint), got ${JSON.stringify(pkg.exports)}`);
if (!pkg.dependencies || !pkg.dependencies["@opencode-ai/plugin"]) problems.push("missing dependency @opencode-ai/plugin");
if (problems.length) {
  console.error("npm/package.json metadata check failed:\n  " + problems.join("\n  "));
  process.exit(1);
}
EOF

cd "$NPM_DIR"
npm publish --access public
echo "published armin-opencode@$VERSION — binaries come from the v$VERSION GitHub release"
