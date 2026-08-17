// Network mode N1 — WebSocket push test.
//
// Verifies over a live `teamx serve` (mTLS):
//   1. register: an owner + a member WS client both receive `registered`.
//   2. fan-out: an owner RPC publish is pushed to the member's WS in real time.
//   3. heartbeat: the server emits `ping` (interval shortened via env).
//   4. the /health endpoint reports the live connection count.
//
// Run with Bun: `bun tests/ws-test.ts` (TEAMX bin defaults to ../target/debug/teamx).

import { spawn, spawnSync } from "node:child_process"
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

const TEAMX = process.env.TEAMX ?? join(import.meta.dir, "../target/debug/teamx")
const PORT = Number(process.env.TEAMX_TEST_PORT ?? 5791)
const ROOT = mkdtempSync(join(tmpdir(), "teamx-ws-"))
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

/** A small promise-based wrapper around Bun's WebSocket with mTLS. */
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

async function main() {
  // --- setup ---
  teamx(["init"])
  const create = teamx(["team", "create", "Ws", "--session", "s:owner", "--json"])
  const ownerId = jget(create, ["owner_member_id"]) as string
  const ownerDir = join(ROOT, "owner")
  mkdirSync(ownerDir)
  teamx(["cert", "issue", ownerId, "owner", "--out", ownerDir, "--json"])

  const invite = teamx(["team", "invite", "reviewer: code review", "--session", "s:owner", "--json"])
  const letter = jget(invite, ["letter"]) as string
  const memberId = jget(invite, ["member_id"]) as string
  const invitationId = jget(invite, ["invitation_id"]) as string

  // extract the member cert/key/ca from the letter
  const memberDir = join(ROOT, "member")
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
    ca,
    serverName: "127.0.0.1",
  }
  const memberTls: Tls = {
    cert: readFileSync(join(memberDir, "client.crt"), "utf8"),
    key: readFileSync(join(memberDir, "client.key"), "utf8"),
    ca: readFileSync(join(memberDir, "ca.crt"), "utf8"),
    serverName: "127.0.0.1",
  }

  // --- start serve (short heartbeat for the test) ---
  serveProc = spawn(TEAMX, ["serve", "--addr", "127.0.0.1", "--port", String(PORT)], {
    env: { ...process.env, TEAMX_DB: DB, TEAMX_HOME: HOME, TEAMX_WS_HEARTBEAT_SECS: "1" },
    stdio: "ignore",
  })
  await waitReady(ownerTls)
  pass("serve is up")

  // The member must first import (claim the pending seat) over RPC so they
  // have a team to subscribe to.
  const imp = await fetchRpc(memberTls, "team.import", { letter, name: "Alice" })
  if (imp?.ok !== true) fail(`team.import over RPC: ${JSON.stringify(imp)}`)
  else pass("member imported over RPC")

  // --- register both clients ---
  const ownerWs = new WsClient(`wss://127.0.0.1:${PORT}/ws`, ownerTls)
  await ownerWs.open()
  const ownerReg = await ownerWs.next()
  if (ownerReg.type === "registered" && ownerReg.member_id === ownerId) pass("owner WS registered")
  else fail(`owner registered: ${JSON.stringify(ownerReg)}`)

  const memberWs = new WsClient(`wss://127.0.0.1:${PORT}/ws`, memberTls)
  await memberWs.open()
  const memberReg = await memberWs.next()
  if (memberReg.type === "registered" && memberReg.member_id === memberId) pass("member WS registered")
  else fail(`member registered: ${JSON.stringify(memberReg)}`)

  // --- real-time fan-out: owner broadcasts, member receives over WS ---
  const pub = await fetchRpc(ownerTls, "publish", { type: "decision", data: '{"message":"ws fanout works"}' })
  if (pub?.ok !== true) fail(`publish: ${JSON.stringify(pub)}`)
  else {
    const ev = await memberWs.next()
    if (ev.type === "event" && ev.event?.type === "decision.broadcast") pass("member received broadcast event in real time")
    else fail(`member event frame: ${JSON.stringify(ev)}`)
  }

  // --- heartbeat: server emits ping within ~3s (interval=1s) ---
  const hb = await memberWs.next(4000)
  if (hb.type === "ping") pass("heartbeat ping received")
  else fail(`heartbeat: ${JSON.stringify(hb)}`)

  // --- health reports live connections ---
  const hres = await fetch(`https://127.0.0.1:${PORT}/health`, {
    tls: { cert: ownerTls.cert, key: ownerTls.key, ca: ownerTls.ca, serverName: ownerTls.serverName },
  } as any)
  const hbody = await hres.json()
  if (typeof hbody.connections === "number" && hbody.connections >= 2) pass(`health reports ${hbody.connections} live connections`)
  else fail(`health connections: ${JSON.stringify(hbody)}`)

  // --- I2: revocation enforcement ---
  const rev = await fetchRpc(ownerTls, "team.invite_revoke", { id: invitationId })
  if (rev?.ok !== true) fail(`revoke: ${JSON.stringify(rev)}`)
  else {
    // 1. the live member WS is actively disconnected (close frame, skipping ping/event noise)
    let closed = false
    const deadline = Date.now() + 5000
    while (!closed && Date.now() < deadline) {
      try {
        const m = await memberWs.next(deadline - Date.now())
        if (m.type === "close") { closed = true; pass("revoke actively disconnected the member WS") }
      } catch { break }
    }
    if (!closed) fail("member WS was not closed after revoke")

    // 2. the revoked member's RPC is rejected
    const st = await fetchRpc(memberTls, "team.status", {})
    if (st?.ok === false && /revoked/i.test(String(st.error))) pass("revoked member RPC rejected")
    else fail(`revoked member RPC should be rejected: ${JSON.stringify(st)}`)

    // 3. the revoked member's WS reconnect is rejected
    const ws2 = new WsClient(`wss://127.0.0.1:${PORT}/ws`, memberTls)
    await ws2.open()
    const err = await ws2.next(4000)
    if (err.type === "error" && err.code === "revoked") pass("revoked member WS reconnect rejected")
    else fail(`revoked member WS reconnect should be rejected: ${JSON.stringify(err)}`)
    ws2.close()
  }

  ownerWs.close()
  memberWs.close()
  serveProc.kill("SIGTERM")
  serveProc = null

  console.log(failures === 0 ? "\nWS TEST PASS" : `\nWS TEST FAILED (${failures})`)
  process.exit(failures === 0 ? 0 : 1)
}

main().catch((e) => {
  console.error("WS TEST ERROR:", e)
  cleanup()
  process.exit(1)
})
