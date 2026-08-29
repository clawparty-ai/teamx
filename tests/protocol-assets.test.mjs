import { strict as assert } from "node:assert"
import { spawnSync } from "node:child_process"
import { createHash } from "node:crypto"
import { readFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

const root = join(dirname(fileURLToPath(import.meta.url)), "..")
const read = (...parts) => readFileSync(join(root, ...parts), "utf8")

const generator = spawnSync("node", ["scripts/generate-grill-protocol.mjs", "--check"], {
  cwd: root,
  encoding: "utf8",
})
assert.equal(generator.status, 0, generator.stderr)

const protocol = read("protocols", "grill-with-docs.md")
const hash = createHash("sha256").update(protocol.replace(/\r\n/g, "\n")).digest("hex")

for (const invariant of [
  "Call `teamx_sync` and verify that the current member is the team owner",
  "Only the owner settles decisions",
  "The Design Session itself does not implement code",
  "the human owner explicitly confirms Shared Understanding",
]) {
  assert.ok(protocol.includes(invariant), `canonical protocol is missing: ${invariant}`)
}

for (const name of ["team-grill.md", "team-grill.en.md"]) {
  const asset = read("opencode-plugin", "assets", "command", name)
  assert.match(asset, /^---\ndescription: .+\nagent: teamx\n---/)
  assert.ok(asset.includes("User arguments: $ARGUMENTS") || asset.includes("用户参数: $ARGUMENTS"))
  assert.ok(asset.includes(`protocol v1; sha256 ${hash}`))
}

const dshAdapter = read("dsh-plugin", "src", "grill-with-docs.generated.ts")
assert.ok(dshAdapter.includes("TEAMX_GRILL_PROTOCOL_VERSION = 1"))
assert.ok(dshAdapter.includes(`TEAMX_GRILL_PROTOCOL_SHA256 = '${hash}'`))
assert.ok(dshAdapter.includes("name: 'teamx-grill-with-docs'"))

const dshEntry = read("dsh-plugin", "src", "index.ts")
assert.ok(dshEntry.includes("ctx.skills.register(TEAMX_GRILL_WITH_DOCS_SKILL)"))

const installer = read("install.sh")
assert.match(installer, /for cmd[\s\S]*team-grill[\s\S]*; do/)
assert.ok(installer.includes('"$CONFIG_DIR/commands/team-grill.md"'))

for (const packagePath of [
  ["opencode-plugin", "package.json"],
  ["dsh-plugin", "package.json"],
]) {
  const pkg = JSON.parse(read(...packagePath))
  assert.equal(pkg.scripts["check:protocols"], "node ../scripts/generate-grill-protocol.mjs --check")
}

console.log("ALL PASS (grill-with-docs protocol assets)")
