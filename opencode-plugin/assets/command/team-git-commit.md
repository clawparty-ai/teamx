# teamx git commit

在 teamx 克隆的仓库中提交本地更改（`git add -A` + `git commit`）。

## 用法

```
teamx git commit -m <message> [--dir <dir>]
```

## 参数

- `-m <message>` - 提交信息
- `--dir <dir>` - 工作目录（默认：当前目录）

## 示例

```bash
# 在当前目录提交所有更改
teamx git commit -m "add feature"

# 在指定目录提交
teamx git commit -m "fix bug" --dir /path/to/repo
```

## 说明

- 这是**本地操作**，不经过网络
- 需要在一个 teamx 克隆的仓库中运行（含 `.git/teamx-origin.json`）
- 提交后需要 `teamx git push` 才能上传到服务器

## 输出

```json
{
  "ok": true,
  "message": "[main f1f7144] add feature\n 1 file changed, 1 insertion(+)"
}
```
