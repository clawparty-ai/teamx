# 租户 teamx server 镜像（design/014 §3）—— 与 member 镜像**同源**（同一 base + 同一 teamx 二进制），
# 但**独立 tag** 且**更轻**：server 只需 teamx serve（不需要 opencode/ttyd/bun）。
#
# 运行形态（由 tenant_manifests 生成）：
#   args: serve --addr 0.0.0.0 --port 5783 --db /data/teamx.db
#   env : TEAMX_HOME=/data  TEAMX_DB=/data/teamx.db  TEAMX_TENANT_TEAM_ID=<team_id>
#   卷  : /data（SQLite + 租户 CA 只读挂载在 /data/ca）
#
# 构建（与 member 同源输入）：
#   docker build -f docker/server/Dockerfile --build-arg TEAMX_BIN=target/release/teamx \
#     -t teamx-server:dev .
# 或统一用：bash docker/build.sh（一次构建两个镜像，共享同一 TEAMX_BIN 与镜像源参数）

# syntax=docker/dockerfile:1
ARG BASE_IMAGE=ubuntu:24.04
FROM --platform=$TARGETPLATFORM ${BASE_IMAGE} AS server

ARG DEBIAN_FRONTEND=noninteractive
# 可选 APT 镜像源（受限网络，如 mirrors.aliyun.com）
ARG APT_MIRROR=''
# teamx 预编译二进制（与 member 镜像同一份）
ARG TEAMX_BIN=target/release/teamx

RUN if [ -n "$APT_MIRROR" ]; then \
      sed -i "s|http://archive.ubuntu.com|https://$APT_MIRROR|g; s|http://security.ubuntu.com|https://$APT_MIRROR|g" \
        /etc/apt/sources.list.d/ubuntu.sources 2>/dev/null || true; \
    fi \
 && apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl bash git \
    && rm -rf /var/lib/apt/lists/*

COPY ${TEAMX_BIN} /usr/local/bin/teamx
RUN chmod 0755 /usr/local/bin/teamx && /usr/local/bin/teamx version | head -2

# 非 root（与 Deployment 的 runAsUser/fsGroup 1000 对齐）
RUN (useradd -m -u 1000 -s /bin/bash teamx || true) \
 && mkdir -p /data && chown -R 1000:1000 /data
USER 1000
ENV HOME=/home/teamx
ENV PATH=/usr/local/bin:/usr/bin:/bin
# 租户 server 的工作目录就是数据卷根（TEAMX_HOME=/data）
WORKDIR /data

# 默认入口：serve（端口/SAN 由 Deployment args 覆盖）
ENTRYPOINT ["/usr/local/bin/teamx"]
CMD ["serve", "--addr", "0.0.0.0", "--port", "5783", "--db", "/data/teamx.db"]
