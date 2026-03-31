#!/usr/bin/env bun
/**
 * Nanika Dashboard MCP channel bridge.
 *
 * Architecture:
 *   Claude Code ←─stdio─→ [this server] ←─HTTP POST /channel─→ Dashboard (React)
 *
 * When the user interacts with the dashboard (e.g. clicks a mission), the React
 * app POSTs to /channel. This server forwards the event to Claude Code as a
 * notifications/claude/channel notification so Claude can act on it.
 *
 * Port: DASHBOARD_CHANNEL_PORT env (default 7332).
 *
 * Plugin discovery routes (added):
 *   GET  /api/plugins                  — scan plugins/*/plugin.json, return api_version>=1
 *   GET  /api/plugins/:name            — single plugin info
 *   GET  /api/plugins/:name/status     — exec `<binary> query status --json`
 *   GET  /api/plugins/:name/items      — exec `<binary> query items --json`
 *   POST /api/plugins/:name/action     — exec `<binary> query action <verb> [item_id] --json`
 */

import { readdir, readFile } from 'node:fs/promises'
import { join } from 'node:path'
import { Server } from '@modelcontextprotocol/sdk/server/index.js'
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js'
import { CallToolRequestSchema, ListToolsRequestSchema } from '@modelcontextprotocol/sdk/types.js'

// ── Plugin discovery ──────────────────────────────────────────────────────────

// Repo root is 3 levels up: channel/ → dashboard/ → plugins/ → repo root
const REPO_ROOT = join(import.meta.dir, '..', '..', '..')

interface PluginJson {
  name: string
  version?: string
  api_version?: number
  description?: string
  binary?: string
  provides?: string[]
  actions?: Record<string, unknown>
  tags?: string[]
}

async function scanPlugins(): Promise<PluginJson[]> {
  const pluginsDir = join(REPO_ROOT, 'plugins')
  let entries: import('node:fs').Dirent[]
  try {
    entries = await readdir(pluginsDir, { withFileTypes: true })
  } catch {
    return []
  }

  const results: PluginJson[] = []
  for (const entry of entries) {
    if (!entry.isDirectory()) continue
    const jsonPath = join(pluginsDir, entry.name, 'plugin.json')
    try {
      const raw = await readFile(jsonPath, 'utf-8')
      const data = JSON.parse(raw) as PluginJson
      if (typeof data.api_version === 'number' && data.api_version >= 1 && data.binary && data.name) {
        results.push(data)
      }
    } catch {
      // skip missing or unparseable plugin.json
    }
  }
  return results
}

async function execBinary(
  binary: string,
  args: string[],
): Promise<{ stdout: string; stderr: string; exitCode: number }> {
  try {
    const proc = Bun.spawn([binary, ...args], {
      stdout: 'pipe',
      stderr: 'pipe',
    })
    const [stdout, stderr] = await Promise.all([
      new Response(proc.stdout).text(),
      new Response(proc.stderr).text(),
    ])
    const exitCode = await proc.exited
    return { stdout, stderr, exitCode }
  } catch (err) {
    return { stdout: '', stderr: String(err), exitCode: 1 }
  }
}

async function handlePluginsRoute(req: Request, url: URL): Promise<Response | null> {
  const segments = url.pathname.split('/').filter(Boolean)
  // segments: ['api', 'plugins', ...rest]
  if (segments[0] !== 'api' || segments[1] !== 'plugins') return null

  const pluginName = segments[2]
  const subpath = segments[3]

  // GET /api/plugins
  if (req.method === 'GET' && !pluginName) {
    const plugins = await scanPlugins()
    return new Response(JSON.stringify(plugins), {
      headers: { ...CORS_HEADERS, 'Content-Type': 'application/json' },
    })
  }

  // GET /api/plugins/:name  (single plugin info)
  if (req.method === 'GET' && pluginName && !subpath) {
    const plugins = await scanPlugins()
    const plugin = plugins.find(p => p.name === pluginName)
    if (!plugin) return new Response('not found', { status: 404, headers: CORS_HEADERS })
    return new Response(JSON.stringify(plugin), {
      headers: { ...CORS_HEADERS, 'Content-Type': 'application/json' },
    })
  }

  // GET /api/plugins/:name/status  or  /items
  if (req.method === 'GET' && pluginName && (subpath === 'status' || subpath === 'items')) {
    const plugins = await scanPlugins()
    const plugin = plugins.find(p => p.name === pluginName)
    if (!plugin?.binary) return new Response('not found', { status: 404, headers: CORS_HEADERS })

    const result = await execBinary(plugin.binary, ['query', subpath, '--json'])
    if (result.exitCode !== 0) {
      return new Response(
        JSON.stringify({ error: result.stderr.trim() || `exit code ${result.exitCode}` }),
        { status: 502, headers: { ...CORS_HEADERS, 'Content-Type': 'application/json' } },
      )
    }
    return new Response(result.stdout, {
      headers: { ...CORS_HEADERS, 'Content-Type': 'application/json' },
    })
  }

  // POST /api/plugins/:name/action
  if (req.method === 'POST' && pluginName && subpath === 'action') {
    const plugins = await scanPlugins()
    const plugin = plugins.find(p => p.name === pluginName)
    if (!plugin?.binary) return new Response('not found', { status: 404, headers: CORS_HEADERS })

    let body: unknown
    try { body = await req.json() } catch {
      return new Response('bad request: invalid JSON', { status: 400, headers: CORS_HEADERS })
    }
    const b = body as Record<string, unknown>
    const action = typeof b.action === 'string' ? b.action : ''
    const itemId = typeof b.item_id === 'string' ? b.item_id : undefined

    if (!action) {
      return new Response('bad request: action required', { status: 400, headers: CORS_HEADERS })
    }

    // Construct: <binary> query action <action> [item_id] --json
    const args = ['query', 'action', action, ...(itemId ? [itemId] : []), '--json']
    const result = await execBinary(plugin.binary, args)
    return new Response(result.stdout || JSON.stringify({ ok: result.exitCode === 0 }), {
      status: result.exitCode === 0 ? 200 : 502,
      headers: { ...CORS_HEADERS, 'Content-Type': 'application/json' },
    })
  }

  return null
}

const PORT = Number(process.env.DASHBOARD_CHANNEL_PORT ?? 7332)

process.on('unhandledRejection', (err) => {
  process.stderr.write(`dashboard channel: unhandled rejection: ${err}\n`)
})
process.on('uncaughtException', (err) => {
  process.stderr.write(`dashboard channel: uncaught exception: ${err}\n`)
})

const mcp = new Server(
  { name: 'dashboard', version: '1.0.0' },
  {
    capabilities: { tools: {}, experimental: { 'claude/channel': {} } },
    instructions: [
      'Dashboard interactions arrive as <channel source="dashboard" ...>.',
      'Actions: mission.select (user clicked a mission to inspect it), missions.refresh (user refreshed the list), conversation.message (user sent a message in the conversation panel).',
      'Use this context to understand what the user is currently looking at. If they selected a mission, they may want you to describe its status, show logs, or take action on it.',
      'For conversation.message actions, reply using the reply tool with a concise, helpful response. The reply will be shown inline in the dashboard conversation panel.',
      'Do not respond to ambient actions (mission.select, missions.refresh) unless the user asks a direct question. Always respond to conversation.message.',
    ].join('\n'),
  },
)

// ── SSE clients ──────────────────────────────────────────────────────────────

type SseController = ReadableStreamDefaultController<Uint8Array>
const sseClients = new Set<SseController>()

function pushSseEvent(type: string, payload: unknown): void {
  const data = `data: ${JSON.stringify({ type, payload })}\n\n`
  const encoded = new TextEncoder().encode(data)
  for (const ctrl of sseClients) {
    try {
      ctrl.enqueue(encoded)
    } catch {
      sseClients.delete(ctrl)
    }
  }
}

// ── MCP tools ────────────────────────────────────────────────────────────────

mcp.setRequestHandler(ListToolsRequestSchema, async () => ({
  tools: [
    {
      name: 'navigate',
      description: 'Navigate the dashboard to a specific page.',
      inputSchema: {
        type: 'object',
        properties: {
          page: { type: 'string', description: 'Page name or path to navigate to.' },
        },
        required: ['page'],
      },
    },
    {
      name: 'notify',
      description: 'Send a notification to the dashboard.',
      inputSchema: {
        type: 'object',
        properties: {
          text: { type: 'string', description: 'Notification message text.' },
          type: {
            type: 'string',
            enum: ['info', 'success', 'warning', 'error'],
            description: 'Notification severity level.',
          },
        },
        required: ['text'],
      },
    },
    {
      name: 'reply',
      description: 'Send a reply to the active conversation in the dashboard. Use this in response to conversation.message actions.',
      inputSchema: {
        type: 'object',
        properties: {
          text: { type: 'string', description: 'Reply text to show in the conversation panel.' },
        },
        required: ['text'],
      },
    },
  ],
}))

mcp.setRequestHandler(CallToolRequestSchema, async (req) => {
  const { name, arguments: args } = req.params
  const a = (args ?? {}) as Record<string, unknown>

  if (name === 'navigate') {
    const page = typeof a.page === 'string' ? a.page : ''
    if (!page) {
      return { isError: true, content: [{ type: 'text', text: 'navigate: page (string) required' }] }
    }
    pushSseEvent('navigate', { page })
    return { content: [{ type: 'text', text: `navigated to ${page}` }] }
  }

  if (name === 'notify') {
    const text = typeof a.text === 'string' ? a.text : ''
    if (!text) {
      return { isError: true, content: [{ type: 'text', text: 'notify: text (string) required' }] }
    }
    const notifyType = typeof a.type === 'string' ? a.type : undefined
    pushSseEvent('notify', { text, ...(notifyType ? { type: notifyType } : {}) })
    return { content: [{ type: 'text', text: `notification sent: ${text}` }] }
  }

  if (name === 'reply') {
    const text = typeof a.text === 'string' ? a.text : ''
    if (!text) {
      return { isError: true, content: [{ type: 'text', text: 'reply: text (string) required' }] }
    }
    pushSseEvent('conversation.reply', { text })
    return { content: [{ type: 'text', text: 'reply sent' }] }
  }

  return { isError: true, content: [{ type: 'text', text: `unknown tool: ${name}` }] }
})

// ── CORS ────────────────────────────────────────────────────────────────────
// Accepts requests from the Vite dev server and any localhost origin.
const CORS_HEADERS = {
  'Access-Control-Allow-Origin': '*',
  'Access-Control-Allow-Methods': 'GET, POST, OPTIONS',
  'Access-Control-Allow-Headers': 'Content-Type',
}

// ── HTTP server ──────────────────────────────────────────────────────────────

type ChannelEventBody = {
  action: string
  context?: Record<string, unknown>
}

function isChannelEventBody(v: unknown): v is ChannelEventBody {
  if (!v || typeof v !== 'object') return false
  const obj = v as Record<string, unknown>
  return typeof obj.action === 'string' && obj.action.length > 0
}

function formatContent(action: string, context: Record<string, unknown>): string {
  switch (action) {
    case 'mission.select': {
      const id = typeof context.mission_id === 'string' ? context.mission_id : undefined
      const task = typeof context.task === 'string' ? context.task : undefined
      const status = typeof context.status === 'string' ? context.status : undefined
      const parts: string[] = ['User selected mission']
      if (id) parts.push(` ${id}`)
      if (task) parts.push(`: "${task}"`)
      if (status) parts.push(` (${status})`)
      return parts.join('') + ' in the nanika dashboard.'
    }
    case 'missions.refresh':
      return 'User refreshed the missions panel in the nanika dashboard.'
    default: {
      const ctx = Object.keys(context).length ? ` — ${JSON.stringify(context)}` : ''
      return `Dashboard action: ${action}${ctx}`
    }
  }
}

function flattenMeta(
  action: string,
  context: Record<string, unknown>,
): Record<string, string> {
  const meta: Record<string, string> = {
    source: 'dashboard',
    action,
    ts: new Date().toISOString(),
  }
  for (const [k, v] of Object.entries(context)) {
    meta[k] = typeof v === 'string' ? v : JSON.stringify(v)
  }
  return meta
}

function handleEvents(): Response {
  let ctrl: SseController
  let heartbeat: ReturnType<typeof setInterval>
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      ctrl = controller
      sseClients.add(ctrl)
      // Send an initial keep-alive comment so the browser confirms the connection.
      ctrl.enqueue(new TextEncoder().encode(': connected\n\n'))
      // Send a heartbeat comment every 15s to prevent idle proxies from closing the connection.
      heartbeat = setInterval(() => {
        try {
          ctrl.enqueue(new TextEncoder().encode(': heartbeat\n\n'))
        } catch {
          clearInterval(heartbeat)
          sseClients.delete(ctrl)
        }
      }, 15_000)
    },
    cancel() {
      clearInterval(heartbeat)
      sseClients.delete(ctrl)
    },
  })

  return new Response(stream, {
    status: 200,
    headers: {
      ...CORS_HEADERS,
      'Content-Type': 'text/event-stream',
      'Cache-Control': 'no-cache',
      Connection: 'keep-alive',
    },
  })
}

async function handlePost(req: Request): Promise<Response> {
  let body: unknown
  try {
    body = await req.json()
  } catch {
    return new Response('bad request: invalid JSON', { status: 400, headers: CORS_HEADERS })
  }

  if (!isChannelEventBody(body)) {
    return new Response('bad request: action (string) required', {
      status: 400,
      headers: CORS_HEADERS,
    })
  }

  const ctx = body.context ?? {}
  const content = formatContent(body.action, ctx)
  const meta = flattenMeta(body.action, ctx)

  mcp
    .notification({ method: 'notifications/claude/channel', params: { content, meta } })
    .catch((err) => {
      process.stderr.write(`dashboard channel: failed to deliver to Claude: ${err}\n`)
    })

  return new Response('ok', { status: 200, headers: CORS_HEADERS })
}

Bun.serve({
  port: PORT,
  idleTimeout: 255, // max value — SSE connections are long-lived
  async fetch(req) {
    const url = new URL(req.url)

    if (req.method === 'OPTIONS') {
      return new Response(null, { status: 204, headers: CORS_HEADERS })
    }

    if (req.method === 'GET' && url.pathname === '/events') {
      return handleEvents()
    }

    if (req.method === 'POST' && url.pathname === '/channel') {
      return handlePost(req)
    }

    // Plugin discovery + proxy routes
    const pluginResponse = await handlePluginsRoute(req, url)
    if (pluginResponse) return pluginResponse

    return new Response('not found', { status: 404 })
  },
})

// Connect to Claude Code via stdio — must happen after HTTP is listening so
// Claude doesn't time out waiting while the port binds.
// When stdin is a TTY the server is running standalone (e.g. for testing); skip MCP in that case.
if (process.stdin.isTTY) {
  process.stderr.write(`dashboard channel: standalone mode (TTY detected), skipping MCP stdio\n`)
  process.stderr.write(`dashboard channel: ready on :${PORT}\n`)
} else {
  await mcp.connect(new StdioServerTransport())
  process.stderr.write(`dashboard channel: ready on :${PORT}\n`)

  // When Claude Code closes the MCP connection (EOF on stdin), shut down cleanly.
  process.stdin.on('end', () => {
    process.stderr.write('dashboard channel: stdin closed, exiting\n')
    process.exit(0)
  })
}
