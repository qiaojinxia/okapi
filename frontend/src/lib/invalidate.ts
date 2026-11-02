import type { QueryClient } from '@tanstack/react-query'
import { qk } from '@/lib/query-keys'

// 按实体写的失效：一个实体变了，连带要刷新的视图都登记在这里。调用点只说「什么变了」，
// 不再各自手抄要刷哪几个前缀——少抄一个就是一处静默的旧数据。只刷一个前缀的照旧直接调
// `invalidateQueries({ queryKey: qk.xxx })`。

type Key = readonly unknown[]

function refresh(client: QueryClient, keys: readonly Key[]): Promise<void> {
  return Promise.all(keys.map((queryKey) => client.invalidateQueries({ queryKey }))).then(() => undefined)
}

/** 出口：代理、代理组、缺省出口互相引用，渠道行上也显示生效的出口。 */
export function invalidateEgress(client: QueryClient): Promise<void> {
  return refresh(client, [qk.adminProxies, qk.adminProxyGroups, qk.egressDefault, qk.adminChannels])
}

/// 由某个设置派生出来的视图：设置改了它们也得刷新。
const DERIVED_FROM_SETTING: Record<string, readonly Key[]> = {
  site_notice: [qk.notice],
  registration_policy: [qk.registrationPolicy],
  oauth_providers: [qk.oauthProviders],
  playground_presets: [qk.playgroundPresets],
  pricing_base_per_1m_micro: [qk.adminModels],
}

/** 站点设置：单键、设置总表，以及由这个键派生的视图。 */
export function invalidateSetting(client: QueryClient, key: string): Promise<void> {
  return refresh(client, [qk.setting(key), ...settingViews(key)])
}

/**
 * 调用方已把刚保存的值 `setQueryData` 进单键缓存时用：只刷设置总表与派生视图，不再重读单键——
 * 省一次请求，也免得一个更早发出的读取把刚保存的快照盖回去。
 */
export function invalidateSettingViews(client: QueryClient, key: string): Promise<void> {
  return refresh(client, settingViews(key))
}

function settingViews(key: string): Key[] {
  return [qk.adminSettings, ...(DERIVED_FROM_SETTING[key] ?? [])]
}

/** 发布价格：管理端模型表与设置总表；公开价目、目录与试用台的模型列表都挂在 public-pricing 前缀下，一个前缀刷完。 */
export function invalidatePublishedPricing(client: QueryClient): Promise<void> {
  return refresh(client, [qk.adminModels, qk.adminSettings, qk.publicPricing])
}

/** 管理端改了某个用户（余额、角色、状态）：抽屉里的概览、用量与用户列表。 */
export function invalidateUser(client: QueryClient, id: number): Promise<void> {
  return refresh(client, [qk.userOverview(id), qk.userUsage(id), qk.adminUsersAll])
}

/** 登录会话变了：会话列表、当前身份与资料（踢下线当前设备后身份可能随之失效）。 */
export function invalidateSessions(client: QueryClient): Promise<void> {
  return Promise.all([
    client.invalidateQueries({ queryKey: qk.mySessions }),
    client.invalidateQueries({ queryKey: qk.me, exact: true }),
    client.invalidateQueries({ queryKey: qk.myProfile }),
  ]).then(() => undefined)
}
