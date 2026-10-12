#!/usr/bin/env bash
set -Eeuo pipefail

# 验证重跑安装按保留的 Compose 服务选择镜像用户，不修改真实部署或权限
source "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/install.sh"
INSTALL_DIR="$(mktemp -d)"
trap 'rm -rf "$INSTALL_DIR"' EXIT
trap - ERR

docker() {
  [[ "$*" == 'compose -f deploy/compose.yaml config --services' ]]
  printf '%s\n' "$TEST_SERVICES"
}

service_user_ids() {
  case "$1:$2" in
    postgres:postgres | redis:redis) printf '999:999\n' ;;
    "$EXPECTED_SERVICE:$EXPECTED_USER") printf '10001:10001\n' ;;
    *) return 1 ;;
  esac
}

run_root() { :; }

for brand in current legacy; do
  if [[ "$brand" == current ]]; then
    EXPECTED_SERVICE=aicodex-agent-teams
    EXPECTED_USER=aicodex-agent-teams
  else
    EXPECTED_SERVICE=codex-proxy-rs
    EXPECTED_USER=cpr
  fi
  TEST_SERVICES="$(printf 'postgres\nredis\n%s' "$EXPECTED_SERVICE")"
  resolve_app_service
  [[ "$APP_SERVICE" == "$EXPECTED_SERVICE" && "$APP_USER" == "$EXPECTED_USER" ]]
  fix_runtime_permissions >/dev/null
done

for TEST_SERVICES in $'postgres\nredis' $'aicodex-agent-teams\ncodex-proxy-rs'; do
  if (resolve_app_service) >/dev/null 2>&1; then
    printf '应拒绝缺失或歧义的应用服务\n' >&2
    exit 1
  fi
done

printf '安装服务与权限回归检查通过\n'
