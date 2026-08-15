#!/usr/bin/env bash
# teamx 多窗口协作 demo 启动器（macOS）
#
# 用法：./demo/start.sh [N]     # N = 窗口数，默认 2；3 为「owner + contributor + reviewer」
#
# 每个窗口各自在 demo workspace 里启动 opencode。窗口角色按启动顺序：
#   2 窗口：A=owner、B=reviewer（见 docs/demo.md）
#   3 窗口：A=owner、B=contributor、C=reviewer（见 docs/demo-3p.md）
#
# 前提：已运行 ./install.sh 并重启过 opencode（/Team 命令可用）。
set -euo pipefail

COUNT="${1:-2}"
WORKSPACE="$(cd "$(dirname "$0")/workspace" && pwd)"

LABELS=("OWNER（创建团队 + 协调 + 关闭）" "CONTRIBUTOR（设计方案/实现）" "REVIEWER（评审方案）" "MEMBER")

open_oc() {
  osascript -e 'tell application "Terminal"' \
    -e 'activate' \
    -e 'do script "cd '"$WORKSPACE"' && clear && echo \"[teamx demo] opencode 已启动\" && exec opencode"' \
    -e 'end tell' >/dev/null 2>&1
}

echo "启动 $COUNT 个 opencode 窗口（工作目录: $WORKSPACE）..."
for ((i = 0; i < COUNT; i++)); do
  open_oc
  sleep 1
done

echo
LETTERS=(A B C D E F G H)
for ((i = 0; i < COUNT; i++)); do
  printf '  窗口 %s → %s\n' "${LETTERS[$i]}" "${LABELS[$i]:-MEMBER}"
done
echo
echo "具体步骤：2 窗口见 docs/demo.md；3 窗口见 docs/demo-3p.md。所有窗口共享 ~/.teamx/teamx.db（全局库）。"
