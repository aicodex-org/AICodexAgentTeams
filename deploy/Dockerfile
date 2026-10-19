# syntax=docker/dockerfile:1.7@sha256:a57df69d0ea827fb7266491f2813635de6f17269be881f696fbfdf2d83dda33e

# PostgreSQL 服务端镜像：备份客户端（pg_dump/pg_restore）从这里取，与 compose 同源、
# 版本严格一致。升级数据库版本时只需同步修改这里与 deploy/compose.yaml。
ARG POSTGRES_IMAGE=postgres:18-bookworm@sha256:d9c83446333daec3f0588cc709adb80c26090b7f9f0f7ec8d43c243385d79818

FROM node:26-bookworm-slim@sha256:86f07bc9c5dce4578cf37e5a418b7bfc7f817cda25cde66e2b66e95ed86c4567 AS frontend-builder

WORKDIR /app/webui
COPY webui/package.json webui/pnpm-lock.yaml webui/pnpm-workspace.yaml ./
RUN --mount=type=cache,target=/root/.npm \
    --mount=type=cache,id=aicodex-webui-pnpm,target=/pnpm/store \
    npm install --global "$(node -p 'require("./package.json").packageManager')" \
    && pnpm install --frozen-lockfile --store-dir=/pnpm/store
COPY webui ./
RUN pnpm build

FROM rust:1.98-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS backend-builder

WORKDIR /app/agent_teams
ARG TARGETARCH
ENV CARGO_TARGET_DIR=/app/agent_teams/target/${TARGETARCH}
COPY agent_teams/Cargo.toml agent_teams/Cargo.lock ./
COPY agent_teams/apps ./apps
COPY agent_teams/crates ./crates
COPY agent_teams/vendor ./vendor
COPY agent_teams/migrations ./migrations
COPY release/version.yaml /app/release/version.yaml
# 构建元数据只影响编译，避免每次更新时间时使源码复制缓存失效
ARG AICODEX_AGENT_TEAMS_VERSION=
ARG AICODEX_AGENT_TEAMS_GIT_SHA=unknown
ARG AICODEX_AGENT_TEAMS_BUILD_TIME=unknown
ARG AICODEX_AGENT_TEAMS_BUILD_TYPE=release
ENV AICODEX_AGENT_TEAMS_VERSION=${AICODEX_AGENT_TEAMS_VERSION}
ENV AICODEX_AGENT_TEAMS_GIT_SHA=${AICODEX_AGENT_TEAMS_GIT_SHA}
ENV AICODEX_AGENT_TEAMS_BUILD_TIME=${AICODEX_AGENT_TEAMS_BUILD_TIME}
ENV AICODEX_AGENT_TEAMS_BUILD_TYPE=${AICODEX_AGENT_TEAMS_BUILD_TYPE}
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/agent_teams/target \
    cargo build --release --locked --bin AICodexAgentTeams \
    && cp "${CARGO_TARGET_DIR}/release/AICodexAgentTeams" /tmp/AICodexAgentTeams

# 备份客户端来源：与 compose 服务端同一固定镜像，版本一致。
FROM ${POSTGRES_IMAGE} AS pg-client

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587 AS runtime-base

ENV TZ=UTC

# 运行时基础只从 Debian 官方源补 libpq（pg_dump 的动态依赖；ABI 兼容，无需引入第三方
# APT 源）。pg_dump/pg_restore 二进制由 pg-client stage COPY，构建期不依赖运行时安装。
# 更新基础镜像已有的系统包，使固定 digest 的镜像也能获取 Debian 安全更新。
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get upgrade -y \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ca-certificates curl tzdata libpq5 \
    && install -d /usr/lib/postgresql/18/bin \
    && ln -snf "/usr/share/zoneinfo/${TZ}" /etc/localtime \
    && printf '%s\n' "${TZ}" > /etc/timezone \
    && groupadd --system --gid 10001 aicodex-agent-teams \
    && useradd --system --uid 10001 --gid aicodex-agent-teams --home-dir /app --shell /usr/sbin/nologin aicodex-agent-teams \
    && install -d -o aicodex-agent-teams -g aicodex-agent-teams \
        /app/bin /app/deploy /app/.runtime/data /app/.runtime/logs /app/web \
    && rm -rf /var/lib/apt/lists/*

COPY --from=pg-client \
    /usr/lib/postgresql/18/bin/pg_dump \
    /usr/lib/postgresql/18/bin/pg_restore \
    /usr/lib/postgresql/18/bin/
RUN /usr/lib/postgresql/18/bin/pg_dump --version \
    && /usr/lib/postgresql/18/bin/pg_restore --version

WORKDIR /app
ENV AICODEX_AGENT_TEAMS_DEPLOYMENT_MODE=docker
ENV PATH="/app/bin:/usr/lib/postgresql/18/bin:${PATH}"

EXPOSE 9699

# 首次升级的历史请求迁移需要启动宽限期
HEALTHCHECK --interval=30s --timeout=5s --start-period=5m --retries=3 \
    CMD curl -fsS http://127.0.0.1:9699/healthz || exit 1

CMD ["/app/bin/AICodexAgentTeams"]

FROM runtime-base AS runtime

COPY --from=backend-builder --chown=aicodex-agent-teams:aicodex-agent-teams /tmp/AICodexAgentTeams /app/bin/AICodexAgentTeams
COPY --from=frontend-builder --chown=aicodex-agent-teams:aicodex-agent-teams /app/webui/dist ./web/dist
USER aicodex-agent-teams

FROM runtime-base AS runtime-prebuilt

ARG TARGETARCH
ARG AICODEX_AGENT_TEAMS_PREBUILT_DIR=dist/docker

COPY --chown=aicodex-agent-teams:aicodex-agent-teams --chmod=0755 ${AICODEX_AGENT_TEAMS_PREBUILT_DIR}/linux-${TARGETARCH}/AICodexAgentTeams /app/bin/AICodexAgentTeams
COPY --chown=aicodex-agent-teams:aicodex-agent-teams ${AICODEX_AGENT_TEAMS_PREBUILT_DIR}/web/dist ./web/dist
COPY --chown=aicodex-agent-teams:aicodex-agent-teams ${AICODEX_AGENT_TEAMS_PREBUILT_DIR}/linux-${TARGETARCH}/plugins/official /app/bin/plugins/official
RUN find /app/bin/plugins/official -type f -exec chmod 0444 '{}' + \
    && chmod 0555 /app/bin/plugins/official \
    && chmod 0755 /app/bin/plugins
USER aicodex-agent-teams
