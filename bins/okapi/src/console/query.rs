//! console 层列表通用分页参数 `PageQuery`；`Query<T>` 提取器本体在 `gateway::extract`
//! （两个角色共用，拒绝时回 `AppError` 而非 axum 的英文纯文本），这里再导出一份省得
//! 每个 console 模块都写 `crate::gateway::extract::Query`。

pub use crate::gateway::extract::Query;
use okapi_store::listing::Slice;
use serde::Deserialize;
use uuid::Uuid;

/// 管理列表和门户资源列表统一的缺省页宽（IMPLEMENTATION §11.6）。
pub const DEFAULT_LIMIT: i64 = 20;

/// 列表通用查询串。省略 `limit`（包括只传 offset）也只回 `DEFAULT_LIMIT` 条，
/// 显式页宽由 store 层封顶。响应附过滤后的 `total`；需要完整选项的调用方逐页读取。
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
    /// 所有 HTTP 列表都有界；内部配置加载可直接使用 store 的 `Slice::ALL`。
    pub fn slice(&self) -> Slice {
        Slice::new(Some(self.limit.unwrap_or(DEFAULT_LIMIT)), self.offset)
    }

    /// 大表列表的切片：不传 limit 也只回一页（`DEFAULT_LIMIT`），store 层再封顶 `MAX_PAGE`。
    pub fn bounded(&self) -> Slice {
        self.slice()
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
