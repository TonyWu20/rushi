//! `hook_io` — typed payload builders for hook windows.
//!
//! Extensions that emit hook decisions (for example a `tool.after`
//! `transform`) build the stdout JSON with the helpers in this module
//! instead of hand-rolling the envelope. The kernel is the single
//! source of truth for the envelope shape. When the kernel changes a
//! contract, extensions recompile against this crate and the break is
//! caught at compile time, not at runtime.
//!
//! These helpers are plain Rust types plus a `to_stdout_json` builder.
//! They print one JSON object; the hook writes it as a single stdout
//! line. No serde derive is needed because this crate has no serde
//! dependency.

use serde_json::Value;

/// The `tool.after` `transform` decision payload.
///
/// A transform rewrites individual tool results by call id. The
/// kernel splices each entry into the routed results. Calls the hook
/// does not list keep their routed result. A transform that lists
/// every call id is a whole swap.
///
/// Build one with [`ToolAfterTransform::new`], chain
/// [`rewrite`](Self::rewrite) calls, and print `to_stdout_json` as the
/// hook's single stdout line.
#[derive(Clone, Debug, Default)]
pub struct ToolAfterTransform {
    results: Vec<(String, Value)>,
    reason: Option<String>,
}

impl ToolAfterTransform {
    /// Build an empty transform (no rewrites yet).
    pub fn new() -> Self {
        Self {
            results: Vec::new(),
            reason: None,
        }
    }

    /// Rewrite the routed result for one tool call.
    ///
    /// `result` is the full replacement `tool_result` event JSON.
    pub fn rewrite(mut self, call_id: impl Into<String>, result: Value) -> Self {
        self.results.push((call_id.into(), result));
        self
    }

    /// Attach a human-readable reason (logged by the kernel as an
    /// `ext_status` marker).
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    /// The decision word this payload carries.
    pub fn decision(&self) -> &'static str {
        "transform"
    }

    /// Serialize the full hook stdout line:
    /// `{"decision":"transform","payload":{"results":{
    /// "<call id>": <new tool_result JSON>, ... }}}`.
    ///
    /// An optional `reason` field is added when set.
    pub fn to_stdout_json(&self) -> String {
        let mut results_map = serde_json::Map::new();
        for (id, result) in &self.results {
            results_map.insert(id.clone(), result.clone());
        }
        let mut payload = serde_json::Map::new();
        payload.insert("results".to_string(), Value::Object(results_map));
        if let Some(r) = &self.reason {
            payload.insert("reason".to_string(), Value::String(r.clone()));
        }
        let out = serde_json::json!({
            "decision": "transform",
            "payload": Value::Object(payload),
        });
        out.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_serializes_the_expected_envelope() {
        let result = serde_json::json!({
            "v": 1,
            "type": "tool_result",
            "id": "call-1",
            "value": { "type": "image", "data": "AAA=" },
            "is_error": false,
        });
        let out = ToolAfterTransform::new()
            .rewrite("call-1", result)
            .with_reason("compressed 17 MB image to 0.5 MB");
        let json = out.to_stdout_json();
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["decision"], "transform");
        let results = parsed["payload"]["results"].as_object().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results["call-1"]["value"]["type"], "image");
        assert_eq!(parsed["payload"]["reason"], "compressed 17 MB image to 0.5 MB");
        assert_eq!(out.decision(), "transform");
    }

    #[test]
    fn empty_transform_omits_the_reason_field() {
        let out = ToolAfterTransform::new();
        let parsed: Value = serde_json::from_str(&out.to_stdout_json()).unwrap();
        assert_eq!(parsed["payload"]["results"].as_object().unwrap().len(), 0);
        assert!(parsed["payload"].get("reason").is_none());
    }
}
