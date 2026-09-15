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
  "grill-doc is human-led and may run before a team exists",
  "any team member may start and lead a session inside their team",
  "Only the owner settles decisions",
  "The Design Session itself does not implement code",
  "the human owner explicitly confirms Shared Understanding",
]) {
  assert.ok(protocol.includes(invariant), `canonical protocol is missing: ${invariant}`)
}

let protocolVersion = null
for (const name of ["team-grill.md", "team-grill.en.md"]) {
  const asset = read("opencode-plugin", "assets", "command", name)
  assert.match(asset, /^---\ndescription: .+\nagent: teamx\n---/)
  assert.ok(asset.includes("User arguments: $ARGUMENTS") || asset.includes("用户参数: $ARGUMENTS"))
  // The generator stamps `protocol v<N>; sha256 <hash>`. Assert the hash is
  // current and that both assets agree on the version — without pinning a
  // literal that has to be hand-edited on every protocol bump.
  const marker = asset.match(/protocol v(\d+); sha256 ([0-9a-f]+)/)
  assert.ok(marker, `${name} must embed a "protocol v<N>; sha256 <hash>" marker`)
  assert.equal(marker[2], hash, `${name} hash must match protocols/grill-with-docs.md`)
  if (protocolVersion === null) protocolVersion = marker[1]
  else assert.equal(marker[1], protocolVersion, "the two assets disagree on the protocol version")
}

const dshAdapter = read("dsh-plugin", "src", "grill-with-docs.generated.ts")
assert.ok(dshAdapter.includes(`TEAMX_GRILL_PROTOCOL_VERSION = ${protocolVersion}`))
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
