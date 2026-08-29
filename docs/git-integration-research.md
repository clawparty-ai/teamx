# TeamX Server Git 集成调研报告

## 1. 需求分析

### 1.1 用户场景
用户希望通过 mTLS 连接到 teamx server 后，能够直接操作 server 上的 git 仓库，包括：
- `git clone` - 克隆仓库
- `git pull` - 拉取更新
- `git push` - 推送代码
- `git fetch` - 获取远程更新
- 其他标准 git 操作

### 1.2 核心需求
1. **认证集成**：复用 teamx 现有的 mTLS 认证体系
2. **权限控制**：基于 teamx 的成员角色系统控制仓库访问权限
3. **仓库管理**：支持创建、删除、列出仓库
4. **协议兼容**：支持标准 git HTTP 协议（Smart HTTP）

## 2. 技术方案对比

### 2.1 方案一：git2-rs (libgit2 绑定)

**优点**：
- ✅ 纯 Rust 实现，与 teamx 生态一致
- ✅ 功能完整，支持所有 git 操作
- ✅ 可以深度集成到 teamx 系统中
- ✅ 支持细粒度的权限控制
- ✅ 无需外部依赖

**缺点**：
- ❌ 需要编译 libgit2，增加构建复杂度
- ❌ libgit2 体积较大（约 5MB）
- ❌ 需要处理 libgit2 的依赖关系

**适用场景**：需要深度集成、细粒度控制的场景

### 2.2 方案二：git-http-backend (CGI)

**优点**：
- ✅ 标准 git HTTP 协议，兼容性好
- ✅ 使用现有 git 工具，开发成本低
- ✅ 社区成熟，稳定性高

**缺点**：
- ❌ 需要外部依赖（git）
- ❌ 需要处理 CGI 调用，性能较低
- ❌ 集成度较低，权限控制困难

**适用场景**：快速原型、兼容性要求高的场景

### 2.3 方案三：自实现 git 协议

**优点**：
- ✅ 完全控制，可以深度集成
- ✅ 无外部依赖

**缺点**：
- ❌ 工作量巨大，需要实现完整 git 协议
- ❌ 需要深入理解 git 内部原理
- ❌ 维护成本高

**适用场景**：特殊需求、学习目的

### 2.4 方案对比总结

| 维度 | git2-rs | git-http-backend | 自实现 |
|------|---------|------------------|--------|
| 开发成本 | 中等 | 低 | 高 |
| 集成度 | 高 | 低 | 高 |
| 性能 | 高 | 中等 | 高 |
| 维护成本 | 中等 | 低 | 高 |
| 功能完整性 | 高 | 高 | 中等 |
| 依赖管理 | 中等 | 低 | 无 |

**推荐方案**：git2-rs（方案一）

## 3. 推荐方案架构设计

### 3.1 系统架构

```
┌─────────────────────────────────────────────────────────┐
│                    TeamX Server                          │
├─────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐      │
│  │   mTLS      │  │  Git Service │  │  Permission │      │
│  │  Auth Layer │  │  (git2-rs)  │  │  Controller │      │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘      │
│         │                │                │              │
│         └────────────────┼────────────────┘              │
│                          │                               │
│                    ┌─────▼─────┐                         │
│                    │   Router  │                         │
│                    │  (Axum)   │                         │
│                    └─────┬─────┘                         │
│                          │                               │
│  ┌───────────────────────▼───────────────────────┐      │
│  │              HTTP Handlers                     │      │
│  │  /git/:repo/info/refs                         │      │
│  │  /git/:repo/git-upload-pack                   │      │
│  │  /git/:repo/git-receive-pack                  │      │
│  │  /git/repos                                   │      │
│  └───────────────────────┬───────────────────────┘      │
│                          │                               │
│                    ┌─────▼─────┐                         │
│                    │   SQLite  │                         │
│                    │   DB      │                         │
│                    └───────────┘                         │
└─────────────────────────────────────────────────────────┘
```

### 3.2 数据库 Schema 扩展

```sql
-- Git 仓库管理
CREATE TABLE IF NOT EXISTS git_repos (
  id            TEXT PRIMARY KEY,
  team_id       TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  name          TEXT NOT NULL,
  path          TEXT NOT NULL,  -- 服务器上的路径
  description   TEXT,
  is_bare       INTEGER NOT NULL DEFAULT 1,  -- bare repo
  created_by    TEXT NOT NULL,
  created_at    TEXT NOT NULL,
  updated_at    TEXT NOT NULL,
  UNIQUE(team_id, name)
);

-- Git 仓库权限
CREATE TABLE IF NOT EXISTS git_repo_permissions (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  repo_id       TEXT NOT NULL REFERENCES git_repos(id) ON DELETE CASCADE,
  member_id     TEXT NOT NULL REFERENCES members(id) ON DELETE CASCADE,
  permission    TEXT NOT NULL DEFAULT 'read',  -- read, write, admin
  granted_by    TEXT NOT NULL,
  granted_at    TEXT NOT NULL,
  UNIQUE(repo_id, member_id)
);
```

### 3.3 路由设计

```rust
// Git Smart HTTP 协议路由
.route("/git/:repo/info/refs", get(info_refs))
.route("/git/:repo/git-upload-pack", post(git_upload_pack))  // git clone/fetch
.route("/git/:repo/git-receive-pack", post(git_receive_pack))  // git push

// Git 操作 API
.route("/git/repos", get(list_repos).post(create_repo))
.route("/git/repos/:repo", get(get_repo).delete(delete_repo))
.route("/git/repos/:repo/permissions", get(list_permissions).post(grant_permission))
```

### 3.4 认证流程

```
┌──────────────┐     ┌──────────────┐     ┌──────────────┐
│   Client     │     │   TeamX      │     │   Git        │
│   (git)      │     │   Server     │     │   Service    │
└──────┬───────┘     └──────┬───────┘     └──────┬───────┘
       │                    │                    │
       │  1. HTTPS Request  │                    │
       │  (mTLS cert)       │                    │
       │───────────────────>│                    │
       │                    │                    │
       │  2. Extract CN     │                    │
       │  (member_id)       │                    │
       │                    │                    │
       │  3. Check Perm     │                    │
       │───────────────────>│                    │
       │                    │  4. Query DB       │
       │                    │───────────────────>│
       │                    │                    │
       │  5. Allow/Deny     │                    │
       │<───────────────────│                    │
       │                    │                    │
       │  6. Git Operation  │                    │
       │────────────────────────────────────────>│
       │                    │                    │
       │  7. Result         │                    │
       │<────────────────────────────────────────│
```

### 3.5 权限模型

| 权限级别 | 允许操作 | 说明 |
|---------|---------|------|
| `read` | clone, pull, fetch | 只读访问 |
| `write` | read + push | 读写访问 |
| `admin` | write + 管理权限 | 完全控制 |

## 4. 实现步骤

### 4.1 第一阶段：基础框架（1-2 周）

1. **添加依赖**
   ```toml
   [dependencies]
   git2 = "0.18"
   ```

2. **创建模块结构**
   - `crates/teamx/src/git_service.rs` - 核心 git 服务
   - `crates/teamx/src/git_handlers.rs` - HTTP 处理程序

3. **数据库迁移**
   - 添加 `git_repos` 表
   - 添加 `git_repo_permissions` 表

### 4.2 第二阶段：核心功能（2-3 周）

1. **实现 Git Service**
   - 仓库创建/删除
   - 仓库列表查询
   - 权限管理

2. **实现 HTTP Handlers**
   - `/git/:repo/info/refs` - 服务发现
   - `/git/:repo/git-upload-pack` - 克隆/拉取
   - `/git/:repo/git-receive-pack` - 推送

3. **集成 mTLS 认证**
   - 从证书 CN 提取 member_id
   - 验证仓库访问权限

### 4.3 第三阶段：高级功能（1-2 周）

1. **仓库管理 API**
   - 创建仓库
   - 删除仓库
   - 列出仓库
   - 管理权限

2. **日志和审计**
   - 记录所有 git 操作
   - 操作审计日志

3. **错误处理**
   - 详细的错误信息
   - 错误恢复机制

## 5. 风险评估

### 5.1 技术风险

| 风险 | 影响 | 缓解措施 |
|------|------|---------|
| libgit2 编译问题 | 构建失败 | 使用预编译版本或静态链接 |
| 性能瓶颈 | 大仓库操作慢 | 实现缓存和异步处理 |
| 内存使用 | 大仓库内存占用高 | 实现流式处理和内存限制 |
| 并发冲突 | 数据损坏 | 使用文件锁和事务 |

### 5.2 安全风险

| 风险 | 影响 | 缓解措施 |
|------|------|---------|
| 权限绕过 | 未授权访问 | 严格的权限验证 |
| 路径遍历 | 文件泄露 | 路径规范化和验证 |
| DoS 攻击 | 服务不可用 | 限流和资源限制 |

### 5.3 维护风险

| 风险 | 影响 | 缓解措施 |
|------|------|---------|
| 依赖更新 | 兼容性问题 | 固定版本和定期更新 |
| 功能膨胀 | 复杂度增加 | 模块化设计和文档 |

## 6. 工作量估算

| 阶段 | 工作内容 | 工时（人天） |
|------|---------|------------|
| 第一阶段 | 基础框架 | 5-10 |
| 第二阶段 | 核心功能 | 10-15 |
| 第三阶段 | 高级功能 | 5-10 |
| 测试和文档 | 测试、文档、部署 | 5-10 |
| **总计** | | **25-45** |

## 7. 结论和建议

### 7.1 推荐方案

**使用 git2-rs 实现 teamx server 的 git 集成功能**，理由如下：

1. **技术契合度高**：与 teamx 的 Rust 生态一致
2. **功能完整**：支持所有标准 git 操作
3. **集成度高**：可以深度集成到 teamx 的认证和权限系统
4. **可维护性好**：使用成熟的开源库，社区支持

### 7.2 实施建议

1. **分阶段实施**：先实现基础功能，再逐步完善
2. **充分测试**：特别是并发和边界情况
3. **文档完善**：提供详细的 API 文档和使用指南
4. **性能监控**：实现性能监控和告警

### 7.3 替代方案

如果 git2-rs 的编译或依赖问题难以解决，可以考虑：
1. **git-http-backend**：作为快速原型方案
2. **gitolite**：作为外部依赖方案

## 8. 附录

### 8.1 参考资料

- [git2-rs 文档](https://docs.rs/git2/)
- [Git Smart HTTP 协议](https://git-scm.com/docs/http-protocol)
- [libgit2 文档](https://libgit2.org/docs/)

### 8.2 示例代码

```rust
// 创建裸仓库
fn create_bare_repo(path: &Path) -> Result<Repository, git2::Error> {
    Repository::init_bare(path)
}

// 克隆仓库
fn clone_repo(url: &str, path: &Path) -> Result<Repository, git2::Error> {
    Repository::clone(url, path)
}

// 拉取更新
fn pull(repo: &Repository) -> Result<(), git2::Error> {
    let mut remote = repo.find_remote("origin")?;
    remote.fetch(&["main"], None, None)?;
    // ... 合并逻辑
    Ok(())
}
```

## 9. 实现落地（已实现）

> 2026-08-28 更新：核心 MVP 已实现并端到端验证通过。

### 9.1 最终方案：git bundle 传输 + 系统 git binary

实际实现采用了**不同于调研初稿的方案**，避免了引入 libgit2 依赖：

- **传输**：git bundle（base64）通过既有 mTLS JSON-RPC 通道传输
- **server 端**：裸仓库存于 `~/.teamx/repos/<team_id>/<name>.git`，用系统 `git` binary 完成 `init --bare` / `bundle create` / `bundle verify` + `fetch`
- **client 端**：本地用系统 `git` binary 执行 `init` / `fetch bundle` / `checkout` / `commit` / `bundle create`
- **授权**：`~/.teamx/certs/` 的 mTLS 客户端证书 CN（`member:<id>:<role>`）→ 映射成员 → 查 `git_repo_permissions`

### 9.2 已实现的命令

```bash
# 仓库管理（owner 创建 / admin 删除）
teamx git create <name> [--description <desc>]
teamx git delete <name>
teamx git list
teamx git grant <name> <member_id> [--permission read|write|admin]
teamx git permissions <name>

# 日常操作（在克隆的目录内）
teamx git clone <repo> [--directory <dir>]
teamx git pull <repo> [--branch <branch>]
teamx git push <repo> [--branch <branch>]
teamx git commit -m <msg>            # 纯本地，无需网络
teamx git commit-push -m <msg>       # commit + push 一步完成
```

### 9.3 权限模型（已实现）

| 权限 | clone/pull | push | 授权/删除 |
|------|:---:|:---:|:---:|
| read | ✅ | ❌ | ❌ |
| write | ✅ | ✅ | ❌ |
| admin | ✅ | ✅ | ✅ |

创建仓库仅限 **team owner**；授权/删除仅限 **admin**。

### 9.4 端到端验证结果

在 `TEAMX_HOME=/tmp/teamx-git-test` 的独立环境中全部通过：

1. ✅ `teamx team create` → `teamx cert issue` → `teamx serve` 启动
2. ✅ `teamx git create demo-repo` 创建裸仓库（磁盘可见）
3. ✅ `git bundle` RPC 空仓库返回 `empty:true`
4. ✅ 本地 commit → `teamx git push` → server 收到 commit
5. ✅ 第二台 clone 拿到全部文件与历史
6. ✅ `teamx git pull` 快进合并成功
7. ✅ `teamx git commit-push` 一步完成
8. ✅ 无权限成员 clone 被拒（`read permission required`）
9. ✅ 授权 read 后可 clone，但 push 被拒（`write permission required`）
10. ✅ 授权 write 后 push 成功
11. ✅ 非 owner 创建仓库被拒（`only the team owner can create repositories`）

### 9.5 已知限制

- **全量 bundle**：每次 clone/pull 传输全量 bundle，大仓库效率低（后续可做增量 `--since=`）
- **非 fast-forward 冲突**：server 端 `git fetch` 会拒绝非快进，客户端需要手动解决
- **bundle 大小**：JSON base64 有 ~33% 膨胀，超大仓库不建议
- **并发 push**：两个成员同时 push 时，后到者若基于旧 commit 会被拒绝（符合 git 语义）

### 9.6 标准 Git Smart HTTP over mTLS（推荐连接方式）

> 2026-08-29 更新：改为标准 git 协议，用户可用原生 `git` 客户端。

**Server 端**（`git_service.rs` + `serve.rs`）实现 Git Smart HTTP 端点，复用系统 git plumbing：

| 端点 | 用途 | 权限 |
|------|------|------|
| `GET /git/<team>/<repo>/info/refs?service=git-upload-pack` | clone/fetch advertisement | read |
| `GET /git/<team>/<repo>/info/refs?service=git-receive-pack` | push advertisement | write |
| `POST /git/<team>/<repo>/git-upload-pack` | clone/fetch/pull | read |
| `POST /git/<team>/<repo>/git-receive-pack` | push | write |

认证仍复用 mTLS：客户端证书 CN → member_id → 查 `git_repo_permissions`。实现了 `# service=<name>` pkt-line advertisement 前缀。

**Client 端**（`teamx git setup`）：

```bash
# 一次性配置：从 invitation letter 私有目录读证书写入 ~/.gitconfig
teamx git setup --server https://server
# 之后就是普通 git（自动 mTLS，零证书参数）
git clone https://server/git/<team_id>/<repo>
git pull && git push
```

写入的 per-URL 配置：`http.<server>/.sslCert/.sslKey/.sslCAInfo`（注意 key 需用 `/.` 前缀，`git config` CLI 不接受含 `/` 的 key，需 `--file ~/.gitconfig`）。

### 9.7 团队自动化（create → repo，import → clone）

> 2026-08-30 更新：三个自动化点。

1. **team create 自动建 repo**：owner 在项目目录执行 `teamx team create` 时，自动用当前目录内容创建团队 git repo（repo 名 = sanitized team 名），初始 commit `initial import (teamx team create)`。自动跳过 `.git/.teamx/target/node_modules/vendor/dist/.build`。
2. **approve 自动授权**：owner approve 新成员时，自动给该成员授予团队所有 git repo 的 `read` 权限，之后即可 clone/pull。
3. **import 自动 clone 指引**：`teamx team import` 返回 `git_repos` + `server_url` + `clone_hint`；opencode 插件的 `teamx_team_import` 成功后自动执行 `teamx git setup`，用户批准后即可 `git clone`。

**验证**（`TEAMX_HOME=/tmp/teamx-f1` 独立环境）：
- ✅ `team create "My Project"` → 自动生成 `my-project.git`（含 main.rs/src/lib.rs/.gitignore + initial commit）
- ✅ import 返回 `git_repos: ['my-project']`
- ✅ approve dev2 → 自动获得 `read` 权限（permissions 可查）
- ✅ dev2 `git clone`（自动 mTLS）成功拿到代码


