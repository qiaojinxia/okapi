//! 运维监控（IMPLEMENTATION §11.43）：服务器压力、中间件占用、近 24 小时趋势与告警日志。
//! 只读：会改数据的运维动作在控制台「运维操作」页（console::dlq 等），这里只看数。

// 监控读数是展示用的近似值：u64 计数转 f64 丢掉的低位、秒数取整都无关紧要
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub mod host;
pub mod logbuf;
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
pub mod probes;
pub mod sampler;
