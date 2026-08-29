# teamx git list

列出当前成员可访问的 git 仓库。

## 用法

```
teamx git list [--team <id>]
```

## 参数

- `--team <id>` - 团队 ID（当会话属于多个团队时需要）

## 示例

```bash
# 列出可访问的仓库
teamx git list

# 列出指定团队的仓库
teamx git list --team team-123
```

## 输出

```json
{
  "ok": true,
  "repos": [
    {
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
  ]
}
```
