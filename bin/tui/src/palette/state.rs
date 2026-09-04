//! The command-palette state machine (docs/tui-command-palette.md section 11).
//!
//! `PaletteState` is a pure, crossterm-free state machine. It holds the
//! open flag, the query string, the sub-stage (`Root` or `SessionList`),
//! the cursor index, the visible window, and the preview pane state.
//!
//! Keys handled by the state machine:
//! - `j` / `k` or `Ctrl+J` / `Ctrl+K` → move cursor (or option cursor
//!   when a Set item is highlighted)
//! - `Down` / `Up` → same as j/k
//! - other printable chars + `Backspace` → edit the query
//! - `PgUp` / `PgDn` → page
//! - `Home` / `End` → jump
//! - `Enter` → commit the highlighted item
//! - `Esc` → close (Root) or drop the sub-stage (SessionList)
//! - `Ctrl+U` / `Ctrl+D` → scroll the preview pane
//! - `Ctrl+P` → toggle the preview pane

/// The sub-stage of the palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteStage {
    /// The root command list.
    Root,
    /// The session buffer sub-list (entered via the `b` Goto item).
    SessionList,
}

/// The outcome of a palette key press.
#[derive(Debug, Clone, PartialEq)]
pub enum PaletteAction {
    /// The query was modified (char typed or backspaced). The caller
    /// should re-rank the item list.
    Query,
    /// The list cursor moved. No query change.
    Move,
    /// The option cursor moved (Set item options). No list change.
    OptionMove,
    /// The user confirmed the highlighted item. The caller reads the
    /// cursor and stage to decide the commit action.
    Commit,
    /// `Esc` in `SessionList` stage: drop back to `Root`, keep the
    /// palette open, reset the query.
    DropSubStage,
    /// `Esc` in `Root` stage: close the palette.
    Closed,
    /// The preview pane scrolled.
    ScrollPreview,
    /// The preview pane visibility toggled.
    TogglePreview,
    /// The key is not handled by the palette. The caller decides
    /// whether to fall through.
    Nothing,
}

/// The command-palette state machine.
#[derive(Debug, Clone)]
pub struct PaletteState {
    pub open: bool,
    /// The query string the user is typing after `:`.
    pub query: String,
    /// The current sub-stage.
    pub stage: PaletteStage,
    /// When entering `SessionList`, this records the query length at
    /// that moment. The session filter is `query[goto_prefix_len..]`.
    pub goto_prefix_len: usize,
    /// The cursor index into the ranked list.
    cursor: usize,
    /// The top visible item index (scroll window).
    top: usize,
    /// The number of visible rows for the result list.
    pub visible: usize,
    /// The preview pane's scroll offset (line index).
    pub preview_scroll: usize,
    /// The user's preview-pane preference (toggled by Ctrl+P).
    pub preview_shown: bool,
    /// Whether the user explicitly forced the pane on, overriding the
    /// auto-hide cutoff.
    pub preview_forced: bool,
    /// The option cursor index for Set items with options.
    pub option_cursor: usize,
}

impl Default for PaletteState {
    fn default() -> Self {
        Self::new()
    }
}

impl PaletteState {
    /// A fresh closed palette state.
    pub fn new() -> Self {
        Self {
            open: false,
            query: String::new(),
            stage: PaletteStage::Root,
            goto_prefix_len: 0,
            cursor: 0,
            top: 0,
            visible: 10,
            preview_scroll: 0,
            preview_shown: true,
            preview_forced: false,
            option_cursor: 0,
        }
    }

    /// Open the palette with a blank query and the given visible row
    /// count. Resets the stage to `Root`.
    pub fn open(&mut self, visible: usize) {
        self.open = true;
        self.query.clear();
        self.stage = PaletteStage::Root;
        self.goto_prefix_len = 0;
        self.cursor = 0;
        self.top = 0;
        self.visible = visible.max(1);
        self.preview_scroll = 0;
        self.preview_shown = true;
        self.preview_forced = false;
        self.option_cursor = 0;
    }

    /// Close the palette, resetting all state.
    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.stage = PaletteStage::Root;
        self.goto_prefix_len = 0;
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        self.preview_forced = false;
        self.option_cursor = 0;
    }

    /// The current cursor index.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The top visible item index.
    pub fn top(&self) -> usize {
        self.top
    }

    /// Enter the `SessionList` sub-stage. Records the current query
    /// length as the goto prefix so the filter is everything typed
    /// after the Goto item was committed.
    pub fn goto_session_list(&mut self) {
        self.stage = PaletteStage::SessionList;
        self.goto_prefix_len = self.query.len();
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        self.option_cursor = 0;
    }

    /// Drop back to `Root` from `SessionList`, clearing the query.
    pub fn drop_sub_stage(&mut self) {
        self.stage = PaletteStage::Root;
        self.query.clear();
        self.goto_prefix_len = 0;
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        self.option_cursor = 0;
    }

    /// The effective filter query for the current stage. In
    /// `SessionList`, the filter is everything typed after the goto
    /// commit point, with leading whitespace trimmed so a natural
    /// `b <name>` typing pattern still fuzzy-matches.
    pub fn filter_query(&self) -> &str {
        match self.stage {
            PaletteStage::Root => &self.query,
            PaletteStage::SessionList => {
                let p = self.goto_prefix_len.min(self.query.len());
                self.query[p..].trim_start_matches(' ')
            }
        }
    }

    /// Type a character into the query. Resets the cursor and option
    /// cursor to the top.
    pub fn type_char(&mut self, c: char) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        self.query.push(c);
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Query
    }

    /// Backspace from the query. In `SessionList` stage, if the query
    /// would go below the goto prefix, the sub-stage is dropped.
    pub fn backspace(&mut self) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        if self.stage == PaletteStage::SessionList
            && self.query.len() <= self.goto_prefix_len
        {
            self.drop_sub_stage();
            return PaletteAction::DropSubStage;
        }
        self.query.pop();
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Query
    }

    /// Move the list cursor down by one, clamped. If the highlighted
    /// item has options, move the option cursor instead.
    pub fn move_down(&mut self, count: usize, highlighted_options: usize) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        if highlighted_options > 0 {
            self.option_cursor = (self.option_cursor + 1) % highlighted_options;
            return PaletteAction::OptionMove;
        }
        if count == 0 {
            return PaletteAction::Nothing;
        }
        if self.cursor < count - 1 {
            self.cursor += 1;
        }
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Move
    }

    /// Move the list cursor up by one, clamped. If the highlighted
    /// item has options, move the option cursor instead.
    pub fn move_up(&mut self, highlighted_options: usize) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        if highlighted_options > 0 {
            if self.option_cursor == 0 {
                self.option_cursor = highlighted_options - 1;
            } else {
                self.option_cursor -= 1;
            }
            return PaletteAction::OptionMove;
        }
        self.cursor = self.cursor.saturating_sub(1);
        self.adjust_top_count();
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Move
    }

    /// Page down: move the cursor down by the visible window.
    pub fn page_down(&mut self, count: usize) -> PaletteAction {
        if !self.open || count == 0 {
            return PaletteAction::Nothing;
        }
        self.cursor = (self.cursor + self.visible).min(count - 1);
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Move
    }

    /// Page up: move the cursor up by the visible window.
    pub fn page_up(&mut self, count: usize) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        self.cursor = self.cursor.saturating_sub(self.visible);
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Move
    }

    /// Jump the cursor to the top of the list.
    pub fn go_home(&mut self) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        self.cursor = 0;
        self.top = 0;
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Move
    }

    /// Jump the cursor to the last item.
    pub fn go_end(&mut self, count: usize) -> PaletteAction {
        if !self.open || count == 0 {
            return PaletteAction::Nothing;
        }
        self.cursor = count - 1;
        self.adjust_top(count);
        self.preview_scroll = 0;
        self.option_cursor = 0;
        PaletteAction::Move
    }

    /// Clamp the cursor and top to the current list length.
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

    /// Adjust top without a count (for move_up where count is unknown
    /// to the state machine).
    fn adjust_top_count(&mut self) {
        // Without the count, just make sure top does not exceed
        // cursor. The renderer will call `sync` to clamp.
        if self.cursor < self.top {
            self.top = self.cursor;
        }
    }

    /// Scroll the preview pane up by `page` lines.
    pub fn scroll_preview_up(&mut self, page: usize) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        self.preview_scroll = self.preview_scroll.saturating_sub(page);
        PaletteAction::ScrollPreview
    }

    /// Scroll the preview pane down by `page` lines.
    pub fn scroll_preview_down(&mut self, page: usize) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        self.preview_scroll = self.preview_scroll.saturating_add(page);
        PaletteAction::ScrollPreview
    }

    /// Whether the preview pane is actually visible for a given result
    /// count and cutoff.
    pub fn preview_visible(&self, count: usize, cutoff: usize) -> bool {
        self.preview_shown && (count >= cutoff || self.preview_forced)
    }

    /// Toggle the preview pane visibility.
    pub fn toggle_preview(&mut self, count: usize, cutoff: usize) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        if self.preview_visible(count, cutoff) {
            self.preview_shown = false;
            self.preview_forced = false;
        } else {
            self.preview_shown = true;
            self.preview_forced = true;
        }
        PaletteAction::TogglePreview
    }

    /// Handle one key. `count` is the current ranked item count.
    /// `highlighted_options` is the option count of the item at the
    /// current cursor (0 for non-Set items). `preview_page` and
    /// `preview_cutoff` mirror the picker constants.
    pub fn press(
        &mut self,
        key: &crate::app::Key,
        count: usize,
        highlighted_options: usize,
        preview_page: usize,
        preview_cutoff: usize,
    ) -> PaletteAction {
        if !self.open {
            return PaletteAction::Nothing;
        }
        use crate::app::Key;
        match key {
            Key::Esc => {
                if self.stage == PaletteStage::SessionList {
                    self.drop_sub_stage();
                    PaletteAction::DropSubStage
                } else {
                    self.close();
                    PaletteAction::Closed
                }
            }
            Key::Enter => {
                PaletteAction::Commit
            }
            Key::Char('j') | Key::CtrlJ | Key::Down => {
                self.move_down(count, highlighted_options)
            }
            Key::Char('k') | Key::CtrlK | Key::Up => {
                self.move_up(highlighted_options)
            }
            Key::PgDn => self.page_down(count),
            Key::PgUp => self.page_up(count),
            Key::Home => self.go_home(),
            Key::End => self.go_end(count),
            Key::CtrlU => self.scroll_preview_up(preview_page),
            Key::CtrlD => self.scroll_preview_down(preview_page),
            Key::CtrlP => self.toggle_preview(count, preview_cutoff),
            Key::Backspace => self.backspace(),
            Key::Char(c) => self.type_char(*c),
            _ => PaletteAction::Nothing,
        }
    }
}

// ── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Key;

    fn open_palette(items: usize) -> PaletteState {
        let mut s = PaletteState::new();
        s.open(items);
        s
    }

    #[test]
    fn open_sets_state() {
        let mut s = PaletteState::new();
        assert!(!s.open);
        s.open(5);
        assert!(s.open);
        assert_eq!(s.stage, PaletteStage::Root);
        assert_eq!(s.cursor(), 0);
        assert_eq!(s.top(), 0);
        assert_eq!(s.visible, 5);
        assert!(s.preview_shown);
        assert!(s.query.is_empty());
    }

    #[test]
    fn close_resets_state() {
        let mut s = open_palette(10);
        s.query = "abc".into();
        s.cursor = 5;
        s.close();
        assert!(!s.open);
        assert!(s.query.is_empty());
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn type_char_resets_cursor() {
        let mut s = open_palette(10);
        s.query = "t".into();
        s.cursor = 5;
        s.top = 3;
        let act = s.press(&Key::Char('x'), 10, 0, 5, 4);
        assert_eq!(act, PaletteAction::Query);
        assert_eq!(s.query, "tx");
        assert_eq!(s.cursor(), 0);
        assert_eq!(s.top(), 0);
    }

    #[test]
    fn j_moves_down() {
        let mut s = open_palette(3);
        s.press(&Key::Char('j'), 3, 0, 5, 4);
        assert_eq!(s.cursor(), 1);
        s.press(&Key::Char('j'), 3, 0, 5, 4);
        assert_eq!(s.cursor(), 2);
        s.press(&Key::Char('j'), 3, 0, 5, 4);
        assert_eq!(s.cursor(), 2, "clamped at last item");
    }

    #[test]
    fn ctrl_j_moves_down() {
        let mut s = open_palette(3);
        s.press(&Key::CtrlJ, 3, 0, 5, 4);
        assert_eq!(s.cursor(), 1);
    }

    #[test]
    fn k_moves_up() {
        let mut s = open_palette(3);
        s.cursor = 2;
        s.press(&Key::Char('k'), 3, 0, 5, 4);
        assert_eq!(s.cursor(), 1);
        s.press(&Key::Char('k'), 3, 0, 5, 4);
        assert_eq!(s.cursor(), 0, "clamped at top");
    }

    #[test]
    fn option_move_down_wraps() {
        let mut s = open_palette(5);
        // Simulate a Set item with 3 options at the cursor.
        s.option_cursor = 0;
        let act = s.press(&Key::Char('j'), 5, 3, 5, 4);
        assert_eq!(act, PaletteAction::OptionMove);
        assert_eq!(s.option_cursor, 1);
        s.press(&Key::Char('j'), 5, 3, 5, 4);
        assert_eq!(s.option_cursor, 2);
        s.press(&Key::Char('j'), 5, 3, 5, 4);
        assert_eq!(s.option_cursor, 0, "wraps to first");
    }

    #[test]
    fn option_move_up_wraps() {
        let mut s = open_palette(5);
        s.option_cursor = 0;
        let act = s.press(&Key::Char('k'), 5, 3, 5, 4);
        assert_eq!(act, PaletteAction::OptionMove);
        assert_eq!(s.option_cursor, 2, "wraps to last");
    }

    #[test]
    fn goto_session_list() {
        let mut s = open_palette(10);
        s.query = "b myproj".into();
        s.goto_session_list();
        assert_eq!(s.stage, PaletteStage::SessionList);
        assert_eq!(s.goto_prefix_len, 8);
        assert_eq!(s.filter_query(), "");
        s.query.push_str("foo");
        assert_eq!(s.filter_query(), "foo");
    }

    #[test]
    fn backspace_in_session_list_below_prefix_drops() {
        let mut s = open_palette(10);
        s.query = "b".into();
        s.goto_session_list();
        // goto_prefix_len is 1, query is "b" (len 1)
        // backspace would make query shorter than prefix
        let act = s.backspace();
        assert_eq!(act, PaletteAction::DropSubStage);
        assert_eq!(s.stage, PaletteStage::Root);
        assert!(s.query.is_empty());
    }

    #[test]
    fn backspace_in_session_list_above_prefix_edits() {
        let mut s = open_palette(10);
        s.query = "b".into();
        s.goto_session_list();
        // goto_prefix_len is 1, query is "b" (len 1)
        // Now simulate typing "fo" into the session filter
        s.query = "b fo".into();
        let act = s.backspace();
        assert_eq!(act, PaletteAction::Query);
        assert_eq!(s.query, "b f");
        assert_eq!(s.stage, PaletteStage::SessionList);
    }

    #[test]
    fn esc_in_root_closes() {
        let mut s = open_palette(5);
        let act = s.press(&Key::Esc, 5, 0, 5, 4);
        assert_eq!(act, PaletteAction::Closed);
        assert!(!s.open);
    }

    #[test]
    fn esc_in_session_list_drops_sub_stage() {
        let mut s = open_palette(5);
        s.query = "b".into();
        s.goto_session_list();
        let act = s.press(&Key::Esc, 5, 0, 5, 4);
        assert_eq!(act, PaletteAction::DropSubStage);
        assert_eq!(s.stage, PaletteStage::Root);
        assert!(s.open, "palette stays open");
        assert!(s.query.is_empty(), "query is reset");
    }

    #[test]
    fn commit_returns_commit() {
        let mut s = open_palette(5);
        s.cursor = 2;
        let act = s.press(&Key::Enter, 5, 0, 5, 4);
        assert_eq!(act, PaletteAction::Commit);
        assert!(s.open, "commit does not close; the caller decides");
    }

    #[test]
    fn page_down_moves_by_visible() {
        let mut s = open_palette(20);
        s.visible = 10;
        s.press(&Key::PgDn, 20, 0, 5, 4);
        assert_eq!(s.cursor(), 10);
    }

    #[test]
    fn go_end_jumps_to_last() {
        let mut s = open_palette(15);
        s.press(&Key::End, 15, 0, 5, 4);
        assert_eq!(s.cursor(), 14);
    }

    #[test]
    fn go_home_resets_cursor() {
        let mut s = open_palette(15);
        s.cursor = 10;
        s.top = 5;
        s.press(&Key::Home, 15, 0, 5, 4);
        assert_eq!(s.cursor(), 0);
        assert_eq!(s.top(), 0);
    }

    #[test]
    fn sync_clamps_cursor() {
        let mut s = open_palette(10);
        s.cursor = 8;
        s.sync(3);
        assert_eq!(s.cursor(), 2, "cursor clamped to last valid index");
    }

    #[test]
    fn scroll_preview() {
        let mut s = open_palette(5);
        s.press(&Key::CtrlD, 5, 0, 3, 4);
        assert_eq!(s.preview_scroll, 3);
        s.press(&Key::CtrlU, 5, 0, 3, 4);
        assert_eq!(s.preview_scroll, 0, "saturates at 0");
    }

    #[test]
    fn toggle_preview_flips_when_above_cutoff() {
        let mut s = open_palette(5);
        assert!(s.preview_visible(5, 4), "above the cutoff the pane is visible");
        s.press(&Key::CtrlP, 5, 5, 4, 4);
        assert!(!s.preview_shown, "toggle turns it off");
        assert!(!s.preview_visible(5, 4));
        s.press(&Key::CtrlP, 5, 5, 4, 4);
        assert!(s.preview_shown, "toggle turns it back on");
        assert!(s.preview_visible(5, 4));
    }

    #[test]
    fn toggle_preview_forces_on_below_cutoff() {
        let mut s = open_palette(3);
        s.preview_shown = true;
        s.preview_forced = false;
        assert!(!s.preview_visible(3, 4), "auto-hidden below the cutoff");
        s.press(&Key::CtrlP, 3, 5, 4, 4);
        assert!(s.preview_shown && s.preview_forced, "forced on");
        assert!(s.preview_visible(3, 4), "forced on overrides the cutoff");
    }
}
