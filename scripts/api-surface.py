#!/usr/bin/env python3
"""Inventory literal Axum routes. Static evidence only, never a test-pass claim.

Unlike a text window, this scanner ignores comments/strings and balances each
call. Unsupported route syntax fails instead of silently reducing coverage.
"""
import argparse
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parent.parent
METHODS = ('GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS', 'TRACE', 'CONNECT')


@dataclass
class Token:
    kind: str
    text: str
    start: int
    end: int


def tokens(source):
    out, i = [], 0
    while i < len(source):
        if source[i].isspace():
            i += 1
            continue
        if source.startswith('//', i):
            end = source.find('\n', i)
            i = len(source) if end < 0 else end
            continue
        if source.startswith('/*', i):
            depth, i = 1, i + 2
            while depth and i < len(source):
                if source.startswith('/*', i):
                    depth, i = depth + 1, i + 2
                elif source.startswith('*/', i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            if depth:
                raise ValueError('Unterminated block comment')
            continue
        start = i
        raw = re.match(r'(?:b|c)?r(#+)?"', source[i:])
        if raw:
            closing = '"' + (raw[1] or '')
            end = source.find(closing, i + len(raw[0]))
            if end < 0:
                raise ValueError('Unterminated raw string')
            i = end + len(closing)
            out.append(Token('string', source[start:i], start, i))
            continue
        string = re.match(r'(?:b|c)?"(?:\\.|[^"\\])*"', source[i:], re.S)
        char = re.match(r"(?:b)?'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'", source[i:])
        literal = string or char
        if literal:
            i += len(literal[0])
            out.append(Token('string' if string else 'char', source[start:i], start, i))
            continue
        ident = re.match(r'[a-zA-Z_][a-zA-Z_0-9]*', source[i:])
        i += len(ident[0]) if ident else 1
        out.append(Token('id' if ident else 'punct', source[start:i], start, i))
    return out


def close_at(items, start):
    pairs = {'(': ')', '[': ']', '{': '}'}
    stack = []
    for i in range(start, len(items)):
        token = items[i]
        if token.kind != 'punct':
            continue
        if token.text in pairs:
            stack.append(pairs[token.text])
        elif token.text in pairs.values():
            if not stack or stack.pop() != token.text:
                raise ValueError('Unbalanced route expression')
            if not stack:
                return i
    raise ValueError('Unclosed route expression')


def path_literal(token):
    if token.kind != 'string':
        raise ValueError('Route path must be a literal; add explicit support before proceeding')
    if token.text.startswith('"'):
        return json.loads(token.text)
    raw = re.fullmatch(r'r(#+)?"(.*)"\1', token.text, re.S)
    if raw:
        return raw[2]
    if token.text.startswith('r"'):
        return token.text[2:-1]
    raise ValueError(f'Unsupported route path literal: {token.text}')


def extract(source, surface, filename):
    items, result = tokens(source), []
    for i, token in enumerate(items):
        if token.text == '.' and i + 2 < len(items) and items[i + 1].text in ('nest', 'nest_service', 'route_service') and items[i + 2].text == '(':
            raise ValueError(f'Unsupported router composition in {filename}: {items[i + 1].text}')
        if token.text != '.' or i + 1 >= len(items) or items[i + 1].text != 'route':
            continue
        if i + 2 >= len(items) or items[i + 2].text != '(':
            raise ValueError(f'Unsupported route call in {filename}')
        end = close_at(items, i + 2)
        path = path_literal(items[i + 3])
        if not path.startswith('/') or items[i + 4].text != ',':
            raise ValueError('Unsupported path or route arguments')
        cursor, methods = i + 5, {}
        while cursor < end and items[cursor].text != ',':
            # Accept a qualified constructor (axum::routing::any), then method chains.
            if items[cursor].text == '.':
                cursor += 1
            name = items[cursor].text
            cursor += 1
            while [t.text for t in items[cursor:cursor + 2]] == [':', ':']:
                name = items[cursor + 2].text
                cursor += 3
            if items[cursor].text != '(':
                raise ValueError(f'Unsupported method router for {path}')
            call_end = close_at(items, cursor)
            handler = source[items[cursor].end:items[call_end].start].strip()
            if name.upper() in METHODS:
                methods[name.upper()] = (handler, 'explicit')
            elif name == 'any':
                methods.update({method: (handler, 'any') for method in METHODS})
            elif name not in ('layer', 'route_layer'):
                raise ValueError(f'Unsupported method {name} for {path}')
            cursor = call_end + 1
        if not methods or cursor > end or (cursor < end and cursor != end - 1):
            raise ValueError(f'Incomplete method router for {path}')
        if 'GET' in methods and 'HEAD' not in methods:
            methods['HEAD'] = (methods['GET'][0], 'implicit_head')
        for method, (handler, registration) in methods.items():
            result.append(dict(surface=surface, method=method, path=path,
                               handler=handler, registration=registration,
                               source=filename, line=source.count('\n', 0, token.start) + 1))
    return result


def inventory():
    endpoints, sources = [], {}
    for filename in sorted((ROOT / 'bins/okapi/src').rglob('*.rs')):
        source = filename.read_text()
        relative = filename.relative_to(ROOT).as_posix()
        parsed = extract(source, filename.parent.name, relative)
        if not parsed:
            continue
        if filename.parent.name not in ('gateway', 'console'):
            raise ValueError(f'Unknown router surface: {relative}')
        sources[relative] = hashlib.sha256(source.encode()).hexdigest()
        endpoints.extend(parsed)
    endpoints.sort(key=lambda row: (row['surface'], row['path'], row['method']))
    keys = [(row['surface'], row['method'], row['path']) for row in endpoints]
    if len(keys) != len(set(keys)):
        raise ValueError('Duplicate surface + method + path; inspect router composition')
    if not endpoints:
        raise ValueError('No routes found')
    return dict(schema=1, evidence='static route inventory; runtime/business behavior unverified',
                sources=sources, endpoints=endpoints)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--check', type=Path, help='Fail when a saved inventory differs from source')
    args = parser.parse_args()
    rendered = json.dumps(inventory(), ensure_ascii=False, indent=2) + '\n'
    if args.check:
        if args.check.read_text() != rendered:
            raise SystemExit(f'Stale API inventory: regenerate {args.check}')
        print(f'API inventory matches current source: {args.check}')
    elif args.output:
        args.output.write_text(rendered)
    else:
        print(rendered, end='')
