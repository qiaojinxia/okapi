//! okapi：单二进制多角色入口（DESIGN §8.2）。

use clap::{Parser, Subcommand};
use okapi::config::Config;
use okapi::{console, gateway, migrate, worker};

#[derive(Parser)]
#[command(name = "okapi", version, about = "Okapi AI API gateway")]
struct Cli {
    #[command(subcommand)]
    role: Role,
}

#[derive(Subcommand)]
enum Role {
    /// 数据面：鉴权、限流、预扣/结算、SSE 透传
    Gateway,
    /// 控制面：管理后台 + 用户门户 API + 定价发布 + MCP（M2）
    Console,
    /// 异步面：outbox relay、chsink、DLQ、对账、通知（M2）
    Worker,
    /// 单机模式：一进程运行全部角色（M1 = gateway；console/worker 于 M2 并入）
    All,
    /// 迁移：JSONL 导出 → Okapi（--from newapi | okapi-old）
    Migrate {
        /// 源系统：newapi（三表）或 okapi-old（老 Go 版五表）
        #[arg(long, default_value = "newapi")]
        from: String,
        /// JSONL 导出目录
        #[arg(long)]
        dir: std::path::PathBuf,
        /// 老 ok-api API key 加密口令（okapi-old 专用；缺省读 OKAPI_OLD_ENC_PASSPHRASE）
        #[arg(long)]
        enc_passphrase: Option<String>,
        /// 只统计不写入
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
    /// 存量渠道凭证一次性信封加密（幂等；需 OKAPI_MASTER_KEY）
    SealCredentials,
}

impl Role {
    /// 只读写 PG 的一次性命令（封装存量凭证、只统计的迁移）不要求配 Redis。
    fn needs_redis(&self) -> bool {
        !matches!(
            self,
            Self::SealCredentials | Self::Migrate { dry_run: true, .. }
        )
    }
}

/// 服务角色把告警日志汇总到 Redis（§11.43），并启动出口代理流量的落库。独立连接：
/// 日志汇总不该和业务共用一条连接排队；连不上就只留本进程缓冲。迁移等一次性命令不启动。
fn start_ops_log(role: &Role, cfg: &Config) {
    let role = match role {
        Role::Gateway => "gateway",
        Role::Console => "console",
        Role::Worker => "worker",
        Role::All => "all",
        Role::Migrate { .. } | Role::SealCredentials => return,
    };
    // 出口代理流量同样每个服务进程各自落库
    okapi::ops::egress_traffic::spawn(cfg);
    // 整机重启时 Redis 可能还在加载数据集（LOADING）：连不上就退避重试，不能一次失败就停到下次重启。
    // 等待期间告警照常进本进程缓冲，接上后补写。
    let (redis_url, node) = (cfg.redis_url.clone(), cfg.node.clone());
    tokio::spawn(async move {
        let mut backoff = std::time::Duration::from_secs(2);
        loop {
            match okapi_store::connect_redis(&redis_url).await {
                Ok(redis) => {
                    okapi::ops::logbuf::spawn_flusher(redis, &node, role);
                    return;
                }
                Err(e) => {
                    tracing::warn!(error = %e, retry_in_secs = backoff.as_secs(), "ops log flusher waiting for Redis");
                }
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(std::time::Duration::from_mins(1));
        }
    });
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use tracing_subscriber::prelude::*;
    // 运维监控的告警日志层（§11.43）：WARN / ERROR 同时收进缓冲，汇总到 Redis
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .with(okapi::ops::logbuf::OpsLogLayer)
        .init();

    let cli = Cli::parse();
    let cfg = Config::from_env(cli.role.needs_redis())?;
    start_ops_log(&cli.role, &cfg);

    match cli.role {
        Role::SealCredentials => seal_credentials(&cfg).await,
        Role::Gateway => gateway::run(cfg).await,
        Role::Worker => worker::run(cfg).await,
        Role::Console => run_console(cfg).await,
        Role::All => run_all(cfg).await,
        Role::Migrate {
            from,
            dir,
            enc_passphrase,
            dry_run,
        } => {
            anyhow::ensure!(
                from == "newapi" || from == "okapi-old",
                "--from 支持 newapi | okapi-old"
            );
            let pg = okapi_store::connect_pg(&cfg.database_url).await?;
            okapi_store::run_migrations(&pg).await?;
            let ledger = if dry_run {
                None
            } else {
                let redis = okapi_store::connect_redis(&cfg.redis_url).await?;
                Some(okapi_ledger::BalanceLedger::new(redis))
            };
            if from == "okapi-old" {
                let pass = enc_passphrase
                    .or_else(|| std::env::var("OKAPI_OLD_ENC_PASSPHRASE").ok())
                    .filter(|p| !p.is_empty());
                if pass.is_none() {
                    tracing::warn!(
                        "未提供 --enc-passphrase：API key 将全部跳过（bcrypt 不可转换）"
                    );
                }
                let stats = migrate::run_okapi_old(
                    &pg,
                    ledger.as_ref(),
                    &dir,
                    pass.as_deref(),
                    dry_run,
                    cfg.master_key.as_deref(),
                )
                .await?;
                tracing::info!(
                    users = stats.users,
                    users_credited = stats.users_credited,
                    keys = stats.keys,
                    keys_undecryptable = stats.keys_undecryptable,
                    channels = stats.channels,
                    models = stats.models,
                    dry_run,
                    "老 ok-api 迁移完成"
                );
                for w in &stats.skipped {
                    tracing::warn!("{w}");
                }
                return Ok(());
            }
            let stats = migrate::run_newapi(
                &pg,
                ledger.as_ref(),
                &dir,
                dry_run,
                cfg.master_key.as_deref(),
            )
            .await?;
            tracing::info!(
                users = stats.users,
                users_credited = stats.users_credited,
                keys = stats.keys,
                channels = stats.channels,
                dry_run,
                "迁移完成"
            );
            for w in &stats.skipped {
                tracing::warn!("{w}");
            }
            Ok(())
        }
    }
}

async fn run_console(cfg: Config) -> anyhow::Result<()> {
    let state = gateway::build_state(
        &cfg.database_url,
        &cfg.redis_url,
        &cfg.node,
        cfg.clickhouse_url.as_deref(),
        cfg.nats_url.as_deref(),
    )
    .await?;
    console::run_with_state(cfg, state, true, okapi_kernel::shutdown::signal()).await
}

struct StopOnDrop(tokio::sync::watch::Sender<bool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

/// Run all three roles in one process with a shared runtime substrate and isolated PG pools
/// (data-plane gateway pool vs. background console/worker pool), running migrations once.
async fn run_all(cfg: Config) -> anyhow::Result<()> {
    let gw_max = okapi_store::default_pg_pool_size();
    let bg_max = okapi_store::background_pg_pool_size();
    let gateway_pg = okapi_store::connect_pg_with_max(&cfg.database_url, gw_max).await?;
    okapi_store::run_migrations(&gateway_pg).await?;
    let background_pg = okapi_store::connect_pg_with_max(&cfg.database_url, bg_max).await?;
    let redis = okapi_store::connect_redis(&cfg.redis_url).await?;
    let nats = gateway::connect_nats_optional(cfg.nats_url.as_deref()).await;
    let ch = gateway::build_ch_optional(cfg.clickhouse_url.as_deref())?;
    let gateway_state =
        gateway::build_state_with_clients(gateway_pg, gw_max, redis.clone(), &cfg.node, ch, nats)
            .await?;
    gateway::spawn_state_sync(&gateway_state);

    let console_state = gateway_state.with_isolated_pg(background_pg.clone(), bg_max);
    let worker_state = gateway_state.with_isolated_pg(background_pg, bg_max);

    let (stop_tx, _) = tokio::sync::watch::channel(false);
    let _stop_guard = StopOnDrop(stop_tx.clone());
    let signal_tx = stop_tx.clone();
    tokio::spawn(async move {
        okapi_kernel::shutdown::signal().await;
        signal_tx.send_replace(true);
    });
    let wait_stop = |rx: tokio::sync::watch::Receiver<bool>| {
        let mut rx = rx;
        async move {
            let _ = rx.wait_for(|stop| *stop).await;
        }
    };

    let gw_cfg = cfg.clone();
    let worker_cfg = cfg.clone();
    tokio::try_join!(
        Box::pin(gateway::run_with_state(
            gw_cfg,
            gateway_state,
            false,
            wait_stop(stop_tx.subscribe()),
        )),
        Box::pin(worker::run_with_state(
            worker_cfg,
            worker_state,
            redis,
            false,
            wait_stop(stop_tx.subscribe()),
        )),
        Box::pin(console::run_with_state(
            cfg,
            console_state,
            false,
            wait_stop(stop_tx.subscribe()),
        )),
    )
    .map(|_| ())
}

/// `okapi seal-credentials`：把存量明文渠道凭证一次性封成 AES-GCM 信封。
/// 幂等，可反复跑；已封的行原样跳过。
async fn seal_credentials(cfg: &Config) -> anyhow::Result<()> {
    let Some(master_key) = cfg.master_key.as_deref() else {
        anyhow::bail!("需要 OKAPI_MASTER_KEY（32 字节 hex）才能封装凭证");
    };
    let pg = okapi_store::connect_pg(&cfg.database_url).await?;
    let stats = okapi_store::credential::seal_existing(&pg, master_key).await?;
    tracing::info!(
        sealed = stats.sealed,
        already_sealed = stats.already_sealed,
        unreadable = stats.unreadable.len(),
        "存量渠道凭证封装完成"
    );
    for id in &stats.unreadable {
        tracing::warn!(channel_key_id = id, "凭证非 UTF-8，已跳过");
    }
    // 出口代理 URL 含认证信息，同一信封（§11.41；迁移来的旧 settings.proxy_url 是明文）
    let proxies = okapi_store::egress::seal_existing(&pg, master_key).await?;
    tracing::info!(
        sealed = proxies.sealed,
        already_sealed = proxies.already_sealed,
        unreadable = proxies.unreadable.len(),
        "存量代理地址封装完成"
    );
    for id in &proxies.unreadable {
        tracing::warn!(proxy_id = id, "代理地址非 UTF-8，已跳过");
    }
    Ok(())
}
