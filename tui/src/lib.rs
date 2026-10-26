//! Stoic Codex TUI views (standalone crate wrapper).
//!
//! In the fork these modules live inside `codex-rs/tui` as `mod views;` — see
//! `stoic-codex/tui/WORKLOG.md`. This crate exists so the views can be type-checked in
//! isolation, which is how CI should check them before paying for a full workspace build.
//!
//! Four pieces:
//!
//! * [`app`] — terminal lifecycle, live chat streaming, mission event loop and reducer.
//! * [`theme`] — the palette: **red and black first, everything else is punctuation**.
//! * [`chrome`] — the frame every screen shares: mark, tabs, one rule, one footer.
//! * [`views`] — the mission board, the diff review and the memory graph.
#[path = "../views/mod.rs"]
pub mod views;

pub mod app;
pub mod chrome;
pub mod theme;
