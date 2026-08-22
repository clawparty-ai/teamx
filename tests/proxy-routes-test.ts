// Proxy per-target egress routing (`proxy start --routes`) — end-to-end test.
//
// Verifies over a live `teamx serve` (mTLS):
//   1. member-b1 registers proxy exit `egress`  (serves an IPv4 service svc-a)
//   2. member-b2 registers proxy exit `egress2` (serves an IPv6 service svc-b)
//   3. member-a starts a SOCKS5 port with a route table:
//        default=egress,  "::1" -> egress2
//   4. SOCKS5 CONNECT to ::1         -> routed to egress2 -> returns svc-b
//      SOCKS5 CONNECT to 127.0.0.1   -> default egress  -> returns svc-a
//   5. regression: `proxy start --exit egress2` (no --routes) still uses the
//      fixed exit
//
// Route matching logic is unit-tested in routes.rs; this proves the table is
// wired through the CLI into per-connection exit selection.
//
// Run with Bun: `bun tests/proxy-routes-test.ts`.

import { spawn, spawnSync } from "node:child_process"
import { createServer } from "node:http"
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { connect } from "node:net"

const TEAMX = process.env.TEAMX ?? join(import.meta.dir, "../target/debug/teamx")
// random base port to avoid collisions between runs / parallel tests
const PORT = Number(process.env.TEAMX_TEST_PORT ?? 12000 + Math.floor(Math.random() * 500))
const SOCKS_PORT = Number(process.env.TEAMX_SOCKS_PORT ?? 13000 + Math.floor(Math.random() * 500))
const ROOT = mkdtempSync(join(tmpdir(), "teamx-routes-"))
const DB = join(ROOT, "t.db")
const HOME = join(ROOT, "home")
mkdirSync(HOME, { recursive: true })

let serveProc: ReturnType<typeof spawn> | null = null
let socksProc: ReturnType<typeof spawn> | null = null
let socksFixedProc: ReturnType<typeof spawn> | null = null
let exitProcs: ReturnType<typeof spawn>[] = []
function cleanup() {
  for (const p of [serveProc, socksProc, socksFixedProc, ...exitProcs]) if (p) p.kill("SIGTERM")
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

/** A local HTTP service listening on 127.0.0.1 (IPv4) or ::1 (IPv6). */
function startService(host: string, body: string): Promise<{ port: number; stop: () => void }> {
  return new Promise((resolve) => {
    const server = createServer((_req, res) => {
      res.writeHead(200, { "Content-Type": "text/plain" })
      res.end(body)
    })
    server.listen(0, host, () => {
      const addr = server.address() as { port: number }
      resolve({ port: addr.port, stop: () => server.close() })
    })
  })
}

/** SOCKS5 client targeting an IPv4 address. */
function socks5ToIPv4(socksPort: number, targetPort: number, payload: string): Promise<{ status: number; body: string }> {
  return new Promise((resolve, reject) => {
    const sock = connect(socksPort, "127.0.0.1")
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
          sock.destroy(); clearTimeout(timeout); reject(new Error(`socks5 greeting failed: ${buf.toString("hex")}`))
          return
        }
        const req = Buffer.concat([
          Buffer.from([0x05, 0x01, 0x00, 0x01]),
          Buffer.from([127, 0, 0, 1]),
          Buffer.from([(targetPort >> 8) & 0xff, targetPort & 0xff]),
        ])
        sock.write(req)
        stage = 2
        return
      }
      if (stage === 2) {
        if (buf.length < 10 || buf[0] !== 0x05 || buf[1] !== 0x00) {
          sock.destroy(); clearTimeout(timeout); reject(new Error(`socks5 connect failed: ${buf.toString("hex")}`))
          return
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
          const status = Number(resp.subarray(9, 12).toString())
          resolve({ status, body: resp.subarray(headerEnd).toString() })
        }
      }
    })
    sock.on("error", (e) => { clearTimeout(timeout); reject(e) })
  })
}

/** SOCKS5 client targeting ::1 (IPv6). */
function socks5ToV6(socksPort: number, targetPort: number, payload: string): Promise<{ status: number; body: string }> {
  return new Promise((resolve, reject) => {
    const sock = connect(socksPort, "127.0.0.1")
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
          sock.destroy(); clearTimeout(timeout); reject(new Error(`socks5 greeting failed: ${buf.toString("hex")}`))
          return
        }
        // ::1 -> 16 bytes: all zero, last byte 1
        const v6 = Buffer.alloc(16)
        v6[15] = 1
        const req = Buffer.concat([
          Buffer.from([0x05, 0x01, 0x00, 0x04]),
          v6,
          Buffer.from([(targetPort >> 8) & 0xff, targetPort & 0xff]),
        ])
        sock.write(req)
        stage = 2
        return
      }
      if (stage === 2) {
        if (buf.length < 10 || buf[0] !== 0x05 || buf[1] !== 0x00) {
          sock.destroy(); clearTimeout(timeout); reject(new Error(`socks5 connect failed (v6): ${buf.toString("hex")}`))
          return
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
          const status = Number(resp.subarray(9, 12).toString())
          resolve({ status, body: resp.subarray(headerEnd).toString() })
        }
      }
    })
    sock.on("error", (e) => { clearTimeout(timeout); reject(e) })
  })
}

async function main() {
  // --- setup team + owner cert ---
  teamx(["init"])
  const create = teamx(["team", "create", "RoutesTeam", "--session", "s:owner", "--json"])
  const ownerId = jget(create, ["owner_member_id"]) as string
  const ownerDir = join(ROOT, "owner")
  mkdirSync(ownerDir)
  teamx(["cert", "issue", ownerId, "owner", "--out", ownerDir, "--json"])

  // two providers: egress and egress2
  const providerIds: Record<string, string> = {}
  const memberDirs: Record<string, string> = {}
  const providerTls: Record<string, Tls> = {}
  const letters: Record<string, string> = {}
  for (const name of ["egress", "egress2"]) {
    const invite = teamx(["team", "invite", `contributor: exit ${name}`, "--session", "s:owner", "--json"])
    const letter = jget(invite, ["letter"]) as string
    const memberId = jget(invite, ["member_id"]) as string
    providerIds[name] = memberId
    letters[name] = letter
    const dir = join(ROOT, `provider-${name}`)
    mkdirSync(dir)
    memberDirs[name] = dir
    const b64 = letter.slice("teamx-inv:v1:".length)
    const d = JSON.parse(Buffer.from(b64, "base64").toString())
    writeFileSync(join(dir, "client.crt"), d.certificates.client_cert)
    writeFileSync(join(dir, "client.key"), d.certificates.client_key)
    writeFileSync(join(dir, "ca.crt"), d.certificates.ca_cert)
    providerTls[name] = {
      cert: d.certificates.client_cert, key: d.certificates.client_key,
      ca: d.certificates.ca_cert, serverName: "127.0.0.1",
    }
  }

  const ca = readFileSync(join(HOME, "ca", "ca.crt"), "utf8")
  const ownerTls: Tls = {
    cert: readFileSync(join(ownerDir, "member.crt"), "utf8"),
    key: readFileSync(join(ownerDir, "member.key"), "utf8"),
    ca, serverName: "127.0.0.1",
  }
  const teamId = (jget(create, ["team_id"]) ?? jget(create, ["team", "id"])) as string

  // --- start serve ---
  serveProc = spawn(TEAMX, ["serve", "--addr", "127.0.0.1", "--port", String(PORT)], {
    env: { ...process.env, TEAMX_DB: DB, TEAMX_HOME: HOME } as any,
  })
  await waitReady(ownerTls)

  // import + approve both providers
  for (const [name, id] of Object.entries(providerIds)) {
    const imp = await fetchRpc(providerTls[name], "team.import", { letter: letters[name], name: `Exit-${name}` })
    if (imp?.ok !== true) fail(`provider ${name} import: ${JSON.stringify(imp)}`)
    const appr = await fetchRpc(ownerTls, "team.approve", { member_id: id, session: "s:owner", team: teamId })
    if (appr?.ok !== true) fail(`approve ${name}: ${JSON.stringify(appr)}`)
    else pass(`provider ${name} imported + approved`)
  }

  // --- services: svc-a on 127.0.0.1 (via egress), svc-b on ::1 (via egress2) ---
  const svcA = await startService("127.0.0.1", "ROUTE-OK-A")
  const svcB = await startService("::1", "ROUTE-OK-B")

  // --- start both exits ---
  for (const name of ["egress", "egress2"]) {
    const env = {
      ...process.env,
      TEAMX_DB: DB, TEAMX_HOME: HOME,
      TEAMX_SERVER_URL: `https://127.0.0.1:${PORT}`,
      TEAMX_MTLS_CERT: join(memberDirs[name], "client.crt"),
      TEAMX_MTLS_KEY: join(memberDirs[name], "client.key"),
      TEAMX_MTLS_CA: join(HOME, "ca", "ca.crt"),
    } as Record<string, string>
    exitProcs.push(spawn(TEAMX, ["proxy", "exit", name], { env: env as any }))
  }
  await new Promise((r) => setTimeout(r, 1500))

  // --- routes table: ::1 -> egress2, default -> egress ---
  const routesFile = join(ROOT, "routes.json")
  writeFileSync(routesFile, JSON.stringify({ default: "egress", rules: [{ match: "::1", exit: "egress2" }] }))

  // --- start routed SOCKS5 proxy ---
  const socksEnv = {
    ...process.env,
    TEAMX_DB: DB, TEAMX_HOME: HOME,
    TEAMX_SERVER_URL: `https://127.0.0.1:${PORT}`,
    TEAMX_MTLS_CERT: join(ownerDir, "member.crt"),
    TEAMX_MTLS_KEY: join(ownerDir, "member.key"),
    TEAMX_MTLS_CA: join(HOME, "ca", "ca.crt"),
  } as Record<string, string>
  socksProc = spawn(TEAMX, ["proxy", "start", "--port", String(SOCKS_PORT), "--exit", "egress", "--routes", routesFile], { env: socksEnv as any })
  await new Promise((r) => setTimeout(r, 1200))

  const payload = "GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"

  // --- 1. ::1 (matches route) -> egress2 -> svc-b ---
  const toB = await socks5ToV6(SOCKS_PORT, svcB.port, payload)
  if (toB.status === 200 && toB.body === "ROUTE-OK-B") pass("CONNECT ::1 routed to egress2 -> svc-b")
  else fail(`::1 route mismatch: status=${toB.status} body=${JSON.stringify(toB.body)}`)

  // --- 2. 127.0.0.1 (no rule) -> default egress -> svc-a ---
  const toA = await socks5ToIPv4(SOCKS_PORT, svcA.port, payload)
  if (toA.status === 200 && toA.body === "ROUTE-OK-A") pass("CONNECT 127.0.0.1 default -> egress -> svc-a")
  else fail(`127.0.0.1 default mismatch: status=${toA.status} body=${JSON.stringify(toA.body)}`)

  // --- 3. regression: fixed --exit (no routes) still uses that exit ---
  const socksFixedPort = SOCKS_PORT + 1
  socksFixedProc = spawn(TEAMX, ["proxy", "start", "--port", String(socksFixedPort), "--exit", "egress2"], { env: socksEnv as any })
  await new Promise((r) => setTimeout(r, 1800))
  // small retry: the exit's WS + local dial can take a beat to be ready
  let fixedToB = { status: -1, body: "" }
  for (let attempt = 0; attempt < 4 && fixedToB.status !== 200; attempt++) {
    try { fixedToB = await socks5ToV6(socksFixedPort, svcB.port, payload) } catch {}
    if (fixedToB.status !== 200) await new Promise((r) => setTimeout(r, 700))
  }
  if (fixedToB.status === 200 && fixedToB.body === "ROUTE-OK-B") pass("fixed --exit egress2 (no routes) -> svc-b")
  else fail(`fixed --exit regression mismatch: status=${fixedToB.status} body=${JSON.stringify(fixedToB.body)}`)

  // --- 4. SQLite-backed routes: configure via `proxy routes` then start
  //        with no --exit / no -f; the table is read from the DB ---
  const sqliteSocksPort = SOCKS_PORT + 2
  teamx(["proxy", "routes", "set-default", "egress"])
  teamx(["proxy", "routes", "add", "::1", "egress2"])
  socksFixedProc = spawn(TEAMX, ["proxy", "start", "--port", String(sqliteSocksPort)], { env: socksEnv as any })
  await new Promise((r) => setTimeout(r, 1500))
  let sqliteToB = { status: -1, body: "" }
  for (let attempt = 0; attempt < 3 && sqliteToB.status !== 200; attempt++) {
    try { sqliteToB = await socks5ToV6(sqliteSocksPort, svcB.port, payload) } catch {}
    if (sqliteToB.status !== 200) await new Promise((r) => setTimeout(r, 500))
  }
  if (sqliteToB.status === 200 && sqliteToB.body === "ROUTE-OK-B") pass("SQLite routes (no --exit/-f): ::1 -> egress2 -> svc-b")
  else fail(`SQLite routes ::1 mismatch: status=${sqliteToB.status} body=${JSON.stringify(sqliteToB.body)}`)
  const sqliteToA = await socks5ToIPv4(sqliteSocksPort, svcA.port, payload)
  if (sqliteToA.status === 200 && sqliteToA.body === "ROUTE-OK-A") pass("SQLite routes (no --exit/-f): default egress -> svc-a")
  else fail(`SQLite routes default mismatch: status=${sqliteToA.status} body=${JSON.stringify(sqliteToA.body)}`)

  svcA.stop(); svcB.stop()
  console.log(failures === 0 ? "\nALL PROXY ROUTES TESTS PASS" : `\n${failures} FAILURES`)
  process.exit(failures === 0 ? 0 : 1)
}

main().catch((e) => {
  console.error("FATAL:", e)
  process.exit(1)
})
