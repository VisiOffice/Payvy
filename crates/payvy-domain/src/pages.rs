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
    #[error("page {0} is missing: list every page of the original, hidden ones too")]
    Missing(u32),
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
