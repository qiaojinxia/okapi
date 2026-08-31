/// 通知事件。与后端 worker::notify 的事件名一一对应，写死在这里是刻意的：
/// 让用户从清单里勾，而不是猜字符串拼写。
export const NOTIFY_EVENTS = ['drift', 'channel_cooldown', 'balance_low'] as const



/// 事件 → 文案键的显式映射（理由同 AXIS_LABEL：动态拼键绕过文案闸门）。
export const EVENT_LABEL = {
  drift: 'admin:notifyEventDrift',
  channel_cooldown: 'admin:notifyEventCooldown',
  balance_low: 'admin:notifyEventBalanceLow',
} as const
