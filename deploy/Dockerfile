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
# 编译器版本与 rust-toolchain.toml 一致：跟着 `rust:1` 走时镜像里的 stable 与钉的版本不同，rustup 每次构建
# 都要在没有缓存的层里重新下载一整套工具链
FROM public.ecr.aws/docker/library/rust:1.99.0-slim-bookworm AS builder
WORKDIR /src
# cmake / make / clang / libclang / git：从源码编译 BoringSSL（Claude Code 外形的 TLS 握手，IMPLEMENTATION §11.42；boring-sys 用 git 打补丁）
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev cmake make clang libclang-dev git && rm -rf /var/lib/apt/lists/*
COPY . .
COPY --from=web /src/frontend/dist frontend/dist
# 编译期 SQL 校验走离线快照（仓库内 .sqlx）
ENV SQLX_OFFLINE=true
# 依赖源码与编译产物放进 BuildKit 缓存挂载：`COPY . .` 这一层随任何改动失效，没有挂载时每次部署都从零编译
# 全部依赖（含 BoringSSL），4 核机器上光这部分就二十多分钟。产物不在镜像层里，编完拷出来。
# 挂载属于构建缓存：部署收尾的 `docker builder prune --keep-storage` 按最近使用保留，刚用过的不会先被清。
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked --bin okapi --features embed-web \
    && cp target/release/okapi /usr/local/bin/okapi

# ---- 运行时 ----
FROM public.ecr.aws/docker/library/debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl tzdata && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/okapi /usr/local/bin/okapi
USER 65534:65534
# gateway :8080 / console :8081。程序缺省只听回环（裸机上不意外暴露）；容器里听回环则
# `docker run -p` 永远连不上，所以镜像里改成全部网卡，对外暴露与否交给端口映射决定
ENV OKAPI_BIND=0.0.0.0:8080 OKAPI_CONSOLE_BIND=0.0.0.0:8081
EXPOSE 8080 8081
ENTRYPOINT ["okapi"]
CMD ["all"]
