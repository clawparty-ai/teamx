// tunnel.ts — reverse-tunnel client (provider side, frp-style).
//
// Runs as a resident subprocess spawned by the plugin. It opens a persistent
// mTLS WebSocket to `teamx serve`'s `/tunnel` endpoint, registers a local
// service, and bridges bytes between the server and the local TCP port:
//
//   consumer ⇄ server TCP relay ⇄ WS (binary frames) ⇄ provider ⇄ local service
//
// Wire format (server ↔ provider):
//   provider → server (text):  {"type":"register","name","port","lan_ip"}
//                              {"type":"unregister","name"}
//   server → provider (text):  {"type":"registered","name","port"}
//                              {"type":"open_stream","stream_id"}
//                              {"type":"error","message"}
//   data (binary, both ways):  [4-byte BE stream_id][payload]

import { connect as tcpConnect, Socket } from "node:net"
import { wsUrl } from "./ws"
import { mtlsFor } from "./client"

export interface TunnelExposeOpts {
  serverUrl: string
  name: string
  /** local port to expose */
  port: number
  /** provider LAN IP for direct-connect hints (optional) */
  lanIp?: string
  log?: (level: "info" | "warn" | "error", message: string) => void
}

export interface TunnelHandle {
  close(): void
  /** resolve once the tunnel is registered and the public port is known */
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
  let readyResolve: ((port: number | null) => void) | null = null
  let readyTimer: ReturnType<typeof setTimeout> | null = null
  const streams = new Map<number, Socket>()

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
    sock.on("close", () => streams.delete(sid))
    sock.on("error", () => streams.delete(sid))
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
          if (readyResolve) {
            readyResolve(msg.port ?? null)
            readyResolve = null
            if (readyTimer) clearTimeout(readyTimer)
          }
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
      if (readyResolve) return new Promise((r) => (readyResolve = r))
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
  const serverUrl = flag("server") ?? process.env.TEAMX_SERVER_URL
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
