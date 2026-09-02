// Boot a real `teamx ui` against a freshly seeded workspace and expose
// { baseURL, token } to the specs via ./env.json.
import { mkdtempSync, writeFileSync, readFileSync, existsSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, dirname } from "node:path"
import { execFileSync, spawn, ChildProcess } from "node:child_process"

const root = dirname(dirname(__dirname))
const bin = process.env.TEAMX_BIN || join(root, "target", "debug", "teamx")
const port = Number(process.env.TEAMX_UI_TEST_PORT || 19543)
const base = `https://127.0.0.1:${port}`
const work = mkdtempSync(join(tmpdir(), "teamx-ui-e2e-"))
const envFile = join(__dirname, "env.json")

let child: ChildProcess | null = null

function tx(args: string[]): string {
  return execFileSync(bin, args, { cwd: work, encoding: "utf8" }).trim()
}

export default async function globalSetup() {
  console.log(`[ui-e2e] seed workspace ${work} (bin=${bin})`)
  tx(["team", "create", "UIe2e", "--session", "owner"])
  // invite + approve a second member so the Dashboard has 2 member nodes
  const list = JSON.parse(tx(["team", "list", "--session", "owner", "--json"]))
  const team = list.teams?.[0]
  if (!team?.invite_token) throw new Error("team list did not return invite_token")
  tx(["team", "join", team.invite_token, "--name", "bob", "--session", "s2"])
  const status = JSON.parse(tx(["team", "status", "--team", team.team_id, "--session", "owner", "--json"]))
  const pending = (status.teams?.[0]?.members ?? []).find((m: { state?: string; id?: string }) => m.state === "pending")
  if (pending?.id) tx(["team", "approve", pending.id, "--session", "owner"])
  // optional node location so the map/topology have coordinates
  writeFileSync(
    join(work, ".teamx", "ui-locations.json"),
    JSON.stringify({
      server: { city: "Hong Kong", lat: 22.32, lng: 114.17 },
      members: { UIe2e: { city: "Shenzhen", lat: 22.54, lng: 114.06 }, bob: { city: "Guangzhou", lat: 23.13, lng: 113.26 } },
    }),
  )

  const log = join(work, "ui.log")
  const logFd = await import("node:fs").then((fs) => fs.openSync(log, "w"))
  child = spawn(bin, ["ui", "--addr", "127.0.0.1", "--port", String(port), "--session", "owner", "--db", join(work, ".teamx", "teamx.db")], {
    cwd: work,
    stdio: ["ignore", "ignore", logFd],
    detached: false,
  })
  // wait for the token line in the log file
  let token = ""
  for (let i = 0; i < 100; i++) {
    try {
      const text = readFileSync(log, "utf8")
      const m = text.match(/token=([0-9a-f]+)/)
      if (m) { token = m[1]; break }
    } catch { /* not yet */ }
    await new Promise((r) => setTimeout(r, 200))
  }
  if (!token) {
    try { console.error(readFileSync(log, "utf8")) } catch {}
    throw new Error("teamx ui did not print a token")
  }
  writeFileSync(envFile, JSON.stringify({ baseURL: base, token, work }, null, 2))
  console.log(`[ui-e2e] ui ready at ${base}/?token=${token.slice(0, 6)}…`)
}

export function cleanup() {
  if (child) child.kill("SIGKILL")
  try { if (existsSync(envFile)) rmSync(envFile, { force: true }) } catch {}
  try { rmSync(work, { recursive: true, force: true }) } catch {}
}
