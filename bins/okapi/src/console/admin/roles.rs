//! 管理角色：创建、分配与权限点校验。
use super::super::query::{PageQuery, Query};
use super::{audit, guard, guard_super_admin};
use crate::gateway::error::AppError;
use crate::gateway::extract::Json as ExtractJson;
use crate::gateway::extract::Path;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use serde::Deserialize;
use serde_json::{Value, json};

// ---- 角色管理（仅 super_admin）----

#[derive(Deserialize)]
pub struct CreateRoleReq {
    pub role_code: String,
    pub display_name: String,
    /// 权限点集合（okapi_api::permissions 常量；`*` 为全权）。
    pub permissions: Vec<String>,
}

pub async fn create_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<CreateRoleReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard_super_admin(&state, &headers).await?;
    if req
        .permissions
        .iter()
        .any(|permission| !valid_role_permission(permission))
    {
        return Err(AppError::bad_request().with_param("permissions"));
    }
    let permissions = json!(req.permissions);
    let role_id = okapi_store::admin::create_admin_role(
        &state.pg,
        &req.role_code,
        &req.display_name,
        &permissions,
    )
    .await?;
    // upsert 可能改了已绑定管理员的权限集合：全量失效鉴权缓存（与改用户角色同一动作）
    state.sched.auth_flush().await;
    audit(
        &state,
        &actor,
        "role.upsert",
        &req.role_code,
        json!({ "permissions": req.permissions }),
    )
    .await;
    Ok(Json(json!({ "admin_role_id": role_id })))
}

pub(super) fn valid_role_permission(permission: &str) -> bool {
    let base = permission
        .strip_suffix(".own")
        .or_else(|| permission.strip_suffix(".all"));
    permission == "*"
        || permissions::ALL.contains(&permission)
        || base.is_some_and(|base| permissions::ALL.contains(&base))
}

#[derive(Deserialize)]
pub struct AssignRoleReq {
    /// 平台角色（1/10/100）；None = 不变。
    #[serde(default)]
    pub role: Option<i16>,
    /// 自定义子角色 id；None = 解绑（admin 回到默认全权）。
    #[serde(default)]
    pub admin_role_id: Option<i64>,
}

pub async fn assign_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    ExtractJson(req): ExtractJson<AssignRoleReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard_super_admin(&state, &headers).await?;
    if let Some(role) = req.role
        && !matches!(role, 1 | 10 | 100)
    {
        return Err(AppError::bad_request().with_param("role"));
    }
    // admin_role_id 有外键：不存在的子角色会让 UPDATE 撞约束变成 500，先挡成 404
    if let Some(role_id) = req.admin_role_id {
        let known: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM admin_roles WHERE id = $1)")
                .bind(role_id)
                .fetch_one(&state.pg)
                .await
                .map_err(okapi_store::StoreError::from)?;
        if !known {
            return Err(
                AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND).with_param("admin_role_id")
            );
        }
    }
    let hit = okapi_store::admin::assign_user_role(&state.pg, user_id, req.role, req.admin_role_id)
        .await?;
    if !hit {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    }
    state.sched.auth_flush().await;
    audit(
        &state,
        &actor,
        "role.assign",
        &user_id.to_string(),
        json!({ "role": req.role, "admin_role_id": req.admin_role_id }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

/// GET /admin/roles：自定义管理角色列表，默认每页 20 条并返回总数。
pub async fn list_roles(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::ROLE_MANAGE).await?;
    let slice = q.slice();
    let (rows, total) = tokio::try_join!(
        sqlx::query!(
            r#"SELECT id, role_code, display_name, permissions FROM admin_roles
           ORDER BY id LIMIT $1 OFFSET $2"#,
            slice.limit,
            slice.offset
        )
        .fetch_all(&state.pg),
        okapi_store::listing::count_unless_all(
            slice,
            sqlx::query_scalar!(r#"SELECT COUNT(*)::bigint AS "c!" FROM admin_roles"#)
                .fetch_one(&state.pg)
        ),
    )
    .map_err(okapi_store::StoreError::from)?;
    let total = total.unwrap_or_else(|| okapi_store::listing::len_as_total(rows.len()));
    let data: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "role_code": r.role_code,
                "display_name": r.display_name,
                "permissions": r.permissions,
            })
        })
        .collect();
    Ok(Json(json!({ "data": data, "total": total })))
}

#[cfg(test)]
mod role_permission_tests {
    use super::*;
    #[test]
    fn role_permissions_reject_typos_and_unknown_resources() {
        for valid in ["*", "billing.read.own", "channel.write.all", "mcp.write"] {
            assert!(valid_role_permission(valid), "{valid}");
        }
        for invalid in [
            "billing.reed",
            "channel.unknown.own",
            "*.own",
            "billing.read.own.all",
            "",
        ] {
            assert!(!valid_role_permission(invalid), "{invalid}");
        }
    }
}
