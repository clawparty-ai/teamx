# teamx git grant

授予团队成员对仓库的访问权限（仅 owner/admin）。

## 用法

```
teamx git grant <name> <member_id> [--permission <read|write|admin>] [--team <id>]
```

## 参数

- `name` - 仓库名称
- `member_id` - 要授权的成员 ID
- `--permission <level>` - 权限级别：`read`（clone/pull）、`write`（+push）、`admin`（+管理），默认 `read`
- `--team <id>` - 团队 ID

## 权限级别

| 级别 | 允许操作 |
|------|---------|
| read | clone, pull |
| write | read + push |
| admin | write + 授权/删除 |

## 示例

```bash
# 授予读权限
teamx git grant demo-repo <member_id>

# 授予写权限
teamx git grant demo-repo <member_id> --permission write
```

## 输出

```json
{
  "ok": true
}
```
