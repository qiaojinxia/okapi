#!/usr/bin/env bash
# 开发依赖容器管理（不依赖 compose 插件，不在本机安装任何服务）。
# 镜像走 public.ecr.aws（Docker 官方 library 镜像的 AWS 公共分发），
# 规避部分网络对 registry-1.docker.io 的劫持/污染。
set -euo pipefail

PG_IMAGE=public.ecr.aws/docker/library/postgres:16-alpine
REDIS_IMAGE=public.ecr.aws/docker/library/redis:7-alpine
NATS_IMAGE=public.ecr.aws/docker/library/nats:2-alpine
# clickhouse 非 library 官方镜像，ECR 不镜像；走 daocloud（探测可用）
CH_IMAGE=docker.m.daocloud.io/clickhouse/clickhouse-server:24.8-alpine
PG_NAME=okapi-dev-pg
REDIS_NAME=okapi-dev-redis
NATS_NAME=okapi-dev-nats
CH_NAME=okapi-dev-ch
PG_PORT=54329
REDIS_PORT=63790
NATS_PORT=14222
CH_HTTP_PORT=18123
CH_NATIVE_PORT=19000

case "${1:-}" in
  up)
    docker rm -f "$PG_NAME" "$REDIS_NAME" "$NATS_NAME" "$CH_NAME" >/dev/null 2>&1 || true
    # max_connections 提到 300：cargo test 并行跑多个测试二进制，每个各建连接池
    # （setup 一个 + build_state 一个），默认 100 会连接耗尽导致测试随机失败
    docker run -d --name "$PG_NAME" \
      -e POSTGRES_USER=okapi -e POSTGRES_PASSWORD=okapi_dev -e POSTGRES_DB=okapi \
      -p "$PG_PORT:5432" "$PG_IMAGE" \
      -c max_connections=300 >/dev/null
    docker run -d --name "$REDIS_NAME" -p "$REDIS_PORT:6379" "$REDIS_IMAGE" >/dev/null
    docker run -d --name "$NATS_NAME" -p "$NATS_PORT:4222" "$NATS_IMAGE" -js >/dev/null
    docker run -d --name "$CH_NAME" \
      -e CLICKHOUSE_DB=okapi -e CLICKHOUSE_USER=okapi -e CLICKHOUSE_PASSWORD=okapi_dev \
      -p "$CH_HTTP_PORT:8123" -p "$CH_NATIVE_PORT:9000" "$CH_IMAGE" >/dev/null
    for _ in $(seq 1 30); do
      if docker exec "$PG_NAME" pg_isready -U okapi >/dev/null 2>&1; then break; fi
      sleep 1
    done
    docker exec "$PG_NAME" pg_isready -U okapi
    docker exec "$REDIS_NAME" redis-cli ping
    echo "dev 依赖就绪：PG=:$PG_PORT Redis=:$REDIS_PORT NATS=:$NATS_PORT CH=:${CH_HTTP_PORT}（对齐 .env.example）"
    ;;
  down)
    docker rm -f "$PG_NAME" "$REDIS_NAME" "$NATS_NAME" "$CH_NAME"
    ;;
  status)
    docker ps --filter "name=okapi-dev" --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}"
    ;;
  *)
    echo "用法: $0 up|down|status" >&2
    exit 1
    ;;
esac
