// Comprehensive tunnel + proxy end-to-end test.
//
// Covers every tunnel mode (Frp / Local / Proxy), the full lifecycle
// (register -> relay -> access -> list/status/close -> cleanup), edge cases,
// and the SOCKS5 outbound proxy -- all against a live `teamx serve` with mTLS.
//
// Run with Bun: `bun tests/tunnel-proxy-comprehensive.ts`

import { spawn, spawnSync } from "node:child_process"
import { createServer, IncomingMessage, ServerResponse } from "node:http"
import { createConnection, Socket, createServer as netCreateServer } from "node:net"
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

// -- Config --
const TEAMX = process.env.TEAMX ?? join(import.meta.dir, "../target/debug/teamx")
const PORT = Number(process.env.TEAMX_TEST_PORT ?? 5795)
const ROOT = mkdtempSync(join(tmpdir(), "teamx-comp-"))
const DB = join(ROOT, "t.db")
const HOME = join(ROOT, "home")
mkdirSync(HOME, { recursive: true })

// -- Helpers --
let serveProc: ReturnType<typeof spawn> | null = null
let extraProcs: ReturnType<typeof spawn>[] = []
function cleanup() {
  if (serveProc) serveProc.kill("SIGTERM")
  for (const p of extraProcs) try { p.kill("SIGTERM") } catch {}
  try { rmSync(ROOT, { recursive: true, force: true }) } catch {}
}
process.on("exit", cleanup)

let failures = 0
let passed = 0
function pass(msg: string) { passed++; console.log(`  ok: ${msg}`) }
function fail(msg: string) { failures++; console.log(`FAIL: ${msg}`) }

function teamxCmd(args: string[]): string {
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
  closed = false
  constructor(url: string, tls?: Tls) {
    this.ws = new WebSocket(url, { tls } as any)
    this.ws.onmessage = (ev) => {
      let msg: any
      try { msg = JSON.parse(String(ev.data)) } catch { msg = String(ev.data) }
      const w = this.waiters.shift()
      if (w) w(msg)
      else this.queue.push(msg)
    }
    this.ws.onclose = () => { this.closed = true }
  }
  open(): Promise<void> {
    return new Promise((res, rej) => {
      this.ws.onopen = () => res()
      this.ws.onerror = () => rej(new Error("ws open failed"))
    })
  }
  next(timeoutMs = 5000): Promise<any> {
    if (this.closed) return Promise.reject(new Error("ws already closed"))
    if (this.queue.length > 0) return Promise.resolve(this.queue.shift())
    return new Promise((res, rej) => {
      const t = setTimeout(() => rej(new Error(`timeout ${timeoutMs}ms`)), timeoutMs)
      this.waiters.push((m) => { clearTimeout(t); res(m) })
    })
  }
  send(obj: unknown) { this.ws.send(JSON.stringify(obj)) }
  sendBinary(buf: Uint8Array) { this.ws.send(buf) }
  close() { try { this.ws.close() } catch {} }
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

function startLocalService(body = "hello from local service"): Promise<{ port: number; stop: () => void }> {
  return new Promise((resolve) => {
    const server = createServer((_req: IncomingMessage, res: ServerResponse) => {
      res.writeHead(200, { "Content-Type": "text/plain" })
      res.end(body)
    })
    server.listen(0, "127.0.0.1", () => {
      const addr = server.address() as { port: number }
      resolve({ port: addr.port, stop: () => server.close() })
    })
  })
}

/** Provider-side bridge: intercept open_stream / binary frames on a tunnel WS. */
function bridgeProvider(ws: WsClient, targetPort: number) {
  const streams = new Map<number, Socket>()
  const origOnMessage = ws.ws.onmessage
  function connect(sid: number) {
    const sock = createConnection(targetPort, "127.0.0.1")
    let closed = false
    sock.on("data", (buf: Buffer) => {
      if (closed) return
      const frame = Buffer.alloc(4 + buf.length)
      frame.writeUInt32BE(sid)
      buf.copy(frame, 4)
      ws.sendBinary(new Uint8Array(frame))
    })
    sock.on("close", () => { closed = true; streams.delete(sid) })
    sock.on("error", () => { closed = true; streams.delete(sid) })
    return sock
  }
  ws.ws.onmessage = (ev: any) => {
    const data = ev.data
    if (typeof data === "string") {
      const msg = JSON.parse(data)
      if (msg.type === "open_stream") streams.set(msg.stream_id, connect(msg.stream_id))
      return
    }
    const buf = Buffer.from(data as Uint8Array)
    if (buf.length < 4) return
    const sid = buf.readUInt32BE(0)
    const st = streams.get(sid)
    if (st && !st.destroyed) st.write(buf.subarray(4))
  }
  return {
    destroy() {
      for (const s of streams.values()) { try { s.destroy() } catch {} }
      streams.clear()
      if (origOnMessage) ws.ws.onmessage = origOnMessage as any
    }
  }
}

/** Consumer-side forwarder: listen on a local port; open /tunnel/forward per connection. */
function startForward(tls: Tls, name: string, localPort: number): Promise<{ port: number; stop: () => void }> {
  return new Promise((resolve, reject) => {
    const sockets = new Set<Socket>()
    const server = netCreateServer((clientSocket: any) => {
      const cs = clientSocket as Socket
      sockets.add(cs)
      cs.on("close", () => sockets.delete(cs))
      cs.on("error", () => sockets.delete(cs))
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
          } else if (msg.type === "error") { cs.destroy() }
          return
        }
        const buf = Buffer.from(data as Uint8Array)
        if (buf.length < 4) return
        const sid = buf.readUInt32BE(0)
        if (state.sid >= 0 && sid === state.sid) cs.write(buf.subarray(4))
      }
      ws.onclose = () => cs.destroy()
      ws.onerror = () => cs.destroy()
      cs.on("data", (buf) => {
        const b = Buffer.isBuffer(buf) ? buf : Buffer.from(buf as unknown as Uint8Array)
        if (state.sid < 0) { pending.push(b); return }
        if (ws.readyState !== WebSocket.OPEN) return
        const frame = Buffer.alloc(4 + b.length)
        frame.writeUInt32BE(state.sid)
        b.copy(frame, 4)
        ws.send(frame)
      })
      cs.on("close", () => { try { ws.close() } catch {} })
    })
    server.once("error", (e: NodeJS.ErrnoException) => reject(e))
    server.listen(localPort, "127.0.0.1", () => {
      resolve({ port: localPort, stop: () => { server.close(); for (const s of sockets) { try { s.destroy() } catch {} } } })
    })
  })
}

/** SOCKS5 client: greeting -> CONNECT -> send payload -> read response. */
function socks5Request(socksHost: string, socksPort: number, targetHost: string, targetPort: number, payload: string): Promise<{ status: number; body: string }> {
  return new Promise((resolve, reject) => {
    const sock = createConnection(socksPort, socksHost)
    const timeout = setTimeout(() => { sock.destroy(); reject(new Error("socks5 timeout")) }, 8000)
    let stage = 0
    let resp = Buffer.alloc(0)
    sock.on("connect", () => {
      sock.write(Buffer.from([0x05, 0x01, 0x00]))
      stage = 1
    })
    sock.on("data", (buf: Buffer) => {
      if (stage === 1) {
        if (buf.length < 2 || buf[0] !== 0x05 || buf[1] !== 0x00) {
          sock.destroy(); clearTimeout(timeout); reject(new Error(`socks5 greeting: ${buf.toString("hex")}`)); return
        }
        const hostBuf = Buffer.from(targetHost)
        const req = Buffer.concat([
          Buffer.from([0x05, 0x01, 0x00, 0x03, hostBuf.length]),
          hostBuf,
          Buffer.from([(targetPort >> 8) & 0xff, targetPort & 0xff]),
        ])
        sock.write(req)
        stage = 2
        return
      }
      if (stage === 2) {
        if (buf.length < 10 || buf[0] !== 0x05 || buf[1] !== 0x00) {
          sock.destroy(); clearTimeout(timeout); reject(new Error(`socks5 connect: ${buf.toString("hex")}`)); return
        }
        sock.write(payload)
        stage = 3
        return
      }
      if (stage === 3) {
        resp = Buffer.concat([resp, buf])
        if (resp.includes(Buffer.from("\r\n\r\n"))) {
          clearTimeout(timeout)
          sock.destroy()
          const headerEnd = resp.indexOf("\r\n\r\n") + 4
          const statusLine = resp.subarray(0, resp.indexOf("\r\n")).toString()
          const status = Number(statusLine.split(" ")[1])
          resolve({ status, body: resp.subarray(headerEnd).toString() })
        }
      }
    })
    sock.on("error", (e) => { clearTimeout(timeout); reject(e) })
  })
}

// -- Test harness --
let teamCounter = 0
async function setupTeamAndCerts(teamName: string) {
  const session = `s:${teamName.toLowerCase()}`
  const create = teamxCmd(["team", "create", teamName, "--session", session, "--json"])
  const ownerId = jget(create, ["owner_member_id"]) as string
  const ownerDir = join(ROOT, `${teamName}-owner`)
  mkdirSync(ownerDir)
  teamxCmd(["cert", "issue", ownerId, "owner", "--out", ownerDir, "--json"])

  const invite = teamxCmd(["team", "invite", "contributor: builds the service", "--session", session, "--json"])
  const letter = jget(invite, ["letter"]) as string
  const providerId = jget(invite, ["member_id"]) as string
  const memberDir = join(ROOT, `${teamName}-provider`)
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

  const imp = await fetchRpc(providerTls, "team.import", { letter, name: "Dev" })
  if (imp?.ok !== true) fail(`provider import: ${JSON.stringify(imp)}`)
  const appr = await fetchRpc(ownerTls, "team.approve", { member_id: providerId, session, team: teamId })
  if (appr?.ok !== true) fail(`approve provider: ${JSON.stringify(appr)}`)

  return { ownerTls, providerTls, providerId, teamId, ownerDir, memberDir, session }
}

// ================================================================
// Main
// ================================================================
async function main() {
  teamxCmd(["init"])
  teamxCmd(["cert", "init"])

  const serveEnv = { ...process.env, TEAMX_DB: DB, TEAMX_HOME: HOME } as Record<string, string>
  serveProc = spawn(TEAMX, ["serve", "--addr", "127.0.0.1", "--port", String(PORT)], { env: serveEnv as any, stdio: "ignore" })

  // -- 2. FRP Tunnel --
  console.log("\n=== 2. FRP Tunnel ===")
  {
    const { ownerTls, providerTls, teamId } = await setupTeamAndCerts("FrpTeam")
    await waitReady(ownerTls)
    pass("serve is up")

    const svc = await startLocalService("hello from frp service")
    const ws = new WsClient(`wss://127.0.0.1:${PORT}/tunnel`, providerTls)
    await ws.open()
    ws.send({ type: "register", name: "httpbin", port: svc.port, mode: "frp", lan_ip: "127.0.0.1" })
    const reg = await ws.next()
    if (reg.type !== "registered") { fail(`FRP register: ${JSON.stringify(reg)}`) }
    else {
      const pubPort = reg.port as number
      pass(`FRP tunnel registered on public port ${pubPort}`)

      const bridge = bridgeProvider(ws, svc.port)

      try {
        const body = await fetch(`http://127.0.0.1:${pubPort}/`, { signal: AbortSignal.timeout(5000) }).then(r => r.text())
        if (body === "hello from frp service") pass("consumer reached provider via FRP relay")
        else fail(`FRP relay body mismatch: ${body}`)
      } catch (e) { fail(`FRP relay fetch: ${e}`) }

      const list = await fetchRpc(ownerTls, "tunnel.list", { team: teamId, session: "s:owner" })
      const tunnels = list?.data?.tunnels ?? list?.tunnels ?? []
      if (Array.isArray(tunnels) && tunnels.some((t: any) => t.name === "httpbin" && t.mode === "frp"))
        pass("tunnel.list reports FRP tunnel")
      else fail(`tunnel.list: ${JSON.stringify(list)}`)

      const st = await fetchRpc(ownerTls, "tunnel.status", { team: teamId, name: "httpbin", session: "s:owner" })
      if (st?.data?.port === pubPort && st?.data?.mode === "frp" && st?.data?.lan_ip === "127.0.0.1")
        pass("tunnel.status reports port + mode + lan_ip")
      else fail(`tunnel.status: ${JSON.stringify(st)}`)
      if (st?.data?.same_subnet === true) pass("tunnel.status same_subnet=true (both loopback)")
      else fail(`tunnel.status same_subnet: ${JSON.stringify(st?.data)}`)
      if (st?.data?.direct_addr === `127.0.0.1:${svc.port}`) pass("tunnel.status direct_addr present")
      else fail(`tunnel.status direct_addr: ${JSON.stringify(st?.data)}`)

      const close = await fetchRpc(ownerTls, "tunnel.close", { team: teamId, name: "httpbin", session: "s:owner" })
      if (close?.data?.closed === true) pass("tunnel.close frees the FRP tunnel")
      else fail(`tunnel.close: ${JSON.stringify(close)}`)

      try {
        await fetch(`http://127.0.0.1:${pubPort}/`, { signal: AbortSignal.timeout(3000) })
        fail("public port still reachable after close")
      } catch { pass("public port closed after tunnel.close") }

      bridge.destroy()
    }
    ws.close()
    svc.stop()
  }

  // -- 3. Local Tunnel --
  console.log("\n=== 3. Local Tunnel ===")
  {
    const { ownerTls, providerTls, teamId } = await setupTeamAndCerts("LocalTeam")
    const svc = await startLocalService("hello via local forward")
    const ws = new WsClient(`wss://127.0.0.1:${PORT}/tunnel`, providerTls)
    await ws.open()
    ws.send({ type: "register", name: "httpbin", port: svc.port, mode: "local", lan_ip: "127.0.0.1" })
    const reg = await ws.next()
    if (reg.type !== "registered") { fail(`local register: ${JSON.stringify(reg)}`) }
    else {
      pass(`local tunnel registered (mode=${reg.mode}, port=${reg.port})`)

      const bridge = bridgeProvider(ws, svc.port)

      const list = await fetchRpc(ownerTls, "tunnel.list", { team: teamId, session: "s:owner" })
      const tunnels = list?.data?.tunnels ?? list?.tunnels ?? []
      const t = Array.isArray(tunnels) ? tunnels[0] : undefined
      if (t && t.mode === "local" && t.port === 0) pass("tunnel.list: mode=local, port=0")
      else fail(`tunnel.list local mode: ${JSON.stringify(list)}`)

      const st = await fetchRpc(ownerTls, "tunnel.status", { team: teamId, name: "httpbin", session: "s:owner" })
      if (st?.data?.mode === "local" && st?.data?.port === 0) pass("tunnel.status: mode=local, port=0")
      else fail(`tunnel.status local mode: ${JSON.stringify(st)}`)

      const fwd = await startForward(ownerTls, "httpbin", 18851)
      try {
        const res = await fetch(`http://127.0.0.1:${fwd.port}/`, { signal: AbortSignal.timeout(8000) })
        const body = await res.text()
        if (body === "hello via local forward") pass("consumer reached provider via local forward")
        else fail(`forward body mismatch: ${body}`)
      } catch (e) { fail(`forward fetch: ${e}`) }
      fwd.stop()

      const close = await fetchRpc(ownerTls, "tunnel.close", { team: teamId, name: "httpbin", session: "s:owner" })
      if (close?.data?.closed === true) pass("tunnel.close frees the local-mode tunnel")
      else fail(`tunnel.close: ${JSON.stringify(close)}`)

      bridge.destroy()
    }
    ws.close()
    svc.stop()
  }

  // -- 4. Proxy Tunnel (exit + SOCKS5 consumer) --
  console.log("\n=== 4. Proxy Tunnel ===")
  {
    const { ownerTls, teamId, memberDir } = await setupTeamAndCerts("ProxyTeam")
    const svc = await startLocalService("proxy-ok: reachable via exit")

    const exitEnv = {
      ...process.env, TEAMX_DB: DB, TEAMX_HOME: HOME,
      TEAMX_SERVER_URL: `https://127.0.0.1:${PORT}`,
      TEAMX_MTLS_CERT: join(memberDir, "client.crt"),
      TEAMX_MTLS_KEY: join(memberDir, "client.key"),
      TEAMX_MTLS_CA: join(HOME, "ca", "ca.crt"),
    } as Record<string, string>
    const exitProc = spawn(TEAMX, ["proxy", "exit", "egress"], { env: exitEnv as any })
    extraProcs.push(exitProc)
    await new Promise(r => setTimeout(r, 1200))

    const socksEnv = {
      ...process.env, TEAMX_DB: DB, TEAMX_HOME: HOME,
      TEAMX_SERVER_URL: `https://127.0.0.1:${PORT}`,
      TEAMX_MTLS_CERT: join(ROOT, "ProxyTeam-owner/member.crt"),
      TEAMX_MTLS_KEY: join(ROOT, "ProxyTeam-owner/member.key"),
      TEAMX_MTLS_CA: join(HOME, "ca", "ca.crt"),
    } as Record<string, string>
    const SOCKS_PORT = 11081
    const socksProc = spawn(TEAMX, ["proxy", "start", "--port", String(SOCKS_PORT), "--exit", "egress"], { env: socksEnv as any })
    extraProcs.push(socksProc)
    await new Promise(r => setTimeout(r, 1200))

    const r = await socks5Request("127.0.0.1", SOCKS_PORT, "127.0.0.1", svc.port, "GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
    if (r.status === 200 && r.body === "proxy-ok: reachable via exit") pass("SOCKS5 CONNECT reached exit service")
    else fail(`proxy e2e: status=${r.status} body=${JSON.stringify(r.body)}`)

    const results = await Promise.all([
      socks5Request("127.0.0.1", SOCKS_PORT, "127.0.0.1", svc.port, "GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"),
      socks5Request("127.0.0.1", SOCKS_PORT, "127.0.0.1", svc.port, "GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"),
      socks5Request("127.0.0.1", SOCKS_PORT, "127.0.0.1", svc.port, "GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"),
    ])
    if (results.every(x => x.status === 200 && x.body.includes("proxy-ok"))) pass("3 concurrent SOCKS5 connections succeeded")
    else fail(`concurrent: ${JSON.stringify(results.map(x => x.status))}`)

    const list = await fetchRpc(ownerTls, "tunnel.list", { team: teamId, session: "s:owner" })
    const tunnels = list?.data?.tunnels ?? list?.tunnels ?? []
    const egress = (tunnels as any[]).find(t => t.name === "egress")
    if (egress && egress.mode === "proxy" && egress.port === 0) pass("tunnel.list reports mode=proxy exit")
    else fail(`tunnel.list: ${JSON.stringify(list)}`)

    exitProc.kill("SIGTERM")
    extraProcs = extraProcs.filter(p => p !== exitProc)
    await new Promise(r => setTimeout(r, 800))
    const list2 = await fetchRpc(ownerTls, "tunnel.list", { team: teamId, session: "s:owner" })
    const tunnels2 = list2?.data?.tunnels ?? list2?.tunnels ?? []
    if (!(tunnels2 as any[]).some(t => t.name === "egress")) pass("exit disconnect removed the tunnel")
    else fail(`tunnel still listed after exit: ${JSON.stringify(list2)}`)

    svc.stop()
  }

  // -- 5. Multi-Tunnel (two FRP tunnels on separate WS) --
  console.log("\n=== 5. Multi-Tunnel ===")
  {
    const { ownerTls, providerTls, teamId } = await setupTeamAndCerts("MultiTeam")
    const svcA = await startLocalService("service-a")
    const svcB = await startLocalService("service-b")

    // Two separate WS connections (each tunnel is its own WS)
    const wsA = new WsClient(`wss://127.0.0.1:${PORT}/tunnel`, providerTls)
    await wsA.open()
    wsA.send({ type: "register", name: "svc-a", port: svcA.port, mode: "frp", lan_ip: "127.0.0.1" })
    const regA = await wsA.next()
    if (regA.type !== "registered") fail(`register svc-a: ${JSON.stringify(regA)}`)
    else pass(`svc-a registered on port ${regA.port}`)
    const portA = regA.port as number
    const bridgeA = bridgeProvider(wsA, svcA.port)

    const wsB = new WsClient(`wss://127.0.0.1:${PORT}/tunnel`, providerTls)
    await wsB.open()
    wsB.send({ type: "register", name: "svc-b", port: svcB.port, mode: "frp", lan_ip: "127.0.0.1" })
    const regB = await wsB.next()
    if (regB.type !== "registered") fail(`register svc-b: ${JSON.stringify(regB)}`)
    else pass(`svc-b registered on port ${regB.port}`)
    const portB = regB.port as number
    const bridgeB = bridgeProvider(wsB, svcB.port)

    // Both accessible
    try {
      const bodyA = await fetch(`http://127.0.0.1:${portA}/`, { signal: AbortSignal.timeout(5000) }).then(r => r.text())
      if (bodyA === "service-a") pass("consumer reached svc-a via FRP relay")
      else fail(`svc-a body mismatch: ${bodyA}`)
    } catch (e) { fail(`svc-a fetch: ${e}`) }

    try {
      const bodyB = await fetch(`http://127.0.0.1:${portB}/`, { signal: AbortSignal.timeout(5000) }).then(r => r.text())
      if (bodyB === "service-b") pass("consumer reached svc-b via FRP relay")
      else fail(`svc-b body mismatch: ${bodyB}`)
    } catch (e) { fail(`svc-b fetch: ${e}`) }

    // list shows both
    const list = await fetchRpc(ownerTls, "tunnel.list", { team: teamId, session: "s:owner" })
    const tunnels = list?.data?.tunnels ?? list?.tunnels ?? []
    const names = (tunnels as any[]).map(t => t.name).sort()
    if (names.includes("svc-a") && names.includes("svc-b") && tunnels.length === 2)
      pass("tunnel.list reports both tunnels")
    else fail(`tunnel.list: ${JSON.stringify(list)}`)

    // Close svc-a
    await fetchRpc(ownerTls, "tunnel.close", { team: teamId, name: "svc-a", session: "s:owner" })
    // svc-a port closed
    try {
      await fetch(`http://127.0.0.1:${portA}/`, { signal: AbortSignal.timeout(3000) })
      fail("svc-a port still reachable after close")
    } catch { pass("svc-a port closed after tunnel.close") }
    // svc-b still works
    try {
      const bodyB2 = await fetch(`http://127.0.0.1:${portB}/`, { signal: AbortSignal.timeout(5000) }).then(r => r.text())
      if (bodyB2 === "service-b") pass("svc-b still accessible after svc-a close")
      else fail(`svc-b body after svc-a close: ${bodyB2}`)
    } catch (e) { fail(`svc-b after svc-a close: ${e}`) }
    // list shows only svc-b
    const list2 = await fetchRpc(ownerTls, "tunnel.list", { team: teamId, session: "s:owner" })
    const tunnels2 = list2?.data?.tunnels ?? list2?.tunnels ?? []
    if (tunnels2.length === 1 && (tunnels2 as any[])[0].name === "svc-b") pass("tunnel.list shows only svc-b after close")
    else fail(`tunnel.list after svc-a close: ${JSON.stringify(list2)}`)

    bridgeA.destroy()
    bridgeB.destroy()
    wsA.close()
    wsB.close()
    svcA.stop()
    svcB.stop()
  }

  // -- 6. Edge Cases --
  console.log("\n=== 6. Edge Cases ===")
  {
    const { ownerTls, providerTls, teamId } = await setupTeamAndCerts("EdgeTeam")
    const svc = await startLocalService("edge-service")

    // Register a tunnel
    const ws = new WsClient(`wss://127.0.0.1:${PORT}/tunnel`, providerTls)
    await ws.open()
    ws.send({ type: "register", name: "httpbin", port: svc.port, mode: "frp", lan_ip: "127.0.0.1" })
    const reg = await ws.next()
    if (reg.type === "registered") pass("edge: tunnel registered")
    else fail(`edge register: ${JSON.stringify(reg)}`)
    const bridge = bridgeProvider(ws, svc.port)

    // Duplicate name -> error
    const ws2 = new WsClient(`wss://127.0.0.1:${PORT}/tunnel`, providerTls)
    await ws2.open()
    ws2.send({ type: "register", name: "httpbin", port: svc.port, mode: "frp", lan_ip: "127.0.0.1" })
    const dup = await ws2.next()
    if (dup.type === "error" || dup.message) pass("duplicate name rejected")
    else fail(`duplicate should be rejected: ${JSON.stringify(dup)}`)
    ws2.close()

    // tunnel.close non-existent -> returns closed: false (not an error)
    const badClose = await fetchRpc(ownerTls, "tunnel.close", { team: teamId, name: "nope", session: "s:owner" })
    if (badClose?.data?.closed === false) pass("close non-existent tunnel returns closed: false")
    else fail(`close non-existent: ${JSON.stringify(badClose)}`)

    // tunnel.status non-existent -> should error
    const badStatus = await fetchRpc(ownerTls, "tunnel.status", { team: teamId, name: "nope", session: "s:owner" })
    if (badStatus?.ok === false || badStatus?.data?.error || badStatus?.error) pass("status non-existent tunnel rejected")
    else fail(`status non-existent: ${JSON.stringify(badStatus)}`)

    // Clean up
    bridge.destroy()
    ws.close()
    svc.stop()
  }

  // -- 7. Provider Disconnect Cleanup --
  console.log("\n=== 7. Provider Disconnect ===")
  {
    const { ownerTls, providerTls, teamId } = await setupTeamAndCerts("DisconnectTeam")
    const svc = await startLocalService("disconnect-service")

    const ws = new WsClient(`wss://127.0.0.1:${PORT}/tunnel`, providerTls)
    await ws.open()
    ws.send({ type: "register", name: "httpbin", port: svc.port, mode: "frp", lan_ip: "127.0.0.1" })
    const reg = await ws.next()
    if (reg.type !== "registered") { fail(`disconnect register: ${JSON.stringify(reg)}`); svc.stop(); ws.close(); return }
    const pubPort = reg.port as number
    pass(`disconnect: tunnel registered on port ${pubPort}`)

    const bridge = bridgeProvider(ws, svc.port)

    // Verify it works
    try {
      const body = await fetch(`http://127.0.0.1:${pubPort}/`, { signal: AbortSignal.timeout(5000) }).then(r => r.text())
      if (body === "disconnect-service") pass("disconnect: initial access works")
      else fail(`disconnect initial body: ${body}`)
    } catch (e) { fail(`disconnect initial fetch: ${e}`) }

    // Kill the provider WS
    bridge.destroy()
    ws.close()
    await new Promise(r => setTimeout(r, 800))

    // Tunnel should be removed from list
    const list = await fetchRpc(ownerTls, "tunnel.list", { team: teamId, session: "s:owner" })
    const tunnels = list?.data?.tunnels ?? list?.tunnels ?? []
    if (!(tunnels as any[]).some(t => t.name === "httpbin")) pass("provider disconnect removed tunnel from list")
    else fail(`tunnel still listed after provider disconnect: ${JSON.stringify(list)}`)

    // Public port no longer reachable
    try {
      await fetch(`http://127.0.0.1:${pubPort}/`, { signal: AbortSignal.timeout(3000) })
      fail("public port still reachable after provider disconnect")
    } catch { pass("public port unreachable after provider disconnect") }

    svc.stop()
  }

  // -- 8. Port Pool --
  console.log("\n=== 8. Port Pool ===")
  {
    const { ownerTls, providerTls, teamId } = await setupTeamAndCerts("PortPoolTeam")
    const ports: number[] = []

    for (let i = 0; i < 3; i++) {
      const svc = await startLocalService(`port-pool-${i}`)
      const ws = new WsClient(`wss://127.0.0.1:${PORT}/tunnel`, providerTls)
      await ws.open()
      ws.send({ type: "register", name: `pool-${i}`, port: svc.port, mode: "frp", lan_ip: "127.0.0.1" })
      const reg = await ws.next()
      if (reg.type !== "registered") { fail(`pool register ${i}: ${JSON.stringify(reg)}`); ws.close(); svc.stop(); continue }
      const pubPort = reg.port as number
      ports.push(pubPort)
      pass(`pool-${i}: allocated port ${pubPort}`)

      if (pubPort >= 9100 && pubPort <= 9999) pass(`pool-${i}: port in valid range (9100-9999)`)
      else fail(`pool-${i}: port ${pubPort} out of range`)
    }

    // All ports unique
    const unique = new Set(ports)
    if (unique.size === ports.length) pass(`all ${ports.length} ports are unique`)
    else fail(`duplicate ports: ${JSON.stringify(ports)}`)

    // Close pool-1, re-register, might get same port back
    const closeRes = await fetchRpc(ownerTls, "tunnel.close", { team: teamId, name: "pool-1", session: "s:owner" })
    if (closeRes?.data?.closed === true) pass("pool-1 closed")
    else fail(`pool-1 close: ${JSON.stringify(closeRes)}`)

    const svcNew = await startLocalService("port-pool-reuse")
    const wsNew = new WsClient(`wss://127.0.0.1:${PORT}/tunnel`, providerTls)
    await wsNew.open()
    wsNew.send({ type: "register", name: "pool-reuse", port: svcNew.port, mode: "frp", lan_ip: "127.0.0.1" })
    const regNew = await wsNew.next()
    if (regNew.type !== "registered") fail(`pool-reuse register: ${JSON.stringify(regNew)}`)
    else {
      const newPort = regNew.port as number
      if (newPort >= 9100 && newPort <= 9999) pass(`pool-reuse: port ${newPort} in valid range`)
      else fail(`pool-reuse: port ${newPort} out of range`)
    }

    // Cleanup remaining tunnels
    for (let i = 0; i < 3; i++) {
      if (i !== 1) await fetchRpc(ownerTls, "tunnel.close", { team: teamId, name: `pool-${i}`, session: "s:owner" }).catch(() => {})
    }
    await fetchRpc(ownerTls, "tunnel.close", { team: teamId, name: "pool-reuse", session: "s:owner" }).catch(() => {})
    wsNew.close()
    svcNew.stop()
  }

  // -- Summary --
  console.log(`\n${passed} passed, ${failures} failures`)
  console.log(failures === 0 ? "\nALL TESTS PASS" : `\n${failures} FAILURES`)
  process.exit(failures === 0 ? 0 : 1)
}

main().catch((e) => {
  console.error("FATAL:", e)
  cleanup()
  process.exit(1)
})
