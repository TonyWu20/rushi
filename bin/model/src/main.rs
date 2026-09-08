#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

use clap::Parser;
use rushi_common::model_settings;
use rushi_common::stage::ModelDelta;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, Read, Write};

/// Call the model API via Responses format
#[derive(Parser)]
#[command(name = "model", about = "Call the model API and output model response")]
struct Args {
    /// Path to config file
    #[arg(long, default_value = "config.toml")]
    config: String,

    /// Print the resolved call config as one JSON object and exit:
    /// the active model, the reasoning effort as sent in the
    /// request, and the 0-4 thinking level (docs/tui.md section
    /// 7.2). No API call, no stdin. The loop publishes the level
    /// as the `model_thinking` ext_status; the resolution is this
    /// binary's, so the published level matches the request.
    #[arg(long)]
    describe: bool,

    /// The session-local stream channel (docs/tui-streaming-response.md
    /// section 4.1): the loop creates this file before the call and
    /// deletes it when the call returns. One JSON line lands here per
    /// SSE delta event, in arrival order, flushed as each event is
    /// parsed: the body is read line by line, so the TUI's live block
    /// fills while the response is still in flight. The stdout
    /// contract is unchanged. The file opens truncate-at-start; an
    /// open failure warns but never fails the model call (section
    /// 4.3).
    #[arg(long)]
    delta_file: Option<String>,
}

/// The optional stream-channel writer (the `--delta-file` side
/// channel, docs/tui-streaming-response.md section 4.2). One JSON
/// line per SSE delta event; each line flushes, so the TUI reader
/// sees it as soon as the event arrives. Write failures drop
/// silently: a broken side channel must not fail the model call
/// (section 4.3).
struct StreamWriter<W: Write> {
    w: Option<W>,
}

impl<W: Write> StreamWriter<W> {
    /// Write one channel line and flush it. A `None` channel (the
    /// flag absent) is a no-op.
    fn emit(&mut self, line: &str) {
        let Some(w) = self.w.as_mut() else {
            return;
        };
        let _ = writeln!(w, "{line}");
        let _ = w.flush();
    }
}

/// Map the resolved reasoning effort to the 0-4 thinking level of
/// docs/tui.md section 7.2: 0 no thinking (the default), 1 low,
/// 2 medium, 3 high, 4+ highest. The level is what the loop
/// publishes as the `model_thinking` ext_status; the TUI colors
/// the input-area border from it. Unknown efforts map to 0: the
/// level must never claim a thinking the request does not carry.
fn thinking_level_for(effort: &str) -> u32 {
    match effort.to_ascii_lowercase().as_str() {
        "none" => 0,
        "minimal" | "low" => 1,
        "medium" => 2,
        "high" => 3,
        "xhigh" | "max" => 4,
        _ => 0,
    }
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

    // Resolve the active model and its settings via the shared module
    // (single source of truth, docs/itches.md).
    let active_model = model_settings::resolve_active_model(&config);
    let ms = model_settings::resolve_model_settings(&config, &active_model);

    let base_url = ms.base_url;
    let model_name = ms.model_id;
    let max_output_tokens = ms.max_output_tokens;
    let reasoning_effort = ms.reasoning_effort;
    let api_key_env = ms.api_key_env;
    let model_timeout_s = ms.timeout_s;

    let api_key = std::env::var(&api_key_env).unwrap_or_default();

    if args.describe {
        // The resolved call config, one JSON object. The loop reads
        // `thinking_level` and publishes it; the overflow guard reads
        // `model_id` (the request model source, the model field of
        // every assemble request). Nothing here touches the network
        // or the session log.
        let out = serde_json::json!({
            "active": active_model,
            "model_id": model_name,
            "reasoning_effort": normalize_effort(&reasoning_effort),
            "thinking_level": thinking_level_for(&normalize_effort(&reasoning_effort)),
        });
        println!("{out}");
        return;
    }

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

    // The config effort is the default. An optional request-level
    // `reasoning_effort` wins: the compaction summary call carries
    // its own (cheaper) effort (docs/auto-compact-plan.md 4.5).
    let request_effort = request.get("reasoning_effort").and_then(|v| v.as_str());
    let effort = resolve_effort(&reasoning_effort, request_effort);

    // The stream channel (docs/tui-streaming-response.md section
    // 4.1): truncate-at-start. A failed open warns on stderr and
    // continues without the channel: the side channel must never
    // fail the model call (section 4.3).
    let mut stream: StreamWriter<std::io::BufWriter<std::fs::File>> = StreamWriter { w: None };
    if let Some(p) = &args.delta_file {
        match fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(p)
        {
            Ok(f) => stream.w = Some(std::io::BufWriter::new(f)),
            Err(e) => {
                eprintln!(
                    "model: warning: cannot open stream channel {p}: {e}; \
                     continuing without the live stream"
                );
            }
        }
    }

    // Build API request
    let url = format!("{}/v1/responses", base_url);

    // Prepare the request body with the full OpenAI Responses spec
    // surface. The server must not retain state: the session log is
    // the store. The encrypted reasoning content is requested back so
    // it can round-trip verbatim. Reasoning models get the configured
    // effort with an automatic summary.
    let mut api_request = request.clone();
    // The hard-trim marker is a log record of the harness
    // (docs/auto-compact-plan.md section 9.8). It must not reach the
    // provider payload.
    if let Some(obj) = api_request.as_object_mut() {
        obj.remove("hard_trim");
    }
    api_request["stream"] = serde_json::json!(true);
    api_request["store"] = serde_json::json!(false);
    api_request["include"] = serde_json::json!(["reasoning.encrypted_content"]);
    api_request["reasoning"] = serde_json::json!({
        "effort": effort,
        "summary": "auto"
    });
    apply_output_budget(&mut api_request, max_output_tokens);

    // Try responses API first
    let result = call_responses_api(
        &url, &api_key, &api_request, &model_name, model_timeout_s, &mut stream,
    );

    match result {
        Ok(response) => {
            println!("{}", response);
        }
        Err(e) => {
            eprintln!("Error: model API call failed: {e}");
            // Output error event with the failure detail. The parse step
            // includes the detail in the logged error message. The
            // stream channel, if open, keeps its partial lines: the
            // loop deletes the file when the call ends, and the TUI
            // settles its live buffer on the error event.
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

/// "off" is not an API effort value; it normalizes to "none". The
/// normalized value is what the request carries, and it is the
/// value `--describe` reports.
fn normalize_effort(effort: &str) -> String {
    if effort.eq_ignore_ascii_case("off") {
        "none".to_string()
    } else {
        effort.to_string()
    }
}

/// The effective effort of one call: the request-level value wins
/// over the config value. Both normalize "off" to "none".
fn resolve_effort(config_effort: &str, request_effort: Option<&str>) -> String {
    match request_effort {
        Some(e) => normalize_effort(e),
        None => normalize_effort(config_effort),
    }
}

/// The config output cap is the default. A request-provided
/// `max_output_tokens` wins: the handoff summary request carries a
/// cheap cap of its own (correction 57).
fn apply_output_budget(request: &mut serde_json::Value, config_max: u64) {
    if request.get("max_output_tokens").is_none() {
        request["max_output_tokens"] = serde_json::json!(config_max);
    }
}

/// Build the HTTP client for model calls.
///
/// Streaming SSE responses can run for minutes on thinking models. The
/// reqwest blocking default applies a 30 second overall request timeout,
/// which cuts long streams short and silently turns them into empty
/// turns. Bound the overall call to `timeout_s` instead: long streams
/// stay safe, and a stalled provider (accepted connection, no response)
/// cannot hang the loop or a compaction summary call forever.
/// `timeout_s = 0` disables the cap (the pre-timeout behavior).
fn build_client(timeout_s: u64) -> reqwest::blocking::Client {
    let timeout = if timeout_s > 0 {
        Some(std::time::Duration::from_secs(timeout_s))
    } else {
        None
    };
    match reqwest::blocking::Client::builder()
        .timeout(timeout)
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

fn call_responses_api<W: Write>(
    url: &str,
    api_key: &str,
    request: &serde_json::Value,
    model_name: &str,
    timeout_s: u64,
    stream: &mut StreamWriter<W>,
) -> Result<String, String> {
    let client = build_client(timeout_s);

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
                    return call_chat_completions(
                        url, api_key, request, model_name, timeout_s, stream,
                    );
                }
                let body = resp.text().unwrap_or_default();
                return Err(format!("API returned status {}: {}", status, body));
            }

            // Stream the SSE body line by line (docs/tui-streaming-response.md
            // sections 1 and 4.2): a fully buffered read would hold every
            // channel line until the stream ends, and the TUI's live block
            // would never show in-progress text. A failed body read is a
            // transport failure: report it, and keep the channel's partial
            // lines for the TUI to settle on the error event.
            let mut parser = SseParser::new();
            let mut reader = std::io::BufReader::new(resp);
            let mut line = String::new();
            loop {
                line.clear();
                let n = reader
                    .read_line(&mut line)
                    .map_err(|e| format!("Failed to read response stream: {e}"))?;
                if n == 0 {
                    break;
                }
                parser.feed_line(&line, stream);
            }
            parser.finalize(stream)
        }
        Err(e) => Err(format!("Request failed: {e} (debug: {e:?})")),
    }
}

fn call_chat_completions<W: Write>(
    base_url: &str,
    api_key: &str,
    request: &serde_json::Value,
    _model_name: &str,
    timeout_s: u64,
    stream: &mut StreamWriter<W>,
) -> Result<String, String> {
    let url = format!("{}/v1/chat/completions", base_url);
    let client = build_client(timeout_s);

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
                return Err(format!(
                    "Chat completions API returned status {}: {}",
                    status, body
                ));
            }
            let body = match resp.text() {
                Ok(b) => b,
                Err(e) => {
                    return Err(format!("Failed to read response: {e}"));
                }
            };
            parse_chat_response(&body, stream)
        }
        Err(e) => Err(format!("Request failed: {e}")),
    }
}

fn convert_to_chat_format(request: &serde_json::Value) -> serde_json::Value {
    let mut messages: Vec<serde_json::Value> = Vec::new();

    // A reasoning item rides into the chat format as the deepseek
    // thinking format: its thinking text becomes the reasoning_content
    // of the assistant message that follows it.
    let mut pending_reasoning: Option<String> = None;

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
                    let content = item.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    let mut message = serde_json::json!({
                        "role": role,
                        "content": content
                    });
                    if role == "assistant" {
                        if let Some(thinking) = pending_reasoning.take() {
                            message["reasoning_content"] = serde_json::json!(thinking);
                        }
                    }
                    messages.push(message);
                }
                Some("function_call") => {
                    let call_id = item.get("call_id").and_then(|c| c.as_str()).unwrap_or("");
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
                    let call_id = item.get("call_id").and_then(|c| c.as_str()).unwrap_or("");
                    let output = item.get("output").and_then(|o| o.as_str()).unwrap_or("");
                    messages.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": output
                    }));
                }
                Some("reasoning") => {
                    let mut thinking = String::new();
                    for list in ["content", "summary"] {
                        if let Some(parts) = item.get(list).and_then(|p| p.as_array()) {
                            for part in parts {
                                if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                                    thinking.push_str(t);
                                }
                            }
                        }
                    }
                    if !thinking.is_empty() {
                        pending_reasoning = Some(thinking);
                    }
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

/// The SSE response state machine (docs/tui-streaming-response.md
/// section 4.2). The HTTP path feeds the response body one line at a
/// time as it arrives; the channel line for each delta is emitted the
/// moment that event is parsed, so the TUI's live block fills while
/// the model is still generating, not after the stream ends.
struct SseParser {
    text: String,
    tool_calls: Vec<serde_json::Value>,
    stop_reason: String,
    usage: Option<serde_json::Value>,
    final_response: Option<serde_json::Value>,
    // A well-formed SGLang/OpenAI stream ends with exactly one terminal
    // event: response.completed, response.incomplete, or response.failed.
    // If none arrives, the stream was cut short (network or server side).
    // That is a transport failure, not an empty model turn.
    saw_terminal: bool,
    // One `done` line per stream, on the first terminal event (or in
    // `finalize`, on a cut stream): the channel's close marker.
    done_emitted: bool,

    // Streaming fallback state: item_id -> function name / arguments
    fc_names: HashMap<String, String>,
    fc_args: HashMap<String, String>,
    fc_order: Vec<String>,

    // Reasoning capture. The terminal event holds the complete items
    // and is authoritative. The delta stream is the fallback for a cut
    // stream. Items are kept verbatim, pi-style: content, encrypted
    // content, id, status, and summary all survive so the next
    // request can send them back unchanged.
    reasoning_order: Vec<String>,
    reasoning_items: HashMap<String, serde_json::Value>,
    reasoning_text: HashMap<String, String>,
    reasoning_done: HashSet<String>,
}

impl SseParser {
    fn new() -> SseParser {
        SseParser {
            text: String::new(),
            tool_calls: Vec::new(),
            stop_reason: "stop".to_string(),
            usage: None,
            final_response: None,
            saw_terminal: false,
            done_emitted: false,
            fc_names: HashMap::new(),
            fc_args: HashMap::new(),
            fc_order: Vec::new(),
            reasoning_order: Vec::new(),
            reasoning_items: HashMap::new(),
            reasoning_text: HashMap::new(),
            reasoning_done: HashSet::new(),
        }
    }

    /// Feed one line of the response body. The channel line for a
    /// delta is emitted right here, as the line is parsed.
    fn feed_line<W: Write>(&mut self, line: &str, stream: &mut StreamWriter<W>) {
        if !line.starts_with("data: ") {
            return;
        }
        let data = &line[6..];

        if data.trim() == "[DONE]" {
            return;
        }

        let event: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return,
        };

        let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match event_type {
            "response.output_text.delta" => {
                if let Some(delta) = event.get("delta").and_then(|d| d.as_str()) {
                    self.text.push_str(delta);
                    stream.emit(&ModelDelta::Text(delta.to_string()).to_json_line());
                }
            }
            "response.output_item.added" | "response.output_item.done" => {
                if let Some(item) = event.get("item") {
                    match item.get("type").and_then(|t| t.as_str()) {
                        Some("function_call") => {
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
                            self.fc_names.entry(item_id.clone()).or_insert(name);
                            if let Some(args) = item.get("arguments").and_then(|a| a.as_str()) {
                                self.fc_args.insert(item_id.clone(), args.to_string());
                            }
                            if !self.fc_order.contains(&item_id) {
                                self.fc_order.push(item_id);
                            }
                        }
                        Some("reasoning") => {
                            let item_id = item
                                .get("id")
                                .and_then(|id| id.as_str())
                                .unwrap_or("")
                                .to_string();
                            if !self.reasoning_order.contains(&item_id) {
                                self.reasoning_order.push(item_id.clone());
                            }
                            self.reasoning_items.insert(item_id, item.clone());
                        }
                        _ => {}
                    }
                }
            }
            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                let item_id = event
                    .get("item_id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(delta) = event.get("delta").and_then(|d| d.as_str()) {
                    self.reasoning_text
                        .entry(item_id.clone())
                        .or_default()
                        .push_str(delta);
                    stream.emit(
                        &ModelDelta::Reasoning {
                            item_id: item_id.clone(),
                            delta: delta.to_string(),
                        }
                        .to_json_line(),
                    );
                }
                if !self.reasoning_order.contains(&item_id) {
                    self.reasoning_order.push(item_id);
                }
            }
            "response.reasoning_text.done" | "response.reasoning_summary_text.done" => {
                let item_id = event
                    .get("item_id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(text) = event.get("text").and_then(|t| t.as_str()) {
                    self.reasoning_text
                        .insert(item_id.clone(), text.to_string());
                }
                self.reasoning_done.insert(item_id.clone());
                if !self.reasoning_order.contains(&item_id) {
                    self.reasoning_order.push(item_id);
                }
            }
            "response.function_call_arguments.delta" => {
                let item_id = event
                    .get("item_id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(delta) = event.get("delta").and_then(|d| d.as_str()) {
                    self.fc_args
                        .entry(item_id.clone())
                        .or_default()
                        .push_str(delta);
                    // The item event may not have named the call yet; the
                    // name fills in as soon as it is known.
                    let name = self.fc_names.get(&item_id).cloned().unwrap_or_default();
                    stream.emit(
                        &ModelDelta::ToolCallDelta {
                            call_id: item_id.clone(),
                            name,
                            args_delta: delta.to_string(),
                        }
                        .to_json_line(),
                    );
                }
                if !self.fc_order.contains(&item_id) {
                    self.fc_order.push(item_id);
                }
            }
            "response.function_call_arguments.done" => {
                let item_id = event
                    .get("item_id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(args) = event.get("arguments").and_then(|a| a.as_str()) {
                    self.fc_args.insert(item_id.clone(), args.to_string());
                }
                if !self.fc_order.contains(&item_id) {
                    self.fc_order.push(item_id);
                }
            }
            "response.completed" | "response.incomplete" | "response.failed" => {
                if event_type != "response.completed" {
                    self.stop_reason = if event_type == "response.incomplete" {
                        "length".to_string()
                    } else {
                        "error".to_string()
                    };
                }
                self.saw_terminal = true;
                self.final_response = event.get("response").cloned();
                if !self.done_emitted {
                    stream.emit(
                        &ModelDelta::Done {
                            stop_reason: self.stop_reason.clone(),
                        }
                        .to_json_line(),
                    );
                    self.done_emitted = true;
                }
            }
            _ => {}
        }
    }

    /// Close the stream: apply the terminal-event fallbacks, emit the
    /// channel's close marker when no terminal event did, and build
    /// the stdout JSON.
    fn finalize<W: Write>(&mut self, stream: &mut StreamWriter<W>) -> Result<String, String> {
        // The terminal event carries the full response object. Use it
        // as authoritative. The streaming deltas are only a fallback.
        let mut reasoning_out: Vec<serde_json::Value> = Vec::new();
        let final_response = self.final_response.take();
        if let Some(resp) = &final_response {
            if let Some(u) = resp.get("usage") {
                self.usage = Some(u.clone());
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
                                .and_then(|id| id.as_str())
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
                        // The reasoning item is the server's own item. Keep
                        // it verbatim: content, encrypted content, id,
                        // status, and summary all carry over to the next
                        // request unchanged.
                        Some("reasoning") => {
                            reasoning_out.push(item.clone());
                        }
                        _ => {}
                    }
                }
            }
            if !final_text.is_empty() {
                self.text = final_text;
            }
            if !final_calls.is_empty() {
                self.tool_calls = final_calls;
            }
        } else {
            // Fall back to streaming accumulation if no terminal response
            for item_id in &self.fc_order {
                let name = self.fc_names.get(item_id).cloned().unwrap_or_default();
                let args = self
                    .fc_args
                    .get(item_id)
                    .cloned()
                    .unwrap_or_else(|| "{}".to_string());
                self.tool_calls.push(serde_json::json!({
                    "id": item_id,
                    "name": name,
                    "arguments": args
                }));
            }
        }

        // The terminal event may carry no reasoning items: a failed or cut
        // stream still streamed the thinking deltas. Rebuild the items
        // from the delta state when the terminal event gave none.
        if reasoning_out.is_empty() && !self.reasoning_order.is_empty() {
            for item_id in &self.reasoning_order {
                reasoning_out.push(reasoning_item_fallback(
                    item_id,
                    &self.reasoning_items,
                    &self.reasoning_text,
                    &self.reasoning_done,
                ));
            }
        }

        // Build output. The parser state is fully consumed here.
        let text = std::mem::take(&mut self.text);
        let tool_calls = std::mem::take(&mut self.tool_calls);
        let stop_reason = std::mem::take(&mut self.stop_reason);
        let usage = match self.usage.take() {
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
            "reasoning": reasoning_out,
            "stop_reason": stop_reason,
            "usage": usage
        });

        // Reclassify a "stop" or "tool_calls" response whose tool-call
        // arguments are truncated JSON as a length stop. Some local
        // model servers (e.g. vLLM) report `response.completed` even
        // when the output hit the token cap mid-arguments; without
        // this the parse stage would hard-fail on "malformed tool
        // arguments" instead of treating it as a recoverable
        // truncation.
        if matches!(stop_reason.as_str(), "stop" | "tool_calls") {
            for tc in &tool_calls {
                if let Some(args_str) = tc.get("arguments").and_then(|a| a.as_str()) {
                    if is_truncated_json(args_str) {
                        output["stop_reason"] = serde_json::json!("length");
                        output["detail"] = serde_json::json!(
                            "Tool-call arguments were truncated (incomplete JSON); the response hit the output token limit."
                        );
                        break;
                    }
                }
            }
        }

        // Mark a cut stream as an error with a detail, so the caller
        // can tell a truncated transport apart from a genuine empty
        // model turn.
        if !self.saw_terminal && output["stop_reason"] == "stop" {
            output["stop_reason"] = serde_json::json!("error");
            output["detail"] = serde_json::json!(
                "SSE stream ended without a terminal event (response.completed/incomplete/failed); the response was truncated."
            );
        }

        // The stream closed without a terminal event: close the
        // channel too, so a reader stops polling without waiting for
        // the log event.
        if !self.done_emitted {
            stream.emit(
                &ModelDelta::Done {
                    stop_reason: "error".to_string(),
                }
                .to_json_line(),
            );
        }

        Ok(serde_json::to_string(&output).unwrap())
    }
}

/// Parse a complete SSE body at once. Test-only: the HTTP path in
/// `call_responses_api` drives [`SseParser`] line by line instead, so
/// the delta channel fills while the response is in flight.
#[cfg(test)]
fn parse_sse_response<W: Write>(
    body: &str,
    stream: &mut StreamWriter<W>,
) -> Result<String, String> {
    let mut parser = SseParser::new();
    for line in body.lines() {
        parser.feed_line(line, stream);
    }
    parser.finalize(stream)
}

/// Rebuild one reasoning item from the streaming fallback state.
///
/// The item event may hold the id and the summary only, with an
/// empty content. The delta stream holds the thinking text. Merge
/// the two, and mark the item completed when its text is done.
fn reasoning_item_fallback(
    item_id: &str,
    items: &HashMap<String, serde_json::Value>,
    text: &HashMap<String, String>,
    done: &HashSet<String>,
) -> serde_json::Value {
    let mut item = items.get(item_id).cloned().unwrap_or_else(|| {
        serde_json::json!({
            "type": "reasoning",
            "id": item_id,
            "status": "in_progress",
            "summary": [],
            "encrypted_content": null,
            "content": []
        })
    });
    // Fill the content from the delta stream when the item event held
    // none: missing or empty content means the deltas carry the text.
    let content_empty = item
        .get("content")
        .and_then(|c| c.as_array())
        .is_none_or(|a| a.is_empty());
    if content_empty {
        let thinking = text.get(item_id).cloned().unwrap_or_default();
        item["content"] = serde_json::json!([{
            "type": "reasoning_text",
            "text": thinking
        }]);
    }
    if done.contains(item_id) {
        item["status"] = serde_json::json!("completed");
    }
    item["type"] = serde_json::json!("reasoning");
    item
}

/// Returns true when `s` looks like a JSON string that was cut off
/// mid-structure: it is not valid JSON and has more opening than
/// closing brackets/braces, or ends inside an open string literal.
/// A string that parses as valid JSON (even if not an object) is NOT
/// considered truncated — that is a genuine model error, not a cut.
fn is_truncated_json(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }
    // Already valid JSON? Not a truncation.
    if serde_json::from_str::<serde_json::Value>(s).is_ok() {
        return false;
    }
    // Walk the string counting unmatched openers, skipping the
    // contents of JSON string literals.
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escaped = false;
    for ch in s.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else {
            match ch {
                '"' => in_string = true,
                '{' | '[' => depth += 1,
                '}' | ']' => depth -= 1,
                _ => {}
            }
        }
    }
    // Still inside a string or has unclosed structures.
    in_string || depth > 0
}

fn parse_chat_response<W: Write>(
    body: &str,
    stream: &mut StreamWriter<W>,
) -> Result<String, String> {
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

    // Normalize the completions usage to the responses names. The
    // event log carries `usage.input_tokens` on every
    // assistant_message; the token-driven budget (work item B) reads
    // that field on both API paths.
    let mut usage_map: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    if let Some(u) = usage {
        if let Some(v) = u.get("prompt_tokens").and_then(|v| v.as_u64()) {
            usage_map.insert("input_tokens".to_string(), serde_json::json!(v));
        }
        if let Some(v) = u.get("completion_tokens").and_then(|v| v.as_u64()) {
            usage_map.insert("output_tokens".to_string(), serde_json::json!(v));
        }
    }
    let usage_norm: Option<serde_json::Value> = if usage_map.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(usage_map))
    };

    // The completions fallback speaks the deepseek thinking format:
    // the thinking text rides in message.reasoning_content. Capture
    // it as a reasoning item so the event log carries the thinking
    // on this path too.
    let mut reasoning: Vec<serde_json::Value> = Vec::new();
    if let Some(rc) = chat_resp
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("reasoning_content"))
        .and_then(|r| r.as_str())
    {
        if !rc.is_empty() {
            reasoning.push(serde_json::json!({
                "type": "reasoning",
                "id": "reasoning_chat",
                "status": "completed",
                "content": [{ "type": "reasoning_text", "text": rc }],
                "summary": [],
                "encrypted_content": null
            }));
        }
    }

    // The completions fallback is non-streaming: no delta lines, but
    // the `done` marker still tells a reader the channel is closed
    // (docs/tui-streaming-response.md section 4.2).
    stream.emit(
        &ModelDelta::Done {
            stop_reason: stop_reason.to_string(),
        }
        .to_json_line(),
    );

    let output = serde_json::json!({
        "text": text,
        "tool_calls": tool_calls,
        "reasoning": reasoning,
        "stop_reason": stop_reason,
        "usage": usage_norm
    });

    Ok(serde_json::to_string(&output).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A disabled channel (tests): `parse_*` writes nothing to it.
    fn empty_stream() -> StreamWriter<Vec<u8>> {
        StreamWriter { w: None }
    }

    /// A channel that records its lines into `out` (tests).
    fn capture_stream(out: &mut Vec<u8>) -> StreamWriter<&mut Vec<u8>> {
        StreamWriter { w: Some(out) }
    }

    /// A complete stream: deltas plus the terminal response.completed event.
    fn completed_stream() -> String {
        let mut s = String::new();
        s.push_str("event: response.created\n");
        s.push_str("data: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\n");
        s.push_str("data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello \"}\n\n");
        s.push_str("data: {\"type\":\"response.output_text.delta\",\"delta\":\"world\"}\n\n");
        s.push_str(
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\n",
        );
        s
    }

    #[test]
    fn complete_stream_parses_clean() {
        let out: serde_json::Value = serde_json::from_str(
            &parse_sse_response(&completed_stream(), &mut empty_stream()).unwrap(),
        )
        .unwrap();
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
            serde_json::from_str(&parse_sse_response(&cut, &mut empty_stream()).unwrap()).unwrap();
        assert_eq!(out["stop_reason"], "error");
        assert_eq!(out["text"], "hello world");
        assert!(out["detail"].as_str().unwrap().contains("truncated"));
    }

    #[test]
    fn empty_body_reports_error() {
        let out: serde_json::Value =
            serde_json::from_str(&parse_sse_response("", &mut empty_stream()).unwrap()).unwrap();
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
            serde_json::from_str(&parse_sse_response(&s, &mut empty_stream()).unwrap()).unwrap();
        assert_eq!(out["stop_reason"], "length");
        assert!(out.get("detail").is_none());
    }

    #[test]
    fn failed_stream_reports_error() {
        let s = "data: {\"type\":\"response.failed\",\"response\":{\"id\":\"r1\",\"status\":\"failed\"}}\n\n";
        let out: serde_json::Value =
            serde_json::from_str(&parse_sse_response(s, &mut empty_stream()).unwrap()).unwrap();
        assert_eq!(out["stop_reason"], "error");
        assert!(out.get("detail").is_none());
    }

    /// The terminal event holds a reasoning item. The parser keeps it
    /// verbatim: every key survives into the output reasoning field.
    #[test]
    fn terminal_stream_captures_reasoning_item_verbatim() {
        let mut s = String::new();
        s.push_str("data: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\n");
        s.push_str(
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"status\":\"completed\",\"output\":[",
        );
        s.push_str(
            "{\"id\":\"rs_1\",\"type\":\"reasoning\",\"status\":\"completed\",\"content\":[{\"type\":\"reasoning_text\",\"text\":\"step one\"}],\"summary\":[],\"encrypted_content\":\"enc-42\",\"type2\":\"extra\"}",
        );
        s.push_str(
            ",{\"id\":\"m1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"done\"}]}],\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}}",
        );
        s.push_str("\n\n");
        let out: serde_json::Value =
            serde_json::from_str(&parse_sse_response(&s, &mut empty_stream()).unwrap()).unwrap();
        assert_eq!(out["text"], "done");
        let reasoning = out["reasoning"].as_array().expect("reasoning is an array");
        assert_eq!(reasoning.len(), 1);
        let item = &reasoning[0];
        // Every key of the server item survives, including extras.
        assert_eq!(item["id"], "rs_1");
        assert_eq!(item["status"], "completed");
        assert_eq!(item["encrypted_content"], "enc-42");
        assert_eq!(item["content"][0]["text"], "step one");
        assert_eq!(item["type2"], "extra");
    }

    /// A cut stream: no terminal event. The reasoning item rebuilds
    /// from the item event plus the reasoning_text delta stream.
    #[test]
    fn cut_stream_rebuilds_reasoning_from_deltas() {
        let mut s = String::new();
        s.push_str(
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"rs_9\",\"type\":\"reasoning\",\"content\":[],\"summary\":[],\"encrypted_content\":null,\"status\":\"in_progress\"}}\n\n",
        );
        s.push_str(
            "data: {\"type\":\"response.reasoning_text.delta\",\"item_id\":\"rs_9\",\"delta\":\"think \"}\n\n",
        );
        s.push_str(
            "data: {\"type\":\"response.reasoning_text.delta\",\"item_id\":\"rs_9\",\"delta\":\"harder\"}\n\n",
        );
        let out: serde_json::Value =
            serde_json::from_str(&parse_sse_response(&s, &mut empty_stream()).unwrap()).unwrap();
        assert_eq!(out["stop_reason"], "error");
        let item = &out["reasoning"][0];
        assert_eq!(item["type"], "reasoning");
        assert_eq!(item["id"], "rs_9");
        assert_eq!(item["status"], "in_progress");
        assert_eq!(item["content"][0]["type"], "reasoning_text");
        assert_eq!(item["content"][0]["text"], "think harder");
    }

    /// A reasoning item with no text at all stays empty, not a crash.
    #[test]
    fn reasoning_item_without_text_is_empty_content() {
        let mut s = String::new();
        s.push_str(
            "data: {\"type\":\"response.reasoning_text.delta\",\"item_id\":\"rs_0\",\"delta\":\"\"}\n\n",
        );
        s.push_str("data: {\"type\":\"response.failed\",\"response\":{\"id\":\"r1\"}}\n\n");
        let out: serde_json::Value =
            serde_json::from_str(&parse_sse_response(&s, &mut empty_stream()).unwrap()).unwrap();
        let item = &out["reasoning"][0];
        assert_eq!(
            item["content"]
                .as_array()
                .expect("content is an array")
                .len(),
            1
        );
        assert_eq!(item["content"][0]["text"], "");
    }

    /// The completions fallback carries the deepseek thinking text.
    #[test]
    fn chat_response_captures_reasoning_content() {
        let body = r#"{"choices":[{"message":{"content":"done","reasoning_content":"the plan"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3}}"#;
        let out: serde_json::Value =
            serde_json::from_str(&parse_chat_response(body, &mut empty_stream()).unwrap()).unwrap();
        let item = &out["reasoning"][0];
        assert_eq!(item["type"], "reasoning");
        assert_eq!(item["content"][0]["text"], "the plan");
    }

    /// A completions response without thinking carries an empty list.
    #[test]
    fn chat_response_without_thinking_has_empty_reasoning() {
        let body = r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]}"#;
        let out: serde_json::Value =
            serde_json::from_str(&parse_chat_response(body, &mut empty_stream()).unwrap()).unwrap();
        assert!(out["reasoning"].as_array().unwrap().is_empty());
    }

    /// The config output cap is the default; a request-provided cap
    /// wins (the handoff summary request, correction 57).
    #[test]
    fn output_budget_honors_request_value() {
        let mut request = serde_json::json!({"model": "m"});
        apply_output_budget(&mut request, 32768);
        assert_eq!(request["max_output_tokens"], 32768);
        let mut request = serde_json::json!({"model": "m", "max_output_tokens": 4096});
        apply_output_budget(&mut request, 32768);
        assert_eq!(request["max_output_tokens"], 4096, "the request cap wins");
    }

    /// The completions usage normalizes to the responses names, so
    /// both API paths feed the same `usage.input_tokens` field that
    /// the token-driven budget reads (work item B).
    #[test]
    fn chat_response_normalizes_usage_to_input_tokens() {
        let body = r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":120,"completion_tokens":7}}"#;
        let out: serde_json::Value =
            serde_json::from_str(&parse_chat_response(body, &mut empty_stream()).unwrap()).unwrap();
        assert_eq!(out["usage"]["input_tokens"], 120);
        assert_eq!(out["usage"]["output_tokens"], 7);
        assert!(out["usage"].get("prompt_tokens").is_none());
    }

    /// A completions response without usage reports null, as before.
    #[test]
    fn chat_response_without_usage_is_null() {
        let body = r#"{"choices":[{"message":{"content":"ok"}}]}"#;
        let out: serde_json::Value =
            serde_json::from_str(&parse_chat_response(body, &mut empty_stream()).unwrap()).unwrap();
        assert_eq!(out["usage"], serde_json::Value::Null);
    }

    /// The channel gets one line per SSE delta, in arrival order, and
    /// closes with a done marker. The stdout JSON is unchanged by
    /// the channel (docs/tui-streaming-response.md P1 / P2).
    #[test]
    fn channel_gets_one_line_per_sse_delta() {
        let mut s = String::new();
        s.push_str(
            "data: {\"type\":\"response.reasoning_text.delta\",\"item_id\":\"rs_1\",\"delta\":\"think \"}\n\n",
        );
        s.push_str("data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello \"}\n\n");
        s.push_str("data: {\"type\":\"response.output_text.delta\",\"delta\":\"world\"}\n\n");
        s.push_str(
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"name\":\"bash\",\"status\":\"in_progress\"}}\n\n",
        );
        s.push_str(
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"echo hi\"}\n\n",
        );
        s.push_str(
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"status\":\"completed\"}}\n\n",
        );

        let mut buf: Vec<u8> = Vec::new();
        let out = parse_sse_response(&s, &mut capture_stream(&mut buf)).unwrap();

        let lines = String::from_utf8(buf).unwrap();
        let parsed: Vec<serde_json::Value> = lines
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            parsed.len(),
            5,
            "one line per delta plus the done marker: {lines}"
        );
        assert_eq!(parsed[0]["kind"], "reasoning");
        assert_eq!(parsed[0]["item_id"], "rs_1");
        assert_eq!(parsed[0]["delta"], "think ");
        assert_eq!(parsed[1]["kind"], "text");
        assert_eq!(parsed[1]["delta"], "Hello ");
        assert_eq!(parsed[2]["kind"], "text");
        assert_eq!(parsed[2]["delta"], "world");
        assert_eq!(parsed[3]["kind"], "tool_call_delta");
        assert_eq!(parsed[3]["call_id"], "fc_1");
        assert_eq!(parsed[3]["name"], "bash");
        assert_eq!(parsed[3]["args_delta"], "echo hi");
        assert_eq!(parsed[4]["kind"], "done");
        assert_eq!(parsed[4]["stop_reason"], "stop");

        // The stdout contract is unchanged: a disabled channel
        // yields the identical JSON.
        let plain = parse_sse_response(&s, &mut empty_stream()).unwrap();
        assert_eq!(out, plain);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["text"], "Hello world");
    }

    /// The channel line for a delta lands the moment its line is fed
    /// (docs/tui-streaming-response.md section 4.2): the reader sees
    /// the stream in flight, never a batch at the end. Producer-level
    /// mirror of `ex_stream_hello` in lean/TuiStreamSpec.lean
    /// ("He" then "llo" converges to "Hello").
    #[test]
    fn sse_parser_emits_channel_lines_as_lines_arrive() {
        let mut out: Vec<u8> = Vec::new();
        let mut parser = SseParser::new();

        // The handle is block-scoped so the channel can be read between
        // feeds: that is the in-flight property under test.
        {
            let mut stream = capture_stream(&mut out);
            parser.feed_line(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"He\"}",
                &mut stream,
            );
        }
        // One line landed, and it is the first delta. The JSON key
        // layout is not part of the contract, so compare fields.
        let first_str = String::from_utf8(out.to_vec()).unwrap();
        let first_lines: Vec<&str> = first_str.lines().collect();
        assert_eq!(
            first_lines.len(),
            1,
            "exactly one channel line after the first feed: {first_str}"
        );
        let first: serde_json::Value = serde_json::from_str(first_lines[0]).unwrap();
        assert_eq!(first["kind"], "text");
        assert_eq!(first["delta"], "He");

        {
            let mut stream = capture_stream(&mut out);
            parser.feed_line(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"llo\"}",
                &mut stream,
            );
        }
        let two_str = String::from_utf8(out.to_vec()).unwrap();
        let two_lines: Vec<serde_json::Value> = two_str
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(two_lines.len(), 2, "both deltas on the channel: {two_str}");
        assert_eq!(two_lines[0]["delta"], "He");
        assert_eq!(two_lines[1]["delta"], "llo");

        let json: serde_json::Value;
        {
            let mut stream = capture_stream(&mut out);
            parser.feed_line(
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"status\":\"completed\"}}",
                &mut stream,
            );
            json = serde_json::from_str(&parser.finalize(&mut stream).unwrap()).unwrap();
        }
        assert_eq!(json["text"], "Hello");
        let settled_str = String::from_utf8(out).unwrap();
        let settled: Vec<&str> = settled_str.lines().collect();
        assert_eq!(settled.len(), 3, "two deltas plus the close marker: {settled:?}");
        assert_eq!(settled[2], r#"{"kind":"done","stop_reason":"stop"}"#);
    }

    /// An empty response (a terminal event, no deltas) settles exactly
    /// one channel line, the close marker. Producer-level mirror of
    /// `ex_empty_response` in lean/TuiStreamSpec.lean.
    #[test]
    fn sse_parser_empty_response_settles_done_only() {
        let mut out: Vec<u8> = Vec::new();
        let mut parser = SseParser::new();
        let json: serde_json::Value;
        {
            let mut stream = capture_stream(&mut out);
            parser.feed_line(
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"status\":\"completed\"}}",
                &mut stream,
            );
            json = serde_json::from_str(&parser.finalize(&mut stream).unwrap()).unwrap();
        }
        assert_eq!(json["text"], "");
        let settled_str = String::from_utf8(out).unwrap();
        let settled: Vec<&str> = settled_str.lines().collect();
        assert_eq!(settled, vec![r#"{"kind":"done","stop_reason":"stop"}"#]);
    }

    /// A cut stream still closes the channel with an error marker: a
    /// reader stops polling without waiting for the log event.
    #[test]
    fn cut_stream_closes_the_channel_with_error() {
        let mut s = String::new();
        s.push_str("data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n");
        let mut buf: Vec<u8> = Vec::new();
        parse_sse_response(&s, &mut capture_stream(&mut buf)).unwrap();
        let lines = String::from_utf8(buf).unwrap();
        let parsed: Vec<serde_json::Value> = lines
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["kind"], "text");
        assert_eq!(parsed[0]["delta"], "partial");
        assert_eq!(parsed[1]["kind"], "done");
        assert_eq!(parsed[1]["stop_reason"], "error");
    }

    /// The non-streaming completions fallback emits no deltas, only
    /// the close marker.
    #[test]
    fn chat_path_emits_only_the_done_line() {
        let body = r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]}"#;
        let mut buf: Vec<u8> = Vec::new();
        parse_chat_response(body, &mut capture_stream(&mut buf)).unwrap();
        let lines = String::from_utf8(buf).unwrap();
        let parsed: Vec<serde_json::Value> = lines
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["kind"], "done");
        assert_eq!(parsed[0]["stop_reason"], "stop");
    }

    /// A reasoning item converts to the deepseek thinking format: its
    /// text lands in the reasoning_content of the assistant message
    /// that follows it.
    #[test]
    fn convert_to_chat_attaches_reasoning_content() {
        let request = serde_json::json!({
            "model": "m",
            "input": [
                {"type": "message", "role": "user", "content": "hi"},
                {"type": "reasoning", "id": "rs_1", "content": [
                    {"type": "reasoning_text", "text": "plan A"}
                ], "summary": [], "status": "completed", "encrypted_content": null},
                {"type": "message", "role": "assistant", "content": "doing"}
            ]
        });
        let chat = convert_to_chat_format(&request);
        let msgs = chat["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["reasoning_content"], "plan A");
    }

    // The 0-4 scale of docs/tui.md section 7.2: the TUI colors the
    // input-area border from this level, so the mapping is the
    // host palette's.
    #[test]
    fn thinking_level_maps_the_documented_efforts() {
        let cases = [
            ("none", 0),
            ("minimal", 1),
            ("low", 1),
            ("medium", 2),
            ("high", 3),
            ("xhigh", 4),
            ("max", 4),
        ];
        for (effort, level) in cases {
            assert_eq!(thinking_level_for(effort), level, "{effort}");
        }
    }

    #[test]
    fn thinking_level_is_case_insensitive_and_unknown_is_zero() {
        assert_eq!(
            thinking_level_for("XHIGH"),
            4,
            "effort is compared case-insensitively"
        );
        assert_eq!(
            thinking_level_for("turbo"),
            0,
            "an unknown effort claims no thinking"
        );
        assert_eq!(thinking_level_for(""), 0);
    }

    /// An optional request-level effort wins over the config value;
    /// the config value stands when the request carries none.
    #[test]
    fn resolve_effort_prefers_the_request_value() {
        assert_eq!(
            resolve_effort("xhigh", None),
            "xhigh",
            "no request value: the config effort"
        );
        assert_eq!(
            resolve_effort("xhigh", Some("low")),
            "low",
            "the request value wins"
        );
        assert_eq!(
            resolve_effort("xhigh", Some("off")),
            "none",
            "the request off normalizes"
        );
        assert_eq!(
            resolve_effort("off", None),
            "none",
            "the config off normalizes"
        );
    }

    /// A terminal stream whose tool-call arguments are cut off mid-JSON
    /// reclassifies to a length stop, so the parse stage treats it as a
    /// recoverable truncation instead of a hard malformed-args failure.
    #[test]
    fn completed_stream_with_truncated_args_reports_length() {
        let mut s = String::new();
        s.push_str(
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"status\":\"completed\",\"output\":[{\"id\":\"fc_1\",\"type\":\"function_call\",\"name\":\"edit\",\"arguments\":\"{\\\"file_path\\\": \\\"/tmp/x.rs\\\", \\\"old\\\"\"}]}}\n\n",
        );
        let out: serde_json::Value =
            serde_json::from_str(&parse_sse_response(&s, &mut empty_stream()).unwrap()).unwrap();
        assert_eq!(out["stop_reason"], "length");
        assert!(out["detail"].as_str().unwrap().contains("truncated"));
    }

    /// The same terminal stream with well-formed arguments keeps the
    /// stop reason: a valid JSON object is not a truncation.
    #[test]
    fn completed_stream_with_valid_args_stays_stop() {
        let mut s = String::new();
        s.push_str(
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"status\":\"completed\",\"output\":[{\"id\":\"fc_1\",\"type\":\"function_call\",\"name\":\"bash\",\"arguments\":\"{\\\"command\\\": \\\"ls\\\"}\"}]}}\n\n",
        );
        let out: serde_json::Value =
            serde_json::from_str(&parse_sse_response(&s, &mut empty_stream()).unwrap()).unwrap();
        assert_eq!(out["stop_reason"], "stop");
        assert!(out.get("detail").is_none());
    }

    /// When no terminal event arrives, the streaming delta args are the
    /// fallback; truncated deltas are reclassified as length.
    #[test]
    fn streaming_fallback_truncated_args_reports_length() {
        let mut s = String::new();
        s.push_str(
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"name\":\"edit\"}}\n\n",
        );
        s.push_str(
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"{\\\"file_path\\\": \\\"/tmp/x.rs\\\", \\\"old\"}\n\n",
        );
        // No terminal event: the stream was cut mid-arguments.
        let out: serde_json::Value =
            serde_json::from_str(&parse_sse_response(&s, &mut empty_stream()).unwrap()).unwrap();
        assert_eq!(out["stop_reason"], "length");
        assert!(out["detail"].as_str().unwrap().contains("truncated"));
    }

    #[test]
    fn is_truncated_json_flags_cut_off_structures() {
        assert!(is_truncated_json("{\"file_path\": \"/tmp/x.rs\", \"old"));
        assert!(is_truncated_json("{\"a\": [1, 2, 3"));
        assert!(is_truncated_json("{\"a\": {"));
        assert!(is_truncated_json("{\"key\": \"unterminated"));
    }

    #[test]
    fn is_truncated_json_rejects_complete_and_non_json() {
        assert!(!is_truncated_json("{\"command\": \"ls\"}"));
        assert!(!is_truncated_json("{}"));
        assert!(!is_truncated_json(""));
        // A complete string literal is valid JSON: not a truncation.
        assert!(!is_truncated_json("\"hello\""));
        // Plain text that is not JSON is not a truncated JSON value.
        assert!(!is_truncated_json("hello world"));
    }
}
