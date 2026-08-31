// 集中式 query key 工厂（禁字符串散写）。

export const qk = {
  me: ['me'] as const,
  usage: (scope: string, days: number) => ['usage', scope, days] as const,
  keys: ['keys'] as const,
  adminChannels: ['admin', 'channels'] as const,
  adminPricingRules: ['admin', 'pricing-rules'] as const,
  adminUsers: (q: string) => ['admin', 'users', q] as const,
  adminRoles: ['admin', 'roles'] as const,
  statsChannels: (days: number) => ['admin', 'stats', 'channels', days] as const,
  statsModels: (days: number) => ['admin', 'stats', 'models', days] as const,
  statsMargin: (days: number) => ['admin', 'stats', 'margin', days] as const,
  reconciliation: ['admin', 'reconciliation'] as const,
  userOverview: (id: number) => ['admin', 'user-overview', id] as const,
  publicPricing: ['public-pricing'] as const,
  logs: ['logs'] as const,
  setupStatus: ['setup-status'] as const,
  oauthProviders: ['oauth-providers'] as const,
  oauthExchange: ['oauth-exchange'] as const,
}
