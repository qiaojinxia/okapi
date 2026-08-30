//! 单用户模式引导（IMPLEMENTATION §6.5）：跳过注册体系，启动时确保 root key。

use super::state::AppState;
use okapi_domain::Money;
use rand::RngExt;
use rand::distr::Alphanumeric;
use sha2::{Digest, Sha256};

/// 引导额度：$1,000,000（单用户自用，余额语义仍完整走账本）。
const BOOTSTRAP_CREDIT_MICRO: i64 = 1_000_000_000_000;

pub async fn ensure_single_user(state: &AppState) -> anyhow::Result<()> {
    let secret: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(43)
        .map(char::from)
        .collect();
    let token = format!("sk-okapi-{secret}");
    let key_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let key_prefix: String = token.chars().take(16).collect();

    let (user_id, _key_id, created) =
        okapi_store::provision::ensure_root(&state.pg, &key_hash, &key_prefix).await?;

    if created {
        let amount = Money::from_micros(BOOTSTRAP_CREDIT_MICRO);
        state.ledger.credit(user_id, amount).await?;
        okapi_ledger::pg::record_credit(
            &state.pg,
            user_id,
            amount,
            "adjust",
            "system",
            serde_json::json!({ "tags": ["single_user_bootstrap"] }),
        )
        .await?;
        // 唯一一次明文输出（新 key 引导）；后续启动只能重置找回
        tracing::warn!(api_key = %token, "单用户模式：root key 已生成，请立即保存（仅本次打印）");
    } else {
        tracing::info!(
            "单用户模式：root key 已存在（遗失可删除 api_keys 中 name='root' 行后重启重建）"
        );
    }
    Ok(())
}
