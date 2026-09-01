# teamx Homebrew 安装与发布（brew install teamx）

> 状态：**已实现并验证**（v0.2.0 通过 `brew install caishu97/tap/teamx` 安装成功）
> 目标读者：想用 Homebrew 安装 teamx 的用户 / 负责发版的维护者

---

## 1. 用户安装方式

```bash
brew tap caishu97/tap
brew install caishu97/tap/teamx
# 之后可简写：brew install teamx（若 tap 已在）
```

- macOS（Apple Silicon / M1 及以上）：`brew install teamx` 直接装 `aarch64-apple-darwin` 二进制
- Linux x86_64 / arm64：同样适用（formula 按 os/arch 选择对应 tarball）
- **Windows 不走 brew**：用企业版构建的 `teamx-windows-x86_64.zip` 手动安装

## 2. 架构

```
brew install caishu97/tap/teamx
        │
        ▼
GitHub 公开 tap 仓库  caishu97/homebrew-tap
  Formula/teamx.rb    ← 声明下载 URL + sha256 + 平台分支
        │
        ▼ 匿名下载
公开 Release 资产     caishu97/homebrew-tap/releases/tag/v0.2.0
  teamx-macos-arm64.tar.gz
  teamx-linux-x86_64.tar.gz
  teamx-linux-arm64.tar.gz
```

**为什么二进制放在公开 tap 而不是企业版仓库**：
企业版源码仓库 `caishu97/teamx-enterprise` 是**私有的**，其 Release 资产无法被 brew 匿名下载（GitHub 对私有仓库资产返回 404）。因此二进制 tarball 镜像到**公开的 homebrew-tap 仓库**（formula 元数据 + 二进制均可公开，不含企业版源码）。

## 3. Tarball 结构（release.yml 生成）

每个 tarball 解压后：

```
teamx-<version>/
  bin/teamx          ← 单文件静态二进制（rusqlite bundled，无外部动态库）
  LICENSE            ← 私有版权声明
```

Homebrew 会自动 chdir 进 `teamx-<version>/`，formula 用 `bin.install "bin/teamx"` 安装。

## 4. 发版 SOP（维护者）

### 4.1 打 tag 触发构建

```bash
git tag v0.2.0            # 版本号与 Cargo.toml / formula version 对齐
git push enterprise-origin v0.2.0
```

触发 `.github/workflows/release.yml`：
- 3 平台并行构建：macos-arm64（macos-14）、linux-x86_64（ubuntu-latest）、linux-arm64（ubuntu-24.04-arm）
- 每个平台打成 `teamx-<platform>.tar.gz`
- 上传到**私有** enterprise release（v0.2.0）
- 若配置了 `TAP_RELEASE_TOKEN`（PAT，repo 作用域），自动镜像到**公开** homebrew-tap release

### 4.2 同步 tarball 到公开 tap（若未配 PAT，手动兜底）

```bash
# 从私有 enterprise release 下载
cd /tmp
gh release download v0.2.0 --repo caishu97/teamx-enterprise \
  --pattern "teamx-*.tar.gz" --clobber

# 上传到公开 tap release（先确保 tap release 存在）
gh release create v0.2.0 --repo caishu97/homebrew-tap \
  --title "teamx v0.2.0 binaries" \
  --notes "Pre-built teamx binaries for Homebrew"  # 首次
gh release upload v0.2.0 teamx-*.tar.gz --repo caishu97/homebrew-tap
```

### 4.3 更新 formula 的 sha256

```bash
shasum -a 256 teamx-macos-arm64.tar.gz teamx-linux-x86_64.tar.gz teamx-linux-arm64.tar.gz
```

把 3 个 sha256 填回 `caishu97/homebrew-tap` 的 `Formula/teamx.rb`（同时更新 `version`），提交推送：

```bash
cd /tmp/homebrew-tap
# 编辑 Formula/teamx.rb：version = "X.Y.Z" + 三个 sha256
git add Formula/teamx.rb
git commit -m "teamx vX.Y.Z: update sha256"
git push origin main
```

### 4.4 验证

```bash
brew untap caishu97/tap && brew tap caishu97/tap   # 刷新 tap
HOMEBREW_NO_REQUIRE_TAP_TRUST=1 brew install caishu97/tap/teamx
teamx --version
```

## 5. Tap trust 说明（Homebrew 6+）

新版 Homebrew 对第三方 tap 增加信任检查。用户首次安装时会看到提示，两种处理：
- 信任整个 tap：`brew trust caishu97/tap`
- 或临时跳过：`HOMEBREW_NO_REQUIRE_TAP_TRUST=1 brew install caishu97/tap/teamx`

（信任是一次性的，之后正常 `brew install teamx`。）

## 6. 已知限制

- **version 是手动维护点**：每次发版需同步 3 处（git tag / Cargo.toml / formula version）。
- **sha256 无法完全自动化**：二进制构建后才生成，需回填 formula。
- **Homebrew 只装 CLI 二进制**：opencode 插件（`/team` 命令、agent 路由）仍需 `install.sh` 或 `opencode plugin install` 单独安装。
- **Xcode 版本**：macOS 上 brew 可能提示 Xcode/CLT 过旧，属于 Homebrew 环境提示，不影响 teamx 二进制安装。
