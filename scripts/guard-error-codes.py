#!/usr/bin/env python3
"""后端 error_code 全集 → 前端 errors 命名空间反向核对（IMPLEMENTATION §8 i18n 红线的另一半）。

`guard-i18n-keys.py` 只核对前端引用到的键是否定义；这里反过来：后端能返回的每个 error_code
（`okapi_api::codes::*` 常量、`AppError::new / unauthorized` 与 `StoreError::Conflict` 的字面量）
在 zh-CN / en 两个语言包的 `errors` 命名空间里都必须有文案，否则用户看到的是
`describeError` 的兜底"未知错误 (code)"。

用法：python3 scripts/guard-error-codes.py    # 有缺失即非零退出
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RUST_DIRS = [ROOT / "bins" / "okapi" / "src", *(p / "src" for p in (ROOT / "crates").iterdir() if p.is_dir())]
CODES_RS = ROOT / "crates" / "okapi-api" / "src" / "error.rs"
LOCALES = {
    "zh-CN": ROOT / "frontend" / "src" / "locales" / "zh-CN.ts",
    "en": ROOT / "frontend" / "src" / "locales" / "en.ts",
}
# 由前端按 HTTP 状态 / 内部兜底渲染，不是后端 error_code
FRONTEND_ONLY = {"unknown"}


def split_top_level_args(text: str) -> list[str]:
    """按顶层逗号切分参数列表（忽略括号与字符串内的逗号）。"""
    args, depth, buf, in_str = [], 0, [], False
    i = 0
    while i < len(text):
        ch = text[i]
        if in_str:
            buf.append(ch)
            if ch == "\\":
                buf.append(text[i + 1])
                i += 1
            elif ch == '"':
                in_str = False
        elif ch == '"':
            in_str = True
            buf.append(ch)
        elif ch in "([{":
            depth += 1
            buf.append(ch)
        elif ch in ")]}":
            depth -= 1
            buf.append(ch)
        elif ch == "," and depth == 0:
            args.append("".join(buf).strip())
            buf = []
        else:
            buf.append(ch)
        i += 1
    if "".join(buf).strip():
        args.append("".join(buf).strip())
    return args


def call_args(src: str, open_paren: int) -> str:
    """返回 `(` 到配对 `)` 之间的文本。"""
    depth, i, in_str = 0, open_paren, False
    while i < len(src):
        ch = src[i]
        if in_str:
            if ch == "\\":
                i += 1
            elif ch == '"':
                in_str = False
        elif ch == '"':
            in_str = True
        elif ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
            if depth == 0:
                return src[open_paren + 1 : i]
        i += 1
    return src[open_paren + 1 :]


def main() -> int:
    consts = dict(re.findall(r'pub const ([A-Z_]+): &str = "([a-z0-9_]+)";', CODES_RS.read_text()))
    codes: dict[str, set[str]] = {}  # code → 出现位置
    dynamic: list[str] = []

    def note(code: str, where: str) -> None:
        codes.setdefault(code, set()).add(where)

    def resolve(arg: str, where: str, local_consts: dict[str, str]) -> None:
        arg = arg.strip()
        m = re.fullmatch(r'"([a-z][a-z0-9_]*)"', arg)
        if m:
            note(m.group(1), where)
            return
        m = re.fullmatch(r"(?:okapi_api::)?codes::([A-Z_]+)", arg)
        if m:
            if m.group(1) not in consts:
                print(f"❌ {where}: 未知常量 codes::{m.group(1)}")
                sys.exit(1)
            note(consts[m.group(1)], where)
            return
        if arg in local_consts:
            note(local_consts[arg], where)
            return
        dynamic.append(f"{where}: {arg[:60]}")

    for value in consts.values():
        note(value, str(CODES_RS.relative_to(ROOT)))

    patterns = [
        # (调用名, 取第几个参数作为 code, 只在哪些文件里找)
        (re.compile(r"\bAppError::new\("), 1, None),
        (re.compile(r"\bAppError::unauthorized\("), 0, None),
        (re.compile(r"\bStoreError::Conflict\("), 0, None),
        (re.compile(r"\bErrorBody::new\("), 0, None),
        # AppError 自己的 From 实现里用 Self::new（StatusCode 打头的才是它）
        (re.compile(r"\bSelf::new\((?=StatusCode::)"), 1, "bins/okapi/src/gateway/error.rs"),
    ]
    for rust_dir in RUST_DIRS:
        for path in sorted(rust_dir.rglob("*.rs")):
            src = path.read_text()
            rel = str(path.relative_to(ROOT))
            # 模块级 `const X: &str = "code";`（如 subscriptions.rs 的 PLAN_NOT_PURCHASABLE）
            local_consts = dict(re.findall(r'const ([A-Z_]+): &str = "([a-z][a-z0-9_]*)";', src))
            for pat, idx, only in patterns:
                if only and rel != only:
                    continue
                for m in pat.finditer(src):
                    line = src.count("\n", 0, m.start()) + 1
                    args = split_top_level_args(call_args(src, m.end() - 1))
                    if len(args) <= idx:
                        continue
                    resolve(args[idx], f"{rel}:{line}", local_consts)

    missing: dict[str, list[str]] = {}
    for lang, path in LOCALES.items():
        text = path.read_text()
        block = re.search(r"^  errors: \{\n(.*?)^  \},?\n", text, re.S | re.M)
        if not block:
            print(f"❌ {path.relative_to(ROOT)}：找不到 errors 命名空间")
            return 1
        keys = set(re.findall(r"^\s+'?([A-Za-z0-9_]+)'?\s*:", block.group(1), re.M))
        for code in sorted(codes):
            if code not in keys and code not in FRONTEND_ONLY:
                missing.setdefault(code, []).append(lang)

    if missing:
        print(f"❌ guard-error-codes：{len(missing)} 个后端 error_code 缺少语言包文案")
        for code, langs in missing.items():
            print(f"   {code} ← 缺 {'/'.join(langs)}；出处 {sorted(codes[code])[0]}")
        return 1
    print(f"✅ guard-error-codes：后端 {len(codes)} 个 error_code 在 zh-CN / en 的 errors 命名空间均有文案")
    if dynamic:
        print(f"   （{len(dynamic)} 处 code 为变量，无法静态核对，例：{dynamic[0]}）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
