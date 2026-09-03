// autoclone.ts — first-login convenience: after a member becomes active in a
// (network-mode) workspace, auto-clone the team's default git repository so the
// shared docs are already on disk.
//
// Guards:
//  - opt out with TEAMX_NO_AUTO_CLONE=1
//  - runs at most once per workspace (marker `.teamx/git-auto-clone`)
//  - only when the workspace directory is essentially empty (besides `.teamx`)
//  - only clones into `<workspace>/<repo>` when that path does not exist yet
//
// If the member is still pending, the `git list` call fails and we simply do
// nothing (the marker stays unset, so it is retried later).

import { existsSync, mkdirSync, readdirSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { mtlsFor, runCli, serverUrlFor, teamxDir } from "./client"

export async function maybeAutoClone(cwd: string): Promise<void> {
  if (process.env.TEAMX_NO_AUTO_CLONE === "1") return
  const serverUrl = serverUrlFor(cwd)
  if (!serverUrl) return
  if (!mtlsFor(serverUrl, cwd)) return // not a member workspace yet
  const marker = join(teamxDir(cwd), "git-auto-clone")
  if (existsSync(marker)) return

  // Only auto-clone into a fresh/empty workspace — never into a dir that
  // already has user content (we must not surprise people with a clone).
  let entries: string[] = []
  try {
    entries = readdirSync(cwd)
  } catch {
    return
  }
  const userFiles = entries.filter((n) => n !== ".teamx" && !n.startsWith("."))
  if (userFiles.length > 0) return

  // Find the team's repositories (default = the auto-created one).
  const list = await runCli(["git", "list", "--json"], { cwd })
  const repos: { name: string }[] =
    (list.data as { repos?: { name: string }[] })?.repos ?? []
  if (repos.length === 0) return
  const repo = repos[0]
  const target = join(cwd, repo.name)
  if (existsSync(target)) {
    // Already present: record the marker so we stop probing.
    mkdirSync(teamxDir(cwd), { recursive: true })
    writeFileSync(marker, JSON.stringify({ repo: repo.name, target }, null, 2))
    return
  }

  const r = await runCli(["git", "clone", repo.name], { cwd, forceLocal: true })
  if (r.ok) {
    mkdirSync(teamxDir(cwd), { recursive: true })
    writeFileSync(marker, JSON.stringify({ repo: repo.name, target, at: new Date().toISOString() }, null, 2))
  }
}
