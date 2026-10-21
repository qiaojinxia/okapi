import { expect, test } from '@playwright/test'
import { apiFetch } from '../src/lib/api'

/// 不经浏览器：把全局 fetch 换成固定应答，直接调客户端。
async function call(make: () => Response) {
  const original = globalThis.fetch
  globalThis.fetch = async () => make()
  try {
    return await apiFetch<unknown>('/x', { key: 'test-key' })
  } finally {
    globalThis.fetch = original
  }
}

test('成功但没有响应体（204 / 空 200）时返回 undefined，有 JSON 照常解析，非 JSON 的成功体仍是错误', async () => {
  expect(await call(() => new Response(null, { status: 204 }))).toBeUndefined()
  expect(await call(() => new Response('', { status: 200 }))).toBeUndefined()
  expect(await call(() => new Response('  \n', { status: 200 }))).toBeUndefined()
  expect(await call(() => new Response('{"a":1}', { status: 200 }))).toEqual({ a: 1 })
  await expect(call(() => new Response('<html>', { status: 200 }))).rejects.toBeInstanceOf(SyntaxError)
})

test('错误响应仍按错误体解析：code 与 param 原样带出，没有响应体时回落到 http_<status>', async () => {
  await expect(call(() => new Response('{"error":{"code":"bad_request","param":"x"}}', { status: 400 })))
    .rejects.toMatchObject({ status: 400, code: 'bad_request', param: 'x' })
  await expect(call(() => new Response(null, { status: 502 }))).rejects.toMatchObject({ status: 502, code: 'http_502' })
})
