// SOCKS5 outbound proxy (network mode) — end-to-end test.
//
// Verifies over a live `teamx serve` (mTLS):
//   1. member-b registers a proxy exit (mode=proxy, no fixed target)
//   2. member-a starts a local SOCKS5 port (`proxy start --exit`)
//   3. a SOCKS5 client connects through the proxy; member-b dials the
//      requested target (its own local HTTP service) and relays bytes
//   4. tunnel.list reports the proxy exit
//
// The SOCKS5 client is implemented with raw sockets (no curl dependency):
//   - greeting: 05 01 00  -> expect 05 00
//   - CONNECT:  05 01 00 03 <domain> <port>  -> expect success
//   - then send an HTTP GET and read the response
//
// Run with Bun: `bun tests/proxy-test.ts` (TEAMX defaults to ../target/debug/teamx).

import { spawn, spawnSync } from "node:child_process"
import { createServer } from "node:http"
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { connect } from "node:net"

const TEAMX = process.env.TEAMX ?? join(import.meta.dir, "../target/debug/teamx")
const PORT = Number(process.env.TEAMX_TEST_PORT ?? 5793)
const SOCKS_PORT = Number(process.env.TEAMX_SOCKS_PORT ?? 11080)
const ROOT = mkdtempSync(join(tmpdir(), "teamx-proxy-"))
const DB = join(ROOT, "t.db")
const HOME = join(ROOT, "home")
mkdirSync(HOME, { recursive: true })

let serveProc: ReturnType<typeof spawn> | null = null
let socksProc: ReturnType<typeof spawn> | null = null
let exitProc: ReturnType<typeof spawn> | null = null
function cleanup() {
  for (const p of [serveProc, socksProc, exitProc]) if (p) p.kill("SIGTERM")
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

/** member-b's "network-exit-reachable" service (target of the proxy). */
function startLocalService(): Promise<{ port: number; stop: () => void }> {
  return new Promise((resolve) => {
    const server = createServer((_req, res) => {
      res.writeHead(200, { "Content-Type": "text/plain" })
      res.end("proxy-ok: reachable via member-b's exit")
    })
    server.listen(0, "127.0.0.1", () => {
      const addr = server.address() as { port: number }
      resolve({ port: addr.port, stop: () => server.close() })
    })
  })
}

/** Minimal SOCKS5 client over a raw socket. Returns the full response body. */
function socks5Request(socksHost: string, socksPort: number, targetHost: string, targetPort: number, payload: string): Promise<{ status: number; body: string }> {
  return new Promise((resolve, reject) => {
    const sock = connect(socksPort, socksHost)
    const timeout = setTimeout(() => { sock.destroy(); reject(new Error("socks5 timeout")) }, 8000)
    let stage = 0 // 0=greeting sent, 1=connect sent, 2=relay
    let resp = Buffer.alloc(0)
    sock.on("connect", () => {
      sock.write(Buffer.from([0x05, 0x01, 0x00])) // greeting: NO AUTH
      stage = 1
    })
    sock.on("data", (buf: Buffer) => {
      if (stage === 1) {
        // expect 05 00
        if (buf.length < 2 || buf[0] !== 0x05 || buf[1] !== 0x00) {
          sock.destroy(); clearTimeout(timeout); reject(new Error(`socks5 greeting failed: ${buf.toString("hex")}`))
          return
        }
        // CONNECT to domain target
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
        // expect 10-byte success: 05 00 00 01 ...
        if (buf.length < 10 || buf[0] !== 0x05 || buf[1] !== 0x00) {
          sock.destroy(); clearTimeout(timeout); reject(new Error(`socks5 connect failed: ${buf.toString("hex")}`))
          return
        }
        sock.write(payload) // send the HTTP request through the tunnel
        stage = 3
        return
      }
      if (stage === 3) {
        resp = Buffer.concat([resp, buf])
        if (resp.includes(Buffer.from("\r\n\r\n"))) {
          clearTimeout(timeout)
          sock.destroy()
          const headerEnd = resp.indexOf("\r\n\r\n") + 4
          const status = Number(resp.subarray(9, 12).toString())
          resolve({ status, body: resp.subarray(headerEnd).toString() })
        }
      }
    })
    sock.on("error", (e) => { clearTimeout(timeout); reject(e) })
  })
}

async function main() {
  // --- setup team + member certs (owner = consumer a, member b = exit) ---
  teamx(["init"])
  const create = teamx(["team", "create", "ProxyTeam", "--session", "s:owner", "--json"])
  const ownerId = jget(create, ["owner_member_id"]) as string
  const ownerDir = join(ROOT, "owner")
  mkdirSync(ownerDir)
  teamx(["cert", "issue", ownerId, "owner", "--out", ownerDir, "--json"])

  const invite = teamx(["team", "invite", "contributor: provides the proxy exit", "--session", "s:owner", "--json"])
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

  // provider (member b) imports + approved
  const imp = await fetchRpc(providerTls, "team.import", { letter, name: "ExitNode" })
  if (imp?.ok !== true) fail(`provider import: ${JSON.stringify(imp)}`)
  else pass("exit provider imported over RPC")
  const appr = await fetchRpc(ownerTls, "team.approve", { member_id: providerId, session: "s:owner", team: teamId })
  if (appr?.ok !== true) { fail(`approve provider: ${JSON.stringify(appr)}`) }
  else pass("exit provider approved")

  // --- member-b: start the proxy exit (`teamx proxy exit`) ---
  const exitEnv = {
    ...process.env,
    TEAMX_DB: DB,
    TEAMX_HOME: HOME,
    TEAMX_SERVER_URL: `https://127.0.0.1:${PORT}`,
    TEAMX_MTLS_CERT: join(memberDir, "client.crt"),
    TEAMX_MTLS_KEY: join(memberDir, "client.key"),
    TEAMX_MTLS_CA: join(HOME, "ca", "ca.crt"),
  } as Record<string, string>
  exitProc = spawn(TEAMX, ["proxy", "exit", "egress"], { env: exitEnv as any })
  await new Promise((r) => setTimeout(r, 1200))

  // --- member-b's local service (the "network target") ---
  const svc = await startLocalService()

  // --- member-a: start the local SOCKS5 proxy (`teamx proxy start`) ---
  const socksEnv = {
    ...process.env,
    TEAMX_DB: DB,
    TEAMX_HOME: HOME,
    TEAMX_SERVER_URL: `https://127.0.0.1:${PORT}`,
    TEAMX_MTLS_CERT: join(ownerDir, "member.crt"),
    TEAMX_MTLS_KEY: join(ownerDir, "member.key"),
    TEAMX_MTLS_CA: join(HOME, "ca", "ca.crt"),
  } as Record<string, string>
  socksProc = spawn(TEAMX, ["proxy", "start", "--port", String(SOCKS_PORT), "--exit", "egress"], { env: socksEnv as any })
  await new Promise((r) => setTimeout(r, 1200))

  // --- end-to-end: SOCKS5 client on member-a reaches member-b's service ---
  const r = await socks5Request("127.0.0.1", SOCKS_PORT, "127.0.0.1", svc.port, "GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
  if (r.status === 200 && r.body === "proxy-ok: reachable via member-b's exit") {
    pass("SOCKS5 CONNECT reached member-b's service through the proxy (a→server→b)")
  } else {
    fail(`proxy e2e mismatch: status=${r.status} body=${JSON.stringify(r.body)}`)
  }

  // --- concurrent SOCKS5 connections (3 streams) ---
  const results = await Promise.all([
    socks5Request("127.0.0.1", SOCKS_PORT, "127.0.0.1", svc.port, "GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"),
    socks5Request("127.0.0.1", SOCKS_PORT, "127.0.0.1", svc.port, "GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"),
    socks5Request("127.0.0.1", SOCKS_PORT, "127.0.0.1", svc.port, "GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"),
  ])
  if (results.every((x) => x.status === 200 && x.body.includes("proxy-ok"))) pass("3 concurrent SOCKS5 connections all succeeded")
  else fail(`concurrent: ${JSON.stringify(results.map((x) => x.status))}`)

  // --- tunnel.list reports the proxy exit ---
  const list = await fetchRpc(ownerTls, "tunnel.list", { team: teamId, session: "s:owner" })
  const tunnels = list?.data?.tunnels ?? list?.tunnels ?? []
  const egress = (tunnels as any[]).find((t) => t.name === "egress")
  if (egress && egress.mode === "proxy" && egress.port === 0) pass("tunnel.list reports mode=proxy exit (port 0)")
  else fail(`tunnel.list: ${JSON.stringify(list)}`)

  // --- exit WS disconnects -> tunnel removed ---
  exitProc?.kill("SIGTERM")
  exitProc = null
  await new Promise((r) => setTimeout(r, 800))
  const list2 = await fetchRpc(ownerTls, "tunnel.list", { team: teamId, session: "s:owner" })
  const tunnels2 = list2?.data?.tunnels ?? list2?.tunnels ?? []
  if (!(tunnels2 as any[]).some((t) => t.name === "egress")) pass("exit disconnect removed the tunnel")
  else fail(`tunnel still listed after exit disconnect: ${JSON.stringify(list2)}`)

  svc.stop()
  socksProc?.kill("SIGTERM")
  socksProc = null

  console.log(failures === 0 ? "\nALL PROXY TESTS PASS" : `\n${failures} FAILURES`)
  process.exit(failures === 0 ? 0 : 1)
}

main().catch((e) => {
  console.error("FATAL:", e)
  process.exit(1)
})