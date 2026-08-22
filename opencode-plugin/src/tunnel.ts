// tunnel.ts — reverse-tunnel client (provider + consumer sides).
//
// Runs as a resident subprocess spawned by the plugin.
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

import { connect as tcpConnect, createServer as netCreateServer, Socket } from "node:net"
import { wsUrl } from "./ws"
import { mtlsFor, TEAMX_SERVER_URL } from "./client"

export interface TunnelExposeOpts {
  serverUrl: string
  name: string
  /** local port to expose */
  port: number
  /** exposure mode: "local" (default) or "frp" */
  mode?: "local" | "frp"
  /** provider LAN IP for direct-connect hints (optional) */
  lanIp?: string
  log?: (level: "info" | "warn" | "error", message: string) => void
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
  log?: (level: "info" | "warn" | "error", message: string) => void
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
  // Last known registration result; lets ready() return immediately even when
  // the `registered` ack arrives before the caller calls ready().
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
        type: "register",
        name: opts.name,
        port: opts.port,
        mode: opts.mode ?? "local",
        lan_ip: opts.lanIp,
      }),
    )
  }

  /** Dial the local service for a new stream and bridge it over the WS. */
  function openStream(sid: number) {
    if (streams.has(sid)) return
    let sock: Socket
    try {
      sock = tcpConnect({ host: "127.0.0.1", port: opts.port })
    } catch (e) {
      log("warn", `connect local service: ${String(e)}`)
      notifyServerClose(sid)
      return
    }
    streams.set(sid, sock)
    sock.on("data", (buf: Buffer) => {
      if (ws?.readyState !== WebSocket.OPEN) return
      const frame = Buffer.alloc(4 + buf.length)
      frame.writeUInt32BE(sid)
      buf.copy(frame, 4)
      ws.send(frame)
    })
    sock.on("close", () => {
      // The local end is gone (dial failed / closed): tell the server so the
      // consumer is not left waiting on a half-open stream.
      streams.delete(sid)
      notifyServerClose(sid)
    })
    sock.on("error", () => {
      // 'close' fires afterwards and does the cleanup + notification.
    })
  }

  /** Tell the server to tear down a stream (best-effort). */
  function notifyServerClose(sid: number) {
    if (ws?.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: "close_stream", stream_id: sid }))
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

  function connect() {
    if (closed) return
    const tls = mtlsFor(opts.serverUrl)
    let sock: WebSocket
    try {
      sock = new WebSocket(wsUrl(opts.serverUrl).replace(/\/ws$/, "/tunnel"), (tls ? { tls } : undefined) as never)
    } catch (e) {
      log("warn", `tunnel connect error: ${String(e)}`)
      scheduleReconnect()
      return
    }
    ws = sock
    sock.onopen = () => {
      attempts = 0
      register()
    }
    sock.onmessage = (ev) => {
      const data = ev.data
      if (typeof data === "string") {
        let msg: { type?: string; stream_id?: number; name?: string; port?: number; message?: string }
        try {
          msg = JSON.parse(data)
        } catch {
          return
        }
        if (msg.type === "registered") {
          log("info", `tunnel "${opts.name}" registered on public port ${msg.port}`)
          settleReady(msg.port ?? null)
        } else if (msg.type === "open_stream" && typeof msg.stream_id === "number") {
          openStream(msg.stream_id)
        } else if (msg.type === "close_stream" && typeof msg.stream_id === "number") {
          closeStream(msg.stream_id)
        } else if (msg.type === "error") {
          log("warn", `tunnel error: ${msg.message ?? "unknown"}`)
        }
      } else {
        // binary data frame: [stream_id][payload] → local socket
        const buf = Buffer.from(data as Uint8Array)
        if (buf.length < 4) return
        const sid = buf.readUInt32BE(0)
        const sock = streams.get(sid)
        if (sock) sock.write(buf.subarray(4))
      }
    }
    sock.onclose = () => {
      ws = null
      // drop all streams (the local sockets are closed by the remote side)
      for (const s of streams.values()) {
        try {
          s.destroy()
        } catch {
          // ignore
        }
      }
      streams.clear()
      if (!closed) {
        log("warn", "tunnel disconnected; reconnecting")
        scheduleReconnect()
      }
    }
    sock.onerror = () => {
      // onclose fires afterwards
    }
  }

  connect()

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
 * CLI entrypoint used by the plugin's `teamx_tunnel_expose` tool and by manual
 * invocation: `bun tunnel.ts expose --server <url> --name <n> --port <p>`.
 * The process runs forever (resident); the caller closes it via a signal.
 */
export async function runTunnelCli(argv: string[]): Promise<number> {
  const args = [...argv]
  const flag = (name: string): string | undefined => {
    const i = args.indexOf(`--${name}`)
    return i >= 0 && i + 1 < args.length ? args[i + 1] : undefined
  }
  const serverUrl = flag("server") ?? TEAMX_SERVER_URL
  const name = flag("name")
  const port = Number(flag("port") ?? 0)
  if (!serverUrl || !name || !port) {
    console.error("usage: teamx tunnel expose --server <url> --name <name> --port <port> [--lan-ip <ip>]")
    return 1
  }
  const handle = exposeTunnel({
    serverUrl,
    name,
    port,
    lanIp: flag("lan-ip"),
    log: (level, m) => console[level === "error" ? "error" : "log"](`[teamx tunnel] ${m}`),
  })
  const pubPort = await handle.ready()
  if (pubPort === null) {
    console.error(`[teamx tunnel] failed to register tunnel "${name}"`)
    handle.close()
    return 1
  }
  console.log(JSON.stringify({ ok: true, name, public_port: pubPort }))
  // keep alive forever
  return await new Promise<number>(() => {})
}

/**
 * Consumer-side local forward (T2): listen on a LOCAL port; each connection
 * opens a mTLS WS to `/tunnel/forward`, sends `{"type":"connect","name"}`, and
 * bridges bytes bidirectionally with the provider's tunnel. Accessing the
 * local port feels like using a local service.
 */
export function forwardTunnel(opts: TunnelForwardOpts): TunnelForwardHandle {
  const log = opts.log ?? ((_l, m) => console.log(`[teamx forward] ${m}`))
  let closed = false
  const sockets = new Set<Socket>()
  const streams = new Map<number, Socket>()
  // Last known bind result; lets ready() return immediately even when the
  // listener binds before the caller calls ready().
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
    clientSocket.on("close", () => sockets.delete(clientSocket))
    clientSocket.on("error", () => sockets.delete(clientSocket))

    // One WS per local connection.
    const tls = mtlsFor(opts.serverUrl)
    let ws: WebSocket
    try {
      ws = new WebSocket(
        wsUrl(opts.serverUrl).replace(/\/ws$/, "/tunnel/forward"),
        (tls ? { tls } : undefined) as never,
      )
    } catch (e) {
      log("warn", `forward connect error: ${String(e)}`)
      clientSocket.destroy()
      return
    }
    const streamId: number | null = null // assigned on stream_open
    const state = { sid: -1 }
    const pending: Buffer[] = []

    ws.onopen = () => {
      ws.send(JSON.stringify({ type: "connect", name: opts.name }))
    }
    ws.onmessage = (ev) => {
      const data = ev.data
      if (typeof data === "string") {
        let msg: { type?: string; stream_id?: number; message?: string }
        try {
          msg = JSON.parse(data)
        } catch {
          return
        }
        if (msg.type === "stream_open" && typeof msg.stream_id === "number") {
          state.sid = msg.stream_id
          // flush data buffered while the stream was opening
          for (const b of pending) {
            if (ws.readyState !== WebSocket.OPEN) return
            const frame = Buffer.alloc(4 + b.length)
            frame.writeUInt32BE(state.sid)
            b.copy(frame, 4)
            ws.send(frame)
          }
          pending.length = 0
        } else if (msg.type === "error") {
          log("warn", `forward error: ${msg.message ?? "unknown"}`)
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
    }
    ws.onclose = () => {
      streams.delete(state.sid)
      clientSocket.destroy()
    }
    ws.onerror = () => {
      clientSocket.destroy()
    }

    // local socket → WS binary frame (buffer until the stream is open)
    clientSocket.on("data", (buf) => {
      const data = Buffer.isBuffer(buf) ? buf : Buffer.from(buf as unknown as Uint8Array)
      if (state.sid < 0) {
        pending.push(data)
        return
      }
      if (ws.readyState !== WebSocket.OPEN) return
      const frame = Buffer.alloc(4 + data.length)
      frame.writeUInt32BE(state.sid)
      data.copy(frame, 4)
      ws.send(frame)
    })

    clientSocket.on("close", () => {
      try {
        ws.close()
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
        server.off("listening", onOk)
        // Surface the errno so a bind failure is diagnosable.
        log("warn", `bind 127.0.0.1:${port} failed: ${e.code ?? e.message}`)
        resolve(null)
      }
      const onOk = () => {
        server.off("error", onErr)
        // Resolve the ACTUAL bound port: with port=0 the OS assigns an
        // ephemeral port and reporting the requested 0 would be wrong.
        const addr = server.address()
        resolve(typeof addr === "object" && addr !== null ? addr.port : null)
      }
      server.once("error", onErr)
      server.once("listening", onOk)
      server.listen(port, "127.0.0.1")
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

/**
 * CLI entrypoint for `forward`: `bun tunnel.ts forward --server <url> --name
 * <n> [--local-port <p>]`. Resolves ready() and stays resident.
 */
export async function runForwardCli(argv: string[]): Promise<number> {
  const args = [...argv]
  const flag = (name: string): string | undefined => {
    const i = args.indexOf(`--${name}`)
    return i >= 0 && i + 1 < args.length ? args[i + 1] : undefined
  }
  const serverUrl = flag("server") ?? TEAMX_SERVER_URL
  const name = flag("name")
  const localPort = Number(flag("local-port") ?? 0)
  if (!serverUrl || !name) {
    console.error("usage: teamx tunnel forward --server <url> --name <name> [--local-port <port>]")
    return 1
  }
  const handle = forwardTunnel({
    serverUrl,
    name,
    localPort: localPort || undefined,
    log: (level, m) => console[level === "error" ? "error" : "log"](`[teamx forward] ${m}`),
  })
  const port = await handle.ready()
  if (port === null) {
    console.error(`[teamx forward] failed to bind local port for tunnel "${name}"`)
    handle.close()
    return 1
  }
  console.log(JSON.stringify({ ok: true, name, local_port: port }))
  // keep alive forever
  return await new Promise<number>(() => {})
}
