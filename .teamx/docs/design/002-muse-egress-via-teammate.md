# 操作手册：经 teammate 出口在国内使用 muse LLM（teamx-ent）

- 状态：draft
- 所有者：owner
- 创建者：caishu（owner）
- 创建日期：2026-09-18
- 最近更新：2026-09-18（据 weiye-mini 实测反馈补：opencode 版本要求 / GUI 代理改可选 / letter 位置与 plist 凭据 / 实践记录见 §9）
- 适用对象：teamx-ent 成员（本文档供 weiye-mini 执行）

## 1. 背景与目标

- `muse-spark` 系列模型由 `https://opencode.ai/zen/v1` 提供，**中国大陆直连会被区域限制**：

  ```json
  {"type":"error","error":{"type":"RegionError","message":"This model is not available in your country."}}
  ```

- teamx-ent 内已有可用出口：**`caicai-ent`**（`mode=proxy`，provider = caicai，主机在美国 `184.34.85.90`）。
- 目标：在**不新开云主机**的前提下，让本机 opencode 用上 muse。

## 2. 原理链路

```text
本机 (国内)
  opencode  默认模型 = opencode/muse-spark-1.3-contributor-free
        │  HTTPS_PROXY=http://127.0.0.1:8118
        ▼
  privoxy 127.0.0.1:8118 ──forward-socks5t──► SOCKS5 127.0.0.1:1080
                                                    │
                                    teamx proxy start --exit caicai-ent
                                                    │ mTLS / WSS
                                                    ▼
                                      teamx serve  https://120.27.222.33:5783
                                                    │ 转发到 exit
                                                    ▼
                                    caicai 云主机 184.34.85.90 (美国)
                                                    │ 出网 IP = 184.34.85.90
                                                    ▼
                                    https://opencode.ai/zen/v1 (muse-spark)
```

要点：`teamx proxy exit`（provider 在 caicai 主机）↔ teamx serve 转发 ↔ `teamx proxy start`（consumer 在本机）→ 本机得到 SOCKS5；再用 privoxy 把 SOCKS5 桥成 HTTP 代理；opencode 走该代理。

## 3. 前置条件

1. 你是 teamx-ent 成员，本机已安装 `teamx` CLI 且导入过邀请函（工作区下存在 `.teamx/letters/`）。
2. 本机能访问 teamx serve `https://120.27.222.33:5783`。
3. 出口 `caicai-ent` 在线（provider = caicai）。
4. **opencode ≥ 1.18.0**（关键）：free tier 的 `muse-spark-*` 只在 ≥1.18.0 的模型列表里出现；旧版（如 1.15.x）即使配好代理也看不到该模型。
   升级：`brew upgrade opencode`（或安装到 `~/.opencode/bin`），并把 PATH 首位指向新版。
5. 本机为 macOS（Linux 见 §8）。

## 4. 操作步骤（macOS）

### 4.0 确认出口在线

```bash
cd <你的 teamx-ent 工作区>          # 含 .teamx/letters
teamx tunnel list                  # 期望看到：caicai-ent  mode=proxy
```

> 若本机残留了指向其它 server 的 `TEAMX_SERVER_URL`，先 `unset TEAMX_SERVER_URL`，让 CLI 从 `.teamx/letters` 自动发现（否则会连错服务端/证书）。

### 4.1 起本地 SOCKS5（consumer 侧）

```bash
teamx proxy start --port 1080 --exit caicai-ent --server https://120.27.222.33:5783
# 期望输出：ok proxy: exit=caicai-ent (fixed) SOCKS5 listening on 127.0.0.1:1080
```

另开一个终端验证出口 IP：

```bash
curl -s --socks5-hostname 127.0.0.1:1080 https://api.ipify.org
# 期望：184.34.85.90（caicai 美国主机）
```

### 4.2 用 privoxy 把 SOCKS5 桥成 HTTP 代理

`HTTPS_PROXY` 只接受 http(s) 代理，所以需要 privoxy 桥接：

```bash
brew install privoxy
C=/opt/homebrew/etc/privoxy/config
grep -q "^forward-socks5t" "$C" || echo "forward-socks5t / 127.0.0.1:1080 ." >> "$C"
brew services start privoxy
# 验证
curl -s -x http://127.0.0.1:8118 https://api.ipify.org   # 期望：184.34.85.90
```

### 4.3 opencode 默认模型设为 muse

> 先确认版本：`opencode --version` 必须 **≥ 1.18.0**，且 `opencode models | grep muse` 能看到 `muse-spark-*`。看不到就先升级（见 §3-4），否则这一步配了也没用。

`~/.config/opencode/opencode.jsonc`：

```json
{
  "$schema": "https://opencode.ai/config.json",
  "model": "opencode/muse-spark-1.3-contributor-free",
  "permission": { "external_directory": { "~/.clawparty/**": "allow" } },
  "provider": {}
}
```

### 4.4 写代理环境变量（关键：NO_PROXY 必须含 teamx server）

追加到 `~/.zshrc` 与 `~/.zshenv`：

```bash
# --- teamx muse proxy (via caicai-ent egress) ---
export HTTPS_PROXY=http://127.0.0.1:8118
export HTTP_PROXY=http://127.0.0.1:8118
export NO_PROXY=127.0.0.1,localhost,120.27.222.33
export TEAMX_SERVER_URL=https://120.27.222.33:5783
```

让 **GUI 应用**也生效（**可选，默认不做**）：

```bash
# ⚠️ 风险：launchctl setenv 是「全局 GUI 代理」，会把浏览器等所有 GUI 流量
# 都导向共享出口（caicai 主机）。CLI 级环境变量已足够 opencode 使用。
# 仅当确实需要 GUI 走代理时再执行，并自行评估带宽/隐私影响。
launchctl setenv HTTPS_PROXY http://127.0.0.1:8118
launchctl setenv HTTP_PROXY  http://127.0.0.1:8118
launchctl setenv NO_PROXY    "127.0.0.1,localhost,120.27.222.33"
launchctl setenv TEAMX_SERVER_URL https://120.27.222.33:5783
```

### 4.5 验证 muse

```bash
zsh -lic 'opencode run -m opencode/muse-spark-1.3-contributor-free "reply exactly: MUSE-OK"'
# 期望输出：MUSE-OK
```

### 4.6 持久化（重启后仍生效）

`~/Library/LaunchAgents/com.teamx.egress.plist`：

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.teamx.egress</string>
  <key>ProgramArguments</key><array>
    <string>/opt/homebrew/bin/teamx</string><string>proxy</string><string>start</string>
    <string>--port</string><string>1080</string>
    <string>--exit</string><string>caicai-ent</string>
    <string>--server</string><string>https://120.27.222.33:5783</string>
  </array>
  <key>EnvironmentVariables</key><dict>
    <key>TEAMX_SERVER_URL</key><string>https://120.27.222.33:5783</string>
  </dict>
  <key>WorkingDirectory</key><string>&lt;你的 teamx-ent 工作区&gt;</string>
  <key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/tmp/teamx-egress.log</string>
  <key>StandardErrorPath</key><string>/tmp/teamx-egress.err</string>
</dict></plist>
```

```bash
launchctl load ~/Library/LaunchAgents/com.teamx.egress.plist
launchctl list | grep teamx.egress
```

> **凭据位置注意**：若你的邀请函在 `~/.teamx/letters/`（而不是 workspace 的 `.teamx/letters/`），`proxy start` 可能找不到 mTLS 材料。此时在 plist 的 `EnvironmentVariables` 里显式补上：
> ```xml
> <key>TEAMX_MTLS_CERT</key><string>/Users/&lt;you&gt;/.teamx/letters/&lt;id&gt;/client.crt</string>
> <key>TEAMX_MTLS_KEY</key><string>/Users/&lt;you&gt;/.teamx/letters/&lt;id&gt;/client.key</string>
> <key>TEAMX_MTLS_CA</key><string>/Users/&lt;you&gt;/.teamx/letters/&lt;id&gt;/ca.crt</string>
> ```

privoxy 已由 `brew services start privoxy` 常驻。

## 5. 验收清单（逐项打勾）

- [ ] `teamx tunnel list` 能看到 `caicai-ent mode=proxy`
- [ ] `opencode --version` ≥ `1.18.0`，且 `opencode models | grep muse` 有 `muse-spark-*`
- [ ] `curl --socks5-hostname 127.0.0.1:1080 https://api.ipify.org` = `184.34.85.90`
- [ ] `curl -x http://127.0.0.1:8118 https://api.ipify.org` = `184.34.85.90`
- [ ] `opencode run -m opencode/muse-spark-1.3-contributor-free "reply exactly: MUSE-OK"` → `MUSE-OK`
- [ ] `launchctl list | grep teamx.egress` 有该服务（重启后仍可用）
- [ ] `echo $NO_PROXY` 含 `120.27.222.33`

## 6. 注意事项与常见问题

1. **`NO_PROXY` 必须包含 teamx server**（`120.27.222.33`）：否则 teamx 自己的 mTLS RPC 也会绕道美国，既慢又容易断。
2. **出口是共享资源**：`caicai-ent` 跑在 caicai 的主机上，带宽/流量大家共用，**不要跑大流量下载**；建议只让 `opencode.ai` 走代理（可用 `teamx proxy routes` 按域名选出口，见 `docs/08-design-proxy-routes.md`）。
3. **`HTTPS_PROXY` 不吃 SOCKS5**：所以必须用 privoxy（`forward-socks5t`）把 SOCKS5 桥成 HTTP。
4. 报 `no mTLS material`：设置 `TEAMX_MTLS_CERT/KEY/CA`，或在含 `.teamx/letters` 的工作区里执行；持久化到 LaunchAgent 时注意 letter 的实际位置（见 §4.6）。
5. 报 `RegionError` 仍出现：说明流量没走代理——检查 `HTTPS_PROXY` 与 `NO_PROXY` 是否在**同一个 shell/进程**里生效。
6. **opencode 版本不够会「静默失败」**：`< 1.18.0` 时模型列表里根本没有 `muse-spark-*`，看起来像"模型不存在"而不是代理问题。先 `opencode --version` + `opencode models | grep muse` 确认。
7. **GUI 全局代理（`launchctl setenv`）默认不做**：会把浏览器等全部 GUI 流量导向共享出口，风险大于收益；CLI 环境变量已满足 opencode。确需 GUI 再单独评估。
8. 已知问题：`teamx tunnel forward` 在部分版本会 panic（nested tokio runtime），**本方案不使用它**。

## 7. 参考

- 现成案例：powernano-v2 的 `yongyan-mac` 已用同一方案跑通（2026-09-13），出口同为 caicai 的美国主机。
- 出口注册方式（provider 侧，仅供参考，不需你执行）：`teamx proxy exit <name>`。

## 8. Linux 变体

- SOCKS5：同上（`teamx proxy start ...`）。
- HTTP 桥：`privoxy` 或 `gost`（`gost -L http://127.0.0.1:8118 -F socks5://127.0.0.1:1080`）。
- 常驻：写成 systemd **user** unit（`~/.config/systemd/user/teamx-egress.service`，`Restart=always`）。
- 环境变量：`~/.profile` / `~/.bashrc`，或 systemd 用户单元的 `Environment=`。

## 9. 实践记录（weiye-mini 实测 · 2026-09-18）

首次按本手册在 teamx-ent 内实际落地（任务 `muse-egress-via-caicai`），**§5 验收 6/6 全过**：
SOCKS5/HTTP 出口 IP 均为 `184.34.85.90`，LaunchAgent `com.teamx.egress` 运行中，`NO_PROXY` 正确，`muse-spark-1.3-contributor-free` 实测返回 `MUSE-OK`。

本次反馈并已并入正文的修正：

1. **opencode 版本要求**：本机原为 1.15.10/1.15.13，模型列表里**没有** `muse-spark-*`；升级到 **1.18.30** 后才出现并跑通（free tier 需 ≥1.18.0）。已补 §3-4 / §4.3 / §5 / §6-6。
2. **GUI 代理改为可选**：未执行 `launchctl setenv`（会把浏览器等全部 GUI 流量导向共享出口，风险大于收益）；CLI 级环境变量已满足 opencode。已补 §4.4 / §6-7。
3. **凭据位置**：letter 在 `~/.teamx/letters/` 而非 workspace，LaunchAgent plist 里显式补了 `TEAMX_MTLS_CERT/KEY/CA`。已补 §4.6。
4. 使用纪律：**仅 `opencode.ai` 走代理**，安装/下载走 brew 直连，避免占用共享出口带宽。

无关本方案的旁证：`deepseek-v4-flash-free` 报 `upstream Model unavailable`，属该模型自身不可用，与代理无关。
