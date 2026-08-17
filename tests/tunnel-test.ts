// Reverse tunnel (network mode) — end-to-end test.
//
// Verifies over a live `teamx serve` (mTLS):
//   1. a provider registers a tunnel (WS) exposing a local HTTP service
//   2. the server allocates a public TCP port (9000-9999)
//   3. a consumer connects to the public port and reaches the provider's
//      local service through the relay
//   4. tunnel.list / tunnel.status RPC report the registry
//   5. closing the tunnel frees the port
//
// Run with Bun: `bun tests/tunnel-test.ts` (TEAMX defaults to ../target/debug/teamx).

import { spawn, spawnSync } from "node:child_process"
import { createServer } from "node:http"
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { Socket } from "node:net"

const TEAMX = process.env.TEAMX ?? join(import.meta.dir, "../target/debug/teamx")
const PORT = Number(process.env.TEAMX_TEST_PORT ?? 5792)
const ROOT = mkdtempSync(join(tmpdir(), "teamx-tunnel-"))
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

/** Start a local HTTP "service" that echoes a fixed body. */
function startLocalService(): Promise<{ port: number; stop: () => void }> {
  return new Promise((resolve) => {
    const server = createServer((_req, res) => {
      res.writeHead(200, { "Content-Type": "text/plain" })
      res.end("hello from member-b's local service")
    })
    server.listen(0, "127.0.0.1", () => {
      const addr = server.address() as { port: number }
      resolve({ port: addr.port, stop: () => server.close() })
    })
  })
}

async function main() {
  // --- setup team + member certs ---
  teamx(["init"])
  const create = teamx(["team", "create", "Tunnel", "--session", "s:owner", "--json"])
  const ownerId = jget(create, ["owner_member_id"]) as string
  const ownerDir = join(ROOT, "owner")
  mkdirSync(ownerDir)
  teamx(["cert", "issue", ownerId, "owner", "--out", ownerDir, "--json"])

  const invite = teamx(["team", "invite", "contributor: builds the service", "--session", "s:owner", "--json"])
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
  const ownerTls: Tls = {
    cert: readFileSync(join(ownerDir, "member.crt"), "utf8"),
    key: readFileSync(join(ownerDir, "member.key"), "utf8"),
    ca, serverName: "127.0.0.1",
  }
  const providerTls: Tls = {
    cert: readFileSync(join(memberDir, "client.crt"), "utf8"),
    key: readFileSync(join(memberDir, "client.key"), "utf8"),
    ca, serverName: "127.0.0.1",
  }

  const teamId = (jget(create, ["team_id"]) ?? jget(create, ["team", "id"])) as string

  // --- start serve ---
  const serveEnv = { ...process.env, TEAMX_DB: DB, TEAMX_HOME: HOME } as Record<string, string>
  serveProc = spawn(TEAMX, ["serve", "--addr", "127.0.0.1", "--port", String(PORT)], { env: serveEnv as any })
  await waitReady(ownerTls)

  // provider imports the invitation (claims the pending seat) over RPC
  const imp = await fetchRpc(providerTls, "team.import", { letter, name: "Dev" })
  if (imp?.ok !== true) fail(`provider import: ${JSON.stringify(imp)}`)
  else pass("provider imported over RPC")

  // approve the provider
  const appr = await fetchRpc(ownerTls, "team.approve", { member_id: providerId, session: "s:owner", team: teamId })
  if (appr?.ok !== true) { fail(`approve provider: ${JSON.stringify(appr)}`) }
  else pass("provider approved")

  // --- start local service + register tunnel ---
  const svc = await startLocalService()
  const ws = new WsClient(`wss://127.0.0.1:${PORT}/tunnel`, providerTls)
  await ws.open()
  ws.send({ type: "register", name: "httpbin", port: svc.port, lan_ip: "127.0.0.1" })
  const reg = await ws.next()
  if (reg.type !== "registered") { fail(`register: ${JSON.stringify(reg)}`) }
  else {
    pass(`tunnel registered on public port ${reg.port}`)
    const pubPort = reg.port as number

    // --- provider-side relay: bridge the WS tunnel to the local service ---
    // Each incoming WS binary frame is [4B stream_id][payload]. We dial the
    // local service once per stream and relay bytes both ways.
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

    // Intercept incoming WS messages: text control frames + binary data.
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
      // binary: [stream_id][payload] → local socket
      const buf = Buffer.from(data as Uint8Array)
      if (buf.length < 4) return
      const sid = buf.readUInt32BE(0)
      const st = streams.get(sid)
      if (st) st.sock.write(buf.subarray(4))
    }

    // --- consumer (owner) accesses the service through the relay ---
    const body = await fetch(`http://127.0.0.1:${pubPort}/`, { headers: { Host: "httpbin.test" } }).then((r) => r.text())
    if (body === "hello from member-b's local service") pass("consumer reached provider's local service through relay")
    else fail(`relay body mismatch: ${body}`)

    // --- tunnel.list RPC ---
    const list = await fetchRpc(ownerTls, "tunnel.list", { team: teamId, session: "s:owner" })
    const tunnels = list?.data?.tunnels ?? list?.tunnels ?? []
    if (Array.isArray(tunnels) && tunnels.length === 1 && tunnels[0].name === "httpbin") pass("tunnel.list reports the tunnel")
    else fail(`tunnel.list: ${JSON.stringify(list)}`)

    // --- tunnel.status RPC ---
    const st = await fetchRpc(ownerTls, "tunnel.status", { team: teamId, name: "httpbin", session: "s:owner" })
    if (st?.data?.port === pubPort && st?.data?.lan_ip === "127.0.0.1") pass("tunnel.status reports port + lan_ip")
    else fail(`tunnel.status: ${JSON.stringify(st)}`)
    if (st?.data?.same_subnet === true) pass("tunnel.status same_subnet=true (consumer + provider both loopback)")
    else fail(`tunnel.status same_subnet: ${JSON.stringify(st?.data)}`)
    if (st?.data?.direct_addr === `127.0.0.1:${svc.port}`) pass("tunnel.status direct_addr present")
    else fail(`tunnel.status direct_addr: ${JSON.stringify(st?.data)}`)

    // --- close the tunnel ---
    const close = await fetchRpc(ownerTls, "tunnel.close", { team: teamId, name: "httpbin", session: "s:owner" })
    if (close?.data?.closed === true) pass("tunnel.close frees the tunnel")
    else fail(`tunnel.close: ${JSON.stringify(close)}`)

    // --- after close, the public port no longer serves ---
    try {
      await fetch(`http://127.0.0.1:${pubPort}/`)
      fail("public port still reachable after close")
    } catch {
      pass("public port closed after tunnel.close")
    }
  }

  svc.stop()
  ws.close()

  // --- summary ---
  console.log(failures === 0 ? "\nALL TUNNEL TESTS PASS" : `\n${failures} FAILURES`)
  process.exit(failures === 0 ? 0 : 1)
}

main().catch((e) => {
  console.error("FATAL:", e)
  process.exit(1)
})
