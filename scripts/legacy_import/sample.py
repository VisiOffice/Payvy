#!/usr/bin/env python3
"""Create a two-invoice synthetic bundle for an importer smoke test."""
import argparse
import json
import os
import uuid
from datetime import datetime, timezone
from pathlib import Path

from common import file_kind, sha256, write_json


def make_sample(output):
    fixtures = Path(__file__).resolve().parents[2] / 'fixtures' / 'invoices'
    output.mkdir(mode=0o700, exist_ok=False)
    (output / 'files').mkdir(mode=0o700)
    entries = []
    for index, name in enumerate(('king-environmental-189727', 'desert-plumbing-5521'), 1):
        data = (fixtures / f'{name}.pdf').read_bytes()
        source_id = str(uuid.uuid5(uuid.NAMESPACE_URL, f'https://payvy.invalid/legacy-sample/{name}'))
        relative = f'files/{source_id}.pdf'
        fd = os.open(output / relative, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        with os.fdopen(fd, 'wb') as stream:
            stream.write(data)
        entries.append({
            'source_id': source_id, 'legacy_bill_id': index, 'status': 'ready', 'reason': None,
            'source_kind': 'synthetic_fixture', 'source_pages': [],
            'reference': json.loads((fixtures / f'{name}.expected.json').read_text()),
            'file': {'path': relative, 'kind': file_kind(data), 'size_bytes': len(data), 'sha256': sha256(data)},
        })
    write_json(output / 'manifest.json', {
        'version': 1, 'sample_only': True,
        'source': {'system': 'payvy-django', 'company_id': 1},
        'exported_at': datetime.now(timezone.utc).isoformat(), 'entries': entries,
    })
    print(f'Created synthetic sample: {output / "manifest.json"}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True, help='new directory; parent must exist')
    args = parser.parse_args()
    make_sample(args.output)
