#!/usr/bin/env bash
set -Eeuo pipefail

# 从源码构建 amd64 镜像并加载到本地 Docker，路径不依赖调用目录
usage() {
  cat <<'EOF'
用法：deploy/build_amd64.sh [镜像名:标签]

默认镜像：yangjianbo/aicodex-agent-teams:<release/version.yaml 中的版本号>
固定平台：linux/amd64；构建完成后加载到本地 Docker

可选环境变量：
  AICODEX_AGENT_TEAMS_IMAGE       默认镜像名，可由位置参数覆盖
  AICODEX_AGENT_TEAMS_GIT_SHA     Git revision，默认读取当前仓库 HEAD
  AICODEX_AGENT_TEAMS_BUILD_TIME  构建时间，默认使用当前 UTC 时间
  AICODEX_AGENT_TEAMS_BUILD_TYPE  构建类型，默认 release
EOF
}

if [[ $# -eq 1 && ( "$1" == '-h' || "$1" == '--help' ) ]]; then
  usage
  exit 0
fi
if [[ $# -gt 1 || ( $# -eq 1 && ( -z "$1" || "$1" == -* ) ) ]]; then
  usage >&2
  exit 2
fi

command -v docker >/dev/null 2>&1 || { printf '缺少命令：docker\n' >&2; exit 1; }
docker buildx version >/dev/null

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# 发行配置同时决定默认镜像标签和编译版本，避免两者不一致
VERSION="$(awk '
  /^version:[[:space:]]*/ {
    sub(/^version:[[:space:]]*/, "")
    sub(/[[:space:]]*#.*/, "")
    gsub(/^[[:space:]\042\047]+|[[:space:]\042\047]+$/, "")
    print
    exit
  }
' "$REPO_ROOT/release/version.yaml")"
if [[ ! "$VERSION" =~ ^[a-zA-Z0-9_][a-zA-Z0-9_.-]{0,127}$ ]]; then
  printf 'release/version.yaml 中的版本号为空或不能用作 Docker 标签：%s\n' "$VERSION" >&2
  exit 1
fi
IMAGE="${1:-${AICODEX_AGENT_TEAMS_IMAGE:-yangjianbo/aicodex-agent-teams:$VERSION}}"
GIT_SHA="${AICODEX_AGENT_TEAMS_GIT_SHA:-$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || printf 'unknown')}"
BUILD_TIME="${AICODEX_AGENT_TEAMS_BUILD_TIME:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}"

printf '构建镜像：%s（linux/amd64）\n' "$IMAGE"
# Dockerfile 最后一个阶段需要预编译产物，源码构建必须显式选择 runtime
docker buildx build \
  --platform linux/amd64 \
  --target runtime \
  --file "$REPO_ROOT/deploy/Dockerfile" \
  --tag "$IMAGE" \
  --build-arg "AICODEX_AGENT_TEAMS_VERSION=$VERSION" \
  --build-arg "AICODEX_AGENT_TEAMS_GIT_SHA=$GIT_SHA" \
  --build-arg "AICODEX_AGENT_TEAMS_BUILD_TIME=$BUILD_TIME" \
  --build-arg "AICODEX_AGENT_TEAMS_BUILD_TYPE=${AICODEX_AGENT_TEAMS_BUILD_TYPE:-release}" \
  --load \
  "$REPO_ROOT"

PLATFORM="$(docker image inspect "$IMAGE" --format '{{.Os}}/{{.Architecture}}')"
if [[ "$PLATFORM" != 'linux/amd64' ]]; then
  printf '镜像架构不符合预期：%s\n' "$PLATFORM" >&2
  exit 1
fi
printf '构建完成：%s（%s）\n' "$IMAGE" "$PLATFORM"
