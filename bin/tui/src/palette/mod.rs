//! The `:` command palette (docs/tui-command-palette.md).
//!
//! - [`state`]: `PaletteState`, the pure state machine.
//! - [`items`]: `PaletteItem`, `CmdKind`, `CmdOption`, built-in registration.
//! - [`preview`]: `CommandPreviewer`, renders help / options / session metadata.
//! - [`render`]: `render_palette`, the two-pane float body.
//!
//! Fuzzy ranking reuses `crate::picker::fuzzy::rank_fuzzy` (section 11:
//! "reuse `picker/fuzzy.rs`, no new ranker").

pub mod items;
pub mod preview;
pub mod render;
pub mod state;
