// N3 plugin unit test — WebSocket push client (`connectWs`).
//
// Verifies the transport's URL mapping, live connect, event callback, and
// reconnect-after-drop (the fallback behavior that lets the M2 poller resume).
// Runs against an in-process Bun WebSocket server (no mTLS needed here).

import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"

// Isolate teamx home so `mtlsFor` finds no invitation letters (plain ws://).
const HOME = mkdtempSync(join(tmpdir(), "teamx-ws-unit-"))
process.env.TEAMX_HOME = HOME
delete process.env.TEAMX_MTLS_CERT
delete process.env.TEAMX_MTLS_KEY
delete process.env.TEAMX_MTLS_CA

const { connectWs, wsUrl } = await import(join(import.meta.dir, "../../opencode-plugin/src/ws"))

let fail = 0
const check = (name: string, ok: boolean) => {
  if (!ok) { fail++; console.log("FAIL:", name) } else console.log("  ok:", name)
}

// --- wsUrl mapping ---
check("http → ws", wsUrl("http://127.0.0.1:5781") === "ws://127.0.0.1:5781/ws")
check("https → wss (strips trailing slash)", wsUrl("https://127.0.0.1:5781/") === "wss://127.0.0.1:5781/ws")

// --- connectWs: connect + event + reconnect ---
const PORT = 5841
let server: ReturnType<typeof Bun.serve> | null = null
let live: any = null

function startServer() {
  server = Bun.serve({
    port: PORT,
    fetch(req, srv) {
      if (srv.upgrade(req)) return undefined
      return new Response("upgrade needed", { status: 426 })
    },
    websocket: {
      open(ws: any) {
        live = ws
        ws.send(JSON.stringify({ type: "registered", member_id: "u1", teams: ["t1"] }))
      },
      message(ws: any, msg: any) {
        const m = JSON.parse(String(msg))
        if (m.type === "ping") ws.send(JSON.stringify({ type: "pong" }))
      },
      close() {
        live = null
      },
    },
  })
}

startServer()

const events: any[] = []
const statuses: boolean[] = []
const h = connectWs({
  serverUrl: `ws://127.0.0.1:${PORT}`,
  onEvent: (e) => events.push(e),
  onStatus: (c) => statuses.push(c),
})

await new Promise((r) => setTimeout(r, 500))
check("connects (onStatus true)", statuses.includes(true))

live?.send(JSON.stringify({ type: "event", event: { seq: 1, type: "decision.broadcast", payload: {} } }))
await new Promise((r) => setTimeout(r, 200))
check("event frame delivered to onEvent", events.some((e) => e.type === "decision.broadcast"))

// drop the server → client reports disconnected and schedules a reconnect
server!.stop(true)
server = null
await new Promise((r) => setTimeout(r, 500))
check("disconnect reported (onStatus false)", statuses[statuses.length - 1] === false)

// restart → client reconnects (backoff ~1s) and reports connected again
startServer()
await new Promise((r) => setTimeout(r, 2500))
check("reconnects after restart (onStatus true again)", statuses[statuses.length - 1] === true)

h.close()
server!.stop(true)
rmSync(HOME, { recursive: true, force: true })

console.log(fail === 0 ? "\nALL PASS (ws unit)" : `\n${fail} FAIL`)
process.exit(fail === 0 ? 0 : 1)
