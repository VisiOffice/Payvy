#!/usr/bin/env python3
"""Upload a small legacy bundle as inbox documents. Never creates bills/payees."""
import argparse
import base64
import fcntl
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
import uuid
from collections import Counter
from pathlib import Path

from common import MAX_BATCH, bundle_file, sha256, write_json


class ApiError(Exception):
    def __init__(self, status, body):
        super().__init__(f'command failed (HTTP {status})')
        self.status = status
        self.body = body


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        # Never forward the API key, including to an application sign-in redirect.
        return None


class Api:
    def __init__(self, url, key):
        parsed = urllib.parse.urlsplit(url)
        if (parsed.scheme not in ('http', 'https') or not parsed.hostname
                or parsed.username or parsed.password or parsed.query or parsed.fragment
                or parsed.path not in ('', '/')):
            raise ValueError('--api must be a server origin, without credentials or a path')
        if parsed.scheme != 'https' and parsed.hostname not in ('localhost', '127.0.0.1', '::1'):
            raise ValueError('HTTP is allowed only on loopback; use HTTPS for remote servers')
        self.url = url.rstrip('/')
        self.key = key
        self.opener = urllib.request.build_opener(NoRedirect())

    def run(self, command, value):
        if command not in ('document.upload', 'document.get', 'document.extract'):
            raise ValueError('importer may only upload, read, and extract documents')
        request = urllib.request.Request(
            f'{self.url}/api/commands/{command}',
            data=json.dumps({'input': value}).encode(),
            headers={'Authorization': f'Bearer {self.key}', 'Content-Type': 'application/json'},
            method='POST',
        )
        try:
            with self.opener.open(request, timeout=180) as response:
                return json.load(response)['result']
        except urllib.error.HTTPError as exc:
            # Retained only in memory to recover duplicate IDs. Never log bodies/URLs.
            body = exc.read(64 * 1024).decode('utf-8', errors='replace')
            raise ApiError(exc.code, body) from None
        except (urllib.error.URLError, TimeoutError, OSError):
            raise RuntimeError('request failed or timed out; rerun to reconcile the upload') from None


def verify_document(doc, entry, document_id=None):
    if (doc.get('sha256') != entry['file']['sha256']
            or doc.get('size_bytes') != entry['file']['size_bytes']
            or (document_id is not None and doc.get('id') != document_id)):
        raise ValueError('destination document does not match the bundle')
    return str(uuid.UUID(doc['id']))


def upload(api, entry, data):
    try:
        doc = api.run('document.upload', {
            'filename': f"legacy-payvy-{entry['source_id']}.{entry['file']['kind']}",
            'content_base64': base64.b64encode(data).decode(),
        })
        verify_document(doc, entry)
        return doc, 'uploaded'
    except ApiError as exc:
        if exc.status != 409:
            raise
        # Existing API reports the duplicate ID in a conflict message. If that
        # contract changes, fail closed. Verify it with document.get and SHA256.
        match = re.search(r'this exact file is already here: document ([0-9a-f-]{36})', exc.body)
        if not match:
            raise
        document_id = str(uuid.UUID(match[1]))
        doc = api.run('document.get', {'document_id': document_id})['document']
        verify_document(doc, entry, document_id)
        return doc, 'duplicate'


def read_bundle(manifest, limit):
    raw = manifest.read_bytes()
    bundle = json.loads(raw)
    if not isinstance(bundle, dict) or not isinstance(bundle.get('source'), dict):
        raise ValueError('invalid bundle structure')
    source = bundle['source']
    if (bundle.get('version') != 1 or source.get('system') != 'payvy-django'
            or type(source.get('company_id')) is not int or source['company_id'] <= 0
            or not isinstance(bundle.get('entries'), list)):
        raise ValueError('not a version 1 Payvy legacy bundle')
    selected, seen = [], set()
    for entry in bundle['entries']:
        if (not isinstance(entry, dict) or not isinstance(entry.get('source_id'), str)
                or type(entry.get('legacy_bill_id')) is not int or entry['legacy_bill_id'] <= 0):
            raise ValueError('entry must contain a legacy bill ID and UUID')
        source_id = str(uuid.UUID(entry['source_id']))
        if source_id != entry['source_id'] or source_id in seen:
            raise ValueError('duplicate or invalid source ID')
        seen.add(source_id)
        if entry['status'] not in ('ready', 'skipped', 'failed'):
            raise ValueError('bundle contains an invalid export status')
        if entry['status'] == 'ready' and len(selected) < limit:
            bundle_file(manifest.parent, entry['file'])
            selected.append(entry)
    return raw, bundle, selected


def run(args):
    manifest = args.manifest.resolve()
    raw, bundle, entries = read_bundle(manifest, args.limit)
    print(json.dumps({'mode': 'apply' if args.apply else 'dry_run',
                      'source_company_id': bundle['source']['company_id'],
                      'selected': len(entries),
                      'export_statuses': dict(Counter(e['status'] for e in bundle['entries']))}))
    if not args.apply:
        return 0  # Offline validation: no API key, requests, writes, or extraction.
    if not args.api:
        raise ValueError('--api is required with --apply')
    key = os.environ.get('PAYVY_IMPORT_API_KEY', '')
    if not key:
        raise ValueError('set PAYVY_IMPORT_API_KEY to a key for the destination test organization')
    api = Api(args.api, key)
    # Binding prevents an old journal from silently skipping work at another
    # destination or under another organization's key. The key itself is never saved.
    binding = {'manifest_sha256': sha256(raw), 'api': api.url, 'key_sha256': sha256(key.encode())}
    state_path = args.state or manifest.parent / 'import-state.json'
    state_path = state_path.resolve()
    if state_path == manifest or any(state_path == (manifest.parent / e['file']['path']).resolve() for e in entries):
        raise ValueError('state must not overwrite a bundle file')
    lock_path = state_path.with_name(state_path.name + '.lock')
    fd = os.open(lock_path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'w') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise ValueError('another importer is using this state file') from None
        if state_path.exists():
            state = json.loads(state_path.read_text())
            if state.get('version') != 1 or state.get('binding') != binding:
                raise ValueError('state belongs to another bundle, server, or API key; use a separate --state')
        else:
            state = {'version': 1, 'binding': binding, 'entries': {}}
        failures = 0
        for entry in entries:
            source_id = entry['source_id']
            record = state['entries'].setdefault(source_id, {'legacy_bill_id': entry['legacy_bill_id']})
            try:
                # Re-read before upload so changes after preflight cannot escape validation.
                data = bundle_file(manifest.parent, entry['file'])
                if record.get('document_id'):
                    got = api.run('document.get', {'document_id': record['document_id']})
                    doc = got['document']
                    verify_document(doc, entry, record['document_id'])
                    disposition = 'resumed'
                else:
                    doc, disposition = upload(api, entry, data)
                    record['document_id'] = doc['id']
                record.update(status=disposition, document_status=doc['status'])
                record.pop('error', None)
                write_json(state_path, state)  # Checkpoint upload before a costly extraction.
                if args.extract and doc['status'] == 'inbox':
                    got = api.run('document.get', {'document_id': doc['id']})
                    reads = got.get('extractions', [])
                    if any(read.get('extraction', {}).get('status') == 'read' for read in reads):
                        record['extraction'] = 'already_read'
                    elif reads:
                        # Failed/unclear reads may already have spent model credits.
                        # Do not retry them automatically on every importer invocation.
                        record['extraction'] = 'needs_review'
                    else:
                        try:
                            result = api.run('document.extract', {'document_id': doc['id']})
                            record['extraction'] = 'read' if result.get('extraction', {}).get('status') == 'read' else 'needs_review'
                        except ApiError as exc:
                            if exc.status != 409:
                                raise
                            # Background reader may have won the race.
                            got = api.run('document.get', {'document_id': doc['id']})
                            if not any(read.get('extraction', {}).get('status') == 'read' for read in got.get('extractions', [])):
                                raise
                            record['extraction'] = 'already_read'
                elif args.extract:
                    record['extraction'] = 'skipped_not_inbox'
                if record.get('extraction') == 'needs_review':
                    failures += 1
            except (ApiError, RuntimeError, ValueError, KeyError, OSError) as exc:
                # No provider payloads, invoice content, credentials, or signed URLs.
                record['error'] = str(exc) if isinstance(exc, (ApiError, RuntimeError)) else type(exc).__name__
                record['status'] = 'failed'
                failures += 1
            write_json(state_path, state)
            print(json.dumps({'source_id': source_id, **record}))
        print(json.dumps({'processed': len(entries), 'needs_attention': failures, 'state': str(state_path)}))
        return 1 if failures else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest', type=Path)
    parser.add_argument('--limit', type=int, default=25, help='maximum files, 1–50 (default 25)')
    parser.add_argument('--api', help='destination server origin; HTTPS except on loopback')
    parser.add_argument('--apply', action='store_true', help='upload documents; default only validates locally')
    parser.add_argument('--extract', action='store_true', help='read new documents with the configured model (may incur cost)')
    parser.add_argument('--state', type=Path, help='resume journal (default beside manifest)')
    args = parser.parse_args()
    if not 1 <= args.limit <= MAX_BATCH:
        parser.error('--limit must be 1–50')
    if args.extract and not args.apply:
        parser.error('--extract requires --apply')
    try:
        return run(args)
    except (ValueError, KeyError, OSError) as exc:
        print(f'Importer stopped: {exc}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
