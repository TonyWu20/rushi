//! `logstream` — the zstd stream layer of the session log.
//!
//! The session log (`events.jsonl`, `tools.jsonl`) is one zstd
//! stream: a concatenation of zstd frames. Every append writes one
//! frame through `LogLine::commit` (docs/session-log-compression.md,
//! property P1). The idle consolidation pass (P5, P7) rewrites the
//! stream as one level-19 frame. Legacy plain-text logs keep
//! working: the reader sniffs the four zstd magic bytes (P2, P3).
//!
//! P8: the frame index sidecar. `LogLine::commit` appends one
//! 32-byte index record to the `<log>.idx` sidecar in the same
//! locked section as the frame. The reader side is `open_indexed`
//! plus the `LogStream` tail and seq reads, and the no-index
//! `read_log_tail` window scan (the tv-rushi decision-84
//! algorithm). A session without the sidecar takes the fallback:
//! the window scan, then the full read. The log stays the
//! authority; a broken index drops and falls back.

use std::io;
use std::path::{Path, PathBuf};

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

/// Consolidate the stream at `path` into one level-19 frame, and
/// rewrite its frame index sidecar in the same pass (P5, P8).
///
/// The caller holds the log-line lock (P5). The pass reads the
/// stream, writes `<path>.consolidate`, verifies the round-trip by
/// byte compare, then renames the temp file over the original. The
/// index gets one record for the consolidated frame: seq from 1,
/// the whole stream, the whole decoded text. The index re-reads
/// and re-parses its own bytes before its rename, which lands
/// after the stream rename: a crash between the two renames leaves
/// an index that fails its open-time validation, so the read falls
/// back to the full text and the next commit heals the pair. A
/// failure deletes the temp files and reports the error.
pub fn consolidate(path: &Path) -> io::Result<()> {
    let text = read_log_text(path)?;
    let idx = index_path(path);
    let tmp = path.with_file_name(format!("{}.consolidate", path.file_name().unwrap().to_string_lossy()));
    let tmp_idx = idx.with_file_name(format!("{}.consolidate", idx.file_name().unwrap().to_string_lossy()));
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
        // The index pair: one record for the consolidated frame.
        let stream_len = std::fs::metadata(path)?.len();
        let record = IndexRecord {
            seq_start: 1,
            frame_offset: 0,
            frame_len: stream_len,
            text_len: text.len() as u64,
        };
        let idx_bytes = encode_index(std::slice::from_ref(&record));
        std::fs::write(&tmp_idx, &idx_bytes)?;
        // The index round trip: one re-read, one parse, one compare.
        let idx_back = read_index(&tmp_idx)?;
        if idx_back != [record] {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "consolidation index round-trip mismatch",
            ));
        }
        std::fs::rename(&tmp_idx, &idx)?;
        Ok(())
    })();
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&tmp_idx);
        return Err(e);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// P8: the frame index sidecar and the shared tail reader
// ---------------------------------------------------------------------------

/// One frame index record: `seq_start frame_offset frame_len
/// text_len`. Four little-endian u64s, 32 bytes per commit.
///
/// `seq_start` is the 1-based line number of the first line in the
/// record's byte range. `frame_offset` and `frame_len` locate the
/// range in the stream. `text_len` is the decoded byte length. The
/// record is a derived fact: the log stays the authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexRecord {
    pub seq_start: u64,
    pub frame_offset: u64,
    pub frame_len: u64,
    pub text_len: u64,
}

/// One index record on disk: 4 x u64, little-endian.
pub const INDEX_RECORD_LEN: usize = 32;

/// The sidecar path for the log at `path`: `<log>.idx`.
pub fn index_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        "{}.idx",
        path.file_name().unwrap().to_string_lossy()
    ))
}

/// Encode one record to its 32-byte on-disk form.
pub fn index_record_bytes(r: &IndexRecord) -> [u8; INDEX_RECORD_LEN] {
    let mut out = [0u8; INDEX_RECORD_LEN];
    out[0..8].copy_from_slice(&r.seq_start.to_le_bytes());
    out[8..16].copy_from_slice(&r.frame_offset.to_le_bytes());
    out[16..24].copy_from_slice(&r.frame_len.to_le_bytes());
    out[24..32].copy_from_slice(&r.text_len.to_le_bytes());
    out
}

/// Encode a whole index: records in order, 32 bytes each.
pub fn encode_index(records: &[IndexRecord]) -> Vec<u8> {
    let mut out = Vec::with_capacity(records.len() * INDEX_RECORD_LEN);
    for r in records {
        out.extend_from_slice(&index_record_bytes(r));
    }
    out
}

/// Load the index sidecar at `path`.
///
/// A missing file yields an empty list. A torn tail (a partial
/// final record) drops the partial record only, the same P4 rule
/// as a torn frame. A structurally broken index (a seq that does
/// not start at 1, a seq step, a frame-offset gap) yields an
/// empty list: the log stays the authority, and a broken index
/// never fails a read.
pub fn read_index(path: &Path) -> io::Result<Vec<IndexRecord>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    // Drop a torn final record: `as_chunks` keeps the whole
    // 32-byte records and leaves the torn tail as the remainder.
    let (chunks, _torn) = bytes.as_chunks::<INDEX_RECORD_LEN>();
    let mut records = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        records.push(IndexRecord {
            seq_start: u64::from_le_bytes(chunk[0..8].try_into().unwrap()),
            frame_offset: u64::from_le_bytes(chunk[8..16].try_into().unwrap()),
            frame_len: u64::from_le_bytes(chunk[16..24].try_into().unwrap()),
            text_len: u64::from_le_bytes(chunk[24..32].try_into().unwrap()),
        });
    }
    if index_structurally_valid(&records) {
        Ok(records)
    } else {
        Ok(Vec::new())
    }
}

/// The structural checks over the records alone: seq from 1,
/// strictly increasing seq, contiguous frame ranges. A broken
/// index yields false; the caller drops it.
fn index_structurally_valid(records: &[IndexRecord]) -> bool {
    if records.is_empty() {
        return true;
    }
    if records[0].seq_start != 1 {
        return false;
    }
    for pair in records.windows(2) {
        if pair[1].seq_start <= pair[0].seq_start {
            return false;
        }
        if pair[1].frame_offset != pair[0].frame_offset + pair[0].frame_len {
            return false;
        }
    }
    true
}

/// True when the index end fits within a stream of `stream_len`
/// bytes. The commit side checks this against the stream length at
/// index-write time; a stream that shrank past its index (a
/// consolidation without its index rewrite) drops the index.
pub fn validate_index(records: &[IndexRecord], stream_len: u64) -> bool {
    match records.last() {
        None => false,
        Some(last) => last.frame_offset + last.frame_len <= stream_len,
    }
}

/// True when every record range starts on the zstd magic. Each
/// record range starts at a frame head, so the magic must sit at
/// `frame_offset` in the stream the record was written against.
fn index_matches_stream(records: &[IndexRecord], stream: &[u8]) -> bool {
    for r in records {
        let at = r.frame_offset as usize;
        if stream.len() < at + 4 {
            return false;
        }
        if stream[at..at + 4] != ZSTD_MAGIC {
            return false;
        }
    }
    true
}

/// One opened session log with its frame index (P8).
///
/// `open_indexed` takes the snapshot: the raw stream bytes and the
/// index records load at open. A live tailer re-opens to observe
/// growth. Reads never take the log-line lock; a torn tail keeps
/// every complete frame (P4).
pub struct LogStream {
    path: PathBuf,
    raw: Vec<u8>,
    is_zstd: bool,
    records: Vec<IndexRecord>,
}

/// Open the log at `path` with its `<log>.idx` sidecar, when the
/// stream is a zstd stream and the index validates against it.
///
/// A session without the sidecar (a pre-P8 log, and every legacy
/// plain log) opens with no records: the reads fall back to the
/// full text. A missing log file is an error, as with
/// `read_log_text`.
pub fn open_indexed(path: &Path) -> io::Result<LogStream> {
    let raw = std::fs::read(path)?;
    let is_zstd = is_zstd_stream(&raw);
    let mut records = Vec::new();
    if is_zstd {
        let idx = read_index(&index_path(path))?;
        if validate_index(&idx, raw.len() as u64) && index_matches_stream(&idx, &raw) {
            records = idx;
        }
    }
    Ok(LogStream {
        path: path.to_path_buf(),
        raw,
        is_zstd,
        records,
    })
}

impl LogStream {
    /// True when the open carries a validated frame index.
    pub fn has_index(&self) -> bool {
        !self.records.is_empty()
    }

    /// The last `n` complete lines of text, oldest first.
    ///
    /// With a frame index the read decodes only the record ranges
    /// that hold the tail: O(frames at the tail) to decode,
    /// O(index) to locate. Without one, or when the stream grew
    /// past its index (a torn commit), the read falls back to the
    /// full text: the un-indexed tail holds the newest lines.
    pub fn tail_lines(&self, n: usize) -> Vec<String> {
        if n == 0 {
            return Vec::new();
        }
        if self.is_zstd && !self.records.is_empty() {
            let last = self.records.last().unwrap();
            let indexed_end = last.frame_offset + last.frame_len;
            if indexed_end >= self.raw.len() as u64 {
                return self.tail_lines_indexed(n);
            }
        }
        // The fallback: the full text, last n complete lines.
        match self.full_text() {
            Ok(t) => last_n_lines(&t, n),
            Err(_) => Vec::new(),
        }
    }

    /// The line at 1-based `seq`, when the log holds it.
    ///
    /// With a frame index the read decodes the one record range
    /// that holds `seq`: O(index) to locate. Without one, or when
    /// `seq` sits past the indexed total, the read falls back to
    /// the full text. `None` when `seq` is zero or past the last
    /// line.
    pub fn line_at(&self, seq: u64) -> Option<String> {
        if seq == 0 {
            return None;
        }
        if self.is_zstd && !self.records.is_empty() {
            let recs = &self.records;
            if let Some(i) = recs.iter().rposition(|r| r.seq_start <= seq) {
                let rec = &recs[i];
                let text = self.decode_range(rec.frame_offset, rec.frame_len);
                let lines = complete_lines(&text);
                if let Some(line) = lines.get((seq - rec.seq_start) as usize) {
                    return Some(line.to_string());
                }
            }
        }
        // The fallback: the full text.
        self.full_text().ok().and_then(|t| {
            let lines = complete_lines(&t);
            lines.get(seq as usize - 1).map(|s| s.to_string())
        })
    }

    /// The lines at 1-based seqs `lo..=hi`, oldest first.
    ///
    /// With a frame index the read decodes only the record ranges
    /// the span crosses. Without one, or when the stream grew
    /// past its index, the read falls back to the full text.
    /// Empty when `lo` is zero, `lo` exceeds `hi`, or the span
    /// sits past the last line.
    pub fn lines_range(&self, lo: u64, hi: u64) -> Vec<String> {
        if lo == 0 || lo > hi {
            return Vec::new();
        }
        if self.is_zstd && !self.records.is_empty() {
            let last = self.records.last().unwrap();
            let indexed_end = last.frame_offset + last.frame_len;
            if indexed_end >= self.raw.len() as u64 {
                if let Some(out) = self.lines_range_indexed(lo, hi) {
                    return out;
                }
            }
        }
        // The fallback: the full text.
        self.full_text()
            .ok()
            .map(|t| {
                let lines = complete_lines(&t);
                lines
                    .get(lo as usize - 1..hi as usize)
                    .map(|v| v.iter().map(|s| s.to_string()).collect())
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    }

    /// The indexed tail read: one contiguous decode from the
    /// record that holds the first wanted line to the stream end.
    fn tail_lines_indexed(&self, n: usize) -> Vec<String> {
        let recs = &self.records;
        let last = recs.last().unwrap();
        // The last record's range decodes once: its line count
        // sets the indexed total.
        let last_text = self.decode_range(last.frame_offset, last.frame_len);
        let n_last = complete_lines(&last_text).len();
        let total = last.seq_start + n_last as u64 - 1;
        if n as u64 >= total {
            // The tail holds every indexed line: one decode of the
            // whole indexed range.
            let all = self.decode_range(0, self.raw.len() as u64);
            return complete_lines(&all).into_iter().map(String::from).collect();
        }
        let target_first = total - n as u64 + 1; // 1-based
        let i = recs.iter().rposition(|r| r.seq_start <= target_first).unwrap();
        let from = &recs[i];
        let dec = self.decode_range(
            from.frame_offset,
            self.raw.len() as u64 - from.frame_offset,
        );
        let lines = complete_lines(&dec);
        let skip = (target_first - from.seq_start) as usize;
        lines.into_iter().skip(skip).map(String::from).collect()
    }

    /// The indexed seq read: the one record range that holds `seq`.
    fn lines_range_indexed(&self, lo: u64, hi: u64) -> Option<Vec<String>> {
        let recs = &self.records;
        let i0 = recs.iter().rposition(|r| r.seq_start <= lo)?;
        let i1 = recs.iter().rposition(|r| r.seq_start <= hi)?;
        let mut out = Vec::new();
        for rec in recs.iter().skip(i0).take(i1 - i0 + 1) {
            let text = self.decode_range(rec.frame_offset, rec.frame_len);
            let lines = complete_lines(&text);
            if lines.is_empty() {
                continue;
            }
            let rec_lo = lo.max(rec.seq_start) - rec.seq_start;
            let rec_hi = hi.min(rec.seq_start + lines.len() as u64 - 1) - rec.seq_start;
            if rec_lo > rec_hi {
                continue;
            }
            let slice = lines.get(rec_lo as usize..(rec_hi + 1) as usize)?;
            out.extend(slice.iter().map(|s| s.to_string()));
        }
        Some(out)
    }

    /// The full decoded text: the P3 read, plain or zstd. A torn
    /// tail keeps the complete frames (P4).
    fn full_text(&self) -> io::Result<String> {
        if !self.is_zstd {
            return String::from_utf8(self.raw.clone()).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "session log is not valid UTF-8",
                )
            });
        }
        decompress_stream(&self.path, &self.raw)
    }

    /// Decode one byte range of the stream: concatenated zstd
    /// frames. A torn range tail keeps the complete frames (P4).
    /// Plain-text ranges copy the bytes.
    fn decode_range(&self, off: u64, len: u64) -> String {
        if len == 0 {
            return String::new();
        }
        let slice = &self.raw[off as usize..(off + len) as usize];
        if !self.is_zstd {
            return String::from_utf8_lossy(slice).into_owned();
        }
        String::from_utf8_lossy(&decode_stream_bytes(slice)).into_owned()
    }
}

/// Decode concatenated zstd frames to bytes. A torn tail keeps
/// every complete frame and drops the partial one (P4). A
/// non-zstd input decodes to empty.
pub fn decode_stream_bytes(buf: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut dec = match zstd::stream::read::Decoder::new(buf) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::with_capacity(buf.len().max(16) * 4);
    let _ = dec.read_to_end(&mut out);
    out
}

/// The complete lines of a decoded log text: the lines that carry
/// their trailing newline. A partial final line (a torn tail)
/// drops at parse time, the P4 rule.
fn complete_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.lines().collect();
    if !text.is_empty() && !text.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// The last `n` complete lines of text, oldest first.
fn last_n_lines(text: &str, n: usize) -> Vec<String> {
    let lines = complete_lines(text);
    let start = lines.len().saturating_sub(n);
    lines[start..].iter().map(|s| (*s).to_string()).collect()
}

/// Read the tail of a session log within a byte budget, without
/// the frame index (the no-index fallback, P8).
///
/// Reads the last `byte_budget` stream bytes, scans the window
/// for the zstd magic, and decodes from the first frame start
/// inside the window (the tv-rushi decision-84 algorithm, moved
/// into the shared module). A window that holds no frame start
/// falls back to the full read. Plain-text logs window the raw
/// bytes.
///
/// The `bool` says whether the window covered the budget: true
/// when the read came from the window (a frame start inside it,
/// or the whole file inside it), false when the full read ran.
pub fn read_log_tail(path: &Path, byte_budget: u64) -> io::Result<(String, bool)> {
    let raw = std::fs::read(path)?;
    let len = raw.len();
    if !is_zstd_stream(&raw) {
        // Plain text: the window is the last budget bytes. The
        // window covers the budget only when the file fits it.
        let start = len.saturating_sub(byte_budget as usize);
        let text = String::from_utf8(raw[start..].to_vec()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "session log is not valid UTF-8",
            )
        })?;
        return Ok((text, len <= byte_budget as usize));
    }
    if len as u64 <= byte_budget {
        // The whole stream fits the budget: the full decode.
        return Ok((decompress_stream(path, &raw)?, true));
    }
    let start = len - byte_budget as usize;
    let window = &raw[start..];
    if let Some(off) = window.windows(4).position(|w| w == &ZSTD_MAGIC[..]) {
        let out = decode_stream_bytes(&window[off..]);
        if !out.is_empty() {
            // The window decodes from its first frame start. An
            // empty decode here means a false magic hit inside
            // compressed data: fall through to the full read.
            let text = String::from_utf8(out).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "zstd log stream is not valid UTF-8",
                )
            })?;
            return Ok((text, true));
        }
    }
    // No frame start in the window: the full read.
    Ok((decompress_stream(path, &raw)?, false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logline::LogLine;
    use std::io::Write;

    /// Commit one JSONL line per event: one frame and one index
    /// record each (P1, P8).
    fn commit_lines(dir: &Path, name: &str, contents: &[&str]) -> PathBuf {
        let p = dir.join(name);
        for c in contents {
            let json = format!(
                r#"{{"v":1,"type":"user_message","ts":"t","content":"{c}"}}"#
            );
            LogLine::from_json(&json).commit(&p).unwrap();
        }
        p
    }

    /// The expected full text of committed lines: each JSON line
    /// plus its trailing newline.
    fn text_of(contents: &[&str]) -> String {
        contents
            .iter()
            .map(|c| {
                format!(
                    "{{\"v\":1,\"type\":\"user_message\",\"ts\":\"t\",\"content\":\"{c}\"}}\n",
                )
            })
            .collect()
    }

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

    // ------------------------------------------------------------------
    // P8: the frame index sidecar
    // ------------------------------------------------------------------

    #[test]
    fn index_records_are_contiguous_and_cover_the_stream() {
        let dir = tempfile::tempdir().unwrap();
        let p = commit_lines(dir.path(), "events.jsonl", &["one", "two", "three"]);
        let raw = std::fs::read(&p).unwrap();
        let records = read_index(&index_path(&p)).unwrap();
        assert_eq!(records.len(), 3, "one record per commit");
        for (i, r) in records.iter().enumerate() {
            assert_eq!(r.seq_start, (i + 1) as u64, "seq is the 1-based line number");
        }
        for pair in records.windows(2) {
            assert_eq!(
                pair[1].frame_offset,
                pair[0].frame_offset + pair[0].frame_len,
                "frames are contiguous in the stream"
            );
        }
        let last = records.last().unwrap();
        assert_eq!(
            last.frame_offset + last.frame_len,
            raw.len() as u64,
            "the index covers the stream"
        );
    }

    #[test]
    fn consolidate_rewrites_the_pair_with_one_record() {
        // P5 plus P8: one level-19 frame, one index record.
        let dir = tempfile::tempdir().unwrap();
        let p = commit_lines(dir.path(), "events.jsonl", &["one", "two", "three"]);
        let expected = text_of(&["one", "two", "three"]);
        consolidate(&p).unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert_eq!(read_log_text(&p).unwrap(), expected);
        let records = read_index(&index_path(&p)).unwrap();
        assert_eq!(records.len(), 1, "the consolidated frame carries one record");
        let r = &records[0];
        assert_eq!(r.seq_start, 1);
        assert_eq!(r.frame_offset, 0);
        assert_eq!(r.frame_len, raw.len() as u64);
        assert_eq!(r.text_len, expected.len() as u64);
        // Resume: the next commit extends the index, seq continues.
        commit_lines(dir.path(), "events.jsonl", &["four"]);
        let records = read_index(&index_path(&p)).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].seq_start, 4, "the resume line is seq 4");
        let stream = open_indexed(&p).unwrap();
        assert_eq!(stream.line_at(4), Some(text_of(&["four"]).trim_end().to_string()));
    }

    #[test]
    fn tail_lines_reads_the_last_n_lines_of_per_commit_frames() {
        let dir = tempfile::tempdir().unwrap();
        let p = commit_lines(
            dir.path(),
            "events.jsonl",
            &["one", "two", "three", "four", "five"],
        );
        let stream = open_indexed(&p).unwrap();
        assert!(stream.has_index());
        let expected: Vec<&str> = vec![
            r#"{"v":1,"type":"user_message","ts":"t","content":"three"}"#,
            r#"{"v":1,"type":"user_message","ts":"t","content":"four"}"#,
            r#"{"v":1,"type":"user_message","ts":"t","content":"five"}"#,
        ];
        assert_eq!(stream.tail_lines(3), expected.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        // n past the total returns every line.
        assert_eq!(stream.tail_lines(99).len(), 5);
        assert!(stream.tail_lines(0).is_empty());
        // Cross-check against the full read.
        let text = read_log_text(&p).unwrap();
        assert_eq!(stream.tail_lines(5), last_n_lines(&text, 5));
    }

    #[test]
    fn line_at_matches_the_decoded_line_at_the_seq() {
        let dir = tempfile::tempdir().unwrap();
        let p = commit_lines(dir.path(), "events.jsonl", &["one", "two", "three"]);
        let stream = open_indexed(&p).unwrap();
        for (i, c) in ["one", "two", "three"].iter().enumerate() {
            let want = format!(
                r#"{{"v":1,"type":"user_message","ts":"t","content":"{c}"}}"#
            );
            assert_eq!(stream.line_at((i + 1) as u64), Some(want));
        }
        assert_eq!(stream.line_at(0), None, "seq is 1-based");
        assert_eq!(stream.line_at(4), None, "past the last line");
    }

    #[test]
    fn lines_range_spans_the_consolidated_boundary() {
        // P5 plus P6 plus P8: consolidated frame, then resumed
        // per-commit frames. The range crosses the boundary.
        let dir = tempfile::tempdir().unwrap();
        let p = commit_lines(dir.path(), "events.jsonl", &["one", "two", "three"]);
        consolidate(&p).unwrap();
        commit_lines(dir.path(), "events.jsonl", &["four", "five"]);
        let stream = open_indexed(&p).unwrap();
        let lines: Vec<String> = (1..=5)
            .map(|i| {
                format!(
                    r#"{{"v":1,"type":"user_message","ts":"t","content":"{line}"}}"#,
                    line = ["one", "two", "three", "four", "five"][i - 1]
                )
            })
            .collect();
        assert_eq!(stream.lines_range(2, 5), &lines[1..5]);
        assert_eq!(stream.lines_range(4, 4), &lines[3..4]);
        assert_eq!(stream.lines_range(5, 2), Vec::<String>::new(), "lo past hi is empty");
        assert_eq!(stream.lines_range(0, 2), Vec::<String>::new(), "seq is 1-based");
        assert_eq!(stream.lines_range(6, 9), Vec::<String>::new(), "past the end is empty");
    }

    #[test]
    fn tail_lines_on_a_consolidated_frame_decodes_the_one_record() {
        let dir = tempfile::tempdir().unwrap();
        let p = commit_lines(
            dir.path(),
            "events.jsonl",
            &["one", "two", "three", "four", "five"],
        );
        consolidate(&p).unwrap();
        let stream = open_indexed(&p).unwrap();
        assert_eq!(stream.tail_lines(2).len(), 2);
        assert_eq!(stream.line_at(1), Some(stream.tail_lines(5)[0].clone()));
        assert_eq!(stream.tail_lines(5), last_n_lines(&read_log_text(&p).unwrap(), 5));
    }

    #[test]
    fn tail_lines_on_a_torn_tail_drops_the_torn_line() {
        // P4 plus P8: the stream is cut inside its last frame. The
        // index then points past the stream end and drops; the read
        // falls back to the full text and keeps the complete lines.
        let dir = tempfile::tempdir().unwrap();
        let p = commit_lines(dir.path(), "events.jsonl", &["one", "two", "three"]);
        let raw = std::fs::read(&p).unwrap();
        // Tear the last frame: keep all but its final 10 bytes.
        std::fs::write(&p, &raw[..raw.len() - 10]).unwrap();
        let stream = open_indexed(&p).unwrap();
        assert!(!stream.has_index(), "the index points past the stream end");
        let tail = stream.tail_lines(10);
        assert_eq!(tail.len(), 2, "the torn line drops");
        assert!(tail[0].contains("\"one\""));
        assert!(tail[1].contains("\"two\""));
    }

    #[test]
    fn torn_index_tail_drops_the_last_record() {
        // P4 rule for the index: a partial final record drops; the
        // whole records keep.
        let dir = tempfile::tempdir().unwrap();
        let p = commit_lines(dir.path(), "events.jsonl", &["one", "two", "three"]);
        let idx = index_path(&p);
        let bytes = std::fs::read(&idx).unwrap();
        assert_eq!(bytes.len(), 3 * INDEX_RECORD_LEN);
        std::fs::write(&idx, &bytes[..2 * INDEX_RECORD_LEN + 17]).unwrap();
        let records = read_index(&idx).unwrap();
        assert_eq!(records.len(), 2, "the torn record drops, the rest keep");
        // The stream outgrew the index by one frame: the tail read
        // falls back to the full text and still finds every line.
        let stream = open_indexed(&p).unwrap();
        assert!(stream.has_index());
        assert_eq!(stream.tail_lines(10).len(), 3);
        assert_eq!(stream.line_at(3), Some(stream.tail_lines(10)[2].clone()));
    }

    #[test]
    fn a_broken_index_drops_and_falls_back_to_the_full_read() {
        // The log stays the authority: a structurally broken index
        // never fails a read.
        let dir = tempfile::tempdir().unwrap();
        let p = commit_lines(dir.path(), "events.jsonl", &["one", "two"]);
        let idx = index_path(&p);
        let bytes = std::fs::read(&idx).unwrap();
        // Break the first record's seq start: 1 becomes 7.
        let mut broken = bytes.clone();
        broken[0..8].copy_from_slice(&7u64.to_le_bytes());
        std::fs::write(&idx, &broken).unwrap();
        let stream = open_indexed(&p).unwrap();
        assert!(!stream.has_index(), "the broken index drops");
        assert_eq!(stream.tail_lines(2).len(), 2);
        assert_eq!(stream.line_at(1), Some(stream.tail_lines(2)[0].clone()));
    }

    #[test]
    fn a_stream_grown_past_its_index_falls_back_to_the_full_read() {
        // A torn commit: the frame landed, the record did not. The
        // un-indexed tail holds the newest lines, so the indexed
        // reads fall back to the full text.
        let dir = tempfile::tempdir().unwrap();
        let p = commit_lines(dir.path(), "events.jsonl", &["one", "two"]);
        let extra = compress_frame(b"three\n").unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(&extra)
            .unwrap();
        let stream = open_indexed(&p).unwrap();
        assert!(stream.has_index(), "the valid prefix records keep");
        let tail = stream.tail_lines(3);
        assert_eq!(tail.len(), 3, "the un-indexed tail is in the read");
        assert_eq!(tail[2], "three");
        assert_eq!(stream.line_at(3), Some(tail[2].clone()));
        assert_eq!(stream.lines_range(2, 3), &tail[1..3]);
    }

    #[test]
    fn pre_p8_stream_reads_through_the_fallback() {
        // Replay: a pre-P8 session holds frames with no sidecar.
        // The rows read unchanged through the full-text fallback.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("events.jsonl");
        let a = compress_frame(b"old-a\n").unwrap();
        let b = compress_frame(b"old-b\n").unwrap();
        let mut buf = a;
        buf.extend_from_slice(&b);
        std::fs::write(&p, &buf).unwrap();
        let stream = open_indexed(&p).unwrap();
        assert!(!stream.has_index(), "no sidecar, no records");
        assert_eq!(stream.tail_lines(1), vec!["old-b".to_string()]);
        assert_eq!(stream.line_at(1), Some("old-a".to_string()));
        assert_eq!(stream.lines_range(1, 2), vec!["old-a".to_string(), "old-b".to_string()]);
    }

    #[test]
    fn a_plain_log_opens_and_reads_without_an_index() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("events.jsonl");
        std::fs::write(&p, "a\nb\nc\n").unwrap();
        let stream = open_indexed(&p).unwrap();
        assert!(!stream.has_index());
        assert_eq!(stream.tail_lines(2), vec!["b".to_string(), "c".to_string()]);
        assert_eq!(stream.line_at(2), Some("b".to_string()));
        assert_eq!(stream.lines_range(1, 3), vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    }

    // ------------------------------------------------------------------
    // P8: the no-index window scan
    // ------------------------------------------------------------------

    #[test]
    fn read_log_tail_scans_the_window_for_the_first_frame_start() {
        let dir = tempfile::tempdir().unwrap();
        let p = commit_lines(
            dir.path(),
            "events.jsonl",
            &["one", "two", "three", "four", "five"],
        );
        let raw = std::fs::read(&p).unwrap();
        // A window that clips the first frame but holds the rest.
        let (text, covered) = read_log_tail(&p, raw.len() as u64 - 1).unwrap();
        assert!(covered, "the window covered the budget");
        // The read starts at the second frame head: line one drops.
        let want = &read_log_text(&p).unwrap()[text_of(&["one"]).len()..];
        assert_eq!(&text, want);
    }

    #[test]
    fn read_log_tail_falls_back_when_the_window_holds_no_frame_start() {
        // One consolidated frame: a small window sits inside the
        // frame body, holds no frame start, and takes the full
        // read with the coverage flag down.
        let dir = tempfile::tempdir().unwrap();
        let p = commit_lines(
            dir.path(),
            "events.jsonl",
            &["one", "two", "three", "four", "five"],
        );
        consolidate(&p).unwrap();
        let full = read_log_text(&p).unwrap();
        let (text, covered) = read_log_tail(&p, 64).unwrap();
        assert!(!covered, "the window held no frame start");
        assert_eq!(text, full, "the full read kept every line");
    }

    #[test]
    fn read_log_tail_windows_a_plain_text_log() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("events.jsonl");
        std::fs::write(&p, "a\nb\nc\n").unwrap();
        let (text, covered) = read_log_tail(&p, 1024).unwrap();
        assert!(covered, "the file fits the budget");
        assert_eq!(text, "a\nb\nc\n");
        let (text, covered) = read_log_tail(&p, 3).unwrap();
        assert!(!covered, "the window clipped the file");
        assert_eq!(text, "\nc\n", "the last three bytes, mid-line and all");
    }
}
