//! 上游倍率在线同步（IMPLEMENTATION §11.36，对照 new-api ratio_sync）：拉取若干公开定价源，
//! 逐模型逐轴与本地对比，管理员勾选后逐项应用。不落新表、不自动发布 epoch。
//!
//! 三种源形状（判据与 new-api 一致）：ratio_config 对象、new-api `/api/pricing` 列表、Okapi `/api/pricing`。
//! 数值全部以十进制字面量流转、经 `RatioFp` 定点校验，不经浮点。

use super::admin::{audit, guard};
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use okapi_pricing::RatioFp;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

const MAX_SOURCES: usize = 8;
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 10;
const MAX_TIMEOUT_SECS: u64 = 60;

/// 可同步的轴（与 `RatioAxes` 七轴 + 按次价对齐）。
pub const AXES: [&str; 8] = [
    "model_ratio",
    "completion_ratio",
    "cache_ratio",
    "cache_write_ratio",
    "audio_ratio",
    "audio_completion_ratio",
    "image_ratio",
    "per_call_price",
];

/// 一个模型在某个源 / 本地的定价：轴名 → 十进制字面量（已过 `RatioFp` 校验）。
pub type ModelAxes = BTreeMap<&'static str, String>;
pub type PricingTable = BTreeMap<String, ModelAxes>;

#[derive(Deserialize)]
pub struct SourceReq {
    pub name: String,
    pub url: String,
}

#[derive(Deserialize)]
pub struct FetchReq {
    pub sources: Vec<SourceReq>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// JSON 数字 / 数字字符串 → 合法的非负十进制字面量（`RatioFp` 能解析的才算）。
fn literal(v: &Value) -> Option<String> {
    let text = match v {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.trim().to_owned(),
        _ => return None,
    };
    text.parse::<RatioFp>().ok().map(|_| text)
}

fn axis_key(name: &str) -> Option<&'static str> {
    AXES.iter().copied().find(|a| *a == name)
}

/// ratio_config 对象：`{model_ratio: {m: v}, completion_ratio: {...}, create_cache_ratio, model_price, ...}`。
fn parse_ratio_config(obj: &Map<String, Value>) -> PricingTable {
    // new-api 的键名 → 我们的轴名
    const KEYS: [(&str, &str); 8] = [
        ("model_ratio", "model_ratio"),
        ("completion_ratio", "completion_ratio"),
        ("cache_ratio", "cache_ratio"),
        ("create_cache_ratio", "cache_write_ratio"),
        ("cache_write_ratio", "cache_write_ratio"),
        ("audio_ratio", "audio_ratio"),
        ("audio_completion_ratio", "audio_completion_ratio"),
        ("image_ratio", "image_ratio"),
    ];
    let mut table = PricingTable::new();
    for (src, axis) in KEYS {
        let Some(map) = obj.get(src).and_then(Value::as_object) else {
            continue;
        };
        let axis = axis_key(axis).unwrap_or("model_ratio");
        for (model, v) in map {
            if let Some(lit) = literal(v) {
                table.entry(model.clone()).or_default().insert(axis, lit);
            }
        }
    }
    if let Some(prices) = obj.get("model_price").and_then(Value::as_object) {
        for (model, v) in prices {
            if let Some(lit) = literal(v) {
                table
                    .entry(model.clone())
                    .or_default()
                    .insert("per_call_price", lit);
            }
        }
    }
    table
}

/// new-api `/api/pricing`：`[{model_name, quota_type, model_ratio, model_price, completion_ratio, ...}]`。
fn parse_newapi_pricing(items: &[Value]) -> PricingTable {
    let mut table = PricingTable::new();
    for item in items {
        let Some(model) = item
            .get("model_name")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
        else {
            continue;
        };
        let axes = table.entry(model.to_owned()).or_default();
        let per_call = item.get("quota_type").and_then(Value::as_i64) == Some(1);
        if per_call {
            if let Some(lit) = item.get("model_price").and_then(literal) {
                axes.insert("per_call_price", lit);
            }
        } else {
            for (src, axis) in [
                ("model_ratio", "model_ratio"),
                ("completion_ratio", "completion_ratio"),
            ] {
                if let Some(lit) = item.get(src).and_then(literal) {
                    axes.insert(axis_key(axis).unwrap_or("model_ratio"), lit);
                }
            }
        }
        for (src, axis) in [
            ("cache_ratio", "cache_ratio"),
            ("create_cache_ratio", "cache_write_ratio"),
            ("image_ratio", "image_ratio"),
            ("audio_ratio", "audio_ratio"),
            ("audio_completion_ratio", "audio_completion_ratio"),
        ] {
            if let Some(lit) = item.get(src).and_then(literal) {
                axes.insert(axis_key(axis).unwrap_or("model_ratio"), lit);
            }
        }
        if axes.is_empty() {
            table.remove(model);
        }
    }
    table
}

/// Okapi `/api/pricing`：`{models: [{model, mode, model_ratio, ..., per_call_price_micro}]}`。
fn parse_okapi_pricing(models: &[Value]) -> PricingTable {
    let mut table = PricingTable::new();
    for item in models {
        let Some(model) = item
            .get("model")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
        else {
            continue;
        };
        let axes = table.entry(model.to_owned()).or_default();
        if item.get("mode").and_then(Value::as_str) == Some("per_call") {
            if let Some(micro) = item.get("per_call_price_micro").and_then(Value::as_i64) {
                axes.insert("per_call_price", micro_to_usd_literal(micro));
            }
        } else {
            for axis in &AXES[..7] {
                if let Some(lit) = item.get(*axis).and_then(literal) {
                    axes.insert(axis, lit);
                }
            }
        }
        if axes.is_empty() {
            table.remove(model);
        }
    }
    table
}

/// micro-USD 整数 → 十进制美元字面量（整数运算，去尾零）。
fn micro_to_usd_literal(micro: i64) -> String {
    let whole = micro / 1_000_000;
    let frac = (micro % 1_000_000).unsigned_abs();
    if frac == 0 {
        return whole.to_string();
    }
    let frac = format!("{frac:06}");
    let frac = frac.trim_end_matches('0');
    if micro < 0 && whole == 0 {
        format!("-0.{frac}")
    } else {
        format!("{whole}.{frac}")
    }
}

/// 识别并解析一份源响应；None = 形状不认。
pub fn parse_source(body: &Value) -> Option<PricingTable> {
    // new-api 两种形状都包在 {success, data} 里；静态文件可能就是裸 ratio_config
    let data = match body.get("data") {
        Some(d) if body.get("success").and_then(Value::as_bool) != Some(false) => d,
        Some(_) => return None,
        None => body,
    };
    if let Some(obj) = data.as_object() {
        if AXES.iter().any(|a| obj.contains_key(*a))
            || obj.contains_key("create_cache_ratio")
            || obj.contains_key("model_price")
        {
            return Some(parse_ratio_config(obj));
        }
        if let Some(models) = obj.get("models").and_then(Value::as_array) {
            return Some(parse_okapi_pricing(models));
        }
        return None;
    }
    data.as_array().map(|items| parse_newapi_pricing(items))
}

/// 本地定价表（与源同一形状，供对比）。
async fn local_table(pg: &sqlx::PgPool) -> Result<PricingTable, AppError> {
    let rows = sqlx::query!(
        r#"SELECT m.model_name, p.pricing_mode,
                  p.model_ratio::text AS model_ratio,
                  p.completion_ratio::text AS "completion_ratio!",
                  p.cache_ratio::text AS "cache_ratio!",
                  p.cache_write_ratio::text AS "cache_write_ratio!",
                  p.audio_ratio::text AS "audio_ratio!",
                  p.audio_completion_ratio::text AS "audio_completion_ratio!",
                  p.image_ratio::text AS "image_ratio!",
                  p.per_call_price_micro
           FROM model_pricing p JOIN models m ON m.id = p.model_id"#
    )
    .fetch_all(pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let mut table = PricingTable::new();
    for r in rows {
        let mut axes = ModelAxes::new();
        if r.pricing_mode == "per_call" {
            if let Some(micro) = r.per_call_price_micro {
                axes.insert("per_call_price", micro_to_usd_literal(micro));
            }
        } else {
            if let Some(v) = r.model_ratio {
                axes.insert("model_ratio", canonical(&v));
            }
            axes.insert("completion_ratio", canonical(&r.completion_ratio));
            axes.insert("cache_ratio", canonical(&r.cache_ratio));
            axes.insert("cache_write_ratio", canonical(&r.cache_write_ratio));
            axes.insert("audio_ratio", canonical(&r.audio_ratio));
            axes.insert(
                "audio_completion_ratio",
                canonical(&r.audio_completion_ratio),
            );
            axes.insert("image_ratio", canonical(&r.image_ratio));
        }
        table.insert(r.model_name, axes);
    }
    Ok(table)
}

/// 十进制字面量规范化（`1.5000` 与 `1.5` 相等）：走定点整数再回字面量。
fn canonical(lit: &str) -> String {
    lit.parse::<RatioFp>()
        .map_or_else(|_| lit.to_owned(), |r| r.to_string())
}

fn same_value(a: &str, b: &str) -> bool {
    canonical(a) == canonical(b)
}

/// 差异表：模型 → 轴 → `{current, upstreams: {source: value | "same"}}`；全 same 的轴 / 模型不进表。
pub fn build_differences(local: &PricingTable, sources: &[(String, PricingTable)]) -> Value {
    let mut out = Map::new();
    let mut models: Vec<&String> = sources.iter().flat_map(|(_, t)| t.keys()).collect();
    models.sort();
    models.dedup();
    for model in models {
        let local_axes = local.get(model);
        let mut axes_out = Map::new();
        for axis in AXES {
            let current = local_axes.and_then(|a| a.get(axis));
            let mut upstreams = Map::new();
            let mut any_diff = false;
            for (name, table) in sources {
                let Some(v) = table.get(model).and_then(|a| a.get(axis)) else {
                    continue;
                };
                match current {
                    Some(c) if same_value(c, v) => {
                        upstreams.insert(name.clone(), Value::String("same".to_owned()));
                    }
                    _ => {
                        any_diff = true;
                        upstreams.insert(name.clone(), Value::String(canonical(v)));
                    }
                }
            }
            if any_diff {
                axes_out.insert(
                    axis.to_owned(),
                    json!({
                        "current": current.map(|c| canonical(c)),
                        "upstreams": upstreams,
                    }),
                );
            }
        }
        if !axes_out.is_empty() {
            out.insert(model.clone(), Value::Object(axes_out));
        }
    }
    Value::Object(out)
}

/// 拉一个源：SSRF 闸 → GET（无凭证）→ 2xx JSON ≤ 2MB → 识别形状。
async fn fetch_one(
    state: &AppState,
    url: &str,
    timeout: Duration,
) -> Result<PricingTable, &'static str> {
    if super::ssrf::validate_api_base(state, url).await.is_err() {
        return Err("source_url_rejected");
    }
    // 不跟随重定向：SSRF 闸只看得到管理员填的这个 URL，跟着 30x 走就能被引到私网 / 元数据地址
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "client_build")?;
    let resp = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "timeout"
            } else if e.is_connect() {
                "connect"
            } else {
                "request"
            }
        })?;
    if !resp.status().is_success() {
        return Err("upstream_status");
    }
    let body = resp.bytes().await.map_err(|_| "body")?;
    if body.len() > MAX_BODY_BYTES {
        return Err("body_too_large");
    }
    let value: Value = serde_json::from_slice(&body).map_err(|_| "not_json")?;
    parse_source(&value).ok_or("unrecognized_shape")
}

/// POST /admin/pricing/sync/fetch：并发拉取各源，返回差异表与各源状态。
pub async fn fetch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<FetchReq>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::PRICING_READ).await?;
    if req.sources.is_empty() || req.sources.len() > MAX_SOURCES {
        return Err(AppError::bad_request().with_param("sources"));
    }
    let timeout = Duration::from_secs(
        req.timeout_secs
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS),
    );
    let mut names = std::collections::HashSet::new();
    for s in &req.sources {
        let name = s.name.trim();
        if name.is_empty() || name.len() > 64 || !names.insert(name.to_owned()) {
            return Err(AppError::bad_request().with_param("sources.name"));
        }
    }

    let local = local_table(&state.pg).await?;
    let results = futures::future::join_all(req.sources.iter().map(|s| {
        let state = state.clone();
        async move {
            let name = s.name.trim().to_owned();
            (name, fetch_one(&state, s.url.trim(), timeout).await)
        }
    }))
    .await;

    let mut ok_sources: Vec<(String, PricingTable)> = Vec::new();
    let mut statuses = Vec::new();
    for (name, result) in results {
        match result {
            Ok(table) => {
                statuses.push(json!({ "name": name, "status": "ok", "models": table.len() }));
                ok_sources.push((name, table));
            }
            Err(code) => {
                statuses
                    .push(json!({ "name": name, "status": "error", "error": code, "models": 0 }));
            }
        }
    }
    let differences = build_differences(&local, &ok_sources);
    Ok(Json(
        json!({ "differences": differences, "sources": statuses }),
    ))
}

#[derive(Deserialize)]
pub struct ChangeReq {
    pub model: String,
    pub axis: String,
    /// 十进制字面量（倍率或按次 USD）。
    pub value: String,
}

#[derive(Deserialize)]
pub struct ApplyReq {
    pub changes: Vec<ChangeReq>,
}

/// POST /admin/pricing/sync/apply：逐项写入选中的改动；未选中的轴保持本地现值。
pub async fn apply(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ApplyReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    if req.changes.is_empty() || req.changes.len() > 2000 {
        return Err(AppError::bad_request().with_param("changes"));
    }
    for c in &req.changes {
        if c.model.trim().is_empty() || axis_key(&c.axis).is_none() {
            return Err(AppError::bad_request().with_param("axis"));
        }
        if c.value.parse::<RatioFp>().is_err() {
            return Err(AppError::bad_request().with_param("value"));
        }
    }

    // 同一模型的多轴改动合并成一次 upsert；其余轴取本地现值（缺省 1）
    let local = local_table(&state.pg).await?;
    let mut by_model: BTreeMap<String, Vec<(&'static str, String)>> = BTreeMap::new();
    for c in &req.changes {
        let axis = axis_key(&c.axis).unwrap_or("model_ratio");
        by_model
            .entry(c.model.trim().to_owned())
            .or_default()
            .push((axis, c.value.trim().to_owned()));
    }
    let mut applied = 0usize;
    for (model, changes) in &by_model {
        let mut axes: ModelAxes = local.get(model).cloned().unwrap_or_default();
        let mut per_call: Option<String> = None;
        for (axis, value) in changes {
            if *axis == "per_call_price" {
                per_call = Some(value.clone());
            } else {
                axes.insert(axis, value.clone());
            }
        }
        // 只有按次价改动时保持按次模式；倍率改动会把模型写成倍率模式（与 import 同一语义）
        let ratio_changed = changes.iter().any(|(a, _)| *a != "per_call_price");
        if ratio_changed {
            let get = |k: &str| axes.get(k).cloned().unwrap_or_else(|| "1".to_owned());
            let model_ratio = axes
                .get("model_ratio")
                .cloned()
                .ok_or_else(|| AppError::bad_request().with_param("model_ratio"))?;
            okapi_store::admin::upsert_model_ratio(
                &state.pg,
                model,
                okapi_store::admin::RatioAxes {
                    model: &model_ratio,
                    completion: &get("completion_ratio"),
                    cache: &get("cache_ratio"),
                    cache_write: &get("cache_write_ratio"),
                    audio: &get("audio_ratio"),
                    audio_completion: &get("audio_completion_ratio"),
                    image: &get("image_ratio"),
                },
            )
            .await?;
        }
        if let Some(price) = per_call
            && !ratio_changed
        {
            let micro = price
                .parse::<RatioFp>()
                .map_err(|_| AppError::bad_request().with_param("value"))?
                .as_scaled();
            okapi_store::admin::upsert_model_per_call(&state.pg, model, micro).await?;
        }
        applied += changes.len();
    }
    audit(
        &state,
        &actor,
        "pricing.sync_apply",
        "batch",
        json!({
            "applied": applied,
            "changes": req.changes.iter().map(|c| json!({"model": c.model, "axis": c.axis, "value": c.value})).collect::<Vec<_>>(),
        }),
    )
    .await;
    Ok(Json(json!({ "applied": applied, "published": false })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ratio_config_shape() {
        let body = json!({"success": true, "data": {
            "model_ratio": {"gpt-4o": 1.25, "o3": "10"},
            "completion_ratio": {"gpt-4o": 4},
            "create_cache_ratio": {"gpt-4o": 1.25},
            "model_price": {"dall-e-3": 0.04},
            "junk": {"gpt-4o": "x"}
        }});
        let t = parse_source(&body).unwrap();
        assert_eq!(t["gpt-4o"]["model_ratio"], "1.25");
        assert_eq!(t["gpt-4o"]["completion_ratio"], "4");
        assert_eq!(t["gpt-4o"]["cache_write_ratio"], "1.25");
        assert_eq!(t["o3"]["model_ratio"], "10");
        assert_eq!(t["dall-e-3"]["per_call_price"], "0.04");
        // 裸对象（静态 JSON 文件）同样认
        let bare = json!({"model_ratio": {"m": 0.1}});
        assert_eq!(parse_source(&bare).unwrap()["m"]["model_ratio"], "0.1");
        // success=false 不认
        assert!(parse_source(&json!({"success": false, "data": {"model_ratio": {}}})).is_none());
    }

    #[test]
    fn parses_newapi_pricing_list() {
        let body = json!({"success": true, "data": [
            {"model_name": "gpt-4o", "quota_type": 0, "model_ratio": 1.25, "completion_ratio": 4,
             "cache_ratio": 0.5, "create_cache_ratio": 1.25},
            {"model_name": "dall-e-3", "quota_type": 1, "model_price": 0.04, "model_ratio": 99},
            {"model_name": "", "model_ratio": 1},
            {"model_name": "empty"}
        ]});
        let t = parse_source(&body).unwrap();
        assert_eq!(t["gpt-4o"]["model_ratio"], "1.25");
        assert_eq!(t["gpt-4o"]["cache_ratio"], "0.5");
        assert_eq!(t["gpt-4o"]["cache_write_ratio"], "1.25");
        // 按次模型只取 model_price，不把 model_ratio 当倍率
        assert_eq!(t["dall-e-3"]["per_call_price"], "0.04");
        assert!(!t["dall-e-3"].contains_key("model_ratio"));
        assert!(!t.contains_key("") && !t.contains_key("empty"));
    }

    #[test]
    fn parses_okapi_pricing_and_micro_to_usd() {
        let body = json!({"models": [
            {"model": "gpt-4o", "mode": "ratio", "model_ratio": "1.250000", "completion_ratio": "4",
             "cache_ratio": "0.5", "cache_write_ratio": "1", "audio_ratio": "1",
             "audio_completion_ratio": "1", "image_ratio": "1", "per_call_price_micro": null},
            {"model": "dall-e-3", "mode": "per_call", "per_call_price_micro": 40000}
        ], "groups": []});
        let t = parse_source(&body).unwrap();
        assert_eq!(t["gpt-4o"]["model_ratio"], "1.250000");
        assert_eq!(t["dall-e-3"]["per_call_price"], "0.04");
        assert_eq!(micro_to_usd_literal(2_000_000), "2");
        assert_eq!(micro_to_usd_literal(123_456), "0.123456");
        assert_eq!(micro_to_usd_literal(1_500_000), "1.5");
    }

    #[test]
    fn differences_mark_same_missing_and_changed() {
        let mut local = PricingTable::new();
        local.insert(
            "gpt-4o".to_owned(),
            ModelAxes::from([
                ("model_ratio", "1.25".to_owned()),
                ("completion_ratio", "4".to_owned()),
            ]),
        );
        let mut a = PricingTable::new();
        a.insert(
            "gpt-4o".to_owned(),
            ModelAxes::from([
                ("model_ratio", "1.250000".to_owned()),
                ("completion_ratio", "5".to_owned()),
            ]),
        );
        a.insert(
            "new-model".to_owned(),
            ModelAxes::from([("model_ratio", "2".to_owned())]),
        );
        let diff = build_differences(&local, &[("A".to_owned(), a)]);
        // model_ratio 相等（1.250000 == 1.25）→ 整轴不进表
        assert!(diff["gpt-4o"].get("model_ratio").is_none());
        assert_eq!(diff["gpt-4o"]["completion_ratio"]["current"], "4");
        assert_eq!(diff["gpt-4o"]["completion_ratio"]["upstreams"]["A"], "5");
        assert!(diff["new-model"]["model_ratio"]["current"].is_null());
        assert_eq!(diff["new-model"]["model_ratio"]["upstreams"]["A"], "2");
    }

    #[test]
    fn decimals_do_not_pass_through_floats() {
        let body = json!({"model_ratio": {"m": 0.1}, "completion_ratio": {"m": "0.3"}});
        let t = parse_source(&body).unwrap();
        assert_eq!(t["m"]["model_ratio"], "0.1");
        assert_eq!(t["m"]["completion_ratio"], "0.3");
        assert_eq!(canonical("0.30"), "0.3");
        assert!(literal(&json!(-1)).is_none(), "负数不是合法倍率");
        assert!(literal(&json!(true)).is_none());
    }
}
