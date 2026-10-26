//! Views: the mission board, the diff review, the memory graph, and the black hole.
//!
//! Each view is split in two:
//!   * view-model logic lives in the `stoic-codex-ui-core` crate (pure, unit-tested, no terminal
//!     types — see `stoic-codex/ui-core`);
//!   * these modules only *draw* that model with ratatui, using `crate::theme` for colour
//!     and `crate::chrome` for the frame around them.
//!
//! That split is why the layout rules (hunk pairing, fold counts, graph ranks, the boss
//! staying on the top row) are covered by tests even though a terminal cannot be asserted
//! against in CI.
//!
//! Two house rules every view follows, because they are what makes the screen calm:
//!
//! 1. **No boxes.** Separation is a hairline or a blank line, never a border char drawn
//!    around a panel. Borders eat four cells and add nothing a rule cannot say.
//! 2. **One red thing per region.** The selected row, the active tab, the failing state —
//!    pick one. Anything else is grey, and grey is what makes the red mean something.

pub mod blackhole;
pub mod diff_review;
pub mod help;
pub mod memory_graph;
pub mod mission_board;
pub mod welcome;

pub(crate) use crate::chrome::fit;
