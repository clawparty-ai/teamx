// argv-parse regression test — `cliArgsToRpc` boundary hardening.
//
// Regression: `teamx_publish` with assignee + long/multiline message crashed
// the plugin with `next.startsWith is not a function`
// (`next.startsWith("--")` with `next` undefined). Both plugin copies now
// guard with `typeof` checks; these cases must parse without throwing and
// keep the existing flags→args mapping intact.
//
// Run: bun tests/plugin-unit/argv-parse.test.ts

import { join } from "node:path"

const { cliArgsToRpc: oParse } = await import(join(import.meta.dir, "../../opencode-plugin/src/client"))
const { cliArgsToRpc: dParse } = await import(join(import.meta.dir, "../../dsh-plugin/src/client"))

let fail = 0
const check = (name: string, ok: boolean) => {
  if (!ok) { fail++; console.log("FAIL:", name) } else console.log("  ok:", name)
}
const noThrow = (name: string, fn: () => void) => {
  try { fn(); check(name, true) } catch (e) { check(`${name} (threw: ${e})`, false) }
}

for (const [label, parse] of [["opencode", oParse], ["dsh", dParse]] as const) {
  // 1. Trailing flag with no value → boolean true, no crash.
  noThrow(`${label}: trailing --assignee with no value`, () => {
    const r = parse(["publish", "progress", "--assignee"])
    check(`${label}: trailing flag becomes true`, (r.args as Record<string, unknown>)["assignee"] === true)
  })

  // 2. Flag followed by another flag → first is true, second parsed normally.
  noThrow(`${label}: flag-then-flag`, () => {
    const r = parse(["task", "list", "--mine", "--session", "k"])
    check(`${label}: --mine is true`, (r.args as Record<string, unknown>)["mine"] === true)
    check(`${label}: --session keeps value`, (r.args as Record<string, unknown>)["session"] === "k")
  })

  // 3. Long multiline --data value stays one verbatim token (publish path).
  noThrow(`${label}: multiline --data value`, () => {
    const msg = "line1\nline2 --not-a-flag\n" + "x".repeat(5000)
    const data = JSON.stringify({ message: msg })
    const r = parse(["publish", "progress", "--data", data, "--assignee", "member-1", "--session", "k"])
    check(`${label}: method=publish`, r.method === "publish")
    check(`${label}: data verbatim`, (r.args as Record<string, unknown>)["data"] === data)
    check(`${label}: assignee kept`, (r.args as Record<string, unknown>)["assignee"] === "member-1")
  })

  // 4. Undefined / hole element from a misbuilt vector must not crash.
  noThrow(`${label}: undefined element skipped`, () => {
    const r = parse(["sync", undefined as unknown as string, "--session", "k"])
    check(`${label}: method=sync`, r.method === "sync")
    check(`${label}: session kept`, (r.args as Record<string, unknown>)["session"] === "k")
  })

  // 5. Existing mapping intact (grouped method + kebab→snake + slots).
  noThrow(`${label}: normal mapping intact`, () => {
    const r = parse(["team", "status", "--team", "t1", "--no-advance"])
    check(`${label}: method=team.status`, r.method === "team.status")
    check(`${label}: team=t1`, (r.args as Record<string, unknown>)["team"] === "t1")
    check(`${label}: --no-advance → no_advance=true`, (r.args as Record<string, unknown>)["no_advance"] === true)
    const r2 = parse(["task", "ack", "task-1"])
    check(`${label}: positional slot id`, (r2.args as Record<string, unknown>)["id"] === "task-1")
  })
}

console.log(fail === 0 ? "\nALL PASS (argv-parse)" : `\n${fail} FAIL`)
process.exit(fail === 0 ? 0 : 1)
