//! Payvy's rules, with no I/O: the command registry, bills and vendor
//! workflows. Everything here is deterministic; the server supplies `now`.

pub mod bills;
pub mod commands;
pub mod documents;
pub mod duties;
pub mod email;
pub mod extraction;
pub mod invoice_review;
pub mod pages;
pub mod permissions;
pub mod purchase_orders;
pub mod workflow;

/// Money is integer cents, always.
pub type Cents = i64;
