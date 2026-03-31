#!/usr/bin/env bun

const PORT = 7332
const clients = new Set<ReadableStreamDefaultController<Uint8Array>>()

function push(type: string, payload: unknown) {
  const data = `data: ${JSON.stringify({ type, payload })}\n\n`
  const encoded = new TextEncoder().encode(data)
  for (const c of clients) {
    try { c.enqueue(encoded) } catch { clients.delete(c) }
  }
  console.log(`[sse] pushed ${type} to ${clients.size} clients`)
}

Bun.serve({
  port: PORT,
  hostname: "127.0.0.1",
  idleTimeout: 255, // max value — SSE connections are long-lived
  fetch(req) {
    const url = new URL(req.url)
    const cors = {
      "Access-Control-Allow-Origin": "*",
      "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
      "Access-Control-Allow-Headers": "Content-Type",
    }

    if (req.method === "OPTIONS") {
      return new Response(null, { status: 204, headers: cors })
    }

    if (req.method === "GET" && url.pathname === "/events") {
      let ctrl: ReadableStreamDefaultController<Uint8Array>
      const stream = new ReadableStream<Uint8Array>({
        start(c) {
          ctrl = c
          clients.add(ctrl)
          ctrl.enqueue(new TextEncoder().encode(": connected\n\n"))
          console.log(`[sse] client connected (${clients.size} total)`)
        },
        cancel() {
          clients.delete(ctrl)
          console.log(`[sse] client disconnected (${clients.size} total)`)
        },
      })
      return new Response(stream, {
        headers: { ...cors, "Content-Type": "text/event-stream", "Cache-Control": "no-cache" },
      })
    }

    if (req.method === "POST" && url.pathname === "/notify") {
      return req.json()
        .then((body: unknown) => {
          push("notify", body)
          return new Response("ok", { headers: cors })
        })
        .catch(() => new Response("bad json", { status: 400, headers: cors }))
    }

    if (req.method === "POST" && url.pathname === "/navigate") {
      return req.json()
        .then((body: unknown) => {
          push("navigate", body)
          return new Response("ok", { headers: cors })
        })
        .catch(() => new Response("bad json", { status: 400, headers: cors }))
    }

    return new Response("not found", { status: 404 })
  },
})

console.log(`[sse] server ready on http://127.0.0.1:${PORT}`)
