#!/bin/bash
# Linux 容器复测（docs/perf-report.md 复现用）：
# 在 rust 容器内 release 编译并压测，PG/Redis 走宿主容器（host.docker.internal）。
# 宿主侧启动：
#   docker volume create okapi-bench-target
#   docker run --rm -v "$PWD":/work -v okapi-bench-target:/build-target \
#     --add-host=host.docker.internal:host-gateway \
#     public.ecr.aws/docker/library/rust:latest bash /work/scripts/linux-bench.sh
set -e
export DATABASE_URL="postgres://okapi:okapi_dev@host.docker.internal:54329/okapi"
export OKAPI_REDIS_URL="redis://host.docker.internal:63790"
export CARGO_TARGET_DIR=/build-target
cd /work
echo "== rustc: $(rustc --version) / nproc: $(nproc) / kernel: $(uname -r) =="
cargo build --release --bin okapi --example loadgen 2>&1 | tail -2
echo "== 启动 gateway（日志入 /tmp/gw.log）=="
OKAPI_GATEWAY_BIND=127.0.0.1:8080 /build-target/release/okapi gateway >/tmp/gw.log 2>&1 &
GW=$!
sleep 2

MODE="${1:-bench}"
if [ "$MODE" = "soak" ]; then
  CONNS="${2:-20000}"
  HOLD="${3:-120}"
  echo "== SSE 持有专项（$CONNS 条 × ${HOLD}s；ulimit -n $(ulimit -n)）=="
  # gateway 资源采样（RSS KB / fd 数）
  ( while kill -0 $GW 2>/dev/null; do
      RSS=$(awk '/VmRSS/{print $2}' /proc/$GW/status 2>/dev/null || echo n/a)
      FDS=$(ls /proc/$GW/fd 2>/dev/null | wc -l || echo n/a)
      echo "gateway_sample rss_kb=$RSS fds=$FDS"
      sleep 20
    done ) &
  SAMPLER=$!
  /build-target/release/examples/loadgen "$CONNS" "$HOLD" hold
  kill $SAMPLER 2>/dev/null || true
else
  echo "== baseline 档（mock 直连，64 并发 × 10s）=="
  /build-target/release/examples/loadgen 64 10 baseline
  echo "== json 档（64 并发 × 10s）=="
  /build-target/release/examples/loadgen 64 10
  echo "== stream 档（64 并发 × 10s）=="
  /build-target/release/examples/loadgen 64 10 stream
fi
sleep 20  # 留给后台结算排空
kill $GW 2>/dev/null || true
echo "== gateway 错误统计 =="
grep -c "ERROR" /tmp/gw.log || true
grep "ERROR" /tmp/gw.log | head -3 || true
echo "== BENCH_DONE =="
