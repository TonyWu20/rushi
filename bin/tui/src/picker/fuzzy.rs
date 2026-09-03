//! The frizbee-backed fuzzy ranker and background worker (section 4.2).
//!
//! A background worker thread owns the `frizbee::Matcher`. The UI
//! pushes queries through an `mpsc` channel. The worker publishes an
//! `Arc<Snapshot>` that the UI reads without blocking. This is the
//! `television` model (research doc, section 2).
//!
//! Sort order: score, then an optional index bias. A frecency sort
//! is a later add (section 4.4).

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use frizbee::{Config, Matcher};

use super::items::PickerItem;

/// An immutable snapshot of ranked items for one query.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// Ranked items, best first.
    pub items: Vec<PickerItem>,
    /// The query these items answer for.
    pub query: String,
    /// Whether the match has settled for this query.
    pub settled: bool,
}

enum Cmd {
    Query(String),
    Shutdown,
}

/// The frizbee-backed ranker. Owns a background worker thread that
/// re-ranks the full item list on every query. The UI reads the
/// latest [`Snapshot`] without blocking.
pub struct PickerMatcher {
    tx: mpsc::Sender<Cmd>,
    snap: Arc<Mutex<Arc<Snapshot>>>,
    _worker: Option<thread::JoinHandle<()>>,
}

impl PickerMatcher {
    /// Create a new matcher over the given item list.
    ///
    /// The background worker starts immediately with a snapshot of
    /// all items (the empty-query state).
    pub fn new(items: Vec<PickerItem>) -> Self {
        let labels: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
        let initial = Arc::new(Snapshot {
            items: items.clone(),
            query: String::new(),
            settled: true,
        });
        let snap = Arc::new(Mutex::new(initial));
        let (tx, rx) = mpsc::channel::<Cmd>();

        let snap_clone = Arc::clone(&snap);
        let worker = thread::spawn(move || {
            worker_loop(rx, labels, items, snap_clone);
        });

        Self {
            tx,
            snap,
            _worker: Some(worker),
        }
    }

    /// Push a new query to the background worker (non-blocking).
    /// The worker re-ranks the full item list and publishes a new
    /// snapshot.
    pub fn query(&self, q: &str) {
        let _ = self.tx.send(Cmd::Query(q.to_string()));
    }

    /// Read the latest snapshot without blocking.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snap.lock().unwrap().clone()
    }
}

impl Drop for PickerMatcher {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        // The worker exits when the channel is closed. No join:
        // the thread is short-lived and the channel close is enough.
    }
}

fn worker_loop(
    rx: mpsc::Receiver<Cmd>,
    labels: Vec<String>,
    items: Vec<PickerItem>,
    snap: Arc<Mutex<Arc<Snapshot>>>,
) {
    for cmd in rx {
        match cmd {
            Cmd::Query(q) => {
                let ranked = if q.is_empty() {
                    items.clone()
                } else {
                    let mut m = Matcher::new(&q, &Config::default());
                    let matches = m.match_list(&labels);
                    matches
                        .iter()
                        .map(|m| items[m.index as usize].clone())
                        .collect()
                };
                let mut guard = snap.lock().unwrap();
                *guard = Arc::new(Snapshot {
                    items: ranked,
                    query: q,
                    settled: true,
                });
            }
            Cmd::Shutdown => break,
        }
    }
}

// ── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_items() -> Vec<PickerItem> {
        vec![
            PickerItem {
                label: "src/main.rs".into(),
                value: "src/main.rs".into(),
                payload: "/repo/src/main.rs".into(),
            },
            PickerItem {
                label: "src/vim_editor.rs".into(),
                value: "src/vim_editor.rs".into(),
                payload: "/repo/src/vim_editor.rs".into(),
            },
            PickerItem {
                label: "src/render.rs".into(),
                value: "src/render.rs".into(),
                payload: "/repo/src/render.rs".into(),
            },
            PickerItem {
                label: "docs/tui-file-picker.md".into(),
                value: "docs/tui-file-picker.md".into(),
                payload: "/repo/docs/tui-file-picker.md".into(),
            },
            PickerItem {
                label: "README.md".into(),
                value: "README.md".into(),
                payload: "/repo/README.md".into(),
            },
        ]
    }

    #[test]
    fn empty_query_returns_all_items() {
        let items = test_items();
        let m = PickerMatcher::new(items.clone());
        // The initial snapshot has all items.
        let snap = m.snapshot();
        assert_eq!(snap.items.len(), items.len());
        assert!(snap.query.is_empty());
        assert!(snap.settled);
    }

    #[test]
    fn query_filters_and_ranks() {
        let items = test_items();
        let m = PickerMatcher::new(items);
        m.query("main");
        // Wait a bit for the background thread to publish.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let snap = m.snapshot();
        assert!(!snap.items.is_empty(), "expected matches for 'main'");
        assert!(
            snap.items.iter().any(|i| i.label == "src/main.rs"),
            "src/main.rs should match 'main'"
        );
    }

    #[test]
    fn fuzzy_match_finds_partial() {
        let items = test_items();
        let m = PickerMatcher::new(items);
        m.query("vim");
        std::thread::sleep(std::time::Duration::from_millis(50));
        let snap = m.snapshot();
        assert!(
            snap.items.iter().any(|i| i.label == "src/vim_editor.rs"),
            "'vim' should match src/vim_editor.rs"
        );
    }

    #[test]
    fn no_match_returns_empty() {
        let items = test_items();
        let m = PickerMatcher::new(items);
        m.query("zzzzz");
        std::thread::sleep(std::time::Duration::from_millis(50));
        let snap = m.snapshot();
        assert!(snap.items.is_empty(), "no item matches 'zzzzz'");
    }
}
