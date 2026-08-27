#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use clap::Parser;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};

/// Call the model API via Responses format
#[derive(Parser)]
#[command(name = "model", about = "Call the model API and output model response")]
struct Args {
    /// Path to config file
    #[arg(long, default_value = "config.toml")]
    config: String,
}

/// Resolve the active model name from the MODEL env var or config.
fn resolve_active_model(config: &toml::Value) -> String {
    std::env::var("MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            config
                .get("active")
                .and_then(|a| a.get("model"))
                .and_then(|m| m.as_str())
                .unwrap_or("deepseek")
                .to_string()
        })
}

fn val_str(v: &toml::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

fn val_int(v: &toml::Value, key: &str) -> Option<i64> {
    v.get(key).and_then(|x| x.as_integer())
}

fn main() {
    let args = Args::parse();

    let config_path = &args.config;
    let config_content = match fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: cannot read config: {e}");
            std::process::exit(1);
        }
    };

    let config: toml::Value = match config_content.parse() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error: invalid config TOML: {e}");
            std::process::exit(1);
        }
    };

    // Resolve the active model and its settings.
    let active_model = resolve_active_model(&config);
    let empty = toml::Value::Table(toml::map::Map::new());
    let model_root = config.get("model").unwrap_or(&empty);
    let mdl = model_root.get(&active_model).unwrap_or(&empty);

    let base_url = val_str(mdl, "base_url")
        .unwrap_or_else(|| "http://127.0.0.1:8080".to_string());

    let model_name = val_str(mdl, "model_id").unwrap_or_else(|| active_model.clone());

    let max_output_tokens = val_int(mdl, "max_output_tokens")
        .or_else(|| val_int(model_root, "max_output_tokens"))
        .unwrap_or(4096) as u64;

    let reasoning_effort = val_str(mdl, "reasoning_effort")
        .or_else(|| val_str(model_root, "reasoning_effort"))
        .unwrap_or_else(|| "medium".to_string());

    let api_key_env = val_str(mdl, "api_key_env")
        .unwrap_or_else(|| "MODEL_API_KEY".to_string());

    let api_key = std::env::var(api_key_env).unwrap_or_default();

    // Read model request from stdin
    let mut request_str = String::new();
    io::stdin()
        .read_to_string(&mut request_str)
        .expect("Failed to read stdin");

    let request: serde_json::Value = match serde_json::from_str(&request_str) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error: invalid model request JSON: {e}");
            std::process::exit(1);
        }
    };

    // Build API request
    let url = format!("{}/v1/responses", base_url);

    // Prepare request body
    let mut api_request = request.clone();
    api_request["max_output_tokens"] = serde_json::json!(max_output_tokens);
    api_request["stream"] = serde_json::json!(true);
    api_request["reasoning"] = serde_json::json!({
        "effort": reasoning_effort
    });

    // Try responses API first
    let result = call_responses_api(&url, &api_key, &api_request, &model_name);

    match result {
        Ok(response) => {
            println!("{}", response);
        }
        Err(e) => {
            eprintln!("Error: model API call failed: {e}");
            // Output error event with the failure detail. The parse step
            // includes the detail in the logged error message.
            let error_event = serde_json::json!({
                "text": "",
                "tool_calls": [],
                "stop_reason": "error",
                "usage": null,
                "detail": e
            });
            println!("{}", error_event);
        }
    }
}

/// Build the HTTP client for model calls.
///
/// Streaming SSE responses can run for minutes on thinking models. The
/// reqwest blocking default applies a 30 second overall request timeout,
/// which cuts long streams short and silently turns them into empty
/// turns. Disable the overall timeout for this client and bound only the
/// connect phase.
fn build_client() -> reqwest::blocking::Client {
    match reqwest::blocking::Client::builder()
        .timeout(None)
        .connect_timeout(std::time::Duration::from_secs(30))
        .pool_idle_timeout(std::time::Duration::from_secs(60))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: failed to build HTTP client: {e:?}");
            reqwest::blocking::Client::new()
        }
    }
}

fn call_responses_api(
    url: &str,
    api_key: &str,
    request: &serde_json::Value,
    model_name: &str,
) -> Result<String, String> {
    let client = build_client();

    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", api_key))
        .body(serde_json::to_string(request).unwrap())
        .send();

    match response {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                // Check if it's a 404 or 405 for fallback
                if status == 404 || status == 405 {
                    return call_chat_completions(url, api_key, request, model_name);
                }
                let body = resp.text().unwrap_or_default();
                return Err(format!("API returned status {}: {}", status, body));
            }

            // Parse SSE stream. A failed body read is a transport failure.
            // Report it instead of feeding an empty body to the parser.
            let body = match resp.text() {
                Ok(b) => b,
                Err(e) => return Err(format!("Failed to read response stream: {e}")),
            };
            parse_sse_response(&body)
        }
        Err(e) => Err(format!("Request failed: {e} (debug: {e:?})")),
    }
}

fn call_chat_completions(
    base_url: &str,
    api_key: &str,
    request: &serde_json::Value,
    _model_name: &str,
) -> Result<String, String> {
    let url = format!("{}/v1/chat/completions", base_url);
    let client = build_client();

    // Convert responses format to chat completions format
    let chat_request = convert_to_chat_format(request);

    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", api_key))
        .body(serde_json::to_string(&chat_request).unwrap())
        .send();

    match response {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().unwrap_or_default();
                return Err(format!("Chat completions API returned status {}: {}", status, body));
            }
            let body = match resp.text() {
                Ok(b) => b,
                Err(e) => {
                    return Err(format!("Failed to read response: {e}"));
                }
            };
            parse_chat_response(&body)
        }
        Err(e) => Err(format!("Request failed: {e}")),
    }
}

fn convert_to_chat_format(request: &serde_json::Value) -> serde_json::Value {
    let mut messages: Vec<serde_json::Value> = Vec::new();

    // Instructions become the system message
    if let Some(inst) = request.get("instructions").and_then(|i| i.as_str()) {
        if !inst.is_empty() {
            messages.push(serde_json::json!({
                "role": "system",
                "content": inst
            }));
        }
    }

    // Map Responses input items to chat messages
    if let Some(input) = request.get("input").and_then(|i| i.as_array()) {
        for item in input {
            match item.get("type").and_then(|t| t.as_str()) {
                Some("message") => {
                    let role = item.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                    let content = item
                        .get("content")
                        .and_then(|c| c.as_str())
                        .unwrap_or("");
                    messages.push(serde_json::json!({
                        "role": role,
                        "content": content
                    }));
                }
                Some("function_call") => {
                    let call_id = item
                        .get("call_id")
                        .and_then(|c| c.as_str())
                        .unwrap_or("");
                    let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let arguments = item
                        .get("arguments")
                        .and_then(|a| a.as_str())
                        .unwrap_or("{}");
                    // Merge into the trailing assistant message
                    if let Some(last) = messages.last_mut() {
                        if last.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                            let mut tcs: Vec<serde_json::Value> = last
                                .get("tool_calls")
                                .and_then(|t| t.as_array())
                                .cloned()
                                .unwrap_or_default();
                            tcs.push(serde_json::json!({
                                "id": call_id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": arguments
                                }
                            }));
                            last["tool_calls"] = serde_json::json!(tcs);
                            continue;
                        }
                    }
                    // No trailing assistant message: emit a standalone one
                    messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": "",
                        "tool_calls": [{
                            "id": call_id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": arguments
                            }
                        }]
                    }));
                }
                Some("function_call_output") => {
                    let call_id = item
                        .get("call_id")
                        .and_then(|c| c.as_str())
                        .unwrap_or("");
                    let output = item
                        .get("output")
                        .and_then(|o| o.as_str())
                        .unwrap_or("");
                    messages.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": output
                    }));
                }
                _ => {}
            }
        }
    }

    // Tools: convert Responses top-level name to chat nested function
    let tools = request.get("tools").cloned().map(|arr| {
        if let Some(a) = arr.as_array() {
            serde_json::json!(a
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.get("name"),
                            "description": t.get("description"),
                            "parameters": t.get("parameters")
                        }
                    })
                })
                .collect::<Vec<_>>())
        } else {
            arr
        }
    });

    serde_json::json!({
        "model": request.get("model").and_then(|m| m.as_str()).unwrap_or(""),
        "messages": messages,
        "tools": tools,
        "stream": false
    })
}

fn parse_sse_response(body: &str) -> Result<String, String> {
    let mut text = String::new();
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
    let mut stop_reason = "stop";
    let mut usage: Option<serde_json::Value> = None;
    let mut final_response: Option<serde_json::Value> = None;
    // A well-formed SGLang/OpenAI stream ends with exactly one terminal
    // event: response.completed, response.incomplete, or response.failed.
    // If none arrives, the stream was cut short (network or server side).
    // That is a transport failure, not an empty model turn.
    let mut saw_terminal = false;

    // Streaming fallback state: item_id -> function name / arguments
    let mut fc_names: HashMap<String, String> = HashMap::new();
    let mut fc_args: HashMap<String, String> = HashMap::new();
    let mut fc_order: Vec<String> = Vec::new();

    for line in body.lines() {
        if !line.starts_with("data: ") {
            continue;
        }
        let data = &line[6..];

        if data.trim() == "[DONE]" {
            continue;
        }

        let event: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match event_type {
            "response.output_text.delta" => {
                if let Some(delta) = event.get("delta").and_then(|d| d.as_str()) {
                    text.push_str(delta);
                }
            }
            "response.output_item.added" | "response.output_item.done" => {
                if let Some(item) = event.get("item") {
                    if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                        let item_id = item
                            .get("id")
                            .and_then(|id| id.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        fc_names.entry(item_id.clone()).or_insert(name);
                        if let Some(args) = item
                            .get("arguments")
                            .and_then(|a| a.as_str())
                        {
                            fc_args.insert(item_id.clone(), args.to_string());
                        }
                        if !fc_order.contains(&item_id) {
                            fc_order.push(item_id);
                        }
                    }
                }
            }
            "response.function_call_arguments.delta" => {
                let item_id = event
                    .get("item_id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(delta) = event.get("delta").and_then(|d| d.as_str()) {
                    fc_args.entry(item_id.clone()).or_default().push_str(delta);
                }
                if !fc_order.contains(&item_id) {
                    fc_order.push(item_id);
                }
            }
            "response.function_call_arguments.done" => {
                let item_id = event
                    .get("item_id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(args) = event.get("arguments").and_then(|a| a.as_str()) {
                    fc_args.insert(item_id.clone(), args.to_string());
                }
                if !fc_order.contains(&item_id) {
                    fc_order.push(item_id);
                }
            }
            "response.completed" | "response.incomplete" => {
                if event_type == "response.incomplete" {
                    stop_reason = "length";
                }
                saw_terminal = true;
                final_response = event.get("response").cloned();
            }
            "response.failed" => {
                stop_reason = "error";
                saw_terminal = true;
                final_response = event.get("response").cloned();
            }
            _ => {}
        }
    }

    // The terminal event carries the full response object. Use it as
    // authoritative. The streaming deltas are only a fallback.
    if let Some(resp) = &final_response {
        if let Some(u) = resp.get("usage") {
            usage = Some(u.clone());
        }
        let mut final_text = String::new();
        let mut final_calls: Vec<serde_json::Value> = Vec::new();
        if let Some(output) = resp.get("output").and_then(|o| o.as_array()) {
            for item in output {
                match item.get("type").and_then(|t| t.as_str()) {
                    Some("message") => {
                        if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                            for part in content {
                                if part.get("type").and_then(|t| t.as_str())
                                    == Some("output_text")
                                {
                                    if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                                        final_text.push_str(t);
                                    }
                                }
                            }
                        }
                    }
                    Some("function_call") => {
                        let item_id = item
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let call_id = item
                            .get("call_id")
                            .and_then(|c| c.as_str())
                            .unwrap_or(&item_id)
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let arguments = item
                            .get("arguments")
                            .and_then(|a| a.as_str())
                            .unwrap_or("{}")
                            .to_string();
                        final_calls.push(serde_json::json!({
                            "id": call_id,
                            "name": name,
                            "arguments": arguments
                        }));
                    }
                    _ => {}
                }
            }
        }
        if !final_text.is_empty() {
            text = final_text;
        }
        if !final_calls.is_empty() {
            tool_calls = final_calls;
        }
    } else {
        // Fall back to streaming accumulation if no terminal response
        for item_id in &fc_order {
            let name = fc_names.get(item_id).cloned().unwrap_or_default();
            let args = fc_args
                .get(item_id)
                .cloned()
                .unwrap_or_else(|| "{}".to_string());
            tool_calls.push(serde_json::json!({
                "id": item_id,
                "name": name,
                "arguments": args
            }));
        }
    }

    // Build output
    let usage = match usage {
        Some(u) => {
            let mut map = serde_json::Map::new();
            if let Some(v) = u.get("input_tokens").and_then(|t| t.as_u64()) {
                map.insert("input_tokens".to_string(), serde_json::json!(v));
            }
            if let Some(v) = u.get("output_tokens").and_then(|t| t.as_u64()) {
                map.insert("output_tokens".to_string(), serde_json::json!(v));
            }
            if let Some(v) = u
                .get("input_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|t| t.as_u64())
            {
                map.insert("cached_tokens".to_string(), serde_json::json!(v));
            }
            if map.is_empty() {
                None
            } else {
                Some(serde_json::Value::Object(map))
            }
        }
        None => None,
    };

    let mut output = serde_json::json!({
        "text": text,
        "tool_calls": tool_calls,
        "stop_reason": stop_reason,
        "usage": usage
    });

    // Mark a cut stream as an error with a detail, so the caller can tell a
    // truncated transport apart from a genuine empty model turn.
    if !saw_terminal {
        output["stop_reason"] = serde_json::json!("error");
        output["detail"] = serde_json::json!(
            "SSE stream ended without a terminal event (response.completed/incomplete/failed); the response was truncated."
        );
    }

    Ok(serde_json::to_string(&output).unwrap())
}

fn parse_chat_response(body: &str) -> Result<String, String> {
    let chat_resp: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return Err(format!("Invalid chat response JSON: {e}")),
    };

    let text = chat_resp
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    let stop_reason = chat_resp
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("finish_reason"))
        .and_then(|r| r.as_str())
        .unwrap_or("stop");

    // Extract tool calls from the chat completions message
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
    if let Some(tcs) = chat_resp
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("tool_calls"))
        .and_then(|t| t.as_array())
    {
        for tc in tcs {
            let tc_id = tc.get("id").and_then(|id| id.as_str()).unwrap_or("");
            let name = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            let arguments = tc
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|a| a.as_str())
                .unwrap_or("{}");
            tool_calls.push(serde_json::json!({
                "id": tc_id,
                "name": name,
                "arguments": arguments
            }));
        }
    }

    let usage = chat_resp.get("usage");

    let output = serde_json::json!({
        "text": text,
        "tool_calls": tool_calls,
        "stop_reason": stop_reason,
        "usage": usage
    });

    Ok(serde_json::to_string(&output).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A complete stream: deltas plus the terminal response.completed event.
    fn completed_stream() -> String {
        let mut s = String::new();
        s.push_str("event: response.created\n");
        s.push_str("data: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\n");
        s.push_str(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello \"}\n\n",
        );
        s.push_str(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"world\"}\n\n",
        );
        s.push_str(
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\n",
        );
        s
    }

    #[test]
    fn complete_stream_parses_clean() {
        let out: serde_json::Value =
            serde_json::from_str(&parse_sse_response(&completed_stream()).unwrap()).unwrap();
        assert_eq!(out["stop_reason"], "stop");
        assert_eq!(out["text"], "hello world");
        assert!(out.get("detail").is_none());
        assert_eq!(out["usage"]["input_tokens"], 10);
    }

    /// The same stream cut after the last text delta: no terminal event.
    #[test]
    fn cut_stream_reports_error_with_detail() {
        let stream = completed_stream();
        let cut = stream
            .split("\"type\":\"response.completed\"")
            .next()
            .unwrap()
            .to_string();
        let out: serde_json::Value =
            serde_json::from_str(&parse_sse_response(&cut).unwrap()).unwrap();
        assert_eq!(out["stop_reason"], "error");
        assert_eq!(out["text"], "hello world");
        assert!(out["detail"].as_str().unwrap().contains("truncated"));
    }

    #[test]
    fn empty_body_reports_error() {
        let out: serde_json::Value =
            serde_json::from_str(&parse_sse_response("").unwrap()).unwrap();
        assert_eq!(out["stop_reason"], "error");
        assert!(out["detail"].as_str().is_some());
    }

    #[test]
    fn incomplete_stream_reports_length() {
        let mut s = String::new();
        s.push_str("data: {\"type\":\"response.in_progress\",\"response\":{\"id\":\"r1\"}}\n\n");
        s.push_str(
            "data: {\"type\":\"response.incomplete\",\"response\":{\"id\":\"r1\",\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"},\"output\":[]}}\n\n",
        );
        let out: serde_json::Value =
            serde_json::from_str(&parse_sse_response(&s).unwrap()).unwrap();
        assert_eq!(out["stop_reason"], "length");
        assert!(out.get("detail").is_none());
    }

    #[test]
    fn failed_stream_reports_error() {
        let s = "data: {\"type\":\"response.failed\",\"response\":{\"id\":\"r1\",\"status\":\"failed\"}}\n\n";
        let out: serde_json::Value =
            serde_json::from_str(&parse_sse_response(s).unwrap()).unwrap();
        assert_eq!(out["stop_reason"], "error");
        assert!(out.get("detail").is_none());
    }
}

