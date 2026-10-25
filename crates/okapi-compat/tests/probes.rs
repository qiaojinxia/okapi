//! 内置厂商的声明式余额探针：URL 的拼法与官方响应的解析。
//!
//! 响应样本是 2026-10-25 对照各家文档示例写的；旧的 `channel_balance.rs` 五个解析函数的断言
//! 逐条搬到了这里，证明数据化之后行为不变。

use okapi_compat::probe::{self, Balance};
use okapi_compat::vendors;
use serde_json::{Value, json};

fn date(offset: i64) -> String {
    format!("2026-09-06{offset:+}")
}

fn urls(vendor: &str, protocol: &str) -> Vec<String> {
    let base = vendors::endpoint_base(vendor, protocol).expect("端点");
    vendors::get(vendor)
        .and_then(vendors::Vendor::balance_probe)
        .expect("探针")
        .urls(base, &date)
}

fn parse(vendor: &str, bodies: &[Value]) -> Option<Balance> {
    vendors::get(vendor)
        .and_then(vendors::Vendor::balance_probe)
        .expect("探针")
        .parse(bodies)
}

#[test]
fn every_vendor_probe_builds_the_documented_url() {
    assert_eq!(
        urls("deepseek", "openai.chat"),
        ["https://api.deepseek.com/user/balance"],
        "DeepSeek 的余额接口在站点根，不在 /v1 下"
    );
    assert_eq!(
        urls("moonshot", "openai.chat"),
        ["https://api.moonshot.cn/v1/users/me/balance"]
    );
    assert_eq!(
        urls("siliconflow", "openai.chat"),
        ["https://api.siliconflow.cn/v1/user/info"]
    );
    assert_eq!(
        urls("openrouter", "openai.chat"),
        ["https://openrouter.ai/api/v1/credits"]
    );
}

#[test]
fn the_dashboard_urls_keep_the_old_date_window() {
    let today = "2026-09-06";
    let dashboard = probe::openai_dashboard();
    let built = dashboard.urls("https://relay.example.com/v1", &|offset| {
        let day: i64 = 6 + offset;
        format!("{today}/{day}")
    });
    assert_eq!(
        built[0],
        "https://relay.example.com/v1/dashboard/billing/subscription"
    );
    assert_eq!(
        built[1],
        "https://relay.example.com/v1/dashboard/billing/usage?end_date=2026-09-06/7&start_date=2026-09-06/-94"
    );
}

#[test]
fn deepseek_reads_the_first_balance_info() {
    // 官方示例
    let documented = json!({"is_available": true, "balance_infos": [
        {"currency": "CNY", "total_balance": "110.00",
         "granted_balance": "10.00", "topped_up_balance": "100.00"}]});
    let balance = parse("deepseek", &[documented]).unwrap();
    assert_eq!(
        (balance.currency.as_str(), balance.remaining_micro),
        ("CNY", 110_000_000)
    );
    assert_eq!((balance.total_micro, balance.used_micro), (None, None));

    // 旧测试的样本
    let old = json!({"is_available": true, "balance_infos": [
        {"currency": "CNY", "total_balance": "110.00", "granted_balance": "0.00"}]});
    assert_eq!(
        parse("deepseek", &[old]).unwrap().remaining_micro,
        110_000_000
    );

    // 美元账户；不认识的币种按人民币（旧行为）
    let usd = json!({"balance_infos": [{"currency": "USD", "total_balance": "3.21"}]});
    let balance = parse("deepseek", &[usd]).unwrap();
    assert_eq!(
        (balance.currency.as_str(), balance.remaining_micro),
        ("USD", 3_210_000)
    );
    let odd = json!({"balance_infos": [{"currency": "EUR", "total_balance": "1"}]});
    assert_eq!(parse("deepseek", &[odd]).unwrap().currency, "CNY");
    let missing = json!({"balance_infos": [{"total_balance": "1"}]});
    assert_eq!(parse("deepseek", &[missing]).unwrap().currency, "CNY");

    // 形状不认
    assert!(parse("deepseek", &[json!({"unexpected": 1})]).is_none());
    assert!(parse("deepseek", &[json!({"balance_infos": []})]).is_none());
    assert!(
        parse(
            "deepseek",
            &[json!({"balance_infos": {"total_balance": "1"}})]
        )
        .is_none()
    );
}

#[test]
fn moonshot_reads_available_balance() {
    let documented = json!({"code": 0, "data": {"available_balance": 49.58894,
        "voucher_balance": 46.58893, "cash_balance": 3.00001}, "scode": "0x0", "status": true});
    let balance = parse("moonshot", &[documented]).unwrap();
    assert_eq!(
        (balance.currency.as_str(), balance.remaining_micro),
        ("CNY", 49_588_940)
    );

    let old = json!({"code": 0, "data": {"available_balance": 49.58, "voucher_balance": 0}});
    assert_eq!(
        parse("moonshot", &[old]).unwrap().remaining_micro,
        49_580_000
    );

    // 欠费时 available_balance 可以是负数
    let debt = json!({"data": {"available_balance": -1.5}});
    assert_eq!(
        parse("moonshot", &[debt]).unwrap().remaining_micro,
        -1_500_000
    );
    assert!(parse("moonshot", &[json!({"data": {}})]).is_none());
}

#[test]
fn siliconflow_reads_total_balance() {
    let documented = json!({"code": 20000, "message": "OK", "status": true, "data": {
        "id": "userid", "isAdmin": false, "balance": "0.88", "status": "normal",
        "chargeBalance": "88.00", "totalBalance": "88.88"}});
    let balance = parse("siliconflow", &[documented]).unwrap();
    assert_eq!(
        (balance.currency.as_str(), balance.remaining_micro),
        ("CNY", 88_880_000)
    );

    let old = json!({"code": 20000, "data": {"totalBalance": "12.34"}});
    assert_eq!(
        parse("siliconflow", &[old]).unwrap().remaining_micro,
        12_340_000
    );
    assert!(parse("siliconflow", &[json!({"code": 20000})]).is_none());
}

#[test]
fn openrouter_subtracts_usage_from_credits() {
    let documented = json!({"data": {"total_credits": 100.5, "total_usage": 25.75}});
    let balance = parse("openrouter", &[documented]).unwrap();
    assert_eq!(balance.currency, "USD");
    assert_eq!(
        (
            balance.total_micro,
            balance.used_micro,
            balance.remaining_micro
        ),
        (Some(100_500_000), Some(25_750_000), 74_750_000)
    );

    let old = json!({"data": {"total_credits": 10.0, "total_usage": 3.5}});
    let balance = parse("openrouter", &[old]).unwrap();
    assert_eq!(
        (balance.currency.as_str(), balance.remaining_micro),
        ("USD", 6_500_000)
    );

    // 少一个字段就是形状不认（剩余要靠两者相减）
    assert!(parse("openrouter", &[json!({"data": {"total_credits": 10.0}})]).is_none());
    assert!(parse("openrouter", &[json!({"data": {"total_usage": 1.0}})]).is_none());
}

#[test]
fn the_generic_dashboard_matches_the_old_parser() {
    let subscription = json!({"hard_limit_usd": 100.0, "has_payment_method": true});
    let usage = json!({"total_usage": 1234.5});
    let balance = probe::openai_dashboard()
        .parse(&[subscription, usage])
        .unwrap();
    assert_eq!(balance.currency, "USD");
    assert_eq!(balance.total_micro, Some(100_000_000));
    assert_eq!(balance.used_micro, Some(12_345_000));
    assert_eq!(balance.remaining_micro, 87_655_000);
}

#[test]
fn the_catalog_serializes_the_probe_back_to_the_data_it_came_from() {
    // 管理端「参考」页直接展示探针规格：序列化回来的 JSON 要能再读成同一个探针
    for vendor in vendors::all() {
        let Some(probe) = vendor.balance_probe() else {
            continue;
        };
        let json = serde_json::to_value(probe).unwrap();
        let back: probe::BalanceProbe = serde_json::from_value(json).unwrap();
        assert_eq!(&back, probe, "{}", vendor.id);
        back.check().unwrap();
    }
}
