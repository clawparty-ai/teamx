#!/usr/bin/env bash
# 构建 Teamx Cloud 的两个镜像（design/013/014）——**同源**：同一 TEAMX_BIN + 同一镜像源参数。
#   member 镜像：teamx + opencode + ttyd + bun + git（一个座位一个 Pod）
#   server 镜像：teamx serve（每租户一个，轻量）
#
# 用法：
#   bash docker/build.sh                     # 默认 teamx-member:dev / teamx-server:dev
#   TAG=0.2.5 bash docker/build.sh           # 指定 tag 后缀
#   BASE_IMAGE=docker.m.daocloud.io/library/ubuntu:24.04 \
#   APT_MIRROR=mirrors.aliyun.com bash docker/build.sh    # 受限网络
#   bash docker/build.sh --check             # 只做前置检查（不构建）
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TAG="${TAG:-dev}"
MEMBER_IMAGE="${MEMBER_IMAGE:-teamx-member:$TAG}"
SERVER_IMAGE="${SERVER_IMAGE:-teamx-server:$TAG}"
BASE_IMAGE="${BASE_IMAGE:-ubuntu:24.04}"
APT_MIRROR="${APT_MIRROR:-}"
TEAMX_BIN="${TEAMX_BIN:-target/release/teamx}"
CHECK_ONLY=0
[ "${1:-}" = "--check" ] && CHECK_ONLY=1

say() { printf '\n=== %s ===\n' "$*"; }
ok()  { printf '  ok: %s\n' "$*"; }
die() { echo "FAIL: $*" >&2; exit 1; }

cd "$ROOT"
say "0) 前置检查"
command -v docker >/dev/null || die "需要 docker"
[ -f "$TEAMX_BIN" ] || die "缺少 $TEAMX_BIN —— 先 cargo build --release -p teamx（或设 TEAMX_BIN=）"
[ -f docker/member/Dockerfile ] || die "缺少 docker/member/Dockerfile"
[ -f docker/server/Dockerfile ] || die "缺少 docker/server/Dockerfile"
echo "  base=$BASE_IMAGE  apt_mirror=${APT_MIRROR:-<default>}  bin=$TEAMX_BIN"
echo "  member=$MEMBER_IMAGE  server=$SERVER_IMAGE"
ok "前置检查通过"
[ "$CHECK_ONLY" = "1" ] && { echo "(--check 模式，结束)"; exit 0; }

COMMON_ARGS=(--build-arg "BASE_IMAGE=$BASE_IMAGE" --build-arg "TEAMX_BIN=$TEAMX_BIN")
[ -n "$APT_MIRROR" ] && COMMON_ARGS+=(--build-arg "APT_MIRROR=$APT_MIRROR")

say "1) member 镜像（teamx + opencode + ttyd + bun + git）"
docker build -f docker/member/Dockerfile "${COMMON_ARGS[@]}" -t "$MEMBER_IMAGE" .
ok "构建完成：$MEMBER_IMAGE"

say "2) server 镜像（teamx serve，与 member 同源同二进制）"
docker build -f docker/server/Dockerfile "${COMMON_ARGS[@]}" -t "$SERVER_IMAGE" .
ok "构建完成：$SERVER_IMAGE"

say "3) 校验（同源：两者 teamx 版本必须一致）"
M="$(docker run --rm --entrypoint /usr/local/bin/teamx "$MEMBER_IMAGE" version | head -2 | tr '\n' ' ')"
S="$(docker run --rm "$SERVER_IMAGE" version | head -2 | tr '\n' ' ')"
echo "  member: $M"
echo "  server: $S"
[ "$M" = "$S" ] || die "member 与 server 的 teamx 版本不一致（破坏了同源约定）"
ok "同源校验通过"

cat <<EOF

就绪。让 provider/tenant 用上它们：

  export TEAMX_MEMBER_IMAGE=$MEMBER_IMAGE    # member Pod（provider=k8e）
  export TEAMX_SERVER_IMAGE=$SERVER_IMAGE    # 租户 server（teamx tenant create）
EOF
