//! `tui` — the first stateful Rust binary of the harness.
//!
//! A window onto the session log and a supervisor for the opaque loop
//! (docs/tui.md). Everything session-shaped goes through
//! [`crate::port::SessionPort`]; this file is the composition root:
//! terminal, runtime, port, and the draw/input loop.

#![deny(clippy::todo, clippy::unimplemented, clippy::unreachable)]

mod app;
mod browse;
mod color;
mod config;
mod editor;
mod event;
mod ext;
mod float;
mod highlight;
mod palette;
mod picker;
mod port;
mod port_file;
mod render;
mod tool_display;
mod vim_editor;

use std::io::Write;
use std::time::Duration;
use std::time::Instant;

use clap::Parser;
use crossterm::event as cevent;
use crossterm::terminal;
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::{Action, App, Decision, Key};
use config::TuiConfig;
use event::EventKind;
use port::{SessionId, SessionPort, TailCursor, WatchItem};
use port_file::FileSessionPort;

/// Interactive window onto the session log. Renders events, appends
/// user events, and supervises the opaque loop command from config.
#[derive(Parser, Debug)]
#[command(name = "tui", about = "Terminal UI for the harness session log")]
struct Args {
    /// The session to open. If omitted, the TUI asks for a new session
    /// name: the session is created on its first appended event.
    session: Option<String>,

    /// Path to the harness config file. Relative paths resolve against
    /// the current directory.
    #[arg(long, default_value = "config.toml")]
    config: String,
}

/// Terminal state restoration that must survive a panic: raw mode,
/// mouse capture, and the alternate screen all come back.
struct TermGuard;

impl TermGuard {
    fn init() -> std::io::Result<TermGuard> {
        let mut out = std::io::stdout();
        out.execute(terminal::EnterAlternateScreen)?;
        terminal::enable_raw_mode()?;
        out.execute(cevent::EnableMouseCapture)?;
        Ok(TermGuard)
    }
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let mut out = std::io::stdout();
        let _ = out.execute(cevent::DisableMouseCapture);
        crossterm::execute!(out, terminal::LeaveAlternateScreen, crossterm::cursor::Show).ok();
        let _ = out.flush();
    }
}

/// Map a crossterm key event to the app-level [`Key`].
fn key_input(k: &cevent::KeyEvent) -> Option<Key> {
    if k.modifiers.contains(cevent::KeyModifiers::CONTROL) {
        return match k.code {
            cevent::KeyCode::Char('c') => Some(Key::CtrlC),
            cevent::KeyCode::Char('e') => Some(Key::CtrlE),
            cevent::KeyCode::Char('j') => Some(Key::CtrlJ),
            cevent::KeyCode::Char('k') => Some(Key::CtrlK),
            cevent::KeyCode::Char('o') => Some(Key::CtrlO),
            cevent::KeyCode::Char('t') => Some(Key::CtrlT),
            cevent::KeyCode::Char('x') => Some(Key::CtrlX),
            cevent::KeyCode::Char('f') => Some(Key::CtrlF),
            cevent::KeyCode::Char('l') => Some(Key::CtrlL),
            cevent::KeyCode::Char('p') => Some(Key::CtrlP),
            cevent::KeyCode::Char('i') => Some(Key::CtrlI),
            cevent::KeyCode::Char('q') => Some(Key::Quit),
            cevent::KeyCode::Char('r') => Some(Key::CtrlR),
            cevent::KeyCode::Char('u') => Some(Key::CtrlU),
            cevent::KeyCode::Char('d') => Some(Key::CtrlD),
            _ => None,
        };
    }
    if k.modifiers.contains(cevent::KeyModifiers::ALT)
        && k.code == cevent::KeyCode::Up
    {
        return Some(Key::AltUp);
    }
    match k.code {
        cevent::KeyCode::Char('q') => Some(Key::Quit),
        cevent::KeyCode::Char(c) => Some(Key::Char(c)),
        cevent::KeyCode::Enter => Some(Key::Enter),
        cevent::KeyCode::Backspace => Some(Key::Backspace),
        cevent::KeyCode::Tab => Some(Key::Tab),
        cevent::KeyCode::BackTab => Some(Key::BackTab),
        cevent::KeyCode::PageUp => Some(Key::PgUp),
        cevent::KeyCode::PageDown => Some(Key::PgDn),
        cevent::KeyCode::Esc => Some(Key::Esc),
        cevent::KeyCode::Left => Some(Key::Left),
        cevent::KeyCode::Right => Some(Key::Right),
        cevent::KeyCode::Up => Some(Key::Up),
        cevent::KeyCode::Down => Some(Key::Down),
        cevent::KeyCode::Home => Some(Key::Home),
        cevent::KeyCode::End => Some(Key::End),
        cevent::KeyCode::Delete => Some(Key::Delete),
        _ => None,
    }
}

/// The thinking-level values the thinking-level key cycles through
/// (docs/tui-thinking-block.md section 4, the thinking-level control).
/// `none` turns thinking off. `minimal`/`low` share the low level
/// bucket, and `max`/`xhigh` share the highest (docs/tui.md 7.2).
const THINKING_LEVEL_ORDER: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// TUI process diagnostic log. One `pid ms msg` line appended to a
/// log file, never to stderr: after `TermGuard::init` the TUI owns
/// the terminal (alt-screen), and a raw stderr write bypasses the
/// renderer and corrupts the frame (FT-016). The path is `TUI_LOG`
/// when set, otherwise the XDG cache default `<cache>/tui/tui.log`
/// under `$XDG_CACHE_HOME` or `$HOME/.cache`. Write failures are
/// dropped (the log must not take the UI down).
fn tui_log(msg: &str) {
    let Some(path) = tui_log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let line = format!("{} {} {msg}\n", std::process::id(), ms);
        let _ = f.write_all(line.as_bytes());
    }
}

/// The TUI diagnostic log file. `TUI_LOG` when set, otherwise the
/// XDG cache default `<cache>/tui/tui.log`.
fn tui_log_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("TUI_LOG") {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|d| d.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")));
    cache.map(|d| d.join("tui").join("tui.log"))
}

/// The 0-4 level of one effort value, mirroring the bin/model
/// mapping (docs/tui.md 7.2). The flash states the level so the
/// input-area border color matches what the next step publishes.
fn effort_level(effort: &str) -> u32 {
    match effort.to_ascii_lowercase().as_str() {
        "none" => 0,
        "minimal" | "low" => 1,
        "medium" => 2,
        "high" => 3,
        "xhigh" | "max" => 4,
        _ => 0,
    }
}

/// The active model name for the effort write-back, matching the
/// bin/model resolution: the `MODEL` env var wins, then the config's
/// `[active] model`, then the default.
fn resolve_active_model_name(cfg: &TuiConfig) -> String {
    std::env::var("MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| cfg.active_model.clone())
        .unwrap_or_else(|| "deepseek".to_string())
}

/// The resolved reasoning effort of one model name, matching the
/// bin/model precedence: the per-model table wins over the global
/// `[model]` table; a missing key defaults to `medium`; `off`
/// normalizes to `none`.
fn resolve_reasoning_effort(text: &str, active: &str) -> String {
    let v: toml::Value = toml::from_str(text)
        .unwrap_or_else(|_| toml::Value::Table(toml::map::Map::new()));
    let model_root = v
        .get("model")
        .cloned()
        .unwrap_or_else(|| toml::Value::Table(toml::map::Map::new()));
    let mdl = model_root
        .get(active)
        .cloned()
        .unwrap_or_else(|| toml::Value::Table(toml::map::Map::new()));
    let s = |t: &toml::Value, k: &str| -> Option<String> {
        t.get(k).and_then(|x| x.as_str()).map(str::to_string)
    };
    let effort = s(&mdl, "reasoning_effort")
        .or_else(|| s(&model_root, "reasoning_effort"))
        .unwrap_or_else(|| "medium".to_string());
    if effort.eq_ignore_ascii_case("off") {
        "none".to_string()
    } else {
        effort
    }
}

/// Write a specific reasoning effort value into the active model's
/// config entry (docs/tui-command-palette.md section 8). The write-
/// back is a targeted, comment-preserving text edit of the
/// `[model.<active>]` table. Returns the value written.
fn set_effort(config_path: &std::path::Path, active: &str, value: &str) -> Result<String, String> {
    let text = std::fs::read_to_string(config_path)
        .map_err(|e| format!("cannot read config {}: {e}", config_path.display()))?;
    let headers = [format!("[model.{active}]"), format!("[model.\"{active}\"]")];
    let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();

    let is_header = |l: &str| headers.iter().any(|h| l.trim() == h);
    let mut section_open: Option<usize> = None;
    let mut key_at: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if is_header(t) {
            section_open = Some(i);
            key_at = None;
            continue;
        }
        if section_open.is_some() && t.starts_with('[') {
            section_open = None;
            continue;
        }
        if section_open.is_some() && t.starts_with("reasoning_effort") {
            key_at = Some(i);
        }
    }

    let mut out: Vec<String> = lines.clone();
    match (section_open, key_at) {
        (_, Some(k)) => {
            out[k] = format!("reasoning_effort = \"{value}\"");
        }
        (Some(s), None) => {
            out.insert(s + 1, format!("reasoning_effort = \"{value}\""));
        }
        (None, _) => {
            if !out.is_empty() && !out.last().unwrap().trim().is_empty() {
                out.push(String::new());
            }
            out.push(format!("[model.{active}]"));
            out.push(format!("reasoning_effort = \"{value}\""));
        }
    }
    let mut new_text = out.join("\n");
    if !new_text.ends_with('\n') {
        new_text.push('\n');
    }
    std::fs::write(config_path, new_text)
        .map_err(|e| format!("cannot write config {}: {e}", config_path.display()))?;
    Ok(value.to_string())
}

/// Cycle the active model's reasoning effort to the next value and
/// write it back to the config (docs/tui-thinking-block.md section
/// 4, the effort control). Returns the new effort value.
fn cycle_reasoning_effort(config_path: &std::path::Path, active: &str) -> Result<String, String> {
    let text = std::fs::read_to_string(config_path)
        .map_err(|e| format!("cannot read config {}: {e}", config_path.display()))?;
    let current = resolve_reasoning_effort(&text, active);
    let pos = THINKING_LEVEL_ORDER
        .iter()
        .position(|e| e.eq_ignore_ascii_case(&current))
        .unwrap_or(3); // the default `medium` index
    let next = THINKING_LEVEL_ORDER[(pos + 1) % THINKING_LEVEL_ORDER.len()];
    set_effort(config_path, active, next)
}

fn main() {
    let args = Args::parse();
    let cfg = match TuiConfig::load(&args.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("tui: {e}");
            std::process::exit(1);
        }
    };
    // Extension discovery fails loud before any terminal effect:
    // a bad manifest or a broken command refuses the start and names
    // the file (docs/ui-extension-plan.md stage 1).
    let disc = match ext::discover(&cfg) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("tui: {e}");
            std::process::exit(1);
        }
    };
    let port = FileSessionPort::new(&cfg);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("cannot build the async runtime");

    let _guard = TermGuard::init().expect("cannot initialize the terminal (is this a tty?)");

    let active = args.session.clone().map(SessionId::new);

    let backend = CrosstermBackend::new(std::io::stdout());
    let mut term = Terminal::new(backend).expect("cannot create the terminal");
    let mut app = App::new();
    // The [tui] color override forces the capability level; absent,
    // the environment detection stands (color.rs module docs). The
    // [tui] color scheme (docs/tui-color-scheme.md section 3) maps
    // every color role to a hex value; the values lower to the level.
    let level = cfg.color.unwrap_or_else(crate::color::Level::detect);
    let palette = match crate::color::palette_from_config(
        level,
        cfg.color_scheme.as_deref(),
        &cfg.custom_schemes,
    ) {
        Ok(p) => p,
        Err(e) => {
            tui_log(&format!("palette config failed: {e}"));
            std::process::exit(1);
        }
    };
    app.set_palette(palette);
    // The tool-result display config (docs/tui-tool-display-port.md
    // section 2, the config part): the `[tui] tool_display` table.
    app.set_tool_display(cfg.tool_display);
    let host = ext::ExtHost::new(&disc, &cfg);

    for item in host.start() {
        match item {
            ext::ExtItem::Skipped { ext: name, reason } => {
                app.flash(format!("ext {name} skipped: {reason}"));
            }
            ext::ExtItem::Dead { ext: name } => {
                app.flash(format!("ext {name} dead at start"));
            }
            _ => {}
        }
    }

    if let Some(id) = active {
        let events = match rt.block_on(port.read_events(&id)) {
            Ok(evs) => evs,
            Err(e) => {
                trace(
                    &rt,
                    &port,
                    Some(&id),
                    "port",
                    &format!("event read failed: {e}"),
                );
                Vec::new()
            }
        };
        app.set_active(id.clone(), events.clone());
        app.set_watch_rx(port.watch(&id, TailCursor::end()));
        app.set_sessions(rt.block_on(port.list_sessions()).unwrap_or_default());
        // Reattach a live loop from an earlier TUI (FT-003): the
        // persistent probe marks the session running, so the status
        // bit shows the real state, not this process's memory.
        resync_external_loop(&rt, &port, &mut app, &id);
        host.send_history(&events);
    } else {
        // No session argument: ask for a new session name instead of
        // resuming the most recent session.
        app.set_sessions(rt.block_on(port.list_sessions()).unwrap_or_default());
        app.start_naming();
    }

    // Restore the persisted thinking level for the active model from
    // the config file. This must happen after set_active because
    // set_active rebuilds ext_status_values from the session log,
    // which would overwrite the config value with a stale value from
    // the previous loop run.
    if let Ok(cfg_text) = std::fs::read_to_string(&cfg.config_path) {
        let active_model = resolve_active_model_name(&cfg);
        let effort = resolve_reasoning_effort(&cfg_text, &active_model);
        tui_log(&format!(
            "startup model={active_model} effort={effort} level={}",
            effort_level(&effort)
        ));
        app.set_effort_current(effort.clone());
        app.set_thinking_level(effort_level(&effort));
    } else {
        tui_log(&format!(
            "startup config read failed ({}); using defaults",
            cfg.config_path.display()
        ));
    }

    let mut last_width: usize = 0;
    let mut last_loop_probe = Instant::now();
    'ui: loop {
        // 1. New log events for the active session.
        while let Some(item) = app.drain_watch() {
            let is_event = matches!(item, WatchItem::Event { .. });
            app.on_watch_item(item);
            if is_event {
                // Forward the new event to the extensions whose kinds
                // match; the event id is its index in the log
                // (docs/ui-extension.md section 4 history rule).
                let evs = app.events();
                let ev = evs.last().cloned().expect("a watch event was just pushed");
                // A malformed line the transcript now shows is a
                // TUI-side finding: leave it in the trace log so it
                // is readable without a human report (FT-001).
                if ev.kind() == EventKind::BadLine {
                    let raw = ev
                        .raw_line()
                        .unwrap_or("")
                        .chars()
                        .take(80)
                        .collect::<String>();
                    trace(&rt, &port, app.active(), "malformed_line", &raw);
                }
                host.forward_event((evs.len() - 1) as u64, &ev);
            }
        }
        // 1.5 Extension items: log appends, terminal notify ops, and
        // the dead/skipped flashes. The reply items already folded
        // into the transcript cache key via the reply version.
        while let Some(item) = host.drain() {
            match item {
                ext::ExtItem::LinesCached { .. }
                | ext::ExtItem::StatusUpdated { .. }
                | ext::ExtItem::FrameUpdated { .. }
                | ext::ExtItem::RowUpdated { .. }
                | ext::ExtItem::TransformedCached { .. } => {}
                ext::ExtItem::AppendReq { ext: name, event } => {
                    let Some(sid) = app.active().cloned() else {
                        app.flash(format!("ext {name} append failed: no active session"));
                        trace(
                            &rt,
                            &port,
                            None,
                            "port",
                            &format!("ext {name} append skipped: no active session"),
                        );
                        continue;
                    };
                    let type_name = event.get("type").and_then(|t| t.as_str()).unwrap_or("?");
                    let ev = event::Event::Json { obj: event.clone() };
                    match rt.block_on(port.append_event(&sid, &ev)) {
                        Ok(()) => app.flash(format!("ext {name} appended {type_name}")),
                        Err(e) => {
                            trace(
                                &rt,
                                &port,
                                Some(&sid),
                                "port",
                                &format!("ext {name} append failed: {e}"),
                            );
                            app.flash(format!("ext {name} append failed: {e}"));
                        }
                    }
                }
                ext::ExtItem::AppendRejected { ext: name, reason } => {
                    app.flash(format!("ext {name}: append rejected: {reason}"));
                }
                ext::ExtItem::NotifyBell { .. } => {
                    let mut out = std::io::stdout();
                    let _ = out.write_all(b"\x07");
                    let _ = out.flush();
                }
                ext::ExtItem::NotifyOsc { code, args, .. } => {
                    let mut out = std::io::stdout();
                    let _ = out.write_all(format!("\x1b]{};{}\x07", code, args).as_bytes());
                    let _ = out.flush();
                }
                ext::ExtItem::Dead { ext: name } => {
                    app.flash(format!("ext {name} is dead (restart budget exhausted)"));
                }
                ext::ExtItem::Skipped { ext: name, reason } => {
                    app.flash(format!("ext {name} skipped: {reason}"));
                }
                ext::ExtItem::CommandsListUpdated { .. } => {
                    let items = host.command_items();
                    app.set_ext_commands(items);
                }
                ext::ExtItem::InvokeReply { ext: name, req, ok, message } => {
                    if ok {
                        app.flash(format!("ext {name} (req {req}): {message}"));
                    } else {
                        app.flash(format!("ext {name} (req {req}) error: {message}"));
                    }
                }
                ext::ExtItem::InvokeTimeout { ext: name } => {
                    app.flash(format!("ext {name} invoke timed out — commands dropped"));
                    app.drop_ext_commands(&name);
                }
            }
        }
        // 2. Output of every running loop (all sessions).
        for _ in app.drain_loop_lines() {}

        // 3. One input batch, or a short wait. A wheel notch queues
        // many scroll events at once; drain the whole pending queue,
        // then draw once. One event per frame (with a full redraw
        // between) starved key input behind a mouse burst: the keys
        // sat in the queue and the UI looked hung. Draining keeps
        // keys responsive (docs/tui_feature_requests_from_human.md).
        let mut evs: Vec<cevent::Event> = Vec::new();
        // While a response is streaming, tick at 16 ms (about 60 FPS)
        // so the paced stream release renders at a smooth frame rate
        // (docs/tui-streaming-response.md section 6.5); the idle 100 ms
        // wait is enough otherwise.
        let poll_to = if app.stream_live() {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(100)
        };
        match cevent::poll(poll_to) {
            Ok(true) => {
                // Read pending events until the queue is empty.
                // Use a 1 ms poll, not 0: with crossterm's
                // `use-dev-tty` input source a zero poll timeout
                // skips the parser check and always reports empty.
                while cevent::poll(Duration::from_millis(1)).unwrap_or(false) {
                    match cevent::read() {
                        Ok(e) => evs.push(e),
                        Err(err) => {
                            trace(
                                &rt,
                                &port,
                                app.active(),
                                "key",
                                &format!("input event read failed: {err}"),
                            );
                            break;
                        }
                    }
                }
            }
            Ok(false) => {}
            Err(err) => {
                trace(
                    &rt,
                    &port,
                    app.active(),
                    "key",
                    &format!("input poll failed: {err}; the TUI exits"),
                );
                break 'ui;
            }
        }

        let mut actions = Vec::new();
        for e in evs {
            match e {
                cevent::Event::Key(k) => {
                    // Only press/repeat matter; release events carry
                    // no information for this UI.
                    if matches!(
                        k.kind,
                        cevent::KeyEventKind::Press | cevent::KeyEventKind::Repeat
                    ) {
                        if let Some(key) = key_input(&k) {
                            actions.extend(app.press(key));
                        }
                    }
                }
                cevent::Event::Mouse(m) => match m.kind {
                    cevent::MouseEventKind::ScrollUp => actions.extend(app.press(Key::Wheel(-1))),
                    cevent::MouseEventKind::ScrollDown => actions.extend(app.press(Key::Wheel(1))),
                    _ => {}
                },
                // Resize: ratatui re-queries the terminal on each draw.
                _ => {}
            }
        }

        // 4. Execute the port-level actions.
        for action in actions {
            match action {
                Action::SendDraft => {
                    let Some(sid) = app.active().cloned() else {
                        continue;
                    };
                    let content = app.take_draft();
                    // The input queue toggle (docs/tui-pending-user-
                    // messages.md stage 2): the follow queue keeps the
                    // message for a turn restart; the steer queue is
                    // the default (the missing field).
                    let ev = if app.follow_queue() {
                        event::produce::user_message_follow(&content)
                    } else {
                        event::produce::user_message(&content)
                    };
                    match rt.block_on(port.append_event(&sid, &ev)) {
                        Ok(()) => {
                            app.flash("message sent");
                        }
                        Err(e) => {
                            trace(
                                &rt,
                                &port,
                                Some(&sid),
                                "port",
                                &format!("user message append failed: {e}"),
                            );
                            // Keep the text: the user must not lose a
                            // message that failed to land in the log.
                            app.set_draft(content);
                            app.flash(e.to_string());
                        }
                    }
                    if let Ok(list) = rt.block_on(port.list_sessions()) {
                        app.set_sessions(list);
                    }
                }
                Action::AnswerApproval(d) => {
                    let Some(sid) = app.active().cloned() else {
                        continue;
                    };
                    let Some(p) = app.oldest_pending_approval() else {
                        app.flash("no pending approval");
                        continue;
                    };
                    let built = match d {
                        Decision::Allow => {
                            Some(event::produce::approval(&p.request_id, "allow", None))
                        }
                        Decision::Deny => {
                            Some(event::produce::approval(&p.request_id, "deny", None))
                        }
                        Decision::Edit => {
                            let initial = p
                                .arguments
                                .clone()
                                .map(|v| serde_json::to_string_pretty(&v).unwrap_or_default())
                                .unwrap_or_else(|| "{}".to_string());
                            match edit_in_terminal(&initial, &mut term) {
                                Some(text) => {
                                    match serde_json::from_str::<serde_json::Value>(&text) {
                                        Ok(v) => Some(event::produce::approval(
                                            &p.request_id,
                                            "allow",
                                            Some(v),
                                        )),
                                        Err(_) => {
                                            app.flash(
                                                "edited text is not valid JSON; nothing appended",
                                            );
                                            None
                                        }
                                    }
                                }
                                None => {
                                    app.flash("edit aborted; nothing appended");
                                    None
                                }
                            }
                        }
                    };
                    let Some(ev) = built else { continue };
                    match rt.block_on(port.append_event(&sid, &ev)) {
                        Ok(()) => app.flash(match d {
                            Decision::Allow => "approval appended: allow",
                            Decision::Deny => "approval appended: deny",
                            Decision::Edit => "edited approval appended",
                        }),
                        Err(e) => {
                            trace(
                                &rt,
                                &port,
                                Some(&sid),
                                "port",
                                &format!("approval append failed: {e}"),
                            );
                            app.flash(e.to_string());
                        }
                    }
                }
                Action::RunLoop => {
                    let Some(sid) = app.active().cloned() else {
                        continue;
                    };
                    // The persistent probe is the one source of truth
                    // for loop liveness across a TUI restart (FT-003).
                    // It blocks a duplicate start, external or local.
                    match port.external_loop_pid(&sid) {
                        Ok(Some(pid)) => {
                            app.attach_external_loop(sid.clone());
                            trace(
                                &rt,
                                &port,
                                Some(&sid),
                                "loop_spawn",
                                &format!("skipped: a live loop holds the session (pid {pid})"),
                            );
                            app.flash("loop already active");
                            continue;
                        }
                        Ok(None) => {
                            app.clear_loop_running(&sid);
                        }
                        Err(e) => {
                            trace(
                                &rt,
                                &port,
                                Some(&sid),
                                "loop_spawn",
                                &format!("external loop probe failed: {e}"),
                            );
                            app.flash(e.to_string());
                            continue;
                        }
                    }
                    match rt.block_on(port.spawn_loop(&sid)) {
                        Ok(handle) => {
                            let lines = handle.take_lines().unwrap_or_else(empty_lines);
                            app.attach_loop(sid, handle, lines);
                            trace(&rt, &port, app.active(), "loop_spawn", "loop started");
                            app.flash("loop started");
                        }
                        Err(e) => {
                            trace(
                                &rt,
                                &port,
                                Some(&sid),
                                "loop_spawn",
                                &format!("loop spawn failed: {e}"),
                            );
                            app.flash(e.to_string());
                        }
                    }
                }
                Action::StopLoop => {
                    let Some(sid) = app.active().cloned() else {
                        continue;
                    };
                    let handle = app.loops_mut_for(&sid).and_then(|st| st.handle.take());
                    match handle {
                        Some(h) => {
                            h.stop();
                            trace(&rt, &port, Some(&sid), "loop_stop", "loop stop requested");
                            app.flash("loop stop requested");
                            // A stop is a decision worth surviving a
                            // restart: append the cancel event
                            // (docs/tui.md section 5).
                            let ev = event::produce::cancel("turn");
                            if let Err(e) = rt.block_on(port.append_event(&sid, &ev)) {
                                trace(
                                    &rt,
                                    &port,
                                    Some(&sid),
                                    "port",
                                    &format!("cancel event append failed: {e}"),
                                );
                                app.flash(e.to_string());
                            }
                        }
                        None => {
                            // No local handle: the loop may live on from
                            // an earlier TUI. Reattach through the
                            // persistent loop.pid and stop it (FT-003).
                            match port.stop_external_loop(&sid) {
                                Ok(Some(msg)) => {
                                    trace(&rt, &port, Some(&sid), "loop_stop", &msg);
                                    app.clear_loop_running(&sid);
                                    app.flash(msg);
                                    let ev = event::produce::cancel("turn");
                                    if let Err(e) = rt.block_on(port.append_event(&sid, &ev)) {
                                        trace(
                                            &rt,
                                            &port,
                                            Some(&sid),
                                            "port",
                                            &format!("cancel event append failed: {e}"),
                                        );
                                        app.flash(e.to_string());
                                    }
                                }
                                Ok(None) => {
                                    trace(
                                        &rt,
                                        &port,
                                        Some(&sid),
                                        "loop_stop",
                                        "no running loop to stop",
                                    );
                                    app.clear_loop_running(&sid);
                                    app.flash("no running loop to stop");
                                }
                                Err(e) => {
                                    trace(
                                        &rt,
                                        &port,
                                        Some(&sid),
                                        "loop_stop",
                                        &format!("external loop stop failed: {e}"),
                                    );
                                    app.flash(e.to_string());
                                }
                            }
                        }
                    }
                }
                Action::OpenEditor => {
                    let Some(sid) = app.active().cloned() else {
                        continue;
                    };
                    let _ = sid;
                    let old = app.take_draft();
                    match edit_in_terminal(&old, &mut term) {
                        Some(text) => app.set_draft(text),
                        None => {
                            app.set_draft(old);
                            app.flash("edit aborted; draft unchanged");
                        }
                    }
                }
                Action::CycleSessions(delta) => {
                    let list = match rt.block_on(port.list_sessions()) {
                        Ok(list) => list,
                        Err(e) => {
                            trace(
                                &rt,
                                &port,
                                app.active(),
                                "port",
                                &format!("session list read failed: {e}"),
                            );
                            Vec::new()
                        }
                    };
                    app.set_sessions(list);
                    let target = app.cycle_target(delta);
                    if let Some(id) = target {
                        if Some(&id) != app.active() {
                            let events = match rt.block_on(port.read_events(&id)) {
                                Ok(evs) => evs,
                                Err(e) => {
                                    trace(
                                        &rt,
                                        &port,
                                        Some(&id),
                                        "port",
                                        &format!("event read failed: {e}"),
                                    );
                                    Vec::new()
                                }
                            };
                            app.set_active(id.clone(), events.clone());
                            app.set_watch_rx(port.watch(&id, TailCursor::end()));
                            // Event ids restart per session: clear the
                            // reply caches and resend this session's
                            // history (docs/ui-extension.md section 4).
                            host.clear_replies();
                            host.send_history(&events);
                            resync_external_loop(&rt, &port, &mut app, &id);
                        }
                    } else {
                        app.flash("no sessions to cycle");
                    }
                }
                Action::ConfirmNewSession(name) => {
                    let sid = SessionId::new(&name);
                    let events = match rt.block_on(port.read_events(&sid)) {
                        Ok(evs) => evs,
                        Err(e) => {
                            trace(
                                &rt,
                                &port,
                                Some(&sid),
                                "port",
                                &format!("event read failed: {e}"),
                            );
                            Vec::new()
                        }
                    };
                    app.set_active(sid.clone(), events.clone());
                    app.set_watch_rx(port.watch(&sid, TailCursor::end()));
                    if let Ok(list) = rt.block_on(port.list_sessions()) {
                        app.set_sessions(list);
                    }
                    app.flash(format!("session {name} opened"));
                    host.clear_replies();
                    host.send_history(&events);
                    resync_external_loop(&rt, &port, &mut app, &sid);
                }
                Action::Handoff(name) => {
                    // The one-key resume of the automatic handoff
                    // (correction 57). Switch to the seeded session
                    // and start its loop. The old session's local loop
                    // stops when it still runs: the handoff
                    // supersedes it.
                    let Some(old_sid) = app.active().cloned() else {
                        continue;
                    };
                    let new_sid = SessionId::new(&name);
                    if new_sid == old_sid {
                        continue;
                    }
                    if let Some(h) = app.loops_mut_for(&old_sid).and_then(|st| st.handle.take()) {
                        h.stop();
                    }
                    // Keep the old session's events and reattach on a
                    // failed start: the user lands back where the
                    // marker lives.
                    let old_events = app.events().to_vec();
                    let events = match rt.block_on(port.read_events(&new_sid)) {
                        Ok(evs) => evs,
                        Err(e) => {
                            trace(
                                &rt,
                                &port,
                                Some(&new_sid),
                                "port",
                                &format!("event read failed: {e}"),
                            );
                            Vec::new()
                        }
                    };
                    app.set_active(new_sid.clone(), events.clone());
                    app.set_watch_rx(port.watch(&new_sid, TailCursor::end()));
                    if let Ok(list) = rt.block_on(port.list_sessions()) {
                        app.set_sessions(list);
                    }
                    host.clear_replies();
                    host.send_history(&events);
                    // Reattach the target's persistent loop state and
                    // block a double start (FT-003): a live loop for
                    // the target would get a second start here.
                    resync_external_loop(&rt, &port, &mut app, &new_sid);
                    match port.external_loop_pid(&new_sid) {
                        Ok(Some(pid)) => {
                            // A live loop owns the target session: a
                            // second start would double-append to its
                            // log. Stay on the old session.
                            app.set_active(old_sid.clone(), old_events);
                            app.set_watch_rx(port.watch(&old_sid, TailCursor::end()));
                            trace(
                                &rt,
                                &port,
                                Some(&new_sid),
                                "handoff",
                                &format!("blocked: a live loop holds the session (pid {pid})"),
                            );
                            app.flash(format!(
                                "handoff to {name} failed: a loop is already active"
                            ));
                        }
                        _ => match rt.block_on(port.spawn_loop(&new_sid)) {
                            Ok(handle) => {
                                let lines = handle.take_lines().unwrap_or_else(empty_lines);
                                app.attach_loop(new_sid, handle, lines);
                                app.flash(format!("handoff to {name} — loop started"));
                            }
                            Err(e) => {
                                app.set_active(old_sid.clone(), old_events);
                                app.set_watch_rx(port.watch(&old_sid, TailCursor::end()));
                                app.flash(format!("handoff to {name} failed: {e}"));
                            }
                        },
                    }
                }
                Action::ToggleToolExpand
                | Action::ToggleThinking
                | Action::ToggleThinkingExpand
                | Action::ToggleFollowQueue => {
                    // The state lives on the app; the next draw
                    // repaints. No port work.
                }
                Action::CycleEffort => {
                    // The reasoning-effort cycle (docs/tui-thinking-
                    // block.md section 4): the next effort value in
                    // the order, written to the active model's
                    // config entry. The loop publishes the new level
                    // on its next step; the input-area border moves
                    // then (docs/tui.md 7.2).
                    let active = resolve_active_model_name(&cfg);
                    match cycle_reasoning_effort(&cfg.config_path, &active) {
                        Ok(next) => {
                            let lvl = effort_level(&next);
                            app.set_thinking_level(lvl);
                            app.flash(format!(
                                "thinking level → {next} (level {lvl}) — the border follows on the next step"
                            ));
                        }
                        Err(e) => app.flash(e),
                    }
                }
                Action::SetEffort(value) => {
                    // Palette effort picker (docs/tui-command-palette.md
                    // section 8): write the chosen value to the active
                    // model's config entry.
                    let active = resolve_active_model_name(&cfg);
                    match set_effort(&cfg.config_path, &active, &value) {
                        Ok(v) => {
                            let lvl = effort_level(&v);
                            app.set_thinking_level(lvl);
                            app.flash(format!(
                                "thinking level → {v} (level {lvl})"
                            ));
                        }
                        Err(e) => app.flash(e),
                    }
                    // Update the palette's cached current effort so the
                    // `*` marker stays correct on the next open.
                    app.set_effort_current(value);
                }
                Action::SwitchSession(name) => {
                    // The palette's session sub-list (docs/tui-command-
                    // palette.md section 7): switch to the named session
                    // without the cycle delta.
                    let sid = SessionId::new(&name);
                    let events = match rt.block_on(port.read_events(&sid)) {
                        Ok(evs) => evs,
                        Err(e) => {
                            trace(
                                &rt,
                                &port,
                                Some(&sid),
                                "port",
                                &format!("event read failed: {e}"),
                            );
                            app.flash(format!("cannot switch to {name}: {e}"));
                            Vec::new()
                        }
                    };
                    app.set_active(sid.clone(), events.clone());
                    app.set_watch_rx(port.watch(&sid, TailCursor::end()));
                    host.clear_replies();
                    host.send_history(&events);
                    resync_external_loop(&rt, &port, &mut app, &sid);
                    app.flash(format!("switched to {name}"));
                }
                Action::InvokeExtCommand { ext, id, value } => {
                    // Extension command (docs/tui-command-palette.md
                    // section 10): send an invoke op to the owning
                    // extension process.
                    let req = host.request_invoke(&ext, &id, value.as_deref());
                    match req {
                        Some(req_id) => {
                            app.flash(format!(
                                "ext {ext}: {id} — pending (req {req_id})"
                            ));
                        }
                        None => {
                            app.flash(format!(
                                "ext {ext}: {id} — extension not available"
                            ));
                        }
                    }
                }
                Action::RecallQueue => {
                    // Bulk recall of pending user messages
                    // (docs/user-message-editing.md).
                    let retracted_ids = app.recall_queue();
                    if let Some(sid) = app.active().cloned() {
                        for id in &retracted_ids {
                            let ev = event::produce::user_message_retract(id);
                            if let Err(e) = rt.block_on(port.append_event(&sid, &ev)) {
                                app.flash(format!(
                                    "failed to retract message {id}: {e}"
                                ));
                            }
                        }
                    }
                }
                Action::Quit => {
                    // Loops keep running after the TUI exits. Each loop
                    // lives in its own session (setsid) and survives as
                    // an orphan. Dropping the handles does not stop the
                    // groups. Ctrl+C is the only loop interrupt
                    // (docs/tui.md section 5).
                    let _ = app.detach_all_handles();
                    // Extensions still die with the TUI: SIGTERM now,
                    // SIGKILL escalation in the background. No orphan
                    // extension outlives the TUI (docs/ui-extension.md
                    // section 7).
                    host.stop();
                    return finish(&mut term);
                }
            }
        }

        // 4.5 Extension ticks, transform timeouts, and resize
        // re-requests. The tick cadence is per extension; the main
        // loop drives the host because it owns the width, session,
        // and loop state.
        if let Some(sid) = app.active().cloned() {
            // The live model stream channel (docs/tui-streaming-response.md
            // section 6.2): poll the loop-owned file only while the loop
            // runs; a dead loop settles the live block (no stale block,
            // P5).
            match port.model_stream_path(&sid) {
                Ok(path) if app.loop_running(&sid) => app.refresh_stream(&path),
                _ => app.clear_stream(),
            }
        }
        // Release the queued stream content at the per-frame pace
        // before the draw (docs/tui-streaming-response.md section
        // 6.5): a few characters per frame instead of whole deltas,
        // so the text advances at a smooth frame rate.
        app.pump_stream_pacing();
        let width = term.size().map(|s| s.width as usize).unwrap_or(80);
        if width != last_width {
            // The re-request width is the transform budget: the
            // terminal width minus the border (2), the transcript
            // padding (2), and the content gutter (12), so the
            // re-rendered art fits the pane like the first pass.
            host.on_resize(width.saturating_sub(16));
            last_width = width;
        }
        let tick = ext::TickPayload {
            width,
            session: app.active().map(|s| s.as_str()),
            model: cfg.active_model.as_deref(),
            loop_running: app.active().is_some_and(|s| app.loop_running(s)),
            thinking: app.thinking_level(),
            statuses: app.ext_statuses(),
        };
        host.pump_ticks(&tick);
        // The frame owner also gets a cadence ping so its frame_spec
        // reply stays live (border color tracks the thinking level).
        host.pump_frame(&tick, &app.editor_mode_label());
        // The row owner (the host-reserved row above the input box)
        // gets the same cadence ping so its row_spec reply stays
        // live (docs/ui-extension.md section 4, `row` capability).
        host.pump_row(&tick, &app.editor_mode_label());
        host.poll_transforms();
        host.poll_status();
        host.poll_invokes();
        // Request extension commands when the palette opens.
        if app.palette_cmd_requested() {
            let loop_running = app.active().map(|s| app.loop_running(s)).unwrap_or(false);
            let session = app.active().map(|s| s.as_str());
            host.request_commands(session, loop_running);
        }
        // 4.6 The persistent loop probe, once a second (FT-003): it
        // flips the running bit when an external loop finishes, and
        // marks it when another TUI started a loop for this session.
        if last_loop_probe.elapsed() >= Duration::from_secs(1) {
            last_loop_probe = Instant::now();
            if let Some(sid) = app.active().cloned() {
                resync_external_loop(&rt, &port, &mut app, &sid);
            }
        }

        // 5. Draw.
        let mut cursor: Option<(u16, u16)> = None;
        if let Err(e) = term.draw(|f| render::draw(f, &mut app, &mut cursor, &host)) {
            trace(
                &rt,
                &port,
                app.active(),
                "render",
                &format!("draw failed: {e}"),
            );
            tui_log(&format!("draw failed: {e}"));
            break;
        }
        if let Some(pos) = cursor {
            let _ = term.set_cursor_position(pos);
        }
    }
    finish(&mut term);
}

/// Write one TUI trace record to the active session's trace log
/// (docs/tool-log-design_from_human.md). With no session there is
/// no session dir to hold the trace: the record is dropped. Start-up
/// failures before a session stay on stderr (fail loud at start).
/// The trace must not take the UI down: write failures are dropped.
fn trace(
    rt: &tokio::runtime::Runtime,
    port: &FileSessionPort,
    session: Option<&SessionId>,
    kind: &str,
    message: &str,
) {
    let Some(sid) = session else {
        return;
    };
    let _ = rt.block_on(port.append_trace(sid, kind, message));
}

/// A lines stream that is empty: for handles without output.
fn empty_lines() -> tokio::sync::mpsc::UnboundedReceiver<port::LoopLine> {
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    rx
}

/// Reattach the persistent loop state of one session (FT-003):
/// probe the session's `loop.pid`, mark the app state to match. The
/// probe is the one source of truth for loop liveness across a TUI
/// restart. A probe failure keeps the current state: it must not take
/// the UI down. State transitions trace a record.
fn resync_external_loop(
    rt: &tokio::runtime::Runtime,
    port: &FileSessionPort,
    app: &mut App,
    sid: &SessionId,
) {
    match port.external_loop_pid(sid) {
        Ok(Some(pid)) => {
            if !app.loop_running(sid) {
                app.attach_external_loop(sid.clone());
                trace(
                    rt,
                    port,
                    Some(sid),
                    "loop_reattach",
                    &format!("live external loop (pid {pid})"),
                );
            }
        }
        Ok(None) => {
            if app.is_external_loop(sid) {
                app.clear_loop_running(sid);
                trace(
                    rt,
                    port,
                    Some(sid),
                    "loop_reattach",
                    "no live loop group for the session",
                );
            }
        }
        Err(e) => {
            trace(
                rt,
                port,
                Some(sid),
                "port",
                &format!("external loop probe failed: {e}"),
            );
        }
    }
}

fn edit_in_terminal(
    initial: &str,
    term: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
) -> Option<String> {
    let suspend = || {
        let _ = terminal::disable_raw_mode();
        let mut out = std::io::stdout();
        let _ = out.execute(cevent::DisableMouseCapture);
    };
    let resume = || {
        let mut out = std::io::stdout();
        let _ = out.execute(cevent::EnableMouseCapture);
        let _ = terminal::enable_raw_mode();
    };
    let result = editor::run_editor(initial, suspend, resume);
    // The editor drew on the alternate screen; clear before the next
    // frame so its content does not bleed through.
    let _ = term.clear();
    match result {
        Ok(text) => Some(text),
        Err(e) => {
            tui_log(&format!("editor failed: {e}"));
            None
        }
    }
}

fn finish(term: &mut Terminal<CrosstermBackend<std::io::Stdout>>) {
    let _ = term.clear();
}

mod guardrail {
    //! docs/tui.md section 10: the TUI source must not contain loop
    //! internals or storage layout. Those strings live only in
    //! `port_file.rs` (the storage detail) and in this scan.
    #[test]
    fn storage_and_loop_strings_stay_behind_the_port() {
        let forbidden = [
            "turn.sh",
            "step.sh",
            "events.jsonl",
            "state.json",
            "pending/",
        ];
        let allowed = ["port_file.rs", "main.rs"];
        for entry in std::fs::read_dir("src").expect("tests run from the crate root") {
            let p = entry.unwrap().path();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            if allowed.iter().any(|a| a == &name) {
                continue;
            }
            let content = std::fs::read_to_string(&p).unwrap_or_default();
            for lit in forbidden {
                assert!(
                    !content.contains(lit),
                    "{name} contains the forbidden literal `{lit}` (docs/tui.md 10)"
                );
            }
        }
    }

    #[test]
    fn stage_names_are_not_strings_in_the_tui() {
        // Stage names are config values, not TUI strings.
        let forbidden = ["claim", "assemble"];
        let allowed = ["main.rs"];
        for entry in std::fs::read_dir("src").expect("tests run from the crate root") {
            let p = entry.unwrap().path();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            if allowed.iter().any(|a| a == &name) {
                continue;
            }
            let content = std::fs::read_to_string(&p).unwrap_or_default();
            for lit in forbidden {
                assert!(
                    !content.contains(lit),
                    "{name} contains the stage name `{lit}` (docs/tui.md 10)"
                );
            }
        }
    }
}

// ── startup thinking-level tests ─────────────────────────────
// These tests verify the fix for the startup bug where the
// config-derived thinking level was ignored and the stale
// session-log value (or the default "medium") persisted.

#[cfg(test)]
mod startup_effort_tests {
    use crate::app::{App, THINKING_LEVELS, THINKING_STATUS_ID};
    use crate::event::produce;
    use crate::port::SessionId;
    use super::{resolve_reasoning_effort, effort_level};

    /// Build an `App` whose session log carries a stale `model_thinking`
    /// event, mimicking the state `set_active` restores on resume.
    fn app_with_stale_thinking(stale_level: u32) -> App {
        let events = vec![
            produce::user_message("hello"),
            produce::ext_status(
                THINKING_STATUS_ID,
                serde_json::Value::from(stale_level),
            ),
        ];
        let mut app = App::new();
        app.set_active(SessionId::new("s1"), events);
        let expected = if stale_level < THINKING_LEVELS {
            stale_level
        } else {
            THINKING_LEVELS - 1
        };
        assert_eq!(app.thinking_level(), expected, "set_active restores the stale value");
        app
    }

    /// Mimic the startup restore block in `main()`: resolve the effort
    /// from config text, set both `effort_current` and `thinking_level`.
    fn apply_startup_restore(app: &mut App, config_text: &str, active_model: &str) {
        let effort = resolve_reasoning_effort(config_text, active_model);
        app.set_effort_current(effort.clone());
        app.set_thinking_level(effort_level(&effort));
    }

    /// Mirrors config-low.toml: global [model] has reasoning_effort = "low",
    /// the quoted model-specific table has no reasoning_effort key.
    const CONFIG_LOW_TOML: &str = r#"
[model]
api = "responses"
max_output_tokens = 32768
reasoning_effort = "low"

[model."Qwen3.8-27B-NVFP4-RTX5090-DFlash2"]
model_id = "Qwen3.8-27B-NVFP4-RTX5090-DFlash2"
base_url = "http://127.0.0.1:30000"
context_tokens = 262144

[active]
model = "Qwen3.8-27B-NVFP4-RTX5090-DFlash2"
"#;

    /// Mirrors config.toml: global [model] has reasoning_effort = "xhigh".
    const CONFIG_XHIGH_TOML: &str = r#"
[model]
api = "responses"
max_output_tokens = 32768
reasoning_effort = "xhigh"

[model."Qwen3.8-27B-NVFP4-RTX5090-DFlash2"]
model_id = "Qwen3.8-27B-NVFP4-RTX5090-DFlash2"
base_url = "http://127.0.0.1:30000"
context_tokens = 262144

[active]
model = "Qwen3.8-27B-NVFP4-RTX5090-DFlash2"
"#;

    #[test]
    fn low_config_overrides_stale_xhigh_log() {
        let mut app = app_with_stale_thinking(4);
        assert_eq!(app.thinking_level(), 4, "stale log value before restore");

        apply_startup_restore(&mut app, CONFIG_LOW_TOML, "Qwen3.8-27B-NVFP4-RTX5090-DFlash2");

        // The config value must beat the stale log value.
        assert_eq!(app.thinking_level(), 1, "border level must match config");
        // The palette item hint must show the config effort, not "medium".
        let hint = app
            .palette_items()
            .iter()
            .find(|i| i.id == "thinking-level")
            .expect("thinking-level item exists")
            .hint
            .clone();
        assert_eq!(hint, "low", "palette hint must reflect config effort");
    }

    #[test]
    fn xhigh_config_overrides_stale_medium_log() {
        let mut app = app_with_stale_thinking(2);
        assert_eq!(app.thinking_level(), 2, "stale medium before restore");

        apply_startup_restore(&mut app, CONFIG_XHIGH_TOML, "Qwen3.8-27B-NVFP4-RTX5090-DFlash2");

        assert_eq!(app.thinking_level(), 4, "border level must match config");
        let hint = app
            .palette_items()
            .iter()
            .find(|i| i.id == "thinking-level")
            .expect("thinking-level item exists")
            .hint
            .clone();
        assert_eq!(hint, "xhigh", "palette hint must reflect config effort");
    }

    #[test]
    fn effort_level_mapping_is_correct() {
        assert_eq!(effort_level("none"), 0);
        assert_eq!(effort_level("minimal"), 1);
        assert_eq!(effort_level("low"), 1);
        assert_eq!(effort_level("medium"), 2);
        assert_eq!(effort_level("high"), 3);
        assert_eq!(effort_level("xhigh"), 4);
        assert_eq!(effort_level("max"), 4);
        assert_eq!(effort_level("unknown"), 0);
    }

    #[test]
    fn resolve_reasoning_effort_global_fallback() {
        let toml = r#"
[model]
reasoning_effort = "low"

[model.my-model]
model_id = "my-model"
"#;
        let effort = resolve_reasoning_effort(toml, "my-model");
        assert_eq!(effort, "low", "global fallback must work");
    }

    #[test]
    fn resolve_reasoning_effort_model_specific_wins() {
        let toml = r#"
[model]
reasoning_effort = "low"

[model.my-model]
reasoning_effort = "xhigh"
"#;
        let effort = resolve_reasoning_effort(toml, "my-model");
        assert_eq!(effort, "xhigh", "per-model value must win");
    }

    #[test]
    fn resolve_reasoning_effort_default_medium() {
        let toml = r#"
[model]
api = "responses"
"#;
        let effort = resolve_reasoning_effort(toml, "some-model");
        assert_eq!(effort, "medium", "default when no key exists");
    }

    #[test]
    fn resolve_reasoning_effort_off_maps_to_none() {
        let toml = r#"
[model]
reasoning_effort = "off"
"#;
        let effort = resolve_reasoning_effort(toml, "any-model");
        assert_eq!(effort, "none");
    }
}

// ── tui_log tests (FT-017) ──────────────────────────────────────

#[cfg(test)]
mod tui_log_tests {
    use super::tui_log;
    use tempfile::TempDir;

    /// `TUI_LOG` wins over the cache default and the line lands in
    /// the file. One test owns the process-global `TUI_LOG` var.
    /// Keep it that way to avoid a race with parallel tests.
    #[test]
    fn tui_log_writes_to_the_tui_log_file() {
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("tui.log");
        std::env::set_var("TUI_LOG", log.as_os_str());

        // The env override wins over the default cache path.
        let p = super::tui_log_path().unwrap();
        assert_eq!(p, log, "TUI_LOG wins over the default cache path");

        tui_log("startup model=m effort=high level=3");
        let body = std::fs::read_to_string(&log).unwrap();
        let l = body.lines().next().unwrap();
        // The line is `pid ms msg` with the msg carrying the text.
        let msg = l.split(' ').skip(2).collect::<Vec<_>>().join(" ");
        assert_eq!(msg, "startup model=m effort=high level=3");
    }

    /// `TermGuard::init` call, so no raw stderr write may follow
    /// that point in `main.rs`. Diagnostics append to the log file
    /// instead. The pattern is split so this test source does not
    /// trip its own scan.
    #[test]
    fn no_stderr_writes_after_the_terminal_takeover() {
        let src =
            std::fs::read_to_string("src/main.rs").expect("tests run from the crate root");
        let at = src
            .find("let _guard = TermGuard::init()")
            .expect("the terminal takeover call exists");
        let after = &src[at..];
        let pat = ["eprint", "ln!"].concat();
        assert!(
            !after.contains(&pat),
            "main.rs writes raw stderr after the terminal takeover (FT-017)"
        );
    }
}

#[cfg(test)]
mod key_input_tests {
    use super::key_input;
    use crate::app::Key;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    #[test]
    fn ctrl_i_maps_to_key_ctrl_i() {
        let ev = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::CONTROL);
        assert_eq!(key_input(&ev), Some(Key::CtrlI));
    }

    #[test]
    fn ctrl_i_repeat_events_also_map() {
        use crossterm::event::KeyEventState;
        let ev = KeyEvent {
            code: KeyCode::Char('i'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Repeat,
            state: KeyEventState::NONE,
        };
        assert_eq!(key_input(&ev), Some(Key::CtrlI));
    }

    #[test]
    fn plain_i_is_not_ctrl_i() {
        let ev = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE);
        assert_eq!(key_input(&ev), Some(Key::Char('i')));
    }

    #[test]
    fn tab_maps_to_key_tab() {
        // In a standard terminal a physical Ctrl+I press arrives as
        // byte 0x09, which crossterm parses as KeyCode::Tab (not
        // Char('i')+CONTROL). The picker binds Tab to the scope cycle
        // so Ctrl+I works end to end (docs/tui-file-picker.md P9).
        let ev = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(key_input(&ev), Some(Key::Tab));
    }
}
