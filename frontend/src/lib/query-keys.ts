// 集中式 query key 工厂（禁字符串散写）。
//
// 带参数的 key 同时暴露一个 `xxxAll` 前缀：写操作后 `invalidateQueries({ queryKey: qk.xxxAll })`
// 按前缀失效所有分页 / 过滤变体，而不是在调用点手抄 `['admin', 'keys']`——
// 手抄的字面量与工厂一旦拼写不一致，失效就静默落空。

const adminUsers = ['admin', 'users'] as const
const adminKeys = ['admin', 'keys'] as const
const adminRedemptions = ['admin', 'redemptions'] as const

export const qk = {
  me: ['me'] as const,
  meAff: ['me', 'aff'] as const,
  keys: ['keys'] as const,
  /// 新手引导用的密钥概览（有几把、是否调过）；与 `keys` 同前缀，建 / 删 key 后一起失效。
  keysSummary: ['keys', 'summary'] as const,
  /// 站点设置全表（键值卡片）；单个设置项走 `setting(key)`，两者分开失效。
  adminSettings: ['admin', 'settings'] as const,
  setting: (key: string) => ['setting', key] as const,
  adminLeaderboard: (days: number) => ['admin', 'leaderboard', days] as const,
  adminChannels: ['admin', 'channels'] as const,
  adminPricingRules: ['admin', 'pricing-rules'] as const,
  adminUsersAll: adminUsers,
  adminUsers: (q: string) => [...adminUsers, q] as const,
  adminRoles: ['admin', 'roles'] as const,
  adminPermissions: ['admin', 'permissions'] as const,
  adminPools: ['admin', 'pools'] as const,
  poolDetail: (code: string) => ['admin', 'pools', 'detail', code] as const,
  audit: (params: string) => ['admin', 'audit', params] as const,
  auditActions: ['admin', 'audit', 'actions'] as const,
  myLogins: ['me', 'logins'] as const,
  mySessions: ['me', 'sessions'] as const,
  myGroups: ['me', 'groups'] as const,
  adminGroups: ['admin', 'groups'] as const,
  adminPlans: ['admin', 'plans'] as const,
  adminModels: ['admin', 'models'] as const,
  adminKeysAll: adminKeys,
  adminKeys: (userId: number | null, q: string) => [...adminKeys, userId, q] as const,
  adminRedemptionsAll: adminRedemptions,
  adminRedemptions: (status: string) => [...adminRedemptions, status] as const,
  channelModels: (id: number) => ['admin', 'channel-models', id] as const,
  statsOverview: (days: number) => ['admin', 'stats', 'overview', days] as const,
  myBreakdown: (scope: string, days: number, range = '') => ['me', 'stats', 'breakdown', scope, days, range] as const,
  myActivity: (scope: string, year?: number) => ['me', 'stats', 'activity', scope, year] as const,
  myTeams: ['me', 'teams'] as const,
  teamUsage: (id: number) => ['team', 'usage', id] as const,
  statsChannels: (days: number) => ['admin', 'stats', 'channels', days] as const,
  statsModels: (days: number) => ['admin', 'stats', 'models', days] as const,
  statsMargin: (days: number) => ['admin', 'stats', 'margin', days] as const,
  statsRealtime: ['admin', 'stats', 'realtime'] as const,
  statsErrors: (days: number) => ['admin', 'stats', 'errors', days] as const,
  statsCashflow: (days: number) => ['admin', 'stats', 'cashflow', days] as const,
  statsModelTrend: (days: number) => ['admin', 'stats', 'model-trend', days] as const,
  statsClients: (days: number) => ['admin', 'stats', 'clients', days] as const,
  statsGroups: (days: number) => ['admin', 'stats', 'groups', days] as const,
  statsTrend: (params: string) => ['admin', 'stats', 'trend', params] as const,
  statsBreakdown: (params: string) => ['admin', 'stats', 'breakdown', params] as const,
  statsFlow: (params: string) => ['admin', 'stats', 'flow', params] as const,
  statsInventory: ['admin', 'stats', 'inventory'] as const,
  entityUsage: (kind: string, ids: string, days: number) =>
    ['admin', 'stats', 'entity-usage', kind, ids, days] as const,
  channelTimeline: (id: number, hours: number) =>
    ['admin', 'stats', 'channel-timeline', id, hours] as const,
  diagnose: ['admin', 'diagnose'] as const,
  dlq: (all: boolean) => ['admin', 'dlq', all] as const,
  adminLogs: (params: string) => ['admin', 'logs', params] as const,
  adminLogStat: (params: string) => ['admin', 'logs', 'stat', params] as const,
  reconciliation: ['admin', 'reconciliation'] as const,
  userOverview: (id: number) => ['admin', 'user-overview', id] as const,
  userUsage: (id: number) => ['admin', 'user-usage', id] as const,
  publicPricing: ['public-pricing'] as const,
  notice: ['public-notice'] as const,
  logs: (params: string) => ['logs', params] as const,
  myLedger: ['me', 'ledger'] as const,
  myOrders: ['me', 'orders'] as const,
  /// 门户在售订阅套餐 + 我的订阅（§11.28）。
  publicPlans: ['plans'] as const,
  mySubscription: ['me', 'subscription'] as const,
  userSubscription: (id: number) => ['admin', 'user-subscription', id] as const,
  setupStatus: ['setup-status'] as const,
  oauthProviders: ['oauth-providers'] as const,
  registrationPolicy: ['registration-policy'] as const,
  oauthExchange: ['oauth-exchange'] as const,
}
