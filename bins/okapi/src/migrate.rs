//! 迁移工具：new-api 与老 ok-api（Go/UUID schema）→ Okapi。
//!
//! ## new-api（IMPLEMENTATION §11 注记）：JSONL 三表导出
//!
//! 导出（在 new-api 库上执行，任一能出 JSONL 的方式均可，如 mysql-shell
//! `util.exportTable` 或 `SELECT JSON_OBJECT(...)`）：
//!   users.jsonl    ：id, username, email, role, status, quota, "group"
//!   tokens.jsonl   ：user_id, name, key, status, expired_time
//!   channels.jsonl ：name, type, key, base_url, models, priority, weight, status
//!
//! 语义：quota→micro ×2（500000 quota = $1）；幂等 upsert；余额走
//! billing_events（actor system:migrate）+ Redis credit；日志表不迁。
//!
//! ## 老 ok-api：JSONL 五表导出（PG `\copy (SELECT row_to_json(t)) TO ...`）
//!
//!   users.jsonl             ：id(uuid), email, username, name, password_hash, role, status, balance
//!   api_keys.jsonl          ：user_id(uuid), name, key_prefix, key_hash, key_encrypted, status,
//!                             allowed_models, rate_limit_rpm, expires_at
//!   providers.jsonl         ：id, provider_code, provider_name, api_endpoint, status
//!   provider_api_keys.jsonl ：provider_id, key_name, api_key, base_url, adapter_type,
//!                             supported_models, weight, priority, status
//!   models.jsonl            ：model_code, pricing_type, input_price, output_price,
//!                             cached_input_price, request_price, status
//!
//! 语义（老系统 → Okapi）：
//! - balance DECIMAL(20,8) USD → micro-USD（字符串定点截断，禁浮点）；≤0 不入账记注记。
//! - 密码 bcrypt 哈希原样迁移，登录由 `identity::verify_password` 按 `$2` 前缀兼容验证。
//! - API key：老库 key_hash 为 bcrypt 不可逆，依赖 `key_encrypted`（AES-256-GCM，
//!   密钥 = PBKDF2-HMAC-SHA256(passphrase, SHA256("okapi-key-derivation:"+pass)[..16], 100k, 32B)
//!   与老 Go `pkg/crypto` 完全一致）解出 `sk-` 明文重新 SHA-256；无密文/解密失败 → 跳过记注记。
//! - providers×provider_api_keys → 每 key 一个 channel（保留独立 base_url/models/权重路由属性）。
//! - models：`token` 型单价（USD/1K tokens）→ 倍率（基准 $0.002/1K）；`request` 型 → per_call；
//!   其余计价类型（hourly/monthly）不迁记注记。定价规则绑定表（pricing_rules）不迁——
//!   Okapi 以 price_groups + user_pricing 表达（吸收语义不照搬结构）。

use okapi_domain::Money;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::path::Path;

#[derive(Debug, Default)]
pub struct MigrateStats {
    pub users: u64,
    pub users_credited: u64,
    pub keys: u64,
    pub channels: u64,
    pub skipped: Vec<String>,
}

/// new-api channels.type → Okapi provider。
fn provider_of(newapi_type: i64) -> (&'static str, bool) {
    match newapi_type {
        1 => ("openai", true),
        14 => ("anthropic", true),
        24 | 25 => ("gemini", true),
        _ => ("openai_compat", false),
    }
}

fn read_jsonl(dir: &Path, name: &str) -> anyhow::Result<Vec<Value>> {
    let path = dir.join(name);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)?;
    let mut rows = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .map_err(|e| anyhow::anyhow!("{name}:{} 非法 JSON：{e}", lineno + 1))?;
        rows.push(value);
    }
    Ok(rows)
}

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|x| !x.is_empty())
}

fn i(v: &Value, key: &str) -> Option<i64> {
    let field = v.get(key)?;
    field
        .as_i64()
        .or_else(|| field.as_str().and_then(|x| x.parse().ok()))
}

/// 执行迁移。`ledger` 为 None 时（dry-run 或 Redis 不可用）余额只统计不入账。
// 线性三表导入脚本，拆分割裂迁移时序
#[allow(clippy::too_many_lines)]
pub async fn run_newapi(
    pg: &PgPool,
    ledger: Option<&okapi_ledger::BalanceLedger>,
    dir: &Path,
    dry_run: bool,
) -> anyhow::Result<MigrateStats> {
    let mut stats = MigrateStats::default();

    // ---- users ----
    // 源 id → 新 id（tokens 关联用）
    let mut user_map: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    for row in read_jsonl(dir, "users.jsonl")? {
        let Some(username) = s(&row, "username") else {
            stats.skipped.push(format!("users: 缺 username（{row}）"));
            continue;
        };
        let Some(src_id) = i(&row, "id") else {
            stats.skipped.push(format!("users:{username} 缺 id"));
            continue;
        };
        let quota = i(&row, "quota").unwrap_or(0);
        // 500000 quota = $1 = 1_000_000 micro → ×2
        let balance_micro = quota.saturating_mul(2);
        let role: i16 = match i(&row, "role").unwrap_or(1) {
            r if r >= 100 => 100,
            r if r >= 10 => 10,
            _ => 1,
        };
        let row_status: i16 = if i(&row, "status").unwrap_or(1) == 1 {
            1
        } else {
            2
        };
        stats.users += 1;
        if balance_micro > 0 {
            stats.users_credited += 1;
        }
        if dry_run {
            user_map.insert(src_id, -1);
            continue;
        }
        let email = s(&row, "email");
        let user_id = sqlx::query_scalar!(
            r#"
            INSERT INTO users (username, email, role, status)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (username) DO UPDATE SET
                email = COALESCE(EXCLUDED.email, users.email),
                role = EXCLUDED.role,
                status = EXCLUDED.status,
                updated_at = now()
            RETURNING id
            "#,
            username,
            email,
            role,
            row_status
        )
        .fetch_one(pg)
        .await?;
        user_map.insert(src_id, user_id);

        // 余额：幂等锚 = 每用户一条 newapi_import 事件
        if balance_micro > 0 {
            let already = sqlx::query_scalar!(
                r#"SELECT COUNT(*)::bigint AS "c!" FROM billing_events
                   WHERE user_id = $1 AND actor = 'system:migrate'"#,
                user_id
            )
            .fetch_one(pg)
            .await?;
            if already == 0 {
                let amount = Money::from_micros(balance_micro);
                if let Some(ledger) = ledger {
                    ledger.credit(user_id, amount).await?;
                }
                okapi_ledger::pg::record_credit(
                    pg,
                    user_id,
                    amount,
                    "adjust",
                    "system:migrate",
                    serde_json::json!({"tags": ["newapi_import"], "src_quota": quota}),
                )
                .await?;
            }
        }
    }

    // ---- tokens → api_keys ----
    for row in read_jsonl(dir, "tokens.jsonl")? {
        let Some(key) = s(&row, "key") else {
            stats.skipped.push("tokens: 缺 key".to_owned());
            continue;
        };
        let Some(new_user) = i(&row, "user_id").and_then(|src| user_map.get(&src).copied()) else {
            stats
                .skipped
                .push(format!("tokens: user_id 无映射（{row}）"));
            continue;
        };
        stats.keys += 1;
        if dry_run {
            continue;
        }
        // new-api 存明文 key（可能不带 sk- 前缀）；Okapi 只存 SHA-256
        let full = if key.starts_with("sk-") {
            key.to_owned()
        } else {
            format!("sk-{key}")
        };
        let key_hash = hex::encode(Sha256::digest(full.as_bytes()));
        let prefix: String = full.chars().take(16).collect();
        let name = s(&row, "name").unwrap_or("newapi-import");
        let row_status: i16 = if i(&row, "status").unwrap_or(1) == 1 {
            1
        } else {
            2
        };
        sqlx::query!(
            r#"
            INSERT INTO api_keys (user_id, key_hash, key_prefix, name, status)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (key_hash) DO UPDATE SET status = EXCLUDED.status
            "#,
            new_user,
            key_hash,
            prefix,
            name,
            row_status
        )
        .execute(pg)
        .await?;
    }

    // ---- channels ----
    for row in read_jsonl(dir, "channels.jsonl")? {
        let Some(name) = s(&row, "name") else {
            stats.skipped.push("channels: 缺 name".to_owned());
            continue;
        };
        let Some(credential) = s(&row, "key") else {
            stats.skipped.push(format!("channels:{name} 缺 key"));
            continue;
        };
        let newapi_type = i(&row, "type").unwrap_or(1);
        let (provider, exact) = provider_of(newapi_type);
        if !exact {
            stats.skipped.push(format!(
                "channels:{name} type={newapi_type} 按 openai_compat 导入"
            ));
        }
        let models: Vec<String> = s(&row, "models")
            .map(|m| {
                m.split(',')
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        stats.channels += 1;
        if dry_run {
            continue;
        }
        let base_url = s(&row, "base_url").unwrap_or("");
        let row_status: i16 = if i(&row, "status").unwrap_or(1) == 1 {
            1
        } else {
            2
        };
        let priority = i32::try_from(i(&row, "priority").unwrap_or(0)).unwrap_or(0);
        let weight = i32::try_from(i(&row, "weight").unwrap_or(1))
            .unwrap_or(1)
            .max(1);
        // 幂等：name 唯一 upsert（new-api 渠道名可重复——追加 #type 消歧由导出侧保证）
        let channel_id = sqlx::query_scalar!(
            r#"
            SELECT id FROM channels WHERE name = $1 AND deleted_at IS NULL
            "#,
            name
        )
        .fetch_optional(pg)
        .await?;
        if let Some(channel_id) = channel_id {
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
        } else {
            let model_refs: Vec<&str> = models.iter().map(String::as_str).collect();
            let (channel_id, key_id) = okapi_store::provision::create_channel(
                pg,
                name,
                provider,
                base_url,
                credential,
                &model_refs,
                false,
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

    Ok(stats)
}

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
fn pbkdf2_sha256_block1(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
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
fn decrypt_old(key: &[u8; 32], cipher_b64: &str) -> Option<String> {
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
fn dec_str_to_micro(s: &str, allow_neg: bool) -> Option<i64> {
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
fn e6_to_dec(v: i64) -> String {
    format!("{}.{:06}", v / 1_000_000, (v % 1_000_000).unsigned_abs())
}

/// 数值字段读取：JSON 字符串原样、JSON 数字取字面量（DECIMAL 建议 ::text 导出保精度）。
fn dec_field(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn old_role(role: &str) -> i16 {
    match role {
        "super_admin" => 100,
        "admin" => 10,
        _ => 1,
    }
}

fn old_status(status: Option<&str>) -> i16 {
    if status.unwrap_or("active") == "active" {
        1
    } else {
        2
    }
}

/// provider_code / adapter_type → Okapi provider。
fn old_provider(code: &str, adapter: Option<&str>) -> &'static str {
    let norm = adapter.filter(|a| !a.is_empty()).unwrap_or(code);
    match norm.to_ascii_lowercase().as_str() {
        "openai" => "openai",
        "anthropic" | "claude" => "anthropic",
        "gemini" | "google" => "gemini",
        _ => "openai_compat",
    }
}

/// supported/allowed models 字段：JSON 数组或逗号分隔字符串两种导出形态都接受。
fn model_list(v: &Value, key: &str) -> Vec<String> {
    match v.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(Value::String(s)) => s
            .split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

const OLD_ACTOR: &str = "system:migrate:okapi_old";

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
) -> anyhow::Result<OldStats> {
    let mut stats = OldStats::default();
    let enc_key = enc_passphrase.map(derive_old_key);

    // ---- users ----（uuid → 新 id）
    let mut user_map: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for row in read_jsonl(dir, "users.jsonl")? {
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
        let balance_micro = dec_field(&row, "balance")
            .and_then(|b| dec_str_to_micro(&b, true))
            .unwrap_or(0);
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
            let alt = format!("{username}-{}", &src_id[..src_id.len().min(8)]);
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
            let already = sqlx::query_scalar!(
                r#"SELECT COUNT(*)::bigint AS "c!" FROM billing_events
                   WHERE user_id = $1 AND actor = $2"#,
                user_id,
                OLD_ACTOR
            )
            .fetch_one(pg)
            .await?;
            if already == 0 {
                let amount = Money::from_micros(balance_micro);
                if let Some(ledger) = ledger {
                    ledger.credit(user_id, amount).await?;
                }
                okapi_ledger::pg::record_credit(
                    pg,
                    user_id,
                    amount,
                    "adjust",
                    OLD_ACTOR,
                    serde_json::json!({"tags": ["okapi_old_import"], "src_user": src_id}),
                )
                .await?;
            }
        }
    }

    // ---- api_keys ----（bcrypt 不可逆：依赖 key_encrypted 解密重哈希）
    for row in read_jsonl(dir, "api_keys.jsonl")? {
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
        if let (Some(id), Some(code)) = (i(&row, "id"), s(&row, "provider_code")) {
            let endpoint = s(&row, "api_endpoint").unwrap_or("").to_owned();
            providers.insert(id, (code.to_owned(), endpoint));
        }
    }
    for row in read_jsonl(dir, "provider_api_keys.jsonl")? {
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
                    &e6_to_dec(model_ratio_e6),
                    &e6_to_dec(completion_e6),
                    &e6_to_dec(cache_e6),
                    "1",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7914 §11 PBKDF2-HMAC-SHA256 测试向量（P="passwd" S="salt" c=1 dkLen=64 的前 32B）。
    #[test]
    fn pbkdf2_rfc7914_vector() {
        let dk = pbkdf2_sha256_block1(b"passwd", b"salt", 1);
        assert_eq!(
            hex::encode(dk),
            "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc"
        );
    }

    #[test]
    fn old_key_encrypt_decrypt_roundtrip() {
        let key = derive_old_key("test-passphrase");
        let ct = encrypt_old(&key, "sk-abc123").unwrap();
        assert_eq!(decrypt_old(&key, &ct).as_deref(), Some("sk-abc123"));
        let wrong = derive_old_key("wrong");
        assert_eq!(decrypt_old(&wrong, &ct), None);
    }

    #[test]
    fn dec_str_fixed_point() {
        assert_eq!(dec_str_to_micro("123.45678901", true), Some(123_456_789));
        assert_eq!(dec_str_to_micro("0.03", false), Some(30_000));
        assert_eq!(dec_str_to_micro("-5.5", true), Some(-5_500_000));
        assert_eq!(dec_str_to_micro("-5.5", false), None);
        assert_eq!(dec_str_to_micro("2", false), Some(2_000_000));
        assert_eq!(dec_str_to_micro(".5", false), Some(500_000));
        assert_eq!(dec_str_to_micro("abc", false), None);
        assert_eq!(dec_str_to_micro("", false), None);
    }

    #[test]
    fn ratio_formatting() {
        // $0.03/1K → model_ratio 15（0.03/0.002）
        assert_eq!(e6_to_dec(30_000i64.saturating_mul(500)), "15.000000");
        // completion = 0.06/0.03 = 2
        assert_eq!(e6_to_dec(60_000i64 * 1_000_000 / 30_000), "2.000000");
    }
}
