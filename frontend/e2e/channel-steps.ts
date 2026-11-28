import type { Locator, Page } from '@playwright/test'

/// 新建渠道已改为 `/admin/channels` 的独立页签（引导模式工作区，不再是右侧抽屉）。
export function channelCreatePanel(page: Page): Locator {
  return page.locator('[data-slot="channel-create-workspace"]')
}

/// 新建渠道是分步引导（接入 → 模型与调度 →〔订阅账号〕登录授权）：第一步填完才能往下走。
/// 只关心模型 / 调度的用例用它把第一步填上。`credential: null` 给订阅账号的浏览器登录（那一步没有凭证可填）。
export async function toModelsStep(
  panel: Locator,
  { name = 'draft-channel', credential = 'sk-draft' }: { name?: string | null; credential?: string | null } = {},
) {
  if (name !== null) await panel.locator('#d-name').fill(name)
  if (credential !== null) await panel.locator('#d-cred').fill(credential)
  await nextStep(panel)
}

export async function nextStep(panel: Locator) {
  await panel.getByRole('button', { name: '下一步', exact: true }).click()
}

export async function previousStep(panel: Locator) {
  await panel.getByRole('button', { name: '上一步', exact: true }).click()
}

/// 点步骤条直接跳到某一步（只能跳到已经够得着的步骤：它前面的都填完了）。
export async function gotoStep(panel: Locator, label: '接入' | '模型与调度' | '登录授权') {
  await panel.getByRole('navigation', { name: '新建渠道的步骤' }).getByRole('button', { name: label }).click()
}
