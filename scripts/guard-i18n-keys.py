#!/usr/bin/env python3
"""校验代码里 t('ns:key') 用到的键都在语言包中定义，且两种语言不缺项。

为什么要有这道闸：缺键在运行时不报错，只是把 "admin:channelsTitle" 这种原始键
直接渲染到界面上——本地跑一遍未必点到那个页面，等用户看见就晚了。
"""

import glob
import re
import sys

LOCALES = {
    'zh-CN': 'frontend/src/locales/zh-CN.ts',
    'en': 'frontend/src/locales/en.ts',
}


def flatten(path: str) -> set[str]:
    """按花括号配平切出顶层命名空间，再收集其直接子键（ns:key）。"""
    src = open(path, encoding='utf-8').read()
    keys: set[str] = set()
    for m in re.finditer(r'^  ([A-Za-z0-9_]+): \{$', src, re.M):
        ns = m.group(1)
        depth, i = 1, m.end()
        while depth > 0 and i < len(src):
            if src[i] == '{':
                depth += 1
            elif src[i] == '}':
                depth -= 1
            i += 1
        body = src[m.end():i]
        for km in re.finditer(r'^    ([A-Za-z0-9_]+):', body, re.M):
            keys.add(f'{ns}:{km.group(1)}')
    return keys


def used_keys() -> dict[str, set[str]]:
    """代码里出现的 t('ns:key') → 出现位置。"""
    out: dict[str, set[str]] = {}
    files = glob.glob('frontend/src/**/*.tsx', recursive=True)
    files += glob.glob('frontend/src/**/*.ts', recursive=True)
    for f in files:
        if '/locales/' in f:
            continue
        src = open(f, encoding='utf-8').read()
        for m in re.finditer(r"t\('([A-Za-z0-9_]+:[A-Za-z0-9_]+)'", src):
            out.setdefault(m.group(1), set()).add(f)
    return out


def main() -> int:
    defined = {name: flatten(path) for name, path in LOCALES.items()}
    used = used_keys()
    failed = False

    for lang, keys in defined.items():
        missing = sorted(k for k in used if k not in keys)
        if missing:
            failed = True
            print(f'❌ {lang} 缺少 {len(missing)} 个键（会把原始键渲染到界面）:')
            for k in missing:
                where = ', '.join(sorted(used[k])[:2])
                print(f'   {k}  ←  {where}')

    # 两种语言必须对齐，否则切语言时零星漏字
    only_zh = sorted(defined['zh-CN'] - defined['en'])
    only_en = sorted(defined['en'] - defined['zh-CN'])
    if only_zh or only_en:
        failed = True
        if only_zh:
            print(f'❌ en 缺少 {len(only_zh)} 个键: {", ".join(only_zh[:12])}')
        if only_en:
            print(f'❌ zh-CN 缺少 {len(only_en)} 个键: {", ".join(only_en[:12])}')

    if not failed:
        print(f'✅ guard-i18n-keys：{len(used)} 个引用键均已定义，中英对齐')
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
