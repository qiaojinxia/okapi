# Okapi 发布镜像：前端产物嵌入单二进制（embed-web）。
# 构建上下文 = 仓库根：docker build -f deploy/Dockerfile .

# ---- 前端 ----
FROM public.ecr.aws/docker/library/node:22-alpine AS web
WORKDIR /src
RUN corepack enable
COPY frontend/package.json frontend/pnpm-lock.yaml frontend/
RUN cd frontend && pnpm install --frozen-lockfile
COPY frontend/ frontend/
RUN cd frontend && pnpm build

# ---- 后端 ----
# 构建与运行阶段必须钉在同一个 Debian 发行版：`rust:1-slim` 已跟到 trixie（glibc 2.41），
# 二进制拿到 bookworm-slim 运行时会 `GLIBC_2.38 not found` 直接起不来（scripts/verify-deploy.sh 抓到）
FROM public.ecr.aws/docker/library/rust:1-slim-bookworm AS builder
WORKDIR /src
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY . .
COPY --from=web /src/frontend/dist frontend/dist
# 编译期 SQL 校验走离线快照（仓库内 .sqlx）
ENV SQLX_OFFLINE=true
RUN cargo build --release --bin okapi --features embed-web

# ---- 运行时 ----
FROM public.ecr.aws/docker/library/debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/okapi /usr/local/bin/okapi
USER 65534:65534
# gateway :8080 / console :8081
EXPOSE 8080 8081
ENTRYPOINT ["okapi"]
CMD ["all"]
