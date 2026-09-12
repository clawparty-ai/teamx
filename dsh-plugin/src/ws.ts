/**
 * WebSocket push client for teamx server events.
 * Mirrors opencode-plugin's ws.ts but runs on Node (using `ws` package).
 *
 * IMPORTANT: the teamx server identifies the caller by the mTLS client
 * certificate CN (`pki::parse_member_cn`), NOT by any HTTP header. So this
 * client only needs the server URL + mTLS cert material; there are no
 * team/session headers. Cert material is resolved via env vars first and
 * then by scanning imported invitation letters (see `client.ts`), so a
 * letter imported after plugin startup is picked up by the retry loop
 * without a process restart.
 * @module @teamx-ai/dsh-plugin/ws
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
  private warnedNoMtls = false

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

    const mtls = await mtlsFor(this.opts.serverUrl)
    if (!mtls) {
      // The server identifies members by client-cert CN; without a cert the
      // connection can never succeed RIGHT NOW. But "no cert" usually means
      // the invitation letter simply hasn't been imported yet (or the env is
      // incomplete), which fixes itself later — so keep retrying with backoff
      // instead of giving up for the lifetime of the process.
      if (!this.warnedNoMtls) {
        this.warnedNoMtls = true
        console.warn('[teamx-dsh] WS push waiting for mTLS material (no env cert and no imported letter yet); will retry')
      }
      this.scheduleReconnect()
      return
    }
    this.warnedNoMtls = false

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
        // `{ type: "error", code, message }` frames carry auth failures
        if (msg?.type === 'error') {
          const code = msg?.code
          if (code === 'revoked') {
            // The cert itself was revoked — reconnecting can never succeed.
            this.gaveUp = true
            console.warn(`[teamx-dsh] WS push auth error (revoked): ${msg?.message ?? ''}`)
            this.ws?.close()
            return
          }
          if (code === 'no_identity' || code === 'not_a_member') {
            // Retryable: the letter may still be awaiting owner approval, or
            // the seat may be claimed a moment after import. Close and let the
            // backoff loop try again.
            console.warn(`[teamx-dsh] WS push auth not ready (${code}): ${msg?.message ?? ''}; will retry`)
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
    // Exponential backoff (1s → 30s cap) with jitter to avoid thundering herd
    // when the server bounces and every member reconnects at once.
    const delay = this.reconnectMs + Math.random() * 500
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null
      this.connect()
    }, delay)
    this.reconnectMs = Math.min(this.reconnectMs * 2, RECONNECT_MAX_MS)
  }
}
