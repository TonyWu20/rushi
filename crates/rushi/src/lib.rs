//! `rushi-common` — the shared utility crate for the agent harness.
//!
//! Modules:
//! - `logline` — the one `LogLine` type (FT-005 atomic append).
//! - `event_validation` — the one schema validator.
//! - `compact_math` — pure trigger math and cut walk.
//! - `stage` — the `StageRunner` trait and payload types.
//! - `hooks` — lifecycle-window dispatcher and decision types.
//! - `rewind` — the active-path computation over rewind events
//!   (docs/rewind-fork-design.md section 3).
//! - `model_settings` — shared model-section resolution and defaults
//!   (docs/itches.md: the "hard copies" itch).

pub mod logline;
pub mod event_validation;
pub mod compact_math;
pub mod stage;
pub mod hooks;
pub mod rewind;
pub mod model_settings;
