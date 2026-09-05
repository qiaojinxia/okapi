import { expect, test } from '@playwright/test'
import { spawnSync } from 'node:child_process'
import { buildRequestExample, defaultApiBase, normalizeApiBase, requestTemplates } from '../src/features/public-pricing/request-examples'

const fakeEnv = { ...process.env, OKAPI_API_KEY: 'request-example-test' }
const badText = `single' double" $HOME $(printf INJECTED) \u0060printf INJECTED\u0060 \\ newline\n你好\nEOF`

test('网关地址推导、前缀和版本路径保留，拒绝夹带凭证的 URL', () => {
  expect(defaultApiBase('http://127.0.0.1:8081')).toBe('http://127.0.0.1:8080/v1')
  expect(defaultApiBase('http://localhost:5173')).toBe('http://localhost:8080/v1')
  expect(defaultApiBase('http://[::1]:8081')).toBe('http://[::1]:8080/v1')
  expect(defaultApiBase('https://console.example.com')).toBe('https://console.example.com/v1')
  expect(defaultApiBase('https://console.example.com', 'https://gateway.example.com/api/v1')).toBe('https://gateway.example.com/api/v1')
  expect(normalizeApiBase('https://api.example.com/proxy/v1///')).toBe('https://api.example.com/proxy/v1')
  expect(normalizeApiBase('https://api.example.com/proxy')).toBe('https://api.example.com/proxy/v1')
  for (const invalid of ['javascript:alert(1)', 'ftp://example.com', 'not-url', 'https://user:key@api.example.com', 'https://api.example.com?key=secret', 'https://api.example.com/#secret']) expect(normalizeApiBase(invalid)).toBeNull()
})

test('生成的 Shell 在 sh、bash、zsh 中保持模型名和提示词原样，不执行其中的语句', () => {
  const example = buildRequestExample('https://api.example.com/v1', 'chat', badText, badText, true)!
  // curl 替身只记录参数；测试不发起网络请求，也不会调用真实模型。
  const stub = `curl() { python3 -c 'import json, sys; print(json.dumps(sys.argv[1:]))' "$@"; }\n`
  for (const shell of ['sh', 'bash', 'zsh']) {
    const result = spawnSync(shell, ['-c', stub + example.curl], { env: fakeEnv, encoding: 'utf-8' })
    expect(result.status, result.stderr).toBe(0)
    const args = JSON.parse(result.stdout) as string[]
    expect(args).toContain('Authorization: Bearer request-example-test')
    expect(args).toContain(example.url)
    expect(JSON.parse(args[args.indexOf('--data-raw') + 1])).toEqual({ model: badText, messages: [{ role: 'user', content: badText }], stream: true })
  }
  const missingKey = spawnSync('sh', ['-c', stub + example.curl], { env: { ...fakeEnv, OKAPI_API_KEY: '' }, encoding: 'utf-8' })
  expect(missingKey.status).not.toBe(0)
  expect(missingKey.stdout).toBe('')
})

test('Python 与 JavaScript 实际解释执行后的请求体和 URL 正确，仅使用本地替身', () => {
  const example = buildRequestExample('https://api.example.com/prefix/v1', 'messages', badText, badText, false)!
  const pythonHarness = `import urllib.request, json, sys
class Response:
    def __enter__(self): return self
    def __exit__(self, *args): pass
    def read(self): return b'{}'
def capture(request):
    print(json.dumps({'url': request.full_url, 'body': json.loads(request.data), 'auth': request.get_header('Authorization')}))
    return Response()
urllib.request.urlopen = capture
exec(sys.stdin.read())
`
  const python = spawnSync('python3', ['-c', pythonHarness], { input: example.python, env: fakeEnv, encoding: 'utf-8' })
  expect(python.status, python.stderr).toBe(0)
  const jsHarness = `globalThis.fetch = async (url, init) => { console.log(JSON.stringify({url, body:JSON.parse(init.body),auth:init.headers.Authorization})); return {ok:true,text:async()=> '{}'} }\n`
  const js = spawnSync(process.execPath, ['--input-type=module'], { input: jsHarness + example.javascript, env: fakeEnv, encoding: 'utf-8' })
  expect(js.status, js.stderr).toBe(0)
  for (const output of [python.stdout, js.stdout]) expect(JSON.parse(output.split('\n')[0])).toEqual({ url: example.url, body: JSON.parse(example.body), auth: 'Bearer request-example-test' })
})

test('每种模板符合本地路由形状，非流式接口不携带 stream，语音下载到文件', () => {
  for (const template of requestTemplates) {
    const example = buildRequestExample('https://api.example.com', template.id, 'custom-model', 'hello', true)!
    const payload = JSON.parse(example.body)
    expect(example.url).toBe(`https://api.example.com/v1${template.path}`)
    expect(payload.model).toBe('custom-model')
    if (template.stream) expect(payload.stream).toBe(true)
    else expect(payload).not.toHaveProperty('stream')
    const syntax = spawnSync('python3', ['-c', 'import ast,sys; ast.parse(sys.stdin.read())'], { input: example.python, encoding: 'utf-8' })
    expect(syntax.status, syntax.stderr).toBe(0)
    const js = spawnSync(process.execPath, ['--input-type=module', '--check'], { input: example.javascript, encoding: 'utf-8' })
    expect(js.status, js.stderr).toBe(0)
    if (template.id === 'speech') expect(example.curl).toContain("--output 'speech.mp3'")
    if (template.id === 'messages') expect(payload.max_tokens).toBe(1024)
  }
})
