//! `rushi docs` — print the embedded harness reference.
//!
//! The `docs/reference/` tree is compiled into the binary at build time
//! via `include_str!`, so it is always available regardless of install
//! method (Nix store, `install.sh`, dev checkout).
//!
//! - `rushi docs` prints the default reference (`README.md`).
//! - `rushi docs <section>` prints a single section of the default
//!   reference, matched by heading number or title substring
//!   (case-insensitive).
//! - `rushi docs <name>` prints a whole bundled sub-document from
//!   `docs/reference/` (e.g. `rushi docs nix-flake-module`).
//! - `rushi docs --list` lists the default sections and the bundled
//!   sub-documents.

/// The default reference document, baked in at compile time.
const REFERENCE: &str = include_str!("../../../docs/reference/README.md");

/// Bundled sub-documents under `docs/reference/nix/`, selectable by
/// name from `rushi docs <name>`. Each is a whole document; per-section
/// filtering is reserved for the default reference.
const NIX_FLAKE_MODULE: &str =
    include_str!("../../../docs/reference/nix/nix-flake-module.md");
const EXT_FLAKE_AUTHORING: &str =
    include_str!("../../../docs/reference/nix/ext-flake-authoring.md");

/// A bundled sub-document, addressable by a stable CLI name.
struct BundledDoc {
    name: &'static str,
    title: &'static str,
    text: &'static str,
}

/// The registry of bundled sub-documents. Order is the `--list` order.
const BUNDLED_DOCS: &[BundledDoc] = &[
    BundledDoc {
        name: "nix-flake-module",
        title: "Nix flaking module (lib.mkRushi + the rushi.* option schema)",
        text: NIX_FLAKE_MODULE,
    },
    BundledDoc {
        name: "ext-flake-authoring",
        title: "Extension-repo flake.nix authoring (tool / hook / UI-ext $out contracts)",
        text: EXT_FLAKE_AUTHORING,
    },
];

/// Look up a bundled sub-document by CLI name (case-insensitive).
fn find_bundled(name: &str) -> Option<&'static BundledDoc> {
    BUNDLED_DOCS
        .iter()
        .find(|d| d.name.eq_ignore_ascii_case(name))
}

/// Print the reference document, optionally filtered to one section, or
/// a whole bundled sub-document by name.
///
/// When `section` is `None` or empty, the default reference is printed.
/// Otherwise:
///   1. If `section` names a bundled sub-document, that whole document is
///      printed (takes precedence over the default-reference matchers).
///   2. Otherwise the default reference is matched by heading number or
///      title substring (case-insensitive).
///
/// If nothing matches, the available sections and bundled docs are listed
/// on stderr and the exit code is 1.
pub fn print_docs(section: Option<&str>) {
    if section.is_none() || section.map_or(false, |s| s.is_empty()) {
        print!("{REFERENCE}");
        return;
    }

    let query = section.unwrap_or("");

    // 1. A whole bundled sub-document by name.
    if let Some(doc) = find_bundled(query) {
        print!("{text}", text = doc.text);
        return;
    }

    // 2. A section of the default reference.
    let q_lower = query.to_lowercase();

    // Split on `\n## ` — parts[0] is the preamble (H1 title + intro),
    // parts[1..] are sections whose first line is the title (number + name).
    let parts: Vec<&str> = REFERENCE.split("\n## ").collect();
    let sections: Vec<(String, String)> = parts
        .iter()
        .skip(1)
        .map(|chunk| {
            let mut lines = chunk.splitn(2, '\n');
            let title_line = lines.next().unwrap_or("");
            let content = lines.next().unwrap_or("");
            let heading = format!("## {title_line}");
            (heading, content.to_string())
        })
        .collect();

    for (heading, content) in &sections {
        // heading looks like "## 7. Prompt Fragments & Extensions"
        let title = heading.trim_start_matches("## ");

        // Match by number: "7" matches "## 7. ..."
        if let Some(num) = title.split('.').next() {
            if num.trim() == q_lower {
                print!("{heading}\n{content}");
                return;
            }
        }

        // Match by title substring (case-insensitive).
        let title_lower = title.to_lowercase();
        if title_lower.contains(&q_lower) {
            print!("{heading}\n{content}");
            return;
        }
    }

    // No match. List what is available to help the user.
    eprintln!("rushi docs: no section matching {query:?}. Available sections:");
    for (heading, _) in &sections {
        eprintln!("  {heading}");
    }
    eprintln!("Bundled docs (run `rushi docs <name>`):");
    for doc in BUNDLED_DOCS {
        eprintln!("  {name:<20} {title}", name = doc.name, title = doc.title);
    }
    std::process::exit(1);
}

/// Print the default reference's section headings and the bundled
/// sub-documents (used when the user wants to see what is available
/// without dumping the full doc).
pub fn list_sections() {
    for line in REFERENCE.lines() {
        if line.starts_with("## ") {
            println!("{line}");
        }
    }
    if !BUNDLED_DOCS.is_empty() {
        println!();
        println!("Bundled docs (run `rushi docs <name>`):");
        for doc in BUNDLED_DOCS {
            println!("  {name:<20} {title}", name = doc.name, title = doc.title);
        }
    }
}
