#!/usr/bin/env python3
"""校验代码里 t('ns:key') 用到的键都在语言包中定义，且两种语言不缺项。

为什么要有这道闸：缺键在运行时不报错，只是把 "admin:channelsTitle" 这种原始键
直接渲染到界面上——本地跑一遍未必点到那个页面，等用户看见就晚了。

「已定义的键」由 scripts/locale-keys.mjs 让 Node 对前端实际装载的资源求值得到（含嵌套键
`admin:modelMeta.callUnit`），而不是用正则切 TS 源码：此前只认到每个命名空间的直接子键，
前端里 `t('admin:modelMeta.xxx')` 这类点分调用（70 多处）整体逃过校验。
"""

import glob
import json
import re
import subprocess
import sys


def defined_keys() -> dict[str, set[str]]:
    """语言 → 该语言全部键（`ns:path`，嵌套的用点连接；对象节点本身也算已定义）。"""
    try:
        run = subprocess.run(
            ['node', '--experimental-strip-types', '--disable-warning=ExperimentalWarning',
             'scripts/locale-keys.mjs'],
            capture_output=True, text=True, check=True,
        )
    except FileNotFoundError:
        raise SystemExit('❌ 找不到 node：这道闸要用 Node 对语言包求值（CI 前端 job 已装 Node 22）')
    except subprocess.CalledProcessError as error:
        raise SystemExit(f'❌ 语言包求值失败（locale-keys.mjs）：\n{error.stderr}')
    return {lang: set(keys) for lang, keys in json.loads(run.stdout).items()}


# `t('ns:key')` 与嵌套的 `t('ns:a.b.c')`。
USED = re.compile(r"(?<![A-Za-z0-9_.$])t\('([A-Za-z0-9_]+:[A-Za-z0-9_]+(?:\.[A-Za-z0-9_]+)*)'")


def used_keys() -> dict[str, set[str]]:
    """代码里出现的 t('ns:key') → 出现位置。"""
    out: dict[str, set[str]] = {}
    files = glob.glob('frontend/src/**/*.tsx', recursive=True)
    files += glob.glob('frontend/src/**/*.ts', recursive=True)
    for f in files:
        if '/locales/' in f:
            continue
        src = open(f, encoding='utf-8').read()
        # 前置断言排除 `format('HH:mm')` 这类以 t 结尾的方法名——那不是翻译调用
        for m in USED.finditer(src):
            out.setdefault(m.group(1), set()).add(f)
    return out


def main() -> int:
    defined = defined_keys()
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
        nested = sum(1 for k in used if '.' in k)
        print(f'✅ guard-i18n-keys：{len(used)} 个引用键（含 {nested} 个嵌套键）均已定义，中英对齐')
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
