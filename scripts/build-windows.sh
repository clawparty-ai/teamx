#!/usr/bin/env bash
# build-windows.sh — cross-compile the teamx CLI for Windows (x86_64 GNU).
#
#   ./scripts/build-windows.sh              # CLI only   -> dist/teamx-windows/teamx.exe
#   ./scripts/build-windows.sh --gui        # + member panel (gui feature)
#   ./scripts/build-windows.sh --zip        # also create dist/teamx-windows-x86_64.zip
#
# Requires (on macOS):
#   - rustup toolchain with the x86_64-pc-windows-gnu std
#       rustup target add x86_64-pc-windows-gnu
#   - mingw-w64 (provides x86_64-w64-mingw32-gcc)
#       brew install mingw-w64
#   - The linker is configured in .cargo/config.toml (target.x86_64-pc-windows-gnu).
#
# Notes:
#   - tun0 (TUN device) is Unix-only and stubbed out on Windows; the rest of the
#     CLI (team/role/events, network mode: serve/tunnel/proxy/git/dns) is fully
#     functional.
#   - `--gui` builds the member-side panel (`teamx gui-member`: import letter +
#     tunnel mappings + SOCKS5). eframe/egui are bundled; no extra runtime.
#   - The Windows binary links the UCRT (api-ms-win-crt-*) which ships with
#     Windows 10/11; no extra runtime is bundled.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

TARGET="x86_64-pc-windows-gnu"
OUT_DIR="$ROOT/dist/teamx-windows"
EXE="$ROOT/target/$TARGET/release/teamx.exe"

# Prefer the rustup cargo shim so the target's std is found (a Homebrew rust in
# PATH may not know the rustup-installed x86_64-pc-windows-gnu std).
CARGO_BIN="${CARGO:-cargo}"
if command -v rustup >/dev/null 2>&1; then
    if [[ -x "$HOME/.cargo/bin/cargo" ]]; then
        CARGO_BIN="$HOME/.cargo/bin/cargo"
    fi
fi

GUI=""
if [[ " $* " == *" --gui "* ]]; then
    GUI="--features gui"
fi

step() { printf '\n== %s ==\n' "$*"; }

step "1/3 build release binary for $TARGET${GUI:+ ($GUI)}"
"$CARGO_BIN" build --release --target "$TARGET" $GUI

step "2/3 copy exe to dist/"
mkdir -p "$OUT_DIR"
cp "$EXE" "$OUT_DIR/teamx.exe"
cp "$ROOT/README.md" "$OUT_DIR/README.md" 2>/dev/null || true
cp "$ROOT/LICENSE" "$OUT_DIR/LICENSE" 2>/dev/null || true

# Minimal usage note for Windows users.
cat > "$OUT_DIR/使用说明.md" <<'MD'
# teamx Windows 版

`teamx.exe` 是 teamx 团队协作 CLI（Windows x86_64）。

## 快速开始

1. 打开 PowerShell 或 CMD，进入本目录：
   ```powershell
   cd 本目录
   .\teamx.exe --help
   ```
2. 加入团队：把 owner 给的邀请函（`teamx-inv:v1:...` 或 letter 文件）导入：
   ```powershell
   .\teamx.exe team import <邀请函>
   .\teamx.exe local member-add m1 "你的名字" --server https://<owner-lan-ip>:5781 --letter <邀请函id>
   ```
3. 同步团队状态：
   ```powershell
   .\teamx.exe team status --session <session-key>
   .\teamx.exe sync --session <session-key>
   ```

## 成员端窗口（GUI，需 --gui 构建）

双击运行或在终端执行，打开可视化面板：
```powershell
.\teamx.exe gui-member
```
面板支持：
- **导入邀请函**：粘贴 `teamx-inv:v1:...` 或选择 letter 文件，自动记录服务器地址
- **隧道端口映射**：expose 本地服务 / forward 队友隧道 / close 关闭，实时列表
- **SOCKS5 代理**：一键启停本地 1080 端口代理

## 常用命令

| 功能 | 命令 |
|---|---|
| 查看帮助 | `teamx.exe --help` |
| 成员端窗口 | `teamx.exe gui-member` |
| 列出团队 | `teamx.exe team list --session <s>` |
| 团队状态 | `teamx.exe team status --session <s>` |
| 同步新事件 | `teamx.exe sync --session <s>` |
| 汇报进度 | `teamx.exe publish progress --data "{\"message\":\"...\"}" --session <s>` |
| 选择角色 | `teamx.exe role set <role> --session <s>` |
| 列出隧道 | `teamx.exe tunnel list --session <s>` |
| 暴露本地端口 | `teamx.exe tunnel expose <名称> --port <端口> --session <s>` |
| 转发队友隧道 | `teamx.exe tunnel forward <名称> --session <s>` |
| 启动 SOCKS5 代理 | `teamx.exe proxy start --port 1080` |

> `tun0` 透明代理为 macOS/Linux 专属，Windows 暂不支持（wintun 适配待做）。
> 在 PowerShell 中内嵌引号建议用 `'\"'` 转义或写成 `'{"message":"..."}'`。
MD

step "3/3 done"
echo
echo "Done: $OUT_DIR/teamx.exe"
echo "  size: $(du -h "$OUT_DIR/teamx.exe" | cut -f1)"
echo
echo "Verify (optional):"
echo "  wine $OUT_DIR/teamx.exe --help"
echo
echo "Package:"
echo "  cd $ROOT && zip -r dist/teamx-windows-x86_64.zip dist/teamx-windows"

if [[ "${1:-}" == "--zip" ]]; then
    (cd "$ROOT" && zip -r dist/teamx-windows-x86_64.zip dist/teamx-windows >/dev/null)
    echo "ZIP: dist/teamx-windows-x86_64.zip"
fi
