#!/usr/bin/env python3
"""每个 Playwright spec 都得在某个 config 的 testMatch 里。

`playwright.interactions.config.ts` 等按文件名逐个列出要跑的 spec，新加的 spec 忘了登记就永远不会跑，
而且没有任何报错（e2e/analytics-metrics.spec.ts 就这样空转过）。CI 不跑 Playwright，
所以至少在这里把「写了却没人跑」拦下来。

用法：python3 scripts/guard-e2e-specs.py    # 有未登记的 spec 即非零退出
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

FRONTEND = Path(__file__).resolve().parent.parent / "frontend"


def main() -> int:
    listed: set[str] = set()
    for config in FRONTEND.glob("playwright*.config.ts"):
        listed |= set(re.findall(r"'([\w.-]+\.spec\.ts)'", config.read_text(encoding="utf-8")))
    specs = sorted(p.name for p in (FRONTEND / "e2e").glob("*.spec.ts"))
    orphans = [s for s in specs if s not in listed]
    stale = sorted(s for s in listed if s not in specs)
    for name in orphans:
        print(f"e2e/{name} 不在任何 playwright*.config.ts 的 testMatch 里，永远不会跑")
    for name in stale:
        print(f"testMatch 里的 {name} 已不存在")
    return 1 if orphans or stale else 0


if __name__ == "__main__":
    sys.exit(main())
