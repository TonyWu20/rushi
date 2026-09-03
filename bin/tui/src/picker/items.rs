//! Picker items and the source seam (section 4.1).

/// One candidate the picker shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerItem {
    /// Display label shown in the list (e.g. relative path).
    pub label: String,
    /// The stable value the user gets on select (e.g. relative path).
    pub value: String,
    /// Payload for the previewer (e.g. absolute path for file read).
    pub payload: String,
}

/// The data seam: streams candidate items into the picker.
///
/// A later symbol source or git-files source implements this trait.
/// This is the extension point for new sources (section 4.5).
pub trait ItemSource {
    /// The human name of this source, shown in the picker header.
    ///
    /// Day 0 wires `FileItemSource` directly; the name is reserved for
    /// future sources (symbols, git files) and is exercised by tests.
    #[allow(dead_code)]
    fn name(&self) -> &str;

    /// All candidate items. Called once when the picker opens;
    /// the background matcher re-ranks this list on every keystroke
    /// (section 4.2).
    fn items(&self) -> Vec<PickerItem>;
}

/// The day-0 source: the working tree's files, honoring `.gitignore`.
///
/// Uses `git ls-files` when a repo is present, a plain walk otherwise
/// (section 4.1 / section 7).
pub struct FileItemSource {
    root: std::path::PathBuf,
}

impl FileItemSource {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            root: root.into(),
        }
    }

    /// Collect the file list for this source.
    pub fn collect(&self) -> Vec<PickerItem> {
        if is_git_repo(&self.root) {
            git_ls_files(&self.root)
                .into_iter()
                .map(|p| {
                    let abs = self.root.join(&p);
                    PickerItem {
                        label: p.clone(),
                        value: p.clone(),
                        payload: abs.to_string_lossy().to_string(),
                    }
                })
                .collect()
        } else {
            walk_files(&self.root)
                .into_iter()
                .map(|p| {
                    let rel = p
                        .strip_prefix(&self.root)
                        .unwrap_or(&p)
                        .to_string_lossy()
                        .to_string();
                    PickerItem {
                        label: rel.clone(),
                        value: rel,
                        payload: p.to_string_lossy().to_string(),
                    }
                })
                .collect()
        }
    }
}

impl ItemSource for FileItemSource {
    fn name(&self) -> &str {
        "files"
    }

    fn items(&self) -> Vec<PickerItem> {
        self.collect()
    }
}

/// Detect a git work tree by walking up from `root` looking for a
/// `.git` entry (section 4.1).
fn is_git_repo(root: &std::path::Path) -> bool {
    let mut dir = root;
    loop {
        if dir.join(".git").exists() {
            return true;
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => return false,
        }
    }
}

/// Run `git ls-files` (tracked) and `git ls-files --others --exclude-standard`
/// (untracked, not ignored) to build a combined list.
fn git_ls_files(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    // Tracked files.
    if let Ok(output) = std::process::Command::new("git")
        .arg("ls-files")
        .current_dir(root)
        .output()
    {
        if let Ok(text) = String::from_utf8(output.stdout) {
            out.extend(text.lines().filter(|l| !l.is_empty()).map(|l| l.to_string()));
        }
    }
    // Untracked files that are not git-ignored.
    if let Ok(output) = std::process::Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(root)
        .output()
    {
        if let Ok(text) = String::from_utf8(output.stdout) {
            out.extend(text.lines().filter(|l| !l.is_empty()).map(|l| l.to_string()));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Plain directory walk (non-git repos). Skips hidden directories and
/// common build/dependency directories.
fn walk_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    walk(root, &mut out);
    out.sort();
    out
}

fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Skip hidden dirs and common build/dependency dirs.
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

// ── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_item_source_is_a_git_repo() {
        // Self-contained: build a throwaway git repo in a temp dir so
        // the test does not depend on the cargo working directory.
        let tmp = std::env::temp_dir().join("picker_test_gitrepo");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(tmp.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(tmp.join("README.md"), "# demo\n").unwrap();
        let git_ok = std::process::Command::new("git")
            .arg("init")
            .current_dir(&tmp)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let src = FileItemSource::new(tmp.clone());
        let items = src.collect();
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"src/main.rs"), "src/main.rs should be found");
        assert!(labels.contains(&"README.md"), "README.md should be found");
        if git_ok {
            assert!(is_git_repo(&tmp), "fresh git init dir should be detected");
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn git_repo_detection_walks_up() {
        let tmp = std::env::temp_dir().join("picker_test_walkup");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("a/b")).unwrap();
        std::fs::create_dir_all(tmp.join(".git")).unwrap();
        assert!(
            is_git_repo(&tmp.join("a/b")),
            "finds .git in an ancestor directory"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn file_item_source_non_git_walks() {
        // Use a temp dir that is not a git repo.
        let tmp = std::env::temp_dir().join("picker_test_walk");
        let _ = std::fs::create_dir_all(&tmp);
        let _ = std::fs::write(tmp.join("hello.txt"), "hello");
        let _ = std::fs::write(tmp.join("world.rs"), "fn main() {}");
        // Create a .git dir to make it NOT a git repo? No, we want the
        // walk path. Remove .git if present.
        let git_dir = tmp.join(".git");
        let _ = std::fs::remove_dir_all(&git_dir);

        let src = FileItemSource::new(tmp.clone());
        let items = src.collect();
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"hello.txt"));
        assert!(labels.contains(&"world.rs"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn item_source_trait_dispatch() {
        let src = FileItemSource::new(".");
        let name = ItemSource::name(&src);
        assert_eq!(name, "files");
        let items = ItemSource::items(&src);
        assert!(!items.is_empty());
    }
}
