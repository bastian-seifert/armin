/**
 * ARMIN — reasoning-graph middleware for opencode (v1 and v2).
 *
 * Spawns the `armin-engine` Rust sidecar, passively captures the session's
 * tool calls and assistant/user prose into an argument graph, and pushes a
 * compact reasoning-state brief into the system prompt so decisions, open
 * questions, and contradictions survive context compaction.
 *
 * One entrypoint serves both opencode generations. V2 reads `id` + `setup()`
 * from the default export; V1 (1.18.29+) reads `server()` from that same
 * object, and older V1 releases call the exported plugin function.
 *
 * Opt-in: either set `ARMIN_ENABLED=1` (or `ARMIN_ENGINE_BIN`), or register
 * the plugin with options — the schema-safe config channel, in both versions:
 *
 *   // opencode v1
 *   { "plugin": [["armin-opencode@<version>", { ...options }]] }
 *
 *   // opencode v2
 *   { "plugins": [{ "package": "armin-opencode@<version>", "options": { ...options } }] }
 *
 *   // options (identical in both)
 *   {
 *     "enabled": true,
 *     "provider": "anthropic",          // anthropic | openai (default: infer from keys)
 *     "apiKey": "sk-...",               // sent to the sidecar only; env key wins if both set
 *     "typesafeKey": "...",             // Typesafe System One key (jev is the
 *                                         // default extraction mode); env wins
 *     "jevProvider": "typesafe",        // typesafe | openrouter — which relay serves
 *                                         // the System One API (openrouter bills
 *                                         // your OpenRouter account); env wins
 *     "openrouterKey": "sk-or-...",     // OpenRouter key, used when jevProvider is
 *                                         // "openrouter"; env wins
 *     "model": "session",               // "session" = follow the live session model
 *                                         // "small"  = reuse opencode's small_model setting
 *                                         // any string = fixed extraction model
 *     "batchMs": 15000,                 // LLM extraction debounce window
 *     "batchEvents": 10,                // events per LLM extraction call
 *     "dbDir": "~/.opencode/armin"      // per-project graph databases
 *                                         // (keyed by git origin remote; path
 *                                         // fallback for non-git dirs)
 *     "port": 4545,                     // optional fixed engine port (default:
 *                                         // OS auto-assign; auto-falls back when
 *                                         // the port is busy); env wins
 *   }
 *
 * Legacy fallback: an `armin` section in any opencode config file
 * (opencode.json / jsonc, global or project). Note opencode strips unknown
 * top-level keys before plugins see the resolved config, so the section is
 * re-read from the raw files — prefer the tuple options above.
 *
 * Precedence: engine defaults < config files (global < project) < plugin
 * options < env vars (ARMIN_MODEL, ARMIN_BATCH_MS, ARMIN_BATCH_EVENTS,
 * ARMIN_DB_DIR, ARMIN_PORT, ARMIN_ENGINE_BIN, ARMIN_DEBUG) — env is the
 * escape hatch.
 *
 * Note: opencode's auth store (OAuth logins) is not exposed to plugins, so
 * extraction uses `apiKey`/environment API keys; the session *model* can
 * still be followed with "model": "session".
 */
import { homedir } from "node:os"
// Type-only imports: the SDKs are compile-time contracts. Neither SDK is
// imported at module load — the V1 SDK is pulled in lazily by the V1 factory,
// so a V2 host never touches the V1 package (and vice versa).
import type { Plugin as PluginV1 } from "@opencode-ai/plugin"
import type { Plugin as PluginV2 } from "@opencode/plugin"

// ── Configuration ─────────────────────────────────────────────────────────────

const DEBUG = process.env.ARMIN_DEBUG === "1"
const MAX_RESTARTS = 3
const MAX_TEXT_CHARS = 2000
/** HOME with a real fallback: an unset HOME must not leak literal "~" into paths. */
const HOME = process.env.HOME || homedir()

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
  const home = HOME
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
    `${HOME}/.local/bin/armin-engine`,
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
  /** Set after a pinned port failed to start; later starts auto-assign. */
  private pinnedPortFailed = false
  port: number | null = null

  get token(): string {
    return this._token
  }

  constructor(
    private engineBin: string,
    private dbFile: string,
    private spawnEnv: Record<string, string> = {},
    private pinnedPort?: number,
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

    // With a pinned port (armin.port / ARMIN_PORT) the engine binds that
    // exact port; once it has failed once, later starts auto-assign again.
    const usePinned = this.pinnedPort !== undefined && !this.pinnedPortFailed
    const args = [
      this.engineBin,
      "--port", usePinned ? String(this.pinnedPort) : "0",
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
      proc.kill()
      if (usePinned) {
        // The engine exited without a handshake — the pinned port is
        // almost certainly busy. Fall back to OS auto-assign and retry.
        log(`sidecar could not bind pinned port ${this.pinnedPort}; falling back to auto-assign`)
        this.pinnedPortFailed = true
        return this.start()
      }
      log("sidecar did not report ARMIN_PORT in time")
      return false
    }
    this.port = port

    for (let i = 0; i < 50; i++) {
      if (await this.healthy()) {
        log(`sidecar ready on 127.0.0.1:${port}${usePinned ? " (pinned)" : ""}`)
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
        // Race against the remaining deadline (not the full timeout again):
        // keeps the total wait bounded by timeoutMs even with slow chunks.
        chunk = await Promise.race([
          reader.read(),
          sleep(Math.max(0, deadline - Date.now())).then(() => null),
        ])
      } catch {
        break
      }
      if (!chunk) break
      if (chunk.done) break // engine exited (e.g. bind failure) — stop waiting
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

/** Extract the "Binding here" section from a reasoning-state brief for the
 * optional tool-output footer. The engine renders the brief as
 * `<reasoning-state ...>` with sections like "Heading (N):\n- item", and
 * "Binding here" is NOT the last section — parsing must stop at the next
 * section heading or it would swallow unrelated debt bullets. Returns null
 * when the section is absent or has no items. */
function bindingsFooter(brief: string): string | null {
  const start = brief.search(/^Binding here\b/m)
  if (start === -1) return null
  const body = brief.slice(start).split("\n")
  body.shift() // the "Binding here (N):" heading itself
  const lines: string[] = []
  for (const line of body) {
    if (line.startsWith("- ")) {
      lines.push(line)
      if (lines.length >= 3) break
    } else if (line.trim() !== "") {
      break // next section heading (e.g. "Unverified edits (1):")
    }
  }
  return lines.length > 0 ? lines.join("\n") : null
}

// ── Shared tool operations ───────────────────────────────────────────────────
// One implementation per tool, wrapped by the v1 (zod `tool()`) and v2 (JSON
// schema + ctx.tool.transform) surfaces, so the two generations cannot drift.

type RecordDecisionInput = {
  label: string
  description: string
  resolves?: string[]
  resolutions?: string[]
  files?: string[]
}
type RaiseQuestionInput = { label: string; description: string; files?: string[] }
type ResolveQuestionInput = { question_id: string; resolver_node_id: string; reasoning: string }
type InvalidateAssumptionInput = { node_id: string; rationale: string }

const TOOL_DESCRIPTION = {
  queryGraph:
    "Ask a question about this session's reasoning graph (decisions, evidence, assumptions, contradictions). Returns an answer with a trace of node IDs usable in resolve_question.",
  recordDecision:
    "Record a decision you made, with its rationale. Optionally pass question node IDs (from query_graph trace) that this decision resolves.",
  raiseQuestion:
    "Record an unresolved question so it is tracked as reasoning debt and resurfaces in later turns and after compaction.",
  resolveQuestion:
    "Mark a tracked question as resolved by linking it to a node (use node IDs from query_graph's trace).",
  invalidateAssumption:
    "Mark a previously recorded assumption as no longer valid (e.g. a constraint turned out to be false).",
  getStatus:
    "Get the session's reasoning status: decisions with validation state, reasoning debt (unresolved questions, contradictions, unsupported claims), and top risks.",
} as const

async function queryGraph(api: EngineClient, question: string): Promise<string> {
  const res = await api.post<{ answer: string; trace: string[]; cited_events: string[] }>("/query", {
    question,
  })
  if (!res) return "Reasoning graph is unavailable."
  if (!res.answer && !res.trace?.length) return "The graph has no relevant reasoning recorded yet."
  return [
    res.answer,
    "",
    `Trace (node IDs): ${JSON.stringify(res.trace)}`,
    `Cited events: ${JSON.stringify(res.cited_events)}`,
  ].join("\n")
}

async function recordDecision(
  api: EngineClient,
  args: RecordDecisionInput,
  sessionID: string,
): Promise<string> {
  const res = await api.post<{ node_id: string; edge_ids: number[] }>("/agent/decision", {
    label: args.label,
    description: args.description,
    session_id: sessionID,
    resolves: args.resolves ?? [],
    resolutions: args.resolutions ?? [],
    files: args.files ?? [],
  })
  return res
    ? `Recorded decision ${res.node_id}${res.edge_ids?.length ? ` (resolved ${res.edge_ids.length} question(s))` : ""}`
    : "Reasoning graph is unavailable; decision not recorded."
}

async function raiseQuestion(
  api: EngineClient,
  args: RaiseQuestionInput,
  sessionID: string,
): Promise<string> {
  const res = await api.post<{ node_id: string }>("/agent/question", {
    label: args.label,
    description: args.description,
    session_id: sessionID,
    files: args.files ?? [],
  })
  return res ? `Recorded question ${res.node_id}` : "Reasoning graph is unavailable; question not recorded."
}

async function resolveQuestion(api: EngineClient, args: ResolveQuestionInput): Promise<string> {
  const res = await api.post("/resolve", args)
  return res ? `Resolved ${args.question_id}` : "Reasoning graph is unavailable."
}

async function invalidateAssumption(
  api: EngineClient,
  args: InvalidateAssumptionInput,
): Promise<string> {
  const res = await api.post("/invalidate", args)
  return res ? `Invalidated ${args.node_id}` : "Reasoning graph is unavailable."
}

async function reasoningStatus(api: EngineClient): Promise<string> {
  const [decisions, debt, risks] = await Promise.all([
    api.get<{ label: string; status: string }[]>("/decisions"),
    api.get<{ items: { debt_type: string; description: string }[]; total_score: number }>("/debt"),
    api.get<{ label: string; impact_score: number }[]>("/risks"),
  ])
  if (!decisions && !debt) return "Reasoning graph is unavailable."
  const parts: string[] = []
  if (Array.isArray(decisions) && decisions.length) {
    parts.push(
      "## Decisions\n" + decisions.slice(0, 10).map((d) => `- [${d.status}] ${d.label}`).join("\n"),
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
}

/** Decisions + open questions re-stated in a compaction request so the
 * reasoning survives the summary. */
async function compactionContext(api: EngineClient): Promise<string> {
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
  const questions = (debt?.items ?? []).filter((i) => String(i.debt_type).includes("Question"))
  if (questions.length) {
    lines.push("Open questions that remain unresolved:")
    for (const q of questions.slice(0, 8)) {
      lines.push(`- ${q.description}`)
    }
  }
  return lines.join("\n")
}

// ── Shared startup (v1 + v2) ─────────────────────────────────────────────────

type ArminRuntime = {
  api: EngineClient
  capture: Capture
  sidecar: Sidecar
  briefFooter: boolean
  followSessionModel: boolean
  rememberFiles: (files: string[]) => void
  pushSessionModel: (model: string | undefined) => void
  brief: () => Promise<{ brief: string; empty: boolean } | null>
  ingestAssistantText: (sessionID: string, partID: string, text: string) => void
}

type ArminStartup =
  | { status: "disabled" }
  | { status: "inert"; dispose: () => Promise<void> }
  | ({ status: "active" } & ArminRuntime)

/** Resolve config, start the sidecar, and wire the shared capture/brief state.
 * Both the v1 factory and the v2 setup() start here. */
async function startArmin(
  opts: Record<string, unknown>,
  worktree: string,
  enableHint: string,
): Promise<ArminStartup> {
  // Config precedence: engine defaults < config files < plugin options < env
  // vars. The registration options ("plugin": [["armin-opencode@x", {...}]] in
  // v1, "plugins": [{ "package": ..., "options": {...} }] in v2) are the
  // schema-safe channel; the raw-file "armin" section is the legacy fallback
  // (opencode strips unknown top-level keys from the resolved config, so the
  // section is re-read from the files directly).
  const fileConfig = await loadArminConfig(worktree)
  const fileArmin = fileConfig.armin
  const armin: Record<string, unknown> = { ...fileArmin, ...opts }

  const enabledViaConfig = armin.enabled === true
  const enabledViaEnv = process.env.ARMIN_ENABLED === "1" || !!process.env.ARMIN_ENGINE_BIN
  if (!enabledViaConfig && !enabledViaEnv) {
    console.log(`[armin] disabled — enable with ${enableHint} or ARMIN_ENABLED=1`)
    return { status: "disabled" }
  }

  const cfgStr = (key: string, envKey: string): string | undefined =>
    (process.env[envKey] as string | undefined) ??
    (typeof armin[key] === "string" ? (armin[key] as string) : undefined)

  const dbDir = cfgStr("dbDir", "ARMIN_DB_DIR") ?? `${HOME}/.opencode/armin`
  const engineBin = await resolveEngineBin(worktree)
  const dbFile = dbPathFor(dbDir, worktree)
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
  // Jev relay: "typesafe" (api.typesafe.ai, default) or "openrouter"
  // (openrouter.ai/api — same System One API, OpenRouter billing).
  const jevProvider = cfgStr("jevProvider", "ARMIN_JEV_PROVIDER")
  if (jevProvider) spawnEnv.ARMIN_JEV_PROVIDER = jevProvider
  const openrouterKey = cfgStr("openrouterKey", "OPENROUTER_API_KEY")
  if (openrouterKey) spawnEnv.OPENROUTER_API_KEY = openrouterKey
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

  // Optional fixed engine port: ARMIN_PORT env > armin.port config. Invalid
  // values (non-integer, 0, out of range) are ignored → OS auto-assign; a
  // busy pinned port falls back to auto-assign when the sidecar starts.
  const portEnv = Number(process.env.ARMIN_PORT)
  const portCfg = typeof armin.port === "number" ? armin.port : Number.NaN
  const pinnedPort =
    Number.isInteger(portEnv) && portEnv >= 1 && portEnv <= 65535
      ? portEnv
      : Number.isInteger(portCfg) && portCfg >= 1 && portCfg <= 65535
        ? portCfg
        : undefined
  if (pinnedPort) log("fixed engine port:", pinnedPort)

  const sidecar = new Sidecar(engineBin, dbFile, spawnEnv, pinnedPort)
  const started = await sidecar.start()
  if (!started) {
    // Unconditional: opencode never logs plugin load failures, so a silent
    // inert plugin is otherwise indistinguishable from a working one.
    console.log(
      `[armin] INERT — engine did not start (binary: ${engineBin}). ` +
        `Run "armin doctor" to diagnose; ARMIN_DEBUG=1 for verbose logs.`,
    )
    return { status: "inert", dispose: () => sidecar.dispose() }
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
      const health = await api.get<{ extraction: string; jev_provider?: string }>("/health")
      const mode = health?.extraction ?? "unknown"
      if (mode === "jev") {
        const via = health?.jev_provider ?? "typesafe"
        console.log(`[armin] active — extraction: jev (System One via ${via})`)
      } else if (mode === "llm") {
        console.log(
          `[armin] active — extraction: llm (fallback). jev is the default but no key is ` +
            `configured: set TYPESAFE_AI_API_KEY (typesafe.ai) or OPENROUTER_API_KEY ` +
            `(openrouter.ai), or "typesafeKey" / "jevProvider"+"openrouterKey" plugin options.`,
        )
      } else {
        console.log(
          `[armin] active — extraction: deterministic-only (no API keys). Import and ` +
            `unverified-edit warnings work; prose extraction does not. Set ` +
            `TYPESAFE_AI_API_KEY or OPENROUTER_API_KEY (or ANTHROPIC/OPENAI_API_KEY) to enable it.`,
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
        .map((f) => `${worktree}/${f}`)
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
  const pushSessionModel = (model: string | undefined) => {
    if (!model || model === lastPushedModel) return
    lastPushedModel = model
    void api.post("/config", { model })
  }
  /** Scoped reasoning-state brief for what the agent is touching right now. */
  const brief = async () => {
    const filesParam =
      recentFiles.length > 0 ? `?files=${encodeURIComponent(recentFiles.slice(0, 10).join(","))}` : ""
    return api.get<{ brief: string; empty: boolean }>(`/state/brief${filesParam}`)
  }

  return {
    status: "active",
    api,
    capture,
    sidecar,
    briefFooter,
    followSessionModel,
    rememberFiles,
    pushSessionModel,
    brief,
    ingestAssistantText,
  }
}

// ── Plugin: opencode v1 ──────────────────────────────────────────────────────

const ArminPlugin: PluginV1 = async (ctx, options) => {
  // V1-only runtime dependency. Loaded here, not at module scope, so the V2
  // host (which never installs @opencode-ai/plugin) can load this file.
  const { tool } = await import("@opencode-ai/plugin")
  const startup = await startArmin(
    (options ?? {}) as Record<string, unknown>,
    ctx.worktree,
    `"plugin": [["armin-opencode", { "enabled": true }]]`,
  )
  if (startup.status === "disabled") return {}
  if (startup.status === "inert") return { dispose: () => startup.dispose() }

  const { api, capture, sidecar, briefFooter, followSessionModel } = startup
  const { rememberFiles, pushSessionModel, brief, ingestAssistantText } = startup

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
        const res = await brief()
        const binding = res && !res.empty ? bindingsFooter(res.brief) : null
        if (binding) {
          output.output = `${output.output ?? ""}\n\n[ARMIN — settled decisions/rules for these files]\n${binding}`
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
      if (followSessionModel) pushSessionModel(input.model?.modelID)
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
        const res = await brief()
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
      const summary = await compactionContext(api)
      if (summary) output.context.push(summary)
    },

    // ── Tools ──────────────────────────────────────────────────────────
    tool: {
      query_graph: tool({
        description: TOOL_DESCRIPTION.queryGraph,
        args: {
          question: tool.schema.string().describe("The question to answer from the reasoning graph"),
        },
        async execute(args) {
          return queryGraph(api, args.question)
        },
      }),

      record_decision: tool({
        description: TOOL_DESCRIPTION.recordDecision,
        args: {
          label: tool.schema.string().describe("Short summary, max ~15 words"),
          description: tool.schema.string().describe("Detailed rationale"),
          resolves: tool.schema.array(tool.schema.string()).optional().describe("Question node IDs this decision resolves"),
          resolutions: tool.schema.array(tool.schema.string()).optional().describe("Reasoning per resolution"),
          files: tool.schema.array(tool.schema.string()).optional().describe("Related file paths"),
        },
        async execute(args, context) {
          return recordDecision(api, args, context.sessionID)
        },
      }),

      raise_question: tool({
        description: TOOL_DESCRIPTION.raiseQuestion,
        args: {
          label: tool.schema.string().describe("Short summary of the question, max ~15 words"),
          description: tool.schema.string().describe("Full question and context"),
          files: tool.schema.array(tool.schema.string()).optional().describe("Related file paths"),
        },
        async execute(args, context) {
          return raiseQuestion(api, args, context.sessionID)
        },
      }),

      resolve_question: tool({
        description: TOOL_DESCRIPTION.resolveQuestion,
        args: {
          question_id: tool.schema.string(),
          resolver_node_id: tool.schema.string(),
          reasoning: tool.schema.string().describe("How the resolver answers the question"),
        },
        async execute(args) {
          return resolveQuestion(api, args)
        },
      }),

      invalidate_assumption: tool({
        description: TOOL_DESCRIPTION.invalidateAssumption,
        args: {
          node_id: tool.schema.string(),
          rationale: tool.schema.string().describe("Why the assumption no longer holds"),
        },
        async execute(args) {
          return invalidateAssumption(api, args)
        },
      }),

      get_status: tool({
        description: TOOL_DESCRIPTION.getStatus,
        args: {},
        async execute() {
          return reasoningStatus(api)
        },
      }),
    },
  }
}

// ── Plugin: opencode v2 ──────────────────────────────────────────────────────

type ToolResultLike = { readonly output?: unknown; readonly content?: unknown }

/** Flatten a V2 tool result to text for the capture stream. */
function resultText(result: ToolResultLike): string {
  if (typeof result.output === "string") return result.output
  if (typeof result.content === "string") return result.content
  if (Array.isArray(result.content)) {
    return result.content
      .map((part) =>
        part && typeof part === "object" && (part as { type?: string }).type === "text"
          ? String((part as { text?: unknown }).text ?? "")
          : "",
      )
      .filter(Boolean)
      .join("\n")
  }
  if (result.output === undefined) return ""
  try {
    return JSON.stringify(result.output) ?? ""
  } catch {
    return ""
  }
}

/** Append the scoped-memory footer without discarding the result's shape. */
function withFooter(result: ToolResultLike, text: string): ToolResultLike {
  if (typeof result.output === "string") return { ...result, output: result.output + text }
  if (typeof result.content === "string") return { ...result, content: result.content + text }
  if (Array.isArray(result.content)) return { ...result, content: [...result.content, { type: "text", text }] }
  return { ...result, content: text }
}

/** V2 tool registry entries (JSON Schema inputs instead of v1 zod args). */
function v2Tools(api: EngineClient) {
  const object = (properties: Record<string, unknown>, required: string[] = []) => ({
    type: "object" as const,
    properties,
    ...(required.length > 0 ? { required } : {}),
    additionalProperties: false,
  })
  const stringList = (description: string) => ({
    type: "array" as const,
    items: { type: "string" as const },
    description,
  })
  const text = (content: string) => ({ content })
  return [
    {
      name: "query_graph",
      description: TOOL_DESCRIPTION.queryGraph,
      input: object(
        { question: { type: "string", description: "The question to answer from the reasoning graph" } },
        ["question"],
      ),
      async execute(input: unknown) {
        return text(await queryGraph(api, (input as { question: string }).question))
      },
    },
    {
      name: "record_decision",
      description: TOOL_DESCRIPTION.recordDecision,
      input: object(
        {
          label: { type: "string", description: "Short summary, max ~15 words" },
          description: { type: "string", description: "Detailed rationale" },
          resolves: stringList("Question node IDs this decision resolves"),
          resolutions: stringList("Reasoning per resolution"),
          files: stringList("Related file paths"),
        },
        ["label", "description"],
      ),
      async execute(input: unknown, context: { sessionID: string }) {
        return text(await recordDecision(api, input as RecordDecisionInput, context.sessionID))
      },
    },
    {
      name: "raise_question",
      description: TOOL_DESCRIPTION.raiseQuestion,
      input: object(
        {
          label: { type: "string", description: "Short summary of the question, max ~15 words" },
          description: { type: "string", description: "Full question and context" },
          files: stringList("Related file paths"),
        },
        ["label", "description"],
      ),
      async execute(input: unknown, context: { sessionID: string }) {
        return text(await raiseQuestion(api, input as RaiseQuestionInput, context.sessionID))
      },
    },
    {
      name: "resolve_question",
      description: TOOL_DESCRIPTION.resolveQuestion,
      input: object({
        question_id: { type: "string" },
        resolver_node_id: { type: "string" },
        reasoning: { type: "string", description: "How the resolver answers the question" },
      }),
      async execute(input: unknown) {
        return text(await resolveQuestion(api, input as ResolveQuestionInput))
      },
    },
    {
      name: "invalidate_assumption",
      description: TOOL_DESCRIPTION.invalidateAssumption,
      input: object({
        node_id: { type: "string" },
        rationale: { type: "string", description: "Why the assumption no longer holds" },
      }),
      async execute(input: unknown) {
        return text(await invalidateAssumption(api, input as InvalidateAssumptionInput))
      },
    },
    {
      name: "get_status",
      description: TOOL_DESCRIPTION.getStatus,
      input: object({}),
      async execute() {
        return text(await reasoningStatus(api))
      },
    },
  ]
}

const ArminPluginV2: PluginV2.Plugin = {
  id: "armin",
  async setup(ctx: PluginV2.Context) {
    const startup = await startArmin(
      (ctx.options ?? {}) as Record<string, unknown>,
      ctx.location.directory,
      `"plugins": [{ "package": "armin-opencode", "options": { "enabled": true } }]`,
    )
    if (startup.status === "disabled") return
    if (startup.status === "inert") return startup.dispose

    const { api, capture, sidecar, briefFooter, followSessionModel } = startup
    const { rememberFiles, pushSessionModel, brief, ingestAssistantText } = startup

    // Assistant prose: V2 has no message.part stream hook, so completed text
    // blocks are taken from the server event stream ("session.text.ended").
    const controller = new AbortController()
    let payloadWarned = false // unexpected shape: warn once, then stay quiet
    void (async () => {
      try {
        for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
          if (event.type !== "session.text.ended") continue
          const data = event.data as {
            sessionID?: string
            assistantMessageID?: string
            ordinal?: number
            text?: string
          }
          // Shape guard: opencode could rename or drop payload fields. Feed
          // only well-formed events into the capture stream — a TypeError
          // here would abort the for-await loop and silently end capture.
          if (typeof data?.sessionID !== "string" || typeof data?.text !== "string") {
            if (!payloadWarned) {
              payloadWarned = true
              console.log(
                "[armin] unexpected session.text.ended payload (skipping; ARMIN_DEBUG=1 for details)",
              )
            }
            log(
              "unexpected session.text.ended payload:",
              JSON.stringify(event.data) ?? String(event.data),
            )
            continue
          }
          const partID = `${data.assistantMessageID ?? "?"}:${data.ordinal ?? "?"}`
          ingestAssistantText(data.sessionID, partID, data.text)
        }
      } catch (e) {
        log("event stream stopped:", String(e))
      }
    })()

    const registrations: { dispose: () => Promise<void> }[] = []

    // User prompt (and, on the first model call, the live session model).
    registrations.push(
      await ctx.session.hook("prompt", (event) => {
        capture.ingest(event.sessionID, "user_prompt", "user", event.prompt.text ?? "")
      }),
    )

    // Zero-cost reasoning-state reminder on every model call of the agent loop.
    registrations.push(
      await ctx.session.hook("context", async (event) => {
        if (followSessionModel) pushSessionModel(event.model?.id)
        try {
          const res = await brief()
          if (res && !res.empty && res.brief) {
            event.system.push({
              type: "text",
              text:
                "Reasoning state (ARMIN): tracked decisions, rules, and open items. " +
                "Use query_graph for detail. If a new request conflicts with a decision or rule " +
                "below, say so explicitly before deviating.\n" +
                res.brief,
            })
          }
        } catch {
          // Engine down — skip injection silently.
        }
      }),
    )

    // Reasoning survives compaction: the summary request carries the decisions
    // and open questions that must not be re-litigated.
    registrations.push(
      await ctx.session.hook("compaction", async (event) => {
        const summary = await compactionContext(api)
        if (summary) event.system.push({ type: "text", text: summary })
      }),
    )

    // Tool capture (and the optional, off-by-default brief footer).
    registrations.push(
      await ctx.tool.hook("execute.after", async (event) => {
        const files = extractFiles(event.input)
        rememberFiles(files)
        const failed = event.status !== "completed"
        // event.error is typed Error but hosts pass what they have (plain
        // strings happen) — String() keeps this from printing "undefined".
        const reason = failed
          ? truncate(
              typeof event.error === "string"
                ? event.error
                : String((event.error as { message?: string } | undefined)?.message ?? ""),
              400,
            )
          : ""
        const text = failed
          ? `${event.tool} failed: ${reason}`
          : `${event.tool}: ${truncate(resultText(event.result), 400)}`
        capture.ingest(event.sessionID, "tool_call", "agent", text, {
          toolName: event.tool,
          files,
        })
        if (briefFooter && files.length > 0 && event.status === "completed") {
          const res = await brief()
          const binding = res && !res.empty ? bindingsFooter(res.brief) : null
          if (binding) {
            event.result = withFooter(
              event.result,
              `\n\n[ARMIN — settled decisions/rules for these files]\n${binding}`,
            ) as typeof event.result
          }
        }
      }),
    )

    // Tools
    registrations.push(
      await ctx.tool.transform((editor) => {
        for (const definition of v2Tools(api)) editor.add(definition)
      }),
    )

    return async () => {
      controller.abort()
      for (const registration of [...registrations].reverse()) {
        try {
          await registration.dispose()
        } catch (e) {
          log("dispose failed:", String(e))
        }
      }
      await sidecar.dispose()
    }
  },
}

export const ArminPluginExport = ArminPlugin

// One entrypoint for both generations: V2 reads id + setup, V1 (1.18.29+)
// reads server(). Older V1 releases call the exported plugin function, whose
// identity is shared with `server` above, so it is never registered twice.
export default {
  ...ArminPluginV2,
  server: ArminPlugin,
}
