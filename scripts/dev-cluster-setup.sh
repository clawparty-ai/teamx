#!/usr/bin/env bash
# 开发/测试集群接入脚本（design/013 第一步：k8e + member 容器化）
#
# 在**台式机（BYO 主机）**上运行：装容器运行时 + 单节点 k8s（k8e 优先，k3s 兜底）
# + 拉团队源码 + 构建 member 镜像，最后打印 provider 需要的环境变量。
#
# 用法：
#   TEAM_REPO_URL=https://<server>/git/<team_id>/<repo> \
#   TEAMX_LETTER=/path/to/invitation.letter \
#   bash scripts/dev-cluster-setup.sh
#
# 可选环境变量：
#   DISTRO=k8e|k3s         集群发行版（默认 k8e，装不上则提示改用 k3s）
#   MEMBER_IMAGE=teamx-member:dev
#   BASE_IMAGE / APT_MIRROR  受限网络下的镜像源（见 docker/member/Dockerfile）
#   TEAMX_SRC_DIR=~/teamx-enterprise
set -euo pipefail

DISTRO="${DISTRO:-k8e}"
MEMBER_IMAGE="${MEMBER_IMAGE:-teamx-member:dev}"
SRC="${TEAMX_SRC_DIR:-$HOME/teamx-enterprise}"
TEAM_REPO_URL="${TEAM_REPO_URL:-}"
TEAMX_LETTER="${TEAMX_LETTER:-}"

say() { printf '\n=== %s ===\n' "$*"; }
ok()  { printf '  ok: %s\n' "$*"; }
die() { echo "FAIL: $*" >&2; exit 1; }

say "0) 前置检查"
command -v curl >/dev/null || die "需要 curl"
command -v git  >/dev/null || die "需要 git"
ARCH="$(uname -m)"
echo "  arch=$ARCH distro=$DISTRO image=$MEMBER_IMAGE src=$SRC"
case "$ARCH" in aarch64|arm64|x86_64|amd64) ;; *) die "不支持的架构 $ARCH" ;; esac
ok "前置检查通过"

say "1) 容器运行时（docker）"
if command -v docker >/dev/null; then
  ok "docker 已安装：$(docker --version 2>/dev/null | head -1)"
else
  curl -fsSL https://get.docker.com | sh || die "docker 安装失败（受限网络请用发行版仓库安装）"
  ok "docker 已安装"
fi
# 免 sudo（重新登录生效）
sudo usermod -aG docker "$USER" 2>/dev/null || true

say "2) 单节点 k8s（$DISTRO）"
if command -v kubectl >/dev/null && kubectl get nodes >/dev/null 2>&1; then
  ok "集群已就绪：$(kubectl get nodes --no-headers | head -1)"
else
  case "$DISTRO" in
    k8e)
      # k8e（xiaods/k8e）现在是一个 <100MB 单二进制「Agentic AI Sandbox Matrix」，
      # 内置 **Cilium**（默认 CNI，执行标准 k8s NetworkPolicy —— 优于 k3s 的 flannel）。
      # 安装源 2026-09 已从 get.k8e.io 迁移到 k8e.sh/install.sh（旧域名 DNS 已失效）。
      # 若本机拿不到安装脚本，可改用 DISTRO=k3s 再跑一次（但 k3s 默认 flannel 不执行
      # NetworkPolicy，须如实声明安全降级，见 design/013 M0）。
      if curl -fsSL https://k8e.sh/install.sh 2>/dev/null | sh -; then
        ok "k8e 安装脚本已执行"
      else
        die "k8e 安装脚本不可达（k8e.sh）—— 用 DISTRO=k3s 重跑，或在能访问 GitHub 的网络下预装"
      fi
      ;;
    k3s)
      curl -sfL https://get.k3s.io | sh -s - --write-kubeconfig-mode 644
      ok "k3s 已安装"
      ;;
    *) die "未知 DISTRO=$DISTRO（k8e|k3s）" ;;
  esac
  sleep 5
  kubectl get nodes >/dev/null 2>&1 || die "集群未就绪，检查 $DISTRO 服务日志"
  ok "集群就绪：$(kubectl get nodes --no-headers | head -1)"
fi

say "3) 团队源码"
if [ -d "$SRC/.git" ]; then
  git -C "$SRC" pull --rebase >/dev/null 2>&1 || true
  ok "已存在，已尝试更新：$SRC"
elif [ -n "$TEAM_REPO_URL" ]; then
  # mTLS 克隆：先 import 邀请函（会写好 git 的 client cert 配置）
  [ -n "$TEAMX_LETTER" ] || die "需要 TEAMX_LETTER 才能用 mTLS 克隆团队仓库"
  command -v teamx >/dev/null || die "需要 teamx CLI（先安装二进制）"
  teamx team import "$TEAMX_LETTER" --session "net:dev-desktop" >/dev/null || true
  git clone "$TEAM_REPO_URL" "$SRC"
  ok "已克隆到 $SRC"
else
  die "没有源码：设 TEAM_REPO_URL（+ TEAMX_LETTER）或用 TEAMX_SRC_DIR 指向已有克隆"
fi

say "4) 构建 member 镜像"
cd "$SRC"
[ -x target/release/teamx ] || cargo build --release -p teamx
docker build -f docker/member/Dockerfile \
  ${BASE_IMAGE:+--build-arg BASE_IMAGE="$BASE_IMAGE"} \
  ${APT_MIRROR:+--build-arg APT_MIRROR="$APT_MIRROR"} \
  --build-arg TEAMX_BIN=target/release/teamx \
  -t "$MEMBER_IMAGE" .
ok "镜像已构建：$MEMBER_IMAGE"

say "5) 就绪 —— 让 provider 指向这台机器"
cat <<EOF

  在**运行 teamx provider 的机器**上设置：

    export TEAMX_KUBECONFIG=$HOME/.kube/config        # 或 k3s 的 /etc/rancher/k3s/k3s.yaml
    export TEAMX_K8S_CONTEXT=$(kubectl config current-context)
    export TEAMX_MEMBER_IMAGE=$MEMBER_IMAGE
    export TEAMX_K8S_EGRESS_ALLOW=<server 可达的 CIDR 白名单，逗号分隔，可空>

  然后：
    teamx node provision --member <id> --provider k8e --region <任意标识>
    teamx node run-jobs           # 建 PVC/Secret/Quota
    teamx team approve <member>   # 建 member Pod（= 启动服务）

  验证：
    bash tests/k8e-provision-test.sh        # 零集群（stub kubectl）
    TEAMX_K8E_CONTEXT=\$(kubectl config current-context) bash tests/k8e-real-cluster-test.sh
EOF
ok "完成"
