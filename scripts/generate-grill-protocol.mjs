#!/usr/bin/env node

import { createHash } from "node:crypto"
import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

const root = dirname(dirname(fileURLToPath(import.meta.url)))
const sourcePath = join(root, "protocols", "grill-with-docs.md")
const source = readFileSync(sourcePath, "utf8").replace(/\r\n/g, "\n")
const match = source.match(/^---\n([\s\S]*?)\n---\n\n?([\s\S]*)$/)

if (!match) {
  throw new Error("protocols/grill-with-docs.md must contain YAML frontmatter")
}

const versionMatch = match[1].match(/^protocol_version:\s*(\d+)\s*$/m)
if (!versionMatch) {
  throw new Error("protocol frontmatter must contain a numeric protocol_version")
}

const protocolVersion = Number(versionMatch[1])
const protocolBody = match[2].trimEnd() + "\n"
const sourceHash = createHash("sha256").update(source).digest("hex")
const generatedMarker = `Generated from protocols/grill-with-docs.md; protocol v${protocolVersion}; sha256 ${sourceHash}`

function commandAsset({ description, invocation }) {
  return `---\ndescription: ${description}\nagent: teamx\n---\n\n<!-- ${generatedMarker}; do not edit. -->\n\n${invocation}\n\n${protocolBody}`
}

const outputs = new Map([
  [
    join(root, "opencode-plugin", "assets", "command", "team-grill.en.md"),
    commandAsset({
      description: "Run an owner-led Teamx design interview and preserve decisions in repository docs",
      invocation: "Run the Teamx grill-with-docs protocol for the explicit arguments below. Treat them as the topic and options for Start or Resume.\n\nUser arguments: $ARGUMENTS",
    }),
  ],
  [
    join(root, "opencode-plugin", "assets", "command", "team-grill.md"),
    commandAsset({
      description: "运行 owner 主导的 Teamx 设计访谈，并把决策沉淀到仓库文档",
      invocation: "按下方 Teamx grill-with-docs 协议处理明确参数，将其作为新建设计主题或恢复选项。全程使用用户当前语言。\n\n用户参数: $ARGUMENTS",
    }),
  ],
])

const dshContent = `${protocolBody}\nProtocol metadata: version ${protocolVersion}; source sha256 ${sourceHash}.\n`
outputs.set(
  join(root, "dsh-plugin", "src", "grill-with-docs.generated.ts"),
  `/** ${generatedMarker}; do not edit. */\n\n` +
    `import type { TeamxSkill } from './skill.js'\n\n` +
    `export const TEAMX_GRILL_PROTOCOL_VERSION = ${protocolVersion}\n` +
    `export const TEAMX_GRILL_PROTOCOL_SHA256 = '${sourceHash}'\n\n` +
    `export const TEAMX_GRILL_WITH_DOCS_SKILL: TeamxSkill = {\n` +
    `  name: 'teamx-grill-with-docs',\n` +
    `  description: 'Owner-led design interview that resolves a design tree and records glossary and architecture decisions',\n` +
    `  whenToUse: 'When the human owner explicitly asks to grill or stress-test a plan or design and preserve the decisions in repository docs',\n` +
    `  source: 'runtime',\n` +
    `  content: ${JSON.stringify(dshContent)},\n` +
    `}\n`,
)

const checkOnly = process.argv.slice(2).includes("--check")
const unknown = process.argv.slice(2).filter((arg) => arg !== "--check")
if (unknown.length > 0) {
  throw new Error(`unknown argument(s): ${unknown.join(", ")}`)
}

let drift = false
for (const [path, expected] of outputs) {
  if (checkOnly) {
    let actual = ""
    try {
      actual = readFileSync(path, "utf8").replace(/\r\n/g, "\n")
    } catch {
      // A missing generated file is drift and is reported uniformly below.
    }
    if (actual !== expected) {
      drift = true
      console.error(`generated protocol adapter is stale: ${path}`)
    }
  } else {
    mkdirSync(dirname(path), { recursive: true })
    writeFileSync(path, expected)
    console.log(`generated ${path}`)
  }
}

if (drift) {
  console.error("run: node scripts/generate-grill-protocol.mjs")
  process.exitCode = 1
}
