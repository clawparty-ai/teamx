/**
 * WebSocket push client for teamx server events.
 * Mirrors opencode-plugin's ws.ts but runs on Node (using `ws` package).
 *
 * IMPORTANT: the teamx server identifies the caller by the mTLS client
 * certificate CN (`pki::parse_member_cn`), NOT by any HTTP header. So this
 * client only needs the server URL + mTLS cert material; there are no
 * team/session headers. Without a client cert the server replies
 * `{"type":"error","code":"no_identity"}` and closes — we fail fast rather
 * than reconnect forever.
 * @module @teamx/dsh-plugin/ws
 */

import { EventEmitter } from 'node:events'
import { WebSocket } from 'ws'
import { mtlsFor } from './client.js'

export interface WsOptions {
  serverUrl: string
}

const RECONNECT_BASE_MS = 1000
const RECONNECT_MAX_MS = 30_000

/**
 * Connect to the teamx server's WS endpoint for real-time event push.
 * Returns a WsClient with start/stop methods and an 'event' listener.
 */
export function createWsClient(opts: WsOptions): WsClient {
  return new WsClient(opts)
}

export class WsClient extends EventEmitter {
  private ws: WebSocket | null = null
  private opts: WsOptions
  private reconnectMs = RECONNECT_BASE_MS
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null
  private stopped = false
  private gaveUp = false

  constructor(opts: WsOptions) {
    super()
    this.opts = opts
  }

  async start(): Promise<void> {
    this.stopped = false
    await this.connect()
  }

  stop(): void {
    this.stopped = true
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }
    if (this.ws) {
      this.ws.removeAllListeners()
      if (this.ws.readyState === WebSocket.OPEN || this.ws.readyState === WebSocket.CONNECTING) {
        this.ws.close()
      }
      this.ws = null
    }
  }

  private async connect(): Promise<void> {
    if (this.stopped) return

    const mtls = await mtlsFor()
    if (!mtls) {
      // The server identifies members by client-cert CN; without a cert the
      // connection can never succeed. Fail fast instead of reconnecting.
      if (!this.gaveUp) {
        this.gaveUp = true
        console.warn('[teamx-dsh] WS push skipped: TEAMX_MTLS_CERT/KEY/CA not set (server identifies members via mTLS)')
      }
      return
    }

    const url = new URL('/ws', this.opts.serverUrl)
    const wsUrl = url.toString().replace(/^http/, 'ws')

    const ws = new WebSocket(wsUrl, {
      ca: mtls.ca,
      cert: mtls.cert,
      key: mtls.key,
      rejectUnauthorized: true,
    })

    this.ws = ws

    ws.on('open', () => {
      this.reconnectMs = RECONNECT_BASE_MS
      this.emit('connected')
    })

    ws.on('message', (raw: Buffer) => {
      try {
        const msg = JSON.parse(raw.toString())
        // Answer heartbeat pings (same as opencode-plugin's ws.ts)
        if (msg?.type === 'ping') {
          ws.send(JSON.stringify({ type: 'pong' }))
          return
        }
        // `{ type: "event", event: {...} }` frames carry ledger events
        if (msg?.type === 'event' && msg.event) {
          this.emit('event', msg.event)
          return
        }
        // `{ type: "error", code, message }` frames are terminal-ish auth errors
        if (msg?.type === 'error') {
          const code = msg?.code
          if (code === 'no_identity' || code === 'revoked' || code === 'not_a_member') {
            // Can't recover without re-importing / re-issuing a cert.
            this.gaveUp = true
            console.warn(`[teamx-dsh] WS push auth error (${code}): ${msg?.message ?? ''}`)
            this.ws?.close()
            return
          }
        }
        // `registered` / `pong` are informational; ignore.
      } catch {
        // ignore malformed messages
      }
    })

    ws.on('close', () => {
      this.ws = null
      if (!this.stopped && !this.gaveUp) {
        this.scheduleReconnect()
      }
    })

    ws.on('error', (err) => {
      console.error('[teamx-dsh] WS error:', err.message)
      // 'close' event will handle reconnection
    })
  }

  private scheduleReconnect(): void {
    if (this.stopped || this.gaveUp) return
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null
      this.connect()
    }, this.reconnectMs)
    // Exponential backoff with cap
    this.reconnectMs = Math.min(this.reconnectMs * 2, RECONNECT_MAX_MS)
  }
}
