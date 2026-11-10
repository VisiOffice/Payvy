# Reading bills from documents

Every document that lands in the inbox, by upload or by email, can be read by a model: vendor,
invoice number, dates, line items, tax and total. Payvy checks what the model says, matches it to a
vendor on file and keeps it as a draft. Nothing becomes a bill until someone calls `bill.create`
with it.

## What happens

1. **The model reads the page.** It fills in one fixed schema and copies amounts exactly as
   printed ("1,234.56"), with no arithmetic. The same schema goes to every provider.
2. **Payvy checks it.** Amounts become cents, or the field fails if they can't be read exactly. The
   lines must add up to the total, with or without the tax. Subtotal plus tax must equal the total.
   The due date can't be before the invoice date. The document must be an invoice, not a statement,
   credit memo, receipt or quote, and in US dollars.
3. **Payvy matches the vendor.** An account number on file is the strongest match, then the email
   domain (free mail like gmail.com doesn't count), then the name, ignoring case, punctuation and
   suffixes like LLC. If two vendors fit equally well, a person chooses. If the vendor already has
   a bill with this invoice number, the draft is flagged as a duplicate.
4. **The result is kept** on the document (`document.get` → `extractions`). It has every check with
   its reason, `ready` (true when every check passed) and `bill_input`: the exact `bill.create`
   input it suggests. That input includes the lines when they can carry the total, with tax as its
   own line. If no vendor matched, `new_vendor` suggests one to create.
5. **Someone enters it.** A person or an agent calls `bill.create` with `bill_input`, as it is or
   corrected. The `bill.created` event records the extraction and which fields were corrected, so
   over time you can see how often each model is right. From there the bill follows its vendor's
   workflow like any other, so people still approve it.

With `PAYVY_EXTRACT_AUTO` on (the default), new inbox documents are read in the background by the
agent `extractor` within about 30 seconds. A failed read is recorded on the document with its error
and tried again after 15 minutes. After three failures, it waits for a person. Agents can also call
`document.extract` themselves; it refuses a document that was already read unless `again` is true.

## Choosing a model

Set `PAYVY_EXTRACT_PROVIDER` and its key. Two backends cover nearly everything:

| Provider | Settings |
|---|---|
| Claude (default model `claude-opus-5-5`) | `PAYVY_EXTRACT_PROVIDER=anthropic`, `PAYVY_EXTRACT_API_KEY` |
| OpenAI | `openai-compatible`, key, `PAYVY_EXTRACT_MODEL` |
| Gemini | `openai-compatible`, `PAYVY_EXTRACT_BASE_URL=https://generativelanguage.googleapis.com/v1beta/openai`, key, model |
| Grok | `openai-compatible`, `PAYVY_EXTRACT_BASE_URL=https://api.x.ai/v1`, key, model |
| OpenRouter (most models) | `openai-compatible`, `PAYVY_EXTRACT_BASE_URL=https://openrouter.ai/api/v1`, key, model |
| A model on your own server | `openai-compatible`, `PAYVY_EXTRACT_BASE_URL=http://127.0.0.1:11434/v1` (Ollama) or your vLLM URL, model, no key |

A local model keeps every invoice on your own hardware. It has to accept images or PDFs. PDFs are
sent as a `file` part, which not every OpenAI-compatible server supports yet. TIFF scans aren't sent
to any provider; convert them to PDF.

Smaller models make more mistakes. The checks catch most of them, so you'll see more drafts that
aren't ready, not more wrong bills.

## Measuring a model

`fixtures/invoices` holds sample documents with known answers. They include an invoice with tax, one
without, one with a discount line, a statement and a credit memo. Score the configured model
against them:

```bash
PAYVY_EXTRACT_PROVIDER=anthropic PAYVY_EXTRACT_API_KEY=... cargo run -- extract-eval fixtures/invoices
```

This sends each document to the model, so it costs a few requests. It prints the fields it got right
and wrong for each document, and exits non-zero if any field is wrong. To add a case, add a
document and its expected answer to `fixtures/invoices/make.py` and run it.

## What isn't done yet

- Re-matching the vendor after you create one without reading the document again (for now,
  `again: true`, or enter the bill from the draft yourself).
- Multi-page statements split into their invoices, and currencies other than US dollars.
- Learning a vendor's layout from corrections. The corrections are recorded, so this can come later.
