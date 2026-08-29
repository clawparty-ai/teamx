# teamx git clone

从 teamx 服务器克隆 git 仓库到本地。

## 用法

```
teamx git clone <repo> [--directory <dir>] [--team <id>]
```

## 参数

- `repo` - 仓库名称
- `--directory <dir>` - 本地目录（默认：仓库名称）
- `--team <id>` - 团队 ID（当会话属于多个团队时需要）

## 示例

```bash
# 克隆仓库
teamx git clone my-project

# 克隆到指定目录
teamx git clone my-project --directory ./local-project

# 克隆指定团队的仓库
teamx git clone my-project --team team-123
```

## 权限

- 需要仓库的 read 权限

## 输出

```json
{
  "ok": true,
  "repo": "my-project",
  "directory": "my-project",
  "message": "Git clone operation (placeholder)"
}
```

## 注意

- 当前为占位实现，实际 git 操作待完善
- 需要 teamx server 运行并配置 git 服务
