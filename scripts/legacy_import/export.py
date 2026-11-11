#!/usr/bin/env python3
"""Read-only export of a small sample from the legacy Django Payvy project."""
import argparse
import json
import os
import sys
from collections import Counter
from datetime import date, datetime, timezone
from pathlib import Path

from common import MAX_BATCH, MAX_BYTES, cents, file_kind, sha256, write_json


def plan_source(bill, pages, source_pages, shared_ids):
    """Conservative pilot: one complete, unshared attachment per legacy bill.

    Querying and copying are separate so inventory and export share the rules.
    source_pages includes deleted pages: original bytes still contain them.
    """
    if not pages or any(p.inbox_item is None for p in pages):
        return None, None, 'missing_page_links'
    inbox_ids = {p.inbox_item.inbox_item_id for p in pages}
    if len(inbox_ids) != 1:
        return None, None, 'multiple_attachments'
    inbox = pages[0].inbox_item.inbox_item
    if inbox.receiver_id != bill.receiver_id:
        return None, None, 'company_mismatch'
    if inbox.processed is not True:
        return None, None, 'source_processing_incomplete'
    if inbox.deleted or any(p.deleted for p in source_pages):
        return None, None, 'deleted_source_pages'
    page_ids = [p.inbox_item_id for p in pages]
    if (len(page_ids) != len(set(page_ids))
            or set(page_ids) != {p.id for p in source_pages} or shared_ids):
        return None, None, 'mixed_or_partial_attachment'
    numbers = {str(n).strip() for p in pages
               for n in (p.invoice_number, p.inbox_item.invoice_number) if n and str(n).strip()}
    if len(numbers) > 1:
        return None, None, 'multiple_invoice_numbers'
    if inbox.original_attachment:
        return inbox.original_attachment, 'original', None
    # A single page is usable without combining or guessing PDF page ordering.
    if len(pages) == 1 and pages[0].inbox_item.page:
        return pages[0].inbox_item.page, 'single_page_fallback', None
    return None, None, 'missing_original'


def reference(bill, pages):
    contact = bill.contact
    numbers = sorted({str(n).strip() for p in pages
                      for n in (p.invoice_number, p.inbox_item.invoice_number if p.inbox_item else None)
                      if n and str(n).strip()})
    def day(value):
        return value.date().isoformat() if value else None
    return {
        'vendor': {'legacy_id': contact.id, 'name': contact.name, 'email': contact.email} if contact else None,
        'invoice_number': numbers[0] if len(numbers) == 1 else None,
        'invoice_date': day(bill.created), 'due_date': day(bill.due_date),
        'amount_cents': cents(bill.amount), 'paid_cents': cents(bill.paid),
        'legacy_status': bill.status,
    }


def export(args):
    # This tool ships in the NEW repo, but executes with the OLD Django runtime,
    # database configuration and storage credentials. No legacy models are saved.
    root = args.django_root.resolve()
    if not (root / 'manage.py').is_file():
        raise ValueError('--django-root must contain the legacy manage.py')
    sys.path.insert(0, str(root))
    os.environ.setdefault('DJANGO_SETTINGS_MODULE', args.settings)
    import django
    django.setup()
    from django.db import connection, transaction
    from billing.models import Bill, BillItemPage
    from inbox.models import InboxItemPage

    if connection.vendor != 'postgresql':
        raise ValueError('export requires PostgreSQL to enforce a read-only transaction')
    output = args.output.resolve() if args.output else None
    if args.apply:
        if output is None:
            raise ValueError('--output is required with --apply')
        output.mkdir(mode=0o700, parents=False, exist_ok=False)
        (output / 'files').mkdir(mode=0o700)
    bundle = {
        'version': 1, 'source': {'system': 'payvy-django', 'company_id': args.company_id},
        'exported_at': datetime.now(timezone.utc).isoformat(), 'entries': [],
    }
    with transaction.atomic():
        with connection.cursor() as cursor:
            cursor.execute('SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY')
        bills = Bill.objects.filter(receiver_id=args.company_id).select_related('contact')
        if args.date_from:
            bills = bills.filter(created__date__gte=args.date_from)
        if args.date_to:
            bills = bills.filter(created__date__lte=args.date_to)
        if args.bill_id:
            bills = bills.filter(pk__in=args.bill_id)
        for bill in bills.order_by('-created_at', '-pk')[:args.limit]:
            pages = list(bill.bill_pages.select_related('inbox_item__inbox_item').order_by('page_number', 'pk'))
            inbox_ids = {p.inbox_item.inbox_item_id for p in pages if p.inbox_item}
            source_pages = list(InboxItemPage.objects.filter(inbox_item_id__in=inbox_ids))
            ids = [p.inbox_item_id for p in pages if p.inbox_item_id]
            shared = set(BillItemPage.objects.filter(inbox_item_id__in=ids).exclude(bill_id=bill.id)
                         .values_list('inbox_item_id', flat=True))
            field, kind, reason = plan_source(bill, pages, source_pages, shared)
            entry = {
                'source_id': str(bill.unique_hash), 'legacy_bill_id': bill.id,
                'status': 'skipped' if reason else 'eligible', 'reason': reason,
                'source_kind': kind,
                'source_pages': [{'bill_page_id': p.id, 'inbox_page_id': p.inbox_item_id,
                                  'inbox_item_id': p.inbox_item.inbox_item_id if p.inbox_item else None,
                                  'page_number': p.inbox_item.page_number if p.inbox_item else None}
                                 for p in pages],
            }
            try:
                entry['reference'] = reference(bill, pages)
            except (ValueError, TypeError):
                entry.update(status='skipped', reason='invalid_legacy_metadata')
            if args.apply and entry['status'] == 'eligible':
                try:
                    # Access via the configured Django storage, never a public URL
                    # or the summary PDF generated by the old billing views.
                    with field.open('rb') as stream:
                        data = stream.read(MAX_BYTES + 1)
                    extension = file_kind(data)
                    name = f"files/{entry['source_id']}.{extension}"
                    fd = os.open(output / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                    with os.fdopen(fd, 'wb') as stream:
                        stream.write(data)
                        stream.flush()
                        os.fsync(stream.fileno())
                    entry['file'] = {'path': name, 'sha256': sha256(data), 'size_bytes': len(data), 'kind': extension}
                    entry.update(status='ready', reason=None)
                except ValueError:
                    entry.update(status='skipped', reason='unsupported_empty_or_oversize_file')
                except Exception:
                    # Storage exceptions can contain signed URLs/credentials. Keep
                    # the actionable category, never the provider's raw message.
                    entry.update(status='failed', reason='source_file_unavailable')
            bundle['entries'].append(entry)
            if args.apply:
                write_json(output / 'manifest.json', bundle)
            print(json.dumps({k: entry[k] for k in ('source_id', 'legacy_bill_id', 'status', 'reason')}))
    if args.apply:
        write_json(output / 'manifest.json', bundle)  # Also writes an empty inventory.
    print(json.dumps({'mode': 'export' if args.apply else 'inventory',
                      'source_company_id': args.company_id, 'selected': len(bundle['entries']),
                      'statuses': dict(Counter(e['status'] for e in bundle['entries']))}))
    return 1 if any(e['status'] == 'failed' for e in bundle['entries']) else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--django-root', type=Path, required=True)
    parser.add_argument('--settings', default='payvy_django.settings')
    parser.add_argument('--company-id', type=int, required=True)
    parser.add_argument('--from', dest='date_from', type=date.fromisoformat, help='inclusive invoice date YYYY-MM-DD')
    parser.add_argument('--to', dest='date_to', type=date.fromisoformat, help='inclusive invoice date YYYY-MM-DD')
    parser.add_argument('--bill-id', type=int, action='append', help='restrict to these bill IDs (repeatable)')
    parser.add_argument('--limit', type=int, default=25, help='latest 1–50 matching bills (default 25)')
    parser.add_argument('--output', type=Path, help='new private bundle directory; parent must exist')
    parser.add_argument('--apply', action='store_true', help='copy files; default inventories the database only')
    args = parser.parse_args()
    if not 1 <= args.limit <= MAX_BATCH or args.company_id <= 0:
        parser.error('company-id must be positive; limit must be 1–50')
    if args.date_from and args.date_to and args.date_from > args.date_to:
        parser.error('--from must not be later than --to')
    try:
        return export(args)
    except Exception as exc:
        # Settings/DB errors can embed connection credentials. Keep raw messages
        # out of the export log; diagnose runtime configuration on the source host.
        print(f'Export stopped ({type(exc).__name__}); check arguments and legacy runtime configuration.', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
