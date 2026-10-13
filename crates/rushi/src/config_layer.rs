//! Layered config loading (docs/project-config-precedence.md).
//!
//! A config file is one layer. When the running binary sits in a
//! Nix package layout, the store `config.toml` beside `bin/` is the
//! base layer. Every other config file is a top layer that
//! deep-merges over the base: table keys recurse, the top wins on a
//! conflict, base keys absent from the top survive. Scalars and
//! arrays: the top value replaces. No base file (dev checkout,
//! plain install) makes the merge a no-op.

use std::path::PathBuf;
use toml::{Value, map::Map};

use crate::paths::resolved_exe;

/// A layered config value: the merged result plus which layers fed it.
#[derive(Debug, Clone)]
pub struct ConfigLayers {
    /// The merged value (base under top).
    pub value: Value,
    /// The top layer, unmerged. The loop scopes its dead-def
    /// warning to this table so machine defs never warn.
    pub top: Value,
    /// The base layer path, when one was found and merged.
    pub base: Option<PathBuf>,
}

/// Locate the Nix side-by-side base layer: `<exe_dir>/../config.toml`
/// (the `resolve_config_path` tier 3 location). None in dev
/// checkouts and plain installs.
pub fn find_base_layer() -> Option<PathBuf> {
    let exe = resolved_exe()?;
    let bin_dir = exe.parent()?;
    let candidate = bin_dir.parent()?.join("config.toml");
    candidate.exists().then_some(candidate)
}

/// Deep-merge `top` over `base` (docs/project-config-precedence.md
/// merge rules): table keys recurse; on every other combination
/// (scalar, array, table-vs-other) the top value replaces; base
/// keys absent from the top survive.
pub fn deep_merge(base: &Value, top: &Value) -> Value {
    match (base, top) {
        (Value::Table(bm), Value::Table(tm)) => {
            let mut out: Map<String, Value> = bm.clone();
            for (k, v) in tm {
                match out.get(k) {
                    Some(existing) => {
                        out.insert(k.clone(), deep_merge(existing, v));
                    }
                    None => {
                        out.insert(k.clone(), v.clone());
                    }
                }
            }
            Value::Table(out)
        }
        _ => top.clone(),
    }
}

/// Load `top`, layering the Nix base under it when present.
///
/// The caller passes the resolved top-tier file (from
/// `resolve_config_path` or an explicit `--config`). A `top` that is
/// the base file itself parses alone.
pub fn load_layered(top: impl AsRef<std::path::Path>) -> Result<ConfigLayers, String> {
    load_layered_with_base(top, find_base_layer().as_deref())
}

/// The core loader with an injectable base. Tests use a fake layout
/// instead of the test binary's real exe path.
pub fn load_layered_with_base(
    top: impl AsRef<std::path::Path>,
    base: Option<impl AsRef<std::path::Path>>,
) -> Result<ConfigLayers, String> {
    let top_path = top.as_ref();
    let raw_top = std::fs::read_to_string(top_path)
        .map_err(|e| format!("cannot read config {}: {e}", top_path.display()))?;
    let top_value: Value = raw_top
        .parse()
        .map_err(|e| format!("invalid config TOML {}: {e}", top_path.display()))?;

    let mut base_path: Option<PathBuf> = None;
    let value = match base {
        Some(b) if !same_file(top_path, b.as_ref()) => {
            let b_path = b.as_ref();
            let raw_base = std::fs::read_to_string(b_path)
                .map_err(|e| format!("cannot read base config {}: {e}", b_path.display()))?;
            let base_value: Value = raw_base
                .parse()
                .map_err(|e| format!("invalid base config TOML {}: {e}", b_path.display()))?;
            base_path = Some(b_path.to_path_buf());
            deep_merge(&base_value, &top_value)
        }
        _ => top_value.clone(),
    };

    Ok(ConfigLayers {
        value,
        top: top_value,
        base: base_path,
    })
}

/// Same-file check: string equality first, then canonicalized
/// equality (the base path comes from a canonicalized exe).
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    a == b
        || std::fs::canonicalize(a)
            .ok()
            .zip(std::fs::canonicalize(b).ok())
            .map(|(x, y)| x == y)
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Value {
        s.parse().unwrap()
    }

    #[test]
    fn deep_merge_table_recurse() {
        let base: Value = r#"
[limits]
compact_enabled = true
compact_keep_tokens = 20000

[model.x]
model_id = "base-id"
base_url = "http://base"
"#
        .parse()
        .unwrap();
        let top: Value = r#"
[model.x]
model_id = "top-id"
"#
        .parse()
        .unwrap();
        let merged = deep_merge(&base, &top);
        assert_eq!(
            merged.get("model").and_then(|m| m.get("x")).and_then(|x| x.get("model_id")).and_then(|v| v.as_str()),
            Some("top-id"),
            "top scalar wins"
        );
        assert_eq!(
            merged.get("model").and_then(|m| m.get("x")).and_then(|x| x.get("base_url")).and_then(|v| v.as_str()),
            Some("http://base"),
            "base keys absent from top survive"
        );
        assert_eq!(
            merged.get("limits").and_then(|l| l.get("compact_keep_tokens")).and_then(|v| v.as_integer()),
            Some(20000),
            "unrelated base tables survive"
        );
    }

    #[test]
    fn deep_merge_scalar_and_array_replace() {
        let base: Value = r#"
[paths]
native_tool_paths = ["a", "b"]

[limits]
compact_enabled = true
"#
        .parse()
        .unwrap();
        let top: Value = r#"
[paths]
native_tool_paths = ["c"]

[limits]
compact_enabled = false
"#
        .parse()
        .unwrap();
        let merged = deep_merge(&base, &top);
        let arr = merged
            .get("paths")
            .and_then(|p| p.get("native_tool_paths"))
            .and_then(|a| a.as_array())
            .unwrap();
        assert_eq!(arr.len(), 1, "array replaces, never concats");
        assert_eq!(
            merged.get("limits").and_then(|l| l.get("compact_enabled")).and_then(|v| v.as_bool()),
            Some(false),
            "top scalar wins"
        );
    }

    #[test]
    fn load_layered_with_base_merges_and_records() {
        let dir = std::env::temp_dir().join(format!(
            "rushi-config-layer-l1-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let top = dir.join("top.toml");
        let base = dir.join("base.toml");
        std::fs::write(&top, "[system_prompt]\ntext = \"top prompt\"\n").unwrap();
        std::fs::write(
            &base,
            "[active]\nmodel = \"basemodel\"\n\n[system_prompt]\ntext = \"base prompt\"\n",
        )
        .unwrap();
        let layers = load_layered_with_base(&top, Some(&base)).unwrap();
        assert_eq!(layers.value.get("system_prompt").and_then(|s| s.get("text")).and_then(|t| t.as_str()), Some("top prompt"));
        assert_eq!(layers.value.get("active").and_then(|a| a.get("model")).and_then(|m| m.as_str()), Some("basemodel"));
        assert_eq!(layers.base.as_deref(), Some(base.as_path()));
        assert_eq!(layers.top.get("active"), None, "top stays unmerged");
    }

    #[test]
    fn load_layered_no_base_is_verbatim() {
        let dir = std::env::temp_dir().join(format!(
            "rushi-config-layer-l4-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let top = dir.join("top.toml");
        std::fs::write(&top, "[active]\nmodel = \"only\"\n").unwrap();
        let layers = load_layered_with_base(&top, None::<&std::path::Path>).unwrap();
        assert_eq!(layers.value, parse("[active]\nmodel = \"only\"\n"));
        assert_eq!(layers.base, None);
    }

    #[test]
    fn load_layered_same_file_parses_alone() {
        let dir = std::env::temp_dir().join(format!(
            "rushi-config-layer-l5-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cfg.toml");
        std::fs::write(&file, "[active]\nmodel = \"solo\"\n").unwrap();
        let layers = load_layered_with_base(&file, Some(&file)).unwrap();
        assert_eq!(layers.base, None, "no self-merge, no base record");
        assert_eq!(layers.value, parse("[active]\nmodel = \"solo\"\n"));
    }

    #[test]
    fn legacy_key_in_base_layer_is_a_load_error() {
        let dir = std::env::temp_dir().join(format!(
            "rushi-config-layer-l9-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let top = dir.join("top.toml");
        let base = dir.join("base.toml");
        std::fs::write(&top, "[system_prompt]\ntext = \"x\"\n").unwrap();
        std::fs::write(&base, "[paths]\ntools_root = \"old\"\n").unwrap();
        let layers = load_layered_with_base(&top, Some(&base)).unwrap();
        let legacy = crate::config_check::legacy_key_report(&layers.value);
        assert_eq!(legacy.len(), 1, "merged value carries the base legacy key");
    }

    #[test]
    fn load_layered_top_errors_are_err() {
        let dir = std::env::temp_dir().join(format!(
            "rushi-config-layer-l10-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("missing.toml");
        assert!(load_layered_with_base(&missing, None::<&std::path::Path>).is_err());
        let bad = dir.join("bad.toml");
        std::fs::write(&bad, "not [valid toml").unwrap();
        assert!(load_layered_with_base(&bad, None::<&std::path::Path>).is_err());
    }

    #[test]
    fn find_base_layer_is_none_in_dev_layout() {
        // The test binary lives under target/<profile>/deps/. Its
        // grandparent dir (target/<profile>) holds no config.toml
        // in a dev checkout.
        assert_eq!(find_base_layer(), None);
    }
}
