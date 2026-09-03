//! The reusable picker / completion window widget.
//!
//! See docs/tui-file-picker.md.
//!
//! - [`items`]: `PickerItem`, the `ItemSource` seam, `FileItemSource`.
//! - [`fuzzy`]: the frizbee-backed ranker and background worker.
//! - [`state`]: the pure `PickerState` machine.
//! - [`render`]: `render_picker` and the float layout (`compute_float_layout`).
//! - [`preview`]: the `Previewer` seam, `FilePreviewer`, and `NullPreviewer`.
//!
//! `match` is a Rust keyword, so the match layer lives in `fuzzy.rs`
//! instead of the spec's `match.rs`.

pub mod items;
pub mod fuzzy;
pub mod state;
pub mod render;
pub mod preview;
