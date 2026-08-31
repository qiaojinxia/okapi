#!/usr/bin/env bash
# i18n 红线（IMPLEMENTATION §8）：前端组件禁裸中文文案，一律 t('ns:key')。
# 中文只允许出现在语言包（frontend/src/locales/）。
set -euo pipefail
cd "$(dirname "$0")/.."
python3 - <<'PY'
import pathlib, re, sys

cjk = re.compile(r'[\u4e00-\u9fa5]')
violations = []
root = pathlib.Path('frontend/src')
for path in root.rglob('*'):
    if path.suffix not in {'.ts', '.tsx'}:
        continue
    if 'locales' in path.parts:
        continue
    for lineno, line in enumerate(path.read_text().splitlines(), 1):
        stripped = line.strip()
        # 注释行放行（文档注释允许中文）
        if stripped.startswith(('//', '*', '/*', '///')):
            continue
        if cjk.search(line):
            violations.append(f'{path}:{lineno}: {stripped}')

if violations:
    print('❌ guard-i18n：组件内发现裸中文文案（应走 locales 语言包）：')
    print('\n'.join(violations))
    sys.exit(1)
print('✅ guard-i18n：组件层无裸中文文案')
PY
