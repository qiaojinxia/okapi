import { expect, test } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { LOG_MAX_HOURS, logsWindow } from '../src/features/analytics/search'

// 纯逻辑用例，不起浏览器。

test('分析窗口带进日志页：不超过后端上限用 hours，更长的改用绝对起点，不被静默截成 90 天', () => {
  expect(logsWindow(1)).toEqual({ hours: 24 })
  expect(logsWindow(7)).toEqual({ hours: 168 })
  expect(logsWindow(90)).toEqual({ hours: 2160 })
  const now = Date.parse('2026-10-23T12:00:00Z')
  // 91 天起就超过 2160 小时：日志页拿 hours 会被后端钳成 90 天
  expect(logsWindow(91, now)).toEqual({ from: '2026-07-24T12:00:00.000Z' })
  expect(logsWindow(366, now)).toEqual({ from: '2025-10-22T12:00:00.000Z' })
})

test('LOG_MAX_HOURS 与后端 hours 钳位同值', () => {
  const rust = readFileSync(fileURLToPath(new URL('../../bins/okapi/src/console/logs.rs', import.meta.url)), 'utf8')
  const clamp = /fn hours\(&self\)[^}]*clamp\(1,\s*(\d+)\)/.exec(rust)
  expect(Number(clamp?.[1])).toBe(LOG_MAX_HOURS)
})
