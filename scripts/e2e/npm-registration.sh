#!/usr/bin/env bash
# E2E: the npm-installed plugin layout actually loads in opencode.
#
# This is the regression test for the silent-failure bug: the installer used
# to register "plugin": ["file://<pkg_root>/plugins/armin.ts"], which cannot
# resolve @opencode-ai/plugin from a project's node_modules — opencode
# published a transient TUI error and never logged anything, leaving ARMIN
# "installed but inert". The npm tuple form ("plugin":
# [["armin-opencode@x", {...}]]) makes opencode install the package WITH its
# declared SDK dependency into its own cache.
#
# What it does:
#   1. npm pack the npm/ package into a scratch registry dir
#   2. npm install the tarball into a fake global prefix (real node_modules
#      layout, not the repo checkout)
#   3. run `armin install` (non-TTY) with an isolated XDG_CONFIG_HOME
#   4. run `armin doctor`
#   5. run the same install/doctor against the opencode v2 config form, and
#      drive the v2 entrypoint (default export id + setup) with a stub context
#   6. run a real headless `opencode run` session in a scratch project and
#      assert the plugin's unconditional "[armin] ..." activation line or an
#      explicit load-failure marker appears
#
# Requirements: node/npm, bun, opencode on PATH, network for npm + model calls
# (opencode needs at least one working model; the prompt is trivial).
#
# Usage: scripts/e2e/npm-registration.sh [--keep]
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
NPM_DIR="$REPO/npm"
KEEP=0
[[ "${1:-}" == "--keep" ]] && KEEP=1

WORK="$(mktemp -d /tmp/armin-e2e.XXXXXX)"
cleanup() {
    if [[ "$KEEP" == "1" ]]; then
        echo "e2e: keeping workdir $WORK"
    else
        rm -rf "$WORK"
    fi
}
trap cleanup EXIT

fail() { echo "e2e: FAIL — $*" >&2; exit 1; }
pass() { echo "e2e: ok — $*"; }

command -v opencode >/dev/null || fail "opencode not on PATH"
command -v npm >/dev/null || fail "npm not on PATH"

# ── 1. Build the package the way npm users get it ────────────────────────────
# The installer registers a PINNED spec ("armin-opencode@<version>"), and
# opencode fetches pinned specs from the real npm registry. To avoid colliding
# with a published version (whose metadata predates this change), pack from a
# scratch copy stamped with a unique never-published prerelease version.
echo "── npm pack (unique e2e version)"
E2E_VERSION="0.0.0-e2e.$(date +%s)"
STAGE="$WORK/stage"
cp -r "$NPM_DIR" "$STAGE"
node - "$STAGE/package.json" "$E2E_VERSION" <<'EOF'
const fs = require("fs");
const [pkgPath, version] = process.argv.slice(2);
const pkg = JSON.parse(fs.readFileSync(pkgPath, "utf-8"));
pkg.version = version;
fs.writeFileSync(pkgPath, JSON.stringify(pkg, null, 2) + "\n");
EOF
(cd "$STAGE" && npm pack --pack-destination "$WORK") || fail "npm pack failed"
TARBALL="$WORK/armin-opencode-$E2E_VERSION.tgz"
[[ -f "$TARBALL" ]] || fail "no tarball produced"

# The published metadata that makes npm registration loadable at all:
node - "$TARBALL" <<'EOF' || fail "package metadata check failed"
const { execFileSync } = require("child_process");
const tarball = process.argv[2];
const out = execFileSync("tar", ["-xzf", tarball, "-O", "package/package.json"], { encoding: "utf-8" });
const pkg = JSON.parse(out);
const problems = [];
if (pkg.main !== "plugins/armin.ts") problems.push(`"main" is ${JSON.stringify(pkg.main)}, opencode's server entrypoint detection needs "plugins/armin.ts"`);
// v2 resolves the entry through "exports" ("./server", then "."), not "main".
if (pkg.exports?.["."] !== "./plugins/armin.ts" || pkg.exports?.["./server"] !== "./plugins/armin.ts")
    problems.push(`"exports" must map "." and "./server" to ./plugins/armin.ts (opencode v2 entrypoint resolution), got ${JSON.stringify(pkg.exports)}`);
if (!pkg.dependencies?.["@opencode-ai/plugin"]) problems.push("missing dependency @opencode-ai/plugin — opencode v1 cannot resolve the SDK and loads nothing");
if (problems.length) { console.error("  " + problems.join("\n  ")); process.exit(1); }
EOF
pass "package metadata (main + exports + SDK dependency)"

# ── 2. Install into a real prefix layout (NOT the repo checkout) ─────────────
echo "── npm install into scratch prefix"
PREFIX="$WORK/prefix"
npm install --prefix "$PREFIX" "$TARBALL" --no-audit --no-fund --loglevel=error \
    || fail "npm install into scratch prefix failed"
PKG_ROOT="$PREFIX/node_modules/armin-opencode"
[[ -f "$PKG_ROOT/plugins/armin.ts" ]] || fail "plugin file missing from installed layout"
# npm may hoist deps to the prefix root (flat layout) or nest them — what
# matters is that the SDK resolves FROM the plugin file. This is exactly what
# the broken file:// registration could never guarantee: without a dependency
# declaration there is no install step, hence no SDK anywhere.
node --experimental-import-meta-resolve -e '
const path = require("path");
const { pathToFileURL } = require("url");
const pluginDir = process.argv[1];
const from = pathToFileURL(path.join(pluginDir, "plugins", "armin.ts")).href;
// Same resolution semantics opencode uses (bun honors the parent arg of
// import.meta.resolve; node needs the experimental flag).
import.meta.resolve("@opencode-ai/plugin", from);
console.log("sdk resolves from the installed plugin");
' "$PKG_ROOT" 2>/dev/null \
  || bun -e 'import.meta.resolve("@opencode-ai/plugin", "file://" + process.argv[1] + "/plugins/armin.ts"); console.log("sdk resolves from the installed plugin (bun)")' "$PKG_ROOT" 2>/dev/null \
  || fail "@opencode-ai/plugin does not resolve from the installed plugin (this is the bug: file:// registration has no npm install step)"
pass "SDK resolves from the installed package"

# ── 3. armin install in an isolated config home ──────────────────────────────
echo "── armin install"
# The e2e version has no matching GitHub release; pre-seed the engine binary
# so install() skips the download (the engine itself is not under test here).
mkdir -p "$WORK/bin"
if [[ -x "$HOME/.local/bin/armin-engine" ]]; then
    cp "$HOME/.local/bin/armin-engine" "$WORK/bin/armin-engine"
else
    printf '#!/bin/sh\n' >"$WORK/bin/armin-engine"; chmod +x "$WORK/bin/armin-engine"
fi
XDG_CONFIG_HOME="$WORK/xdg/config" \
XDG_DATA_HOME="$WORK/xdg/data" \
XDG_CACHE_HOME="$WORK/xdg/cache" \
HOME="$WORK/home" \
ARMIN_BIN_DIR="$WORK/bin" \
    "$PKG_ROOT/bin/armin.js" install >"$WORK/install.log" 2>&1 \
    || fail "armin install failed (see $WORK/install.log)"

CFG="$WORK/xdg/config/opencode/opencode.json"
[[ -f "$CFG" ]] || fail "no opencode config written"
node - "$CFG" "$E2E_VERSION" <<'EOF' || fail "registration form check failed"
const cfg = JSON.parse(require("fs").readFileSync(process.argv[2], "utf-8"));
const plugins = cfg.plugin || [];
const want = "armin-opencode@" + process.argv[3];
const hit = plugins.find((p) => Array.isArray(p) && p[0] === want);
if (!hit) {
    console.error("  no pinned tuple entry in \"plugin\": " + JSON.stringify(plugins));
    console.error("  expected spec: " + want);
    console.error("  (file:// entries in a node_modules layout cannot resolve the SDK)");
    process.exit(1);
}
if (hit[1]?.enabled !== true) { console.error("  tuple options missing enabled:true"); process.exit(1); }
EOF
pass "installer wrote the npm tuple registration"

node -e '
const cfg = JSON.parse(require("fs").readFileSync(process.argv[1], "utf-8"));
if ("armin" in cfg) { console.error("  legacy armin section still present — should be migrated into tuple options"); process.exit(1); }
' "$CFG" || fail "legacy armin section not migrated"
pass "legacy armin section migrated into tuple options"

# ── 3b. opencode v2 config form ──────────────────────────────────────────────
# v2 renamed "plugin" to "plugins" and the tuple to { package, options }.
# Detection is overridden so this step is deterministic regardless of which
# opencode happens to be on PATH.
echo "── armin install (v2 config form)"
V2_HOME="$WORK/v2"
mkdir -p "$V2_HOME/home" "$V2_HOME/bin"
cp "$HOME/.local/bin/armin-engine" "$V2_HOME/bin/armin-engine" 2>/dev/null || cp "$WORK/bin/armin-engine" "$V2_HOME/bin/armin-engine"
XDG_CONFIG_HOME="$V2_HOME/xdg/config" \
XDG_DATA_HOME="$V2_HOME/xdg/data" \
XDG_CACHE_HOME="$V2_HOME/xdg/cache" \
HOME="$V2_HOME/home" \
ARMIN_BIN_DIR="$V2_HOME/bin" \
ARMIN_OPENCODE_MAJOR=2 \
    "$PKG_ROOT/bin/armin.js" install >"$WORK/install-v2.log" 2>&1 \
    || fail "armin install (v2) failed (see $WORK/install-v2.log)"
V2_CFG="$V2_HOME/xdg/config/opencode/opencode.json"
node - "$V2_CFG" "$E2E_VERSION" <<'EOF' || fail "v2 registration form check failed"
const cfg = JSON.parse(require("fs").readFileSync(process.argv[2], "utf-8"));
const want = "armin-opencode@" + process.argv[3];
const entries = Array.isArray(cfg.plugins) ? cfg.plugins : [];
const hit = entries.find((p) => p && typeof p === "object" && p.package === want);
if (!hit) {
    console.error("  no { package, options } entry in \"plugins\": " + JSON.stringify(entries));
    process.exit(1);
}
if (hit.options?.enabled !== true) { console.error("  options missing enabled:true"); process.exit(1); }
if ((cfg.plugin || []).some((p) => JSON.stringify(p).includes("armin-opencode")))
    { console.error("  stale armin entry left under the v1 \"plugin\" key — it would double-activate"); process.exit(1); }
EOF
pass "installer wrote the v2 { package, options } registration"

XDG_CONFIG_HOME="$V2_HOME/xdg/config" \
XDG_DATA_HOME="$V2_HOME/xdg/data" \
XDG_CACHE_HOME="$V2_HOME/xdg/cache" \
HOME="$V2_HOME/home" \
ARMIN_BIN_DIR="$V2_HOME/bin" \
    "$PKG_ROOT/bin/armin.js" doctor >"$WORK/doctor-v2.log" 2>&1 \
    || fail "armin doctor (v2 config) reported problems (see $WORK/doctor-v2.log)"
grep -q "v2 config form" "$WORK/doctor-v2.log" || fail "doctor did not recognize the v2 form: $(cat "$WORK/doctor-v2.log")"
pass "armin doctor accepts the v2 registration form"

# The dual entrypoint itself: v2 reads id + setup() from the default export.
# No v2 binary is published, so drive setup() with a stub context and assert
# the plugin registers its hooks and tools.
echo "── v2 plugin entrypoint (stub context)"
command -v bun >/dev/null || fail "bun not on PATH (needed for the v2 entrypoint check)"
FAKE_ENGINE="$WORK/v2-home/.local/bin/armin-engine"
mkdir -p "$(dirname "$FAKE_ENGINE")"
cat >"$FAKE_ENGINE" <<'EOF'
#!/usr/bin/env node
const http = require("http");
const args = process.argv.slice(2);
const port = Number(args[args.indexOf("--port") + 1]) || 0;
const srv = http.createServer((req, res) => {
    res.setHeader("content-type", "application/json");
    if (req.url.includes("/health")) res.end(JSON.stringify({ ok: true, extraction: "deterministic" }));
    else if (req.url.includes("/state/brief")) res.end(JSON.stringify({ brief: "", empty: true }));
    else res.end("{}");
});
srv.listen(port, "127.0.0.1", () => {
    process.stdout.write(`ARMIN_PORT=${srv.address().port}\n`);
});
process.on("SIGTERM", () => process.exit(0));
setInterval(() => {}, 1 << 30);
EOF
chmod +x "$FAKE_ENGINE"
ARMIN_ENGINE_BIN="$FAKE_ENGINE" \
ARMIN_DB_DIR="$WORK/v2-db" \
ARMIN_PLUGIN_ENTRY="file://$PKG_ROOT/plugins/armin.ts" \
    bun -e '
const m = await import(process.env.ARMIN_PLUGIN_ENTRY);
const plugin = m.default;
if (typeof plugin.id !== "string" || typeof plugin.setup !== "function")
    throw new Error("v2 entrypoint missing: default export needs id + setup()");
if (typeof plugin.server !== "function")
    throw new Error("v1 entrypoint missing: default export needs server()");
const hooks = [];
const tools = [];
const cleanup = await plugin.setup({
    options: { enabled: true },
    location: { directory: process.cwd(), project: { id: "e2e", directory: process.cwd(), canonical: process.cwd() } },
    event: { subscribe: () => ({ [Symbol.asyncIterator]: async function* () {} }) },
    session: { hook: async (name) => { hooks.push(name); return { dispose: async () => {} } } },
    tool: {
        hook: async (name) => { hooks.push("tool." + name); return { dispose: async () => {} } },
        transform: async (cb) => { cb({ add: (t) => tools.push(t), namespace() {}, update() {}, remove() {}, list: () => tools, get: () => undefined }); return { dispose: async () => {} } },
    },
});
for (const want of ["prompt", "context", "compaction", "tool.execute.after"])
    if (!hooks.includes(want)) throw new Error("v2 setup did not register hook: " + want);
if (tools.length !== 6) throw new Error("v2 setup registered " + tools.length + " tools, expected 6");
await cleanup();
console.log("v2 entrypoint ok");
' || fail "v2 entrypoint (id + setup) is broken"
pass "v2 entrypoint registers hooks + tools and cleans up"

# ── 4. armin doctor ──────────────────────────────────────────────────────────
echo "── armin doctor"
XDG_CONFIG_HOME="$WORK/xdg/config" \
XDG_DATA_HOME="$WORK/xdg/data" \
XDG_CACHE_HOME="$WORK/xdg/cache" \
HOME="$WORK/home" \
    "$PKG_ROOT/bin/armin.js" doctor >"$WORK/doctor.log" 2>&1 \
    || fail "armin doctor reported problems (see $WORK/doctor.log)"
grep -q "all checks passed" "$WORK/doctor.log" || fail "doctor did not pass: $(cat "$WORK/doctor.log")"
pass "armin doctor passes"

# ── 5. Real opencode session against the npm-installed layout ────────────────
echo "── opencode run (headless)"
PROJ="$WORK/proj"
mkdir -p "$PROJ"
git -C "$PROJ" init -q
git -C "$PROJ" commit -q --allow-empty -m "e2e baseline"

# The e2e version exists only as a local tarball — opencode's on-demand
# registry install (Npm.add) cannot resolve it, so pre-seed opencode's plugin
# cache with exactly what a registry install of the NEW package would produce
# (package + hoisted SDK). This mirrors the post-publish state, which is the
# thing under test; the registry install step itself is covered by step 2.
CACHE_PKG="$WORK/xdg/cache/opencode/packages/armin-opencode@$E2E_VERSION"
mkdir -p "$CACHE_PKG/node_modules/@opencode-ai"
cp -r "$PKG_ROOT" "$CACHE_PKG/node_modules/armin-opencode"
if [[ -d "$PREFIX/node_modules/@opencode-ai/plugin" ]]; then
    cp -r "$PREFIX/node_modules/@opencode-ai/plugin" "$CACHE_PKG/node_modules/@opencode-ai/plugin"
fi
# SDK's own runtime deps (zod) — present in the real install via npm.
if [[ -d "$PREFIX/node_modules/zod" ]]; then
    cp -r "$PREFIX/node_modules/zod" "$CACHE_PKG/node_modules/zod"
fi

# The engine binary is expected at ~/.local/bin by the plugin's lookup.
# The engine itself is not under test here: use a fake sidecar that does the
# real contract — "ARMIN_PORT=<n>" handshake on stdout + /api/v1/health —
# so the plugin reaches its full activation path.
mkdir -p "$WORK/home/.local/bin"
cat >"$WORK/home/.local/bin/armin-engine" <<'EOF'
#!/usr/bin/env node
const http = require("http");
const args = process.argv.slice(2);
const port = Number(args[args.indexOf("--port") + 1]) || 0;
const srv = http.createServer((req, res) => {
    if (req.url.includes("/health")) {
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify({ ok: true, extraction: "deterministic" }));
    } else { res.statusCode = 404; res.end("{}"); }
});
srv.listen(port, "127.0.0.1", () => {
    process.stdout.write(`ARMIN_PORT=${srv.address().port}\n`);
});
process.on("SIGTERM", () => process.exit(0));
setInterval(() => {}, 1 << 30);
EOF
chmod +x "$WORK/home/.local/bin/armin-engine"

# Pick a usable model (provider auth comes from the invoking user's env).
MODEL="$(cd "$PROJ" && XDG_CONFIG_HOME="$WORK/xdg/config" XDG_DATA_HOME="$WORK/xdg/data" \
    opencode models 2>/dev/null | head -1)"
[[ -n "$MODEL" ]] || fail "no models available (provider auth needed for this test)"

set +e
cd "$PROJ"
XDG_CONFIG_HOME="$WORK/xdg/config" \
XDG_DATA_HOME="$WORK/xdg/data" \
XDG_CACHE_HOME="$WORK/xdg/cache" \
HOME="$WORK/home" \
    timeout 180 opencode run --model "$MODEL" "Reply with exactly: armin-e2e-done" \
    >"$WORK/run.log" 2>&1
RC=$?
set -e

# Acceptance: the plugin must prove it loaded and activated. opencode does
# NOT log plugin load failures anywhere durable (transient TUI error only),
# so the plugin's own unconditional activation line is the only reliable
# signal — that is why it exists.
if grep -q "\[armin\] active" "$WORK/run.log"; then
    pass "plugin activated inside a real opencode session"
elif grep -q "\[armin\] disabled" "$WORK/run.log"; then
    fail "plugin loaded but reports disabled — options channel broken"
elif grep -qiE "failed to (load|install) plugin" "$WORK/run.log"; then
    fail "opencode reported a plugin load/install failure"
else
    sed -n '1,40p' "$WORK/run.log" >&2 || true
    fail "no [armin] activation/disabled line in opencode output — plugin silently inert (the original bug)"
fi

echo
echo "e2e: PASS — npm-installed plugin loads and activates in opencode"
if [[ "$KEEP" == "1" ]]; then
    echo "artifacts: $WORK"
fi
