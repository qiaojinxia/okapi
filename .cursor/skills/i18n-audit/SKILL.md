---
name: i18n-audit
description: 扫描前端裸文案与后端自然语言错误，验证 i18n 红线。改动 UI 文案、新增页面、或发布前使用。
paths: "frontend/**,crates/okapi-api/**"
---

# i18n 审计流程

红线依据：`IMPLEMENTATION.md` §8（后端只回 error_code；前端全 key 化；zh-CN 与 en 双套齐全）。

## 1. 前端裸文案扫描

```bash
# JSX/TS 里的裸中文（排除语言包、测试、注释密集文件）
rg -n "[\u4e00-\u9fff]" frontend/src -g '!**/locales/**' -g '!*test*' --type ts --type tsx

# 可疑英文 UI 文案：JSX 文本节点与常见属性里的自然语言（人工复核命中）
rg -n '>(\s*[A-Z][a-z]+( [a-z]+){2,})<|placeholder="[A-Z]|title="[A-Z]' frontend/src -g '!**/locales/**'
```

命中项应改为 `t('namespace:key')`；技术字符串（className、路由、日志）可豁免但需确认非用户可见。

## 2. 语言包完整性

```bash
# zh-CN 与 en 的 key 集合 diff（每个命名空间）
for ns in common console admin pricing errors; do
  diff <(jq -r 'paths(scalars)|join(".")' frontend/src/locales/zh-CN/$ns.json | sort) \
       <(jq -r 'paths(scalars)|join(".")' frontend/src/locales/en/$ns.json | sort) \
       && echo "$ns ✅" || echo "$ns ❌ key 不对齐"
done
```

## 3. 后端自然语言检查

```bash
# API 响应路径中的中文字符串字面量（注释除外，人工复核）
rg -n '"[^"]*[\u4e00-\u9fff][^"]*"' crates/ -g '!*test*' --type rust
```

- HTTP 响应体只允许 `error_code` + 结构化 params；命中自然语言的响应字符串必须改为错误码。
- 新增 error_code 确认已在 `frontend/src/locales/*/errors.json` 双语言映射。

## 4. 报告

输出三节清单（前端裸文案 / key 对齐 / 后端错误码），每节列出违规文件:行号与建议 key 名；全绿则声明通过。
