//! 站点公告：`settings.site_notices` 里的一个列表。
//!
//! 一条公告是 `{ id, title, body, level, popup, enabled, updated_at }`：`popup` 决定用户没看过时要不要弹窗，
//! `updated_at` 既是排序依据，也是客户端「看没看过」的锚点（改过内容再发布，看过的人会再次看到未读）。
//! 存在 settings 里而不是单开一张表：一个站同时挂着的公告也就几条，写入走现成的设置权限、审计与缓存失效。
//! 写入口整份校验（见 [`valid`]）；公开接口只给已发布的、白名单里的字段（见 [`published`]）。
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

pub const SETTING: &str = "site_notices";
const MAX_NOTICES: usize = 50;
const MAX_TITLE_CHARS: usize = 80;
const MAX_BODY_CHARS: usize = 4000;

struct Notice<'a> {
    id: &'a str,
    title: &'a str,
    body: &'a str,
    level: &'static str,
    popup: bool,
    enabled: bool,
    updated_at: &'a str,
    at: DateTime<Utc>,
}

/// 认一条公告；任何一项不合规都不认。缺省：标题与正文为空、级别 `info`、不弹窗、未发布。
/// 不认识的键不管（也不会被带到公开接口去）。
fn notice(value: &Value) -> Option<Notice<'_>> {
    let item = value.as_object()?;
    let text = |key: &str| item.get(key).map_or(Some(""), Value::as_str);
    let flag = |key: &str| item.get(key).map_or(Some(false), Value::as_bool);
    let id = item.get("id")?.as_str()?;
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let (title, body) = (text("title")?, text("body")?);
    if title.chars().count() > MAX_TITLE_CHARS || body.chars().count() > MAX_BODY_CHARS {
        return None;
    }
    let level = match text("level")? {
        "" | "info" => "info",
        "warning" => "warning",
        "critical" => "critical",
        _ => return None,
    };
    let updated_at = item.get("updated_at")?.as_str()?;
    let at = DateTime::parse_from_rfc3339(updated_at)
        .ok()?
        .with_timezone(&Utc);
    Some(Notice {
        id,
        title,
        body,
        level,
        popup: flag("popup")?,
        enabled: flag("enabled")?,
        updated_at,
        at,
    })
}

/// 写入时整份校验：是数组、不超过 50 条、每条合规、id 不重复。
/// 坏值在这里就拦下——等到公开接口读的时候才发现，站长看到的是「发布了却没人看见」。
pub fn valid(value: &Value) -> bool {
    let Some(items) = value.as_array() else {
        return false;
    };
    let mut seen = std::collections::HashSet::new();
    items.len() <= MAX_NOTICES
        && items
            .iter()
            .all(|item| notice(item).is_some_and(|n| seen.insert(n.id)))
}

/// 公开接口的内容：已发布（`enabled` 且正文非空）的公告，新的在前，只带六个字段。
/// 读的时候逐条认——库里的值即便被手改坏了一条，其余的照常显示，坏的那条不透出。
pub fn published(value: Option<&Value>) -> Vec<Value> {
    let mut notices: Vec<Notice<'_>> = value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(notice)
        .filter(|n| n.enabled && !n.body.trim().is_empty())
        .collect();
    notices.sort_by_key(|n| std::cmp::Reverse(n.at));
    notices
        .iter()
        .take(MAX_NOTICES)
        .map(|n| {
            json!({
                "id": n.id, "title": n.title.trim(), "body": n.body.trim(), "level": n.level,
                "popup": n.popup, "updated_at": n.updated_at,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, at: &str) -> Value {
        json!({"id": id, "title": " 标题 ", "body": " 正文 ", "level": "warning", "popup": true,
            "enabled": true, "updated_at": at})
    }

    #[test]
    fn a_list_is_valid_only_when_every_item_is() {
        assert!(valid(&json!([])));
        assert!(valid(&json!([
            item("a", "2026-11-01T00:00:00Z"),
            item("b-2_x", "2026-11-02T08:00:00+08:00")
        ])));
        // 缺省值：只有 id 和时间也算一条（未发布的草稿）
        assert!(valid(
            &json!([{"id": "draft", "updated_at": "2026-11-01T00:00:00Z"}])
        ));
        for bad in [
            json!(null),
            json!({}),
            json!("text"),
            json!([null]),
            json!([{"title": "没有 id", "updated_at": "2026-11-01T00:00:00Z"}]),
            json!([{"id": "", "updated_at": "2026-11-01T00:00:00Z"}]),
            json!([{"id": "有 空格", "updated_at": "2026-11-01T00:00:00Z"}]),
            json!([{"id": "x".repeat(65), "updated_at": "2026-11-01T00:00:00Z"}]),
            json!([{"id": "a"}]),
            json!([{"id": "a", "updated_at": "昨天"}]),
            json!([{"id": "a", "updated_at": 1_762_000_000}]),
            json!([{"id": "a", "updated_at": "2026-11-01T00:00:00Z", "level": "urgent"}]),
            json!([{"id": "a", "updated_at": "2026-11-01T00:00:00Z", "popup": "yes"}]),
            json!([{"id": "a", "updated_at": "2026-11-01T00:00:00Z", "enabled": 1}]),
            json!([{"id": "a", "updated_at": "2026-11-01T00:00:00Z", "title": 5}]),
            json!([{"id": "a", "updated_at": "2026-11-01T00:00:00Z", "title": "题".repeat(81)}]),
            json!([{"id": "a", "updated_at": "2026-11-01T00:00:00Z", "body": "文".repeat(4001)}]),
            json!([
                item("same", "2026-11-01T00:00:00Z"),
                item("same", "2026-11-02T00:00:00Z")
            ]),
        ] {
            assert!(!valid(&bad), "{bad}");
        }
        // 长度按字符数：80 个汉字的标题、4000 个 emoji 的正文都在上限内
        assert!(valid(
            &json!([{"id": "a", "updated_at": "2026-11-01T00:00:00Z",
            "title": "题".repeat(80), "body": "🙂".repeat(4000)}])
        ));
        // 迁移 0007 把旧的单条公告搬过来时写的形状：id 是 `n-` 加 12 位十六进制，时间带毫秒
        assert!(valid(
            &json!([{"id": "n-705ab6040d02", "title": "维护", "body": "今晚停服",
            "level": "critical", "popup": true, "enabled": true, "updated_at": "2026-11-14T08:56:35.896Z"}])
        ));
        let full: Vec<Value> = (0..51)
            .map(|n| item(&format!("n{n}"), "2026-11-01T00:00:00Z"))
            .collect();
        assert!(valid(&Value::Array(full[..50].to_vec())));
        assert!(!valid(&Value::Array(full)));
    }

    #[test]
    fn only_published_notices_go_out_newest_first_with_whitelisted_fields() {
        let stored = json!([
            item("old", "2026-11-01T00:00:00Z"),
            {"id": "draft", "title": "草稿", "body": "还没发", "enabled": false, "updated_at": "2026-11-09T00:00:00Z"},
            {"id": "empty", "body": "   ", "enabled": true, "updated_at": "2026-11-09T00:00:00Z"},
            {"id": "broken", "body": "级别写错", "enabled": true, "level": "urgent", "updated_at": "2026-11-09T00:00:00Z"},
            // 同一时刻的两种写法：+08:00 的 08:00 就是 UTC 的 00:00，比它晚一小时的排前面
            {"id": "new", "body": "新的", "enabled": true, "updated_at": "2026-11-03T09:00:00+08:00",
             "secret_field": "must-not-leak"},
            item("mid", "2026-11-03T00:00:00Z"),
        ]);
        assert_eq!(
            published(Some(&stored)),
            vec![
                json!({"id": "new", "title": "", "body": "新的", "level": "info", "popup": false,
                    "updated_at": "2026-11-03T09:00:00+08:00"}),
                json!({"id": "mid", "title": "标题", "body": "正文", "level": "warning", "popup": true,
                    "updated_at": "2026-11-03T00:00:00Z"}),
                json!({"id": "old", "title": "标题", "body": "正文", "level": "warning", "popup": true,
                    "updated_at": "2026-11-01T00:00:00Z"}),
            ]
        );
        assert_eq!(published(None), Vec::<Value>::new());
        assert_eq!(
            published(Some(&json!({"enabled": true, "body": "旧的单条形状"}))),
            Vec::<Value>::new()
        );
    }
}
