//! 渠道账号测试（控制台「测试输出」）：把一次真实数据面请求钉在指定渠道 / 账号上。
//!
//! 钉住走 task-local 而不是请求头：数据面的请求头由调用方任意填写，只有控制台中继
//! （已校验渠道写权限）能进入这个作用域。其余一切照常——鉴权、预扣、计费都按管理员
//! 自己的令牌走，日志里就是一次普通请求；OAuth 刷新、客户端模拟、出口代理与真实流量同一路径。

use std::future::Future;

#[derive(Clone, Debug)]
pub struct ChannelPin {
    pub channel_id: i64,
    /// None = 渠道内任一可用账号（按调度顺序）。
    pub key_id: Option<i64>,
    /// 渠道所在的池：不受管理员令牌的池链限制，测哪条就查哪条。
    pub pools: Vec<String>,
}

impl ChannelPin {
    pub(crate) fn matches(&self, cand: &okapi_store::ChannelCandidate) -> bool {
        cand.channel_id == self.channel_id
            && self.key_id.is_none_or(|key| key == cand.channel_key_id)
    }
}

tokio::task_local! { static PIN: ChannelPin; }

/// 当前请求钉住的渠道；数据面正常请求恒为 None。
pub(crate) fn current() -> Option<ChannelPin> {
    PIN.try_with(Clone::clone).ok()
}

pub async fn scope<F: Future>(pin: ChannelPin, future: F) -> F::Output {
    PIN.scope(pin, future).await
}
