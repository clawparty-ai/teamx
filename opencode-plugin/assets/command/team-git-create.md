# teamx git create

在 teamx 服务器上创建新的 git 仓库（仅 owner/admin）。

## 用法

```
teamx git create <name> [--description <desc>] [--team <id>]
```

## 参数

- `name` - 仓库名称
- `--description <desc>` - 仓库描述（可选）
- `--team <id>` - 团队 ID（当会话属于多个团队时需要）

## 示例

```bash
# 创建仓库
teamx git create my-project --description "My project repository"

# 创建仓库并指定团队
teamx git create my-project --team team-123
```

## 权限

- 需要团队 owner 或 admin 角色
- 创建者自动获得 admin 权限

## 输出

```json
{
  "ok": true,
  "repo": {
    "id": "uuid",
    "team_id": "team-123",
    "name": "my-project",
    "path": "/team-123/my-project.git",
    "description": "My project repository",
    "is_bare": true,
    "created_by": "member-id",
    "created_at": "2026-08-28T...",
    "updated_at": "2026-08-28T..."
  }
}
```
