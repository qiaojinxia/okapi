#!/usr/bin/env bash
# 开发环境重置 + 演示数据灌注。
#
# 为什么需要：改动 migrations/0001_init.sql 后，已应用的库会因校验和不符报
# Migrate(VersionMismatch)；而只重建 PG 又会让 Redis 与 ClickHouse 里旧 user_id
# 的存量数据串味（PG 的 id 从 1 重新开始，旧聚合会被算进新用户的账），
# 表现为对账测试莫名失败。三处必须一起清。
#
# 用法：bash scripts/dev-reset.sh [--no-seed]
set -euo pipefail

cd "$(dirname "$0")/.."
set -a
# shellcheck disable=SC1091
. ./.env
set +a

DB_NAME="${DATABASE_URL##*/}"
PG_CONTAINER="${PG_CONTAINER:-okapi-dev-pg}"
REDIS_CONTAINER="${REDIS_CONTAINER:-okapi-dev-redis}"
CH_URL="${OKAPI_CLICKHOUSE_URL:-}"

echo "▶ 重建 PostgreSQL 库 ${DB_NAME}"
docker exec "$PG_CONTAINER" psql -U okapi -d postgres -q \
  -c "DROP DATABASE IF EXISTS ${DB_NAME} WITH (FORCE)" \
  -c "CREATE DATABASE ${DB_NAME}"

echo "▶ 应用迁移"
(cd crates/okapi-store && sqlx migrate run)

echo "▶ 清空 Redis（余额热账本按 user_id 索引，旧键会串到新用户）"
docker exec "$REDIS_CONTAINER" redis-cli FLUSHDB >/dev/null

if [[ -n "$CH_URL" ]]; then
  # 表名从 system.tables 现查，不写死清单。
  #
  # 上一版是硬编码的 7 张表，而 schema 早已长到 13 张——漏掉的 6 张（mv_key_model_day /
  # mv_client_day / mv_error_hour / mv_cube_hour / mv_analysis_hour / mv_cache_write_day）
  # 会带着**重置前的 user_id** 活下来。PG 的 id 从 1 重新开始后，新用户正好撞上旧聚合，
  # 门户里就会看到上一茬用户的用量——本文件开头警告的正是这件事，清单却自己漂了。
  # 改成现查即可免疫后续加表。
  echo "▶ 清空 ClickHouse 明细与全部 MV"
  CH_TABLES=$(curl -sS "$CH_URL/" --data-binary \
    "SELECT name FROM system.tables WHERE database='okapi' AND name NOT LIKE '.inner%' FORMAT TSV" || true)
  if [[ -z "$CH_TABLES" ]]; then
    echo "  ⚠ 未能列出 ClickHouse 表，跳过（旧聚合可能串味）" >&2
  fi
  for t in $CH_TABLES; do
    curl -sS "$CH_URL/" --data-binary "TRUNCATE TABLE IF EXISTS okapi.$t" >/dev/null || true
  done
fi

if [[ "${1:-}" == "--no-seed" ]]; then
  echo "✅ 已重置（未灌演示数据）"
  exit 0
fi

echo "▶ 启动临时 console 用于灌数据"
if [[ ! -x ./target/debug/okapi ]]; then
  echo "✗ 找不到 ./target/debug/okapi，先 cargo build" >&2
  exit 1
fi
LOG=/tmp/okapi-seed-console.log
: >"$LOG"
./target/debug/okapi console >>"$LOG" 2>&1 &
CONSOLE_PID=$!
# shellcheck disable=SC2064
trap "kill $CONSOLE_PID 2>/dev/null || true" EXIT

READY=0
for _ in $(seq 1 40); do
  if ! kill -0 "$CONSOLE_PID" 2>/dev/null; then
    echo "✗ console 进程已退出，日志：" >&2
    cat "$LOG" >&2
    exit 1
  fi
  if curl -sf -o /dev/null "http://127.0.0.1:8081/healthz"; then
    READY=1
    break
  fi
  sleep 0.5
done
if [[ "$READY" != "1" ]]; then
  echo "✗ console 20s 内未就绪，日志：" >&2
  cat "$LOG" >&2
  exit 1
fi

api() { # api <method> <path> <json>
  curl -sS -X "$1" "http://127.0.0.1:8081$2" \
    -H 'content-type: application/json' \
    ${ADMIN_KEY:+-H "Authorization: Bearer $ADMIN_KEY"} \
    -d "$3"
}

echo "▶ 初始化超管（安装向导产出 key-only 管理员）"
SETUP_RESP=$(api POST /api/setup '{"username":"root"}')
ADMIN_KEY=$(printf '%s' "$SETUP_RESP" | python3 -c '
import json, sys
raw = sys.stdin.read()
try:
    print(json.loads(raw)["api_key"])
except Exception:
    sys.exit(f"安装向导返回异常: {raw[:200]}")
')

echo "▶ 建可密码登录的演示超管 root@okapi.local / okapi-demo-2026"
curl -sS -X POST "http://127.0.0.1:8081/auth/register" \
  -H 'content-type: application/json' -H 'x-real-ip: 198.51.100.42' \
  -d '{"email":"root@okapi.local","username":"rootadmin","password":"okapi-demo-2026"}' >/dev/null
DEMO_ID=$(docker exec "$PG_CONTAINER" psql -U okapi -d "$DB_NAME" -At \
  -c "SELECT id FROM users WHERE email='root@okapi.local'")
api POST "/admin/users/${DEMO_ID}/role" '{"role":100}' >/dev/null
api POST "/admin/users/${DEMO_ID}/credit" '{"amount_micro":50000000,"reason":"demo"}' >/dev/null

echo "▶ 模型（含多模态轴）"
for m in \
  '{"model_name":"gpt-4o","model_ratio":"1.25","completion_ratio":"4","cache_ratio":"0.5","cache_write_ratio":"1.25"}' \
  '{"model_name":"gpt-4o-mini","model_ratio":"0.075","completion_ratio":"4","cache_ratio":"0.5"}' \
  '{"model_name":"gpt-4o-audio-preview","model_ratio":"1.25","completion_ratio":"4","audio_ratio":"16","audio_completion_ratio":"2"}' \
  '{"model_name":"claude-sonnet-4-5","model_ratio":"1.5","completion_ratio":"5","cache_ratio":"0.1","cache_write_ratio":"1.25"}' \
  '{"model_name":"gemini-2.5-pro","model_ratio":"0.625","completion_ratio":"8"}'; do
  api POST /admin/models "$m" >/dev/null
done

echo "▶ 渠道池（含一个 least_latency 池）"
api POST /admin/pools '{"pool_code":"stable","description":"官方直连，稳定优先","routing_strategy":"priority_weighted"}' >/dev/null
api POST /admin/pools '{"pool_code":"fast","description":"低时延优先","routing_strategy":"least_latency"}' >/dev/null
api POST /admin/pools '{"pool_code":"cheap","description":"低价渠道，供免费档","routing_strategy":"priority_weighted"}' >/dev/null

echo "▶ 渠道"
for i in 1 2 3; do
  api POST /admin/channels "{\"name\":\"openai-main-$i\",\"provider\":\"openai\",\"api_base\":\"https://api.openai.com/v1\",\"credential\":\"sk-demo-$i\",\"models\":[\"gpt-4o\",\"gpt-4o-mini\",\"gpt-4o-audio-preview\"],\"priority\":$((10 - i)),\"pools\":[\"stable\"]}" >/dev/null
done
api POST /admin/channels '{"name":"anthropic-main","provider":"anthropic","api_base":"https://api.anthropic.com","credential":"sk-ant-demo","models":["claude-sonnet-4-5"],"priority":8,"pools":["stable","fast"]}' >/dev/null
api POST /admin/channels '{"name":"cheap-relay","provider":"openai","api_base":"https://cheap.example.com/v1","credential":"sk-cheap","models":["gpt-4o-mini","gemini-2.5-pro"],"priority":1,"pools":["cheap"]}' >/dev/null

echo "▶ 价格分组绑池"
api POST /admin/groups '{"group_code":"vip","group_ratio":"0.85","description":"VIP：八五折 + 快池","pool_code":"fast"}' >/dev/null
api POST /admin/groups '{"group_code":"free","group_ratio":"1.2","description":"免费档：上浮 + 低价池","pool_code":"cheap"}' >/dev/null

echo "▶ 发布定价 epoch"
api POST /admin/pricing/publish '{}' >/dev/null

echo "✅ 重置并灌注完成"
echo "   控制台 http://127.0.0.1:8081  账号 root@okapi.local / okapi-demo-2026"
echo "   管理 key（key-only 超管）：$ADMIN_KEY"
