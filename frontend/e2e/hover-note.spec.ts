import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'
import { readdirSync, readFileSync } from 'node:fs'
import { join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

const keys = [42, 43].map((id) => ({
  id, name: id === 42 ? 'setup-admin' : 'production', key_prefix: `sk-fixture-${id}`, status: id === 42 ? 1 : 2,
  used_micro: 100, amount_micro: 100, requests: 1, rpm_limit: null,
  created_at: '2026-09-28T00:00:00Z', group_override: null, ip_allowlist: null,
  usage_trend: {
    days: ['2026-09-22', '2026-09-23', '2026-09-24', '2026-09-25', '2026-09-26', '2026-09-27', '2026-09-28'],
    tokens: id === 42 ? [0, 150, 20, 500, 320, 1000, 400] : [0, 0, 0, 0, 0, 0, 0],
    timezone: 'UTC',
  },
}))

async function prepare(page: Page, theme: 'light' | 'dark') {
  await page.addInitScript((theme) => {
    localStorage.setItem('okapi.key', 'note-fixture')
    localStorage.setItem('okapi.login-mode', 'account')
    localStorage.setItem('okapi.usage-scope', 'key')
    localStorage.setItem('okapi.lang', 'zh-CN')
    localStorage.setItem('okapi.theme', theme)
  }, theme)
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await page.route('**/*', (route) => {
    const request = route.request(), url = new URL(request.url())
    if (request.isNavigationRequest()) return route.fulfill({ path: fileURLToPath(new URL('../dist/index.html', import.meta.url)), contentType: 'text/html' })
    if (!/^\/(api|admin|auth)\//.test(url.pathname)) return route.continue()
    const responses: Record<string, unknown> = {
      '/api/me': { user_id: 7, key_id: 1, has_web_session: true, role: 1, permissions: [], balance_micro: 20000000, group: 'default' },
      '/api/notice': { notice: null }, '/api/pricing': { models: [], groups: [] },
      '/api/me/keys': { data: keys, total: keys.length }, '/api/me/groups': { data: [] },
    }
    return route.fulfill({ json: responses[url.pathname] ?? { data: [] } })
  })
}

/// 页面里量便签的颜色：把 CSS 颜色（含 oklch）画到 1×1 画布上读回 sRGB，再算亮度与对比度。
async function paperColors(page: Page, selector: string) {
  return page.evaluate((selector) => {
    const canvas = document.createElement('canvas')
    canvas.width = canvas.height = 1
    const ctx = canvas.getContext('2d', { willReadFrequently: true })!
    const rgb = (css: string) => {
      ctx.clearRect(0, 0, 1, 1)
      ctx.fillStyle = '#000'
      ctx.fillStyle = css.trim()
      ctx.fillRect(0, 0, 1, 1)
      const data = ctx.getImageData(0, 0, 1, 1).data
      return [data[0], data[1], data[2]]
    }
    const luminance = ([r, g, b]: number[]) => {
      const channel = (v: number) => (v / 255 <= 0.03928 ? v / 255 / 12.92 : ((v / 255 + 0.055) / 1.055) ** 2.4)
      return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
    }
    const contrast = (a: number[], b: number[]) => {
      const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x)
      return (hi + 0.05) / (lo + 0.05)
    }
    const element = document.querySelector(selector)!
    const style = getComputedStyle(element)
    const root = getComputedStyle(document.documentElement)
    const paper = rgb(style.backgroundColor), ink = rgb(style.color), card = rgb(root.getPropertyValue('--card')), page = rgb(root.getPropertyValue('--bg'))
    return {
      paper, ink,
      paperLuminance: luminance(paper),
      cardLuminance: luminance(card),
      // 黄调：红通道比蓝通道高多少；页面纸面（--bg）只有 3–14
      yellow: paper[0] - paper[2],
      pageYellow: page[0] - page[2],
      contrast: contrast(ink, paper),
      token: rgb(root.getPropertyValue('--note')),
      border: rgb(style.borderTopColor),
      borderToken: rgb(root.getPropertyValue('--note-border')),
    }
  }, selector)
}

for (const theme of ['light', 'dark'] as const) {
  test(`悬停提示是一张便签 ${theme}：纸色取自令牌、字清晰、右下折角、暖色投影`, async ({ page }) => {
    await prepare(page, theme)
    await page.goto('/portal/keys')
    await page.locator('[data-slot="key-usage-sparkline"]').first().focus()
    const tip = page.getByRole('tooltip')
    await expect(tip).toContainText('2,390')
    // 外层负责定位与投影，内层是纸
    await expect(tip).toHaveClass(/\bnote\b/)
    const body = tip.locator('.note-body')
    await expect(body).toHaveCount(1)
    await expect(body).toContainText('2,390')
    const colors = await paperColors(page, '[role="tooltip"] .note-body')
    expect(colors.paper).toEqual(colors.token)
    expect(colors.border).toEqual(colors.borderToken)
    expect(colors.contrast, '字与纸的对比度').toBeGreaterThanOrEqual(10)
    if (theme === 'light') {
      // 朴素：比页面纸面黄一档就够，不是荧光便利贴
      expect(colors.yellow - colors.pageYellow, '浅色便签比页面纸面更暖').toBeGreaterThanOrEqual(8)
      expect(colors.yellow, '但不能黄得抢眼').toBeLessThanOrEqual(30)
      expect(colors.paperLuminance, '浅色便签是亮纸').toBeGreaterThanOrEqual(0.85)
    } else {
      expect(colors.paperLuminance, '暗色便签是深色纸').toBeLessThanOrEqual(0.12)
      expect(colors.paperLuminance, '暗色便签比卡片亮一档才浮得起来').toBeGreaterThan(colors.cardLuminance)
    }
    // 右下折角：内层被切掉一个小三角，外层 ::after 画翻过来的那一角；投影用 drop-shadow 才不会被切角带走
    const shape = await tip.evaluate((root) => {
      const body = root.querySelector('.note-body')!
      const after = getComputedStyle(root, '::after')
      return {
        clip: getComputedStyle(body).clipPath,
        filter: getComputedStyle(root).filter,
        curl: { content: after.content, width: after.width, height: after.height },
        radius: getComputedStyle(body).borderTopLeftRadius,
        overflowX: root.scrollWidth <= root.clientWidth,
      }
    })
    expect(shape.clip).toContain('polygon')
    expect(shape.filter).toContain('drop-shadow')
    expect(shape.curl).toEqual({ content: '""', width: '8px', height: '8px' })
    expect(shape.radius).toBe('4px')
    expect(shape.overflowX).toBe(true)
    // 位置与可读性不受外观影响：整块在窗口内
    const box = (await tip.boundingBox())!
    expect(box.x).toBeGreaterThanOrEqual(0)
    expect(box.x + box.width).toBeLessThanOrEqual(1280)
    expect(box.y).toBeGreaterThanOrEqual(0)
  })
}

test('图表悬浮卡与 recharts 默认内容共用同一张便签：内联的白底灰边被全局样式压掉', async ({ page }) => {
  await prepare(page, 'light')
  await page.goto('/portal/keys')
  await expect(page.locator('[data-slot="key-usage-sparkline"]').first()).toBeVisible()
  // 没有真图表可悬停：按 recharts 默认的 DOM 与内联样式摆一张
  await page.evaluate(() => {
    document.body.insertAdjacentHTML('beforeend', `
      <div class="recharts-tooltip-wrapper" id="fake-wrapper" style="position:fixed;left:40px;top:300px;visibility:visible;pointer-events:none">
        <div class="recharts-default-tooltip" id="fake-content" style="margin:0;padding:10px;background-color:rgb(255,255,255);border:1px solid rgb(204,204,204);white-space:nowrap">
          <p class="recharts-tooltip-label" style="margin:0">10-18</p>
        </div>
      </div>`)
  })
  const colors = await paperColors(page, '#fake-content')
  expect(colors.paper, '白底被换成便签纸').toEqual(colors.token)
  expect(colors.border, '灰边被换成便签边').toEqual(colors.borderToken)
  const shape = await page.evaluate(() => ({
    clip: getComputedStyle(document.querySelector('#fake-content')!).clipPath,
    filter: getComputedStyle(document.querySelector('#fake-wrapper')!).filter,
    curl: getComputedStyle(document.querySelector('#fake-wrapper')!, '::after').width,
  }))
  expect(shape.clip).toContain('polygon')
  expect(shape.filter).toContain('drop-shadow')
  expect(shape.curl).toBe('8px')
})

test('悬停提示不再有深色小条：源码里没有 bg-foreground + text-background 的组合', () => {
  const root = fileURLToPath(new URL('../src/', import.meta.url))
  const files: string[] = []
  const walk = (dir: string) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name)
      if (entry.isDirectory()) walk(path)
      else if (entry.name.endsWith('.tsx')) files.push(path)
    }
  }
  walk(root)
  // 新的悬停提示请用 <Tooltip>（便签），别再手写深色小条
  const dark = files.filter((file) => /bg-foreground(?![\w/-])[^'"`]*text-background|text-background[^'"`]*bg-foreground(?![\w/-])/.test(readFileSync(file, 'utf8')))
  expect(dark.map((file) => relative(root, file).replaceAll('\\', '/'))).toEqual([])
})
