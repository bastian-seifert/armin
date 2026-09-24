#!/usr/bin/env node
/**
 * armin — installer for the ARMIN opencode middleware.
 *
 *   armin install      download the armin-engine binary for this platform,
 *                      register the opencode plugin, print next steps
 *   armin doctor       verify the plugin is registered, loadable, and the
 *                      engine binary is in place
 *   armin --version    print the package version
 *
 * The engine binary is fetched from the matching GitHub release
 * (bastian-seifert/armin). If the download fails (offline, unsupported
 * platform), fall back to building from source with cargo.
 */
"use strict";

const { execFileSync } = require("child_process");
const fs = require("fs");
const https = require("https");
const os = require("os");
const path = require("path");
const { fileURLToPath } = require("url");
const zlib = require("zlib");

const PKG_ROOT = path.join(__dirname, "..");
const PKG = require(path.join(PKG_ROOT, "package.json"));
const REPO = "bastian-seifert/armin";
const BIN_DIR = process.env.ARMIN_BIN_DIR || path.join(os.homedir(), ".local", "bin");
const ENGINE = path.join(BIN_DIR, "armin-engine");

function platformAsset() {
  const a = process.arch, p = process.platform;
  if (p === "linux" && a === "x64") return "armin-engine-linux-x64.tar.gz";
  if (p === "darwin" && a === "x64") return "armin-engine-macos-x64.tar.gz";
  if (p === "darwin" && a === "arm64") return "armin-engine-macos-arm64.tar.gz";
  return null;
}

function download(url, redirects = 0) {
  return new Promise((resolve, reject) => {
    if (redirects > 5) return reject(new Error("too many redirects"));
    https.get(url, { headers: { "User-Agent": "armin-installer" } }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume();
        return resolve(download(res.headers.location, redirects + 1));
      }
      if (res.statusCode !== 200) {
        res.resume();
        return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
      }
      resolve(res);
    }).on("error", reject);
  });
}

async function fetchEngine(version) {
  const asset = platformAsset();
  if (!asset) throw new Error(`no prebuilt engine for ${process.platform}-${process.arch}`);
  const url = `https://github.com/${REPO}/releases/download/v${version}/${asset}`;
  process.stdout.write(`downloading ${asset} (v${version}) ...\n`);
  const res = await download(url);
  const chunks = [];
  for await (const chunk of res) chunks.push(chunk);
  return Buffer.concat(chunks);
}

function extractTarGz(buf, dest) {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "armin-pkg-"));
  const tgz = path.join(tmp, "pkg.tar.gz");
  fs.writeFileSync(tgz, buf);
  execFileSync("tar", ["-xzf", tgz, "-C", tmp]);
  // archive layout: <archive-name>/armin-engine
  const inner = fs.readdirSync(tmp).find((f) => f !== "pkg.tar.gz");
  const binary = path.join(tmp, inner || ".", "armin-engine");
  fs.mkdirSync(BIN_DIR, { recursive: true });
  fs.copyFileSync(binary, ENGINE);
  fs.chmodSync(ENGINE, 0o755);
  fs.rmSync(tmp, { recursive: true, force: true });
}

function buildFromSource() {
  const repoRoot = process.env.ARMIN_SOURCE_DIR;
  if (!repoRoot || !fs.existsSync(path.join(repoRoot, "armin-core"))) {
    return false;
  }
  process.stdout.write("building armin-engine from source ...\n");
  execFileSync("cargo", ["build", "--release", "-p", "armin-engine"], {
    cwd: path.join(repoRoot, "armin-core"),
    stdio: "inherit",
  });
  fs.mkdirSync(BIN_DIR, { recursive: true });
  fs.copyFileSync(
    path.join(repoRoot, "armin-core", "target", "release", "armin-engine"),
    ENGINE,
  );
  fs.chmodSync(ENGINE, 0o755);
  return true;
}

let pendingTypesafeKey = null;
let pendingOpenrouterKey = null;
let pendingJevProvider = null; // "openrouter" | "skip" | null (typesafe needs no config entry)
let pendingPort = null; // fixed engine port, or null for OS auto-assign

/** Ask which relay serves Jev and for the matching key (TTY only; values
 * go into the opencode config — the plugin passes them to the sidecar).
 * A provider detected from the environment skips the prompt entirely. */
async function askForExtractionSetup() {
  if (!process.stdin.isTTY) return;
  let provider;
  if (process.env.ARMIN_JEV_PROVIDER) provider = process.env.ARMIN_JEV_PROVIDER;
  else if (process.env.TYPESAFE_AI_API_KEY) provider = "typesafe";
  else if (process.env.OPENROUTER_API_KEY) provider = "openrouter";
  if (provider) return;

  const readline = require("readline");
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
  const ask = (q) => new Promise((resolve) => rl.question(q, resolve));
  try {
    console.log("\nExtraction backend (jev is the default — verbatim nodes, no generative LLM):");
    console.log("  1) TypeSafe System One (typesafe.ai)  [default]");
    console.log("  2) Jev via OpenRouter (openrouter.ai — same API, OpenRouter billing)");
    console.log("  3) skip (LLM fallback / deterministic-only)");
    const choice = ((await ask("Choose [1/2/3, Enter=1]: ")) || "1").trim();
    if (choice === "3") { pendingJevProvider = "skip"; return; }
    if (choice === "2") {
      pendingJevProvider = "openrouter";
      const key = ((await ask("OpenRouter API key (openrouter.ai/settings/keys) — paste, or Enter to skip: ")) || "").trim();
      if (key) pendingOpenrouterKey = key;
    } else {
      const key = ((await ask("TypeSafe System One API key — paste, or Enter to skip: ")) || "").trim();
      if (key) pendingTypesafeKey = key;
    }
  } finally {
    rl.close();
  }
}

/** Ask for an optional fixed engine port (TTY only; ARMIN_PORT in the
 * environment is the runtime escape hatch and skips the prompt). A pinned
 * port is written to the opencode config; the sidecar auto-falls back to a
 * free port when it is busy. */
async function askForPort() {
  if (!process.stdin.isTTY) return;
  if (process.env.ARMIN_PORT) return;
  const readline = require("readline");
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
  const ask = (q) => new Promise((resolve) => rl.question(q, resolve));
  try {
    const answer = ((await ask("\nFixed engine port (Enter = auto-assign per session): ")) || "").trim();
    if (!answer) return;
    const port = Number(answer);
    if (Number.isInteger(port) && port >= 1024 && port <= 65535) {
      pendingPort = port;
    } else {
      console.log(`invalid port "${answer}" (need 1024-65535) — engine port: auto-assign`);
    }
  } finally {
    rl.close();
  }
}

const ARMIN_SPEC = `armin-opencode@${PKG.version}`;

/** Installed opencode generation: 2 writes the v2 config schema, 1 the v1
 * tuple form. Detection is best-effort — the v1 tuple still loads under v2
 * (opencode normalizes "plugin" into "plugins"), so an undetectable version
 * keeps writing the long-standing v1 form. ARMIN_OPENCODE_MAJOR overrides
 * detection (used by the e2e test). */
function detectOpencodeMajor() {
  const override = process.env.ARMIN_OPENCODE_MAJOR;
  if (override && /^\d+$/.test(override)) return Number(override);
  try {
    const out = execFileSync("opencode", ["--version"], { stdio: "pipe", timeout: 5000 }).toString();
    const match = out.match(/(\d+)\./);
    if (match) return Number(match[1]);
  } catch {
    // opencode not on PATH — assume the form that works everywhere
  }
  return null;
}

/** True for any config entry referring to the armin plugin: v1 tuples and
 * strings, v2 { package, options } objects, and file:// paths. */
function isArminPluginEntry(item) {
  if (typeof item === "string") {
    return (
      /^armin-opencode(@|$)/.test(item) ||
      (item.startsWith("file://") && /\/plugins\/armin\.ts$/.test(item))
    );
  }
  if (Array.isArray(item)) {
    return typeof item[0] === "string" && /^armin-opencode(@|$)/.test(item[0]);
  }
  return (
    !!item &&
    typeof item === "object" &&
    typeof item.package === "string" &&
    /^armin-opencode(@|$)/.test(item.package)
  );
}

/** The npm spec a config entry points at (v1 tuple, v2 object, or string). */
function pluginEntrySpec(item) {
  if (typeof item === "string") return item;
  if (Array.isArray(item)) return typeof item[0] === "string" ? item[0] : "";
  if (item && typeof item === "object" && typeof item.package === "string") return item.package;
  return "";
}

/** Where opencode installed a plugin package, when it is on disk. v1 keeps
 *   <cache>/opencode/packages/<spec>/node_modules/<name>
 * v2 keeps a hashed, generation-numbered npm cache:
 *   <cache>/npm/<key>/<generation>/node_modules/<name>
 * Returns { dir, layout } or null. */
function findInstalledPackage(name, spec) {
  const cacheRoot = process.env.XDG_CACHE_HOME || path.join(os.homedir(), ".cache");
  const v1Dir = path.join(cacheRoot, "opencode", "packages", spec, "node_modules", name);
  if (fs.existsSync(v1Dir)) return { dir: v1Dir, layout: "v1" };
  const npmCache = path.join(cacheRoot, "npm");
  let best = null;
  for (const key of readDirSafe(npmCache)) {
    for (const generation of readDirSafe(path.join(npmCache, key))) {
      const dir = path.join(npmCache, key, generation, "node_modules", name);
      if (!fs.existsSync(dir)) continue;
      if (!best || Number(generation) >= Number(best.generation)) best = { dir, generation };
    }
  }
  return best ? { dir: best.dir, layout: "v2" } : null;
}

function readDirSafe(dir) {
  try {
    return fs.readdirSync(dir);
  } catch {
    return [];
  }
}

function opencodeConfigPath() {
  const cfgDir = path.join(
    process.env.XDG_CONFIG_HOME || path.join(os.homedir(), ".config"),
    "opencode",
  );
  const jsonc = path.join(cfgDir, "opencode.jsonc");
  const json = path.join(cfgDir, "opencode.json");
  return { cfgDir, cfgPath: fs.existsSync(jsonc) ? jsonc : json };
}

/** Parse the global opencode config (JSON, with a line-comment cleanup for
 * JSONC). Throws on unparseable input. */
function readOpencodeConfig(cfgPath) {
  if (!fs.existsSync(cfgPath)) return {};
  const text = fs.readFileSync(cfgPath, "utf-8");
  return JSON.parse(text.replace(/^\s*\/\/.*$/gm, ""));
}

/** Register the plugin as an npm entry, in the schema the installed opencode
 * generation reads:
 *
 *   v1: "plugin":  [["armin-opencode@<version>", { "enabled": true, ... }]]
 *   v2: "plugins": [{ "package": "armin-opencode@<version>",
 *                     "options": { "enabled": true, ... } }]
 *
 * opencode installs the pinned package — together with its declared
 * @opencode-ai/plugin dependency — into its own cache. The file:// form used
 * by older installers cannot resolve the plugin SDK from a project's
 * node_modules and loads nothing. Options are also the schema-safe channel:
 * opencode strips unknown top-level config keys (the legacy "armin" section
 * survives only as raw-file re-reads), while options travel straight to the
 * plugin factory. */
function registerPlugin() {
  const major = detectOpencodeMajor();
  const v2 = major !== null && major >= 2;
  const key = v2 ? "plugins" : "plugin";
  const otherKey = v2 ? "plugin" : "plugins";
  const { cfgDir, cfgPath } = opencodeConfigPath();
  let data;
  try {
    data = readOpencodeConfig(cfgPath);
  } catch {
    const form = v2
      ? `"plugins": [{ "package": "${ARMIN_SPEC}", "options": { "enabled": true } }]`
      : `"plugin": [["${ARMIN_SPEC}", { "enabled": true }]]`;
    console.log(`\nCould not parse ${cfgPath} — set up manually:\n  add ${form} to your opencode config\n`);
    return false;
  }

  // Options: legacy "armin" keys migrate in; prompted values win.
  const options = {
    ...(data.armin && typeof data.armin === "object" ? data.armin : {}),
    enabled: true,
  };
  if (pendingTypesafeKey && !process.env.TYPESAFE_AI_API_KEY) {
    options.typesafeKey = pendingTypesafeKey;
  }
  if (pendingOpenrouterKey && !process.env.OPENROUTER_API_KEY) {
    options.openrouterKey = pendingOpenrouterKey;
  }
  if (pendingJevProvider === "openrouter") options.jevProvider = "openrouter";
  if (pendingPort) options.port = pendingPort;

  const entry = v2 ? { package: ARMIN_SPEC, options } : [ARMIN_SPEC, options];
  const plugins = Array.isArray(data[key]) ? data[key] : [];
  const next = [];
  let placed = false;
  for (const item of plugins) {
    if (isArminPluginEntry(item)) {
      // One pinned entry replaces any previous armin entry: the legacy
      // file:// forms cannot resolve the SDK, and duplicates would
      // double-activate the plugin.
      if (!placed) next.push(entry);
      placed = true;
    } else {
      next.push(item);
    }
  }
  if (!placed) next.push(entry);

  // A stale armin entry under the other generation's key would double-activate
  // (v2 concatenates normalized "plugin" entries with "plugins").
  const otherBefore = Array.isArray(data[otherKey]) ? data[otherKey] : [];
  const otherAfter = otherBefore.filter((item) => !isArminPluginEntry(item));

  const changed =
    JSON.stringify(next) !== JSON.stringify(plugins) ||
    JSON.stringify(otherAfter) !== JSON.stringify(otherBefore) ||
    data.armin !== undefined;
  if (changed) {
    data[key] = next;
    if (otherAfter.length > 0) data[otherKey] = otherAfter;
    else delete data[otherKey];
    delete data.armin; // migrated into the entry options above
    fs.mkdirSync(cfgDir, { recursive: true });
    fs.writeFileSync(cfgPath, JSON.stringify(data, null, 2));
  }
  console.log(
    v2
      ? `plugin registered in ${cfgPath}: "plugins": [{ "package": "${ARMIN_SPEC}", "options": { "enabled": true, ... } }]`
      : `plugin registered in ${cfgPath}: "plugin": [["${ARMIN_SPEC}", { "enabled": true, ... }]]`,
  );
  return true;
}

/** Post-install verification. "Installed but inert" is otherwise
 * undetectable: opencode surfaces plugin load failures only as a transient
 * TUI error event and never logs them. Checks the registration form, the
 * engine binary, opencode's plugin cache, SDK resolution, and — when bun is
 * available — a real module import of the installed plugin. */
async function doctor() {
  const problems = [];
  const notes = [];
  const ok = (msg) => console.log(`  ok   ${msg}`);
  const warn = (msg) => {
    notes.push(msg);
    console.log(`  warn ${msg}`);
  };
  const fail = (msg) => {
    problems.push(msg);
    console.log(`  FAIL ${msg}`);
  };

  // 1. registration form (v1 "plugin" tuple, v2 "plugins" object, or string)
  const { cfgPath } = opencodeConfigPath();
  let data = {};
  try {
    data = readOpencodeConfig(cfgPath);
  } catch (e) {
    fail(`cannot parse ${cfgPath}: ${e.message}`);
  }
  const entries = [...(Array.isArray(data.plugin) ? data.plugin : []), ...(Array.isArray(data.plugins) ? data.plugins : [])].filter(
    isArminPluginEntry,
  );
  if (entries.length === 0) {
    fail(`plugin not registered in ${cfgPath} — run "armin install"`);
  } else if (entries.length > 1) {
    warn(`plugin registered ${entries.length}x in ${cfgPath} — stale entries should be removed`);
  }

  let pkgDir = null; // installed plugin package dir, when locatable
  let cacheLayout = null;
  if (entries.length > 0) {
    const spec = pluginEntrySpec(entries[0]);
    const form = Array.isArray(entries[0]) || typeof entries[0] === "string" ? "v1" : "v2";
    if (spec.startsWith("file://")) {
      const p = fileURLToPath(spec);
      if (p.includes(`${path.sep}node_modules${path.sep}`)) {
        fail(
          `file:// registration points into node_modules (${p}) — this form cannot resolve ` +
            `@opencode-ai/plugin and loads nothing; re-run "armin install"`,
        );
      } else {
        warn(
          `file:// registration (${p}) — dev/source install; only loads next to a checkout ` +
            `with .opencode/node_modules. The npm registration form is recommended.`,
        );
      }
    } else {
      ok(`plugin registered as npm spec "${spec}" (${form} config form)`);
      // Locate opencode's on-demand install (layout differs per generation)
      const name = spec.split("@")[0];
      const installed = findInstalledPackage(name, spec);
      pkgDir = installed?.dir ?? null;
      cacheLayout = installed?.layout ?? null;
      if (!pkgDir) {
        const cacheRoot = process.env.XDG_CACHE_HOME || path.join(os.homedir(), ".cache");
        warn(
          `opencode has not installed "${spec}" into its cache yet (looked in ` +
            `${cacheRoot}/opencode/packages and ${cacheRoot}/npm) — fetched automatically ` +
            `on first opencode start`,
        );
      } else {
        ok(`opencode cache present: ${pkgDir}`);
        const sdkCandidates = [
          path.join(pkgDir, "node_modules", "@opencode-ai", "plugin"),
          path.join(pkgDir, "..", "..", "node_modules", "@opencode-ai", "plugin"),
        ];
        if (sdkCandidates.some((p) => fs.existsSync(p))) {
          ok(`@opencode-ai/plugin resolves inside the cache`);
        } else if (cacheLayout === "v1") {
          fail(`@opencode-ai/plugin not resolvable inside the cache — opencode v1 sessions will fail to load the plugin`);
        } else {
          // v2 imports the v1 SDK lazily and never needs it for the v2 path.
          warn(`@opencode-ai/plugin not installed next to the package — opencode v1 sessions cannot load the plugin from this install (v2 is unaffected)`);
        }
      }
    }
  }

  // 2. engine binary (same lookup order as the plugin)
  const engineCandidates = [
    process.env.ARMIN_ENGINE_BIN,
    process.env.ARMIN_BIN_DIR && path.join(process.env.ARMIN_BIN_DIR, "armin-engine"),
    path.join(process.cwd(), "armin-core", "target", "release", "armin-engine"),
    path.join(os.homedir(), ".local", "bin", "armin-engine"),
  ].filter(Boolean);
  const engine = engineCandidates.find((p) => {
    try {
      fs.accessSync(p, fs.constants.X_OK);
      return true;
    } catch {
      return false;
    }
  });
  if (engine) ok(`engine binary: ${engine}`);
  else fail(`no executable armin-engine found (looked in: ${engineCandidates.join(", ")}) — run "armin install"`);

  // 3. module import smoke test (opencode loads plugins under bun)
  if (fs.existsSync(pkgDir ?? "")) {
    const entry = path.join(pkgDir, "plugins", "armin.ts");
    if (!fs.existsSync(entry)) {
      fail(`plugin entry missing: ${entry} (package.json "main" must point at plugins/armin.ts)`);
    } else {
      let bun = null;
      try {
        execFileSync("bun", ["--version"], { stdio: "pipe" });
        bun = "bun";
      } catch {
        // bun not on PATH
      }
      if (!bun) {
        warn(`bun not found — skipping module import check (opencode loads plugins with its bundled bun)`);
      } else {
        try {
          execFileSync(
            bun,
            [
              "-e",
              // The dual entrypoint: V1 calls server(), V2 reads id + setup().
              `const m = await import(process.env.ARMIN_PLUGIN_ENTRY); ` +
                `const d = m.default; ` +
                `if (!d || typeof d.server !== "function") throw new Error("default export has no server() (v1)"); ` +
                `if (typeof d.setup !== "function" || typeof d.id !== "string") throw new Error("default export has no v2 id + setup()");`,
            ],
            {
              env: { ...process.env, ARMIN_PLUGIN_ENTRY: entry },
              stdio: "pipe",
            },
          );
          ok(`plugin module imports cleanly and exposes both v1 + v2 entrypoints (${entry})`);
        } catch (e) {
          const detail = (e.stderr && e.stderr.toString().trim()) || e.message;
          fail(`plugin module failed to import: ${detail}`);
        }
      }
    }
  }

  if (problems.length > 0) {
    console.log(`\n${problems.length} problem(s) found${notes.length ? ` (and ${notes.length} warning(s))` : ""}`);
    process.exitCode = 1;
  } else {
    console.log(`\nall checks passed${notes.length ? ` (${notes.length} warning(s))` : ""}`);
  }
}

async function install() {
  if (fs.existsSync(ENGINE)) {
    console.log(`engine already present: ${ENGINE} (delete it to re-install)`);
  } else {
    try {
      const buf = await fetchEngine(PKG.version);
      extractTarGz(buf, BIN_DIR);
      console.log(`installed: ${ENGINE}`);
    } catch (e) {
      console.log(`download failed: ${e.message}`);
      if (!buildFromSource()) {
        console.log(
          `\nFallback: build from source —\n  git clone https://github.com/${REPO}.git\n  cd armin && scripts/install.sh\n`,
        );
        process.exitCode = 1;
        return;
      }
    }
  }
  await askForExtractionSetup();
  await askForPort();
  const registered = registerPlugin();
  const tsKey = process.env.TYPESAFE_AI_API_KEY || pendingTypesafeKey;
  const orKey = process.env.OPENROUTER_API_KEY || pendingOpenrouterKey;
  const llmKey = process.env.ANTHROPIC_API_KEY || process.env.OPENAI_API_KEY;
  if (tsKey) {
    console.log("extraction: jev (TypeSafe System One) — ready");
  } else if (orKey) {
    console.log("extraction: jev via OpenRouter — ready");
  } else if (llmKey) {
    console.log(
      "extraction: LLM fallback — jev is the default but no key for it is set.\n" +
        "  Set TYPESAFE_AI_API_KEY (typesafe.ai) or OPENROUTER_API_KEY (openrouter.ai),\n" +
        '  or put "armin": { "typesafeKey": "..." } / { "jevProvider": "openrouter",\n' +
        '  "openrouterKey": "..." } in your opencode config.',
    );
  } else {
    console.log(
      "extraction: NONE (deterministic-only) — import, capture and unverified-edit\n" +
        "  warnings work; prose extraction does not.\n" +
        "  Set TYPESAFE_AI_API_KEY (typesafe.ai) or OPENROUTER_API_KEY (openrouter.ai)\n" +
        "  for jev extraction — the default — or ANTHROPIC_API_KEY / OPENAI_API_KEY\n" +
        "  for LLM fallback.",
    );
  }
  const v2 = (() => {
    const major = detectOpencodeMajor();
    return major !== null && major >= 2;
  })();
  const registration = v2
    ? `"plugins": [{ "package": "${ARMIN_SPEC}", "options": { "enabled": true, ... } }]`
    : `"plugin": [["${ARMIN_SPEC}", { "enabled": true, ... }]]`;
  console.log(`
── done ────────────────────────────────────────────────────────
${registered
    ? `ARMIN is registered in your opencode config:\n  ${registration}\nRestart opencode to activate it; run "armin doctor" to verify, or remove\nthe entry to turn it off.`
    : "ARMIN is not enabled yet — follow the manual setup steps above."}

Optional overrides (env wins over config):
    ARMIN_ENABLED=1                force-enable without the config entry
    ARMIN_PORT=<port>              fixed engine port (default: OS auto-assign;
                                   a busy pinned port auto-falls back)
    ARMIN_EXTRACTION_MODE=jev      System One extraction (default; needs
                                   TYPESAFE_AI_API_KEY or OPENROUTER_API_KEY)
    ARMIN_JEV_PROVIDER=openrouter  route jev via OpenRouter (auto-detected
                                   when only that key is set)
    ARMIN_JEV_MODEL=<model>        jev model override (default jev-1.13.0,
                                   or jev-latest via OpenRouter)
    ARMIN_MODEL=<model>            extraction model override
    ARMIN_DEBUG=1                  verbose logging + /ui URL
`);
}

const arg = process.argv[2];
if (arg === "--version" || arg === "-v") {
  console.log(`armin-installer ${PKG.version}`);
} else if (arg === "doctor") {
  doctor().catch((e) => {
    console.error(e);
    process.exitCode = 1;
  });
} else if (arg && arg !== "install") {
  console.log("usage: armin [install|doctor|--version]");
  process.exitCode = arg === "help" ? 0 : 1;
} else {
  install().catch((e) => {
    console.error(e);
    process.exitCode = 1;
  });
}
