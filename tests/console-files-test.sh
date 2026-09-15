#!/usr/bin/env bash
# Console 「File」 first-level menu — backend acceptance
# (design docs/design/console-file-browser.md).
#
# Local-only panel: the console runs inside the workspace and reads the
# filesystem through the shared `fs::resolve_with` guard. This test drives the
# real HTTP panel API and asserts the settled safety/limit decisions, including
# the `.teamx` rule: documents browsable, credential entries hidden.
#
# Requires: curl, python3.
set -euo pipefail

TEAMX="${TEAMX:-${TEAMX_BIN:-$(dirname "$0")/../target/debug/teamx}}"
WS="$(mktemp -d /tmp/teamx-files-ws-XXXXXX)"
CPORT="${TEAMX_FILES_CONSOLE_PORT:-9533}"
pass() { printf '  ok: %s\n' "$*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
step() { printf '\n=== %s ===\n' "$*"; }
cleanup() {
  [ -n "${PID:-}" ] && kill "$PID" 2>/dev/null || true
  rm -rf "$WS"
}
trap cleanup EXIT

command -v curl >/dev/null 2>&1 || { echo "skipped: curl missing"; exit 0; }
command -v python3 >/dev/null 2>&1 || { echo "skipped: python3 missing"; exit 0; }

step "setup: workspace + files + a git repo + a .teamx with credentials"
mkdir -p "$WS/src" "$WS/repo-a/.git/refs/heads" "$WS/repo-a/.git/objects" "$WS/node_modules/pkg"
mkdir -p "$WS/.teamx/ca" "$WS/.teamx/letters/x" "$WS/.teamx/members/weiye" "$WS/.teamx/docs/design"
echo '{}' > "$WS/.teamx/instance.json"
printf '# Team\n' > "$WS/.teamx/TEAM.md"
printf '# plan\n' > "$WS/.teamx/docs/design/001.md"
printf 'PRIVATE\n' > "$WS/.teamx/ca/ca.key"
printf 'PRIVATE\n' > "$WS/.teamx/letters/x/client.key"
printf 'PRIVATE\n' > "$WS/.teamx/members/weiye/invitation.letter"
printf 'SQLITE\n' > "$WS/.teamx/teamx.db"
printf '# hello\nsecond line\n' > "$WS/README.md"
printf 'PRIVATE KEY\n' > "$WS/server.key"
printf '<svg/>' > "$WS/logo.svg"
printf 'fn main(){}\n' > "$WS/src/main.rs"
printf 'ref: refs/heads/main\n' > "$WS/repo-a/.git/HEAD"
printf 'init\n' > "$WS/repo-a/.git/COMMIT_EDITMSG"
printf 'obj' > "$WS/repo-a/.git/objects/x"
export TEAMX_HOME="$WS/.teamx"

step "start console"
"$TEAMX" console --port "$CPORT" > "$WS/console.log" 2>&1 &
PID=$!
for _ in $(seq 1 60); do grep -q "token=" "$WS/console.log" 2>/dev/null && break; sleep 0.25; done
TOKEN=$(sed -n 's/.*token=\([A-Za-z0-9]*\).*/\1/p' "$WS/console.log" | head -1)
[ -n "$TOKEN" ] || { cat "$WS/console.log"; fail "no console token"; }
C="Cookie: teamx_console_token=$TOKEN"
BASE="https://127.0.0.1:$CPORT"
for _ in $(seq 1 40); do curl -sk --max-time 2 "$BASE/" -H "$C" >/dev/null 2>&1 && break; sleep 0.2; done
pass "console up (token=$TOKEN)"

panel() { curl -sk --max-time 15 "$BASE/api/panels/$1" -H "$C"; }

step "files: root listing, sensitive flags, repo index"
panel "files" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d['ok'], d
data=d['data']
names=[e['name'] for e in data['entries']]
assert 'src' in names and 'README.md' in names, names
assert '.teamx' in names, '.teamx must be browsable now: %s' % names
assert data['entries'][0]['type']=='directory', data['entries'][0]
key=[e for e in data['entries'] if e['name']=='server.key'][0]
assert key['sensitive'] is True, key
readme=[e for e in data['entries'] if e['name']=='README.md'][0]
assert readme['sensitive'] is False, readme
repos=[r['name'] for r in data['repos']]
assert 'repo-a' in repos, repos
assert 'pkg' not in repos, repos
branch=[r['branch'] for r in data['repos'] if r['name']=='repo-a'][0]
assert branch=='main', branch
print('  ok: entries + sensitive flags + repos', repos, 'branch', branch)
"
pass "files panel"

step ".teamx: documents browsable, credential entries hidden"
panel "files?id=.teamx" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d['ok'], d
names=[e['name'] for e in d['data']['entries']]
for want in ('docs','TEAM.md','instance.json'):
    assert want in names, '%s missing: %s' % (want, names)
for denied in ('ca','letters','members','teamx.db','console-server.json'):
    assert denied not in names, '%s must be hidden: %s' % (denied, names)
print('  ok: .teamx entries', names)
"
panel "file?id=.teamx/docs/design/001.md" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d['ok'], d
assert d['data']['kind']=='markdown', d
assert d['data']['content'].strip()=='# plan', d
"
pass ".teamx documents browsable"

step ".teamx credentials are refused at any depth"
for p in ".teamx/ca/ca.key" ".teamx/letters/x/client.key" ".teamx/members/weiye/invitation.letter" ".teamx/teamx.db" ".teamx/console-server.json"; do
  panel "file?id=$p" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert not d['ok'], d
" || fail "must refuse: $p"
done
pass ".teamx credential paths refused"

step "files: .git is browsable but its internals are collapsed"
panel "files?id=repo-a/.git" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d['ok'], d
names=[e['name'] for e in d['data']['entries']]
assert 'HEAD' in names and 'COMMIT_EDITMSG' in names, names
assert 'objects' not in names, names
print('  ok: .git entries', names)
"
pass ".git collapsed"

step "file: text/markdown preview"
panel "file?id=README.md" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d['ok'], d
x=d['data']
assert x['kind']=='markdown', x
assert x['content'].strip()=='# hello\nsecond line', x
assert x['lines']==2, x
"
panel "file?id=src/main.rs" | python3 -c "
import json,sys
d=json.load(sys.stdin)['data']
assert d['kind']=='text' and 'fn main' in d['content'], d
"
pass "text + markdown preview"

step "file: sensitive + SVG are never previewed inline"
panel "file?id=server.key" | python3 -c "
import json,sys
d=json.load(sys.stdin)['data']
assert d['kind']=='sensitive', d
assert d.get('content') is None, d
"
panel "file?id=logo.svg" | python3 -c "
import json,sys
d=json.load(sys.stdin)['data']
assert d['kind']=='download', d
assert d.get('data_uri') is None, d
"
pass "sensitive + SVG download-only"

step "negatives: traversal and .git internals are refused"
for p in "../../../etc/passwd" "repo-a/.git/objects"; do
  panel "file?id=$p" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert not d['ok'], d
" || fail "must refuse: $p"
done
pass "traversal / .git internals refused"

echo
echo "ALL CONSOLE FILE TESTS PASSED"
