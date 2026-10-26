//! Zenithal TUI views (standalone crate wrapper).
//!
//! In the fork these modules live inside `codex-rs/tui` as `mod views;` — see
//! `zenithal/tui/WORKLOG.md`. This crate exists so the views can be type-checked in
//! isolation, which is how CI should check them before paying for a full workspace build.
//!
//! Three pieces:
//!
//! * [`theme`] — the palette: **red and black first, everything else is punctuation**.
//! * [`chrome`] — the frame every screen shares: mark, tabs, one rule, one footer.
//! * [`views`] — the mission board, the diff review and the memory graph.
#[path = "../views/mod.rs"]
pub mod views;

pub mod chrome;
pub mod theme;
