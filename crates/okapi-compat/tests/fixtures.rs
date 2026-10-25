//! 内置厂商规则的夹具回归：每条规则至少一个「会触发」和一个「不触发」的用例，
//! 并且应用两次等于应用一次。新增厂商 = 提交数据 + 夹具，不改引擎代码。

use okapi_compat::{ContextInput, resolve, vendors};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    vendor: String,
    rule: String,
    context: ContextInput,
    input: Value,
    expect: Value,
    fires: bool,
}

fn cases() -> Vec<(String, Case)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("fixtures dir")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path).expect("read fixture");
        let parsed: Vec<Case> = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let file = path
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        out.extend(parsed.into_iter().map(|case| (file.clone(), case)));
    }
    out
}

fn run(case: &Case, body: &Value) -> (Value, bool) {
    let vendor = vendors::get(&case.vendor)
        .unwrap_or_else(|| panic!("{}: 未知厂商 {}", case.name, case.vendor));
    let effective = resolve(vec![vendor.layer()]);
    let ctx = case.context.as_context();
    let outcome = effective
        .apply_request(&ctx, body.to_string().as_bytes())
        .unwrap_or_else(|error| panic!("{}: {error}", case.name));
    let fired = outcome.report.fired.iter().any(|f| f.rule == case.rule);
    let after = outcome.body.map_or_else(
        || body.clone(),
        |bytes| serde_json::from_slice(&bytes).expect("engine output is json"),
    );
    (after, fired)
}

#[test]
fn every_fixture_produces_its_expected_body() {
    let all = cases();
    assert!(!all.is_empty());
    for (file, case) in &all {
        let (after, fired) = run(case, &case.input);
        assert_eq!(after, case.expect, "{file}: {}", case.name);
        assert_eq!(fired, case.fires, "{file}: {} 的 fires 不符", case.name);
    }
}

#[test]
fn applying_a_builtin_rule_twice_equals_applying_it_once() {
    for (file, case) in &cases() {
        let (once, _) = run(case, &case.input);
        let (twice, fired_again) = run(case, &once);
        assert_eq!(once, twice, "{file}: {}", case.name);
        assert!(!fired_again, "{file}: {} 第二次不该再改任何东西", case.name);
    }
}

#[test]
fn every_builtin_rule_has_a_firing_and_a_quiet_fixture() {
    let mut seen: BTreeMap<(String, String), (u32, u32)> = BTreeMap::new();
    for (_, case) in &cases() {
        let entry = seen
            .entry((case.vendor.clone(), case.rule.clone()))
            .or_default();
        if case.fires {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }
    }
    for vendor in vendors::all() {
        for rule in vendor.rules().rules() {
            let (firing, quiet) = seen
                .remove(&(vendor.id.clone(), rule.id.clone()))
                .unwrap_or_else(|| panic!("{}/{} 没有夹具", vendor.id, rule.id));
            assert!(firing >= 1, "{}/{} 缺会触发的夹具", vendor.id, rule.id);
            assert!(quiet >= 1, "{}/{} 缺不触发的夹具", vendor.id, rule.id);
        }
    }
    assert!(
        seen.is_empty(),
        "夹具指向不存在的规则：{:?}",
        seen.keys().collect::<Vec<_>>()
    );
}

#[test]
fn fixtures_describe_a_request_the_vendor_would_really_get() {
    for (file, case) in &cases() {
        let vendor = vendors::get(&case.vendor).expect("vendor");
        assert_eq!(
            case.context.vendor.as_deref(),
            Some(vendor.namespaced().as_str()),
            "{file}: {}",
            case.name
        );
        let host = case.context.host.as_deref().expect("host");
        assert!(
            vendor.hosts.iter().any(|h| h == host),
            "{file}: {} 的 host {host} 不属于 {}",
            case.name,
            vendor.id
        );
    }
}
