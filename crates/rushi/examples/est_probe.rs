// Probe estimate_from_events on a real session log.
// cargo run -p rushi-common --example est_probe -- <events.jsonl> [cpts]
use rushi_common::compact_math::estimate_from_events;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path_str = args.get(1).expect("usage: est_probe <events.jsonl> [cpts]");
    let cpts: u64 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(4);
    let data = std::fs::read_to_string(path_str).expect("read events.jsonl");
    let events: Vec<serde_json::Value> = data
        .lines()
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect();
    let est = estimate_from_events(&events, cpts);
    println!("events={} cpts={} estimate_from_events={est}", events.len(), cpts);
    // Show the last few measured readings in the kept region for context.
    let mut last = Vec::new();
    for v in &events {
        if v.get("type").and_then(|t| t.as_str()) == Some("assistant_message")
            && v.get("usage").and_then(|u| u.get("input_tokens")).and_then(|i| i.as_u64()).is_some()
        {
            let in_t = v["usage"]["input_tokens"].as_u64().unwrap();
            let out_t = v["usage"]["output_tokens"].as_u64().unwrap_or(0);
            let ts = v.get("ts").and_then(|t| t.as_str()).unwrap_or("");
            last.push((in_t, out_t, ts.to_string()));
        }
    }
    if let Some((i, o, ts)) = last.last() {
        println!("last measured: input={} output={} ts={} (in+out={})", i, o, ts, i + o);
    }
    println!("total measured assistant msgs with usage: {}", last.len());
}
