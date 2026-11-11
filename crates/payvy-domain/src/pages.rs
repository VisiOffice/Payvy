//! A document's pages: an arrangement on top of the original file, which
//! never changes. Each entry names a page of the original by its number and
//! says how it is shown: its place in the list, its rotation and whether it
//! is hidden. A fresh document is every page in order, upright.

use serde::{Deserialize, Serialize};

/// One page of the arrangement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageRef {
    /// The page number in the original, from 1.
    pub page: u32,
    /// Degrees clockwise: 0, 90, 180 or 270.
    #[serde(default)]
    pub rotation: u16,
    #[serde(default)]
    pub hidden: bool,
}

/// Every page in order, upright: version 0 of any document.
pub fn original(page_count: u32) -> Vec<PageRef> {
    (1..=page_count)
        .map(|page| PageRef {
            page,
            rotation: 0,
            hidden: false,
        })
        .collect()
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PagesError {
    #[error("page {page} doesn't exist: this document has {count} page(s)")]
    NoSuchPage { page: u32, count: u32 },
    #[error("page {0} is listed more than once")]
    Duplicate(u32),
    #[error("page {0} is missing: list every page of this document, hidden ones too")]
    Missing(u32),
    #[error("page {0} isn't in this document: it is part of another document now")]
    Elsewhere(u32),
    #[error("page {page} has rotation {rotation}; rotation is 0, 90, 180 or 270")]
    Rotation { page: u32, rotation: u16 },
    #[error("at least one page must stay visible")]
    AllHidden,
}

/// The list must name each page of the original exactly once, with a
/// quarter-turn rotation, and leave at least one page showing.
pub fn validate(pages: &[PageRef], page_count: u32) -> Result<(), PagesError> {
    let mut seen = vec![false; page_count as usize];
    for p in pages {
        if p.page == 0 || p.page > page_count {
            return Err(PagesError::NoSuchPage {
                page: p.page,
                count: page_count,
            });
        }
        let slot = &mut seen[(p.page - 1) as usize];
        if *slot {
            return Err(PagesError::Duplicate(p.page));
        }
        *slot = true;
        if !matches!(p.rotation, 0 | 90 | 180 | 270) {
            return Err(PagesError::Rotation {
                page: p.page,
                rotation: p.rotation,
            });
        }
    }
    if let Some(i) = seen.iter().position(|s| !s) {
        return Err(PagesError::Missing(i as u32 + 1));
    }
    if pages.iter().all(|p| p.hidden) {
        return Err(PagesError::AllHidden);
    }
    Ok(())
}

/// The same check for a document whose pages are `current`: a list must
/// name each of them exactly once, and nothing else. A fresh original's
/// `current` is every page of the file; a document split from it, or one
/// that had pages split off, has fewer. `page_count` is the file's.
pub fn validate_within(
    pages: &[PageRef],
    current: &[PageRef],
    page_count: u32,
) -> Result<(), PagesError> {
    for p in pages {
        if p.page == 0 || p.page > page_count {
            return Err(PagesError::NoSuchPage {
                page: p.page,
                count: page_count,
            });
        }
        if !current.iter().any(|c| c.page == p.page) {
            return Err(PagesError::Elsewhere(p.page));
        }
    }
    let mut seen = vec![false; page_count as usize];
    for p in pages {
        let slot = &mut seen[(p.page - 1) as usize];
        if *slot {
            return Err(PagesError::Duplicate(p.page));
        }
        *slot = true;
        if !matches!(p.rotation, 0 | 90 | 180 | 270) {
            return Err(PagesError::Rotation {
                page: p.page,
                rotation: p.rotation,
            });
        }
    }
    if let Some(c) = current.iter().find(|c| !seen[(c.page - 1) as usize]) {
        return Err(PagesError::Missing(c.page));
    }
    if pages.iter().all(|p| p.hidden) {
        return Err(PagesError::AllHidden);
    }
    Ok(())
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SplitError {
    #[error("name at least one group of pages to split off")]
    NoGroups,
    #[error("group {0} is empty: a new document needs at least one page")]
    EmptyGroup(usize),
    #[error("group {0} has only hidden pages: a new document needs a page to show")]
    OnlyHidden(usize),
    #[error("page {0} isn't in this document's current pages")]
    NotHere(u32),
    #[error("page {0} is in more than one group")]
    Overlap(u32),
}

/// A split worked out: what stays, and each new document's pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    /// The source's next arrangement: its pages without the moved ones, in
    /// the order they were. Empty when every page moved.
    pub remaining: Vec<PageRef>,
    /// One list per group, in the order given, each page with the rotation
    /// and visibility it had.
    pub parts: Vec<Vec<PageRef>>,
}

/// Moves `groups` (page numbers of the original) out of `current`. Each
/// page must be one of the document's current pages, in one group only,
/// and every group must keep a visible page.
pub fn split(current: &[PageRef], groups: &[Vec<u32>]) -> Result<Split, SplitError> {
    if groups.is_empty() {
        return Err(SplitError::NoGroups);
    }
    let mut moved: Vec<u32> = Vec::new();
    let mut parts = Vec::with_capacity(groups.len());
    for (i, group) in groups.iter().enumerate() {
        if group.is_empty() {
            return Err(SplitError::EmptyGroup(i + 1));
        }
        let mut part = Vec::with_capacity(group.len());
        for &page in group {
            let found = current
                .iter()
                .find(|c| c.page == page)
                .ok_or(SplitError::NotHere(page))?;
            if moved.contains(&page) {
                return Err(SplitError::Overlap(page));
            }
            moved.push(page);
            part.push(*found);
        }
        if part.iter().all(|p| p.hidden) {
            return Err(SplitError::OnlyHidden(i + 1));
        }
        parts.push(part);
    }
    let remaining = current
        .iter()
        .filter(|c| !moved.contains(&c.page))
        .copied()
        .collect();
    Ok(Split { remaining, parts })
}

/// Pages coming back from a split joined with `current`: each goes before
/// the first page with a higher number in the original, else at the end.
/// For pages that were never reordered this is the original order again.
pub fn rejoin(current: &[PageRef], returning: &[PageRef]) -> Vec<PageRef> {
    let mut out = current.to_vec();
    for r in returning {
        let at = out
            .iter()
            .position(|p| p.page > r.page)
            .unwrap_or(out.len());
        out.insert(at, *r);
    }
    out
}

/// "page 3", "pages 5–6", "pages 1–3 and 7": page numbers of the original,
/// in the order given, with runs shortened.
pub fn describe_pages(pages: &[u32]) -> String {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &p in pages {
        match runs.last_mut() {
            Some((_, end)) if p == *end + 1 => *end = p,
            _ => runs.push((p, p)),
        }
    }
    let parts: Vec<String> = runs
        .iter()
        .map(|(a, b)| {
            if a == b {
                a.to_string()
            } else {
                format!("{a}–{b}")
            }
        })
        .collect();
    let list = match parts.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    let word = if pages.len() == 1 { "page" } else { "pages" };
    format!("{word} {list}")
}

/// The name of a document split from `filename`: "waste.pdf" with pages
/// 1–3 is "waste (pages 1–3).pdf". Splitting a split document again names
/// only its new pages.
pub fn part_filename(filename: &str, pages: &[u32]) -> String {
    let (stem, ext) = match filename.rfind('.') {
        Some(i) if i > 0 => (&filename[..i], &filename[i..]),
        _ => (filename, ""),
    };
    let stem = match stem.rfind(" (page") {
        Some(i) if stem.ends_with(')') => &stem[..i],
        _ => stem,
    };
    format!("{stem} ({}){ext}", describe_pages(pages))
}

/// One difference between two arrangements, as recorded in the event.
/// Page numbers are the original's, so they stay the same whatever the order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum Change {
    /// One page moved; `to` is its new place (from 1) among `of` pages.
    Moved {
        page: u32,
        to: u32,
        of: u32,
    },
    /// More than one page moved: the new order.
    Reordered {
        order: Vec<u32>,
    },
    Rotated {
        page: u32,
        from: u16,
        to: u16,
    },
    Hidden {
        page: u32,
    },
    Restored {
        page: u32,
    },
}

/// What changed from `old` to `new`, both valid arrangements of the same
/// pages: the order first, then each page's rotation and visibility in the
/// new order. Empty when they are the same.
pub fn changes(old: &[PageRef], new: &[PageRef]) -> Vec<Change> {
    let mut out = Vec::new();
    let old_order: Vec<u32> = old.iter().map(|p| p.page).collect();
    let new_order: Vec<u32> = new.iter().map(|p| p.page).collect();
    if old_order != new_order {
        out.push(moved(&old_order, &new_order).unwrap_or(Change::Reordered {
            order: new_order.clone(),
        }));
    }
    for n in new {
        let Some(o) = old.iter().find(|o| o.page == n.page) else {
            continue;
        };
        if o.rotation != n.rotation {
            out.push(Change::Rotated {
                page: n.page,
                from: o.rotation,
                to: n.rotation,
            });
        }
        match (o.hidden, n.hidden) {
            (false, true) => out.push(Change::Hidden { page: n.page }),
            (true, false) => out.push(Change::Restored { page: n.page }),
            _ => {}
        }
    }
    out
}

/// The single page whose move turns `old` into `new`, if one does. Two
/// neighbours swapping could be either one moving; the page that moved
/// towards the front is the one named.
fn moved(old: &[u32], new: &[u32]) -> Option<Change> {
    let without = |list: &[u32], page: u32| -> Vec<u32> {
        list.iter().copied().filter(|p| *p != page).collect()
    };
    let position = |list: &[u32], page: u32| list.iter().position(|p| *p == page);
    let candidates: Vec<u32> = new
        .iter()
        .copied()
        .filter(|&p| without(old, p) == without(new, p))
        .collect();
    let page = candidates
        .iter()
        .copied()
        .find(|&p| position(new, p) < position(old, p))
        .or_else(|| candidates.first().copied())?;
    Some(Change::Moved {
        page,
        to: position(new, page)? as u32 + 1,
        of: new.len() as u32,
    })
}

/// The changes in words, for the audit log: "moved page 3 to first and hid
/// page 4". The web app words them the same way from `Change`.
pub fn describe(changes: &[Change]) -> String {
    let parts: Vec<String> = changes
        .iter()
        .map(|c| match c {
            Change::Moved { page, to, of } => {
                let place = if *to == 1 {
                    "first".to_owned()
                } else if to == of {
                    "last".to_owned()
                } else {
                    format!("position {to}")
                };
                format!("moved page {page} to {place}")
            }
            Change::Reordered { order } => {
                let order: Vec<String> = order.iter().map(u32::to_string).collect();
                format!("put the pages in the order {}", order.join(", "))
            }
            Change::Rotated { page, to: 0, .. } => format!("turned page {page} upright"),
            Change::Rotated { page, to, .. } => format!("rotated page {page} to {to}°"),
            Change::Hidden { page } => format!("hid page {page}"),
            Change::Restored { page } => format!("restored page {page}"),
        })
        .collect();
    match parts.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(page: u32) -> PageRef {
        PageRef {
            page,
            rotation: 0,
            hidden: false,
        }
    }

    #[test]
    fn a_valid_arrangement_is_an_exact_permutation() {
        let mut pages = vec![p(3), p(1), p(2)];
        assert_eq!(validate(&pages, 3), Ok(()));
        assert_eq!(
            validate(&[p(1), p(1), p(2)], 3),
            Err(PagesError::Duplicate(1))
        );
        assert_eq!(validate(&[p(1), p(2)], 3), Err(PagesError::Missing(3)));
        assert_eq!(
            validate(&[p(1), p(2), p(4)], 3),
            Err(PagesError::NoSuchPage { page: 4, count: 3 })
        );
        assert_eq!(
            validate(&[p(0), p(1), p(2)], 3),
            Err(PagesError::NoSuchPage { page: 0, count: 3 })
        );
        pages[0].rotation = 45;
        assert_eq!(
            validate(&pages, 3),
            Err(PagesError::Rotation {
                page: 3,
                rotation: 45
            })
        );
        pages[0].rotation = 270;
        for page in &mut pages {
            page.hidden = true;
        }
        assert_eq!(validate(&pages, 3), Err(PagesError::AllHidden));
        pages[1].hidden = false;
        assert_eq!(validate(&pages, 3), Ok(()));
    }

    #[test]
    fn changes_are_described_with_original_page_numbers() {
        let old = original(4);
        // Page 3 to the front.
        let new = vec![p(3), p(1), p(2), p(4)];
        let c = changes(&old, &new);
        assert_eq!(
            c,
            vec![Change::Moved {
                page: 3,
                to: 1,
                of: 4
            }]
        );
        assert_eq!(describe(&c), "moved page 3 to first");

        // Neighbours swapping: the one that went forward is named.
        let c = changes(&old, &[p(2), p(1), p(3), p(4)]);
        assert_eq!(describe(&c), "moved page 2 to first");
        let c = changes(&old, &[p(1), p(3), p(2), p(4)]);
        assert_eq!(describe(&c), "moved page 3 to position 2");
        let c = changes(&old, &[p(2), p(3), p(4), p(1)]);
        assert_eq!(describe(&c), "moved page 1 to last");

        // Two moves at once are a new order.
        let c = changes(&old, &[p(4), p(3), p(2), p(1)]);
        assert_eq!(
            c,
            vec![Change::Reordered {
                order: vec![4, 3, 2, 1]
            }]
        );

        // Rotation and visibility, with several changes in one sentence.
        let mut new = old.clone();
        new[1].rotation = 90;
        new[3].hidden = true;
        let c = changes(&old, &new);
        assert_eq!(describe(&c), "rotated page 2 to 90° and hid page 4");
        let c = changes(&new, &old);
        assert_eq!(describe(&c), "turned page 2 upright and restored page 4");

        assert!(changes(&old, &old).is_empty());
    }

    #[test]
    fn a_derived_documents_arrangement_names_only_its_pages() {
        // Pages 2 and 4 of a five-page file, the rest elsewhere.
        let current = vec![p(4), p(2)];
        assert_eq!(validate_within(&[p(2), p(4)], &current, 5), Ok(()));
        assert_eq!(
            validate_within(&[p(2), p(4), p(1)], &current, 5),
            Err(PagesError::Elsewhere(1))
        );
        assert_eq!(
            validate_within(&[p(2)], &current, 5),
            Err(PagesError::Missing(4))
        );
        assert_eq!(
            validate_within(&[p(2), p(6)], &current, 5),
            Err(PagesError::NoSuchPage { page: 6, count: 5 })
        );
        assert_eq!(
            validate_within(&[p(2), p(2), p(4)], &current, 5),
            Err(PagesError::Duplicate(2))
        );
        // For an original it is the same check as `validate`.
        assert_eq!(
            validate_within(&[p(3), p(1), p(2)], &original(3), 3),
            Ok(())
        );
        assert_eq!(
            validate_within(&[p(1), p(2)], &original(3), 3),
            Err(PagesError::Missing(3))
        );
    }

    #[test]
    fn splitting_moves_pages_out_and_rejoining_puts_them_back() {
        let mut current = original(6);
        current[4].rotation = 90;
        current[1].hidden = true;
        let s = split(&current, &[vec![5, 6], vec![3]]).unwrap();
        assert_eq!(
            s.remaining.iter().map(|p| p.page).collect::<Vec<_>>(),
            [1, 2, 4]
        );
        assert!(s.remaining[1].hidden, "page 2 stays hidden");
        assert_eq!(
            s.parts[0].iter().map(|p| p.page).collect::<Vec<_>>(),
            [5, 6]
        );
        assert_eq!(s.parts[0][0].rotation, 90, "a page keeps its rotation");
        assert_eq!(s.parts[1], vec![p(3)]);
        // In the order given.
        let s2 = split(&current, &[vec![6, 5]]).unwrap();
        assert_eq!(s2.parts[0][0].page, 6);

        assert_eq!(split(&current, &[]), Err(SplitError::NoGroups));
        assert_eq!(split(&current, &[vec![]]), Err(SplitError::EmptyGroup(1)));
        assert_eq!(split(&current, &[vec![7]]), Err(SplitError::NotHere(7)));
        assert_eq!(
            split(&current, &[vec![1, 3], vec![3]]),
            Err(SplitError::Overlap(3))
        );
        assert_eq!(split(&current, &[vec![1, 1]]), Err(SplitError::Overlap(1)));
        assert_eq!(split(&current, &[vec![2]]), Err(SplitError::OnlyHidden(1)));
        // Every page may go; the source is then empty.
        assert!(
            split(&current, &[vec![1, 2, 3], vec![4, 5, 6]])
                .unwrap()
                .remaining
                .is_empty()
        );

        // Back where they were.
        let back = rejoin(&s.remaining, &s.parts[1]);
        let back = rejoin(&back, &s.parts[0]);
        assert_eq!(back, current);
        // Into a reordered document they go before the first higher page.
        let back = rejoin(&[p(4), p(1)], &[p(2)]);
        assert_eq!(back.iter().map(|p| p.page).collect::<Vec<_>>(), [2, 4, 1]);
        let back = rejoin(&[p(1), p(2)], &[p(5)]);
        assert_eq!(back.iter().map(|p| p.page).collect::<Vec<_>>(), [1, 2, 5]);
        let back = rejoin(&[p(1), p(4)], &[p(2), p(3)]);
        assert_eq!(
            back.iter().map(|p| p.page).collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
    }

    #[test]
    fn split_documents_are_named_for_their_pages() {
        assert_eq!(
            part_filename("waste.pdf", &[1, 2, 3]),
            "waste (pages 1–3).pdf"
        );
        assert_eq!(
            part_filename("waste (pages 1–3).pdf", &[2]),
            "waste (page 2).pdf"
        );
        assert_eq!(part_filename("scan", &[4, 5]), "scan (pages 4–5)");
        assert_eq!(
            part_filename("my (draft).pdf", &[1]),
            "my (draft) (page 1).pdf"
        );
    }

    #[test]
    fn page_lists_are_described_in_runs() {
        assert_eq!(describe_pages(&[3]), "page 3");
        assert_eq!(describe_pages(&[5, 6]), "pages 5–6");
        assert_eq!(describe_pages(&[1, 2, 3, 7]), "pages 1–3 and 7");
        assert_eq!(describe_pages(&[1, 3, 5]), "pages 1, 3 and 5");
        assert_eq!(describe_pages(&[6, 5]), "pages 6 and 5");
    }

    #[test]
    fn pages_read_from_json_with_defaults() {
        let pages: Vec<PageRef> =
            serde_json::from_str(r#"[{"page":2},{"page":1,"rotation":180,"hidden":true}]"#)
                .unwrap();
        assert_eq!(pages[0], p(2));
        assert_eq!(
            pages[1],
            PageRef {
                page: 1,
                rotation: 180,
                hidden: true
            }
        );
        assert!(serde_json::from_str::<Vec<PageRef>>(r#"[{"page":1,"deleted":true}]"#).is_err());
    }
}
