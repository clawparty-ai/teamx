// tunnel.ts — reverse-tunnel client (provider + consumer sides).
//
// Ported from opencode-plugin/src/tunnel.ts, adapted for the dsh runtime:
// - uses the `ws` npm package (Node) instead of Bun's global WebSocket
//   (the `ws` options object carries mTLS material directly);
// - mtlsFor is async in dsh (returns a Promise), so connect() awaits it.
//
// Runs as a resident connection managed by the plugin.
//
// Provider side (exposeTunnel): opens a persistent mTLS WebSocket to
// `teamx serve`'s `/tunnel` endpoint, registers a local service, and bridges
// bytes between the server and the local TCP port:
//
//   consumer ⇄ server TCP relay ⇄ WS (binary frames) ⇄ provider ⇄ local service
//
// Consumer side (forwardTunnel, local-forward mode): listens on a LOCAL port
// and, for each connection, opens an mTLS WS to `/tunnel/forward` sending
// `{"type":"connect","name"}`; the server bridges that stream to the
// provider's WS. Accessing the local port feels like a local service.
//
// Wire format (server ↔ provider):
//   provider → server (text):  {"type":"register","name","port","lan_ip","mode"}
//                              {"type":"unregister","name"}
//   server → provider (text):  {"type":"registered","name","port"}
//                              {"type":"open_stream","stream_id"}
//                              {"type":"error","message"}
//   consumer → server (text):  {"type":"connect","name"}   (over /tunnel/forward)
//   server → consumer (text):  {"type":"stream_open","stream_id"}
//   data (binary, both ways):  [4-byte BE stream_id][payload]
// @module @teamx-ai/dsh-plugin/tunnel

import { connect as tcpConnect, createServer as netCreateServer, Socket } from 'node:net'
import WebSocket from 'ws'
import { mtlsFor, resolveServerUrl } from './client.js'

/** http(s)://… → ws(s)://…/ws (teamx event endpoint shape). */
function wsUrl(serverUrl: string): string {
  return serverUrl.replace(/^http/, 'ws').replace(/\/$/, '') + '/ws'
}

export interface TunnelExposeOpts {
  serverUrl: string
  name: string
  /** local port to expose */
  port: number
  /** exposure mode: "local" (default) or "frp" */
  mode?: 'local' | 'frp'
  /** provider LAN IP for direct-connect hints (optional) */
  lanIp?: string
  log?: (level: 'info' | 'warn' | 'error', message: string) => void
}

export interface TunnelHandle {
  close(): void
  /** resolve once the tunnel is registered and the public port is known */
  ready(): Promise<number | null>
}

export interface TunnelForwardOpts {
  serverUrl: string
  /** tunnel name exposed by the provider */
  name: string
  /** local port to listen on (optional; defaults to the provider's target port) */
  localPort?: number
  /** provider's target port (used as default local port when given) */
  targetPort?: number
  log?: (level: 'info' | 'warn' | 'error', message: string) => void
}

export interface TunnelForwardHandle {
  close(): void
  /** resolve once the local listener is bound and ready to accept */
  ready(): Promise<number | null>
}

/**
 * Open a persistent reverse tunnel. Keeps the WS alive and re-registers on
 * reconnect (exponential backoff). Resolves `ready()` once registered.
 */
export function exposeTunnel(opts: TunnelExposeOpts): TunnelHandle {
  const log = opts.log ?? ((_l, m) => console.log(`[teamx tunnel] ${m}`))
  let closed = false
  let ws: WebSocket | null = null
  let attempts = 0
  let timer: ReturnType<typeof setTimeout> | null = null
  let readyResult: number | null | undefined = undefined
  let readyResolve: ((port: number | null) => void) | null = null
  let readyTimer: ReturnType<typeof setTimeout> | null = null
  const streams = new Map<number, Socket>()

  function settleReady(v: number | null) {
    readyResult = v
    if (readyResolve) {
      const r = readyResolve
      readyResolve = null
      if (readyTimer) clearTimeout(readyTimer)
      r(v)
    }
  }

  function scheduleReconnect() {
    if (closed) return
    const delay = Math.min(60_000, 1000 * Math.pow(2, attempts)) + Math.random() * 500
    attempts += 1
    timer = setTimeout(connect, delay)
  }

  function register() {
    if (!ws || ws.readyState !== WebSocket.OPEN) return
    ws.send(
      JSON.stringify({
        type: 'register',
        name: opts.name,
        port: opts.port,
        mode: opts.mode ?? 'local',
        lan_ip: opts.lanIp,
      }),
    )
  }

  /** Dial the local service for a new stream and bridge it over the WS. */
  function openStream(sid: number) {
    if (streams.has(sid)) return
    let sock: Socket
    try {
      sock = tcpConnect({ host: '127.0.0.1', port: opts.port })
    } catch (e) {
      log('warn', `connect local service: ${String(e)}`)
      notifyServerClose(sid)
      return
    }
    streams.set(sid, sock)
    sock.on('data', (buf: Buffer) => {
      if (!ws || ws.readyState !== WebSocket.OPEN) return
      const frame = Buffer.alloc(4 + buf.length)
      frame.writeUInt32BE(sid)
      buf.copy(frame, 4)
      ws.send(frame)
    })
    sock.on('close', () => {
      streams.delete(sid)
      notifyServerClose(sid)
    })
    sock.on('error', () => {
      // 'close' fires afterwards and does the cleanup + notification.
    })
  }

  /** Tell the server to tear down a stream (best-effort). */
  function notifyServerClose(sid: number) {
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: 'close_stream', stream_id: sid }))
    }
  }

  function closeStream(sid: number) {
    const sock = streams.get(sid)
    if (sock) {
      streams.delete(sid)
      try {
        sock.destroy()
      } catch {
        // ignore
      }
    }
  }

  async function connect() {
    if (closed) return
    const tls = await mtlsFor(opts.serverUrl)
    let sock: WebSocket
    try {
      sock = new WebSocket(
        wsUrl(opts.serverUrl).replace(/\/ws$/, '/tunnel'),
        tls ? { ca: tls.ca, cert: tls.cert, key: tls.key, rejectUnauthorized: true } : undefined,
      )
    } catch (e) {
      log('warn', `tunnel connect error: ${String(e)}`)
      scheduleReconnect()
      return
    }
    ws = sock
    sock.on('open', () => {
      attempts = 0
      register()
    })
    sock.on('message', (data: Buffer, isBinary: boolean) => {
      if (!isBinary) {
        let msg: { type?: string; stream_id?: number; name?: string; port?: number; message?: string }
        try {
          msg = JSON.parse(data.toString())
        } catch {
          return
        }
        if (msg.type === 'registered') {
          log('info', `tunnel "${opts.name}" registered on public port ${msg.port}`)
          settleReady(msg.port ?? null)
        } else if (msg.type === 'open_stream' && typeof msg.stream_id === 'number') {
          openStream(msg.stream_id)
        } else if (msg.type === 'close_stream' && typeof msg.stream_id === 'number') {
          closeStream(msg.stream_id)
        } else if (msg.type === 'error') {
          log('warn', `tunnel error: ${msg.message ?? 'unknown'}`)
        }
      } else {
        // binary data frame: [stream_id][payload] → local socket
        const buf = Buffer.from(data as Uint8Array)
        if (buf.length < 4) return
        const sid = buf.readUInt32BE(0)
        const sock = streams.get(sid)
        if (sock) sock.write(buf.subarray(4))
      }
    })
    sock.on('close', () => {
      ws = null
      for (const s of streams.values()) {
        try {
          s.destroy()
        } catch {
          // ignore
        }
      }
      streams.clear()
      if (!closed) {
        log('warn', 'tunnel disconnected; reconnecting')
        scheduleReconnect()
      }
    })
    sock.on('error', () => {
      // 'close' fires afterwards
    })
  }

  void connect()

  return {
    close() {
      closed = true
      if (timer) clearTimeout(timer)
      if (readyTimer) clearTimeout(readyTimer)
      for (const s of streams.values()) {
        try {
          s.destroy()
        } catch {
          // ignore
        }
      }
      streams.clear()
      try {
        ws?.close()
      } catch {
        // ignore
      }
      ws = null
    },
    ready(): Promise<number | null> {
      if (readyResult !== undefined) return Promise.resolve(readyResult)
      return new Promise((r) => {
        readyResolve = r
        readyTimer = setTimeout(() => {
          const f = readyResolve
          readyResolve = null
          f?.(null)
        }, 10_000)
      })
    },
  }
}

/**
 * Consumer-side local forward: listen on a LOCAL port; each connection
 * opens a mTLS WS to `/tunnel/forward`, sends `{"type":"connect","name"}`, and
 * bridges bytes bidirectionally with the provider's tunnel. Accessing the
 * local port feels like using a local service.
 */
export function forwardTunnel(opts: TunnelForwardOpts): TunnelForwardHandle {
  const log = opts.log ?? ((_l, m) => console.log(`[teamx forward] ${m}`))
  let closed = false
  const sockets = new Set<Socket>()
  const streams = new Map<number, Socket>()
  let readyResult: number | null | undefined = undefined
  let readyResolve: ((port: number | null) => void) | null = null
  let readyTimer: ReturnType<typeof setTimeout> | null = null

  function settleReady(v: number | null) {
    readyResult = v
    if (readyResolve) {
      const r = readyResolve
      readyResolve = null
      if (readyTimer) clearTimeout(readyTimer)
      r(v)
    }
  }

  const server = netCreateServer((clientSocket) => {
    if (closed) {
      clientSocket.destroy()
      return
    }
    sockets.add(clientSocket)
    clientSocket.on('close', () => sockets.delete(clientSocket))
    clientSocket.on('error', () => sockets.delete(clientSocket))

    // One WS per local connection. mtlsFor is async in dsh, so the WS is
    // created inside the promise and the socket buffers until then.
    let ws: WebSocket | null = null
    const state = { sid: -1 }
    const pending: Buffer[] = []
    void (async () => {
      const tls = await mtlsFor(opts.serverUrl)
      try {
        ws = new WebSocket(
          wsUrl(opts.serverUrl).replace(/\/ws$/, '/tunnel/forward'),
          tls ? { ca: tls.ca, cert: tls.cert, key: tls.key, rejectUnauthorized: true } : undefined,
        )
      } catch (e) {
        log('warn', `forward connect error: ${String(e)}`)
        clientSocket.destroy()
        return
      }
      ws.on('open', () => {
        ws?.send(JSON.stringify({ type: 'connect', name: opts.name }))
      })
      ws.on('message', (data: Buffer, isBinary: boolean) => {
        if (!isBinary) {
          let msg: { type?: string; stream_id?: number; message?: string }
          try {
            msg = JSON.parse(data.toString())
          } catch {
            return
          }
          if (msg.type === 'stream_open' && typeof msg.stream_id === 'number') {
            state.sid = msg.stream_id
            // flush data buffered while the stream was opening
            for (const b of pending) {
              if (!ws || ws.readyState !== WebSocket.OPEN) return
              const frame = Buffer.alloc(4 + b.length)
              frame.writeUInt32BE(state.sid)
              b.copy(frame, 4)
              ws.send(frame)
            }
            pending.length = 0
          } else if (msg.type === 'error') {
            log('warn', `forward error: ${msg.message ?? 'unknown'}`)
            clientSocket.destroy()
          }
        } else {
          // binary frame: [stream_id][payload] → local socket
          const buf = Buffer.from(data as Uint8Array)
          if (buf.length < 4) return
          const sid = buf.readUInt32BE(0)
          if (state.sid >= 0 && sid === state.sid) {
            clientSocket.write(buf.subarray(4))
          }
        }
      })
      ws.on('close', () => {
        streams.delete(state.sid)
        clientSocket.destroy()
      })
      ws.on('error', () => {
        clientSocket.destroy()
      })
    })()

    // local socket → WS binary frame (buffer until the stream is open)
    clientSocket.on('data', (buf) => {
      const data = Buffer.isBuffer(buf) ? buf : Buffer.from(buf as unknown as Uint8Array)
      if (state.sid < 0) {
        pending.push(data)
        return
      }
      if (!ws || ws.readyState !== WebSocket.OPEN) return
      const frame = Buffer.alloc(4 + data.length)
      frame.writeUInt32BE(state.sid)
      data.copy(frame, 4)
      ws.send(frame)
    })

    clientSocket.on('close', () => {
      try {
        ws?.close()
      } catch {
        // ignore
      }
    })
  })

  // Bind the local listener (default: provider target port).
  const defaultPort = opts.localPort ?? opts.targetPort ?? 0
  const tryBind = (port: number) => {
    return new Promise<number | null>((resolve) => {
      const onErr = (e: NodeJS.ErrnoException) => {
        server.off('listening', onOk)
        log('warn', `bind 127.0.0.1:${port} failed: ${e.code ?? e.message}`)
        resolve(null)
      }
      const onOk = () => {
        server.off('error', onErr)
        // Resolve the ACTUAL bound port: with port=0 the OS assigns an
        // ephemeral port and reporting the requested 0 would be wrong.
        const addr = server.address()
        resolve(typeof addr === 'object' && addr !== null ? addr.port : null)
      }
      server.once('error', onErr)
      server.once('listening', onOk)
      server.listen(port, '127.0.0.1')
    })
  }

  tryBind(defaultPort).then(settleReady)

  return {
    close() {
      closed = true
      try {
        server.close()
      } catch {
        // ignore
      }
      for (const s of sockets) {
        try {
          s.destroy()
        } catch {
          // ignore
        }
      }
    },
    ready(): Promise<number | null> {
      if (readyResult !== undefined) return Promise.resolve(readyResult)
      return new Promise((r) => {
        readyResolve = r
        readyTimer = setTimeout(() => {
          const f = readyResolve
          readyResolve = null
          f?.(null)
        }, 10_000)
      })
    },
  }
}

/** Best tunnel server URL for the current dsh instance (network mode only). */
export function tunnelServerUrl(): string | null {
  return resolveServerUrl()
}
