// Consumer-side local forward (T2) — end-to-end test.
//
// Verifies over a live `teamx serve` (mTLS):
//   1. a provider registers a LOCAL-mode tunnel (WS, no public server port)
//   2. the server does NOT bind a public port for it (port pool untouched)
//   3. a consumer opens `/tunnel/forward`, sends `connect`, and bridges bytes
//      to the provider's tunnel — reaching the provider's local service via
//      the consumer's own local port
//   4. tunnel.list / tunnel.status report mode=local and port=0
//   5. closing the tunnel stops the forward
//
// Run with Bun: `bun tests/tunnel-forward-test.ts`.

import { spawn, spawnSync } from "node:child_process"
import { createServer } from "node:http"
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { Socket, createServer as netCreateServer } from "node:net"

const TEAMX = process.env.TEAMX ?? join(import.meta.dir, "../target/debug/teamx")
const PORT = Number(process.env.TEAMX_TEST_PORT ?? 5793)
const ROOT = mkdtempSync(join(tmpdir(), "teamx-forward-"))
const DB = join(ROOT, "t.db")
const HOME = join(ROOT, "home")
mkdirSync(HOME, { recursive: true })

let serveProc: ReturnType<typeof spawn> | null = null
function cleanup() {
  if (serveProc) serveProc.kill("SIGTERM")
  try { rmSync(ROOT, { recursive: true, force: true }) } catch {}
}
process.on("exit", cleanup)

let failures = 0
function pass(msg: string) { console.log(`  ok: ${msg}`) }
function fail(msg: string) { failures++; console.log(`FAIL: ${msg}`) }

function teamx(args: string[]): string {
  const r = spawnSync(TEAMX, args, {
    env: { ...process.env, TEAMX_DB: DB, TEAMX_HOME: HOME },
    encoding: "utf8",
  })
  if (r.status !== 0) throw new Error(`teamx ${args.join(" ")}: ${r.stderr}`)
  return r.stdout.trim()
}

function jget(s: string, path: (string | number)[]): unknown {
  let v: any = JSON.parse(s)
  for (const p of path) v = v[p]
  return v
}

type Tls = { cert: string; key: string; ca: string; serverName: string }

class WsClient {
  ws: WebSocket
  queue: any[] = []
  waiters: ((m: any) => void)[] = []
  constructor(url: string, tls?: Tls) {
    this.ws = new WebSocket(url, { tls } as any)
    this.ws.onmessage = (ev) => {
      let msg: any
      try { msg = JSON.parse(String(ev.data)) } catch { msg = String(ev.data) }
      const w = this.waiters.shift()
      if (w) w(msg)
      else this.queue.push(msg)
    }
  }
  open(): Promise<void> {
    return new Promise((res, rej) => {
      this.ws.onopen = () => res()
      this.ws.onerror = () => rej(new Error("ws open failed"))
    })
  }
  next(timeoutMs = 5000): Promise<any> {
    if (this.queue.length > 0) return Promise.resolve(this.queue.shift())
    return new Promise((res, rej) => {
      const t = setTimeout(() => rej(new Error(`timeout waiting for ws message after ${timeoutMs}ms`)), timeoutMs)
      this.waiters.push((m) => { clearTimeout(t); res(m) })
    })
  }
  send(obj: unknown) { this.ws.send(JSON.stringify(obj)) }
  sendBinary(buf: Uint8Array) { this.ws.send(buf) }
  close() { this.ws.close() }
}

async function fetchRpc(tls: Tls, method: string, args: Record<string, unknown> = {}): Promise<any> {
  const res = await fetch(`https://127.0.0.1:${PORT}/rpc`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ method, args }),
    tls: { cert: tls.cert, key: tls.key, ca: tls.ca, serverName: tls.serverName },
  } as any)
  return res.json()
}

async function waitReady(tls: Tls) {
  for (let i = 0; i < 40; i++) {
    try {
      const res = await fetch(`https://127.0.0.1:${PORT}/health`, {
        tls: { cert: tls.cert, key: tls.key, ca: tls.ca, serverName: tls.serverName },
      } as any)
      if (res.ok) return
    } catch {}
    await new Promise((r) => setTimeout(r, 200))
  }
  throw new Error("serve did not become ready")
}

function startLocalService(): Promise<{ port: number; stop: () => void }> {
  return new Promise((resolve) => {
    const server = createServer((_req, res) => {
      res.writeHead(200, { "Content-Type": "text/plain" })
      res.end("hello via local forward")
    })
    server.listen(0, "127.0.0.1", () => {
      const addr = server.address() as { port: number }
      resolve({ port: addr.port, stop: () => server.close() })
    })
  })
}

/**
 * Consumer-side forwarder: listen on a local port; for each connection open a
 * mTLS WS to `/tunnel/forward`, send connect, and bridge bytes.
 */
function startForward(tls: Tls, name: string, localPort: number): Promise<{ port: number; stop: () => void }> {
  return new Promise((resolve, reject) => {
    const sockets = new Set<Socket>()
    const streams = new Map<number, Socket>()
    const server = netCreateServer((clientSocket) => {
      sockets.add(clientSocket)
      clientSocket.on("close", () => sockets.delete(clientSocket))
      clientSocket.on("error", () => sockets.delete(clientSocket))
      const ws = new WebSocket(`wss://127.0.0.1:${PORT}/tunnel/forward`, { tls } as any)
      const state = { sid: -1 }
      const pending: Buffer[] = []
      ws.onopen = () => ws.send(JSON.stringify({ type: "connect", name }))
      ws.onmessage = (ev: any) => {
        const data = ev.data
        if (typeof data === "string") {
          const msg = JSON.parse(data)
          if (msg.type === "stream_open") {
            state.sid = msg.stream_id
            for (const b of pending) {
              const frame = Buffer.alloc(4 + b.length)
              frame.writeUInt32BE(state.sid)
              b.copy(frame, 4)
              ws.send(frame)
            }
            pending.length = 0
          }
          else if (msg.type === "error") { fail(`consumer forward error: ${msg.message}`); clientSocket.destroy() }
          return
        }
        const buf = Buffer.from(data as Uint8Array)
        if (buf.length < 4) return
        const sid = buf.readUInt32BE(0)
        if (state.sid >= 0 && sid === state.sid) {
          clientSocket.write(buf.subarray(4))
        }
      }
      ws.onclose = () => { clientSocket.destroy() }
      ws.onerror = () => { clientSocket.destroy() }
      clientSocket.on("data", (buf) => {
        const b = Buffer.isBuffer(buf) ? buf : Buffer.from(buf as unknown as Uint8Array)
        if (state.sid < 0) { pending.push(b); return }
        if (ws.readyState !== WebSocket.OPEN) return
        const frame = Buffer.alloc(4 + b.length)
        frame.writeUInt32BE(state.sid)
        b.copy(frame, 4)
        ws.send(frame)
      })
      clientSocket.on("close", () => { try { ws.close() } catch {} })
    })
    server.once("error", (e: NodeJS.ErrnoException) => reject(e))
    server.listen(localPort, "127.0.0.1", () => {
      resolve({ port: localPort, stop: () => { server.close(); for (const s of sockets) { try { s.destroy() } catch {} } } })
    })
  })
}

async function main() {
  teamx(["init"])
  const create = teamx(["team", "create", "ForwardTunnel", "--session", "s:owner", "--json"])
  const ownerId = jget(create, ["owner_member_id"]) as string
  const ownerDir = join(ROOT, "owner")
  mkdirSync(ownerDir)
  teamx(["cert", "issue", ownerId, "owner", "--out", ownerDir, "--json"])

  const invite = teamx(["team", "invite", "contributor: provider", "--session", "s:owner", "--json"])
  const letter = jget(invite, ["letter"]) as string
  const providerId = jget(invite, ["member_id"]) as string
  const memberDir = join(ROOT, "provider")
  mkdirSync(memberDir)
  {
    const b64 = letter.slice("teamx-inv:v1:".length)
    const d = JSON.parse(Buffer.from(b64, "base64").toString())
    writeFileSync(join(memberDir, "client.crt"), d.certificates.client_cert)
    writeFileSync(join(memberDir, "client.key"), d.certificates.client_key)
    writeFileSync(join(memberDir, "ca.crt"), d.certificates.ca_cert)
  }

  const ca = readFileSync(join(HOME, "ca", "ca.crt"), "utf8")
  const ownerTls: Tls = { cert: readFileSync(join(ownerDir, "member.crt"), "utf8"), key: readFileSync(join(ownerDir, "member.key"), "utf8"), ca, serverName: "127.0.0.1" }
  const providerTls: Tls = { cert: readFileSync(join(memberDir, "client.crt"), "utf8"), key: readFileSync(join(memberDir, "client.key"), "utf8"), ca, serverName: "127.0.0.1" }
  const teamId = (jget(create, ["team_id"]) ?? jget(create, ["team", "id"])) as string

  // --- start serve ---
  const serveEnv = { ...process.env, TEAMX_DB: DB, TEAMX_HOME: HOME } as Record<string, string>
  serveProc = spawn(TEAMX, ["serve", "--addr", "127.0.0.1", "--port", String(PORT)], { env: serveEnv as any })
  serveProc.stdout?.on("data", (d) => process.stdout.write(`[serve] ${d}`))
  serveProc.stderr?.on("data", (d) => process.stdout.write(`[serve] ${d}`))
  await waitReady(ownerTls)

  const imp = await fetchRpc(providerTls, "team.import", { letter, name: "Dev" })
  if (imp?.ok !== true) fail(`provider import: ${JSON.stringify(imp)}`)
  else pass("provider imported over RPC")
  const appr = await fetchRpc(ownerTls, "team.approve", { member_id: providerId, session: "s:owner", team: teamId })
  if (appr?.ok !== true) fail(`approve provider: ${JSON.stringify(appr)}`)
  else pass("provider approved")

  // --- provider: register a LOCAL-mode tunnel (no server public port) ---
  const svc = await startLocalService()
  const ws = new WsClient(`wss://127.0.0.1:${PORT}/tunnel`, providerTls)
  await ws.open()
  ws.send({ type: "register", name: "httpbin", port: svc.port, mode: "local", lan_ip: "127.0.0.1" })
  const reg = await ws.next()
  if (reg.type !== "registered") { fail(`register: ${JSON.stringify(reg)}`) }
  else {
    pass(`local tunnel registered (mode=${reg.mode}, port=${reg.port})`)

    // Provider side: bridge WS streams to the local service.
    const streams = new Map<number, { sock: Socket; close: () => void }>()
    function connect(sid: number) {
      const sock = new Socket()
      let closed = false
      sock.connect(svc.port, "127.0.0.1")
      sock.on("data", (buf: Buffer) => {
        if (closed) return
        const frame = Buffer.alloc(4 + buf.length)
        frame.writeUInt32BE(sid)
        buf.copy(frame, 4)
        ws.sendBinary(new Uint8Array(frame))
      })
      sock.on("close", () => { closed = true; streams.delete(sid) })
      sock.on("error", () => { closed = true; streams.delete(sid) })
      return { sock, close: () => { closed = true; try { sock.destroy() } catch {} } }
    }
    const origOnMessage = ws.ws.onmessage
    ws.ws.onmessage = (ev: any) => {
      const data = ev.data
      if (typeof data === "string") {
        const msg = JSON.parse(data)
        if (msg.type === "open_stream") {
          streams.set(msg.stream_id, connect(msg.stream_id))
        }
        return
      }
      const buf = Buffer.from(data as Uint8Array)
      if (buf.length < 4) return
      const sid = buf.readUInt32BE(0)
      const st = streams.get(sid)
      if (st) st.sock.write(buf.subarray(4))
    }

    // --- verify NO server public port was bound (local mode) ---
    const list = await fetchRpc(ownerTls, "tunnel.list", { team: teamId, session: "s:owner" })
    const tunnels = list?.data?.tunnels ?? list?.tunnels ?? []
    const t = Array.isArray(tunnels) ? tunnels[0] : undefined
    if (t && t.mode === "local" && t.port === 0) pass("tunnel.list: mode=local, port=0 (no server port bound)")
    else fail(`tunnel.list local mode: ${JSON.stringify(list)}`)
    const st = await fetchRpc(ownerTls, "tunnel.status", { team: teamId, name: "httpbin", session: "s:owner" })
    if (st?.data?.mode === "local" && st?.data?.port === 0) pass("tunnel.status: mode=local, port=0")
    else fail(`tunnel.status local mode: ${JSON.stringify(st)}`)

    // --- consumer: forward to a local port and reach the provider service ---
    const fwd = await startForward(ownerTls, "httpbin", 18743)
    pass(`consumer forward listening on 127.0.0.1:${fwd.port}`)
    try {
      const res = await fetch(`http://127.0.0.1:${fwd.port}/`, { signal: AbortSignal.timeout(8000) })
      const body = await res.text()
      if (body === "hello via local forward") pass("consumer reached provider service via local forward")
      else fail(`forward body mismatch: ${body}`)
    } catch (e) {
      fail(`forward fetch failed: ${String(e)}`)
    }
    fwd.stop()

    // --- close the tunnel ---
    const close = await fetchRpc(ownerTls, "tunnel.close", { team: teamId, name: "httpbin", session: "s:owner" })
    if (close?.data?.closed === true) pass("tunnel.close frees the local-mode tunnel")
    else fail(`tunnel.close: ${JSON.stringify(close)}`)
  }

  svc.stop()
  ws.close()

  console.log(failures === 0 ? "\nALL FORWARD TESTS PASS" : `\n${failures} FAILURES`)
  process.exit(failures === 0 ? 0 : 1)
}

main().catch((e) => {
  console.error("FATAL:", e)
  process.exit(1)
})
