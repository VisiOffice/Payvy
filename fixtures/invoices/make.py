#!/usr/bin/env python3
"""Writes the sample documents in this directory: plain one-page PDFs with
known answers in <name>.expected.json. Standard library only, so anyone can
rebuild them: python3 fixtures/invoices/make.py

The businesses are made up. To add a case, add a document below and its
expected answer; field names are those of payvy_domain::extraction::Draft,
plus line_count.
"""
import json
import pathlib

HERE = pathlib.Path(__file__).parent


def pdf(rows):
    """rows: (x, y, size, bold, text) in points from the bottom left."""
    def esc(s):
        return s.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")
    ops = []
    for x, y, size, bold, text in rows:
        ops.append(f"BT /{'F2' if bold else 'F1'} {size} Tf {x} {y} Td ({esc(text)}) Tj ET")
    stream = "\n".join(ops).encode("latin-1")
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
        b"/Resources << /Font << /F1 4 0 R /F2 5 0 R >> >> /Contents 6 0 R >>",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>",
        b"<< /Length %d >>\nstream\n" % len(stream) + stream + b"\nendstream",
    ]
    out = bytearray(b"%PDF-1.4\n")
    offsets = []
    for i, body in enumerate(objects, 1):
        offsets.append(len(out))
        out += b"%d 0 obj\n" % i + body + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objects) + 1)
    for o in offsets:
        out += b"%010d 00000 n \n" % o
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objects) + 1, xref)
    return bytes(out)


def document(head, meta, lines, totals, foot=()):
    """A typical layout: letterhead, a details block, a table, totals."""
    rows = []
    y = 740
    for i, text in enumerate(head):
        rows.append((50, y, 18 if i == 0 else 10, i == 0, text))
        y -= 22 if i == 0 else 14
    y = 640
    for label, value in meta:
        rows.append((360, y, 10, True, label))
        rows.append((470, y, 10, False, value))
        y -= 14
    y = min(y, 600) - 20
    rows.append((50, y, 10, True, "Description"))
    rows.append((470, y, 10, True, "Amount"))
    y -= 18
    for desc, amount in lines:
        rows.append((50, y, 10, False, desc))
        rows.append((470, y, 10, False, amount))
        y -= 14
    y -= 10
    for label, value in totals:
        rows.append((360, y, 10, True, label))
        rows.append((470, y, 10, label.upper().startswith(("TOTAL", "AMOUNT DUE", "BALANCE")), value))
        y -= 14
    y -= 20
    for text in foot:
        rows.append((50, y, 9, False, text))
        y -= 12
    return pdf(rows)


CASES = {
    "king-environmental-189727": (
        document(
            ["King Environmental, LLC", "12 Quarry Rd, Mesa, AZ 85201", "ar@kingenv.example"],
            [("INVOICE", ""), ("Invoice #", "189727"), ("Date", "11/01/2026"),
             ("Account", "004417"), ("Terms", "Net 30"), ("Due", "12/01/2026")],
            [("30 yd roll-off haul, 1420 Elm St", "300.00"), ("Disposal, 2.5 tons", "125.00")],
            [("Subtotal", "425.00"), ("Sales tax 8%", "34.00"), ("TOTAL DUE", "$459.00")],
            ["Please remit to King Environmental, PO Box 4410, Mesa AZ. Thank you for your business."],
        ),
        {"kind": "invoice", "vendor_name": "King Environmental", "invoice_number": "189727",
         "invoice_date": "2026-11-01", "due_date": "2026-12-01", "account_number": "004417",
         "tax_cents": 3400, "total_cents": 45900, "line_count": 2,
         "lines": [{"description":"30 yd roll-off haul, 1420 Elm St","amount_cents":30000}, {"description":"Disposal, 2.5 tons","amount_cents":12500}]},
    ),
    "desert-plumbing-5521": (
        document(
            ["Desert Plumbing & Drain", "880 W Broadway, Tempe, AZ", "(480) 555-0142"],
            [("INVOICE", ""), ("No.", "DP-5521"), ("Invoice date", "2026-10-28"),
             ("PO", "QR-2231"), ("Due date", "2026-11-12")],
            [("Service call", "95.00"), ("Replace 40 gal water heater", "1,280.00"),
             ("Haul away old unit", "45.00")],
            [("Total", "1,420.00")],
        ),
        {"kind": "invoice", "vendor_name": "Desert Plumbing & Drain", "invoice_number": "DP-5521",
         "invoice_date": "2026-10-28", "due_date": "2026-11-12", "po_number": "QR-2231",
         "tax_cents": None, "total_cents": 142000, "line_count": 3,
         "lines": [{"description":"Service call","amount_cents":9500}, {"description":"Replace 40 gal water heater","amount_cents":128000}, {"description":"Haul away old unit","amount_cents":4500}]},
    ),
    "mesa-electric-30117": (
        document(
            ["Mesa Electric Supply Co.", "3301 E Main St, Mesa, AZ"],
            [("INVOICE", ""), ("Invoice", "30117"), ("Date", "October 30, 2026"), ("Terms", "Due on receipt")],
            [("12/2 Romex, 250 ft (2)", "238.00"), ("20A breakers (10)", "112.50"),
             ("Contractor discount 10%", "-35.05")],
            [("Subtotal", "315.45"), ("Tax", "26.18"), ("Amount Due", "341.63")],
        ),
        {"kind": "invoice", "vendor_name": "Mesa Electric Supply", "invoice_number": "30117",
         "invoice_date": "2026-10-30", "tax_cents": 2618, "total_cents": 34163, "line_count": 3,
         "lines": [{"description":"12/2 Romex, 250 ft (2)","amount_cents":23800}, {"description":"20A breakers (10)","amount_cents":11250}, {"description":"Contractor discount 10%","amount_cents":-3505}]},
    ),
    "acme-supply-statement": (
        document(
            ["Acme Supply", "Statement of Account"],
            [("STATEMENT", ""), ("Statement date", "10/31/2026"), ("Account", "A-7781")],
            [("Inv 44120  09/30/2026", "512.00"), ("Inv 44388  10/14/2026", "87.25"),
             ("Payment received 10/20 - thank you", "-512.00")],
            [("Balance due", "87.25")],
            ["This is a statement, not an invoice. Please pay from your invoices."],
        ),
        {"kind": "statement", "vendor_name": "Acme Supply", "account_number": "A-7781"},
    ),
    "valley-fuel-credit-4410": (
        document(
            ["Valley Fuel Co."],
            [("CREDIT MEMO", ""), ("Credit #", "CM-4410"), ("Date", "11/03/2026"), ("Applies to", "Inv 9921")],
            [("Overbilled diesel, 40 gal @ 3.89", "-155.60")],
            [("Credit total", "(155.60)")],
        ),
        {"kind": "credit_memo", "vendor_name": "Valley Fuel", "invoice_number": "CM-4410",
         "total_cents": -15560},
    ),
}

if __name__ == "__main__":
    for name, (body, expected) in CASES.items():
        (HERE / f"{name}.pdf").write_bytes(body)
        (HERE / f"{name}.expected.json").write_text(json.dumps(expected, indent=2) + "\n")
        print(name)
