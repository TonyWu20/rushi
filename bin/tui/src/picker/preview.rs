//! Previewers (section 4.4).
//!
//! The `Previewer` trait is the seam that swaps the preview pane
//! content. Day 0: [`FilePreviewer`] shows file text. The
//! [`NullPreviewer`] shows nothing. Later: code, diff, or none.

use super::items::PickerItem;

/// The previewer seam. A picker body takes one previewer and renders
/// the preview pane for the selected item.
pub trait Previewer {
    /// Whether this previewer renders anything. The null previewer
    /// returns `false` and the pane is dropped.
    fn enabled(&self) -> bool;

    /// The header line for the preview pane (path, size, line count).
    /// `None` when the item cannot be previewed.
    fn header(&self, item: &PickerItem) -> Option<String>;

    /// The preview text lines for the selected item.
    fn content(&self, item: &PickerItem) -> Vec<String>;
}

/// A previewer that reads and shows the text content of a file.
///
/// The item's `payload` is the absolute path. The header shows the
/// path, byte size, and line count. The content is the file text,
/// capped at `max_lines` lines.
pub struct FilePreviewer {
    max_lines: usize,
}

impl FilePreviewer {
    /// Create a file previewer that shows at most `max_lines` lines.
    pub fn new(max_lines: usize) -> Self {
        Self { max_lines }
    }
}

impl Previewer for FilePreviewer {
    fn enabled(&self) -> bool {
        true
    }

    fn header(&self, item: &PickerItem) -> Option<String> {
        let path = &item.payload;
        let Ok(meta) = std::fs::metadata(path) else {
            return Some(format!("{} (unreadable)", item.label));
        };
        let size = meta.len();
        let line_count = std::fs::read_to_string(path)
            .map(|t| t.lines().count())
            .unwrap_or(0);
        Some(format!(
            "{}  {}  {} lines",
            item.label,
            human_size(size),
            line_count
        ))
    }

    fn content(&self, item: &PickerItem) -> Vec<String> {
        let path = &item.payload;
        match std::fs::read_to_string(path) {
            Ok(text) => text
                .lines()
                .take(self.max_lines)
                .map(|l| l.to_string())
                .collect(),
            Err(_) => vec![format!("(cannot read {})", item.label)],
        }
    }
}

/// A previewer that shows nothing. The preview pane is dropped.
///
/// Day 0 wires `FilePreviewer`; this is the off seam for later
/// previews that need no pane. Unused in production today, so the
/// dead-code lint is suppressed.
#[allow(dead_code)]
pub struct NullPreviewer;

impl Previewer for NullPreviewer {
    fn enabled(&self) -> bool {
        false
    }

    fn header(&self, _item: &PickerItem) -> Option<String> {
        None
    }

    fn content(&self, _item: &PickerItem) -> Vec<String> {
        Vec::new()
    }
}

/// Format a byte count as a human-readable string.
fn human_size(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if n >= GB {
        format!("{:.1}G", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1}M", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1}K", n as f64 / KB as f64)
    } else {
        format!("{n}B")
    }
}

// ── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn test_item() -> (PickerItem, NamedTempFile) {
        let mut f = NamedTempFile::new().unwrap();
        let _ = writeln!(f, "line one");
        let _ = writeln!(f, "line two");
        let _ = writeln!(f, "line three");
        let path = f.path().to_string_lossy().to_string();
        let label = "test_file.txt".to_string();
        let item = PickerItem {
            label: label.clone(),
            value: label,
            payload: path,
        };
        (item, f)
    }

    #[test]
    fn file_previewer_header_shows_metadata() {
        let (item, _f) = test_item();
        let p = FilePreviewer::new(100);
        let header = p.header(&item).unwrap();
        assert!(header.contains("test_file.txt"));
        assert!(header.contains("lines"));
    }

    #[test]
    fn file_previewer_content_returns_lines() {
        let (item, _f) = test_item();
        let p = FilePreviewer::new(100);
        let content = p.content(&item);
        assert_eq!(content.len(), 3);
        assert_eq!(content[0], "line one");
    }

    #[test]
    fn null_previewer_is_disabled() {
        let p = NullPreviewer;
        assert!(!p.enabled());
    }

    #[test]
    fn file_previewer_respects_max_lines() {
        let mut f = NamedTempFile::new().unwrap();
        for i in 0..20 {
            let _ = writeln!(f, "line {i}");
        }
        let path = f.path().to_string_lossy().to_string();
        let item = PickerItem {
            label: "big.txt".into(),
            value: "big.txt".into(),
            payload: path,
        };
        let p = FilePreviewer::new(5);
        let content = p.content(&item);
        assert_eq!(content.len(), 5);
        assert_eq!(content[0], "line 0");
        assert_eq!(content[4], "line 4");
    }
}
