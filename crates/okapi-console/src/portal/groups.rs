//! 可自选的定价分组与邀请返利（aff）。
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::codes;
use okapi_gateway::state::AppState;
use okapi_kernel::auth::authenticate;
use okapi_kernel::error::AppError;
use serde_json::{Value, json};

/// 用户可为自己的 key 选择的分组（IMPLEMENTATION §11.14 R4，对齐 new-api UserUsableGroups）：
/// 管理员分配给他的组 ∪ 标为 `self_select` 的公开档位 ∪ 默认组（管理员给他分配了分组时不含
/// 默认组，见 `okapi_store::admin::selectable_groups`）。还没发布进价簿的组不列：选了它的 key
/// 每笔请求都会在计价处失败。
/// 价随组走：选了 vip 就按 vip 倍率计费、走 vip 池——这是产品层的"自选套餐档位"。
pub(crate) struct SelectableGroup {
    pub code: String,
    pub ratio: String,
    pub description: Option<String>,
    /// assigned | self_select | default
    pub source: &'static str,
}

pub(crate) async fn selectable_groups(
    state: &AppState,
    user_id: i64,
) -> Result<Vec<SelectableGroup>, AppError> {
    let rows = okapi_store::admin::selectable_groups(&state.pg, user_id).await?;
    // 本进程价簿可能落后（console 只订阅广播）：先追一次最新 epoch，免得把刚发布的组漏掉
    if let Err(err) = okapi_gateway::refresh_pricebook_if_newer(state).await {
        tracing::warn!(error = %err, "列可选分组前刷新价簿失败，按当前价簿过滤");
    }
    let book = state.pricebook.load();
    Ok(rows
        .into_iter()
        .filter(|r| book.has_group(&okapi_domain::GroupCode::from(r.group_code.as_str())))
        .map(|r| SelectableGroup {
            source: if r.assigned {
                "assigned"
            } else if r.self_select {
                "self_select"
            } else {
                "default"
            },
            code: r.group_code,
            ratio: r.ratio,
            description: r.description,
        })
        .collect())
}

/// 校验用户想给 key 选的分组是否在可选集合内；不在 → 403 `group_not_selectable`。
/// 不是 404：组可能存在，只是他没资格选——两种事要分开说。
pub(crate) async fn ensure_selectable(
    state: &AppState,
    user_id: i64,
    group_code: &str,
) -> Result<(), AppError> {
    let ok = selectable_groups(state, user_id)
        .await?
        .iter()
        .any(|g| g.code == group_code);
    if ok {
        Ok(())
    } else {
        Err(
            AppError::new(StatusCode::FORBIDDEN, codes::GROUP_NOT_SELECTABLE)
                .with_param(group_code.to_owned()),
        )
    }
}

/// GET /api/me/groups：我能选的分组 + 当前生效分组。
/// 门户建 key / 改 key 的档位下拉据此渲染；空的 self_select 站点只会看到自己被分配的组。
pub async fn groups(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let list = selectable_groups(&state, key.user_id).await?;
    Ok(Json(json!({
        "current": key.group_code,
        "data": list.iter().map(|g| json!({
            "code": g.code, "ratio": g.ratio, "description": g.description, "source": g.source,
        })).collect::<Vec<_>>(),
    })))
}

/// 惰性生成邀请码：8 位小写字母数字。只有唯一索引冲突才换码重试，且有次数上限——曾把一切
/// PG 错误都当成冲突、无上限地重试（读回失败或账号已删也会一直转），库故障时在这里热循环打库。
pub(super) async fn generate_aff_code(state: &AppState, user_id: i64) -> Result<String, AppError> {
    const ATTEMPTS: usize = 8;
    for _ in 0..ATTEMPTS {
        let candidate: String = {
            use rand::RngExt;
            use rand::distr::Alphanumeric;
            rand::rng()
                .sample_iter(&Alphanumeric)
                .take(8)
                .map(|c| (c as char).to_ascii_lowercase())
                .collect()
        };
        let updated = sqlx::query!(
            r#"UPDATE users SET aff_code = $2 WHERE id = $1 AND aff_code IS NULL"#,
            user_id,
            candidate
        )
        .execute(&state.pg)
        .await;
        match updated {
            Ok(r) if r.rows_affected() == 1 => return Ok(candidate),
            Ok(_) => {
                // 并发请求已经生成：读回；读不到说明账号已不在，不再重试
                return sqlx::query_scalar!(r#"SELECT aff_code FROM users WHERE id = $1"#, user_id)
                    .fetch_optional(&state.pg)
                    .await
                    .map_err(okapi_store::StoreError::from)?
                    .flatten()
                    .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
            }
            Err(error)
                if error
                    .as_database_error()
                    .is_some_and(sqlx::error::DatabaseError::is_unique_violation) => {}
            Err(error) => return Err(okapi_store::StoreError::from(error).into()),
        }
    }
    tracing::error!(user_id, "aff code generation kept colliding");
    Err(AppError::internal())
}

/// GET /api/me/aff：邀请码（惰性生成）+ 邀请人数 + 累计返利（M4 aff）。
pub async fn aff(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    let code = sqlx::query_scalar!(
        r#"SELECT aff_code FROM users WHERE id = $1 AND deleted_at IS NULL"#,
        key.user_id
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    .flatten();
    let code = if let Some(code) = code {
        code
    } else {
        generate_aff_code(&state, key.user_id).await?
    };

    let invitees = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "n!" FROM users WHERE inviter_id = $1 AND deleted_at IS NULL"#,
        key.user_id
    )
    .fetch_one(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    let mut history = okapi_store::history::read(&state.pg).await?;
    let reward_sum = sqlx::query_scalar!(
        r#"SELECT COALESCE(SUM(delta_micro), 0)::bigint AS "s!"
           FROM billing_actor_totals WHERE user_id = $1 AND actor = 'system:aff'"#,
        key.user_id
    )
    .fetch_one(&mut *history)
    .await
    .map_err(okapi_store::StoreError::from)?;

    history
        .commit()
        .await
        .map_err(okapi_store::StoreError::from)?;

    Ok(Json(json!({
        "aff_code": code,
        "invitees": invitees,
        "reward_sum_micro": reward_sum,
    })))
}
