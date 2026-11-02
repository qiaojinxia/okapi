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


def source_files() -> list[str]:
    files = glob.glob('frontend/src/**/*.tsx', recursive=True)
    files += glob.glob('frontend/src/**/*.ts', recursive=True)
    return [f for f in files if '/locales/' not in f]


def used_keys() -> dict[str, set[str]]:
    """代码里出现的 t('ns:key') → 出现位置。"""
    out: dict[str, set[str]] = {}
    for f in source_files():
        src = open(f, encoding='utf-8').read()
        # 前置断言排除 `format('HH:mm')` 这类以 t 结尾的方法名——那不是翻译调用
        for m in USED.finditer(src):
            out.setdefault(m.group(1), set()).add(f)
    return out


# 写死在代码里的键：不只 t() 的参数，映射表、props 里传的键字符串也算（AXIS_LABEL、setting-catalog 之类）。
LITERAL = re.compile(r"""['"`]([A-Za-z0-9_]+:[A-Za-z0-9_]+(?:\.[A-Za-z0-9_]+)*)['"`]""")
# 模板字符串拼出来的键：`logs:source_${x}` 记前缀 `logs:source_`。
DYNAMIC = re.compile(r'`([A-Za-z0-9_]+:[A-Za-z0-9_.]*)\$\{')
PLURAL = re.compile(r'_(?:zero|one|two|few|many|other)$')


def ancestors(key: str) -> list[str]:
    """`ns:a.b.c` → [`ns:a`, `ns:a.b`]。"""
    ns, path = key.split(':', 1)
    parts = path.split('.')
    return [f'{ns}:{".".join(parts[:i])}' for i in range(1, len(parts))]


def report_unused(keys: set[str]) -> None:
    """死键报告，只报不拦：语言包里有、代码里哪儿都引用不到的叶子键。

    注释里提到也算引用（宁可漏报）；复数形式 `x_one` / `x_other` 按 `x` 算；引用了上级对象
    （`returnObjects`）的整棵子树都算用到。整个命名空间都可能被动态拼到的（`t(`logs:${key}`)`）
    证不了死，只报个数。
    """
    literal: set[str] = set()
    prefixes: set[str] = set()
    for f in source_files():
        src = open(f, encoding='utf-8').read()
        literal.update(LITERAL.findall(src))
        # 注释里举的反例（`t(`admin:${key}`)` 这种「别这么写」）不算动态拼键
        code = '\n'.join(line for line in src.splitlines() if not line.lstrip().startswith(('//', '*', '/*')))
        prefixes.update(DYNAMIC.findall(code))
    parents = {a for k in keys for a in ancestors(k)}
    dead: list[str] = []
    maybe: dict[str, int] = {}
    for key in sorted(keys - parents):
        if key in literal or PLURAL.sub('', key) in literal or any(a in literal for a in ancestors(key)):
            continue
        ns = key.split(':', 1)[0]
        covering = [p for p in prefixes if key.startswith(p)]
        if any(p != f'{ns}:' for p in covering):
            continue
        if covering:
            maybe[ns] = maybe.get(ns, 0) + 1
        else:
            dead.append(key)
    if dead:
        print(f'ℹ️  guard-i18n-keys：{len(dead)} 个键在代码里找不到任何引用（只报不拦；确认后从两份语言包删掉）:')
        for key in dead:
            print(f'   {key}')
    if maybe:
        counts = '、'.join(f'{ns} {n}' for ns, n in sorted(maybe.items()))
        print(f'ℹ️  另有键只可能经整个命名空间的动态拼接用到，静态证不了死：{counts}')


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
    report_unused(defined['en'])
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
