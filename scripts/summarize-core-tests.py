#!/usr/bin/env python3
"""Summarize Cargo --nocapture output without equating probes with API coverage.

The caller supplies the process exit code only after observing process completion.
Without it, even an all-green log remains an incomplete execution.
"""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re


ANSI = re.compile(r'\x1b\[[0-9;]*m')
RESULT = re.compile(
    r'^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; '
    r'(\d+) measured; (\d+) filtered out(?=;|\s|$)')
START = re.compile(r'^\s*(Running .+|Doc-tests .+)$')
EMBEDDED_START = re.compile(
    r'\s+(Running (?:unittests |tests/|benches/).+ \(.+\)|Doc-tests \S+)\s*$')
TEST = re.compile(r'^test (.+?) \.\.\.\s*(.*)$')
SKIP = re.compile(r'跳过[：:]|\bskipp(?:ed|ing)\b', re.I)


def log_lines(raw):
    """Keep physical line numbers when Cargo's stderr interrupts a stdout result."""
    for number, raw_line in enumerate(raw.splitlines(), 1):
        line = ANSI.sub('', raw_line).strip()
        result = RESULT.match(line)
        embedded = EMBEDDED_START.fullmatch(line[result.end():]) if result else None
        if embedded:
            yield number, line[:result.end()]
            yield number, embedded[1]
        else:
            yield number, line


def summarize(raw, exit_code=None):
    suites, probes, skip_signals, parse_errors = [], [], [], []
    suite = pending = None
    for number, line in log_lines(raw):
        start = START.match(line)
        if start:
            suite = {'target': start[1], 'line': number, 'tests': [], 'result': None}
            suites.append(suite)
            pending = None
            continue
        if SKIP.search(line):
            skip_signals.append({'line': number, 'target': suite['target'] if suite else None,
                                 'message': line})
        # The first record can share the harness's "test name ..." line.
        if 'API_PROBE ' in line:
            try:
                probe = json.loads(line.split('API_PROBE ', 1)[1])
                if not isinstance(probe, dict) or not all(
                    key in probe for key in ('surface', 'method', 'path', 'phase', 'status', 'evidence')
                ):
                    raise ValueError('missing probe fields')
                probes.append({**probe, 'line': number})
            except (ValueError, TypeError) as error:
                parse_errors.append({'line': number, 'message': f'Invalid API_PROBE: {error}'})
        if suite is None:
            continue
        result = RESULT.match(line)
        if result:
            if suite['result'] is not None:
                parse_errors.append({'line': number, 'message': f"Duplicate result for {suite['target']}"})
                continue
            keys = ('passed', 'failed', 'ignored', 'measured', 'filtered_out')
            suite['result'] = {'status': result[1], **dict(zip(keys, map(int, result.groups()[1:])))}
            pending = None
            continue
        test = TEST.match(line)
        if test:
            pending = {'name': test[1], 'outcome': None}
            suite['tests'].append(pending)
            ending = test[2]
        else:
            ending = line
        if pending is not None:
            outcome = re.fullmatch(r'(ok|FAILED|ignored)(?:, .*)?', ending)
            if outcome:
                pending['outcome'] = outcome[1]
                pending = None

    totals = Counter()
    for item in suites:
        if item['result']:
            totals.update({k: item['result'][k] for k in ('passed', 'failed', 'ignored', 'filtered_out')})
            recorded = Counter(test['outcome'] for test in item['tests'])
            expected = Counter({outcome: item['result'][key] for outcome, key in
                                (('ok', 'passed'), ('FAILED', 'failed'), ('ignored', 'ignored'))
                                if item['result'][key]})
            if recorded != expected:
                parse_errors.append({'line': item['line'], 'message':
                                     f"Test outcomes do not match result for {item['target']}"})
    unfinished = [item['target'] for item in suites if item['result'] is None]
    if exit_code is None:
        status = 'incomplete'
    elif exit_code != 0 or totals['failed']:
        status = 'failed'
    elif not suites or unfinished or parse_errors:
        status = 'incomplete'
    elif totals['ignored'] or skip_signals:
        status = 'passed_with_skips_or_skip_signals'
    else:
        status = 'passed'
    return {
        'scope': 'Listed test suites and API error probes only; not proof of feature parity or API business coverage.',
        'execution_status': status, 'observed_exit_code': exit_code,
        'totals': dict(totals), 'unfinished_suites': unfinished,
        'suites': suites, 'skip_signals': skip_signals, 'parse_errors': parse_errors,
        'api_probe_counts': dict(Counter(f"{p['phase']}:{p['evidence']}" for p in probes)),
        'api_probes': probes,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('log', type=Path)
    parser.add_argument('--exit-code', type=int, help='Observed process exit code; omit while running')
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    raw = args.log.read_bytes()
    report = summarize(raw.decode('utf-8', errors='replace'), args.exit_code)
    report['log'] = {'path': str(args.log.resolve()), 'sha256': hashlib.sha256(raw).hexdigest()}
    args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({k: report[k] for k in ('execution_status', 'observed_exit_code', 'totals', 'api_probe_counts')}, ensure_ascii=False))


if __name__ == '__main__':
    main()
