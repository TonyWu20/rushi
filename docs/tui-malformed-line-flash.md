# TUI malformed log line flash

Status: shipped (2026-08-29 request, commit `90e3bb9`). The
request lives in `docs/tui_feature_requests_from_human.md`.

## 1. Request

Fix the transient `[malformed log line]` flash on live loops.

## 2. Root cause

`read_events` (`bin/tui/src/port_file.rs`) reads the whole file
and splits on newline. A read that races an in-flight append
sees the last line without its trailing newline. That partial
segment fails `Event::parse_line` and renders as malformed. The
persisted line is clean. All lines validate.

## 3. Shipped fix

In `read_events`, when the read data does not end in a newline,
drop the final segment as in progress. Show it on the next read
once the append lands. The live tailer holds a partial line
until its newline lands, so the flash is gone on both paths.

Regression tests: `read_events_drops_in_progress_tail_without_newline`
and `watch_holds_partial_line_until_newline`
(`bin/tui/src/port_file.rs`).
