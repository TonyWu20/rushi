//! Legacy key detection for the tools_root rename.

/// Detect legacy keys in a parsed config.
///
/// Returns an error message per legacy key found. Empty means clean.
pub fn legacy_key_report(cfg: &toml::Value) -> Vec<String> {
    let mut errs = Vec::new();
    if let Some(paths) = cfg.get("paths") {
        if paths.get("tools_root").is_some() {
            errs.push(
                "config [paths] contains legacy key `tools_root` \
                 (renamed to `native_tool_paths` in commit 353424a). \
                 Update the key or run `rushi setup` to regenerate."
                    .to_string(),
            );
        }
        if paths.get("extra_tools_roots").is_some() {
            errs.push(
                "config [paths] contains legacy key `extra_tools_roots` \
                 (renamed to `extension_tool_paths` in commit 353424a). \
                 Update the key or run `rushi setup` to regenerate."
                    .to_string(),
            );
        }
    }

    // Hard cutover (issue #38): the flat `[[hooks.on]]` list is
    // retired. The reader is not dual-mode; a legacy entry is a load
    // error with a migration hint.
    if let Some(hooks) = cfg.get("hooks") {
        if hooks.get("on").is_some() {
            errs.push(
                "config [hooks] contains legacy key `on` (the flat \
                 `[[hooks.on]]` list, retired by the pipeline model, \
                 docs/loop-lifecycle-hooks.md section 12, issue #38). \
                 Migrate to `[hooks.defs.<name>]` + \
                 `[hooks.pipeline.\"<window>\"] steps = [...]`."
                    .to_string(),
            );
        }
    }
    errs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_both_legacy_keys() {
        let cfg: toml::Value =
            r#"
[paths]
tools_root = "tools"
extra_tools_roots = ["ext/goal-tools"]
"#
            .parse()
            .unwrap();
        let errs = legacy_key_report(&cfg);
        assert_eq!(errs.len(), 2);
        assert!(errs[0].contains("tools_root"));
        assert!(errs[1].contains("extra_tools_roots"));
    }

    #[test]
    fn clean_config_has_no_errors() {
        let cfg: toml::Value =
            r#"
[paths]
native_tool_paths = ["tools/bash"]
extension_tool_paths = ["ext/goal-tools"]
"#
            .parse()
            .unwrap();
        assert!(legacy_key_report(&cfg).is_empty());
    }

    #[test]
    fn no_paths_section_is_clean() {
        let cfg: toml::Value = r#"[limits]"#.parse().unwrap();
        assert!(legacy_key_report(&cfg).is_empty());
    }

    #[test]
    fn only_tools_root_flagged() {
        let cfg: toml::Value =
            r#"
[paths]
tools_root = "tools"
"#
            .parse()
            .unwrap();
        let errs = legacy_key_report(&cfg);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("tools_root"));
    }
}
