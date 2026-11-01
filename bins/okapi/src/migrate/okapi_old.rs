//! 老 Go 版 ok-api 五表导出 → Okapi（含旧 key 的 PBKDF2 + AES 解密）。
use super::{KeySource, i, model_list, read_jsonl, require_export, s, short_id, sync_imported_key};
use okapi_domain::Money;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::path::Path;

// ============ 老 ok-api（Go/UUID schema） ============

/// 老 ok-api 迁移统计。
#[derive(Debug, Default)]
pub struct OldStats {
    pub users: u64,
    pub users_credited: u64,
    pub keys: u64,
    pub keys_undecryptable: u64,
    pub channels: u64,
    pub models: u64,
    pub skipped: Vec<String>,
}

/// 与老 Go `pkg/crypto.DeriveKey` 逐字节一致：
/// salt = SHA256("okapi-key-derivation:" + passphrase)[..16]，PBKDF2-HMAC-SHA256 100k 轮，32B。
#[must_use]
pub fn derive_old_key(passphrase: &str) -> [u8; 32] {
    let salt_full = Sha256::digest(format!("okapi-key-derivation:{passphrase}"));
    pbkdf2_sha256_block1(passphrase.as_bytes(), &salt_full[..16], 100_000)
}

/// PBKDF2-HMAC-SHA256 单块派生（dkLen = 32 = 单个 SHA256 块，块号 1）。
/// RFC 8018 §5.2；测试以 RFC 7914 §11 向量对拍。
pub(super) fn pbkdf2_sha256_block1(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    use hmac::{Hmac, KeyInit as _, Mac};
    // HMAC 对任意长度 key 合法，new_from_slice 实际不可失败；空 key 兜底仅为绕过 Result
    let hm = |key: &[u8]| {
        <Hmac<Sha256>>::new_from_slice(key)
            .unwrap_or_else(|_| <Hmac<Sha256>>::new_from_slice(&[]).expect("empty key valid"))
    };
    let mut mac = hm(password);
    mac.update(salt);
    mac.update(&1u32.to_be_bytes());
    let mut u = mac.finalize().into_bytes();
    let mut t = u;
    for _ in 1..iterations {
        let mut m = hm(password);
        m.update(&u);
        u = m.finalize().into_bytes();
        for (ti, ui) in t.iter_mut().zip(u.iter()) {
            *ti ^= ui;
        }
    }
    t.into()
}

/// 解密老库 `key_encrypted`：base64(nonce(12B) || AES-256-GCM ciphertext)。
pub(super) fn decrypt_old(key: &[u8; 32], cipher_b64: &str) -> Option<String> {
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, Key, KeyInit as _, Nonce};
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD
        .decode(cipher_b64.trim())
        .ok()?;
    if data.len() < 12 {
        return None;
    }
    let (nonce, ct) = data.split_at(12);
    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(*key));
    let nonce_arr: [u8; 12] = nonce.try_into().ok()?;
    let plain = cipher.decrypt(&Nonce::from(nonce_arr), ct).ok()?;
    String::from_utf8(plain).ok()
}

/// 老库 Go `Encrypt` 的 Rust 等价（演练样本构造用）：base64(nonce || GCM ct)。
pub fn encrypt_old(key: &[u8; 32], plaintext: &str) -> anyhow::Result<String> {
    use aes_gcm::aead::{Aead, AeadCore, OsRng};
    use aes_gcm::{Aes256Gcm, Key, KeyInit as _};
    use base64::Engine as _;
    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(*key));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher
        .encrypt(&nonce, plaintext.as_bytes())
        .map_err(|_| anyhow::anyhow!("encrypt_old 失败"))?;
    let mut out = nonce.to_vec();
    out.extend(ct);
    Ok(base64::engine::general_purpose::STANDARD.encode(out))
}

/// 十进制字符串 → micro（×1e6，小数第 7 位起截断；资金路径禁浮点）。
pub(super) fn dec_str_to_micro(s: &str, allow_neg: bool) -> Option<i64> {
    let s = s.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) if allow_neg => (true, rest),
        Some(_) => return None,
        None => (false, s),
    };
    let (int_part, frac_part) = body.split_once('.').unwrap_or((body, ""));
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let int_v: i64 = if int_part.is_empty() {
        0
    } else {
        int_part.parse().ok()?
    };
    let mut frac6: String = frac_part.chars().take(6).collect();
    while frac6.len() < 6 {
        frac6.push('0');
    }
    let frac_v: i64 = frac6.parse().ok()?;
    let micro = int_v.checked_mul(1_000_000)?.checked_add(frac_v)?;
    Some(if neg { -micro } else { micro })
}

/// e6 定点整数 → NUMERIC 字符串（"X.YYYYYY"），非负。
pub(super) fn e6_to_dec(v: i64) -> String {
    format!("{}.{:06}", v / 1_000_000, (v % 1_000_000).unsigned_abs())
}

/// 数值字段读取：JSON 字符串原样、JSON 数字取字面量（DECIMAL 建议 ::text 导出保精度）。
pub(super) fn dec_field(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub(super) fn old_role(role: &str) -> i16 {
    match role {
        "super_admin" => 100,
        "admin" => 10,
        _ => 1,
    }
}

pub(super) fn old_status(status: Option<&str>) -> i16 {
    if status.unwrap_or("active") == "active" {
        1
    } else {
        2
    }
}

/// provider_code / adapter_type → Okapi provider。
pub(super) fn old_provider(code: &str, adapter: Option<&str>) -> &'static str {
    let norm = adapter.filter(|a| !a.is_empty()).unwrap_or(code);
    match norm.to_ascii_lowercase().as_str() {
        "openai" => "openai",
        "anthropic" | "claude" => "anthropic",
        "gemini" | "google" => "gemini",
        _ => "openai_compat",
    }
}

pub(super) const OLD_ACTOR: &str = "system:migrate:okapi_old";

/// 老 ok-api 迁移主流程。`enc_passphrase` 为老系统 API key 加密口令
/// （无则全部 key 记为不可解密跳过）。
// 线性五表导入脚本；拆分会割裂迁移时序
#[allow(clippy::too_many_lines)]
pub async fn run_okapi_old(
    pg: &PgPool,
    ledger: Option<&okapi_ledger::BalanceLedger>,
    dir: &Path,
    enc_passphrase: Option<&str>,
    dry_run: bool,
    master_key: Option<&str>,
) -> anyhow::Result<OldStats> {
    require_export(
        dir,
        &[
            "users.jsonl",
            "api_keys.jsonl",
            "providers.jsonl",
            "provider_api_keys.jsonl",
            "models.jsonl",
        ],
    )?;
    let mut stats = OldStats::default();
    let enc_key = enc_passphrase.map(derive_old_key);

    // ---- users ----（uuid → 新 id）
    let mut user_map: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for row in read_jsonl(dir, "users.jsonl")? {
        let row = row?;
        let Some(email) = s(&row, "email") else {
            stats.skipped.push(format!("users: 缺 email（{row}）"));
            continue;
        };
        let Some(src_id) = s(&row, "id") else {
            stats.skipped.push(format!("users:{email} 缺 id"));
            continue;
        };
        let username = s(&row, "username").map_or_else(
            || email.split('@').next().unwrap_or(email).to_owned(),
            str::to_owned,
        );
        // 缺 / 空 = 没有余额；写了却解析不了（千分位、科学计数法、越界……）要记注记，不能静默按 0 入库——
        // 那是一笔凭空消失的钱，而同一处的负余额反倒有注记
        let balance_micro = match dec_field(&row, "balance") {
            None => 0,
            Some(raw) => dec_str_to_micro(&raw, true).unwrap_or_else(|| {
                stats
                    .skipped
                    .push(format!("users:{email} 余额 {raw} 无法解析，按 0 迁移"));
                0
            }),
        };
        let role = old_role(s(&row, "role").unwrap_or("user"));
        let row_status = old_status(s(&row, "status"));
        let password_hash = s(&row, "password_hash");
        stats.users += 1;
        if balance_micro > 0 {
            stats.users_credited += 1;
        } else if balance_micro < 0 {
            stats
                .skipped
                .push(format!("users:{email} 负余额 {balance_micro}µ 不入账"));
        }
        if dry_run {
            user_map.insert(src_id.to_owned(), -1);
            continue;
        }
        // 锚 = email（老库唯一键）；重跑不覆盖已改的密码
        let inserted = sqlx::query_scalar!(
            r#"
            INSERT INTO users (email, username, password_hash, role, status)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (email) DO UPDATE SET
                role = EXCLUDED.role,
                status = EXCLUDED.status,
                password_hash = COALESCE(users.password_hash, EXCLUDED.password_hash),
                updated_at = now()
            RETURNING id
            "#,
            email,
            username,
            password_hash,
            role,
            row_status
        )
        .fetch_one(pg)
        .await;
        // username 撞他人（email 不同）：追加源 id 前 8 位消歧重试一次
        let user_id = if let Ok(id) = inserted {
            id
        } else {
            let alt = format!("{username}-{}", short_id(src_id));
            sqlx::query_scalar!(
                r#"
                INSERT INTO users (email, username, password_hash, role, status)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (email) DO UPDATE SET
                    role = EXCLUDED.role, status = EXCLUDED.status, updated_at = now()
                RETURNING id
                "#,
                email,
                alt,
                password_hash,
                role,
                row_status
            )
            .fetch_one(pg)
            .await?
        };
        user_map.insert(src_id.to_owned(), user_id);

        if balance_micro > 0 {
            okapi_ledger::operations::import_credit(
                pg,
                ledger,
                user_id,
                Money::from_micros(balance_micro),
                OLD_ACTOR,
                serde_json::json!({"tags": ["okapi_old_import"], "src_user": src_id}),
            )
            .await?;
        }
    }

    // ---- api_keys ----（bcrypt 不可逆：依赖 key_encrypted 解密重哈希）
    for row in read_jsonl(dir, "api_keys.jsonl")? {
        let row = row?;
        let Some(new_user) = s(&row, "user_id").and_then(|src| user_map.get(src).copied()) else {
            stats
                .skipped
                .push(format!("api_keys: user_id 无映射（{row}）"));
            continue;
        };
        let prefix_hint = s(&row, "key_prefix").unwrap_or("?");
        let Some(plain) = enc_key
            .as_ref()
            .zip(s(&row, "key_encrypted"))
            .and_then(|(k, ct)| decrypt_old(k, ct))
        else {
            stats.keys_undecryptable += 1;
            stats.skipped.push(format!(
                "api_keys:{prefix_hint} 无密文或解密失败（bcrypt 哈希不可转换，需用户重建）"
            ));
            continue;
        };
        if !plain.starts_with("sk-") {
            stats.keys_undecryptable += 1;
            stats.skipped.push(format!(
                "api_keys:{prefix_hint} 解密结果非 sk- 前缀，疑口令错误"
            ));
            continue;
        }
        stats.keys += 1;
        if dry_run {
            continue;
        }
        let key_hash = hex::encode(Sha256::digest(plain.as_bytes()));
        let prefix: String = plain.chars().take(16).collect();
        let name = s(&row, "name").unwrap_or("okapi-old-import");
        let row_status = old_status(s(&row, "status"));
        let allowlist = row.get("allowed_models").filter(|v| v.is_array()).cloned();
        let rpm = i(&row, "rate_limit_rpm").and_then(|v| i32::try_from(v).ok());
        let expires_at = s(&row, "expires_at");
        sqlx::query!(
            r#"
            INSERT INTO api_keys
                (user_id, key_hash, key_prefix, name, status, model_allowlist, rpm_limit, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, ($8::text)::timestamptz)
            ON CONFLICT (key_hash) DO UPDATE SET status = EXCLUDED.status
            "#,
            new_user,
            key_hash,
            prefix,
            name,
            row_status,
            allowlist,
            rpm,
            expires_at
        )
        .execute(pg)
        .await?;
    }

    // ---- providers × provider_api_keys → channels ----
    let mut providers: std::collections::HashMap<i64, (String, String)> =
        std::collections::HashMap::new();
    for row in read_jsonl(dir, "providers.jsonl")? {
        let row = row?;
        if let (Some(id), Some(code)) = (i(&row, "id"), s(&row, "provider_code")) {
            let endpoint = s(&row, "api_endpoint").unwrap_or("").to_owned();
            providers.insert(id, (code.to_owned(), endpoint));
        }
    }
    for row in read_jsonl(dir, "provider_api_keys.jsonl")? {
        let row = row?;
        let Some((code, endpoint)) = i(&row, "provider_id").and_then(|id| providers.get(&id))
        else {
            stats
                .skipped
                .push(format!("provider_api_keys: provider_id 无映射（{row}）"));
            continue;
        };
        let Some(credential) = s(&row, "api_key") else {
            stats
                .skipped
                .push(format!("provider_api_keys:{code} 缺 api_key"));
            continue;
        };
        let key_name = s(&row, "key_name").unwrap_or("default");
        let name = format!("old/{code}/{key_name}");
        let provider = old_provider(code, s(&row, "adapter_type"));
        if provider == "openai_compat" {
            stats
                .skipped
                .push(format!("channels:{name} 按 openai_compat 导入"));
        }
        let base_url = s(&row, "base_url")
            .filter(|b| !b.is_empty())
            .unwrap_or(endpoint);
        let models = model_list(&row, "supported_models");
        let row_status = old_status(s(&row, "status"));
        let priority = i(&row, "priority")
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(0);
        let weight = i(&row, "weight")
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(1)
            .max(1);
        stats.channels += 1;
        if dry_run {
            continue;
        }
        let existing = sqlx::query_scalar!(
            r#"SELECT id FROM channels WHERE name = $1 AND deleted_at IS NULL"#,
            name
        )
        .fetch_optional(pg)
        .await?;
        if let Some(channel_id) = existing {
            sqlx::query!(
                r#"UPDATE channels SET provider = $2, api_base = NULLIF($3, ''),
                   models = $4, priority = $5, status = $6, updated_at = now()
                   WHERE id = $1"#,
                channel_id,
                provider,
                base_url,
                serde_json::json!(models),
                priority,
                row_status
            )
            .execute(pg)
            .await?;
            let source = KeySource {
                name: &name,
                credential,
                weight,
                master_key,
            };
            sync_imported_key(pg, channel_id, &source, &mut stats.skipped).await?;
        } else {
            let model_refs: Vec<&str> = models.iter().map(String::as_str).collect();
            let (channel_id, key_id) = okapi_store::provision::create_channel(
                pg,
                &name,
                provider,
                base_url,
                credential,
                &model_refs,
                false,
                master_key,
            )
            .await?;
            sqlx::query!(
                r#"UPDATE channels SET priority = $2, status = $3 WHERE id = $1"#,
                channel_id,
                priority,
                row_status
            )
            .execute(pg)
            .await?;
            sqlx::query!(
                r#"UPDATE channel_keys SET weight = $2 WHERE id = $1"#,
                key_id,
                weight
            )
            .execute(pg)
            .await?;
        }
    }

    // ---- models → model_pricing ----（token: USD/1K → 倍率；request: → per_call）
    let mut seen_models: std::collections::HashSet<String> = std::collections::HashSet::new();
    for row in read_jsonl(dir, "models.jsonl")? {
        let row = row?;
        let Some(code) = s(&row, "model_code") else {
            stats.skipped.push("models: 缺 model_code".to_owned());
            continue;
        };
        if old_status(s(&row, "status")) != 1 {
            stats.skipped.push(format!("models:{code} 非 active 不迁"));
            continue;
        }
        if !seen_models.insert(code.to_owned()) {
            stats.skipped.push(format!(
                "models:{code} 重名（多 provider 同码），首个已生效"
            ));
            continue;
        }
        let pricing_type = s(&row, "pricing_type").unwrap_or("token");
        match pricing_type {
            "token" => {
                let Some(in_micro) =
                    dec_field(&row, "input_price").and_then(|p| dec_str_to_micro(&p, false))
                else {
                    stats.skipped.push(format!("models:{code} 缺 input_price"));
                    continue;
                };
                let out_micro = dec_field(&row, "output_price")
                    .and_then(|p| dec_str_to_micro(&p, false))
                    .unwrap_or(in_micro);
                // model_ratio = in_micro/2000，6 位定点：×1e6/2000 = ×500
                let model_ratio_e6 = in_micro.saturating_mul(500);
                let completion_e6 = if in_micro > 0 {
                    out_micro.saturating_mul(1_000_000) / in_micro
                } else {
                    1_000_000
                };
                let cache_e6 = dec_field(&row, "cached_input_price")
                    .and_then(|p| dec_str_to_micro(&p, false))
                    .filter(|_| in_micro > 0)
                    .map_or(1_000_000, |c| c.saturating_mul(1_000_000) / in_micro);
                stats.models += 1;
                if dry_run {
                    continue;
                }
                // 老库无缓存写入价字段（只有 cached_input/output_price = 读取侧），
                // 故写入倍率留 1.0，迁移后由管理端按 provider 官方定价配置
                okapi_store::admin::upsert_model_ratio(
                    pg,
                    code,
                    okapi_store::admin::RatioAxes::basic(
                        &e6_to_dec(model_ratio_e6),
                        &e6_to_dec(completion_e6),
                        &e6_to_dec(cache_e6),
                    ),
                )
                .await?;
            }
            "request" | "per_call" => {
                let Some(per_call) =
                    dec_field(&row, "request_price").and_then(|p| dec_str_to_micro(&p, false))
                else {
                    stats
                        .skipped
                        .push(format!("models:{code} 缺 request_price"));
                    continue;
                };
                stats.models += 1;
                if dry_run {
                    continue;
                }
                okapi_store::admin::upsert_model_per_call(pg, code, per_call).await?;
            }
            other => {
                stats.skipped.push(format!(
                    "models:{code} pricing_type={other} 不迁（无对应语义）"
                ));
            }
        }
    }

    Ok(stats)
}
