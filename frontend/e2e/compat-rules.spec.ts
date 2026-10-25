import { expect, test } from '@playwright/test'
import { fromPointer, readSimpleRules, sameRules, toPointer, writeSimpleRules } from '../src/features/channels/compat'
import type { CompatRule } from '../src/features/channels/compat'

// 渠道抽屉的「剥字段 / 注入字段」编辑器读写渠道层的两条约定规则。这里不起浏览器，
// 只验证两个方向的转换：能表达的往返不丢、表达不了的原样保留，其它规则的内容与顺序不动。

const strip = (...fields: string[]): CompatRule => ({
  id: 'strip-request-fields',
  do: [{ op: 'remove', paths: fields.map((f) => `/${f}`) }],
})
const inject = (fields: Record<string, unknown>): CompatRule => ({
  id: 'inject-request-fields',
  do: Object.entries(fields).map(([name, value]) => ({ op: 'set', path: `/${name}`, value })),
})
const other: CompatRule = { id: 'strict-role', do: [{ op: 'role_map', from: 'developer', to: 'system' }] }

test('指针与字段名互转：转义往返不丢，多段 / 通配 / 非法转义不当作顶层字段', () => {
  for (const field of ['user', 'a/b', 'a~b', '~1', 'x y']) {
    expect(fromPointer(toPointer(field))).toBe(field)
  }
  expect(toPointer('a/b~c')).toBe('/a~1b~0c')
  for (const bad of ['', '/', '/a/b', '/*', 'user', '/a~2', '/a~']) {
    expect(fromPointer(bad), bad).toBeNull()
  }
})

test('读：没有约定规则就是两个空编辑器；能表达的读成字段列表；含嵌套路径 / 其它操作 / 其它键的锁定编辑器', () => {
  expect(readSimpleRules([])).toEqual({ strip: [], inject: {}, stripLocked: false, injectLocked: false })
  expect(readSimpleRules([strip('user', 'logit_bias'), inject({ temperature: 0.2 }), other])).toEqual({
    strip: ['user', 'logit_bias'],
    inject: { temperature: 0.2 },
    stripLocked: false,
    injectLocked: false,
  })
  expect(readSimpleRules([{ id: 'strip-request-fields', do: [{ op: 'remove', paths: ['/metadata/x'] }] }]).stripLocked).toBe(true)
  expect(readSimpleRules([{ ...strip('user'), select: { model: ['gpt-*'] } }]).stripLocked).toBe(true)
  expect(readSimpleRules([{ ...inject({ a: 1 }), order: 5 }]).injectLocked).toBe(true)
  expect(
    readSimpleRules([{ id: 'inject-request-fields', do: [{ op: 'set', path: '/a', value: 1, mode: 'if_absent' }] }]).injectLocked,
  ).toBe(true)
  expect(readSimpleRules([{ id: 'strip-request-fields', do: [{ op: 'remove', paths: ['/a'] }, { op: 'remove', paths: ['/b'] }] }]).stripLocked).toBe(true)
})

test('写：原位替换约定规则，其它规则内容与顺序不动，空了就删掉', () => {
  const original = [other, strip('user'), inject({ a: 1 })]
  expect(writeSimpleRules(original, ['user'], { a: 1 })).toEqual(original)
  expect(sameRules(writeSimpleRules(original, ['user'], { a: 1 }), original)).toBe(true)
  expect(writeSimpleRules(original, ['user', 'seed'], { a: 1 })).toEqual([other, strip('user', 'seed'), inject({ a: 1 })])
  expect(writeSimpleRules(original, [], { a: 1 })).toEqual([other, inject({ a: 1 })])
  expect(writeSimpleRules(original, [], {})).toEqual([other])
})

test('写：原来没有的约定规则先剥后注——剥放在注前，注放在剥后，都没有就放最前', () => {
  expect(writeSimpleRules([other], ['user'], { a: 1 })).toEqual([strip('user'), inject({ a: 1 }), other])
  expect(writeSimpleRules([strip('user')], ['user'], { a: 1 })).toEqual([strip('user'), inject({ a: 1 })])
  expect(writeSimpleRules([other, inject({ a: 1 })], ['user'], { a: 1 })).toEqual([other, strip('user'), inject({ a: 1 })])
  expect(writeSimpleRules([], [], {})).toEqual([])
})

test('写：去空白、去重，剥或注 stream_options 时规则带计费确认', () => {
  expect(writeSimpleRules([], [' user ', 'user', '', 'a/b'], {})).toEqual([
    { id: 'strip-request-fields', do: [{ op: 'remove', paths: ['/user', '/a~1b'] }] },
  ])
  expect(writeSimpleRules([], ['stream_options'], {})[0]).toMatchObject({ billing_critical_ack: true })
  expect(writeSimpleRules([], [], { stream_options: null })[0]).toMatchObject({ billing_critical_ack: true })
  expect(writeSimpleRules([], ['user'], {})[0]).not.toHaveProperty('billing_critical_ack')
})

test('写：锁定的编辑器对应的规则原样保留，编辑器那一半的改动被忽略', () => {
  const nested: CompatRule = { id: 'strip-request-fields', do: [{ op: 'remove', paths: ['/metadata/x'] }] }
  expect(writeSimpleRules([nested, other], ['user'], { a: 1 })).toEqual([nested, inject({ a: 1 }), other])
  expect(writeSimpleRules([nested], [], {})).toEqual([nested])
})
