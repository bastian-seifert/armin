/**
 * ARMIN — reasoning-graph middleware for opencode.
 *
 * Spawns the `armin-engine` Rust sidecar, passively captures the session's
 * tool calls and assistant/user prose into an argument graph, and pushes a
 * compact reasoning-state brief into the system prompt so decisions, open
 * questions, and contradictions survive context compaction.
 *
 * Opt-in: either set `ARMIN_ENABLED=1` (or `ARMIN_ENGINE_BIN`), or add an
 * `armin` section to any opencode config file (opencode.json / jsonc,
 * global or project):
 *
 *   {
 *     "$schema": "https://opencode.ai/config.json",
 *     "armin": {
 *       "enabled": true,
 *       "provider": "anthropic",          // anthropic | openai (default: infer from keys)
 *       "apiKey": "sk-...",               // sent to the sidecar only; env key wins if both set
 *       "typesafeKey": "...",             // Typesafe System One key (jev is the
 *                                         // default extraction mode); env wins
 *       "model": "session",               // "session" = follow the live session model
 *                                         // "small"  = reuse opencode's small_model setting
 *                                         // any string = fixed extraction model
 *       "batchMs": 15000,                 // LLM extraction debounce window
 *       "batchEvents": 10,                // events per LLM extraction call
 *       "dbDir": "~/.opencode/armin"      // per-project graph databases
 *                                         // (keyed by git origin remote; path
 *                                         // fallback for non-git dirs)
 *     }
 *   }
 *
 * Precedence: engine defaults < config files (global < project) < env vars
 * (ARMIN_MODEL, ARMIN_BATCH_MS, ARMIN_BATCH_EVENTS, ARMIN_DB_DIR,
 * ARMIN_ENGINE_BIN, ARMIN_DEBUG) — env is the escape hatch.
 *
 * Note: opencode's auth store (OAuth logins) is not exposed to plugins, so
 * extraction uses `apiKey`/environment API keys; the session *model* can
 * still be followed with "model": "session".
 */
import type { Plugin } from "@opencode-ai/plugin"
import { tool } from "@opencode-ai/plugin"

// ── Configuration ─────────────────────────────────────────────────────────────

const DEBUG = process.env.ARMIN_DEBUG === "1"
const MAX_RESTARTS = 3
const MAX_TEXT_CHARS = 2000

function log(...args: unknown[]) {
  if (DEBUG) console.log("[armin]", ...args)
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms))
}

function truncate(s: string, max: number): string {
  return s.length <= max ? s : `${s.slice(0, max)}…`
}

// ── Config files (JSON + JSONC, no deps) ──────────────────────────────────────

/** Parse JSON, falling back to a string-aware JSONC cleanup (comments,
 * trailing commas). Returns null when the file is unparseable. */
function parseConfigFile(text: string): Record<string, unknown> | null {
  try {
    return JSON.parse(text)
  } catch {
    // fall through to JSONC handling
  }
  let out = ""
  let inString = false
  let escape = false
  for (let i = 0; i < text.length; i++) {
    const c = text[i]
    if (inString) {
      out += c
      if (escape) escape = false
      else if (c === "\\") escape = true
      else if (c === '"') inString = false
      continue
    }
    if (c === '"') {
      inString = true
      out += c
      continue
    }
    if (c === "/" && text[i + 1] === "/") {
      while (i < text.length && text[i] !== "\n") i++
      out += "\n"
      continue
    }
    if (c === "/" && text[i + 1] === "*") {
      i += 2
      while (i < text.length && !(text[i] === "*" && text[i + 1] === "/")) i++
      i++
      continue
    }
    out += c
  }
  const noTrailing = out.replace(/,(\s*[}\]])/g, "$1")
  try {
    return JSON.parse(noTrailing)
  } catch {
    return null
  }
}

/** Read the merged `armin` section (plus the top-level `small_model` key)
 * from opencode config files. Later files win, mirroring opencode's own
 * global→project merge order. */
async function loadArminConfig(worktree: string): Promise<{
  armin: Record<string, unknown>
  smallModel?: string
}> {
  const home = process.env.HOME ?? "~"
  const candidates = [
    // global (same order as opencode itself: later files win)
    `${home}/.config/opencode/opencode.jsonc`,
    `${home}/.config/opencode/opencode.json`,
    `${home}/.config/opencode/config.json`,
    // project
    `${worktree}/opencode.jsonc`,
    `${worktree}/opencode.json`,
    `${worktree}/.opencode/opencode.jsonc`,
    `${worktree}/.opencode/opencode.json`,
  ]

  let armin: Record<string, unknown> = {}
  let smallModel: string | undefined
  for (const file of candidates) {
    try {
      const f = Bun.file(file)
      if (!(await f.exists())) continue
      const parsed = parseConfigFile(await f.text())
      if (!parsed) continue
      if (parsed.armin && typeof parsed.armin === "object") {
        armin = { ...armin, ...(parsed.armin as Record<string, unknown>) }
      }
      if (typeof parsed.small_model === "string") smallModel = parsed.small_model
    } catch (e) {
      log(`skipping unreadable config ${file}:`, String(e))
    }
  }
  return { armin, smallModel }
}

/** Slug for database file names. */
function slugify(s: string): string {
  return s.replace(/[^a-zA-Z0-9]+/g, "-").replace(/^-+|-+$/g, "")
}

/**
 * Project identity key: the git origin remote when the worktree is a clone
 * of a remote, else the worktree path itself. Sessions in different
 * checkouts/worktrees of the same project then share one graph — memory
 * follows the project, not the checkout location.
 */
function projectKeyFor(worktree: string): string {
  try {
    const proc = Bun.spawnSync(
      ["git", "-C", worktree, "config", "--get", "remote.origin.url"],
      { stdout: "pipe", stderr: "pipe" },
    )
    const url = proc.stdout.toString().trim()
    if (url) return url.replace(/\.git\/?$/i, "")
  } catch {
    // not a git repo or no origin — fall through to the worktree path
  }
  return worktree
}

/** One database per project: sessions in any checkout of the same remote
 * share the graph; non-git directories are keyed by their path. */
function dbPathFor(dbDir: string, worktree: string): string {
  const slug = slugify(projectKeyFor(worktree))
  return `${dbDir}/${slug || "default"}.db`
}

/** Pick the first engine binary that exists: env override → repo build → install path. */
async function resolveEngineBin(worktree: string): Promise<string> {
  const candidates = [
    process.env.ARMIN_ENGINE_BIN,
    `${worktree}/armin-core/target/release/armin-engine`,
    `${worktree}/../armin-core/target/release/armin-engine`,
    `${process.env.HOME}/.local/bin/armin-engine`,
  ].filter((c): c is string => !!c)
  for (const candidate of candidates) {
    try {
      if (await Bun.file(candidate).exists()) return candidate
    } catch {
      // unreadable path — try the next candidate
    }
  }
  return candidates[candidates.length - 1]
}

// ── Sidecar lifecycle ─────────────────────────────────────────────────────────

class Sidecar {
  private proc?: Bun.Subprocess<"ignore", "pipe", "pipe">
  private readonly _token = crypto.randomUUID().replace(/-/g, "")
  private starts = 0
  private disposed = false
  port: number | null = null

  get token(): string {
    return this._token
  }

  constructor(
    private engineBin: string,
    private dbFile: string,
    private spawnEnv: Record<string, string> = {},
  ) {}

  /** Start the engine and wait for the port handshake + health check. */
  async start(): Promise<boolean> {
    if (this.disposed) return false
    if (await this.healthy()) return true
    if (this.starts >= MAX_RESTARTS) {
      log("sidecar restart limit reached; ARMIN inert")
      return false
    }
    this.starts++

    const args = [
      this.engineBin,
      "--port", "0",
      "--db-path", this.dbFile,
      "--auth-token", this.token,
    ]

    log("spawning sidecar:", args.join(" "))
    let proc: Bun.Subprocess<"ignore", "pipe", "pipe">
    try {
      proc = Bun.spawn(args, {
        stdout: "pipe",
        stderr: "pipe",
        env: { ...process.env, ...this.spawnEnv },
      })
    } catch (e) {
      log("failed to spawn sidecar:", String(e))
      return false
    }
    this.proc = proc

    // Drain stderr so a chatty engine can't block on a full pipe.
    ;(async () => {
      try {
        for await (const line of proc.stderr) {
          if (DEBUG) log("engine:", String(line).trim())
        }
      } catch {
        // process exited
      }
    })()

    const port = await this.readPort(proc, 5_000)
    if (!port) {
      log("sidecar did not report ARMIN_PORT in time")
      proc.kill()
      return false
    }
    this.port = port

    for (let i = 0; i < 50; i++) {
      if (await this.healthy()) {
        log(`sidecar ready on 127.0.0.1:${port}`)
        return true
      }
      if (this.disposed) return false
      await sleep(200)
    }
    log("sidecar failed health check")
    return false
  }

  /** Ensure the sidecar is up, restarting if it crashed or hung. */
  async ensure(): Promise<boolean> {
    if (await this.healthy()) return true
    // Process still alive? It may just be busy — retry once before restarting.
    if (this.proc && this.proc.exitCode === null) {
      await sleep(300)
      if (await this.healthy()) return true
      this.proc.kill()
    }
    this.port = null
    return this.start()
  }

  async dispose(): Promise<void> {
    this.disposed = true
    if (this.proc && this.proc.exitCode === null) {
      this.proc.kill() // SIGTERM → graceful flush
      await sleep(500)
      if (this.proc.exitCode === null) this.proc.kill(9)
    }
  }

  private async readPort(
    proc: Bun.Subprocess<"ignore", "pipe", "pipe">,
    timeoutMs: number,
  ): Promise<number | null> {
    const reader = proc.stdout.getReader()
    const deadline = Date.now() + timeoutMs
    let buf = ""
    while (Date.now() < deadline) {
      let chunk: Awaited<ReturnType<typeof reader.read>> | null
      try {
        chunk = await Promise.race([
          reader.read(),
          sleep(timeoutMs).then(() => null),
        ])
      } catch {
        break
      }
      if (!chunk) break
      buf += new TextDecoder().decode(chunk.value ?? new Uint8Array())
      const m = buf.match(/ARMIN_PORT=(\d+)/)
      if (m) {
        reader.cancel().catch(() => {})
        return Number(m[1])
      }
    }
    reader.cancel().catch(() => {})
    return null
  }

  private async healthy(): Promise<boolean> {
    if (!this.port) return false
    try {
      const res = await fetch(`http://127.0.0.1:${this.port}/api/v1/health`, {
        headers: authHeaders(this.token),
        signal: AbortSignal.timeout(500),
      })
      return res.ok
    } catch {
      return false
    }
  }
}

// ── HTTP client ───────────────────────────────────────────────────────────────

function authHeaders(token: string): Record<string, string> {
  return { Authorization: `Bearer ${token}`, "Content-Type": "application/json" }
}

class EngineClient {
  constructor(private sidecar: Sidecar) {}

  get token(): string {
    return this.sidecar.token
  }

  async get<T = unknown>(path: string): Promise<T | null> {
    return this.request<T>("GET", path)
  }

  async post<T = unknown>(path: string, body: unknown): Promise<T | null> {
    return this.request<T>("POST", path, body)
  }

  private async request<T>(
    method: string,
    path: string,
    body?: unknown,
  ): Promise<T | null> {
    if (!(await this.sidecar.ensure())) return null
    try {
      const res = await fetch(
        `http://127.0.0.1:${this.sidecar.port}/api/v1${path}`,
        {
          method,
          headers: authHeaders(this.token),
          body: body === undefined ? undefined : JSON.stringify(body),
          signal: AbortSignal.timeout(10_000),
        },
      )
      if (!res.ok) {
        log(`${path} -> ${res.status}`)
        return null
      }
      return (await res.json()) as T
    } catch (e) {
      log(`${path} failed:`, String(e))
      return null
    }
  }
}

// ── Event capture ─────────────────────────────────────────────────────────────

class Capture {
  private seq = 0

  constructor(private api: EngineClient) {}

  /** Fire-and-forget: capture must never block the agent's turn. */
  ingest(
    sessionID: string,
    kind: "tool_call" | "utterance" | "user_prompt",
    role: string,
    text: string,
    opts?: { toolName?: string; files?: string[] },
  ): void {
    if (!text.trim()) return
    const now = Date.now() / 1000
    const event = {
      id: `${sessionID.slice(-8)}-${Date.now().toString(36)}-${this.seq++}`,
      session_id: sessionID,
      agent_role: role,
      start_time: now,
      end_time: now,
      text: truncate(text, MAX_TEXT_CHARS),
      event_kind: kind,
      tool_name: opts?.toolName ?? null,
      files: opts?.files ?? [],
      commit: null,
    }
    void this.api.post("/ingest", [event])
  }
}

// ── Plugin ────────────────────────────────────────────────────────────────────

/** Pull file paths out of common tool-arg shapes (read/edit/write...). */
function extractFiles(args: unknown): string[] {
  const out: string[] = []
  if (args && typeof args === "object") {
    const obj = args as Record<string, unknown>
    for (const key of ["filePath", "path", "file"]) {
      const v = obj[key]
      if (typeof v === "string" && v.includes("/")) out.push(v)
    }
    if (Array.isArray(obj.paths)) {
      for (const v of obj.paths) {
        if (typeof v === "string" && v.includes("/")) out.push(v)
      }
    }
  }
  return [...new Set(out)].slice(0, 8)
}

const ArminPlugin: Plugin = async (ctx) => {
  // Config precedence: engine defaults < config files < env vars.
  const fileConfig = await loadArminConfig(ctx.worktree)
  const armin = fileConfig.armin

  const enabledViaConfig = armin.enabled === true
  const enabledViaEnv = process.env.ARMIN_ENABLED === "1" || !!process.env.ARMIN_ENGINE_BIN
  if (!enabledViaConfig && !enabledViaEnv) {
    log("disabled (set armin.enabled=true in config or ARMIN_ENABLED=1)")
    return {}
  }

  const cfgStr = (key: string, envKey: string): string | undefined =>
    (process.env[envKey] as string | undefined) ??
    (typeof armin[key] === "string" ? (armin[key] as string) : undefined)

  const dbDir = cfgStr("dbDir", "ARMIN_DB_DIR") ?? `${process.env.HOME}/.opencode/armin`
  const engineBin = await resolveEngineBin(ctx.worktree)
  const dbFile = dbPathFor(dbDir, ctx.worktree)
  try {
    await import("node:fs").then((fs) => fs.mkdirSync(dbDir, { recursive: true }))
  } catch {
    // Engine also creates parent dirs; this is just a head start.
  }

  // ── Model & provider resolution ─────────────────────────────────────
  // - "session" → follow the live session model (pushed at runtime via
  //   /config once the first chat message arrives)
  // - "small"   → reuse opencode's top-level small_model setting
  // - other     → fixed model string
  // - unset     → LLM_MODEL env or the provider's default
  const rawModel = cfgStr("model", "ARMIN_MODEL")
  let model: string | undefined
  let followSessionModel = false
  if (rawModel === "session") {
    followSessionModel = true
  } else if (rawModel === "small") {
    const small = fileConfig.smallModel
    if (small) model = small.includes("/") ? small.split("/").slice(1).join("/") : small
  } else if (rawModel) {
    model = rawModel
  }

  const spawnEnv: Record<string, string> = {}
  if (model) spawnEnv.LLM_MODEL = model
  const provider = armin.provider
  if (provider === "anthropic" || provider === "openai") {
    spawnEnv.LLM_PROVIDER = provider
  }
  // Typesafe key (jev is the default extraction mode): config value is
  // passed to the sidecar only; a key already in the environment wins.
  const typesafeKey = cfgStr("typesafeKey", "TYPESAFE_AI_API_KEY")
  if (typesafeKey) spawnEnv.TYPESAFE_AI_API_KEY = typesafeKey
  // API key: config value is passed to the sidecar only; a key already in
  // the environment wins (no override).
  if (typeof armin.apiKey === "string" && armin.apiKey) {
    const keyProvider = provider === "openai" ? "openai" : "anthropic"
    const keyVar = keyProvider === "openai" ? "OPENAI_API_KEY" : "ANTHROPIC_API_KEY"
    if (!process.env[keyVar]) spawnEnv[keyVar] = armin.apiKey as string
  }
  // Batch knobs: env wins, config fills in.
  const batchMs =
    process.env.ARMIN_BATCH_MS ?? (typeof armin.batchMs === "number" ? String(armin.batchMs) : undefined)
  if (batchMs) spawnEnv.ARMIN_BATCH_MS = batchMs
  const batchEvents =
    process.env.ARMIN_BATCH_EVENTS ??
    (typeof armin.batchEvents === "number" ? String(armin.batchEvents) : undefined)
  if (batchEvents) spawnEnv.ARMIN_BATCH_EVENTS = batchEvents

  const sidecar = new Sidecar(engineBin, dbFile, spawnEnv)
  const started = await sidecar.start()
  if (!started) {
    log("sidecar unavailable — plugin inert")
    return { dispose: () => sidecar.dispose() }
  }

  const api = new EngineClient(sidecar)
  const capture = new Capture(api)

  if (sidecar.port) {
    log(`reasoning-state UI: http://127.0.0.1:${sidecar.port}/ui`)
  }
  // Surface the effective extraction mode once — this is the difference
  // between "memory works" and a silent deterministic downgrade.
  void (async () => {
    try {
      const health = await api.get<{ extraction: string }>("/health")
      const mode = health?.extraction ?? "unknown"
      if (mode === "jev") {
        log(`extraction: jev (TypeSafe System One) — ready`)
      } else if (mode === "llm") {
        log(
          `extraction: llm (fallback) — jev is the default but no Typesafe key is ` +
            `configured. Set TYPESAFE_AI_API_KEY in your environment or ` +
            `"armin": { "typesafeKey": "..." } in your opencode config.`,
        )
      } else {
        log(
          `extraction: deterministic-only — no API keys found. Import and ` +
            `unverified-edit warnings work; prose extraction does not. Set ` +
            `TYPESAFE_AI_API_KEY (or ANTHROPIC/OPENAI_API_KEY) to enable it.`,
        )
      }
    } catch {
      // best-effort status only
    }
  })()

  // ── Cold start: import AGENTS.md / CLAUDE.md into an empty graph ──────
  // Deterministic parse on the engine side (content-hash IDs, idempotent).
  // No LLM, no network beyond the local sidecar.
  void (async () => {
    try {
      const snap = await api.get<{ nodes: unknown[] }>("/snapshot")
      if (!snap || (snap.nodes?.length ?? 0) > 0) return
      const fs = await import("node:fs")
      const doc = ["AGENTS.md", "CLAUDE.md"]
        .map((f) => `${ctx.worktree}/${f}`)
        .find((p) => fs.existsSync(p))
      if (!doc) return
      const content = fs.readFileSync(doc, "utf-8")
      const res = await api.post<{ found: number; imported: number; rules: number }>(
        "/import",
        { content, session_id: `import-${Date.now()}` },
      )
      if (res && res.imported > 0) {
        log(
          `imported ${doc}: ${res.imported} node(s) ` +
            `(${res.rules} rule(s), ${res.found - res.rules} decision/open item(s))`,
        )
      }
    } catch {
      // Import is best-effort — never block the session.
    }
  })()

  // Last seen text per streaming part ID — capture each text part once.
  const lastPartText = new Map<string, string>()
  // Recently edited files (ring buffer) — scopes the brief's "Binding here"
  // section to what the agent is actually working on.
  const recentFiles: string[] = []
  const rememberFiles = (files: string[]) => {
    for (const f of files) {
      const i = recentFiles.indexOf(f)
      if (i >= 0) recentFiles.splice(i, 1)
      recentFiles.unshift(f)
    }
    if (recentFiles.length > 20) recentFiles.length = 20
  }
  const briefFooter = (process.env.ARMIN_BRIEF_FOOTER ?? cfgStr("briefFooter", "ARMIN_BRIEF_FOOTER")) === "1"
  // Last model pushed to the engine (for "session" model following).
  let lastPushedModel: string | undefined
  const ingestAssistantText = (sessionID: string, partID: string, text: string) => {
    if (lastPartText.get(partID) === text) return
    lastPartText.set(partID, text)
    if (lastPartText.size > 500) {
      const oldest = lastPartText.keys().next().value
      if (oldest) lastPartText.delete(oldest)
    }
    capture.ingest(sessionID, "utterance", "assistant", text)
  }

  return {
    dispose: () => sidecar.dispose(),

    // ── Passive capture (no LLM, no added latency) ─────────────────────
    "tool.execute.after": async (input, output) => {
      const files = extractFiles(input.args)
      rememberFiles(files)
      const text = `${input.tool} ${output.title ?? ""}: ${truncate(output.output ?? "", 400)}`
      capture.ingest(input.sessionID, "tool_call", "agent", text, {
        toolName: input.tool,
        files,
      })
      // Optional (off by default): append the scoped memory as a footer to
      // mutating tool outputs — the tool result is model context at exactly
      // the moment a remembered constraint matters.
      if (briefFooter && files.length > 0) {
        const filesParam = encodeURIComponent(recentFiles.slice(0, 10).join(","))
        const res = await api.get<{ brief: string; empty: boolean }>(
          `/state/brief?files=${filesParam}`,
        )
        if (res && !res.empty && res.brief.includes("Binding here")) {
          const binding = res.brief
            .split("Binding here")[1]
            ?.split("\n")
            .filter((l) => l.startsWith("- "))
            .slice(0, 3)
            .join("\n")
          if (binding) {
            output.output = `${output.output ?? ""}\n\n[ARMIN — settled decisions/rules for these files]\n${binding}`
          }
        }
      }
    },

    event: async ({ event }) => {
      if (event.type === "message.part.updated") {
        const props = event.properties as Record<string, any>
        const part = props?.part
        if (part?.type === "text" && part?.time?.end && props?.sessionID) {
          ingestAssistantText(props.sessionID, part.id, part.text ?? "")
        }
      }
    },

    "chat.message": async (input, output) => {
      // Follow the live session model if configured: push it to the engine
      // whenever it changes (cheap localhost POST, fire-and-forget).
      if (followSessionModel && input.model?.modelID) {
        const modelID = input.model.modelID
        if (modelID && modelID !== lastPushedModel) {
          lastPushedModel = modelID
          void api.post("/config", { model: modelID })
        }
      }
      const texts = (output.parts ?? [])
        .filter((p: any) => p.type === "text")
        .map((p: any) => p.text ?? "")
        .join("\n")
      if (texts.trim()) {
        capture.ingest(input.sessionID, "user_prompt", "user", texts)
      }
    },

    // ── Push: zero-cost reasoning-state reminder on every turn ────────
    "experimental.chat.system.transform": async (_input, output) => {
      try {
        const filesParam =
          recentFiles.length > 0
            ? `?files=${encodeURIComponent(recentFiles.slice(0, 10).join(","))}`
            : ""
        const res = await api.get<{ brief: string; empty: boolean }>(
          `/state/brief${filesParam}`,
        )
        if (res && !res.empty && res.brief) {
          output.system.push(
            "Reasoning state (ARMIN): tracked decisions, rules, and open items. " +
              "Use query_graph for detail. If a new request conflicts with a decision or rule " +
              "below, say so explicitly before deviating.\n" + res.brief,
          )
        }
      } catch {
        // Engine down — skip injection silently.
      }
    },

    // ── Reasoning survives compaction ─────────────────────────────────
    "experimental.session.compacting": async (_input, output) => {
      const [decisions, debt] = await Promise.all([
        api.get<any[]>("/decisions"),
        api.get<{ items: { debt_type: string; description: string }[] }>("/debt"),
      ])
      const lines: string[] = []
      if (Array.isArray(decisions) && decisions.length) {
        lines.push("Decisions made in this session (do not re-litigate):")
        for (const d of decisions.slice(0, 8)) {
          lines.push(`- [${d.status}] ${d.label}`)
        }
      }
      const questions = (debt?.items ?? []).filter((i) =>
        String(i.debt_type).includes("Question"),
      )
      if (questions.length) {
        lines.push("Open questions that remain unresolved:")
        for (const q of questions.slice(0, 8)) {
          lines.push(`- ${q.description}`)
        }
      }
      if (lines.length) output.context.push(lines.join("\n"))
    },

    // ── Tools ──────────────────────────────────────────────────────────
    tool: {
      query_graph: tool({
        description:
          "Ask a question about this session's reasoning graph (decisions, evidence, assumptions, contradictions). Returns an answer with a trace of node IDs usable in resolve_question.",
        args: {
          question: tool.schema.string().describe("The question to answer from the reasoning graph"),
        },
        async execute(args) {
          const res = await api.post<{ answer: string; trace: string[]; cited_events: string[] }>(
            "/query",
            { question: args.question },
          )
          if (!res) return "Reasoning graph is unavailable."
          if (!res.answer && !res.trace?.length)
            return "The graph has no relevant reasoning recorded yet."
          return [
            res.answer,
            "",
            `Trace (node IDs): ${JSON.stringify(res.trace)}`,
            `Cited events: ${JSON.stringify(res.cited_events)}`,
          ].join("\n")
        },
      }),

      record_decision: tool({
        description:
          "Record a decision you made, with its rationale. Optionally pass question node IDs (from query_graph trace) that this decision resolves.",
        args: {
          label: tool.schema.string().describe("Short summary, max ~15 words"),
          description: tool.schema.string().describe("Detailed rationale"),
          resolves: tool.schema.array(tool.schema.string()).optional().describe("Question node IDs this decision resolves"),
          resolutions: tool.schema.array(tool.schema.string()).optional().describe("Reasoning per resolution"),
          files: tool.schema.array(tool.schema.string()).optional().describe("Related file paths"),
        },
        async execute(args, context) {
          const res = await api.post<{ node_id: string; edge_ids: string[] }>("/agent/decision", {
            label: args.label,
            description: args.description,
            session_id: context.sessionID,
            resolves: args.resolves ?? [],
            resolutions: args.resolutions ?? [],
            files: args.files ?? [],
          })
          return res
            ? `Recorded decision ${res.node_id}${res.edge_ids.length ? ` (resolved ${res.edge_ids.length} question(s))` : ""}`
            : "Reasoning graph is unavailable; decision not recorded."
        },
      }),

      raise_question: tool({
        description:
          "Record an unresolved question so it is tracked as reasoning debt and resurfaces in later turns and after compaction.",
        args: {
          label: tool.schema.string().describe("Short summary of the question, max ~15 words"),
          description: tool.schema.string().describe("Full question and context"),
          files: tool.schema.array(tool.schema.string()).optional().describe("Related file paths"),
        },
        async execute(args, context) {
          const res = await api.post<{ node_id: string }>("/agent/question", {
            label: args.label,
            description: args.description,
            session_id: context.sessionID,
            files: args.files ?? [],
          })
          return res
            ? `Recorded question ${res.node_id}`
            : "Reasoning graph is unavailable; question not recorded."
        },
      }),

      resolve_question: tool({
        description:
          "Mark a tracked question as resolved by linking it to a node (use node IDs from query_graph's trace).",
        args: {
          question_id: tool.schema.string(),
          resolver_node_id: tool.schema.string(),
          reasoning: tool.schema.string().describe("How the resolver answers the question"),
        },
        async execute(args) {
          const res = await api.post("/resolve", args)
          return res ? `Resolved ${args.question_id}` : "Reasoning graph is unavailable."
        },
      }),

      invalidate_assumption: tool({
        description:
          "Mark a previously recorded assumption as no longer valid (e.g. a constraint turned out to be false).",
        args: {
          node_id: tool.schema.string(),
          rationale: tool.schema.string().describe("Why the assumption no longer holds"),
        },
        async execute(args) {
          const res = await api.post("/invalidate", args)
          return res ? `Invalidated ${args.node_id}` : "Reasoning graph is unavailable."
        },
      }),

      get_status: tool({
        description:
          "Get the session's reasoning status: decisions with validation state, reasoning debt (unresolved questions, contradictions, unsupported claims), and top risks.",
        args: {},
        async execute() {
          const [decisions, debt, risks] = await Promise.all([
            api.get<{ label: string; status: string }[]>("/decisions"),
            api.get<{ items: { debt_type: string; description: string }[]; total_score: number }>("/debt"),
            api.get<{ label: string; impact_score: number }[]>("/risks"),
          ])
          if (!decisions && !debt) return "Reasoning graph is unavailable."
          const parts: string[] = []
          if (Array.isArray(decisions) && decisions.length) {
            parts.push(
              "## Decisions\n" +
                decisions.slice(0, 10).map((d) => `- [${d.status}] ${d.label}`).join("\n"),
            )
          }
          if (debt?.items?.length) {
            parts.push(
              `## Reasoning debt (score ${debt.total_score})\n` +
                debt.items
                  .slice(0, 10)
                  .map((i) => `- ${i.debt_type}: ${i.description}`)
                  .join("\n"),
            )
          }
          if (Array.isArray(risks) && risks.length) {
            parts.push(
              "## Risks\n" +
                risks
                  .slice(0, 5)
                  .map((r) => `- (${r.impact_score?.toFixed?.(2) ?? "?"}) ${r.label}`)
                  .join("\n"),
            )
          }
          return parts.join("\n\n") || "No reasoning recorded yet."
        },
      }),
    },
  }
}

export const ArminPluginExport = ArminPlugin
export default ArminPlugin
