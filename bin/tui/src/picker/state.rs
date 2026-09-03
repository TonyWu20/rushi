//! The picker state machine (section 4.3).
//!
//! `PickerState` is a pure, crossterm-free state machine. It holds
//! the open flag, the query string, the cursor index, the visible
//! window, and the preview pane state. Tests drive it directly, like
//! `app.rs` and `browse.rs` do today.
//!
//! Keys the state machine handles:
//! - type a char → edit the query
//! - `j` / `k` or arrows → move the cursor
//! - `PgUp` / `PgDn` → page
//! - `Home` / `End` → jump
//! - `Enter` → commit
//! - `Esc` → close
//! - `Ctrl+U` / `Ctrl+D` → scroll the preview pane
//!
//! Multi-select (`Tab`) and quickfix are later adds (section 9).

/// The outcome of a picker key press.
#[derive(Debug, Clone, PartialEq)]
pub enum PickAction {
    /// The query was modified (char typed or backspaced). The caller
    /// should push the new query to the matcher.
    Query,
    /// The cursor moved (j/k, page, home, end). No query change.
    Move,
    /// The user confirmed the selection. `Some(idx)` = a result was
    /// selected; `None` = zero results, commit the raw `@query` text.
    Commit(Option<usize>),
    /// The picker closed without committing.
    Closed,
    /// The preview pane scrolled.
    ScrollPreview,
    /// The preview pane visibility toggled.
    TogglePreview,
    /// The key is not handled by the picker. The caller falls through
    /// to the editor.
    Nothing,
}

/// The picker state machine.
#[derive(Debug, Clone)]
pub struct PickerState {
    pub open: bool,
    /// The query string the user is typing after the `@`.
    pub query: String,
    /// The cursor index into the ranked snapshot.
    cursor: usize,
    /// The top visible item index (scroll window).
    top: usize,
    /// The number of visible rows for the result list.
    pub visible: usize,
    /// The preview pane's scroll offset (line index into the
    /// previewer's output).
    pub preview_scroll: usize,
    /// The user's preview-pane preference (toggled by a key, auto-hidden
    /// by `preview_cutoff` while it stays at the default).
    pub preview_shown: bool,
    /// Whether the user explicitly forced the pane on, overriding the
    /// `preview_cutoff` auto-hide. Set by the toggle key when the pane
    /// is currently hidden.
    pub preview_forced: bool,
}

impl Default for PickerState {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerState {
    /// A fresh closed picker state.
    pub fn new() -> Self {
        Self {
            open: false,
            query: String::new(),
            cursor: 0,
            top: 0,
            visible: 10,
            preview_scroll: 0,
            preview_shown: true,
            preview_forced: false,
        }
    }

    /// Open the picker with an initial query.
    pub fn open(&mut self, query: &str, visible: usize) {
        self.open = true;
        self.query = query.to_string();
        self.cursor = 0;
        self.top = 0;
        self.visible = visible.max(1);
        self.preview_scroll = 0;
        self.preview_shown = true;
        self.preview_forced = false;
    }

    /// Close the picker without committing.
    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        self.preview_forced = false;
    }

    /// The current cursor index.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The top visible item index.
    pub fn top(&self) -> usize {
        self.top
    }

    /// Type a character into the query. Resets the cursor to the top.
    pub fn type_char(&mut self, c: char) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        self.query.push(c);
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        PickAction::Query
    }

    /// Backspace from the query. If the query becomes empty the picker
    /// stays open (showing all items) but the query is empty.
    pub fn backspace(&mut self) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        self.query.pop();
        self.preview_scroll = 0;
        PickAction::Query
    }

    /// Move the cursor down by one, clamped to the item count.
    pub fn move_down(&mut self, count: usize) -> PickAction {
        if !self.open || count == 0 {
            return PickAction::Nothing;
        }
        if self.cursor < count - 1 {
            self.cursor += 1;
        }
        self.adjust_top(count);
        self.preview_scroll = 0;
        PickAction::Move
    }

    /// Move the cursor up by one.
    pub fn move_up(&mut self, count: usize) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        self.cursor = self.cursor.saturating_sub(1);
        self.adjust_top(count);
        self.preview_scroll = 0;
        PickAction::Move
    }

    /// Page down: move the cursor down by the visible window.
    pub fn page_down(&mut self, count: usize) -> PickAction {
        if !self.open || count == 0 {
            return PickAction::Nothing;
        }
        self.cursor = (self.cursor + self.visible).min(count - 1);
        self.adjust_top(count);
        self.preview_scroll = 0;
        PickAction::Move
    }

    /// Page up: move the cursor up by the visible window.
    pub fn page_up(&mut self, count: usize) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        self.cursor = self.cursor.saturating_sub(self.visible);
        self.adjust_top(count);
        self.preview_scroll = 0;
        PickAction::Move
    }

    /// Jump the cursor to the top of the list.
    pub fn go_home(&mut self) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        PickAction::Move
    }

    /// Jump the cursor to the last item.
    pub fn go_end(&mut self, count: usize) -> PickAction {
        if !self.open || count == 0 {
            return PickAction::Nothing;
        }
        self.cursor = count - 1;
        self.adjust_top(count);
        self.preview_scroll = 0;
        PickAction::Move
    }

    /// Clamp the cursor and top to the current snapshot length.
    /// Called by the renderer each frame before drawing.
    pub fn sync(&mut self, count: usize) {
        if count == 0 {
            self.cursor = 0;
            self.top = 0;
            return;
        }
        self.cursor = self.cursor.min(count - 1);
        self.adjust_top(count);
    }

    fn adjust_top(&mut self, count: usize) {
        if self.cursor >= self.top + self.visible {
            self.top = self.cursor - self.visible + 1;
        }
        if self.cursor < self.top {
            self.top = self.cursor;
        }
        let max_top = count.saturating_sub(self.visible);
        if self.top > max_top {
            self.top = max_top;
        }
    }

    /// Scroll the preview pane up by `page` lines.
    pub fn scroll_preview_up(&mut self, page: usize) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        self.preview_scroll = self.preview_scroll.saturating_sub(page);
        PickAction::ScrollPreview
    }

    /// Scroll the preview pane down by `page` lines.
    pub fn scroll_preview_down(&mut self, page: usize) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        self.preview_scroll = self.preview_scroll.saturating_add(page);
        PickAction::ScrollPreview
    }

    /// Whether the preview pane is actually visible for a given result
    /// count and cutoff. Auto-hidden below the cutoff unless the user
    /// explicitly forced it on with the toggle key.
    pub fn preview_visible(&self, count: usize, cutoff: usize) -> bool {
        self.preview_shown && (count >= cutoff || self.preview_forced)
    }

    /// Toggle the preview pane visibility. The toggle is smart: it
    /// flips the effective visibility. When the pane is currently
    /// visible it turns it off; when it is hidden (by the cutoff or a
    /// previous off) it forces it on, overriding the cutoff. This is
    /// what lets `Ctrl+P` bring the pane back after the cutoff
    /// auto-hid it.
    pub fn toggle_preview(&mut self, count: usize, cutoff: usize) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        if self.preview_visible(count, cutoff) {
            // Currently visible: turn off and drop the forced override.
            self.preview_shown = false;
            self.preview_forced = false;
        } else {
            // Hidden (by the cutoff or a previous off): force it on,
            // overriding the cutoff.
            self.preview_shown = true;
            self.preview_forced = true;
        }
        PickAction::TogglePreview
    }

    /// Handle one key. `count` is the current snapshot item count.
    /// `preview_page` is the scroll page size for Ctrl+U/D.
    /// `preview_cutoff` is the auto-hide threshold for the toggle.
    pub fn press(
        &mut self,
        key: &crate::app::Key,
        count: usize,
        preview_page: usize,
        preview_cutoff: usize,
    ) -> PickAction {
        if !self.open {
            return PickAction::Nothing;
        }
        use crate::app::Key;
        match key {
            Key::Esc => {
                self.close();
                PickAction::Closed
            }
            Key::Enter => {
                let idx = if count > 0 { Some(self.cursor.min(count - 1)) } else { None };
                self.close();
                PickAction::Commit(idx)
            }
            Key::Down | Key::Char('j') => self.move_down(count),
            Key::Up | Key::Char('k') => self.move_up(count),
            Key::PgDn => self.page_down(count),
            Key::PgUp => self.page_up(count),
            Key::Home => self.go_home(),
            Key::End => self.go_end(count),
            Key::CtrlU => self.scroll_preview_up(preview_page),
            Key::CtrlD => self.scroll_preview_down(preview_page),
            Key::CtrlP => self.toggle_preview(count, preview_cutoff),
            Key::Backspace => self.backspace(),
            Key::Char(c) => self.type_char(*c),
            _ => PickAction::Nothing,
        }
    }
}

// ── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Key;

    fn open_picker(items: usize) -> PickerState {
        let mut s = PickerState::new();
        s.open("test", items);
        s
    }

    #[test]
    fn open_sets_state() {
        let mut s = PickerState::new();
        assert!(!s.open);
        s.open("q", 5);
        assert!(s.open);
        assert_eq!(s.query, "q");
        assert_eq!(s.cursor(), 0);
        assert_eq!(s.top(), 0);
        assert_eq!(s.visible, 5);
        assert!(s.preview_shown);
    }

    #[test]
    fn close_resets_state() {
        let mut s = open_picker(10);
        s.query = "abc".into();
        s.cursor = 5;
        s.close();
        assert!(!s.open);
        assert!(s.query.is_empty());
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn type_char_resets_cursor() {
        let mut s = open_picker(10);
        s.query = "t".into();
        s.cursor = 5;
        s.top = 3;
        s.press(&Key::Char('x'), 10, 5, 4);
        assert_eq!(s.query, "tx");
        assert_eq!(s.cursor(), 0);
        assert_eq!(s.top(), 0);
    }

    #[test]
    fn move_down_clamps() {
        let mut s = open_picker(3);
        s.press(&Key::Char('j'), 3, 5, 4);
        assert_eq!(s.cursor(), 1);
        s.press(&Key::Char('j'), 3, 5, 4);
        assert_eq!(s.cursor(), 2);
        s.press(&Key::Char('j'), 3, 5, 4);
        assert_eq!(s.cursor(), 2, "clamped at last item");
    }

    #[test]
    fn move_up_from_top_stays() {
        let mut s = open_picker(3);
        s.press(&Key::Char('k'), 3, 5, 4);
        assert_eq!(s.cursor(), 0, "stays at top");
    }

    #[test]
    fn page_down_moves_by_visible() {
        let mut s = open_picker(20);
        s.visible = 10;
        s.press(&Key::PgDn, 20, 5, 4);
        assert_eq!(s.cursor(), 10);
    }

    #[test]
    fn go_end_jumps_to_last() {
        let mut s = open_picker(15);
        s.press(&Key::End, 15, 5, 4);
        assert_eq!(s.cursor(), 14);
    }

    #[test]
    fn go_home_resets_cursor() {
        let mut s = open_picker(15);
        s.cursor = 10;
        s.top = 5;
        s.press(&Key::Home, 15, 5, 4);
        assert_eq!(s.cursor(), 0);
        assert_eq!(s.top(), 0);
    }

    #[test]
    fn commit_returns_index_when_results() {
        let mut s = open_picker(5);
        s.cursor = 2;
        let action = s.press(&Key::Enter, 5, 5, 4);
        assert_eq!(action, PickAction::Commit(Some(2)));
        assert!(!s.open);
    }

    #[test]
    fn commit_with_zero_results_returns_none() {
        let mut s = open_picker(0);
        let action = s.press(&Key::Enter, 0, 5, 4);
        assert_eq!(action, PickAction::Commit(None));
        assert!(!s.open);
    }

    #[test]
    fn esc_closes_without_commit() {
        let mut s = open_picker(5);
        let action = s.press(&Key::Esc, 5, 5, 4);
        assert_eq!(action, PickAction::Closed);
        assert!(!s.open);
    }

    #[test]
    fn sync_clamps_cursor() {
        let mut s = open_picker(10);
        s.cursor = 8;
        s.sync(3);
        assert_eq!(s.cursor(), 2, "cursor clamped to last valid index");
    }

    #[test]
    fn scroll_preview() {
        let mut s = open_picker(5);
        s.preview_scroll = 0;
        s.press(&Key::CtrlD, 5, 3, 4);
        assert_eq!(s.preview_scroll, 3);
        s.press(&Key::CtrlU, 5, 3, 4);
        assert_eq!(s.preview_scroll, 0, "saturates at 0");
    }

    #[test]
    fn toggle_preview_flips_when_above_cutoff() {
        // 5 items >= cutoff 4: pane starts visible.
        let mut s = open_picker(5);
        assert!(s.preview_visible(5, 4), "above the cutoff the pane is visible");
        s.press(&Key::CtrlP, 5, 5, 4);
        assert!(!s.preview_shown, "toggle turns it off");
        assert!(!s.preview_visible(5, 4));
        s.press(&Key::CtrlP, 5, 5, 4);
        assert!(s.preview_shown, "toggle turns it back on");
        assert!(s.preview_visible(5, 4));
    }

    #[test]
    fn toggle_preview_forces_on_below_cutoff() {
        // 3 items < cutoff 4: auto-hidden even though preview_shown is on.
        let mut s = open_picker(3);
        s.preview_shown = true; // default
        s.preview_forced = false;
        assert!(!s.preview_visible(3, 4), "auto-hidden below the cutoff");
        // Ctrl+P while hidden: force the pane back on, overriding the cutoff.
        s.press(&Key::CtrlP, 3, 5, 4);
        assert!(s.preview_shown && s.preview_forced, "forced on");
        assert!(s.preview_visible(3, 4), "forced on overrides the cutoff");
    }

    #[test]
    fn backspace_shortens_query() {
        let mut s = open_picker(5);
        s.query = "abc".into();
        s.backspace();
        assert_eq!(s.query, "ab");
    }
}
