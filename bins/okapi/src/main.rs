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

/// 服务角色把告警日志汇总到 Redis（§11.43）。独立连接：日志汇总不该和业务共用一条
/// 连接排队；连不上就只留本进程缓冲。迁移等一次性命令不启动。
async fn start_ops_log(role: &Role, cfg: &Config) {
    let role = match role {
        Role::Gateway => "gateway",
        Role::Console => "console",
        Role::Worker => "worker",
        Role::All => "all",
        Role::Migrate { .. } | Role::SealCredentials => return,
    };
    match okapi_store::connect_redis(&cfg.redis_url).await {
        Ok(redis) => okapi::ops::logbuf::spawn_flusher(redis, &cfg.node, role),
        Err(e) => tracing::warn!(error = %e, "ops log flusher disabled"),
    }
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
    let cfg = Config::from_env()?;
    start_ops_log(&cli.role, &cfg).await;

    match cli.role {
        Role::SealCredentials => seal_credentials(&cfg).await,
        Role::Gateway => gateway::run(cfg).await,
        Role::Worker => worker::run(cfg).await,
        Role::Console => console::run(cfg).await,
        Role::All => {
            let worker_cfg = cfg.clone();
            let console_cfg = cfg.clone();
            tokio::try_join!(
                gateway::run(cfg),
                worker::run(worker_cfg),
                console::run(console_cfg)
            )
            .map(|_| ())
        }
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
