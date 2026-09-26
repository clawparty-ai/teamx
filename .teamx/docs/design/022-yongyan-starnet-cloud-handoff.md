# 022 · Handoff：yongyan-dev 收口 + starnet / yongyan-team 物理隔离迁云 + 新日本机 b2-server

> 用途：本文件是**跨会话交接单**。新会话读本文件后即可接手执行；本文件自带 goal 原文，
> 因为 **session goal 不会被新会话继承**。
> 维护：owner 的团队台账仓 `<ledger>/.teamx/docs/design/022-*.md`，并镜像到本仓 `.teamx/docs/design/022-*.md`（两处 diff 必须为 0）。

## §0 先做：set_goal（原文照抄）

**objective**
```
yongyan-dev 全量上云收口；starnet 与 yongyan-team（原 HK 的 powernano-v2）在 teamx cloud 上做物理隔离迁移
（各自独立 namespace/PVC/server Deployment/证书/子域），并把这两个队从 yongyan-dev 租户中摘除以杜绝 split-brain；
新日本主机只跑 b2-server（用研智能体前台+后台）；按序销毁旧日/韩主机（含已授权放弃的 clawparty-seoul）。
```

**successCriteria**
```
1) teamx cloud 上 starnet（HK 队 5e59132f-d5fb-48e5-a90a-33a3ff0c7068，4 成员，仓库 87M）与
   yongyan-team（HK 队 1da9a7f0-da3e-4f8c-90e3-7147f27bc25b，6 成员，仓库 11G）各有独立
   namespace/PVC/Deployment/证书/NodePort/子域 + 独立 tenants 行；成员来信沿用 HK CA（无需重新导信）可正常连接。
2) 无 split-brain：这两队的行已从 yongyan-dev 租户（f2adafd3-b3d7-4a84-838e-d33a53c2640c）移除。
3) yongyan-team 下 5 个用研 opencode agent（yy-trend/yy-patent/yy-paper/yy-general/reviewer）作为云 member
   Running 且 opencode 可用。
4) 新日本主机 b2-server：HTTPS 前台可访问、/api/v1 健康、经云租户连通、知识库检索命中（各有命令+输出证据）。
5) 旧主机按序销毁且每台先做 EBS 快照：clawparty-seoul → starnet-member-jp/us → powernano-zhaoz →
   最后 powernano-zhongm；HK 源机 95.40.221.159 绝不销毁。
6) 台账/文档（部署手册、凭据清单、备份告警）+ 可复现验证证据。
```

**constraints**
```
HK 源机 95.40.221.159 绝不动、不销毁；销毁任何实例前必须有 EBS 快照 + 依赖核对；
云上生产（teamx.beer / yongyan 租户 / 4 个 member Pod）不得中断，改动只做加法；
密码/私钥不入仓库与文档；不 push 本地 enterprise 分支；禁词约束（C 开头冲突代理工具名）；
每阶段结束必须留可复现证据；need owner input 时明确说明再停。
```

## §1 硬约束与红线
- **HK 源机（95.40.221.159 / i-050e785daef10627e / powernano）只读，永不销毁**。
- 销毁前：EBS 快照 + 依赖核对（DNS / teamx 隧道 / 成员本地配置 / letters 是否仍指向它）。
- 已授权放弃：`clawparty-seoul`（i-0dd9eabd667d46b52, ap-northeast-2, stopped）。
- 物理隔离（owner 明确裁决）：starnet 与 yongyan-team 各自独立命名空间/PVC/Deployment；**不做共享命名空间**。
- 因此必须把这两队从 yongyan-dev 租户（f2adafd3 的库）里摘除，否则同一队存在于两套 server = split-brain。

## §2 访问方式
- **云主机**：`ssh -o StrictHostKeyChecking=no -i ~/.ssh/id_ed25519 ubuntu@18.225.44.71`（AWS us-east-2，账号 975049986480，Elastic IP）。
- **集群**：`sudo -n /usr/local/bin/k8e kubectl`；kubeconfig `/home/ubuntu/.kube/config`；context `default`。
- **平台服务**：systemd `teamx-cloud.service`（User=ubuntu；`TEAMX_DB=/home/ubuntu/teamx-cloud/teamx.db`；`TEAMX_HOME=/home/ubuntu/teamx-cloud/home`；`TEAMX_KUBECONFIG=/home/ubuntu/.kube/config`；`TEAMX_SERVER_IMAGE=teamx-server:dev6`；`TEAMX_MEMBER_IMAGE=teamx-member:dev4`；`TEAMX_TENANT_PVC_SIZE=20Gi`；ExecStart=`/usr/local/bin/teamx site --addr 127.0.0.1 --port 9443`）。改 systemd 后 `daemon-reload` + `restart`。
- **旧机 ssh 别名**（均在 `~/.ssh/config`，密钥 `teamx-powernano.pem`）：`nc-hk`=95.40.221.159（HK 源机）、`nc-tokyo`=35.77.5.99（powernano-zhongm）、`starnet-jp`=18.177.87.118、`starnet-us`=44.234.64.4。zhaoz（3.37.16.165）本机直连超时，**经 nc-hk 跳转**。
- **HK 端**：CA 在 `/root/.teamx/ca`；DB `/var/lib/teamx/teamx.db`（**HK 无 sqlite3 CLI，用 `python3 -c "import sqlite3..."`**）；repos `/root/.teamx/repos/<team_id>`。
- **nginx/证书**：`/etc/nginx/teamx-stream.conf`（443 SNI 分流）+ `/etc/nginx/conf.d/`；certbot webroot（`/var/www/certbot`）+ deploy hook `/etc/letsencrypt/renewal-hooks/deploy/teamx.sh`。
- **代码仓**：`/Users/caishu/github/teamx-enterprise`（单 crate `crates/teamx`；本仓单测 469 + clippy `-D warnings` 干净；push `git push enterprise-origin main:main`）。
- **镜像**：改了代码后需重建镜像并**换 tag**（同 tag 不会重拉），再 `docker save | sudo /usr/local/bin/k8e ctr -n k8s.io images import -`。

## §3 凭据（不在本文件里，owner 在会话里提供）
- 平台用户：`caishu`（uid `4087ae63-8494-4865-99e5-7904d62498f7`，owner）+ `zhongm` / `zhaoz` / `caicai` / `demo`；密码由 owner 在会话中给出（不入库）。
- 平台 RPC 会话 token：优先用 owner 提供的；否则用平台登录重新获取（`POST /api/login/password` → cookie 可用于 `/api/my/*`）。
- 模型凭据：云机 `/etc/teamx/llm/{auth.json,opencode.json}`（root:ubuntu 640），勿复制到别处。

## §4 现状（已完成，均有真机证据）
1. **yongyan-dev 全量在云上跑通**：租户 `tenant-f2adafd3b3d74a84`；4 个云 member Pod 6/6 Running；平台反代路由 `/c/<member16>` 已上线（有会话 200 `Teamx Console`，无会话 303 `/console`，URL 无 token）；credit/TCoin/成员 console 均可用。
2. **starnet 物理隔离租户骨架已就绪**：
   - 平台 `teams` 行：`id=5e59132f-d5fb-48e5-a90a-33a3ff0c7068`，`name=starnet`，`state=active`。
   - `namespace=tenant-5e59132fd5fb48e5`；`PVC=teamx-serve-data-5e59132fd5fb48e5`；`Deployment=teamx-serve-5e59132fd5fb48e5`；`Secret=teamx-ca-5e59132fd5fb48e5`；server pod 1/1 Running（日志 `tenant mode (team 5e59132f…), db /data/teamx.db, listening https://0.0.0.0:5783 (mtls)`）。
   - **CA 已换成 HK CA**（sha256 `2F:31:B8:DA:02:72:0F:63:0F:E7:26:C8:0A:03:09:D4:0B:77:D5:C4:E4:00:F0:9E:8A:16:38:B7:8F:81:84:96`）；server cert 用 HK CA 重签（issuer `CN=teamx-ca`，SAN 含 `starnet.teamx.beer` + svc 名 + localhost/127.0.0.1）；`tenants.ca_fingerprint` 已同步。
   - 备忘：yongyan 的 server cert SAN 还含 `18.225.44.71` 与 `t-f2adafd3b3d74a84.teamx.beer`，starnet 证书暂缺，按需补签。
3. **starnet 单队子集库已生成并传到云机**：`/tmp/starnet-subset.db`（3.2MB；4 成员 / 1096 事件 / 1 goal / 1 repo；已剪掉其他队 12604 事件、65412 activity、19523 连接等）。
4. HK 队清单（源）：yongyan-dev `f2adafd3`(active,4) · powernano-v2 `1da9a7f0`(active,6) · starnet `5e59132f`(completed,4) + 历史队。starnet 的机器 `starnet-jp` 的 `TEAMX_SERVER_URL=https://95.40.221.159`。
5. 云租户原为 HK 整机拷贝，5 个仓库目录都在（含 starnet 87M、powernano-v2 11G），所以这两队**无需重新拷数据源**（可从 yongyan 租户 PVC 内取）。
6. **starnet 租户数据已就位**：子集库（3,231,744 B，仅 starnet：4 成员/1096 事件/1 goal/1 repo）已灌入 PVC `/data/teamx.db`；仓库 `87M /data/repos/5e59132f-.../starnet.git` 已灌入；server pod Running 且日志为
   `teamx serve: tenant mode (team 5e59132f-...), identity from /data/ca` + `listening https://0.0.0.0:5783 (mtls, db /data/teamx.db)`。
   平台 tenants 行现状：`owner_member_id` 为空、`endpoint` 为空（待配）。

## §5 下一步：starnet 暴露 + 登记（数据已就位，从这里接续）
> 已完成：隔离租户骨架（ns/PVC/Deployment/Secret）+ HK CA 换入 + server cert 重签 + 平台 teams/tenants 行 + 子集库与 87M 仓库入 PVC。
> 完成方式备忘：临时 Pod 必须带 `restricted` 合规 securityContext（见 §7 坑 1）；**大文件别用 `kubectl exec -i` 灌（90MB 会失败），用 `kubectl cp`**。

1. **暴露**：给该租户建 NodePort（参考 yongyan 租户的 `30443` 模式，或按需另取）；nginx `teamx-stream.conf` 加 `starnet.teamx.beer → 127.0.0.1:<NodePort>`；Let's Encrypt 证书（webroot + deploy hook 条目）。
2. **平台登记**：设 `tenants.owner_member_id`（= HK 库 starnet 的 lead 席位 id）+ `tenants.endpoint`（`https://starnet.teamx.beer` 或同机 port-forward 端点，参照 yongyan 租户的取值方式）；按 HK 库的 4 个 starnet 成员补 `team_join_requests`（`user_id` 指向平台用户、`status='approved'`、`member_id`=席位 id）；补 `owner_letter`（`teamx tenant owner-letter`，平台控制台"已初始化"判定用）。
3. **验证**：用 starnet 成员来信（HK CA 签的旧信，无需重发）经 `https://starnet.teamx.beer` 跑一次 mTLS 调用（如 `teamx status` 或 service 车道名册）确认租户正常服务该队。
4. **摘除**：在 yongyan-dev 租户库（`tenant-f2adafd3b3d74a84` 的 `/data/teamx.db`）删除 starnet 队行（以及第 5 步的 pnv2 行），杜绝 split-brain；摘除后回归一次 yongyan-dev 自身功能。
5. **铺成员**：`teamx --json node provision-cloud --team 5e59132f-d5fb-48e5-a90a-33a3ff0c7068 --all --session <token>`。
6. **yongyan-team（原 powernano-v2）**：按同一套流程再做一遍（独立租户，PVC 建议 ≥40Gi 因仓库 11G；子集库按 `team_id=1da9a7f0-...` 剪裁；队名可改为 `yongyan-team`）。

## §6 后续阶段（按序）
1. **starnet 暴露**：nginx stream 加 `starnet.teamx.beer → 127.0.0.1:<NodePort>`；租户 Pod 侧需 NodePort（或复用 443 的 SNI 分流 + 一个 NodePort）；Let's Encrypt 证书（webroot + deploy hook 条目）。
2. **starnet 平台席位**：`tenants.owner_member_id` = HK 库 starnet 的 lead 席位；按 HK 的 4 个成员补 `team_join_requests`（`user_id` 指向平台用户）；补 `owner_letter`（平台控制台"已初始化"判定用，命令 `teamx tenant owner-letter`）。
3. **摘除**：在 yongyan-dev 租户库（`tenant-f2adafd3b3d74a84` 的 `/data/teamx.db`）删除 starnet 的队行（以及 powernano-v2 的行，见下一步），杜绝 split-brain。
4. **铺 starnet 成员**：`teamx --json node provision-cloud --team 5e59132f-d5fb-48e5-a90a-33a3ff0c7068 --all --session <token>`（该命令已在真机验证）。
5. **yongyan-team（原 powernano-v2）** 同法：平台 `teams` 行已存在（`1da9a7f0`，state active，name 可改为 `yongyan-team`）；建独立租户（PVC 建议 40Gi，因仓库 11G）→ 换 HK CA + 重签 server cert → 灌单队子集库（team_id=1da9a7f0）+ 11G 仓库 → nginx 子域（`yongyan-team.teamx.beer`）→ 席位（`tenants.owner_member_id` + `team_join_requests`）→ **从 f2adafd3 租户摘除 pnv2 行** → `provision-cloud --all`。
6. **5 个用研 agent**：在 yongyan-team 下新建 5 个席位（yy-trend / yy-patent / yy-paper / yy-general / reviewer）→ 铺成云 member（每台 Pod 自带 opencode 容器）→ 验证 `opencode serve` 可用。
7. **新日本主机（ap-northeast-1，2C4G 级）**：只跑 b2-server（Rust release 二进制 + `frontend-v2` 静态 + nginx/LE + systemd），对接 yongyan-team 云租户端点；**不要在该机编译**（2C4G 会 OOM）；验收四项：前台 HTTPS / `/api/v1` 健康 / 经租户连通 / 知识库检索命中。
8. **按序销毁（每台先 EBS 快照 + 依赖核对）**：`clawparty-seoul`(i-0dd9eabd667d46b52) → `starnet-member-jp`(i-0b06aa7a9b2035671) / `starnet-member-us`(i-0f2eefd2c35e8e813) → `powernano-zhaoz`(i-0fe794ad265be751e, 经 nc-hk 跳转盘点) → 最后 `powernano-zhongm`(i-019801448115135cd, 35.77.5.99，须先把它上面的用研栈 + owner 自己的 opencode + pnv2 的 fs 迁走)；随后释放 EIP/SG/密钥并核对账单。HK 源机不动。
9. **收尾**：台账/文档（部署手册、凭据清单、备份告警）；修 `/api/repos` 的 size bug（非 bare 仓库算 0，powernano-v2 显示 26221B 而非 ≈11G）；A 上线后清理旧 `c-*` 子域与 4 张证书；清理 HK 上 `/tmp/migrate*`、`/tmp/migrate_key`。

## §7 已知坑（踩过，勿重犯）
- 覆盖租户 DB 前**必须删 `-wal`/`-shm`**，否则 WAL 重放把旧页盖回。
- `rsync -a` 保留 mtime 会让 cargo 判定"无需重建" → 同步后 `touch`。
- 镜像更新要 `docker save | k8e ctr images import` **并换 tag**（同 tag 不重拉）。
- `pkill -f "teamx site …"` 会自杀（模式出现在 ssh 命令行）→ 用 `[t]eamx` 技巧。
- `libnginx-mod-stream` 需装；`stream{}` 必须在 nginx 顶层 include（`conf.d/*.conf` 在 http 块内）。
- k8e 自带 `sandbox-matrix` Gateway 抢 80/443 → 其清单已移到 `/root/disabled-manifests`，勿恢复。
- 成员 console 的 token 是**确定性派生** `k8s::console_token_for(member_id)`（= SHA256("teamx-console:<member>:<instance_id>")[0..32]），平台与 Pod 各自可算；member 镜像里必须支持 `TEAMX_CONSOLE_TOKEN`（dev4 起支持）。
- member console 反代 `/c/<member16>` 需成员 Pod 里跑的是含"console 支持前缀"的镜像（dev4 起）。
- `/api/repos/clone` 需要成员 console 侧 `git`（member 镜像有）+ server 侧 `git`（`teamx-server:dev6` 起有，否则报 `spawn git: No such file or directory`）。
- 全角字符紧跟 `$VAR` 要写 `${VAR}`；`UID` 是 shell 保留变量。
- 临时 Pod 会被 **PodSecurity `restricted:latest`** 拒绝 → 必须带 `allowPrivilegeEscalation:false` + `capabilities.drop:["ALL"]` + `runAsNonRoot:true` + `seccompProfile:RuntimeDefault`（PVC 属主 uid 1000，另加 runAsUser/runAsGroup/fsGroup=1000）。
- **`kubectl exec -i` 灌大文件（数十 MB）会损坏流**（90MB tar 报 "This does not look like a tar archive"）→ 用 `kubectl cp`（小文件如 3MB DB 用 exec 没问题）。
- 平台控制台 JS 的"引用尚不存在/顺序依赖"踩过多次（死引用会导致整段脚本失效）→ 改前端后务必跑 `node --check` 并保留回归断言。

## §8 关键 id 速查
- 云：Elastic IP `18.225.44.71`；平台 DB `/home/ubuntu/teamx-cloud/teamx.db`；yongyan 租户 ns `tenant-f2adafd3b3d74a84`。
- 队：yongyan-dev `f2adafd3-b3d7-4a84-838e-d33a53c2640c`；starnet `5e59132f-d5fb-48e5-a90a-33a3ff0c7068`；powernano-v2/yongyan-team `1da9a7f0-da3e-4f8c-90e3-7147f27bc25b`。
- 平台用户：caishu `4087ae63-8494-4865-99e5-7904d62498f7`；zhongm `1084c011-822b-4bf8-aea1-f6414ce2d177`；zhaoz `1a2f517c-a880-4690-b1bc-1f273889c53e`；caicai `a5f4ce9d-a96f-4df7-aaf3-2e1f63283e7a`；demo `5e17cc70-67a5-4488-b1e8-efb78367a749`。
- 实例：clawparty-seoul `i-0dd9eabd667d46b52`(ap-northeast-2,stopped) · powernano-zhaoz `i-0fe794ad265be751e`(3.37.16.165) · powernano-zhongm `i-019801448115135cd`(35.77.5.99) · starnet-member-jp `i-0b06aa7a9b2035671`(18.177.87.118) · starnet-member-us `i-0f2eefd2c35e8e813`(44.234.64.4) · powernano-caicai `i-07cc6457d3f165c5e`(us-west-2,184.34.85.90，不在"日/韩"范围) · HK `i-050e785daef10627e`(95.40.221.159，**不动**)。
