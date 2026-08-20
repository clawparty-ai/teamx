/**
 * WebSocket push client for teamx server events.
 * Mirrors opencode-plugin's ws.ts but runs on Node (using `ws` package).
 * Supports mTLS via TEAMX_MTLS_* env vars.
 * @module @teamx/dsh-plugin/ws
 */

import { EventEmitter } from 'node:events'
import { WebSocket } from 'ws'
import { mtlsFor } from './client.js'

export interface WsOptions {
  serverUrl: string
  team: string
  session: string
  token?: string
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
    const url = new URL('/ws', this.opts.serverUrl)
    const wsUrl = url.toString().replace(/^http/, 'ws')

    const headers: Record<string, string> = {
      'X-Teamx-Team': this.opts.team,
      'X-Teamx-Session': this.opts.session,
    }
    if (this.opts.token) {
      headers['Authorization'] = `Bearer ${this.opts.token}`
    }

    const ws = new WebSocket(wsUrl, {
      headers,
      ca: mtls?.ca,
      cert: mtls?.cert,
      key: mtls?.key,
      rejectUnauthorized: !!mtls,
    })

    this.ws = ws

    ws.on('open', () => {
      this.reconnectMs = RECONNECT_BASE_MS
      this.emit('connected')
    })

    ws.on('message', (raw: Buffer) => {
      try {
        const event = JSON.parse(raw.toString())
        this.emit('event', event)
      } catch {
        // ignore malformed messages
      }
    })

    ws.on('close', () => {
      this.ws = null
      if (!this.stopped) {
        this.scheduleReconnect()
      }
    })

    ws.on('error', (err) => {
      console.error('[teamx-dsh] WS error:', err.message)
      // 'close' event will handle reconnection
    })
  }

  private scheduleReconnect(): void {
    if (this.stopped) return
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null
      this.connect()
    }, this.reconnectMs)
    // Exponential backoff with cap
    this.reconnectMs = Math.min(this.reconnectMs * 2, RECONNECT_MAX_MS)
  }
}
