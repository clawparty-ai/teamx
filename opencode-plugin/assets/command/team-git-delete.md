# teamx git delete

从 teamx 服务器删除 git 仓库（仅 owner/admin）。

## 用法

```
teamx git delete <name> [--team <id>]
```

## 参数

- `name` - 仓库名称
- `--team <id>` - 团队 ID（当会话属于多个团队时需要）

## 示例

```bash
# 删除仓库
teamx git delete my-project

# 删除指定团队的仓库
teamx git delete my-project --team team-123
```

## 权限

- 需要仓库的 admin 权限
- 操作不可逆，请谨慎使用

## 输出

```json
{
  "ok": true
}
```
