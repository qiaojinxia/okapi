//! 额度预判的纯数学（无 IO，单测直接喂数）。
//!
//! 模型：每个账号 × 窗口有一个换算系数 θ（窗口百分点 / API 等价美元）。观测到用量 `u_obs` 之后
//! 本 key 又花了 C 美元，现在的用量估计 û = u_obs + θ·C；不确定度 σ 由取整、θ 误差与
//! 账号被外部使用的漂移合成。
//!
//! θ 用「相邻读数 Δu 与 ΔC 之比」在线估计：折扣后的 ΣΔu ÷ ΣΔC（过原点回归的比值形式）。
//! 比值形式对取整噪声天然稳健——相邻差分求和会抵消，误差不随配对数累积；折扣按**花费量**而不是时间算，
//! 流量稀疏时不会把旧数据衰减光；先验按「相当于几美元的观测」加权，开头数据少时不乱跳。
#![allow(clippy::cast_precision_loss)] // 秒数与 micro-USD 都远小于 2^53，换算成浮点不丢精度

use serde::{Deserialize, Serialize};

/// micro-USD 与 USD 的换算。
pub(super) const MICRO_PER_USD: f64 = 1_000_000.0;
pub(crate) const FIVE_HOURS: i64 = 18_000;
pub(crate) const SEVEN_DAYS: i64 = 604_800;

/// θ 的折扣半衰量（美元花费）：每多花这么多，旧读数的权重减半。
const HALF_LIFE_USD: f64 = 100.0;
/// 先验的权重，折成「相当于多少美元的观测」。
const PRIOR_MASS_USD: f64 = 3.0;
/// 两条读数里的重置时刻相差不超过这个秒数，仍算同一个窗口实例（上游重置时刻有秒级抖动）。
const RESET_JITTER_SECS: u64 = 120;
/// 窗口还没开（空闲）时的已用百分点上限：低于它按「没开」算。
const IDLE_EPS: f64 = 0.75;
/// Δu 可容忍的负向噪声（取整误差）；更负说明窗口重置了，这一对作废。
const MAX_NEGATIVE_STEP: f64 = 2.0;
/// 观测超过这个年龄不再外推（窗口在 20 分钟里能动很多）。
pub(super) const MAX_OBSERVATION_AGE_SECS: i64 = 1200;
/// 没有烧速基线时只信足够新的观测。
const MAX_BASELINE_FREE_AGE_SECS: i64 = 120;
/// θ 相对误差：外推出来的那部分用量按这个比例不确定。
const REL_THETA_ERROR: f64 = 0.3;
/// 外部使用 / 未建模消耗造成的漂移，单位：窗口百分点 / √小时。
const DRIFT_PER_ROOT_HOUR: f64 = 0.8;
/// 判据的分位数（单侧约 90%）。
pub(super) const Z_BLOCK: f64 = 1.28;

/// 各档订阅 100% 窗口对应的 API 等价美元：(5 小时窗, 周窗)。
/// 数据来自 2026-10-31 对线上日志的实测：Pro 5h ≈ $30–39、周 ≈ $190–260，Max 5x 的 5h ≈ $161；
/// Max 5x 的周窗与 Max 20x 按 Pro 的倍数外推，等第一批样本来了由在线标定修正。
fn plan_capacity_usd(plan: Option<&str>) -> (f64, f64) {
    let plan = plan.map(str::to_ascii_lowercase);
    match plan.as_deref() {
        Some(p) if p.contains("20x") => (660.0, 4800.0),
        Some(p) if p.contains("5x") || p.contains("max") => (161.0, 1200.0),
        _ => (33.0, 240.0),
    }
}

/// 先验 θ（窗口百分点 / API 等价美元）。短于一天的窗口按 5 小时窗等比换算，其余按周窗。
#[must_use]
pub fn prior_theta(plan: Option<&str>, window_secs: i64) -> f64 {
    let (five, week) = plan_capacity_usd(plan);
    let capacity = if window_secs <= 86_400 {
        five * window_secs.max(60) as f64 / FIVE_HOURS as f64
    } else {
        week * window_secs as f64 / SEVEN_DAYS as f64
    };
    100.0 / capacity
}

/// 一条读数：窗口用量在 `at` 时刻是多少，当时这把 key 的累计烧速计数是多少。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    pub at: i64,
    /// 窗口已用百分点（0..=100+）。取整过的轮询值已去掉平均 0.5 的向上偏差（见 [`debias`]）。
    pub used: f64,
    pub resets_at: Option<i64>,
    pub burn_micro: i64,
}

/// 轮询给的是向上取整的整数百分点：有用量时平均高估 0.5，减掉；0 就是 0。
#[must_use]
pub fn debias(used_percent: u8) -> f64 {
    if used_percent == 0 {
        0.0
    } else {
        f64::from(used_percent) - 0.5
    }
}

/// θ 的在线估计状态。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ThetaState {
    /// 折扣后的 Σ Δu（窗口百分点）。
    sy: f64,
    /// 折扣后的 Σ ΔC（美元）。
    sx: f64,
    last: Option<Reading>,
    pairs: u32,
}

/// 两条读数是不是同一个窗口实例（只有同一个窗口里的差分才能归因给这段花费）。
fn same_window(last: &Reading, new: &Reading) -> bool {
    match (last.resets_at, new.resets_at) {
        (Some(a), Some(b)) => a.abs_diff(b) <= RESET_JITTER_SECS,
        // 上一条读数时窗口还没开（空闲），这一条开了：窗口里的一切都发生在两条读数之间
        (None, Some(_)) => last.used <= IDLE_EPS,
        _ => false,
    }
}

impl ThetaState {
    /// 喂入一条新读数；与上一条配成一对（同窗口、花费为正、时间向前）才进估计。
    pub fn observe(&mut self, new: &Reading) {
        let Some(last) = self.last.replace(new.clone()) else {
            return;
        };
        let spent = (new.burn_micro - last.burn_micro) as f64 / MICRO_PER_USD;
        let step = new.used - last.used;
        if new.at < last.at || spent <= 0.0 || step < -MAX_NEGATIVE_STEP || !same_window(&last, new)
        {
            return;
        }
        let keep = 0.5_f64.powf(spent / HALF_LIFE_USD);
        self.sy = self.sy * keep + step;
        self.sx = self.sx * keep + spent;
        self.pairs = self.pairs.saturating_add(1);
    }

    /// 当前估计（窗口百分点 / API 等价美元）。夹在先验的 0.2–5 倍之间，防止异常数据把它带跑。
    #[must_use]
    pub fn theta(&self, prior: f64) -> f64 {
        let estimate = (self.sy + prior * PRIOR_MASS_USD) / (self.sx + PRIOR_MASS_USD);
        estimate.clamp(prior * 0.2, prior * 5.0)
    }

    #[must_use]
    pub fn pairs(&self) -> u32 {
        self.pairs
    }

    #[must_use]
    pub fn last(&self) -> Option<&Reading> {
        self.last.as_ref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Poll,
    Header,
}

/// 某个窗口的一次观测（来自 5 分钟轮询或响应头）。
#[derive(Clone, Debug)]
pub struct Observation {
    pub window_secs: i64,
    pub used: f64,
    pub resets_at: Option<i64>,
    pub at: i64,
    /// 观测时刻的烧速计数；旧记录没有。
    pub burn_micro: Option<i64>,
    pub source: Source,
}

/// 现在这一刻的用量估计。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Nowcast {
    /// 估计的窗口已用百分点。
    pub used: f64,
    pub sigma: f64,
    pub age_secs: i64,
}

/// 把一次观测外推到 `now`。观测里的窗口已经过了重置点、观测太旧、或缺烧速基线又不够新：
/// 返回 None（未知——调用方不得据此拦截）。
#[must_use]
pub fn nowcast(
    obs: &Observation,
    burn_now_micro: Option<i64>,
    theta: f64,
    now: i64,
) -> Option<Nowcast> {
    if obs.resets_at.is_some_and(|reset| reset <= now) {
        return None;
    }
    let age = (now - obs.at).max(0);
    if age > MAX_OBSERVATION_AGE_SECS {
        return None;
    }
    let (grown, missing) = match (obs.burn_micro, burn_now_micro) {
        (Some(base), Some(current)) => {
            (theta * (current - base).max(0) as f64 / MICRO_PER_USD, 0.0)
        }
        _ if age <= MAX_BASELINE_FREE_AGE_SECS => (0.0, 2.0),
        _ => return None,
    };
    let quantum: f64 = match obs.source {
        Source::Poll => 0.35,
        Source::Header => 0.1,
    };
    let hours = age as f64 / 3600.0;
    let variance = quantum.powi(2)
        + (REL_THETA_ERROR * grown).powi(2)
        + DRIFT_PER_ROOT_HOUR.powi(2) * hours
        + f64::powi(missing, 2);
    Some(Nowcast {
        used: (obs.used + grown).max(0.0),
        sigma: variance.sqrt(),
        age_secs: age,
    })
}

/// 一个请求会不会把窗口推过 `line`：û + z·σ + 本请求的增量 > 线。
#[must_use]
pub fn would_exceed(now: &Nowcast, delta_percent: f64, line: f64, z: f64) -> bool {
    now.used + z * now.sigma + delta_percent > line
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 够用的确定性伪随机（避免测试依赖随机源），返回 [0, 1)。
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 11) as f64 / (1_u64 << 53) as f64
        }
    }

    #[allow(clippy::cast_possible_truncation)] // 测试里的美元数都很小，换算成 micro 不会越界
    fn reading(at: i64, used: f64, resets_at: Option<i64>, burn_usd: f64) -> Reading {
        Reading {
            at,
            used,
            resets_at,
            burn_micro: (burn_usd * MICRO_PER_USD) as i64,
        }
    }

    #[test]
    fn priors_follow_the_plan_and_the_window() {
        let pro5 = prior_theta(Some("pro"), FIVE_HOURS);
        assert!((pro5 - 100.0 / 33.0).abs() < 1e-9);
        assert!(
            (prior_theta(None, FIVE_HOURS) - pro5).abs() < 1e-9,
            "不认识的档位按 Pro"
        );
        assert!((prior_theta(Some("plus"), FIVE_HOURS) - pro5).abs() < 1e-9);
        let max5 = prior_theta(Some("max_5x"), FIVE_HOURS);
        assert!((max5 - 100.0 / 161.0).abs() < 1e-9);
        assert!(prior_theta(Some("Max_20x"), FIVE_HOURS) < max5 / 3.9);
        let week = prior_theta(Some("pro"), SEVEN_DAYS);
        assert!((week - 100.0 / 240.0).abs() < 1e-9);
        assert!(week < pro5 / 7.0, "周窗的每美元消耗远小于 5 小时窗");
        // 未登记的窗口长度按比例换算，不 panic
        assert!(prior_theta(Some("pro"), 3600) > pro5);
        assert!(prior_theta(Some("pro"), 0).is_finite());
    }

    #[test]
    fn debias_removes_the_average_ceiling_error() {
        assert_eq!(debias(0), 0.0);
        assert_eq!(debias(1), 0.5);
        assert_eq!(debias(37), 36.5);
    }

    #[test]
    fn with_no_data_the_prior_is_the_estimate() {
        let state = ThetaState::default();
        assert!((state.theta(3.0) - 3.0).abs() < 1e-12);
    }

    /// 取整后的整数读数、随机的单笔花费、隔几笔才读一次：估计收敛到真值，
    /// 跨窗口的那一对（重置时刻变了）不污染它。
    #[test]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 用量夹在 0..=100 再取整成 u8
    fn theta_converges_from_quantized_readings_across_windows() {
        let truth = 3.2;
        let mut rng = Lcg(42);
        let mut state = ThetaState::default();
        let (mut burn, mut used, mut at) = (0.0_f64, 0.0_f64, 1_000_i64);
        let mut reset = 1_000 + FIVE_HOURS;
        let observe = |state: &mut ThetaState, at: i64, used: f64, reset: i64, burn: f64| {
            let ceiled = used.ceil().min(100.0) as u8;
            state.observe(&reading(at, debias(ceiled), Some(reset), burn));
        };
        observe(&mut state, at, used, reset, burn);
        for step in 0..4_000 {
            let cost = 0.05 + 2.5 * rng.next().powi(3);
            burn += cost;
            used += truth * cost;
            at += 20;
            if used >= 95.0 {
                // 窗口满了：新窗口，用量清零，重置时刻变了
                used = 0.0;
                reset += FIVE_HOURS;
            }
            if step % 4 == 0 {
                observe(&mut state, at, used, reset, burn);
            }
        }
        assert!(state.pairs() > 300, "{}", state.pairs());
        let estimate = state.theta(3.0);
        assert!(
            (estimate - truth).abs() / truth < 0.05,
            "estimate {estimate} truth {truth}"
        );
    }

    #[test]
    fn a_window_change_between_readings_is_not_attributed() {
        let mut state = ThetaState::default();
        state.observe(&reading(100, 90.0, Some(1_000), 0.0));
        // 重置时刻变了 + 用量掉回去：这一对作废，估计仍是先验
        state.observe(&reading(200, 4.0, Some(19_000), 50.0));
        assert_eq!(state.pairs(), 0);
        assert!((state.theta(3.0) - 3.0).abs() < 1e-12);
        // 上游重置时刻有秒级抖动：仍算同一个窗口
        state.observe(&reading(300, 10.0, Some(19_030), 52.0));
        assert_eq!(state.pairs(), 1);
        // 换了窗口、用量还往上涨（新窗口里一上来就有读数）：差分不属于这段花费，照样不配对
        state.observe(&reading(400, 30.0, Some(90_000), 60.0));
        assert_eq!(state.pairs(), 1, "窗口实例变了，Δu 不能算到 ΔC 头上");
    }

    #[test]
    fn an_idle_window_that_opens_between_readings_is_a_valid_pair() {
        let mut state = ThetaState::default();
        state.observe(&reading(100, 0.0, None, 10.0));
        state.observe(&reading(200, 6.4, Some(18_200), 12.0));
        assert_eq!(state.pairs(), 1);
        // sy = 6.4, sx = 2.0；先验 3.0 × 3 美元质量 → (6.4 + 9) / (2 + 3) = 3.08
        assert!((state.theta(3.0) - 3.08).abs() < 1e-9);
        // 之前窗口并不空闲却突然没有重置时刻：无法判断是不是同一个窗口
        let mut busy = ThetaState::default();
        busy.observe(&reading(100, 40.0, None, 10.0));
        busy.observe(&reading(200, 46.4, Some(18_200), 12.0));
        assert_eq!(busy.pairs(), 0);
    }

    #[test]
    fn spend_of_zero_or_a_reset_counter_carries_no_information() {
        let mut state = ThetaState::default();
        state.observe(&reading(100, 10.0, Some(5_000), 20.0));
        state.observe(&reading(200, 12.0, Some(5_000), 20.0)); // 没花钱却涨了：外部使用，不进估计
        state.observe(&reading(300, 14.0, Some(5_000), 5.0)); // 计数器回退（被清过）
        assert_eq!(state.pairs(), 0);
        state.observe(&reading(250, 14.0, Some(5_000), 9.0)); // 时间倒流
        assert_eq!(state.pairs(), 0);
    }

    /// 折扣按花费而不是时间：空闲再久，已有的估计原样不动；花得多了才逐步遗忘旧数据。
    #[test]
    fn forgetting_follows_spend_not_time() {
        let mut state = ThetaState::default();
        state.observe(&reading(0, 0.0, Some(18_000), 0.0));
        state.observe(&reading(10, 31.0, Some(18_000), 10.0)); // θ ≈ 3.1
        let before = state.theta(3.0);
        // 一个星期没有任何读数配对 → 没有变化
        assert_eq!(state.theta(3.0), before);
        // 账号换了档位：之后的 $400 全按新比例（1.0 %/$）涨
        let (mut burn, mut used, mut at) = (10.0, 31.0, 20);
        for _ in 0..40 {
            burn += 10.0;
            used += 10.0;
            at += 60;
            if used > 90.0 {
                used = 5.0;
                // 新窗口
                state.observe(&reading(at, 0.0, Some(40_000 + at), burn));
                state.observe(&reading(at + 1, 5.0, Some(40_000 + at), burn + 5.0));
                burn += 5.0;
            } else {
                state.observe(&reading(at, used, Some(18_000), burn));
            }
        }
        assert!(state.theta(3.0) < 1.6, "{}", state.theta(3.0));
    }

    #[test]
    fn estimate_stays_within_a_sane_band_of_the_prior() {
        let mut state = ThetaState::default();
        state.observe(&reading(0, 0.0, Some(18_000), 0.0));
        state.observe(&reading(10, 100.0, Some(18_000), 0.5)); // 荒唐的比值
        assert!(state.theta(3.0) <= 15.0 + 1e-9);
    }

    fn obs(
        used: f64,
        resets_at: Option<i64>,
        at: i64,
        burn: Option<i64>,
        source: Source,
    ) -> Observation {
        Observation {
            window_secs: FIVE_HOURS,
            used,
            resets_at,
            at,
            burn_micro: burn,
            source,
        }
    }

    #[test]
    fn nowcast_extrapolates_with_the_spend_since_the_observation() {
        let observed = obs(40.0, Some(10_000), 1_000, Some(5_000_000), Source::Poll);
        // 观测后又花了 $2，θ = 3 → +6 个百分点
        let now = nowcast(&observed, Some(7_000_000), 3.0, 1_060).unwrap();
        assert!((now.used - 46.0).abs() < 1e-9);
        assert_eq!(now.age_secs, 60);
        // 没花钱：原值，不确定度只剩取整与一点漂移
        let still = nowcast(&observed, Some(5_000_000), 3.0, 1_060).unwrap();
        assert!((still.used - 40.0).abs() < 1e-9);
        assert!(still.sigma < 0.5, "{}", still.sigma);
        // 外推越多、观测越旧，σ 越大
        let later = nowcast(&observed, Some(7_000_000), 3.0, 1_900).unwrap();
        assert!(later.sigma > now.sigma);
        let header = nowcast(
            &obs(40.0, Some(10_000), 1_000, Some(5_000_000), Source::Header),
            Some(5_000_000),
            3.0,
            1_060,
        )
        .unwrap();
        assert!(header.sigma < still.sigma, "响应头读数没有取整误差");
    }

    #[test]
    fn nowcast_is_unknown_when_it_cannot_be_trusted() {
        let closing = obs(40.0, Some(1_500), 1_000, Some(0), Source::Poll);
        assert!(nowcast(&closing, Some(0), 3.0, 1_499).is_some());
        assert!(
            nowcast(&closing, Some(0), 3.0, 1_500).is_none(),
            "过了重置点"
        );
        let distant = obs(40.0, Some(100_000), 1_000, Some(0), Source::Poll);
        assert!(nowcast(&distant, Some(0), 3.0, 1_000 + MAX_OBSERVATION_AGE_SECS).is_some());
        assert!(
            nowcast(&distant, Some(0), 3.0, 1_000 + MAX_OBSERVATION_AGE_SECS + 1).is_none(),
            "太旧的观测不外推"
        );
        // 缺烧速基线：足够新才用，且不确定度放大
        let fresh = obs(40.0, None, 1_000, None, Source::Poll);
        let guess = nowcast(&fresh, None, 3.0, 1_060).unwrap();
        assert!(guess.sigma > 1.9);
        assert!(nowcast(&fresh, None, 3.0, 1_000 + MAX_BASELINE_FREE_AGE_SECS + 1).is_none());
        // 当前计数读不到同理
        let based = obs(40.0, None, 1_000, Some(0), Source::Poll);
        assert!(nowcast(&based, None, 3.0, 1_600).is_none());
        // 计数器回退（被清过）：按没花钱算，不出负数
        let rolled = nowcast(
            &obs(40.0, None, 1_000, Some(9_000_000), Source::Poll),
            Some(1_000_000),
            3.0,
            1_060,
        )
        .unwrap();
        assert!((rolled.used - 40.0).abs() < 1e-9);
    }

    #[test]
    fn exceeding_uses_the_upper_band_plus_the_request_increment() {
        let now = Nowcast {
            used: 90.0,
            sigma: 2.0,
            age_secs: 30,
        };
        // 90 + 1.28×2 + 7 = 99.56
        assert!(!would_exceed(&now, 7.0, 100.0, Z_BLOCK));
        assert!(would_exceed(&now, 7.5, 100.0, Z_BLOCK));
        // 同一个请求，线更低就拦
        assert!(would_exceed(&now, 0.1, 92.0, Z_BLOCK));
    }
}
