//! `rushi docs` — print the embedded harness reference.
//!
//! The full reference document is compiled into the binary at build
//! time via `include_str!`, so it is always available regardless of
//! install method (Nix store, `install.sh`, dev checkout).
//!
//! `rushi docs` prints the entire document.
//! `rushi docs <section>` prints a single section, matched by
//! heading number or title substring (case-insensitive).

/// The full reference document, baked in at compile time.
const REFERENCE: &str =
    include_str!("../../../docs/reference/README.md");

/// Print the reference document, optionally filtered to one section.
///
/// When `section` is `None` or empty, the full document is printed.
/// Otherwise, sections are matched by:
///   1. exact match on the heading number (e.g. `"7"` matches
///      `## 7. Prompt Fragments & Extensions`)
///   2. case-insensitive substring match on the heading title
///      (e.g. `"fragments"` matches section 7)
///
/// If no section matches, an error is printed to stderr and the
/// exit code is 1.
pub fn print_docs(section: Option<&str>) {
    if section.is_none() || section.map_or(false, |s| s.is_empty()) {
        print!("{REFERENCE}");
        return;
    }

    let query = section.unwrap_or("");
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

    // No match. List available sections to help the user.
    eprintln!(
        "rushi docs: no section matching {query:?}. Available sections:"
    );
    for (heading, _) in &sections {
        eprintln!("  {heading}");
    }
    std::process::exit(1);
}

/// Print just the list of section headings (used when the user wants
/// to see what sections exist without dumping the full doc).
pub fn list_sections() {
    for line in REFERENCE.lines() {
        if line.starts_with("## ") {
            println!("{line}");
        }
    }
}
