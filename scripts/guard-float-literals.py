#!/usr/bin/env python3
"""计费 crate 里禁浮点字面量（`let x = 0.5;`、`1e3`、`2.0_f32`、`1f64`）。

`guard-no-float.sh` 里的 grep 只认 `f32` / `f64` 这两个类型记号，而 Rust 里一个裸的 `0.5` 就足以让变量
推断成浮点——记号一个都不出现，守卫却过了。这里先把注释、字符串（含 raw / byte 字符串）和字符字面量剥掉，
再在剩下的代码里找浮点字面量，所以文档里写的 "0.5"、测试里 `"1.25"` 这样的十进制字符串不会误报。

用法：python3 scripts/guard-float-literals.py <目录>...   有命中则打印并以 1 退出。
"""

import pathlib
import re
import sys

# 浮点字面量：`1.5`、`1.5e-3`、`1e3`、`1.`（单独的点结尾）。前面不能是标识符字符或点（排除 `x.0.1` 这类
# 元组字段链、`0x1e5` 十六进制），`1.` 之后不能再跟标识符字符或点（排除 `1.max(2)`、`0..5`）。
FLOAT = re.compile(
    r'(?<![\w.])(?:\d[\d_]*\.\d[\d_]*(?:[eE][+-]?\d+)?|\d[\d_]*[eE][+-]?\d+|\d[\d_]*\.(?![\w.]))(?:_?f(?:32|64))?'
)
# 整数写法的浮点：`1f64`、`2_f32`。
SUFFIXED = re.compile(r'(?<![\w.])\d[\d_]*_?f(?:32|64)\b')
RAW_STRING = re.compile(r'b?r(#*)"')
CHAR_LITERAL = re.compile(r"b?'(\\.[^']*|[^\\'])'")


def strip_rust(src: str) -> str:
    """去掉注释、字符串和字符字面量，保留换行（行号不变）。"""
    out: list[str] = []
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        nxt = src[i + 1] if i + 1 < n else ''
        if c == '/' and nxt == '/':
            while i < n and src[i] != '\n':
                i += 1
            continue
        if c == '/' and nxt == '*':
            depth = 1
            i += 2
            while i < n and depth:
                if src.startswith('/*', i):
                    depth += 1
                    i += 2
                elif src.startswith('*/', i):
                    depth -= 1
                    i += 2
                else:
                    if src[i] == '\n':
                        out.append('\n')
                    i += 1
            continue
        raw = RAW_STRING.match(src, i)
        if raw and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == '_')):
            end = src.find('"' + raw.group(1), raw.end())
            end = n if end < 0 else end + 1 + len(raw.group(1))
            out.append('\n' * src.count('\n', i, end))
            i = end
            continue
        if c == '"' or (c == 'b' and nxt == '"'):
            i += 1 if c == '"' else 2
            while i < n and src[i] != '"':
                if src[i] == '\\':
                    i += 1
                if i < n and src[i] == '\n':
                    out.append('\n')
                i += 1
            i += 1
            continue
        char = CHAR_LITERAL.match(src, i)
        if char and (c == "'" or c == 'b'):
            i = char.end()
            continue
        out.append(c)
        i += 1
    return ''.join(out)


def float_literals(src: str) -> list[tuple[int, str]]:
    """（行号, 命中的字面量）列表。"""
    hits: list[tuple[int, str]] = []
    for lineno, line in enumerate(strip_rust(src).splitlines(), 1):
        found = [m.group(0) for m in FLOAT.finditer(line)]
        found += [m.group(0) for m in SUFFIXED.finditer(line) if not any(m.group(0) in f for f in found)]
        hits += [(lineno, literal) for literal in found]
    return hits


def main(argv: list[str]) -> int:
    failed = False
    for root in argv:
        base = pathlib.Path(root)
        if not base.is_dir():
            continue
        for path in sorted(base.rglob('*.rs')):
            text = path.read_text(encoding='utf-8')
            lines = text.splitlines()
            for lineno, literal in float_literals(text):
                failed = True
                print(f'{path}:{lineno}: 浮点字面量 {literal}  |  {lines[lineno - 1].strip()}')
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
