#!/usr/bin/env bash
# `okapi all` 单机形态冒烟（IMPLEMENTATION §13 M4 验收项）：
# 一进程三角色起立 → 数据面/控制面健康 → 单用户 root key 引导 → 门户 API 可用 → 前端可达。
# 依赖 dev 容器（scripts/dev-deps.sh up）与 .env。
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=${OKAPI_BIN:-target/debug/okapi}
[ -x "$BIN" ] || cargo build --quiet --bin okapi

LOG=$(mktemp /tmp/okapi-smoke.XXXXXX)
# 端口上已经有实例在应答，下面的探测就会打到它而不是刚起的二进制（新进程绑不上端口退出了，
# 冒烟照样报通过）：先拒绝，别带着旧实例往下测
for port in 8080 8081; do
  if curl -sf -m 2 "http://127.0.0.1:$port/healthz" >/dev/null 2>&1; then
    echo "❌ smoke-all：127.0.0.1:$port 上已有实例在应答，冒烟会测到它而不是新二进制；先停掉再跑" >&2
    exit 1
  fi
done
OKAPI_SINGLE_USER_MODE=true "$BIN" all >"$LOG" 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null || true' EXIT

ok=false
for _ in $(seq 1 30); do
  # 新进程已经退出（配置不对、端口冲突……）就立刻失败，别空等到超时再报一个看不出原因的「健康检查超时」
  if ! kill -0 "$PID" 2>/dev/null; then
    echo "❌ smoke-all：okapi all 进程已退出"; tail -20 "$LOG"; exit 1
  fi
  if curl -sf -m 2 http://127.0.0.1:8080/healthz >/dev/null 2>&1 \
     && curl -sf -m 2 http://127.0.0.1:8081/healthz >/dev/null 2>&1; then
    ok=true
    break
  fi
  sleep 1
done
$ok || { echo "❌ smoke-all：健康检查超时"; tail -20 "$LOG"; exit 1; }

# 单用户模式引导 key（首启打印；已有 root 时从既有日志拿不到 → 跳过 key 断言）
KEY=$(grep -o 'sk-okapi-[A-Za-z0-9]*' "$LOG" | head -1 || true)
if [ -n "$KEY" ]; then
  CODE=$(curl -s -m 3 -o /dev/null -w '%{http_code}' \
    -H "Authorization: Bearer $KEY" http://127.0.0.1:8081/api/me)
  [ "$CODE" = "200" ] && echo "✅ smoke-all：root key 门户可用" \
    || { echo "❌ smoke-all：root key /api/me=$CODE"; exit 1; }
else
  echo "ℹ️ smoke-all：root 已存在（跳过 key 断言）"
fi

# 前端 SPA 可达（磁盘或嵌入形态皆可）
curl -s -m 3 http://127.0.0.1:8081/ | head -c 15 | grep -q '<!doctype html>' \
  && echo "✅ smoke-all：前端可达" || { echo "❌ smoke-all：前端不可达"; exit 1; }

# 数据面拒绝无凭证请求（fail-closed）
CODE=$(curl -s -m 3 -o /dev/null -w '%{http_code}' -X POST \
  -H 'content-type: application/json' -d '{"model":"x","messages":[]}' \
  http://127.0.0.1:8080/v1/chat/completions)
[ "$CODE" = "401" ] && echo "✅ smoke-all：数据面鉴权 fail-closed" \
  || { echo "❌ smoke-all：无凭证请求返回 $CODE（应 401）"; exit 1; }

echo "✅ smoke-all：okapi all 单机形态冒烟通过"
