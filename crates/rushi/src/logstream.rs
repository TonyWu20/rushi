//! `logstream` — the zstd stream layer of the session log.
//!
//! The session log (`events.jsonl`, `tools.jsonl`) is one zstd
//! stream: a concatenation of zstd frames. Every append writes one
//! frame through `LogLine::commit` (docs/session-log-compression.md,
//! property P1). The idle consolidation pass (P5, P7) rewrites the
//! stream as one level-19 frame. Legacy plain-text logs keep
//! working: the reader sniffs the four zstd magic bytes (P2, P3).

use std::io;
use std::path::Path;

/// The four zstd magic bytes. A session log carrying them is a
/// zstd stream; anything else is legacy plain text.
pub const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// The per-commit compression level (P1). Measured 3.2x on the
/// event log. The level is not the lever; the window is.
pub const PER_COMMIT_LEVEL: i32 = 9;

/// The idle consolidation level (P5). Measured 10x on events and
/// 50x on the tool log.
pub const CONSOLIDATE_LEVEL: i32 = 19;

/// Compress one log line into one zstd frame.
///
/// The input holds the whole line plus its trailing newline. The
/// output is one complete frame: a valid zstd stream of its own
/// and a valid tail of any zstd stream.
pub fn compress_frame(bytes: &[u8]) -> io::Result<Vec<u8>> {
    zstd::encode_all(bytes, PER_COMMIT_LEVEL)
}

/// True when the head of `buf` carries the zstd magic bytes.
pub fn is_zstd_stream(buf: &[u8]) -> bool {
    buf.starts_with(&ZSTD_MAGIC[..])
}

/// Read a session log into JSONL text.
///
/// Plain-text logs read verbatim. Zstd streams decompress in
/// frame order. A truncated final frame stops the read at the
/// last complete frame; the partial line drops at parse time
/// (P4). A missing file is an error, as before.
pub fn read_log_text(path: &Path) -> io::Result<String> {
    let buf = std::fs::read(path)?;
    if !is_zstd_stream(&buf) {
        return String::from_utf8(buf).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "session log is not valid UTF-8",
            )
        });
    }
    decompress_stream(path, &buf)
}

/// Strictly decode one zstd stream. Any truncation fails.
fn decode_strict(buf: &[u8]) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let mut dec = zstd::stream::read::Decoder::new(buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let mut out = Vec::with_capacity(buf.len() * 8);
    dec.read_to_end(&mut out).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, format!("zstd: {e}"))
    })?;
    Ok(out)
}

/// Decompress a zstd log stream. A torn final frame keeps every
/// complete frame and reports the truncation (P4).
fn decompress_stream(path: &Path, buf: &[u8]) -> io::Result<String> {
    use std::io::Read;
    let mut dec = zstd::stream::read::Decoder::new(buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let mut out = Vec::with_capacity(buf.len() * 8);
    // The loop fills `out` frame by frame. A truncation in the
    // final frame stops the loop; `out` keeps every complete frame
    // plus any complete block of the torn one. One stderr note, no
    // error to the caller.
    if dec.read_to_end(&mut out).is_err() {
        eprintln!(
            "logstream: {}: torn tail frame; the read stops at the last complete frame",
            path.display()
        );
    }
    String::from_utf8(out).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "zstd log stream is not valid UTF-8")
    })
}

/// Consolidate the stream at `path` into one level-19 frame.
///
/// The caller holds the log-line lock (P5). The pass reads the
/// stream, writes `<path>.consolidate`, verifies the round-trip by
/// byte compare, then renames the temp file over the original.
/// A failure deletes the temp file and reports the error.
pub fn consolidate(path: &Path) -> io::Result<()> {
    let text = read_log_text(path)?;
    let tmp = path.with_file_name(format!("{}.consolidate", path.file_name().unwrap().to_string_lossy()));
    let res = (|| -> io::Result<()> {
        use std::io::Write;
        let mut out = std::fs::File::create(&tmp)?;
        let mut enc = zstd::stream::write::Encoder::new(&mut out, CONSOLIDATE_LEVEL)?;
        enc.write_all(text.as_bytes())?;
        enc.finish()?;
        out.flush()?;
        // The round-trip check: one extra decompress, one compare.
        let check = std::fs::read(&tmp)?;
        let back = decode_strict(&check)?;
        if back != text.as_bytes() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "consolidation round-trip mismatch",
            ));
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip_is_one_complete_frame() {
        let line = r#"{"v":1,"type":"user_message","ts":"t","content":"one"}"#;
        let frame = compress_frame(format!("{line}\n").as_bytes()).unwrap();
        assert!(is_zstd_stream(&frame), "a frame starts with the magic bytes");
        let back = decode_strict(&frame).unwrap();
        assert_eq!(back, format!("{line}\n").as_bytes());
    }

    #[test]
    fn resume_appends_frames_after_a_consolidated_stream() {
        // P6: consolidate, then append. The consolidated stream
        // holds every old line; the new frame rides on after it.
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("events.jsonl");
        let a = compress_frame(b"line-a\n").unwrap();
        let b = compress_frame(b"line-b\n").unwrap();
        let mut buf = a;
        buf.extend_from_slice(&b);
        std::fs::write(&p, &buf).unwrap();
        consolidate(&p).unwrap();
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(&compress_frame(b"line-c\n").unwrap()).unwrap();
        assert_eq!(read_log_text(&p).unwrap(), "line-a\nline-b\nline-c\n");
    }

    #[test]
    fn legacy_plain_text_reads_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("events.jsonl");
        std::fs::write(&p, "plain\nline\n").unwrap();
        assert!(!is_zstd_stream(&std::fs::read(&p).unwrap()));
        assert_eq!(read_log_text(&p).unwrap(), "plain\nline\n");
    }

    #[test]
    fn torn_tail_stops_at_the_last_complete_frame() {
        // P4: the final frame is cut mid-frame. The complete frames
        // still read; the torn line drops at parse time.
        let ok = compress_frame(b"good\n").unwrap();
        let torn = compress_frame(b"tear\n").unwrap();
        let mut buf = ok.clone();
        buf.extend_from_slice(&torn[..torn.len() / 2]);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("events.jsonl");
        std::fs::write(&p, &buf).unwrap();
        let text = read_log_text(&p).unwrap();
        // The torn frame contributes nothing, or a partial line.
        // Either way the good line is whole and parseable.
        assert!(text.starts_with("good\n"), "{text:?}");
        assert!(text.is_empty() || text.starts_with("good\n"));
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.first() == Some(&"good"), "{lines:?}");
    }

    #[test]
    fn consolidate_replaces_the_stream_with_one_frame() {
        let a = compress_frame(b"x\n").unwrap();
        let b = compress_frame(b"y\n").unwrap();
        let mut buf = a;
        buf.extend_from_slice(&b);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("events.jsonl");
        std::fs::write(&p, &buf).unwrap();
        consolidate(&p).unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert!(is_zstd_stream(&raw));
        assert_eq!(read_log_text(&p).unwrap(), "x\ny\n");
        // The temp file is gone after the rename.
        assert!(!p.with_file_name("events.jsonl.consolidate").exists());
    }

    #[test]
    fn consolidate_is_idempotent_on_a_consolidated_stream() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("events.jsonl");
        std::fs::write(&p, compress_frame(b"z\n").unwrap()).unwrap();
        consolidate(&p).unwrap();
        consolidate(&p).unwrap();
        assert_eq!(read_log_text(&p).unwrap(), "z\n");
    }
}
