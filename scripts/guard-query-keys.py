#!/usr/bin/env python3
"""前端的 query key 只许从 frontend/src/lib/query-keys.ts 的 qk 工厂取。

为什么要有这道闸：写操作后按前缀失效（`invalidateQueries({ queryKey: qk.publicPricing })`），
调用点手写的字面量 key 只要和工厂差一个字、或挂在了别的前缀下，失效就静默落空——
发布价格只刷 ['public-pricing']，试用台的模型目录当时挂在 ['playground-catalog', …] 下，
于是最多旧 60 秒。分页、筛选变体也在工厂里声明，调用点不再自己拼。
"""

import pathlib
import re
import sys

ROOT = pathlib.Path('frontend/src')
FACTORY = ROOT / 'lib' / 'query-keys.ts'
# `queryKey: ['…'` 与查询客户端按 key 直接读写缓存的几个方法的字面量数组（允许跨行）。
LITERAL = re.compile(
    r"""(?:queryKey\s*:|(?:set|get)QueryData\(|getQueryState\(|ensureQueryData\(|prefetchQuery\(|fetchQuery\()"""
    r"""\s*\[\s*['"`]"""
)


def main() -> int:
    hits = []
    for path in sorted(ROOT.rglob('*')):
        if path.suffix not in {'.ts', '.tsx'} or path == FACTORY:
            continue
        text = path.read_text(encoding='utf-8')
        for m in LITERAL.finditer(text):
            line_no = text.count('\n', 0, m.start()) + 1
            line = text.splitlines()[line_no - 1].strip()
            if line.startswith(('//', '*')):
                continue
            hits.append(f'{path}:{line_no}: {line}')
    if hits:
        print('❌ guard-query-keys：发现字面量 query key（应在 lib/query-keys.ts 的 qk 里声明后再用）：')
        print('\n'.join(hits))
        return 1
    print('✅ guard-query-keys：query key 都来自 qk 工厂')
    return 0


if __name__ == '__main__':
    sys.exit(main())
