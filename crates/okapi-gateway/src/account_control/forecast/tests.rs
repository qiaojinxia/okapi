use super::model::{Reading, ThetaState};
use super::store::{HeaderObservation, HeaderWindow};
use super::*;
use crate::scheduler::tests::cand_with_key;
use okapi_providers::account::quota::Window;
use std::collections::BTreeMap;

const AT: i64 = 1_800_000_000;

fn policy(control: &Value) -> Policy {
    Policy::parse(&json!({"account_control": control})).unwrap()
}

fn shape(cold_usd: f64, warm_usd: f64, warm_key: Option<i64>) -> RequestShape {
    RequestShape {
        prompt_tokens: 300_000,
        cold_usd,
        warm_usd,
        warm_key,
    }
}

/// 响应头读数：5 小时窗用了 `five_hour`%，周窗 10%，当时烧速计数 = `burn`。
fn header_state(five_hour: f64, at: i64, burn: i64) -> KeyState {
    KeyState {
        burn_micro: Some(burn),
        header: Some(HeaderObservation {
            at,
            burn_micro: burn,
            windows: vec![
                HeaderWindow {
                    window_secs: model::FIVE_HOURS,
                    used: five_hour,
                    resets_at: Some(at + 3_000),
                },
                HeaderWindow {
                    window_secs: model::SEVEN_DAYS,
                    used: 10.0,
                    resets_at: Some(at + 400_000),
                },
            ],
        }),
        theta: BTreeMap::new(),
        plan: None,
    }
}

fn assess_one(
    control: &Value,
    state: Option<&KeyState>,
    poll: Option<&(Snapshot, Option<i64>)>,
    shape: &RequestShape,
) -> Option<Assessment> {
    assess(
        &policy(control),
        &cand_with_key(7, 0),
        poll,
        state,
        shape,
        AT + 5,
    )
}

fn five_hour(assessment: &Assessment) -> &Value {
    assessment.entry["w"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["n"] == "five_hour")
        .unwrap()
}

#[test]
fn a_cold_start_that_overshoots_the_window_is_flagged_with_the_window_that_breaks() {
    let state = header_state(90.0, AT, 0);
    // Pro 先验 θ≈3.03 %/$：冷启动 $4.5 → +13.6 个百分点，90 + 13.6 > 100
    let cold = assess_one(&json!({}), Some(&state), None, &shape(4.5, 0.1, None)).unwrap();
    assert_eq!(cold.entry["v"], "block");
    assert_eq!(cold.entry["warm"], false);
    let detail = cold.block.as_ref().unwrap();
    assert_eq!(detail["rule"], "forecast");
    assert_eq!(detail["window"], "five_hour");
    assert_eq!(detail["window_secs"], model::FIVE_HOURS);
    assert_eq!(detail["limit_percent"], 100.0);
    assert_eq!(detail["resets_at"], AT + 3_000, "最早可能恢复的时刻");
    let window = five_hour(&cold);
    assert_eq!(window["src"], "header");
    assert_eq!(window["hit"], true);
    assert!(
        (window["d"].as_f64().unwrap() - 13.64).abs() < 0.02,
        "{window}"
    );
    // 同一个账号上的小请求或热请求不越线
    for shape in [shape(0.5, 0.1, None), shape(4.5, 0.1, Some(7))] {
        let ok = assess_one(&json!({}), Some(&state), None, &shape).unwrap();
        assert_eq!(ok.entry["v"], "ok", "{shape:?}");
        assert!(ok.block.is_none());
    }
}

#[test]
fn the_keep_line_serves_the_warm_candidate_and_never_goes_below_the_placement_line() {
    let state = header_state(88.0, AT, 0);
    let control = json!({"quota_limits": {"18000": 90}, "quota_keep_limits": {"18000": 95}});
    // 热：+0.3 → 88.4 ≤ 95；冷启动 $1 → +3 越过 90
    let warm = assess_one(&control, Some(&state), None, &shape(1.0, 0.1, Some(7))).unwrap();
    assert_eq!(five_hour(&warm)["line"], 95.0);
    assert_eq!(warm.entry["v"], "ok");
    let cold = assess_one(&control, Some(&state), None, &shape(1.0, 0.1, None)).unwrap();
    assert_eq!(five_hour(&cold)["line"], 90.0);
    assert_eq!(cold.entry["v"], "block");
    // 保留线配得比放置线还低：按放置线算，不会反过来收紧热会话
    let inverted = json!({"quota_limits": {"18000": 90}, "quota_keep_limits": {"18000": 50}});
    let warm = assess_one(&inverted, Some(&state), None, &shape(1.0, 0.1, Some(7))).unwrap();
    assert_eq!(five_hour(&warm)["line"], 90.0);
    // 只配保留线没有放置线：不封顶（线就是 100）
    let keep_only = json!({"quota_keep_limits": {"18000": 50}});
    let any = assess_one(&keep_only, Some(&state), None, &shape(1.0, 0.1, Some(7))).unwrap();
    assert_eq!(five_hour(&any)["line"], 100.0);
}

#[test]
fn observations_are_extrapolated_with_the_spend_since_and_the_freshest_one_wins() {
    // 轮询（整数、去偏后 69.5）基线烧速 0；之后又花了 $2 → +6.06
    let poll = (
        Snapshot {
            observed_at: AT,
            allowed: None,
            windows: vec![Window {
                name: "five_hour".into(),
                used_percent: 70,
                resets_at: Some(AT + 3_000),
                window_secs: Some(model::FIVE_HOURS),
            }],
            threshold_window: None,
            plan: None,
        },
        Some(0),
    );
    let state = KeyState {
        burn_micro: Some(2_000_000),
        ..KeyState::default()
    };
    let only_poll = assess_one(
        &json!({}),
        Some(&state),
        Some(&poll),
        &shape(1.0, 0.1, None),
    )
    .unwrap();
    let window = five_hour(&only_poll);
    assert_eq!(window["src"], "poll");
    assert!(
        (window["u"].as_f64().unwrap() - 75.56).abs() < 0.02,
        "{window}"
    );
    // 更新的响应头读数（55%）盖过更旧的轮询
    let newer = header_state(55.0, AT + 3, 2_000_000);
    let fresh = assess_one(
        &json!({}),
        Some(&newer),
        Some(&poll),
        &shape(1.0, 0.1, None),
    )
    .unwrap();
    assert_eq!(five_hour(&fresh)["src"], "header");
    assert_eq!(five_hour(&fresh)["obs"], 55.0);
    // 同一秒的两路读数：取响应头（它没有取整误差）
    let tied = header_state(55.0, AT, 2_000_000);
    let tie = assess_one(&json!({}), Some(&tied), Some(&poll), &shape(1.0, 0.1, None)).unwrap();
    assert_eq!(five_hour(&tie)["src"], "header");
    // 响应头读数更旧：用轮询
    let older = header_state(55.0, AT - 60, 0);
    let outdated = assess_one(
        &json!({}),
        Some(&older),
        Some(&poll),
        &shape(1.0, 0.1, None),
    )
    .unwrap();
    assert_eq!(five_hour(&outdated)["src"], "poll");
}

#[test]
fn nothing_usable_means_no_assessment() {
    let s = shape(1.0, 0.1, None);
    assert!(
        assess_one(&json!({}), None, None, &s).is_none(),
        "没有任何观测"
    );
    // 观测里的窗口已经过了重置点：未知，不评估
    let mut rolled = header_state(95.0, AT, 0);
    for window in &mut rolled.header.as_mut().unwrap().windows {
        window.resets_at = Some(AT + 1);
    }
    assert!(assess_one(&json!({}), Some(&rolled), None, &s).is_none());
    // 观测太旧
    let old = header_state(95.0, AT - 3_600, 0);
    assert!(assess_one(&json!({}), Some(&old), None, &s).is_none());
}

#[test]
fn a_learned_theta_replaces_the_prior_and_the_plan_picks_the_prior() {
    let mut learned = ThetaState::default();
    learned.observe(&Reading {
        at: AT - 600,
        used: 0.0,
        resets_at: None,
        burn_micro: 0,
    });
    learned.observe(&Reading {
        at: AT - 300,
        used: 30.0,
        resets_at: Some(AT + 3_000),
        burn_micro: 10_000_000,
    });
    let mut state = header_state(50.0, AT, 10_000_000);
    state.theta.insert(model::FIVE_HOURS, learned);
    let with_learning = assess_one(&json!({}), Some(&state), None, &shape(1.0, 0.1, None)).unwrap();
    // (30 + 3.03×3) / (10 + 3) ≈ 3.01：与先验同量级但已经不是先验本身
    let theta = five_hour(&with_learning)["th"].as_f64().unwrap();
    assert!((theta - 3.01).abs() < 0.01, "{theta}");
    // 没学到东西时按档位取先验：Max 5x 的窗口大得多，同样的花费涨得慢
    let mut max = header_state(50.0, AT, 0);
    max.plan = Some("max_5x".into());
    let assessed = assess_one(&json!({}), Some(&max), None, &shape(1.0, 0.1, None)).unwrap();
    assert!((five_hour(&assessed)["th"].as_f64().unwrap() - 0.62).abs() < 0.01);
}

#[test]
fn the_candidates_relative_cost_scales_the_increment() {
    let state = header_state(50.0, AT, 0);
    let mut half_price = cand_with_key(7, 0);
    half_price.cost_milli = 500;
    let request = shape(4.0, 0.1, None);
    let full = assess(
        &policy(&json!({})),
        &cand_with_key(7, 0),
        None,
        Some(&state),
        &request,
        AT + 5,
    )
    .unwrap();
    let half = assess(
        &policy(&json!({})),
        &half_price,
        None,
        Some(&state),
        &request,
        AT + 5,
    )
    .unwrap();
    let (full, half) = (
        five_hour(&full)["d"].as_f64().unwrap(),
        five_hour(&half)["d"].as_f64().unwrap(),
    );
    assert!((full - 2.0 * half).abs() < 0.02, "{full} {half}");
}

/// 没有响应头读数的账号走的是老的整数轮询路径：保留线在那里同样只给缓存还热的会话。
#[test]
fn the_polled_window_cap_uses_the_keep_line_only_for_warm_sessions() {
    use crate::account_control::quota_block;
    let snapshot = |used_percent| Snapshot {
        observed_at: AT,
        allowed: None,
        windows: vec![Window {
            name: "five_hour".into(),
            used_percent,
            resets_at: Some(AT + 3_000),
            window_secs: Some(model::FIVE_HOURS),
        }],
        threshold_window: None,
        plan: None,
    };
    let both = policy(&json!({"quota_limits": {"18000": 80}, "quota_keep_limits": {"18000": 95}}));
    assert!(
        quota_block(&both, &snapshot(85), AT + 1, false).is_some(),
        "冷：放置线"
    );
    assert!(
        quota_block(&both, &snapshot(85), AT + 1, true).is_none(),
        "热：保留线"
    );
    assert!(
        quota_block(&both, &snapshot(96), AT + 1, true).is_some(),
        "保留线也是线"
    );
    let detail = quota_block(&both, &snapshot(96), AT + 1, true).unwrap();
    assert_eq!(detail["limit_percent"], 95);
    // 没配保留线：热与冷一样
    let plain = policy(&json!({"quota_limits": {"18000": 80}}));
    assert!(quota_block(&plain, &snapshot(85), AT + 1, true).is_some());
    // 保留线低于放置线：按放置线，不会反过来收紧
    let inverted =
        policy(&json!({"quota_limits": {"18000": 80}, "quota_keep_limits": {"18000": 50}}));
    assert!(quota_block(&inverted, &snapshot(70), AT + 1, true).is_none());
    assert!(quota_block(&inverted, &snapshot(85), AT + 1, true).is_some());
    // 没配放置线：保留线单独不封顶
    let keep_only = policy(&json!({"quota_keep_limits": {"18000": 50}}));
    assert!(quota_block(&keep_only, &snapshot(90), AT + 1, true).is_none());
}
