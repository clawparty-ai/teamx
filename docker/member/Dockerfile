# member 镜像（design/013 §4）—— 一个 (team, member) 座位 = 一个 Pod 的镜像。
#
# 与 design/006 的 EC2 路径**相反**：镜像**预装** teamx / opencode / ttyd / bun / git
# （容器要快启动，不能像 EC2 那样开机再装）。
#
# 构建（多架构）：
#   docker buildx build --platform linux/arm64,linux/amd64 \
#     --build-arg TEAMX_BIN=target/release/teamx -f docker/member/Dockerfile -t teamx-member:dev .
# 说明：teamx 二进制由构建上下文注入（不在镜像里编译 Rust，保持构建快且可复现）。

# syntax=docker/dockerfile:1
FROM --platform=$TARGETPLATFORM ubuntu:24.04 AS member

ARG DEBIAN_FRONTEND=noninteractive
ARG TARGETARCH
# teamx 预编译二进制（按目标架构构建后注入）
ARG TEAMX_BIN=target/release/teamx

# ---- 基础工具层（变的最少，放最前，利于缓存与拉取）----
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl git unzip xz-utils jq procps less bash \
      python3 ripgrep \
    && rm -rf /var/lib/apt/lists/*

# ---- ttyd（官方静态发行版；Ubuntu 仓库没有）----
RUN case "$TARGETARCH" in \
      arm64) A=ttyd.aarch64 ;; \
      amd64) A=ttyd.x86_64 ;; \
      *) echo "unsupported arch $TARGETARCH" >&2; exit 1 ;; \
    esac \
 && curl -fsSL -o /usr/local/bin/ttyd \
      "https://github.com/tsl0922/ttyd/releases/latest/download/$A" \
 && chmod 0755 /usr/local/bin/ttyd

# ---- bun（独立层：体积大且变动少）----
RUN curl -fsSL https://bun.sh/install | bash \
 && install -m 0755 /root/.bun/bin/bun /usr/local/bin/bun \
 && rm -rf /root/.bun

# ---- opencode（独立层：体积最大）----
RUN curl -fsSL https://opencode.ai/install | bash \
 && install -m 0755 /root/.opencode/bin/opencode /usr/local/bin/opencode \
 && rm -rf /root/.opencode

# ---- teamx（由构建上下文注入）----
COPY ${TEAMX_BIN} /usr/local/bin/teamx
RUN chmod 0755 /usr/local/bin/teamx && /usr/local/bin/teamx version | head -2

# ---- 非 root 运行（与 Pod 的 runAsUser/fsGroup 1000 对齐）----
RUN useradd -m -u 1000 -s /bin/bash teamx \
 && mkdir -p /workspace && chown -R 1000:1000 /workspace
USER 1000
ENV HOME=/home/teamx
ENV PATH=/usr/local/bin:/usr/bin:/bin
WORKDIR /workspace

# 默认不启动任何东西：Pod 的 initContainer/各容器各自覆盖 command。
CMD ["/bin/bash", "-lc", "sleep infinity"]
