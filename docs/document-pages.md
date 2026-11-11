# Document pages

A document's pages can be put in order, turned and hidden, and every change is on its history.
The file that arrived is never touched: an arrangement sits on top of it. This is step 1 of the
Document Pages spec (reorder, rotate, hide, with history and a lock after approval). Splitting,
merging and assembling a new PDF on the server come later.

## The model

- **The original never changes.** It stays in storage under its content hash, so duplicate
  detection and the evidence trail are as they were. `document.get`'s download link and the web
  viewer's file are always the original.
- **An arrangement is a list of every page of the original**, in the order to show them, each
  `{ "page": 3, "rotation": 90, "hidden": false }`. `page` is the page number in the original, so
  it means the same page whatever the order. Rotation is clockwise: 0, 90, 180 or 270.
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
- `pages` must name every page of the original exactly once (no duplicates, none missing), with a
  quarter-turn rotation, and leave at least one page visible. The same arrangement as the current
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
own, because each reading costs a model call. **Limitation in step 1:** the model still receives the
original file, so hidden pages are still read and rotation and order don't reach it. "Read again"
brings the reading up to date with the version, not a different input. Assembling the arranged PDF
for the reader (and for downloads) is step 4.

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

## Later steps

2. `document.split` (agents' suggested splits take effect at once), reading each new document.
3. `document.merge`.
4. Server-side PDF assembly for downloads and for the reader, so hidden pages, order and rotation
   reach the model.
