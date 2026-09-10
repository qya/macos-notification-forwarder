//! Notification processing pipeline: Parser → Filter → Dedup → Dispatcher
//! (PRD §4, §§7, 14–16).
//!
//! This crate is intentionally pure: it never touches the Accessibility API
//! directly. The `nf-accessibility` crate extracts raw banner fields and hands
//! them here for parsing, so the core logic stays unit-testable (AT-02…AT-04,
//! AT-08).

pub mod dedup;
pub mod filter;
pub mod model;
pub mod parser;

pub use dedup::DedupCache;
pub use filter::AppFilter;
pub use model::Notification;
pub use parser::{parse_banner_fields, BannerFields};
