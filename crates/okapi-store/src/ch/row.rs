//! `JSONEachRow` 结果行的取值。ClickHouse 缺省把 64 位整数写成字符串
//! （`output_format_json_quote_64bit_integers`），数字与字符串两种形态都要认。

use serde_json::Value;

/// 整数列；缺列、空值或解析不了都记 0。
#[must_use]
pub fn int(row: &Value, key: &str) -> i64 {
    row.get(key).map_or(0, |v| {
        v.as_str()
            .map_or_else(|| v.as_i64(), |s| s.parse::<i64>().ok())
            .unwrap_or(0)
    })
}

/// 字符串列；缺列或不是字符串记空串。
#[must_use]
pub fn text<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn integers_come_back_from_either_shape() {
        let row =
            json!({"quoted": "42", "plain": 7, "neg": "-3", "bad": "x", "null": null, "s": "hi"});
        assert_eq!(int(&row, "quoted"), 42);
        assert_eq!(int(&row, "plain"), 7);
        assert_eq!(int(&row, "neg"), -3);
        assert_eq!(int(&row, "bad"), 0);
        assert_eq!(int(&row, "null"), 0);
        assert_eq!(int(&row, "missing"), 0);
        assert_eq!(text(&row, "s"), "hi");
        assert_eq!(text(&row, "plain"), "");
        assert_eq!(text(&row, "missing"), "");
    }
}
