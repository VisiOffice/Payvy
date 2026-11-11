import { test, expect } from '@playwright/test';
import { agent, vendor } from './api';
import { pdf } from './pdf';

async function enabled(page: import('@playwright/test').Page, on: boolean) {
  await page.goto('/settings/company');
  const button = page.getByRole('button',{name:on ? 'Enable invoice line review' : 'Turn off new line reviews'});
  await expect(page.getByRole('button',{name:/Enable invoice line review|Turn off new line reviews/})).toBeVisible();
  if (await button.count()) await button.click();
  await expect(page.getByRole('button',{name:on ? 'Turn off new line reviews' : 'Enable invoice line review'})).toBeVisible();
}

test.afterEach(async ({page}) => { await enabled(page,false); });

test('discount lines can be corrected, saved, reloaded and carried to the bill', async ({page}) => {
  await enabled(page,true);
  {
    // A unique vendor exercises the unresolved-vendor path: lines must still show.
    const id = await vendor('Line review electric');
    const doc = await agent<{id:string}>('document.upload',{filename:'line-review.pdf',content_base64:pdf(['Invoice 30117','Line review pilot','12/2 Romex, 250 ft (2) 238.00','20A breakers (10) 112.50','Contractor discount 10% -35.05','Tax 26.18','Total 341.63']).toString('base64')});
    await agent('document.extract',{document_id:doc.id});
    await page.goto(`/documents/${doc.id}`);
    await expect(page.getByLabel('Line amount 3',{exact:true})).toHaveValue('-35.05');
    await page.getByRole('button',{name:/^Vendor/}).click();
    await page.getByRole('option',{name:'Line review electric',exact:true}).click();
    await page.getByRole('combobox',{name:'Tax treatment',exact:true}).click();
    await page.getByRole('option',{name:'Add tax once to the rows',exact:true}).click();
    await page.getByRole('textbox',{name:/^Amount/}).fill('342.00');
    await expect(page.getByLabel('Line amount 3',{exact:true})).toHaveValue('-35.05');
    await expect(page.getByText('Difference: $0.37',{exact:true})).toBeVisible();
    await page.getByRole('textbox',{name:/^Amount/}).fill('341.63');
    await page.getByRole('checkbox',{name:'I checked the invoice details, currency and all included pages against the original'}).check();
    await page.getByRole('button',{name:'Save review',exact:true}).click();
    await expect(page.getByRole('button',{name:'Create bill',exact:true})).toBeEnabled();
    await page.reload();
    await expect(page.getByLabel('Line amount 3',{exact:true})).toHaveValue('-35.05');
    await expect(page.getByRole('button',{name:'Create bill',exact:true})).toBeEnabled();
    // A new model read cannot discard a saved correction.
    await page.getByLabel('Description 1',{exact:true}).fill('Cable for workshop');
    await page.getByRole('button',{name:'Save review',exact:true}).click();
    await expect(page.getByText('Saved · revision 2',{exact:true})).toBeVisible();
    await page.locator('.payvy-review-row').first().scrollIntoViewIfNeeded();
    await page.screenshot({path:'/tmp/payvy-invoice-review-desktop.png'});
    await page.setViewportSize({width:390,height:844});
    await page.locator('.payvy-review-row').first().scrollIntoViewIfNeeded();
    await expect(page.getByLabel('Line amount 3',{exact:true})).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await page.screenshot({path:'/tmp/payvy-invoice-review-mobile.png'});
    await page.setViewportSize({width:1440,height:900});
    await page.getByRole('button',{name:'Read again',exact:true}).last().click();
    await expect(page.getByText('A newer reading is available',{exact:true})).toBeVisible();
    await expect(page.getByLabel('Description 1',{exact:true})).toHaveValue('Cable for workshop');
    await page.getByRole('button',{name:'Create bill',exact:true}).click();
    await expect(page).toHaveURL(/\/bills\//);
    await expect(page.getByText('Cable for workshop',{exact:true})).toBeVisible();
    await expect(page.getByText('-$35.05',{exact:true})).toBeVisible();
    await expect(page.getByText('Invoice review · revision 2',{exact:true})).toBeVisible();
    const billId = page.url().split('/').pop()!;
    const saved=await agent<{bill:{vendor_id:string;amount_cents:number};lines:{amount_cents:number}[]}>('bill.get',{bill_id:billId});
    expect(saved.bill.vendor_id).toBe(id); expect(saved.bill.amount_cents).toBe(34163); expect(saved.lines.map(l=>l.amount_cents)).toEqual([23800,11250,-3505,2618]);
  }
});

test('summary-only is an explicit person decision and editing invalidates it', async ({page}) => {
  await enabled(page,true);
  {
    await vendor('Summary review vendor');
    const doc=await agent<{id:string}>('document.upload',{filename:'summary-review.pdf',content_base64:pdf(['Sonoran Pest Control','Invoice SR-7781','summary-review-test','Total 185.00']).toString('base64')});
    await agent('document.extract',{document_id:doc.id});
    await page.goto(`/documents/${doc.id}`);
    await page.getByRole('button',{name:/^Vendor/}).click();
    await page.getByRole('option',{name:'Summary review vendor',exact:true}).click();
    await expect(page.getByText('No line items captured',{exact:true})).toBeVisible();
    await page.getByRole('checkbox',{name:'I checked the invoice details, currency and all included pages against the original'}).check();
    await page.getByRole('button',{name:'Save review',exact:true}).click();
    await expect(page.getByRole('button',{name:'Create bill',exact:true})).toBeDisabled();
    await page.getByRole('button',{name:'Use summary-only bill…',exact:true}).click();
    await page.getByRole('textbox',{name:/^Summary-only reason/}).fill('This invoice only prints a total');
    await page.getByRole('button',{name:'Accept summary-only',exact:true}).click();
    await expect(page.getByRole('button',{name:'Create bill',exact:true})).toBeEnabled();
    await page.getByRole('textbox',{name:/^Amount/}).fill('186.00');
    await expect(page.getByRole('button',{name:'Create bill',exact:true})).toBeDisabled();
    await page.getByRole('button',{name:'Save review',exact:true}).click();
    await expect(page.getByText('Saved · revision 3',{exact:true})).toBeVisible();
    await page.reload();
    await expect(page.getByRole('button',{name:'Create bill',exact:true})).toBeDisabled();
  }
});
