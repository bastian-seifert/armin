#!/usr/bin/env node
/**
 * armin — installer for the ARMIN opencode middleware.
 *
 *   armin install      download the armin-engine binary for this platform,
 *                      register the opencode plugin, print next steps
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

function registerPlugin() {
  const plugin = path.join(PKG_ROOT, "plugins", "armin.ts");
  if (!fs.existsSync(plugin)) throw new Error(`plugin not found at ${plugin}`);
  const cfgDir = path.join(
    process.env.XDG_CONFIG_HOME || path.join(os.homedir(), ".config"),
    "opencode",
  );
  const cfgPath = fs.existsSync(path.join(cfgDir, "opencode.jsonc"))
    ? path.join(cfgDir, "opencode.jsonc")
    : path.join(cfgDir, "opencode.json");
  let data = {};
  if (fs.existsSync(cfgPath)) {
    const text = fs.readFileSync(cfgPath, "utf-8");
    try {
      data = JSON.parse(text.replace(/^\s*\/\/.*$/gm, ""));
    } catch {
      console.log(`\nCould not parse ${cfgPath} — set up manually:\n  add "plugin": ["file://${plugin}"] and "armin": { "enabled": true } to your opencode config\n`);
      return false;
    }
  }
  const entry = "file://" + plugin;
  let changed = false;
  const plugins = Array.isArray(data.plugin) ? data.plugin : [];
  if (!plugins.includes(entry)) {
    plugins.push(entry);
    data.plugin = plugins;
    changed = true;
  }
  // `armin install` is an explicit opt-in — enable in config so no
  // per-shell env var is needed (ARMIN_ENABLED stays as escape hatch).
  if (data.armin?.enabled !== true) {
    data.armin = { ...(data.armin || {}), enabled: true };
    changed = true;
  }
  if (pendingTypesafeKey && !process.env.TYPESAFE_AI_API_KEY) {
    data.armin = { ...(data.armin || {}), typesafeKey: pendingTypesafeKey };
    changed = true;
  }
  if (pendingOpenrouterKey && !process.env.OPENROUTER_API_KEY) {
    data.armin = { ...(data.armin || {}), openrouterKey: pendingOpenrouterKey };
    changed = true;
  }
  // Persist the relay choice only when the user explicitly picked
  // OpenRouter in the prompt (env-detected providers drive the sidecar
  // via their env vars; writing nothing avoids overriding that later).
  if (pendingJevProvider === "openrouter" && data.armin.jevProvider !== "openrouter") {
    data.armin = { ...(data.armin || {}), jevProvider: "openrouter" };
    changed = true;
  }
  if (pendingPort && data.armin.port !== pendingPort) {
    data.armin = { ...(data.armin || {}), port: pendingPort };
    changed = true;
  }
  if (changed) {
    fs.mkdirSync(cfgDir, { recursive: true });
    fs.writeFileSync(cfgPath, JSON.stringify(data, null, 2));
  }
  console.log(`plugin registered in ${cfgPath}`);
  return true;
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
  console.log(`
── done ────────────────────────────────────────────────────────
${registered
    ? 'ARMIN is enabled in your opencode config ("armin": { "enabled": true }).\nRestart opencode to activate it; remove "enabled" to turn it off.'
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
} else if (arg && arg !== "install") {
  console.log("usage: armin [install|--version]");
  process.exitCode = arg === "help" ? 0 : 1;
} else {
  install().catch((e) => {
    console.error(e);
    process.exitCode = 1;
  });
}
