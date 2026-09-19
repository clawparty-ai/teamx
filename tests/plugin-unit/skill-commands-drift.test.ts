// Plugin drift guard — dsh skill command table vs registered slash commands.
//
// The dsh runtime skill (`dsh-plugin/src/skill.ts`) documents the flat `/team-*`
// commands that the plugin registers (`dsh-plugin/src/commands.ts`). When one
// side gains a command and the other does not, agents are taught commands that
// do not exist (or miss ones that do). This test makes that drift fail loudly.
//
// Rule enforced: every REGISTERED command must be DOCUMENTED in the skill table.
// (The reverse — documented-but-not-registered — is also checked, because a
// stale doc is equally misleading.)

import { readFileSync } from "node:fs"
import { join } from "node:path"

const ROOT = join(import.meta.dir, "../..")
const commandsSrc = readFileSync(join(ROOT, "dsh-plugin/src/commands.ts"), "utf8")
const skillSrc = readFileSync(join(ROOT, "dsh-plugin/src/skill.ts"), "utf8")

function registered(src: string): Set<string> {
  const out = new Set<string>()
  for (const m of src.matchAll(/name:\s*['"](team-[a-z0-9-]+)['"]/g)) out.add(m[1])
  return out
}

function documented(src: string): Set<string> {
  const out = new Set<string>()
  for (const m of src.matchAll(/\/team-[a-z0-9-]+/g)) out.add(m[0].slice(1))
  return out
}

let fail = 0
const check = (ok: boolean, msg: string) => {
  if (ok) {
    console.log("  ok:", msg)
  } else {
    console.log("  FAIL:", msg)
    fail++
  }
}

const reg = registered(commandsSrc)
const doc = documented(skillSrc)
console.log(`registered: ${reg.size} commands, documented: ${doc.size} entries`)

const missingInSkill = [...reg].filter((c) => !doc.has(c)).sort()
check(missingInSkill.length === 0, `every registered command is documented (missing: ${missingInSkill.join(", ") || "none"})`)

const staleInSkill = [...doc].filter((c) => !reg.has(c)).sort()
check(staleInSkill.length === 0, `every documented command is registered (stale: ${staleInSkill.join(", ") || "none"})`)

// The brainstorm (design/007) mechanics must be discoverable from the skill.
check(skillSrc.includes("brainstorm"), "skill documents the brainstorm discussion mode")

if (fail) {
  console.log(`\nFAILED (${fail})`)
  process.exit(1)
}
console.log("\nALL PASS (skill/commands drift)")
