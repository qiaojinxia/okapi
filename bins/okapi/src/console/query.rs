//! console 层列表通用分页参数 `PageQuery`；`Query<T>` 提取器本体在 `gateway::extract`
//! （两个角色共用，拒绝时回 `AppError` 而非 axum 的英文纯文本），这里再导出一份省得
//! 每个 console 模块都写 `crate::gateway::extract::Query`。

pub use crate::gateway::extract::Query;
use okapi_store::listing::Slice;
use serde::Deserialize;
use uuid::Uuid;

/// 大表（令牌 / 兑换码 / 用户）不传 `limit` 时的缺省页宽（IMPLEMENTATION §11.6）。
pub const DEFAULT_LIMIT: i64 = 50;

/// 列表通用查询串。配置类列表（模型 / 分组 / 池 / 套餐 / 规则 / 角色 / 渠道 / 门户令牌 / 团队）
/// 不传 `limit` 即回全量——下拉选项与全量校验这类调用方不分页；令牌 / 兑换码 / 用户天然
/// 成千上万，不传也只回 `DEFAULT_LIMIT`，别把全表推给浏览器。响应一律附 `total`。
#[derive(Deserialize)]
pub struct PageQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: i64,
    /// 关键词（令牌：name/username；渠道：name/api_base；模型：model_name/display_name；
    /// 用户：username/email）。
    #[serde(default)]
    pub q: Option<String>,
    /// 过滤：用户 id（令牌列表）。
    #[serde(default)]
    pub user_id: Option<i64>,
    /// 过滤：批次（兑换码列表）。
    #[serde(default)]
    pub batch: Option<Uuid>,
    /// 过滤：状态（兑换码 / 渠道）。
    #[serde(default)]
    pub status: Option<i16>,
    /// 过滤：协议（渠道列表）。
    #[serde(default)]
    pub provider: Option<String>,
    /// 只看未定价模型（模型列表）。
    #[serde(default)]
    pub unpriced: bool,
}

impl PageQuery {
    /// 配置类列表的切片：不传 limit 回全量。
    pub fn slice(&self) -> Slice {
        Slice::new(self.limit, self.offset)
    }

    /// 大表列表的切片：不传 limit 也只回一页（`DEFAULT_LIMIT`），store 层再封顶 `MAX_PAGE`。
    pub fn bounded(&self) -> Slice {
        Slice::new(Some(self.limit.unwrap_or(DEFAULT_LIMIT)), self.offset)
    }

    /// 去空白后的关键词；空串视为不过滤。
    pub fn keyword(&self) -> Option<&str> {
        trimmed(self.q.as_deref())
    }

    /// 协议过滤；空串视为不过滤。
    pub fn provider(&self) -> Option<&str> {
        trimmed(self.provider.as_deref())
    }
}

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}
