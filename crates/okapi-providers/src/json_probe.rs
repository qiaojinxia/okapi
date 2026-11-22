//! 流式 SSE 热路径共用的零 DOM 轻量反序列化辅助。
use serde::de::{Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use std::borrow::Cow;

/// 记录某个 JSON 键是否出现（即便值为 `null` 也返回 `true`），且不分配任何 DOM。
#[derive(Default)]
pub(crate) struct Present(pub(crate) bool);

impl<'de> Deserialize<'de> for Present {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        IgnoredAny::deserialize(deserializer)?;
        Ok(Self(true))
    }
}

/// 宽容字符串字段：字符串零拷贝借用（有转义时才分配），非字符串 / `null` 静默跳过为 `None`。
pub(crate) fn opt_str<'de, D: Deserializer<'de>>(
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
