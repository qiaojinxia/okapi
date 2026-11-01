//! 迁移工具：new-api 与老 ok-api（Go/UUID schema）→ Okapi。
//!
//! ## new-api（IMPLEMENTATION §11 注记）：JSONL 三表导出
//!
//! 导出（在 new-api 库上执行，任一能出 JSONL 的方式均可，如 mysql-shell
//! `util.exportTable` 或 `SELECT JSON_OBJECT(...)`）：
//!   users.jsonl    ：id, username, email, role, status, quota, "group"
//!   tokens.jsonl   ：user_id, name, key, status, expired_time, "group"
//!   channels.jsonl ：name, type, key, base_url, models, priority, weight, status, "group"
//!
//! 语义：quota→micro ×2（500000 quota = $1）；幂等 upsert；余额走
//! billing_events（actor system:migrate）+ Redis credit；日志表不迁。
//!
//! 分组映射（IMPLEMENTATION §11.14）：new-api 的"分组"同时是价档与渠道集合，Okapi 拆成
//! 价格分组 + 渠道池。每个出现过的分组名 G 建同名池 `G` 与同名价格分组 `G`（倍率 1，
//! 站长事后按 GroupRatio 调）并让分组指向该池；渠道 `group="default,vip"` → 入 default 与
//! vip 两个池；用户 `group` → user_groups；令牌 `group`（非 auto/空）→ group_override。
//! new-api 的 `auto` 令牌分组没有一一对应物（我方靠池级降级表达），按跟随用户分组处理。
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
//!
//! 本文件放两条迁移共用的读入与落库小工具；new-api 在 newapi，老 ok-api 在 okapi_old。

use serde_json::Value;
use sqlx::PgPool;
use std::path::Path;

mod newapi;
mod okapi_old;

pub use self::newapi::run_newapi;
#[cfg(test)]
use self::okapi_old::dec_str_to_micro;
#[cfg(test)]
use self::okapi_old::decrypt_old;
#[cfg(test)]
use self::okapi_old::e6_to_dec;
#[cfg(test)]
use self::okapi_old::pbkdf2_sha256_block1;
pub use self::okapi_old::{OldStats, derive_old_key, encrypt_old, run_okapi_old};

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
        3 => ("azure", true),
        14 => ("anthropic", true),
        24 | 25 => ("gemini", true),
        _ => ("openai_compat", false),
    }
}

/// 导出目录要真的是这次的导出：目录不存在、或一份认得的 JSONL 都没有（多半是路径拼错），
/// 不能当成「源库是空的」跑出 users=0 / keys=0 的假成功。缺个别文件仍按部分导出处理。
fn require_export(dir: &Path, files: &[&str]) -> anyhow::Result<()> {
    anyhow::ensure!(dir.is_dir(), "迁移目录 {} 不存在或不是目录", dir.display());
    anyhow::ensure!(
        files.iter().any(|name| dir.join(name).is_file()),
        "迁移目录 {} 里没有任何导出文件（应有 {} 之一），检查 --dir 是否写错",
        dir.display(),
        files.join(" / ")
    );
    Ok(())
}

/// 一份 JSONL 逐行读出来的行：内存里始终只有当前这一行，导出文件再大也不会整份读进来。
struct JsonlRows {
    name: String,
    lines: Option<std::io::Lines<std::io::BufReader<std::fs::File>>>,
    lineno: usize,
}

impl JsonlRows {
    fn open(path: &Path, name: &str) -> anyhow::Result<Self> {
        let file = std::fs::File::open(path)?;
        Ok(Self {
            name: name.to_owned(),
            lines: Some(std::io::BufRead::lines(std::io::BufReader::new(file))),
            lineno: 0,
        })
    }
}

impl Iterator for JsonlRows {
    type Item = anyhow::Result<Value>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let line = self.lines.as_mut()?.next()?;
            self.lineno += 1;
            let parsed = line
                .map_err(|e| anyhow::anyhow!("{}:{} 读取失败：{e}", self.name, self.lineno))
                .and_then(|line| {
                    let line = line.trim();
                    if line.is_empty() {
                        return Ok(None);
                    }
                    serde_json::from_str(line).map(Some).map_err(|e| {
                        anyhow::anyhow!("{}:{} 非法 JSON：{e}", self.name, self.lineno)
                    })
                });
            match parsed {
                Ok(None) => {}
                Ok(Some(value)) => return Some(Ok(value)),
                Err(error) => {
                    // 出错之后不再往下读：后面的行已经没有意义
                    self.lines = None;
                    return Some(Err(error));
                }
            }
        }
    }
}

/// 读一份导出文件（不存在 = 没有行）。先整份扫一遍校验（只解析、不留存），非法行在这个文件的
/// 任何一行写库之前就报出来；再返回惰性迭代器，边读边处理。
fn read_jsonl(dir: &Path, name: &str) -> anyhow::Result<JsonlRows> {
    let path = dir.join(name);
    if !path.exists() {
        return Ok(JsonlRows {
            name: name.to_owned(),
            lines: None,
            lineno: 0,
        });
    }
    for row in JsonlRows::open(&path, name)? {
        row?;
    }
    JsonlRows::open(&path, name)
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

/// new-api `tokens.expired_time`：Unix 秒；-1（偶见 0）= 永不过期 → None。
fn expired_time(row: &Value) -> Option<chrono::DateTime<chrono::Utc>> {
    i(row, "expired_time")
        .filter(|secs| *secs > 0)
        .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
}

/// 重跑时源系统里这条渠道的 key 与权重。
struct KeySource<'a> {
    name: &'a str,
    credential: &'a str,
    weight: i32,
    master_key: Option<&'a str>,
}

/// 把重跑的源 key / 权重同步到导入时建的那把 key。对得上的只有那一把：渠道后来被运维加了第二把 key
/// 就不猜是哪一把，记注记留给人。凭证没变就不动（轮换会把 key 状态机复位，不该因为重跑清掉冷却 / 失效标记）。
async fn sync_imported_key(
    pg: &PgPool,
    channel_id: i64,
    source: &KeySource<'_>,
    skipped: &mut Vec<String>,
) -> anyhow::Result<()> {
    let keys = sqlx::query_scalar!(
        r#"SELECT id FROM channel_keys WHERE channel_id = $1 ORDER BY id LIMIT 2"#,
        channel_id
    )
    .fetch_all(pg)
    .await?;
    let [key_id] = keys.as_slice() else {
        if keys.len() > 1 {
            skipped.push(format!(
                "channels:{} 有多把 key，重跑不更新凭证与权重",
                source.name
            ));
        }
        return Ok(());
    };
    // 读不出（主密钥对不上等）按「不同」处理：宁可用源系统的凭证覆盖，也不留一把读不了的
    let current = okapi_store::admin::read_key_credential(pg, *key_id, source.master_key)
        .await
        .ok()
        .flatten();
    if current.as_deref() != Some(source.credential) {
        okapi_store::admin::rotate_channel_credential(
            pg,
            channel_id,
            Some(*key_id),
            source.credential,
            source.master_key,
        )
        .await?;
    }
    okapi_store::admin::patch_channel_key(pg, channel_id, *key_id, Some(source.weight), None, None)
        .await?;
    Ok(())
}

/// new-api 分组名 → 同名池 + 同名价格分组（倍率 1，站长事后按 GroupRatio 调）。
/// `default` 已由迁移 0002 内置，直接跳过。幂等。
async fn ensure_group_and_pool(pg: &PgPool, group: &str) -> anyhow::Result<()> {
    if group == okapi_store::channels::DEFAULT_POOL {
        return Ok(());
    }
    sqlx::query!(
        r#"INSERT INTO channel_pools (pool_code, description, routing_strategy)
           VALUES ($1, 'new-api 分组导入', 'priority_weighted')
           ON CONFLICT (pool_code) DO NOTHING"#,
        group
    )
    .execute(pg)
    .await?;
    sqlx::query!(
        r#"INSERT INTO price_groups (group_code, group_ratio, description, pool_code)
           VALUES ($1, 1, 'new-api 分组导入（倍率待按 GroupRatio 调整）', $1)
           ON CONFLICT (group_code) DO NOTHING"#,
        group
    )
    .execute(pg)
    .await?;
    Ok(())
}

/// 源 id 的前 8 个**字符**，用来给撞名的用户名消歧。id 来自导入文件，可能含多字节字符：
/// 按字节切（`&id[..8]`）会在字符中间 panic，一份坏数据就能让整个导入退出。
fn short_id(src_id: &str) -> String {
    src_id.chars().take(8).collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("okapi-jsonl-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 导出文件逐行读：空行跳过、缺文件当没有行；非法行在任何一行被处理之前整体报错并带上行号。
    #[test]
    fn jsonl_is_read_row_by_row_and_validated_before_the_first_row_is_handed_out() {
        let dir = scratch_dir();
        std::fs::write(dir.join("users.jsonl"), "{\"id\":1}\n\n  \n{\"id\":2}\n").unwrap();
        let ids: Vec<i64> = read_jsonl(&dir, "users.jsonl")
            .unwrap()
            .map(|row| row.unwrap()["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, [1, 2]);
        assert_eq!(read_jsonl(&dir, "absent.jsonl").unwrap().count(), 0);

        // 第 3 行非法：一行都不放出来，报错带文件名与行号
        std::fs::write(dir.join("tokens.jsonl"), "{\"id\":1}\n{\"id\":2}\n{oops\n").unwrap();
        let Err(error) = read_jsonl(&dir, "tokens.jsonl") else {
            panic!("非法行必须在放出任何一行之前报错");
        };
        assert!(
            error.to_string().starts_with("tokens.jsonl:3 非法 JSON"),
            "{error}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 一行一行地读：很多行的文件迭代时每行都是独立解析出来的，不需要先整份进内存。
    #[test]
    fn a_large_export_is_walked_without_being_collected() {
        let dir = scratch_dir();
        let line = format!("{{\"pad\":\"{}\"}}\n", "x".repeat(1000));
        std::fs::write(dir.join("channels.jsonl"), line.repeat(2_000)).unwrap();
        let mut rows = read_jsonl(&dir, "channels.jsonl").unwrap();
        assert!(rows.next().unwrap().is_ok());
        assert_eq!(rows.lineno, 1, "只读到第一行，其余还留在文件里");
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
    fn short_id_takes_characters_not_bytes() {
        assert_eq!(short_id("0123456789"), "01234567");
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(short_id(""), "");
        // 第 8 个字节落在 'é'（2 字节）中间：按字节切会 panic
        assert_eq!(short_id("abcdefgéhij"), "abcdefgé");
        assert_eq!(short_id("用户编号一二三四五六"), "用户编号一二三四");
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
