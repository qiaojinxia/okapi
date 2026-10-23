// 把前端运行时真正装载的语言资源展开成「命名空间:点分路径」键集，供 guard-i18n-keys.py 核对。
//
// 直接让 Node 求值，而不是用正则切 TS 源码：嵌套键（admin:modelMeta.callUnit）、`...spread`、
// withModelEditorZh 对 zh-CN 的合并，都按运行时的真实结果算。组合方式要与 frontend/src/lib/i18n.ts 保持一致。
// 用法：node --experimental-strip-types scripts/locale-keys.mjs   → stdout 一行 JSON {"zh-CN": [...], "en": [...]}
import zh from '../frontend/src/locales/zh-CN.ts'
import en from '../frontend/src/locales/en.ts'
import { withModelEditorZh } from '../frontend/src/locales/model-editor-zh.ts'

function walk(node, prefix, out) {
  for (const [key, value] of Object.entries(node)) {
    const path = prefix ? `${prefix}.${key}` : key
    out.add(path)
    if (value !== null && typeof value === 'object' && !Array.isArray(value)) walk(value, path, out)
  }
}

function keysOf(resources) {
  const out = []
  for (const [namespace, tree] of Object.entries(resources)) {
    const paths = new Set()
    walk(tree, '', paths)
    for (const path of paths) out.push(`${namespace}:${path}`)
  }
  return out.sort()
}

process.stdout.write(JSON.stringify({ 'zh-CN': keysOf(withModelEditorZh(zh)), en: keysOf(en) }))
