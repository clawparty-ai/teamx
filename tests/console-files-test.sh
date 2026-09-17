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
printf '| 名称 | 数量 | 说明 |\n|:--|--:|:--:|\n| A | 1 | 左 |\n| B | 22 | 中 |\n' > "$WS/TABLE.md"
printf 'PRIVATE KEY\n' > "$WS/server.key"
printf '//registry:_authToken=secret\n' > "$WS/.npmrc"
mkdir -p "$WS/.ssh"
printf 'PRIVATE\n' > "$WS/.ssh/id_ed25519"
printf '[remote "origin"]\n\turl = https://user:token@example.com/x.git\n' > "$WS/repo-a/.git/config"
printf '<svg/>' > "$WS/logo.svg"
python3 - "$WS/report.pdf" <<'PYEOF'
import sys
objs=[b"<< /Type /Catalog /Pages 2 0 R >>",
      b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
      b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
      b"<< /Length 44 >>\nstream\nBT /F1 18 Tf 20 40 Td (teamx pdf) Tj ET\nendstream",
      b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"]
out=bytearray(b"%PDF-1.4\n"); offs=[]
for i,o in enumerate(objs,1):
    offs.append(len(out)); out+=str(i).encode()+b" 0 obj\n"+o+b"\nendobj\n"
x=len(out); out+=b"xref\n0 "+str(len(objs)+1).encode()+b"\n0000000000 65535 f \n"
for o in offs: out+=("%010d 00000 n \n"%o).encode()
out+=b"trailer\n<< /Size "+str(len(objs)+1).encode()+b" /Root 1 0 R >>\nstartxref\n"+str(x).encode()+b"\n%%EOF\n"
open(sys.argv[1],"wb").write(bytes(out))
PYEOF
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
# Private/credential entries are hidden entirely.
for priv in ('server.key','.npmrc','.ssh'):
    assert priv not in names, '%s must be hidden: %s' % (priv, names)
repos=[r['name'] for r in data['repos']]
assert 'repo-a' in repos, repos
assert 'pkg' not in repos, repos
branch=[r['branch'] for r in data['repos'] if r['name']=='repo-a'][0]
assert branch=='main', branch
print('  ok: entries + private hidden + repos', repos, 'branch', branch)
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
# .git/config may embed a token in a remote URL -> collapsed.
assert 'config' not in names, names
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

step "file: private entries are refused; SVG is download-only"
for priv in "server.key" ".npmrc" ".ssh/id_ed25519" ".teamx/ca/ca.key"; do
  panel "file?id=$priv" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert not d['ok'], d
" || fail "must refuse: $priv"
done
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

step "pdf: classified for inline viewing + raw endpoint serves it inline"
panel "file?id=report.pdf" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d['ok'], d
assert d['data']['kind']=='pdf', d
print('  ok: kind=pdf size=%s' % d['data']['size'])
"
curl -sk -D- -o /tmp/teamx-pdf-out --max-time 15 "$BASE/api/file?path=report.pdf" -H "$C" > /tmp/teamx-pdf-hdr 2>&1
grep -qi '^content-type: application/pdf' /tmp/teamx-pdf-hdr || { cat /tmp/teamx-pdf-hdr; fail "pdf content-type"; }
grep -qi '^content-disposition: inline' /tmp/teamx-pdf-hdr || { cat /tmp/teamx-pdf-hdr; fail "pdf must be inline"; }
head -c 5 /tmp/teamx-pdf-out | grep -q '%PDF-' || fail "pdf bytes not served"
rm -f /tmp/teamx-pdf-out /tmp/teamx-pdf-hdr
pass "pdf inline viewing"

step "markdown: GFM tables render (client mdRender)"
panel "file?id=TABLE.md" | python3 -c "
import json,sys
d=json.load(sys.stdin)
assert d['ok'], d
assert d['data']['kind']=='markdown', d
assert '| 名称 |' in d['data']['content'], d
print('  ok: panel returns markdown table source')
"
if command -v node >/dev/null 2>&1; then
  curl -sk --max-time 15 "$BASE/" -H "$C" > "$WS/page.html"
  node - "$WS/page.html" <<'NODEEOF'
const fs=require("fs");
const html=fs.readFileSync(process.argv[2],"utf8");
// The renderer lives inline in the served console page; extract it by its
// top-level closing brace (the body is uniformly indented).
const m=html.match(/function mdRender\(src\)\{[\s\S]*?\n\}/);
if(!m){ console.error("FAIL: mdRender not found in console HTML"); process.exit(1); }
const mdRender=eval("("+m[0]+")");
const md="| 名称 | 数量 | 说明 |\n|:--|--:|:--:|\n| A | 1 | 左 |\n| B | 22 | 中 |\n";
const out=mdRender(md);
if(!/<table>/.test(out)||!/<thead>/.test(out)||!/<tbody>/.test(out)){ console.error("FAIL: no table markup\n"+out); process.exit(1); }
if(!/<th[^>]*>名称<\/th>/.test(out)){ console.error("FAIL: header cells missing\n"+out); process.exit(1); }
if(!/<td[^>]*>A<\/td>/.test(out)||!/<td[^>]*>B<\/td>/.test(out)){ console.error("FAIL: body cells missing\n"+out); process.exit(1); }
if(!/text-align:left/.test(out)||!/text-align:right/.test(out)||!/text-align:center/.test(out)){ console.error("FAIL: alignment not applied\n"+out); process.exit(1); }
// A pipe row inside a fenced code block must stay code, not become a table.
if(/<table>/.test(mdRender("```\n| a | b |\n|---|---|\n```\n"))){ console.error("FAIL: table inside code fence"); process.exit(1); }
// A lone pipe row with no delimiter row stays a paragraph.
if(/<table>/.test(mdRender("| a | b |\n"))){ console.error("FAIL: table without delimiter row"); process.exit(1); }
console.log("  ok: mdRender -> table/thead/tbody + alignment; fences & delimiter-less rows untouched");
NODEEOF
else
  echo "  (node missing; skipped client mdRender check)"
fi
pass "markdown tables"

echo
echo "ALL CONSOLE FILE TESTS PASSED"
