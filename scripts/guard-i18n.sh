#!/usr/bin/env bash
# i18n 红线（IMPLEMENTATION §8）：前端组件禁裸中文文案，一律 t('ns:key')。
# 中文只允许出现在语言包（frontend/src/locales/）与注释里。
set -euo pipefail
cd "$(dirname "$0")/.."
python3 - <<'PY'
import pathlib, re, sys

cjk = re.compile(r'[\u4e00-\u9fa5]')
# 行注释：只认前面是空白或行首的 //，避免把 'https://…' 字面量后半段当注释吞掉
line_comment = re.compile(r'(^|\s)//.*$')


def strip_comments(lines):
    """逐行去掉注释后返回 (行号, 剩余代码)。

    块注释 /* … */ 可跨行（JSX 里写成 {/* … */}，续行没有任何前缀），必须带状态扫描；
    只按"行首是否 //、*"放行的老规则会把多行注释的续行当成裸文案误报。
    """
    in_block = False
    for lineno, line in enumerate(lines, 1):
        out, i = [], 0
        while i < len(line):
            if in_block:
                j = line.find('*/', i)
                if j == -1:
                    i = len(line)
                    break
                in_block, i = False, j + 2
            else:
                j = line.find('/*', i)
                if j == -1:
                    out.append(line[i:])
                    break
                out.append(line[i:j])
                in_block, i = True, j + 2
        code = line_comment.sub('', ''.join(out))
        yield lineno, code


violations = []
root = pathlib.Path('frontend/src')
for path in sorted(root.rglob('*')):
    if path.suffix not in {'.ts', '.tsx'}:
        continue
    if 'locales' in path.parts:
        continue
    for lineno, code in strip_comments(path.read_text().splitlines()):
        if cjk.search(code):
            violations.append(f'{path}:{lineno}: {code.strip()}')

if violations:
    print('❌ guard-i18n：组件内发现裸中文文案（应走 locales 语言包）：')
    print('\n'.join(violations))
    sys.exit(1)
print('✅ guard-i18n：组件层无裸中文文案（注释不计）')
PY
