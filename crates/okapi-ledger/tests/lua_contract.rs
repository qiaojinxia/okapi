//! 钱包路径 Lua 契约直测（docs/database.md §2.2）：reserve / commit / refund / repair / drain
//! 的返回值形状、幂等性、拒绝时零写入，以及对账不变式 `avail + Σ在途 == 账本`。
//! 订阅池选池规则由 bins 的 `console_subscriptions::lua_pool_contract` 覆盖，这里只碰钱包，
//! 仅在 repair 用例里验证"另一池不动"。依赖 .env 的 OKAPI_REDIS_URL（scripts/dev-deps.sh up）。

use chrono::{DateTime, Utc};
use fred::clients::Client;
use fred::interfaces::{HashesInterface, KeysInterface};
use okapi_domain::Money;
use okapi_ledger::{BalanceLedger, CommitOutcome, LimitCaps, Pool, ReserveOutcome, ReserveRequest};
use uuid::Uuid;

const KID: i64 = 7;

struct Bed {
    ledger: BalanceLedger,
    redis: Client,
    uid: i64,
    now: DateTime<Utc>,
}

impl Bed {
    async fn new() -> Self {
        dotenvy::dotenv().ok();
        let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
        let redis = okapi_store::connect_redis(&redis_url).await.unwrap();
        // 账本键只认 uid：取随机负数，避开真实用户与并行用例
        let salt = u32::from_le_bytes(Uuid::new_v4().as_bytes()[..4].try_into().unwrap());
        let uid = -i64::from(salt) - 1;
        Self {
            ledger: BalanceLedger::new(redis.clone()),
            redis,
            uid,
            now: Utc::now(),
        }
    }

    async fn reserve_with(&self, rid: Uuid, est: i64, kid: i64, caps: LimitCaps) -> ReserveOutcome {
        self.ledger
            .reserve(
                ReserveRequest {
                    user_id: self.uid,
                    api_key_id: kid,
                    request_id: rid,
                    est: Money::from_micros(est),
                    caps,
                    est_tokens: 10,
                },
                self.now,
            )
            .await
            .unwrap()
    }

    async fn reserve(&self, rid: Uuid, est: i64) -> ReserveOutcome {
        self.reserve_with(rid, est, KID, LimitCaps::default()).await
    }

    async fn commit(&self, rid: Uuid, actual: i64) -> CommitOutcome {
        self.ledger
            .commit(self.uid, KID, rid, Money::from_micros(actual))
            .await
            .unwrap()
    }

    async fn avail(&self) -> i64 {
        self.ledger.balance(self.uid).await.unwrap().as_micros()
    }

    async fn inflight_sum(&self) -> i64 {
        self.ledger
            .list_reservations(self.uid)
            .await
            .unwrap()
            .iter()
            .map(|r| r.amount.as_micros())
            .sum()
    }

    async fn conc(&self, kid: i64) -> i64 {
        let raw: Option<String> = self
            .redis
            .get(format!("conc:{{{}}}:k:{kid}", self.uid))
            .await
            .unwrap();
        raw.map_or(0, |s| s.parse().unwrap())
    }

    async fn rpm_counter(&self, kid: i64) -> Option<String> {
        let bucket = self.now.timestamp().div_euclid(60);
        self.redis
            .get(format!("rl:{{{}}}:k:{kid}:rpm:{bucket}", self.uid))
            .await
            .unwrap()
    }
}

fn reserved_balance(out: &ReserveOutcome) -> i64 {
    match out {
        ReserveOutcome::Reserved {
            balance_after,
            pool: Pool::Wallet,
        } => balance_after.as_micros(),
        other => panic!("应从钱包预扣成功：{other:?}"),
    }
}

fn committed(out: &CommitOutcome) -> (i64, i64) {
    match out {
        CommitOutcome::Committed {
            refund_delta,
            balance_after,
            pool: Pool::Wallet,
        } => (refund_delta.as_micros(), balance_after.as_micros()),
        other => panic!("应结算成功：{other:?}"),
    }
}

/// reserve 写入的预扣字段四段齐全；commit 多退少补回钱包并释放并发槽。
#[tokio::test]
async fn reserve_then_commit_settles_both_directions() {
    let bed = Bed::new().await;
    bed.ledger
        .credit(bed.uid, Money::from_micros(10_000))
        .await
        .unwrap();

    let a = Uuid::new_v4();
    assert_eq!(reserved_balance(&bed.reserve(a, 1_000).await), 9_000);
    let rows = bed.ledger.list_reservations(bed.uid).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].request_id, a);
    assert_eq!(rows[0].amount.as_micros(), 1_000);
    assert_eq!(rows[0].api_key_id, KID);
    assert_eq!(rows[0].pool, Pool::Wallet);
    assert_eq!(
        rows[0].deadline_ms,
        bed.now.timestamp_millis() + 600_000,
        "悬置时限 = 预扣时刻 + 10min"
    );
    assert_eq!(bed.conc(KID).await, 1);

    // 少用：退 300
    assert_eq!(committed(&bed.commit(a, 700).await), (300, 9_300));
    assert!(
        bed.ledger
            .list_reservations(bed.uid)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(bed.conc(KID).await, 0);

    // 多用：补扣 500（delta 为负），钱包可以因此低于预扣后的值
    let b = Uuid::new_v4();
    assert_eq!(reserved_balance(&bed.reserve(b, 1_000).await), 8_300);
    assert_eq!(committed(&bed.commit(b, 1_500).await), (-500, 7_800));
    assert_eq!(bed.avail().await, 10_000 - 700 - 1_500);
}

/// 重复 commit → NO_RESERVATION 且不动账；commit 后 refund 释放 0；refund 后 commit 同样拒绝。
#[tokio::test]
async fn commit_and_refund_are_idempotent_in_any_order() {
    let bed = Bed::new().await;
    bed.ledger
        .credit(bed.uid, Money::from_micros(5_000))
        .await
        .unwrap();

    let a = Uuid::new_v4();
    reserved_balance(&bed.reserve(a, 1_000).await);
    assert_eq!(committed(&bed.commit(a, 400).await), (600, 4_600));
    assert!(matches!(
        bed.commit(a, 400).await,
        CommitOutcome::NoReservation
    ));
    assert_eq!(bed.avail().await, 4_600, "二次 commit 不得再动账");
    let again = bed.ledger.refund(bed.uid, KID, a).await.unwrap();
    assert_eq!(again.released.as_micros(), 0);
    assert_eq!(again.balance_after.as_micros(), 4_600);
    assert_eq!(bed.conc(KID).await, 0, "并发槽不会被重复释放成负数");

    let b = Uuid::new_v4();
    reserved_balance(&bed.reserve(b, 1_000).await);
    let first = bed.ledger.refund(bed.uid, KID, b).await.unwrap();
    assert_eq!(first.released.as_micros(), 1_000);
    assert_eq!(first.balance_after.as_micros(), 4_600);
    assert_eq!(first.pool, Pool::Wallet);
    let second = bed.ledger.refund(bed.uid, KID, b).await.unwrap();
    assert_eq!(second.released.as_micros(), 0);
    assert!(matches!(
        bed.commit(b, 1).await,
        CommitOutcome::NoReservation
    ));
    assert_eq!(bed.avail().await, 4_600);
    assert!(
        bed.ledger
            .list_reservations(bed.uid)
            .await
            .unwrap()
            .is_empty()
    );
}

/// 钱包 fail-closed：`avail == est` 放行、`avail < est` 拒绝；拒绝路径零写入
/// （无预扣字段、不占并发槽、不进限速计数）。
#[tokio::test]
async fn insufficient_is_fail_closed_and_writes_nothing() {
    let bed = Bed::new().await;
    bed.ledger
        .credit(bed.uid, Money::from_micros(1_000))
        .await
        .unwrap();

    let a = Uuid::new_v4();
    assert_eq!(reserved_balance(&bed.reserve(a, 1_000).await), 0);
    bed.ledger.refund(bed.uid, KID, a).await.unwrap();
    assert_eq!(bed.avail().await, 1_000);

    let kid = 8;
    let b = Uuid::new_v4();
    match bed.reserve_with(b, 1_001, kid, LimitCaps::default()).await {
        ReserveOutcome::Insufficient { balance } => assert_eq!(balance.as_micros(), 1_000),
        other => panic!("应余额不足：{other:?}"),
    }
    assert_eq!(bed.avail().await, 1_000);
    assert!(
        bed.ledger
            .list_reservations(bed.uid)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(bed.conc(kid).await, 0);
    assert!(bed.rpm_counter(kid).await.is_none(), "拒绝不应计入 rpm");

    // 空键（从未入账的用户）按 0 处理，同样拒绝
    let ghost = Bed::new().await;
    assert!(matches!(
        ghost.reserve(Uuid::new_v4(), 1).await,
        ReserveOutcome::Insufficient { balance } if balance.as_micros() == 0
    ));
}

/// 四个 key 级限额各自触发对应的 which；超限不产生任何写入；并发槽随 commit 释放后可再进。
#[tokio::test]
async fn rate_limits_reject_without_writes_and_release_with_commit() {
    let bed = Bed::new().await;
    bed.ledger
        .credit(bed.uid, Money::from_micros(1_000_000))
        .await
        .unwrap();

    let cases: [(&str, i64, LimitCaps); 4] = [
        (
            "rpm",
            11,
            LimitCaps {
                rpm: 1,
                ..LimitCaps::default()
            },
        ),
        (
            "tpm",
            12,
            // est_tokens 固定 10：第一笔 10 ≤ 15，第二笔 20 > 15
            LimitCaps {
                tpm: 15,
                ..LimitCaps::default()
            },
        ),
        (
            "rpd",
            13,
            LimitCaps {
                rpd: 1,
                ..LimitCaps::default()
            },
        ),
        (
            "concurrency",
            14,
            LimitCaps {
                concurrency: 1,
                ..LimitCaps::default()
            },
        ),
    ];
    for (which, kid, caps) in cases {
        let first = Uuid::new_v4();
        reserved_balance(&bed.reserve_with(first, 100, kid, caps).await);
        let before = bed.avail().await;
        let inflight_before = bed.inflight_sum().await;
        match bed.reserve_with(Uuid::new_v4(), 100, kid, caps).await {
            ReserveOutcome::RateLimited { which: got } => assert_eq!(got, which),
            other => panic!("{which} 应超限：{other:?}"),
        }
        assert_eq!(bed.avail().await, before, "{which} 超限不得动余额");
        assert_eq!(
            bed.inflight_sum().await,
            inflight_before,
            "{which} 超限不得留预扣"
        );
        assert_eq!(bed.conc(kid).await, 1, "{which} 超限不得占并发槽");
        bed.ledger
            .commit(bed.uid, kid, first, Money::from_micros(100))
            .await
            .unwrap();
        assert_eq!(bed.conc(kid).await, 0);
    }

    // 并发槽是唯一随结算释放的限额：释放后同 key 可再进
    let conc = LimitCaps {
        concurrency: 1,
        ..LimitCaps::default()
    };
    reserved_balance(&bed.reserve_with(Uuid::new_v4(), 100, 14, conc).await);
}

/// repair：`next = target − Σ同池在途`，在途字段与另一池不动；负目标不夹逼到 0。
#[tokio::test]
async fn repair_rebuilds_wallet_around_inflight_and_leaves_other_pool() {
    let bed = Bed::new().await;
    bed.ledger
        .credit(bed.uid, Money::from_micros(10_000))
        .await
        .unwrap();
    let until = bed.now.timestamp() + 3_600;
    // 先放一笔订阅池额度再把 sub_until 拨回过去，让后续预扣仍走钱包，但 sub 字段有值可供"另一池不动"断言
    bed.ledger
        .sub_set(bed.uid, Money::from_micros(300), until)
        .await
        .unwrap();
    bed.ledger.sub_touch_until(bed.uid, 0).await.unwrap();

    let a = Uuid::new_v4();
    assert_eq!(reserved_balance(&bed.reserve(a, 1_000).await), 9_000);

    // 模拟热余额丢失（FLUSHDB / 淘汰）：avail 归零，在途字段仍在
    let _: () = bed
        .redis
        .hset(format!("bal:{{{}}}", bed.uid), ("avail", 0))
        .await
        .unwrap();
    let fixed = bed
        .ledger
        .repair(bed.uid, Money::from_micros(10_000), Pool::Wallet)
        .await
        .unwrap();
    assert_eq!(fixed.before.as_micros(), 0);
    assert_eq!(fixed.after.as_micros(), 9_000);
    assert_eq!(fixed.inflight.as_micros(), 1_000);
    assert_eq!(bed.inflight_sum().await, 1_000, "在途预扣不得被抹掉");
    assert_eq!(
        bed.ledger.sub_balance(bed.uid).await.unwrap().0.as_micros(),
        300,
        "另一池不动"
    );

    // 幂等：同一 target 重跑结果相同
    let again = bed
        .ledger
        .repair(bed.uid, Money::from_micros(10_000), Pool::Wallet)
        .await
        .unwrap();
    assert_eq!(again.after.as_micros(), 9_000);

    // 在途按原路径终结后不变式闭合：10_000 − 实际 400
    assert_eq!(committed(&bed.commit(a, 400).await), (600, 9_600));

    // 账本为负说明确实欠着：不夹到 0，且随后预扣被拒
    let neg = bed
        .ledger
        .repair(bed.uid, Money::from_micros(-500), Pool::Wallet)
        .await
        .unwrap();
    assert_eq!(neg.after.as_micros(), -500);
    assert!(matches!(
        bed.reserve(Uuid::new_v4(), 1).await,
        ReserveOutcome::Insufficient { balance } if balance.as_micros() == -500
    ));
}

/// drain：只取走正的可用余额，在途预扣照常结算；非正余额不动返回 0。
#[tokio::test]
async fn drain_takes_positive_available_only_and_keeps_inflight() {
    let bed = Bed::new().await;
    bed.ledger
        .credit(bed.uid, Money::from_micros(5_000))
        .await
        .unwrap();
    let a = Uuid::new_v4();
    assert_eq!(reserved_balance(&bed.reserve(a, 2_000).await), 3_000);

    assert_eq!(bed.ledger.drain(bed.uid).await.unwrap().as_micros(), 3_000);
    assert_eq!(bed.avail().await, 0);
    assert_eq!(bed.inflight_sum().await, 2_000);
    assert_eq!(committed(&bed.commit(a, 2_000).await), (0, 0));
    assert_eq!(bed.ledger.drain(bed.uid).await.unwrap().as_micros(), 0);

    bed.ledger
        .repair(bed.uid, Money::from_micros(-100), Pool::Wallet)
        .await
        .unwrap();
    assert_eq!(bed.ledger.drain(bed.uid).await.unwrap().as_micros(), 0);
    assert_eq!(bed.avail().await, -100, "负余额不被 drain 抹平");
}

enum Op {
    Reserve(usize, i64),
    Commit(usize, i64),
    Refund(usize),
}

/// 对账不变式：任意交错的 reserve / commit / refund 序列中（含对已终结请求的重复
/// commit / refund），`avail + Σ在途 == 入账 − Σ已结算实际` 每一步都成立。
#[tokio::test]
async fn invariant_holds_across_interleaved_operations() {
    let bed = Bed::new().await;
    let credited = 50_000;
    bed.ledger
        .credit(bed.uid, Money::from_micros(credited))
        .await
        .unwrap();

    let script = [
        Op::Reserve(0, 3_000),
        Op::Reserve(1, 2_000),
        Op::Commit(0, 2_500),
        Op::Reserve(2, 7_000),
        Op::Refund(1),
        Op::Commit(2, 9_000),
        Op::Reserve(3, 1_000),
        Op::Commit(1, 999),
        Op::Refund(0),
        Op::Reserve(4, 4_000),
        Op::Commit(3, 0),
        Op::Refund(4),
        Op::Commit(4, 4_000),
    ];
    let rids: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();
    let mut settled = 0_i64;
    for op in script {
        match op {
            Op::Reserve(i, est) => {
                reserved_balance(&bed.reserve(rids[i], est).await);
            }
            Op::Commit(i, actual) => {
                if let CommitOutcome::Committed { .. } = bed.commit(rids[i], actual).await {
                    settled += actual;
                }
            }
            Op::Refund(i) => {
                bed.ledger.refund(bed.uid, KID, rids[i]).await.unwrap();
            }
        }
        assert_eq!(
            bed.avail().await + bed.inflight_sum().await,
            credited - settled,
            "不变式在每一步都必须成立"
        );
    }
    // 只有 0 / 2 / 3 三笔真正结算（2_500 + 9_000 + 0）；1、4 已退款，对它们的 commit 是 NO_RESERVATION
    assert_eq!(settled, 11_500);
    assert_eq!(bed.inflight_sum().await, 0);
    assert_eq!(bed.avail().await, credited - settled);
    assert_eq!(
        bed.conc(KID).await,
        0,
        "5 次占槽对应 5 次释放，重复终结不多减"
    );
}
