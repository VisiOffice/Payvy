# Document pages

A document's pages can be put in order, turned and hidden, and every change is on its history.
The file that arrived is never touched: an arrangement sits on top of it. Step 1 of the Document
Pages spec is reorder, rotate and hide, with history and a lock after approval. Step 2 is splitting:
a bill from some of a file's pages, the reader's suggested invoice boundaries, and agents splitting
straight away (below). Merging and assembling a new PDF on the server come later.

## The model

- **The original never changes.** It stays in storage under its content hash, so duplicate
  detection and the evidence trail are as they were. `document.get`'s download link and the web
  viewer's file are always the original.
- **An arrangement is a list of the document's pages**, in the order to show them, each
  `{ "page": 3, "rotation": 90, "hidden": false }`. `page` is the page number in the original
  file, so it means the same page whatever the order, and in whichever document it now is.
  Rotation is clockwise: 0, 90, 180 or 270. A fresh upload's pages are every page of the file; a
  document split from it, or one that had pages split off, has fewer (see Splitting).
- **Arrangements are versioned, never edited** (`document_arrangements`, migration 0016). Each
  change adds a row with the next version; the current version is the highest. A document with no
  row is at **version 0**: every page in order, upright. Existing documents need no backfill.
- **Page count.** `documents.page_count` is read from the file when it arrives: images are one
  page; a PDF is opened with `lopdf` off the async runtime and behind a panic guard, using the page
  tree's own count (what pdf.js shows) and falling back to walking the tree. A file that can't be
  read stores 0 and can't be arranged; nothing crashes. Documents from before the migration have
  no count, and are counted (and the count kept) the first time `document.get`,
  `document.arrange` or `bill.get` needs it. That is a cached fact about immutable bytes, not a
  change to the document, so it has no event.
- **A bill pins the arrangement** it was entered from (`bills.arrangement_id`; NULL is version 0),
  as it pins its workflow version.

## `document.arrange`

Routine tier, for people and agents alike. An agent's change is recorded under its name.

```json
{ "document_id": "…", "expected_version": 2,
  "pages": [{ "page": 3 }, { "page": 1, "rotation": 90 }, { "page": 2, "hidden": true }] }
```

- `expected_version` is `pages.version` from `document.get`. If someone changed the pages since,
  the command is refused (conflict) and names who made the newer version.
- `pages` must name every page of the document (its current arrangement) exactly once, and no
  other (no duplicates, none missing, none that moved to another document), with a quarter-turn
  rotation, and leave at least one page visible. The same arrangement as the current
  one is an error, as any write that changes nothing is.
- Refused for a document set aside as not a bill, and for a **locked** document (below).
- It writes the new version and a `document.arranged` event whose payload has `changes` and a
  `summary` in words. `changes` lists, with the original's page numbers:
  `{change: "moved", page, to, of}` (one page moved to place `to` of `of`),
  `{change: "reordered", order}` (several moved at once), `{change: "rotated", page, from, to}`,
  `{change: "hidden", page}` and `{change: "restored", page}`. When two neighbours swap, the page
  that moved towards the front is the one named.
- If the document is filed with a bill that is still open and unapproved, the bill's pin moves to
  the new version and the bill gets a `bill.pages_arranged` event, so its own history says so.
- It returns the document as `document.get` does.

## The lock

Approvers approved what they saw. A filed document's pages lock once its bill has **any
approval**, or is no longer open (released, paid, voided or rejected). `document.get` reports
`pages.locked` and `pages.locked_reason`: "Pages are locked after approval" once the bill has an
approval or was released or paid; "Pages are locked: the bill is void" (or rejected) when it closed
without one. A trashed document is locked too.

Rearranging pages doesn't count as entering or uploading a bill for the separation-of-duties rule
(`duties.rs` is unchanged).

## What reads return

`document.get` adds:

- `pages`: `count` (null when the file can't be read), `version`, `arrangement_id`,
  `arrangement` (the pages as shown; the original's at version 0), `locked`, `locked_reason`, and
  `versions` (every version, newest first, with who and when);
- `history`: the document's events, newest first;
- on each extraction, `out_of_date`: true when it was read under an older arrangement version than
  the current one (`extractions.arrangement_version`).

`bill.get` adds `arrangement`: the pinned version and its pages, and `bill.arrangement_id`.

## Reading (extraction)

A reading made before the latest arrangement is flagged out of date; it isn't read again on its
own, because each reading costs a model call. The model still receives the original file (server-side
assembly is step 4), but when the document is only some of its pages (split from it, or with pages
hidden or reordered) the prompt names them: "Read only pages 4, 5 of this file, in that order; the
other pages belong to other documents." Rotation doesn't reach the model, and a model can ignore
the instruction, which is why the checks still decide (a part that reads as two invoices fails
`one_invoice`). "Read again" reads with the current pages.

## Splitting (step 2)

Bill.com shows a multi-page PDF as "Pending Pages" with a checkbox per page: you tick the pages
of one bill and Enter Bill, and the rest stay in the inbox. Payvy does the same, with a command
under it.

### The model: a split document is a view of the same original

- **Nothing is copied.** A document split from another is a new `documents` row with the same
  `storage_key`, `sha256`, size, type, source, email and received time, and `derived_from`
  pointing at the document it came from (migration 0018). Lineage stays within one organization:
  the foreign key is `(org_id, derived_from)`.
- **Its pages are its own arrangement**, which lists only its pages, by their numbers in the
  original file. Its version 1 is written at the split (by whoever split it), so a split document
  never has a version 0; every other page of the file is outside it. `document.arrange` accepts only
  a reordering of those pages, the viewer and page strip load the shared original and draw only
  them, and the reader is told to read only them.
- **Page numbers stay the original's** everywhere: "page 5" means the fifth page of the file in the
  source, in the part, in events and in the reader's answer. That is why rejoining on undo can put
  pages back where they were, and why a suggested boundary can be checked against any document's
  pages without translating.
- **`pages.count` stays the file's page count** (a fact about the bytes, as in step 1); the number
  of pages a document has is the length of its arrangement. The web app shows that.
- **We chose this over copying page ranges into new PDFs** because the original must never change
  and nothing derived is stored (spec rules 1 and 6), and because it needs no PDF writer until
  step 4. The trade-off: `document.get`'s `download_url` (and `/api/documents/{id}/file`, which the
  viewer reads) is the whole original file, other invoices included, until server-side assembly.
  Both are scoped to the organization, which can see the source anyway.
- **Moved pages go away from the source**, rather than being hidden: its next arrangement version
  drops them. When no visible page is left it becomes **`split`** (a new document status) and
  leaves To review; its last arrangement keeps any hidden pages, so an undo can bring it back.
- **Duplicates.** "This exact file is already here" is by hash among files that arrived
  (`derived_from IS NULL`, a partial unique index replaces `UNIQUE (org_id, sha256)`), so split
  documents can share the hash while the same file arriving again is still refused, naming the
  document it first arrived as (even once that one is `split`).
- **Who made it.** A split document's `created_by` is whoever split it (the spec's answer to the
  separation-of-duties question: splitting creates a new document, and its creator is who split
  it). The original keeps its uploader.

### `document.split`

Routine tier, for people and agents. An agent's split takes effect at once (Robert's decision:
it moves no money and can be undone) and is recorded under its name.

```json
{ "document_id": "…", "expected_version": 1, "groups": [[1, 2, 3], [7]] }
```

- Each group becomes one new document, with its pages in the order given and with the rotation
  and hidden flag they had. It is named for its pages: `waste.pdf` → `waste (pages 1–3).pdf`.
- Refused: for a trashed, `split` or locked document (the lock as for `document.arrange`); for a
  stale `expected_version`; for no groups, an empty group, a page in two groups (or twice in one),
  a page not in the current arrangement; and for a group of only hidden pages (the new document
  would have nothing to show). A document filed with a bill (open and unapproved, so not locked)
  must keep a visible page; its bill moves to the new version with `bill.pages_arranged`.
- Events: `document.split` on the source (`parts`, `version`, `summary` "split pages 5–6 into a new
  document" or "split pages 1–3 and page 7 into 2 new documents") and `document.split_from` on each
  new document ("split this from waste.pdf (pages 5–6)").
- It returns the source as `document.get` does, plus `documents`: the new ones.
- `document.get` shows `split_from` (`{id, filename}`) and `split_into` (each part's id, filename,
  status, bill and pages); `document.list` gives each row `split_from`, and takes `status: "split"`.
- `merged_into` isn't added: step 3 (merge) can add it with its own migration.

### Undo: `document.unsplit`

`{ "document_id": "<the split-off document>" }`, routine, people and agents. Its pages go back to
the document it came from, each before the first page there with a higher number (so an unarranged
file gets its original order back), as that document's next version; a `split` source comes back to
To review. The split-off document gets an empty version and becomes `split`. Refused once it is
filed with a bill or set aside (or was already given back), and when the source is locked (filed
and approved, or trashed). Events `document.unsplit` on both ("took back pages 5–6 from …", "gave
pages 5–6 back to …").

### A bill from some pages: `bill.create` with `pages`

`bill.create` takes `pages` (page numbers of the original, in order) and `expected_version` with
`document_id`. When `pages` leaves out some of the visible pages, the same transaction splits them
into a new document (as `document.split`) and files that with the bill, which pins its arrangement
(version 1). A refused bill (a duplicate invoice number, a stale version, a page the document
doesn't have) rolls the split back with it: there is never a split left behind by a failed bill.
`pages` naming every visible page files the whole document, as without `pages`. The extraction may
be the source document's reading; corrections are recorded against it. `bill.created` records
`document_id` and `split_from`.

### The reader's suggested invoices

- The schema has `invoices`: when the file holds more than one invoice, one entry each with the
  `pages` of the file it is printed on, its `invoice_number` and `total` as printed; empty for one.
  The instructions add two sentences; the other fields are filled from the first invoice, so a
  single invoice reads exactly as before. Answers without the field (older ones, fakes) still parse.
- `extraction::invoices` (pure, tested) accepts a suggestion of two or more groups only when every
  group has pages, each page is one of the document's current pages, and no page is in two groups;
  pages are put in the document's order. Accepted groups are stored on the reading
  (`extractions.invoices`) and shown as `invoices` on each reading. Two or more invoices, or a
  suggestion that doesn't check out, fail a `one_invoice` check: such a reading isn't ready to be
  one bill as it stands.
- **A person's reading only suggests.** Document review shows "This file has 2 invoices", lists each
  (number, pages, total) with Select, and pre-ticks the first; the person confirms with Create bill.
- **An agent's reading splits straight away**, the background reader's included: `document.extract`
  by an agent on a file that arrived splits it into one document per invoice, in the same
  transaction, under the agent's name. Pages in no group (a statement or remittance page) stay in
  the source; when none are left it is `split`. The result carries `split.to_read`; the background
  reader reads each new document straight after, in the same pass, and an agent calling the API
  reads them itself (or the reader picks them up within its 30-second pass). If the split is
  refused (say a group of hidden pages), the reading stands and `split.error` says why.
- **It can't loop**: only a document with no `derived_from` is split this way, so a part, whatever
  its reading says, is never split again; its suggestion is only shown to a person. The source's
  own reading is out of date after the split and isn't read again on its own.

### Web

- Document review: the page strip has a checkbox per visible page ("Attach page 3"), with All and
  None and "2 of 4 selected". With some but not all pages ticked, the button reads "Create bill
  from 2 pages" and runs `bill.create` with `pages`; with all ticked it is Create bill, as before;
  with none it is disabled ("Tick the pages for this bill"). Hidden pages have no checkbox.
- "This file has N invoices" lists the reader's groups still on the document, pre-ticks the first
  and fills its number and total; Select picks another.
- A split document shows "Split from X" with a link to the rest and Undo; the source lists "Split
  into"; the page history includes splits and undos. The documents list shows "Split from X" with
  Undo on each part still in To review. It all works at phone width (ticks in the Original document
  tab, Create bill in Invoice details).

## Web

- **Document review** shows a strip of page thumbnails above the original, drawn with pdf.js from
  the original file. Each page can move earlier or later, turn a quarter clockwise, or be hidden;
  hidden pages show greyed with Restore. Every control is a labelled button ("Move page 3
  earlier"), so it works from the keyboard, and focus stays on the control after the save. Pages
  can also be dragged on desktop. Each change saves straight away through `document.arrange`; if
  someone else changed the pages first, the error is shown and the pages reload.
- **Page history** in the invoice details lists the last five changes in words: "Robert moved page
  3 to first", "claude-ap hid page 2".
- **Out of date:** when the latest reading predates the arrangement, a note says "Read before the
  pages were rearranged" with Read again.
- **Locked:** the controls are disabled and the reason is shown.
- **The viewers** on document review and on the bill page show the arrangement: in order, turned,
  hidden pages left out, with the highlights of what was read turned with their page. The bill
  page shows the arrangement the bill pinned. All of it works at phone width, including inside the
  "Original document" tab.
- **One page at a time on document review.** The viewer shows one page of the arrangement, fitted
  to the panel ("Fit page"; Fit width and zoom are still there), with Previous and Next, "Page 3 of
  22" (announced as it changes), and the keys ← → or J K, shown beside the counter and on the
  buttons' tooltips. The keys are ignored while typing (any field but a tick box), in a menu, list
  or radio group, while a dialog is open, and with Ctrl, ⌘, Alt or Shift held. Focusing a form
  field turns to the page its value was read from. A swipe turns the page on a touch screen when it
  fits the width. The toolbar has the page's own "Attach page N" tick, so pages can be ticked while
  flipping through. A one-page document (a photo) has no page controls.
- **The thumbnails can be hidden** (Show/Hide thumbnails). Hidden, the strip is one row: the page
  count, "N of M selected", All and None. The choice is kept in the browser (`localStorage`
  `payvy.review.thumbnails`); until someone chooses, they show for two to six pages and are hidden
  otherwise. Clicking a thumbnail shows that page; the page shown is outlined.
- **The header is one row** over the invoice details (back link, title, file name, status), so on
  a desktop the original starts at the top of the window. The bill page keeps every page in a
  column.

## Later steps

3. `document.merge` (with `merged_into`, and refusing a document already merged), and
   `bill.add_pages` for supporting pages after approval. An explicit Split button on document
   review, for statements, can call `document.split` (agents already can).
4. Server-side PDF assembly for downloads, the viewer's file and the reader, so a split document's
   download is only its pages and rotation reaches the model.
