// 集中式 query key 工厂（禁字符串散写）。

export const qk = {
  me: ['me'] as const,
  usage: (scope: string, days: number) => ['usage', scope, days] as const,
  keys: ['keys'] as const,
  adminChannels: ['admin', 'channels'] as const,
  reconciliation: ['admin', 'reconciliation'] as const,
  userOverview: (id: number) => ['admin', 'user-overview', id] as const,
  publicPricing: ['public-pricing'] as const,
  logs: ['logs'] as const,
  setupStatus: ['setup-status'] as const,
  oauthProviders: ['oauth-providers'] as const,
  oauthExchange: ['oauth-exchange'] as const,
}
