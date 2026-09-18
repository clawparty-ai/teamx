# 操作手册：经 teammate 出口在国内使用 muse LLM（teamx-ent）

- 状态：draft
- 所有者：owner
- 创建者：caishu（owner）
- 创建日期：2026-09-18
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
4. 本机为 macOS（Linux 见 §8）。

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

让 GUI 应用也生效：

```bash
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

privoxy 已由 `brew services start privoxy` 常驻。

## 5. 验收清单（逐项打勾）

- [ ] `teamx tunnel list` 能看到 `caicai-ent mode=proxy`
- [ ] `curl --socks5-hostname 127.0.0.1:1080 https://api.ipify.org` = `184.34.85.90`
- [ ] `curl -x http://127.0.0.1:8118 https://api.ipify.org` = `184.34.85.90`
- [ ] `opencode run -m opencode/muse-spark-1.3-contributor-free "reply exactly: MUSE-OK"` → `MUSE-OK`
- [ ] `launchctl list | grep teamx.egress` 有该服务（重启后仍可用）
- [ ] `echo $NO_PROXY` 含 `120.27.222.33`

## 6. 注意事项与常见问题

1. **`NO_PROXY` 必须包含 teamx server**（`120.27.222.33`）：否则 teamx 自己的 mTLS RPC 也会绕道美国，既慢又容易断。
2. **出口是共享资源**：`caicai-ent` 跑在 caicai 的主机上，带宽/流量大家共用，**不要跑大流量下载**；建议只让 `opencode.ai` 走代理（可用 `teamx proxy routes` 按域名选出口，见 `docs/08-design-proxy-routes.md`）。
3. **`HTTPS_PROXY` 不吃 SOCKS5**：所以必须用 privoxy（`forward-socks5t`）把 SOCKS5 桥成 HTTP。
4. 报 `no mTLS material`：设置 `TEAMX_MTLS_CERT/KEY/CA`，或在含 `.teamx/letters` 的工作区里执行。
5. 报 `RegionError` 仍出现：说明流量没走代理——检查 `HTTPS_PROXY` 与 `NO_PROXY` 是否在**同一个 shell/进程**里生效（GUI 应用要 `launchctl setenv`）。
6. 已知问题：`teamx tunnel forward` 在部分版本会 panic（nested tokio runtime），**本方案不使用它**。

## 7. 参考

- 现成案例：powernano-v2 的 `yongyan-mac` 已用同一方案跑通（2026-09-13），出口同为 caicai 的美国主机。
- 出口注册方式（provider 侧，仅供参考，不需你执行）：`teamx proxy exit <name>`。

## 8. Linux 变体

- SOCKS5：同上（`teamx proxy start ...`）。
- HTTP 桥：`privoxy` 或 `gost`（`gost -L http://127.0.0.1:8118 -F socks5://127.0.0.1:1080`）。
- 常驻：写成 systemd **user** unit（`~/.config/systemd/user/teamx-egress.service`，`Restart=always`）。
- 环境变量：`~/.profile` / `~/.bashrc`，或 systemd 用户单元的 `Environment=`。
