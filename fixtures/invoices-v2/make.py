"""Synthetic contract fixtures, NOT human-labeled accuracy data. python3 make.py"""
from pathlib import Path
import json
from reportlab.pdfgen import canvas
from reportlab.lib.pagesizes import letter
HERE = Path(__file__).parent
cases = [
 ('fractional', 'V2-100', [('Disposal', '2.5', 'ton', '12.345', '30.86'), ('Included visit', '1', 'visit', '0.00', '0.00')], '30.86'),
 ('discount', 'V2-101', [('Rental', '2', 'day', '50.00', '100.00'), ('Discount', None, None, None, '-10.00')], '90.00'),
 ('continuation', 'V2-102', [('Haul', '1', 'trip', '100.00', '100.00'), ('Haul', '1', 'trip', '100.00', '100.00')], '200.00'),
]
for name, number, lines, total in cases:
    c = canvas.Canvas(str(HERE / f'{name}.pdf'), pagesize=letter, invariant=1)
    def header(page):
        c.setFont('Helvetica-Bold',18); c.drawString(45,740,'Synthetic Services')
        c.setFont('Helvetica',11)
        for i, text in enumerate([f'Invoice {number}', 'Date: 2026-11-11', 'Currency: USD', f'Page {page}']):
            c.drawString(45,710-18*i,text)
        c.setFont('Helvetica-Bold',10)
        for x,text in zip([45,260,320,390,490],['Description','Quantity','Unit','Unit price','Amount']):c.drawString(x,610,text)
        c.setFont('Helvetica',11)
    header(1)
    rows=[]
    for i,(desc,qty,unit,rate,amount) in enumerate(lines):
        page=1
        if name=='continuation' and i:
            c.showPage(); header(2); page=2
        y=580 if name=='continuation' else 580-i*24
        for x,text in zip([45,260,320,390,490],[desc,qty,unit,rate,amount]):
            if text is not None:c.drawString(x,y,text)
        details={k:None for k in ['quantity','unit','unit_price','tax_amount','tax_rate','item_code','service_start','service_end']}
        details.update(quantity=qty,unit=unit,unit_price=rate)
        rows.append({'description':desc,'amount':amount,'details':details})
    c.setFont('Helvetica-Bold',12);c.drawString(390,450,'Invoice total');c.drawString(490,450,total)
    c.setFont('Helvetica',9);c.drawString(45,70,'SYNTHETIC TEST FIXTURE - no real supplier or payable')
    c.save()
    (HERE/f'{name}.expected.json').write_text(json.dumps({'schema_version':2,'pages':[1,2] if name=='continuation' else [1], 'expected':{'invoice_number':number,'total':total,'currency':'USD','lines':rows}},indent=2)+'\n')

# Image-only counterpart exercises the local OCR fallback. Requires Poppler.
import subprocess
subprocess.run(['pdftoppm','-singlefile','-scale-to','900','-png',str(HERE/'fractional.pdf'),str(HERE/'scanned')],check=True)
(HERE/'scanned.expected.json').write_text((HERE/'fractional.expected.json').read_text())
