"""Run with python -m unittest discover -s scripts/legacy_import -v."""
import contextlib
import importlib.util
import io
import json
import tempfile
import unittest
import uuid
from pathlib import Path
from types import SimpleNamespace as NS
from unittest.mock import patch

from common import MAX_BYTES, bundle_file, cents, sha256, write_json


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


imp = module('legacy_import', 'import.py')
exp = module('legacy_export', 'export.py')


class LegacyPlanning(unittest.TestCase):
    def setUp(self):
        self.inbox = NS(id=10, receiver_id=7, deleted=False, processed=True, original_attachment='original.pdf')
        self.source = NS(id=20, inbox_item_id=10, inbox_item=self.inbox,
                         invoice_number='INV-1', deleted=False, page='scan.png')
        self.page = NS(id=30, inbox_item_id=20, inbox_item=self.source, invoice_number='INV-1')
        self.bill = NS(receiver_id=7)

    def plan(self, pages=None, source=None, shared=None):
        return exp.plan_source(self.bill, pages or [self.page], source or [self.source], shared or set())

    def test_complete_original_and_single_page_fallback(self):
        self.assertEqual(self.plan(), ('original.pdf', 'original', None))
        self.inbox.original_attachment = None
        self.assertEqual(self.plan(), ('scan.png', 'single_page_fallback', None))

    def test_mixed_deleted_cross_company_and_multiple_invoice_files_skip(self):
        self.assertEqual(self.plan(shared={20})[2], 'mixed_or_partial_attachment')
        self.assertEqual(self.plan(source=[self.source, NS(id=21, deleted=False)])[2], 'mixed_or_partial_attachment')
        self.source.deleted = True
        self.assertEqual(self.plan()[2], 'deleted_source_pages')
        self.source.deleted = False
        self.inbox.receiver_id = 8
        self.assertEqual(self.plan()[2], 'company_mismatch')
        self.inbox.receiver_id = 7
        self.source.invoice_number = 'INV-2'
        self.assertEqual(self.plan()[2], 'multiple_invoice_numbers')

    def test_incomplete_source_processing_is_not_assumed_complete(self):
        self.inbox.processed = False
        self.assertEqual(self.plan()[2], 'source_processing_incomplete')

    def test_multiple_attachments_and_missing_page_links_skip(self):
        second = NS(inbox_item=NS(inbox_item_id=11))
        self.assertEqual(self.plan(pages=[self.page, second])[2], 'multiple_attachments')
        self.page.inbox_item = None
        self.assertEqual(self.plan()[2], 'missing_page_links')

    def test_money_uses_decimal_dollars_not_float_multiplication(self):
        self.assertEqual(cents(1.005), 101)
        self.assertEqual(cents(358.88), 35888)
        self.assertEqual(cents(-10.01), -1001)
        self.assertIsNone(cents(None))
        with self.assertRaises(ValueError):
            cents(float('nan'))


class FakeApi:
    def __init__(self):
        self.url = 'http://127.0.0.1:12345'
        self.doc = None
        self.reads = []
        self.calls = []
        self.lose_upload_response = False
        self.fail_extract = False

    def run(self, command, value):
        self.calls.append(command)
        if command == 'document.upload':
            if self.doc:
                raise imp.ApiError(409, f"this exact file is already here: document {self.doc['id']} (invoice.pdf), in inbox")
            self.doc = {'id': str(uuid.uuid4()), 'sha256': sha256(b'%PDF-1.4\nfixture'),
                        'size_bytes': len(b'%PDF-1.4\nfixture'), 'status': 'inbox'}
            if self.lose_upload_response:
                self.lose_upload_response = False
                raise RuntimeError('request failed or timed out; rerun to reconcile the upload')
            return self.doc
        if command == 'document.get':
            return {'document': self.doc, 'extractions': self.reads}
        if command == 'document.extract':
            answer = {'extraction': {'status': 'failed' if self.fail_extract else 'read'}}
            self.reads.append(answer)
            return answer
        raise AssertionError('unexpected command')


class ImportPilot(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        data = b'%PDF-1.4\nfixture'
        (self.root / 'invoice.pdf').write_bytes(data)
        self.entry = {'source_id': str(uuid.uuid4()), 'legacy_bill_id': 42, 'status': 'ready',
                      'file': {'path': 'invoice.pdf', 'kind': 'pdf', 'sha256': sha256(data), 'size_bytes': len(data)}}
        self.manifest = self.root / 'manifest.json'
        write_json(self.manifest, {'version': 1, 'source': {'system': 'payvy-django', 'company_id': 7},
                                   'entries': [self.entry]})
        self.api = FakeApi()
        self.args = NS(manifest=self.manifest, limit=25, api=self.api.url, apply=True, extract=True, state=None)

    def run_import(self):
        with patch.object(imp, 'Api', return_value=self.api), patch.dict('os.environ', {'PAYVY_IMPORT_API_KEY': 'test-only'}), contextlib.redirect_stdout(io.StringIO()):
            return imp.run(self.args)

    def test_dry_run_is_offline_and_does_not_write_journal(self):
        self.args.apply = False
        self.assertEqual(self.run_import(), 0)
        self.assertEqual(self.api.calls, [])
        self.assertFalse((self.root / 'import-state.json').exists())

    def test_resume_and_lost_journal_verify_duplicate_hash_and_do_not_reextract(self):
        self.assertEqual(self.run_import(), 0)
        self.assertEqual(self.run_import(), 0)
        (self.root / 'import-state.json').unlink()
        self.assertEqual(self.run_import(), 0)
        self.assertEqual(self.api.calls.count('document.upload'), 2)
        self.assertEqual(self.api.calls.count('document.extract'), 1)
        state = json.loads((self.root / 'import-state.json').read_text())
        self.assertEqual(state['entries'][self.entry['source_id']]['status'], 'duplicate')
        self.assertEqual((self.root / 'import-state.json').stat().st_mode & 0o777, 0o600)
        self.assertNotIn('test-only', json.dumps(state))

    def test_lost_upload_response_is_reconciled_on_rerun(self):
        self.api.lose_upload_response = True
        self.assertEqual(self.run_import(), 1)
        self.assertEqual(self.run_import(), 0)
        self.assertEqual(self.api.calls.count('document.extract'), 1)

    def test_failed_extraction_is_not_blindly_retried(self):
        self.api.fail_extract = True
        self.assertEqual(self.run_import(), 1)
        self.assertEqual(self.run_import(), 1)
        self.assertEqual(self.api.calls.count('document.extract'), 1)

    def test_filed_documents_remain_filed(self):
        self.args.extract = False
        self.assertEqual(self.run_import(), 0)
        self.api.doc['status'] = 'filed'
        self.args.extract = True
        self.assertEqual(self.run_import(), 0)
        self.assertNotIn('document.extract', self.api.calls)

    def test_hash_mismatch_and_wrong_destination_stop(self):
        self.assertEqual(self.run_import(), 0)
        self.api.doc['sha256'] = 'wrong'
        self.assertEqual(self.run_import(), 1)
        self.api.url = 'http://localhost:12345'
        with self.assertRaisesRegex(ValueError, 'another bundle'):
            self.run_import()

    def test_all_selected_files_checked_before_first_upload(self):
        entry = {**self.entry, 'source_id': str(uuid.uuid4()), 'file': {**self.entry['file'], 'sha256': 'wrong'}}
        bundle = json.loads(self.manifest.read_text())
        bundle['entries'].append(entry)
        write_json(self.manifest, bundle)
        with self.assertRaisesRegex(ValueError, 'hash'):
            self.run_import()
        self.assertEqual(self.api.calls, [])

    def test_symlink_escape_and_oversize_are_refused(self):
        (self.root / 'escape').symlink_to('/etc/hosts')
        with self.assertRaisesRegex(ValueError, 'escapes'):
            bundle_file(self.root, {**self.entry['file'], 'path': 'escape'})
        with (self.root / 'invoice.pdf').open('wb') as stream:
            stream.truncate(MAX_BYTES + 1)
        with self.assertRaisesRegex(ValueError, 'exceeds'):
            bundle_file(self.root, self.entry['file'])

    def test_api_refuses_remote_http_credentials_paths_and_redirects(self):
        for url in ('http://example.com', 'https://key@example.com', 'https://example.com/api', 'file:///tmp/x'):
            with self.assertRaises(ValueError):
                imp.Api(url, 'test-only')
        self.assertIsNone(imp.NoRedirect().redirect_request(None, None, 302, '', {}, 'https://example.com'))


if __name__ == '__main__':
    unittest.main()
