# teamx git pull

从 teamx 服务器拉取 git 仓库更新。

## 用法

```
teamx git pull <repo> [--branch <branch>] [--team <id>]
```

## 参数

- `repo` - 仓库名称
- `--branch <branch>` - 分支名称（默认：当前分支）
- `--team <id>` - 团队 ID（当会话属于多个团队时需要）

## 示例

```bash
# 拉取更新
teamx git pull my-project

# 拉取指定分支
teamx git pull my-project --branch develop

# 拉取指定团队的仓库
teamx git pull my-project --team team-123
```

## 权限

- 需要仓库的 read 权限

## 输出

```json
{
  "ok": true,
  "repo": "my-project",
  "branch": "main",
  "message": "Git pull operation (placeholder)"
}
```

## 注意

- 当前为占位实现，实际 git 操作待完善
- 需要先克隆仓库才能拉取更新
