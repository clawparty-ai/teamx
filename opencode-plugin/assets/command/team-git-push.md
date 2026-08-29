# teamx git push

推送本地更改到 teamx 服务器的 git 仓库。

## 用法

```
teamx git push <repo> [--branch <branch>] [--team <id>]
```

## 参数

- `repo` - 仓库名称
- `--branch <branch>` - 分支名称（默认：当前分支）
- `--team <id>` - 团队 ID（当会话属于多个团队时需要）

## 示例

```bash
# 推送更改
teamx git push my-project

# 推送到指定分支
teamx git push my-project --branch develop

# 推送到指定团队的仓库
teamx git push my-project --team team-123
```

## 权限

- 需要仓库的 write 权限

## 输出

```json
{
  "ok": true,
  "repo": "my-project",
  "branch": "main",
  "message": "Git push operation (placeholder)"
}
```

## 注意

- 当前为占位实现，实际 git 操作待完善
- 需要先克隆仓库并有本地更改才能推送
