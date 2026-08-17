// ws.ts — live WebSocket push client (network mode N1).
//
// Connects to `teamx serve`'s `/ws` endpoint (mTLS when a client certificate
// is available), receives real-time ledger events, answers heartbeat pings and
// reconnects with exponential backoff. When the server is unreachable the
// plugin falls back to its M2 poller (see index.ts).

import { mtlsFor, TEAMX_SERVER_URL } from "./client"

export type WsEvent = Record<string, unknown> & { seq?: number; type?: string }

export interface WsHandle {
  close(): void
  connected(): boolean
}

/** Derive the ws:// endpoint from a http(s):// server URL. */
export function wsUrl(serverUrl: string): string {
  return serverUrl.replace(/^http/, "ws").replace(/\/$/, "") + "/ws"
}

/**
 * Open (and keep open) a live push connection.
 *
 * `onEvent` is called for each `{ type: "event", event: {...} }` frame.
 * `onStatus` reports connectivity changes (true = connected).
 */
export function connectWs(opts: {
  serverUrl?: string
  onEvent: (event: WsEvent) => void
  onStatus?: (connected: boolean) => void
  log?: (level: "debug" | "warn", message: string) => void
}): WsHandle {
  const serverUrl = opts.serverUrl ?? TEAMX_SERVER_URL
  let closed = false
  let ws: WebSocket | null = null
  let attempts = 0
  let timer: ReturnType<typeof setTimeout> | null = null

  function scheduleReconnect() {
    if (closed) return
    // exponential backoff 1s → 60s with jitter
    const delay = Math.min(60_000, 1000 * Math.pow(2, attempts)) + Math.random() * 500
    attempts += 1
    timer = setTimeout(connect, delay)
  }

  function connect() {
    if (closed || !serverUrl) return
    const tls = mtlsFor(serverUrl)
    let sock: WebSocket
    try {
      sock = new WebSocket(wsUrl(serverUrl), (tls ? { tls } : undefined) as never)
    } catch (e) {
      opts.log?.("warn", `ws connect error: ${String(e)}`)
      scheduleReconnect()
      return
    }
    ws = sock
    sock.onopen = () => {
      attempts = 0
      opts.onStatus?.(true)
    }
    sock.onmessage = (ev) => {
      let msg: unknown
      try {
        msg = JSON.parse(String(ev.data))
      } catch {
        return
      }
      const m = msg as { type?: string; event?: WsEvent }
      if (m.type === "event" && m.event) {
        opts.onEvent(m.event)
      } else if (m.type === "ping") {
        sock.send(JSON.stringify({ type: "pong" }))
      }
      // `registered` / `pong` / `error` are informational; the poller/sync
      // stays the fallback path, so no special handling is required here.
    }
    sock.onclose = () => {
      ws = null
      opts.onStatus?.(false)
      scheduleReconnect()
    }
    sock.onerror = () => {
      // onclose fires afterwards; avoid double scheduling here.
    }
  }

  connect()

  return {
    close() {
      closed = true
      if (timer) clearTimeout(timer)
      try {
        ws?.close()
      } catch {
        // ignore
      }
      ws = null
    },
    connected() {
      return ws?.readyState === WebSocket.OPEN
    },
  }
}
