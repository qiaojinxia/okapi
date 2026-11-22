//! 热路径流式 JSON 探测共用的零 DOM 宽容反序列化辅助。
use serde::de::{Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use std::borrow::Cow;

/// 宽容字符串字段：字符串零拷贝借用（有转义时才分配），非字符串 / `null` 静默跳过为 `None`。
///
/// # Errors
/// 仅当底层 JSON 语法本身非法时返回反序列化错误。
pub fn opt_str<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Cow<'de, str>>, D::Error> {
    struct OptStr;
    impl<'de> Visitor<'de> for OptStr {
        type Value = Option<Cow<'de, str>>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("any JSON value")
        }
        fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Self::Value, E> {
            Ok(Some(Cow::Borrowed(v)))
        }
        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
            Ok(Some(Cow::Owned(v.to_owned())))
        }
        fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
            Ok(Some(Cow::Owned(v)))
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(None)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(None)
        }
        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
    }
    deserializer.deserialize_any(OptStr)
}

/// 宽容无符号整数字段：合法 `u64` 返回 `Some`，其它类型 / `null` 静默跳过为 `None`。
///
/// # Errors
/// 仅当底层 JSON 语法本身非法时返回反序列化错误。
pub fn opt_u64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    struct OptU64;
    impl<'de> Visitor<'de> for OptU64 {
        type Value = Option<u64>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("any JSON value")
        }
        fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
            Ok(u64::try_from(v).ok())
        }
        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(None)
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(None)
        }
    }
    deserializer.deserialize_any(OptU64)
}
