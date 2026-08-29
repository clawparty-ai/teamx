# teamx git commit-push

提交本地更改并推送到 teamx 服务器，一步完成。

## 用法

```
teamx git commit-push -m <message> [--repo <name>] [--branch <branch>] [--dir <dir>] [--team <id>]
```

## 参数

- `-m <message>` - 提交信息
- `--repo <name>` - 仓库名称（默认：克隆时的仓库）
- `--branch <branch>` - 分支（默认：当前分支）
- `--dir <dir>` - 工作目录（默认：当前目录）
- `--team <id>` - 团队 ID

## 示例

```bash
# 提交并推送当前更改
teamx git commit-push -m "add feature"

# 指定仓库和团队
teamx git commit-push -m "fix bug" --repo my-repo --team team-123
```

## 说明

- 先本地 `git add -A` + `git commit`，再通过 mTLS 上传 bundle
- 需要仓库的 **write** 权限
- 对应 opencode 的 `teamx_git_commit_push` 工具和 `/team-git-commit-push` 命令

## 输出

```json
{
  "ok": true,
  "commit": "[main 507705d] README update\n 1 file changed",
  "push": "pushed",
  "repo": "demo-repo"
}
```
