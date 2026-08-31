#!/usr/bin/env python3
"""校验前端引用的权限点都在后端权限清单内。

为什么要有这道闸：前端用权限点决定"哪些入口该出现"。写错一个字符串不会报错，
只会让该入口对**所有带自定义角色的用户**永久消失——而开发与演示用的超管走 `*`
通配，看什么都正常，这类错漏极难在手工点测中暴露。
"""

import glob
import re
import sys

BACKEND = 'crates/okapi-api/src/permissions.rs'
FRONTEND_GLOBS = ('frontend/src/**/*.tsx', 'frontend/src/**/*.ts')


def backend_permissions() -> set[str]:
    src = open(BACKEND, encoding='utf-8').read()
    return set(re.findall(r'pub const [A-Z_]+: &str = "([a-z_]+\.[a-z_]+)"', src))


def frontend_references() -> dict[str, set[str]]:
    """前端出现的 permission: 'x.y' → 文件集合。"""
    out: dict[str, set[str]] = {}
    for pattern in FRONTEND_GLOBS:
        for path in glob.glob(pattern, recursive=True):
            src = open(path, encoding='utf-8').read()
            for m in re.finditer(r"permission:\s*'([^']+)'", src):
                out.setdefault(m.group(1), set()).add(path)
    return out


def main() -> int:
    known = backend_permissions()
    if not known:
        print(f'❌ 未能从 {BACKEND} 解析出任何权限点，闸门本身可能失效')
        return 1

    used = frontend_references()
    unknown = {p: files for p, files in used.items() if p not in known}
    if unknown:
        print(f'❌ 前端引用了 {len(unknown)} 个后端不存在的权限点：')
        for p, files in sorted(unknown.items()):
            print(f'   {p}  ←  {", ".join(sorted(files))}')
        print(f'   后端可用权限点：{", ".join(sorted(known))}')
        return 1

    print(f'✅ guard-frontend-permissions：{len(used)} 个引用权限点均存在于后端清单')
    return 0


if __name__ == '__main__':
    sys.exit(main())
