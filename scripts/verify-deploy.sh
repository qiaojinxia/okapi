#!/usr/bin/env bash
# 部署形态验收（IMPLEMENTATION §13 M4 / docs/verification-checklist.md L5）：
# 1. 模板守卫：compose / K8s 结构、镜像来源、探针、下线宽限期、PG 连接总账；
# 2. embed-web 发布形态：前端产物编进二进制后，在**没有 frontend/dist 的工作目录**里起 console，
#    首页、带哈希的静态资源、SPA 深链都必须从二进制里服出来——这是 deploy/Dockerfile 的运行时前提，
#    此前只有手工验证。
# 依赖 dev 容器（scripts/dev-deps.sh up）与 .env；embed 构建用独立 target 目录，不碰开发用二进制。
set -euo pipefail
cd "$(dirname "$0")/.."

python3 scripts/guard-deploy-manifests.py

echo "▶ 前端构建（embed-web 在编译期读取 frontend/dist）"
pnpm -C frontend build >/dev/null

TARGET="${CARGO_TARGET_DIR:-target}/embed-web"
echo "▶ cargo build --features embed-web（target: ${TARGET}）"
SQLX_OFFLINE=true cargo build --quiet --bin okapi --features embed-web --target-dir "$TARGET"
BIN="$TARGET/debug/okapi"
case "$BIN" in /*) ;; *) BIN="$PWD/$BIN" ;; esac

set -a
# shellcheck disable=SC1091
. ./.env
set +a
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
SCRATCH=$(mktemp -d /tmp/okapi-embed.XXXXXX)
LOG="$SCRATCH/console.log"
# 换到没有 frontend/dist 的目录再起：磁盘形态的兜底路径在这里必须不存在
(cd "$SCRATCH" && OKAPI_CONSOLE_BIND="127.0.0.1:$PORT" OKAPI_WEB_DIR="$SCRATCH/absent" "$BIN" console >"$LOG" 2>&1) &
PID=$!
trap 'kill $PID 2>/dev/null || true; wait $PID 2>/dev/null || true; rm -rf "$SCRATCH"' EXIT

for _ in $(seq 1 30); do
  curl -sf -m 2 "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1 && break
  sleep 1
done
curl -sf -m 2 "http://127.0.0.1:$PORT/healthz" >/dev/null || { echo "❌ verify-deploy：embed console 未就绪"; tail -20 "$LOG"; exit 1; }

INDEX=$(curl -s -m 3 "http://127.0.0.1:$PORT/")
printf '%s' "$INDEX" | head -c 15 | grep -qi '<!doctype html>' \
  && echo "✅ verify-deploy：首页由二进制内嵌产物服出" \
  || { echo "❌ verify-deploy：首页不是 SPA 壳"; exit 1; }

ASSET=$(printf '%s' "$INDEX" | grep -o '/assets/[A-Za-z0-9_.-]*\.js' | head -1)
[ -n "$ASSET" ] || { echo "❌ verify-deploy：首页里找不到 /assets/*.js 引用"; exit 1; }
CT=$(curl -s -m 3 -o /dev/null -w '%{http_code} %{content_type}' "http://127.0.0.1:$PORT$ASSET")
case "$CT" in
  200\ *javascript*) echo "✅ verify-deploy：静态资源 $ASSET → $CT" ;;
  *) echo "❌ verify-deploy：静态资源 $ASSET → $CT"; exit 1 ;;
esac

DEEP=$(curl -s -m 3 -H 'Accept: text/html' "http://127.0.0.1:$PORT/admin/users" | head -c 15)
printf '%s' "$DEEP" | grep -qi '<!doctype html>' \
  && echo "✅ verify-deploy：SPA 深链 /admin/users 回应用壳（内容协商）" \
  || { echo "❌ verify-deploy：深链未回 SPA 壳：$DEEP"; exit 1; }

API=$(curl -s -m 3 -o /dev/null -w '%{http_code}' -H 'Accept: application/json' "http://127.0.0.1:$PORT/admin/users")
[ "$API" = "401" ] && echo "✅ verify-deploy：同路径 JSON 请求仍是 API（401）" \
  || { echo "❌ verify-deploy：/admin/users JSON 请求返回 ${API}（应 401）"; exit 1; }

echo "✅ verify-deploy：embed-web 发布形态验收通过"
