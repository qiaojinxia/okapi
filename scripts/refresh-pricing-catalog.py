#!/usr/bin/env python3
"""Refresh crates/okapi-pricing/data/litellm-catalog.json from the LiteLLM community price list.

The catalog is input for the admin (suggested prices, limits, capability hints shown next to the
model form), never the price book itself: every price an admin accepts is still saved as a draft and
published like any other. See docs/pricing-catalog.md.

    python3 scripts/refresh-pricing-catalog.py                  # newest commit of the upstream file
    python3 scripts/refresh-pricing-catalog.py --source FILE    # a local copy (offline refresh)

Only the fields the suggestion logic reads are kept, so the embedded file stays around a megabyte.
Numbers are written as plain decimals (no floats ever touch a price): the Rust side parses them as
exact decimals.
"""
import argparse
import datetime
import json
import re
import sys
import urllib.request
from decimal import Decimal
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / 'crates/okapi-pricing/data/litellm-catalog.json'
API = 'https://api.github.com/repos/BerriAI/litellm/commits?path=model_prices_and_context_window.json&per_page=1'
RAW = 'https://raw.githubusercontent.com/BerriAI/litellm/{ref}/model_prices_and_context_window.json'
SOURCE_PAGE = 'https://github.com/BerriAI/litellm/blob/{ref}/model_prices_and_context_window.json'

# LiteLLM `mode` values that map to an Okapi model kind. Evaluation / OCR / vector-store / guardrail
# entries have nothing to price on a gateway.
MODES = {
    'chat', 'responses', 'completion', 'embedding', 'rerank', 'image_generation', 'image_edit',
    'audio_transcription', 'audio_speech', 'realtime', 'video_generation', 'moderation', 'search',
}
# Regional copies of the same model (`bedrock/us-east-1/...`, `azure/eu/...`): an admin names the
# model, not a region, so these only add noise.
REGIONAL = re.compile(
    r'^(bedrock|bedrock_converse|azure|vertex_ai)/'
    r'([a-z]{2}-[a-z]+-\d|eu|us|global|global-standard|global-provisioned|us-gov|ap|apac|au|jp|ca)/'
)

# output key -> LiteLLM key; USD per token unless the name says otherwise.
PRICES = {
    'input': 'input_cost_per_token',
    'output': 'output_cost_per_token',
    'cache_read': 'cache_read_input_token_cost',
    'cache_write': 'cache_creation_input_token_cost',
    'cache_write_1h': 'cache_creation_input_token_cost_above_1hr',
    'input_above_200k': 'input_cost_per_token_above_200k_tokens',
    'output_above_200k': 'output_cost_per_token_above_200k_tokens',
    'input_above_272k': 'input_cost_per_token_above_272k_tokens',
    'output_above_272k': 'output_cost_per_token_above_272k_tokens',
    'input_audio': 'input_cost_per_audio_token',
    'output_audio': 'output_cost_per_audio_token',
    'output_reasoning': 'output_cost_per_reasoning_token',
    # not per token:
    'per_image': 'output_cost_per_image',  # USD per generated image
    'per_query': 'input_cost_per_query',  # USD per query (rerank / search)
    'input_second': 'input_cost_per_second',  # USD per second of audio / video input
    'output_second': 'output_cost_per_second',  # USD per second of generated audio / video
}
# service tiers are only recorded as present, the suggestion shows the standard rate
TIERS = {
    'batch': ('input_cost_per_token_batches', 'output_cost_per_token_batches'),
    'flex': ('input_cost_per_token_flex', 'output_cost_per_token_flex'),
    'priority': ('input_cost_per_token_priority', 'output_cost_per_token_priority'),
}
FLAGS = {
    'vision': 'supports_vision',
    'tools': 'supports_function_calling',
    'parallel_tools': 'supports_parallel_function_calling',
    'reasoning': 'supports_reasoning',
    'prompt_cache': 'supports_prompt_caching',
    'audio_in': 'supports_audio_input',
    'audio_out': 'supports_audio_output',
    'video_in': 'supports_video_input',
    'web_search': 'supports_web_search',
    'schema': 'supports_response_schema',
    'computer_use': 'supports_computer_use',
    'system_prompt': 'supports_system_messages',
}
MIN_ENTRIES = 1500  # a truncated download must not silently replace the catalog


def fetch(url):
    request = urllib.request.Request(url, headers={'User-Agent': 'okapi-refresh-pricing-catalog'})
    with urllib.request.urlopen(request, timeout=60) as response:
        return response.read().decode('utf-8')


def decimal_text(value):
    """Plain decimal text, or None for anything that is not a finite non-negative number."""
    if isinstance(value, bool) or not isinstance(value, (int, Decimal)):
        return None
    number = Decimal(value)
    if not number.is_finite() or number < 0:
        return None
    return format(number.normalize(), 'f') if number != 0 else '0'


def count(value):
    if isinstance(value, bool):
        return None
    if isinstance(value, int) and value > 0:
        return value
    if isinstance(value, Decimal) and value == value.to_integral_value() and value > 0:
        return int(value)
    return None


def trim(key, entry):
    mode = entry.get('mode')
    provider = entry.get('litellm_provider')
    if mode not in MODES or not isinstance(provider, str) or REGIONAL.match(key):
        return None
    out = {'provider': provider, 'mode': mode}
    for name, source in PRICES.items():
        text = decimal_text(entry.get(source))
        if text is not None:
            out[name] = text
    for name, (a, b) in TIERS.items():
        if a in entry or b in entry:
            out[name] = True
    context = count(entry.get('max_input_tokens'))
    max_output = count(entry.get('max_output_tokens'))
    legacy = count(entry.get('max_tokens'))
    if context is None and mode in ('embedding', 'rerank', 'moderation'):
        context = legacy
    if max_output is None and mode in ('chat', 'completion', 'responses'):
        max_output = legacy
    if context is not None:
        out['context'] = context
    if max_output is not None:
        out['max_output'] = max_output
    for name, source in FLAGS.items():
        if isinstance(entry.get(source), bool):
            out[name] = entry[source]
    for name, source in (('input_modalities', 'supported_modalities'), ('output_modalities', 'supported_output_modalities')):
        values = entry.get(source)
        if isinstance(values, list) and all(isinstance(v, str) for v in values) and values:
            out[name] = sorted(set(values))
    deprecated = entry.get('deprecation_date')
    if isinstance(deprecated, str) and re.fullmatch(r'\d{4}-\d{2}-\d{2}', deprecated):
        out['deprecated'] = deprecated
    source = entry.get('source')
    if isinstance(source, str) and source.startswith(('http://', 'https://')) and len(source) <= 300:
        out['source'] = source
    return out


def write(models, meta):
    """Deterministic JSON by hand: Decimal text is emitted as a bare number, exactly as stored."""
    def scalar(name, value):
        if isinstance(value, bool):
            return 'true' if value else 'false'
        if isinstance(value, int):
            return str(value)
        if isinstance(value, list):
            return json.dumps(value, ensure_ascii=False, separators=(',', ':'))
        if name in PRICES:
            return value
        return json.dumps(value, ensure_ascii=False)

    lines = ['{', '"meta":' + json.dumps(meta, ensure_ascii=False, sort_keys=True, separators=(',', ':')) + ',', '"models":{']
    items = sorted(models.items())
    for index, (key, entry) in enumerate(items):
        body = ','.join(f'"{name}":{scalar(name, entry[name])}' for name in sorted(entry))
        lines.append(f'{json.dumps(key, ensure_ascii=False)}:{{{body}}}' + (',' if index + 1 < len(items) else ''))
    lines += ['}', '}']
    return '\n'.join(lines) + '\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--source', help='local copy of model_prices_and_context_window.json instead of downloading')
    parser.add_argument('--commit', help='upstream commit to record (and to download when --source is absent)')
    parser.add_argument('--out', default=str(OUT))
    options = parser.parse_args()

    commit, committed_at = options.commit, None
    if options.source:
        text = Path(options.source).read_text(encoding='utf-8')
    else:
        if not commit:
            latest = json.loads(fetch(API))[0]
            commit, committed_at = latest['sha'], latest['commit']['committer']['date']
        text = fetch(RAW.format(ref=commit))
    raw = json.loads(text, parse_float=Decimal)
    raw.pop('sample_spec', None)

    models = {}
    for key, entry in raw.items():
        if isinstance(entry, dict):
            trimmed = trim(key, entry)
            if trimmed is not None:
                models[key] = trimmed
    if len(models) < MIN_ENTRIES:
        sys.exit(f'only {len(models)} usable entries (< {MIN_ENTRIES}): refusing to replace the catalog')

    meta = {
        'dataset': 'litellm',
        'license': 'MIT (c) 2023 Berri AI, see LICENSE-litellm.txt',
        'source': SOURCE_PAGE.format(ref=commit or 'main'),
        'commit': commit,
        'committed_at': committed_at,
        'generated_at': datetime.date.today().isoformat(),
        'entries': len(models),
    }
    content = write(models, meta)
    json.loads(content)  # whatever we wrote must parse
    Path(options.out).parent.mkdir(parents=True, exist_ok=True)
    Path(options.out).write_text(content, encoding='utf-8')
    print(f'{len(models)} of {len(raw)} entries kept, {len(content) / 1024:.0f} KiB -> {options.out}')


if __name__ == '__main__':
    main()
