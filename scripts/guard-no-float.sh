#!/usr/bin/env bash
# 计费红线守卫（.cursor/rules/billing-safety.mdc）：
#   1) okapi-domain / okapi-pricing / okapi-ledger 的 src 与 tests 禁一切浮点
#   2) 上述 crate 的 src 禁 unwrap/expect/panic!/todo!/unimplemented!
set -euo pipefail
cd "$(dirname "$0")/.."

CRATES=(crates/okapi-domain crates/okapi-pricing crates/okapi-ledger)
FLOAT_PATTERN='\bf32\b|\bf64\b'
PANIC_PATTERN='\.unwrap\(\)|\.expect\(|panic!\(|todo!\(|unimplemented!\('

fail=0

for crate in "${CRATES[@]}"; do
    for dir in "$crate/src" "$crate/tests"; do
        [ -d "$dir" ] || continue
        if grep -rnE "$FLOAT_PATTERN" "$dir"; then
            echo "❌ 计费红线：$dir 中出现浮点类型（含测试）" >&2
            fail=1
        fi
    done
    # panic 类调用只查生产代码：#[cfg(test)] 之后的内容是模块内单测，unwrap 在断言语境里正当。
    # 约定测试模块位于文件末尾（rustfmt/社区惯例），故从首个 #[cfg(test)] 起截断。
    if [ -d "$crate/src" ] && ! python3 - "$crate/src" "$PANIC_PATTERN" <<'PY'
import pathlib, re, sys
root, pattern = pathlib.Path(sys.argv[1]), re.compile(sys.argv[2])
hits = []
for path in sorted(root.rglob('*.rs')):
    for lineno, line in enumerate(path.read_text().splitlines(), 1):
        if line.strip().startswith('#[cfg(test)]'):
            break
        if pattern.search(line):
            hits.append(f'{path}:{lineno}:{line}')
print('\n'.join(hits))
sys.exit(1 if hits else 0)
PY
    then
        echo "❌ 计费红线：$crate/src 中出现 unwrap/expect/panic 类调用" >&2
        fail=1
    fi
done

if [ "$fail" -eq 0 ]; then
    echo "✅ guard-no-float：计费路径未发现浮点与 panic 类调用"
fi
exit "$fail"
