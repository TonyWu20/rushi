//! The `--delta-file` channel fills while the response is in flight
//! (docs/tui-streaming-response.md sections 1, 3.3, and 4.2).
//!
//! A fake SSE server spaces its delta events 400 ms apart. The test
//! runs the model binary against it and samples the channel file
//! while the call is still open: the file must be empty before the
//! first delta lands and strictly partial mid-stream. With the old
//! fully-buffered body read every channel line arrived in one batch
//! at the end, and this test fails.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use serde_json::Value;

/// Serve one connection: read the request, answer with a real HTTP
/// status line plus an SSE body, emit the delta events 400 ms apart,
/// and close.
fn paced_sse_server(listener: TcpListener) {
    let (mut sock, _) = listener.accept().unwrap();
    let mut hdr = [0u8; 8192];
    let _ = sock.read(&mut hdr);
    // A real response header: without the status line the HTTP client
    // rejects the body as a protocol error and the call fails fast.
    sock.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
    )
    .unwrap();
    sock.flush().unwrap();
    let events = [
        "event: response.created\n\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"He \"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"llo\"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"!\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"status\":\"completed\"}}\n\n",
    ];
    for ev in events {
        thread::sleep(Duration::from_millis(400));
        sock.write_all(ev.as_bytes()).unwrap();
        sock.flush().unwrap();
    }
}

#[test]
fn delta_file_grows_while_response_streams() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || paced_sse_server(listener));

    // A throwaway config pointing at the fake server. The loop
    // creates the stream file before the call (section 5.1), so the
    // test pre-creates it; the model truncates it at start.
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("config.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "[active]\nmodel = \"fake\"\n\n[model.fake]\nmodel_id = \"fake\"\nbase_url = \"http://127.0.0.1:{port}\"\napi_key_env = \"FAKE_MODEL_KEY\"\n"
        ),
    )
    .unwrap();
    let stream_path = tmp.path().join(".model-stream");
    std::fs::File::create(&stream_path).unwrap();

    let request = r#"{"messages":[]}"#;
    let mut child = Command::new(env!("CARGO_BIN_EXE_model"))
        .arg("--config")
        .arg(&cfg_path)
        .arg("--delta-file")
        .arg(&stream_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.as_bytes())
        .unwrap();
    let mut out_pipe = child.stdout.take().unwrap();

    // Sample the channel file while the call is in flight.
    let mut samples: Vec<u64> = Vec::new();
    let mut status: Option<std::process::ExitStatus> = None;
    while status.is_none() {
        match child.try_wait() {
            Ok(None) => {
                samples.push(std::fs::metadata(&stream_path).map(|m| m.len()).unwrap_or(0));
                thread::sleep(Duration::from_millis(50));
            }
            Ok(s) => status = s,
            Err(e) => panic!("failed to wait on model child: {e}"),
        }
    }
    let status = status.unwrap();
    assert!(status.success(), "model call must succeed: {status:?}");

    let mut stdout_buf = String::new();
    out_pipe.read_to_string(&mut stdout_buf).unwrap();
    let mut err_pipe = child.stderr.take().unwrap();
    let mut err_buf = String::new();
    err_pipe.read_to_string(&mut err_buf).unwrap();
    let stdout: serde_json::Value =
        serde_json::from_str(&stdout_buf).unwrap_or(Value::Null);

    // The channel grew while the call was in flight: empty at first,
    // strictly partial in the middle, complete only at the end.
    let final_size = std::fs::metadata(&stream_path).unwrap().len();
    assert!(
        final_size > 0,
        "the channel must carry the stream (stdout: {stdout_buf}; stderr: {err_buf})"
    );
    let first_grow = samples
        .iter()
        .position(|&s| s > 0)
        .unwrap_or_else(|| {
            panic!(
                "the channel fills during the call, not after (samples {samples:?}; \
                 stdout: {stdout_buf}; stderr: {err_buf})"
            )
        });
    assert!(
        first_grow > 0,
        "the channel is empty before the first delta lands: {samples:?}"
    );
    assert!(
        samples.iter().any(|&s| s > 0 && s < final_size),
        "a mid-stream sample must be partial: {samples:?} (final {final_size})"
    );

    // The channel lines are the deltas in arrival order plus the close
    // marker; the stdout JSON carries the settled full text.
    let stream_text = std::fs::read_to_string(&stream_path).unwrap();
    let lines = stream_text.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 4, "three deltas plus the close marker: {lines:?}");
    let mut acc = String::new();
    for l in &lines[..3] {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        assert_eq!(v["kind"], "text");
        acc.push_str(v["delta"].as_str().unwrap());
    }
    assert_eq!(acc, "He llo!");
    let done: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
    assert_eq!(done["kind"], "done");
    assert_eq!(stdout["text"], "He llo!");

    // The server thread finishes when the connection closes.
    server.join().unwrap();
}
