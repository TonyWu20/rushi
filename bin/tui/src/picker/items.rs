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

    /// All candidate items. Called once when the picker opens; the
    /// background matcher re-ranks this list on every keystroke
    /// (section 4.2). Day-0 consumers call `collect_in` directly;
    /// this trait method stays as the seam for later sources.
    #[allow(dead_code)]
    fn items(&self) -> Vec<PickerItem>;
}

/// How much of the working tree the file source shows. Cycled with
/// `Ctrl+I` while the picker is open (docs/tui-file-picker.md P9).
///
/// - `Standard` (the default): in a git repo, tracked files plus
///   untracked-but-not-ignored files; otherwise a plain walk that
///   skips hidden entries and build/dependency directories.
/// - `IncludeIgnored`: also shows git-ignored files (in a git repo)
///   or build/dependency directories (in a plain walk).
/// - `IncludeHidden`: also shows hidden (dot) files. In a git repo
///   the git listings already report hidden tracked and untracked
///   files, so this step mostly widens the ignored set; in a plain
///   walk it is the dot-entry step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileScope {
    #[default]
    Standard,
    IncludeIgnored,
    IncludeHidden,
}

impl FileScope {
    /// The next scope in the `Ctrl+I` cycle: standard → show
    /// ignored → show hidden → back to standard.
    pub fn next(&self) -> Self {
        match self {
            Self::Standard => Self::IncludeIgnored,
            Self::IncludeIgnored => Self::IncludeHidden,
            Self::IncludeHidden => Self::Standard,
        }
    }

    /// Whether git-ignored files (or, in a plain walk, build/
    /// dependency directories) are included.
    pub fn includes_ignored(self) -> bool {
        !matches!(self, Self::Standard)
    }

    /// Whether hidden (dot) entries are included.
    pub fn includes_hidden(self) -> bool {
        matches!(self, Self::IncludeHidden)
    }

    /// The short title tag for a non-default scope, shown in the
    /// float title so the widened set is visible at a glance.
    pub fn tag(self) -> Option<&'static str> {
        match self {
            Self::Standard => None,
            Self::IncludeIgnored => Some("ignored shown"),
            Self::IncludeHidden => Some("hidden shown"),
        }
    }

    /// The flash-line description of the scope, used when the user
    /// cycles with `Ctrl+I`.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Standard => "scope: default (hidden and git-ignored excluded)",
            Self::IncludeIgnored => "scope: showing git-ignored files",
            Self::IncludeHidden => "scope: showing hidden files too",
        }
    }
}

/// The day-0 source: the working tree's files, honoring `.gitignore`
/// by default. The `Ctrl+I` scope cycle (docs/tui-file-picker.md
/// P9) widens the set past the default.
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

    /// The base root this source was created with.
    pub fn base(&self) -> &std::path::Path {
        &self.root
    }

    /// Collect files under an arbitrary search root at a given scope.
    /// Labels stay relative to the base root when a file is under it
    /// (the day-0 case and `../` navigation); otherwise labels are
    /// absolute paths, which is what path queries outside the base
    /// produce.
    pub fn collect_in(
        &self,
        root: &std::path::Path,
        scope: FileScope,
    ) -> Vec<PickerItem> {
        let base = self.root.clone();
        if is_git_repo(root) {
            git_ls_files(root, scope)
                .into_iter()
                .map(|p| {
                    let abs = root.join(&p);
                    self.file_item(&abs, &base)
                })
                .collect()
        } else {
            walk_files(root, scope)
                .into_iter()
                .map(|abs| self.file_item(&abs, &base))
                .collect()
        }
    }

    fn file_item(&self, abs: &std::path::Path, base: &std::path::Path) -> PickerItem {
        let label = display_path(abs, base);
        PickerItem {
            label: label.clone(),
            value: label,
            payload: abs.to_string_lossy().into_owned(),
        }
    }
}

impl ItemSource for FileItemSource {
    fn name(&self) -> &str {
        "files"
    }

    fn items(&self) -> Vec<PickerItem> {
        self.collect_in(&self.root, FileScope::Standard)
    }
}

/// The label for a file: the path relative to `base` when the file is
/// under it (including `../` navigation, which stays relative the way
/// the user typed it); the absolute path otherwise.
fn display_path(abs: &std::path::Path, base: &std::path::Path) -> String {
    if let Ok(rel) = abs.strip_prefix(base) {
        return rel.to_string_lossy().into_owned();
    }
    abs.to_string_lossy().into_owned()
}

/// Split a raw `@query` into the directory to enumerate and the fuzzy
/// tail to rank against.
///
/// A query that names a path outside the base (starts with `/`, `~`,
/// or `../`, or is `..`) re-roots the search: the part up to the last
/// `/` is the new search root, the remainder is the fuzzy tail. Plain
/// queries stay under the base and are matched whole (day-0
/// behavior, section 4.1).
pub fn query_root_tail(query: &str, base: &std::path::Path) -> (std::path::PathBuf, String) {
    let expanded = expand_home(query);
    if !is_path_query(&expanded) {
        return (base.to_path_buf(), expanded);
    }
    let (dir_part, tail) = match expanded.rfind('/') {
        Some(i) => (&expanded[..i], &expanded[i + 1..]),
        None => (expanded.as_str(), ""),
    };
    let dir = dir_part.trim_end_matches('/');
    let root = if dir.is_empty() {
        std::path::PathBuf::from("/")
    } else if dir.starts_with('/') {
        std::path::PathBuf::from(dir)
    } else {
        base.join(dir)
    };
    (root, tail.to_string())
}

/// Whether a query names a path: absolute, home-relative, or parent
/// navigation.
fn is_path_query(q: &str) -> bool {
    q.starts_with('/')
        || q.starts_with('~')
        || q == ".."
        || q.starts_with("../")
}

/// Expand a leading `~` to `$HOME`.
fn expand_home(q: &str) -> String {
    match q.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{home}{rest}")
        }
        _ => q.to_string(),
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

/// Run `git ls-files` (tracked) and `git ls-files --others
/// --exclude-standard` (untracked, not ignored) to build a combined
/// list. At `IncludeIgnored` or `IncludeHidden` scope the
/// `--others --ignored` set (target/, sessions/, …) is added too.
/// A capped output keeps an arbitrary repo responsive: the same cap
/// as the plain walk.
fn git_ls_files(root: &std::path::Path, scope: FileScope) -> Vec<String> {
    let mut out = Vec::new();
    // Tracked files (hidden tracked files included).
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
    // The cycled-in scope: untracked files that ARE git-ignored.
    if scope.includes_ignored() {
        if let Ok(output) = std::process::Command::new("git")
            .args([
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
            ])
            .current_dir(root)
            .output()
        {
            if let Ok(text) = String::from_utf8(output.stdout) {
                out.extend(
                    text.lines()
                        .filter(|l| !l.is_empty())
                        .map(|l| l.to_string()),
                );
            }
        }
    }
    out.sort();
    out.dedup();
    out.truncate(MAX_WALK_FILES);
    out
}

/// Plain directory walk (non-git repos). The scope controls what is
/// skipped: `Standard` skips hidden directories/files and common
/// build/dependency directories; `IncludeIgnored` adds the
/// build/dependency directories; `IncludeHidden` also walks dot
/// entries (the `.git` internals are never enumerated). Capped so an
/// arbitrary root (e.g. `/` from an absolute path query) stays
/// responsive.
const MAX_WALK_DEPTH: usize = 12;
const MAX_WALK_FILES: usize = 20_000;

fn walk_files(root: &std::path::Path, scope: FileScope) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    walk(root, 0, scope, &mut out);
    out.sort();
    out
}

fn walk(
    dir: &std::path::Path,
    depth: usize,
    scope: FileScope,
    out: &mut Vec<std::path::PathBuf>,
) {
    if depth >= MAX_WALK_DEPTH || out.len() >= MAX_WALK_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // The `.git` internals are never enumerated, at any scope.
        if name == ".git" {
            continue;
        }
        // Hidden entries are skipped unless the scope includes them.
        if !scope.includes_hidden() && name.starts_with('.') {
            continue;
        }
        // Build/dependency dirs are the non-git "ignored" set:
        // skipped unless the scope includes ignored.
        if !scope.includes_ignored() && (name == "target" || name == "node_modules") {
            continue;
        }
        if path.is_dir() {
            walk(&path, depth + 1, scope, out);
        } else if out.len() < MAX_WALK_FILES {
            out.push(path);
        }
    }
}

// ── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture root for the plain-walk path (P8): a writable
    /// directory with no `.git` in any ancestor. `std::env::temp_dir()`
    /// is clean on a normal host, but some sandboxes carry a stray
    /// `.git` into /tmp; the home dir is the fallback.
    fn non_git_fixture(name: &str) -> Option<std::path::PathBuf> {
        let mut bases = vec![std::env::temp_dir()];
        if let Ok(home) = std::env::var("HOME") {
            bases.push(std::path::PathBuf::from(home));
        }
        for base in bases {
            let dir = base.join(name);
            if std::fs::create_dir_all(&dir).is_ok() && !is_git_repo(&dir) {
                return Some(dir);
            }
        }
        None
    }

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
        let items = src.collect_in(src.base(), FileScope::Standard);
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
        // Use a fixture dir with no `.git` in any ancestor, so the
        // plain walk path is taken (P8).
        let tmp = match non_git_fixture("picker_test_walk") {
            Some(t) => t,
            None => {
                eprintln!(
                    "skipped: every candidate base has a .git ancestor; \
                     cannot exercise the plain-walk path"
                );
                return;
            }
        };
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("hello.txt"), "hello").unwrap();
        std::fs::write(tmp.join("world.rs"), "fn main() {}").unwrap();

        let src = FileItemSource::new(tmp.clone());
        let items = src.collect_in(src.base(), FileScope::Standard);
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

    #[test]
    fn query_root_tail_keeps_plain_queries_under_base() {
        let base = std::path::PathBuf::from("/repo");
        assert_eq!(query_root_tail("", &base), (base.clone(), String::new()));
        assert_eq!(
            query_root_tail("src/main", &base),
            (base.clone(), "src/main".into())
        );
        // A `..` inside the name is not parent navigation.
        assert_eq!(
            query_root_tail("..notes", &base),
            (base.clone(), "..notes".into())
        );
    }

    #[test]
    fn query_root_tail_reroots_absolute_paths() {
        let base = std::path::PathBuf::from("/repo");
        assert_eq!(
            query_root_tail("/etc/passwd", &base),
            (std::path::PathBuf::from("/etc"), "passwd".into())
        );
        assert_eq!(
            query_root_tail("/etc/", &base),
            (std::path::PathBuf::from("/etc"), String::new())
        );
        assert_eq!(
            query_root_tail("/", &base),
            (std::path::PathBuf::from("/"), String::new())
        );
    }

    #[test]
    fn query_root_tail_reroots_parent_paths() {
        let base = std::path::PathBuf::from("/repo/app");
        assert_eq!(
            query_root_tail("../sibling/x.rs", &base),
            (std::path::PathBuf::from("/repo/app/../sibling"), "x.rs".into())
        );
        assert_eq!(
            query_root_tail("..", &base),
            (std::path::PathBuf::from("/repo/app/.."), String::new())
        );
    }

    #[test]
    fn collect_in_labels_files_outside_base_as_absolute() {
        let tmp = match non_git_fixture("picker_test_reroot") {
            Some(t) => t,
            None => {
                eprintln!(
                    "skipped: every candidate base has a .git ancestor; \
                     cannot exercise the plain-walk path"
                );
                return;
            }
        };
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("b")).unwrap();
        std::fs::write(tmp.join("b/x.txt"), "x").unwrap();
        // A source based at `a` listing files under `b`: the file is
        // not under the base, so its label is the absolute path.
        let src = FileItemSource::new(tmp.join("a"));
        let items = src.collect_in(&tmp.join("b"), FileScope::Standard);
        let labels: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
        assert!(
            labels.contains(&tmp.join("b/x.txt").to_string_lossy().into_owned()),
            "sibling file should be labeled absolute: {labels:?}"
        );
        // Files under the base keep relative labels.
        let items = src.collect_in(src.base(), FileScope::Standard);
        let labels: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
        assert!(
            labels.iter().all(|l| !l.starts_with('/')),
            "files under the base stay relative: {labels:?}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn file_scope_cycles_standard_to_ignored_to_hidden() {
        let s = FileScope::Standard;
        assert_eq!(s.next(), FileScope::IncludeIgnored);
        assert_eq!(FileScope::IncludeIgnored.next(), FileScope::IncludeHidden);
        assert_eq!(FileScope::IncludeHidden.next(), FileScope::Standard);
        assert_eq!(FileScope::default(), FileScope::Standard);
    }

    #[test]
    fn walk_scope_controls_hidden_and_build_dirs() {
        // Drives the walk directly (no git detection), so the scope
        // semantics are hermetic no matter what `.git` entries the
        // environment carries.
        let tmp = std::env::temp_dir().join("picker_test_scope_walk");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("visible")).unwrap();
        std::fs::write(tmp.join("visible/main.txt"), "x").unwrap();
        std::fs::create_dir_all(tmp.join(".hidden_dir")).unwrap();
        std::fs::write(tmp.join(".hidden_dir/secret.txt"), "x").unwrap();
        std::fs::write(tmp.join(".env"), "x").unwrap();
        std::fs::create_dir_all(tmp.join("target")).unwrap();
        std::fs::write(tmp.join("target/out.bin"), "x").unwrap();

        let names = |scope: FileScope| -> Vec<String> {
            walk_files(&tmp, scope)
                .into_iter()
                .map(|p| p.strip_prefix(&tmp).unwrap().to_string_lossy().into_owned())
                .collect()
        };

        let standard = names(FileScope::Standard);
        assert!(standard.contains(&"visible/main.txt".to_string()));
        assert!(
            !standard.iter().any(|l| l.starts_with('.')),
            "standard skips dot entries: {standard:?}"
        );
        assert!(
            !standard.iter().any(|l| l.starts_with("target/")),
            "standard skips build dirs: {standard:?}"
        );

        let ignored = names(FileScope::IncludeIgnored);
        assert!(
            ignored.iter().any(|l| l.starts_with("target/")),
            "IncludeIgnored adds build dirs: {ignored:?}"
        );
        assert!(
            !ignored.iter().any(|l| l.starts_with('.')),
            "IncludeIgnored still skips dot entries: {ignored:?}"
        );

        let all = names(FileScope::IncludeHidden);
        assert!(
            all.iter().any(|l| l == ".env"),
            "IncludeHidden adds dot files: {all:?}"
        );
        assert!(
            all.iter().any(|l| l.starts_with(".hidden_dir/")),
            "IncludeHidden adds dot dirs: {all:?}"
        );
        assert!(
            all.iter().any(|l| l.starts_with("target/")),
            "IncludeHidden keeps the build dirs: {all:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn git_scope_includes_ignored_and_hidden_files() {
        // Self-contained git repo with an ignored dir (the sessions/
        // case: the user wants `@sessions` to reach session files).
        let tmp = std::env::temp_dir().join("picker_test_scope_git");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(tmp.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(tmp.join(".gitignore"), "ignored/\n").unwrap();
        let git_ok = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&tmp)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !git_ok {
            let _ = std::fs::remove_dir_all(&tmp);
            return;
        }
        std::fs::create_dir_all(tmp.join("ignored")).unwrap();
        std::fs::write(tmp.join("ignored/session.md"), "s\n").unwrap();
        std::fs::create_dir_all(tmp.join(".hid")).unwrap();
        std::fs::write(tmp.join(".hid/notes.md"), "n\n").unwrap();
        let src = FileItemSource::new(tmp.clone());
        let base = src.base();

        let standard: Vec<String> = src
            .collect_in(base, FileScope::Standard)
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert!(standard.contains(&"src/main.rs".to_string()));
        assert!(
            !standard.iter().any(|l| l.starts_with("ignored/")),
            "standard hides git-ignored files: {standard:?}"
        );

        let ignored: Vec<String> = src
            .collect_in(base, FileScope::IncludeIgnored)
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert!(
            ignored.iter().any(|l| l.starts_with("ignored/")),
            "IncludeIgnored shows git-ignored files: {ignored:?}"
        );

        let all: Vec<String> = src
            .collect_in(base, FileScope::IncludeHidden)
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert!(
            all.iter().any(|l| l.starts_with(".hid/")),
            "IncludeHidden shows hidden untracked files: {all:?}"
        );
        assert!(
            all.iter().any(|l| l.starts_with("ignored/")),
            "IncludeHidden keeps the ignored set: {all:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
