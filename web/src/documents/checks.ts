/** What each extraction check looks at, in words. */
export const CHECK: Record<string, string> = {
  is_invoice: 'It is an invoice',
  vendor: 'Vendor',
  total: 'Total',
  invoice_number: 'Invoice number',
  invoice_date: 'Invoice date',
  invoice_date_readable: 'Invoice date',
  due_date_readable: 'Due date',
  due_after_invoice: 'Due after the invoice date',
  currency: 'Currency',
  lines_readable: 'Line items',
  lines_add_up: 'Line items add up',
  subtotal_plus_tax: 'Subtotal plus tax is the total',
  not_a_duplicate: 'Possible duplicate',
  one_invoice: 'One invoice',
};
