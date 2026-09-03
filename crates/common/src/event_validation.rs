//! `event_validation` — the one JSON schema validator for session logs.
//!
//! Migrated from `bin/log` (the superset implementation). The schema
//! directory is loaded by glob (`*.json`), not a hardcoded file list.
//! A new event type needs one schema file and zero code changes here
//! (docs/phase-2-plan.md section 6).

use std::fs;
use std::path::Path;

/// Load all `*.json` schemas from a directory. Returns `(type, schema)`
/// pairs. The event type is derived from `properties.type.const`.
pub fn load_schemas(schemas_dir: &str) -> Vec<(String, serde_json::Value)> {
    let dir = Path::new(schemas_dir);
    let mut schemas: Vec<(String, serde_json::Value)> = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.ends_with(".json") {
                continue;
            }
            let path = entry.path();
            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let val: serde_json::Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("Error: invalid schema {name}: {e}");
                    std::process::exit(1);
                }
            };
            let event_type = val
                .get("properties")
                .and_then(|p| p.get("type"))
                .and_then(|t| t.get("const"))
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            schemas.push((event_type, val));
        }
    }
    schemas
}

/// Validate a single JSON event against a set of loaded schemas.
/// Returns `Ok(())` if the event type is known and matches, or an
/// `Err` with a descriptive message.
pub fn validate_value(
    value: &serde_json::Value,
    schemas: &[(String, serde_json::Value)],
) -> Result<(), String> {
    // No schemas loaded means no validation constraint: pass through.
    if schemas.is_empty() {
        return Ok(());
    }

    let event_type = value.get("type").and_then(|t| t.as_str()).unwrap_or("");

    for (etype, schema) in schemas {
        if etype == event_type {
            if validate_against_schema(value, schema) {
                return Ok(());
            } else {
                return Err(format!(
                    "event does not match schema for event type '{event_type}'"
                ));
            }
        }
    }
    Err(format!("unknown event type '{event_type}'"))
}

/// Validate a batch of JSON lines against the schema set.
/// Returns `Ok(())` or the first error message.
pub fn validate_lines(
    lines: &[String],
    schemas: &[(String, serde_json::Value)],
) -> Result<(), String> {
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|e| format!("line {} is not valid JSON: {e}", idx + 1))?;

        if let Err(msg) = validate_value(&parsed, schemas) {
            return Err(format!("line {}: {msg}", idx + 1));
        }
    }
    Ok(())
}

/// The recursive mini JSON-schema validator (the `bin/log` superset:
/// `const`, `enum`, `required`, `properties`, `items`, and primitive
/// types).
pub fn validate_against_schema(
    value: &serde_json::Value,
    schema: &serde_json::Value,
) -> bool {
    // Check const constraint
    if let Some(const_val) = schema.get("const") {
        return value == const_val;
    }
    // Check enum constraint
    if let Some(allowed) = schema.get("enum").and_then(|e| e.as_array()) {
        return allowed.iter().any(|a| value == a);
    }
    let schema_type = schema.get("type").and_then(|t| t.as_str());
    match schema_type {
        Some("object") => {
            if let Some(obj) = value.as_object() {
                if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
                    for req in required {
                        if let Some(field) = req.as_str() {
                            if !obj.contains_key(field) {
                                return false;
                            }
                        }
                    }
                }
                if let Some(properties) = schema.get("properties") {
                    if let Some(props) = properties.as_object() {
                        for (key, prop_schema) in props {
                            if let Some(val) = obj.get(key) {
                                if !validate_against_schema(val, prop_schema) {
                                    return false;
                                }
                            }
                        }
                    }
                }
                true
            } else {
                false
            }
        }
        Some("string") => value.is_string(),
        Some("integer") => value.is_i64(),
        Some("number") => value.is_f64(),
        Some("boolean") => value.is_boolean(),
        Some("array") => {
            if let Some(arr) = value.as_array() {
                if let Some(items_schema) = schema.get("items") {
                    for item in arr {
                        if !validate_against_schema(item, items_schema) {
                            return false;
                        }
                    }
                }
                true
            } else {
                false
            }
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_schema_dir() -> std::path::PathBuf {
        // The tests run from the crate dir; the schemas live at the
        // workspace root.
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../schemas/events/v1")
    }

    #[test]
    fn glob_loads_schemas() {
        let dir = repo_schema_dir();
        let schemas = load_schemas(dir.to_str().unwrap());
        assert!(!schemas.is_empty(), "at least one schema must load");
        let types: Vec<&str> = schemas.iter().map(|(t, _)| t.as_str()).collect();
        assert!(types.contains(&"user_message"));
        assert!(types.contains(&"assistant_message"));
        assert!(types.contains(&"tool_call"));
        assert!(types.contains(&"tool_result"));
        assert!(types.contains(&"error"));
        assert!(types.contains(&"ext_status"));
    }

    #[test]
    fn user_message_validates() {
        let dir = repo_schema_dir();
        let schemas = load_schemas(dir.to_str().unwrap());
        let ok: serde_json::Value =
            serde_json::from_str(r#"{"v":1,"type":"user_message","ts":"t","content":"hi"}"#)
                .unwrap();
        assert!(validate_value(&ok, &schemas).is_ok());
    }

    #[test]
    fn unknown_type_rejects() {
        let dir = repo_schema_dir();
        let schemas = load_schemas(dir.to_str().unwrap());
        let bad: serde_json::Value =
            serde_json::from_str(r#"{"v":1,"type":"nonexistent","ts":"t"}"#).unwrap();
        assert!(validate_value(&bad, &schemas).is_err());
    }

    #[test]
    fn marker_schemas_load() {
        let dir = repo_schema_dir();
        let schemas = load_schemas(dir.to_str().unwrap());
        let types: Vec<&str> = schemas.iter().map(|(t, _)| t.as_str()).collect();
        assert!(types.contains(&"compaction_started"));
        assert!(types.contains(&"compaction_failed"));
        assert!(types.contains(&"compaction_summary"));
        assert!(types.contains(&"context_exhausted"));
    }
}
