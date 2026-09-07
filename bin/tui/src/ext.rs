//! The UI extension host (docs/ui-extension.md, ui-extension-plan
//! stage 1).
//!
//! An extension is an external process. One JSONL boundary over
//! stdio. The TUI hosts it: spawn, supervise, forward log events,
//! collect styled replies. No extension code compiles into the TUI.
//!
//! This module owns (docs/ui-extension.md sections 3, 4, 6, 7):
//! - `ext.toml` manifest parse and fail-loud validation
//! - discovery: the global dir, then the project dir. A project
//!   entry overrides a global entry by name. Layer order is
//!   host-fixed; within a layer, alphabetical
//! - one process per extension in its own process group (`setsid`),
//!   stdout pump, restart budget 1 s / 2 s / 4 s, then a dead hint
//! - ops: `event` (kind filter), `tick`, `transform` / `transformed`
//!   (request id, 2 s timeout, stale replies drop), `lines`,
//!   `status`, `append` (whitelist), `notify`
//! - per-op G5 fallback (docs/refinement-policy.md G5):
//!   `lines` drops to the built-in render, `status` keeps the last
//!   valid row, `transformed` shows the raw block, `append` is
//!   rejected with a flash
//!
//! The host holds no extension logic. It hosts and supervises
//! (docs/ui-extension.md section 10).

use crate::config::TuiConfig;
use crate::event::{Event, EventKind};
use ratatui::style::{Color, Modifier, Style};
use bon::builder;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, VecDeque};
use std::ffi::CString;
use std::io::{BufRead, BufWriter, Write};
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// Restart budget (docs/ui-extension.md section 7): three restart
/// attempts with 1 s / 2 s / 4 s backoff, then dead.
pub const RESTART_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];
/// A slow `transform` times out at 2 s. The fallback is the raw block.
pub const TRANSFORM_TIMEOUT: Duration = Duration::from_secs(2);
/// Grace between SIGTERM and SIGKILL when the TUI quits, like the
/// loop stop (docs/tui.md section 13.3).
const STOP_GRACE: Duration = Duration::from_secs(3);
/// The protocol version this host speaks.
const PROTOCOL_V: u64 = 1;
/// Default tick cadence for status extensions.
const DEFAULT_TICK_MS: u64 = 1000;
/// The first reply of a generation gets a wider window than the
/// steady-state bound: a cold start (a git spawn on a cold cache)
/// can exceed 3 x tick_ms without the extension being stuck. After
/// the first valid reply, the steady bound applies from the last
/// reply (ui-extension-plan stage 4 open items: status staleness).
const INITIAL_STATUS_GRACE: Duration = Duration::from_secs(10);
/// The capability names a manifest may list.
pub const CAPS: &[&str] = &[
    "render",
    "status",
    "transform",
    "append",
    "notify",
    "frame",
    "commands",
    "row",
];
/// Cap on the number of cached extension line-replies per slot. The
/// render layer only displays the last `TRANSCRIPT_EVENT_CAP` events,
/// so keeping more than a multiple of that in memory is wasted. The
/// cache evicts its oldest entry when it reaches this cap, so an
/// active stream (the newest entry) is never wiped. This prevents
/// unbounded growth in long-running sessions.
const LINES_CACHE_CAP: usize = 4096;

/// Bounded event-id → lines cache with FIFO eviction. `map` gives the
/// render hot path O(1) lookup; `order` records insertion order so an
/// overflow evicts the oldest entry instead of wiping the cache. The
/// newest entry is the live in-progress stream under streaming render;
/// FIFO eviction never touches it. A re-upsert of an existing key
/// updates the value in place and keeps its position in `order`, so
/// streaming chunks cost one hash write and no reorder.
#[derive(Default)]
struct LinesCache {
    map: HashMap<u64, Vec<ExtLine>>,
    order: VecDeque<u64>, // oldest → newest
}

impl LinesCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }
    /// Insert or update one entry. A new key evicts the oldest entry
    /// at the cap; an existing key is updated in place.
    fn upsert(&mut self, id: u64, lines: Vec<ExtLine>) {
        if !self.map.contains_key(&id) {
            if self.map.len() >= LINES_CACHE_CAP {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
            self.order.push_back(id);
        }
        self.map.insert(id, lines);
    }
    fn get(&self, id: &u64) -> Option<&Vec<ExtLine>> {
        self.map.get(id)
    }
    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
    #[allow(dead_code)]
    fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    #[allow(dead_code)]
    fn len(&self) -> usize {
        self.map.len()
    }
}

/// One parsed and validated `ext.toml` (docs/ui-extension.md section 3).
#[derive(Debug, Clone)]
pub struct Manifest {
    /// The directory name; the host shows it in hints and flashes.
    pub name: String,
    /// The directory that holds `ext.toml` and the extension script.
    pub dir: PathBuf,
    /// The `ext.toml` path. Fail-loud errors name this file.
    pub manifest_path: PathBuf,
    /// The raw `command` string from the manifest (used in error
    /// messages and the debug log).
    #[allow(dead_code)]
    pub command: String,
    /// The resolved absolute executable path the host execs. A bare
    /// name resolves on `PATH`; a relative path resolves against
    /// [`Manifest::dir`]; an absolute path is used as-is.
    /// See [`resolve_command`].
    pub command_path: PathBuf,
    pub args: Vec<String>,
    /// Capability names; always a subset of [`CAPS`].
    pub caps: Vec<String>,
    /// Event wire names the host forwards. Empty means all.
    pub kinds: Vec<String>,
    /// Tick cadence in milliseconds for a status extension.
    pub tick_ms: u64,
    /// Rewrite targets, scope-qualified: `fence:<lang>` or
    /// `inline:<delim>`.
    pub transform: Vec<String>,
    /// Log event types this extension may append. Empty means none.
    pub append_types: Vec<String>,
    /// `false` when the manifest declares no `protocol_v` or a value
    /// this host does not speak. The host skips such an extension and
    /// flashes the reason; it does not refuse the start.
    pub protocol_ok: bool,
}

/// A misconfiguration that refuses the start. The message names the
/// file at fault (docs/ui-extension.md section 6: a wrong entry is a
/// misconfiguration, not a silent skip).
#[derive(Debug)]
pub struct ExtError {
    pub message: String,
}

impl std::fmt::Display for ExtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ExtError {}

impl ExtError {
    /// A manifest-level failure. The path is in the message.
    fn refuse(file: &Path, what: &str) -> Self {
        ExtError {
            message: format!("ext manifest {}: {what}", file.display()),
        }
    }
}

/// The raw `[ext]` block of one `ext.toml`.
#[derive(Debug, Default, Deserialize)]
struct RawManifest {
    ext: RawExtBlock,
}

#[derive(Debug, Default, Deserialize)]
struct RawExtBlock {
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    caps: Vec<String>,
    #[serde(default)]
    kinds: Vec<String>,
    #[serde(default)]
    tick_ms: Option<u64>,
    #[serde(default)]
    transform: Vec<String>,
    #[serde(default)]
    append_types: Vec<String>,
    protocol_v: Option<u64>,
}

/// The host-fixed layers (docs/ui-extension.md section 6): built-in
/// renderers first, then the global dir, then the project dir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Global,
    Project,
}

/// One extension entry loaded from a layer.
#[derive(Debug, Clone)]
pub struct LoadedExt {
    pub manifest: Manifest,
    /// Which layer won for this name (the project layer overrides).
    /// Read by the Stage 4 `order.toml` work; not read in Stage 1.
    #[allow(dead_code)]
    pub layer: Layer,
}

/// The composed extension sequence and its ownership maps.
#[derive(Debug, Clone)]
pub struct Discovery {
    /// Composed order: global layer first, then the project layer.
    /// Within a layer, alphabetical by name. A project entry replaces
    /// a global entry of the same name.
    pub exts: Vec<LoadedExt>,
    /// The single `status` owner index, if one exists. Two owners
    /// refuse the start.
    pub status_owner: Option<usize>,
    /// Event kind -> first extension in the sequence that lists it.
    pub kind_owners: HashMap<EventKind, usize>,
    /// Transform target scope -> first extension that declares it.
    /// Read by the Stage 3 span extraction; not read in Stage 1.
    #[allow(dead_code)]
    pub transform_owners: HashMap<String, usize>,
    /// Extension name -> index into [`Discovery::exts`].
    pub index_by_name: HashMap<String, usize>,
    /// The single `frame` owner index, if one exists. The frame owns
    /// the input-area rendering (border style, border color, the
    /// label); one owner across the whole sequence, like `status`.
    pub frame_owner: Option<usize>,
    /// The single `row` owner index, if one exists. The row owner
    /// owns the host-reserved content row above the input box
    /// (between the working row and the input area); one owner
    /// across the whole sequence, like `status` and `frame`.
    pub row_owner: Option<usize>,
}

impl Discovery {
    pub fn owner_name(&self, idx: usize) -> &str {
        &self.exts[idx].manifest.name
    }
}

/// Scan the global dir, then the project dir (docs/ui-extension.md
/// sections 3 and 6). A malformed manifest or a broken command
/// refuses the start and names the file.
pub fn discover(cfg: &TuiConfig) -> Result<Discovery, ExtError> {
    let global_dir = cfg
        .ext_dir
        .clone()
        .unwrap_or_else(|| cfg.config_dir.join("ui_extensions"));
    let project_dir = cfg.config_dir.join(".pi").join("ui_extensions");
    let mut globals = load_layer(&global_dir, Layer::Global)?;
    let mut projects = load_layer(&project_dir, Layer::Project)?;
    let project_names: std::collections::HashSet<&str> =
        projects.iter().map(|e| e.manifest.name.as_str()).collect();
    globals.retain(|e| !project_names.contains(e.manifest.name.as_str()));
    let mut exts: Vec<LoadedExt> = Vec::new();
    exts.append(&mut globals);
    exts.append(&mut projects);

    // The status row allows one owner across the whole sequence.
    // Two owners refuse the start; the error names both files.
    let status: Vec<usize> = (0..exts.len())
        .filter(|&i| exts[i].manifest.caps.iter().any(|c| c == "status"))
        .collect();
    if status.len() > 1 {
        let names = status
            .iter()
            .map(|&i| exts[i].manifest.manifest_path.display().to_string())
            .collect::<Vec<_>>()
            .join(" and ");
        return Err(ExtError {
            message: format!(
                "two or more extensions own the `status` row: {names}. \
                 The host allows one owner across the whole sequence."
            ),
        });
    }
    let status_owner = status.into_iter().next();

    // The frame row allows one owner across the whole sequence, like
    // the status row: the input-area rendering is one surface.
    let frames: Vec<usize> = (0..exts.len())
        .filter(|&i| exts[i].manifest.caps.iter().any(|c| c == "frame"))
        .collect();
    if frames.len() > 1 {
        let names = frames
            .iter()
            .map(|&i| exts[i].manifest.manifest_path.display().to_string())
            .collect::<Vec<_>>()
            .join(" and ");
        return Err(ExtError {
            message: format!(
                "two or more extensions own the `frame` capability: {names}. \
                 The host allows one owner across the whole sequence."
            ),
        });
    }
    let frame_owner = frames.into_iter().next();

    // The row slot allows one owner across the whole sequence, like
    // the status and frame slots: one surface (the host-reserved
    // content row above the input box), one owner.
    let rows: Vec<usize> = (0..exts.len())
        .filter(|&i| exts[i].manifest.caps.iter().any(|c| c == "row"))
        .collect();
    if rows.len() > 1 {
        let names = rows
            .iter()
            .map(|&i| exts[i].manifest.manifest_path.display().to_string())
            .collect::<Vec<_>>()
            .join(" and ");
        return Err(ExtError {
            message: format!(
                "two or more extensions own the `row` slot: {names}. \
                 The host allows one owner across the whole sequence."
            ),
        });
    }
    let row_owner = rows.into_iter().next();

    let mut kind_owners = HashMap::new();
    let mut transform_owners = HashMap::new();
    for (i, e) in exts.iter().enumerate() {
        for k in &e.manifest.kinds {
            if let Some(kind) = EventKind::from_wire(k) {
                kind_owners.entry(kind).or_insert(i);
            }
        }
        for t in &e.manifest.transform {
            transform_owners.entry(t.clone()).or_insert(i);
        }
    }
    let mut index_by_name = HashMap::new();
    for (i, e) in exts.iter().enumerate() {
        index_by_name.insert(e.manifest.name.clone(), i);
    }
    Ok(Discovery {
        exts,
        status_owner,
        frame_owner,
        row_owner,
        kind_owners,
        transform_owners,
        index_by_name,
    })
}

/// Load one layer directory. A missing directory is an empty layer.
/// An entry directory without an `ext.toml` refuses the start.
fn load_layer(dir: &Path, layer: Layer) -> Result<Vec<LoadedExt>, ExtError> {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(ExtError {
                message: format!("ext dir {}: {e}", dir.display()),
            })
        }
    };
    let mut entries: Vec<PathBuf> = read
        .flatten()
        .filter_map(|e| e.path().is_dir().then_some(e.path()))
        .collect();
    entries.sort();
    let mut out = Vec::new();
    for entry in entries {
        if !entry.join("ext.toml").is_file() {
            return Err(ExtError {
                message: format!("ext entry {} has no ext.toml manifest", entry.display()),
            });
        }
        out.push(LoadedExt {
            manifest: load_manifest(&entry)?,
            layer,
        });
    }
    Ok(out)
}

/// Parse and validate one `ext.toml`. Every failure names the file.
fn load_manifest(entry: &Path) -> Result<Manifest, ExtError> {
    let manifest_path = entry.join("ext.toml");
    let raw_text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| ExtError::refuse(&manifest_path, &format!("cannot read manifest: {e}")))?;
    let raw: RawManifest = toml::from_str(&raw_text)
        .map_err(|e| ExtError::refuse(&manifest_path, &format!("invalid TOML: {e}")))?;
    let b = raw.ext;
    let command = b
        .command
        .as_deref()
        .ok_or_else(|| ExtError::refuse(&manifest_path, "missing `command`"))?;
    if command.trim().is_empty() {
        return Err(ExtError::refuse(
            &manifest_path,
            "`command` must not be empty",
        ));
    }
    for cap in &b.caps {
        if !CAPS.contains(&cap.as_str()) {
            return Err(ExtError::refuse(
                &manifest_path,
                &format!("unknown cap `{cap}` (expected one of {CAPS:?})"),
            ));
        }
    }
    for kind in &b.kinds {
        if EventKind::from_wire(kind).is_none() {
            return Err(ExtError::refuse(
                &manifest_path,
                &format!("unknown event kind `{kind}` in `kinds`"),
            ));
        }
    }
    for t in &b.transform {
        if !is_valid_target(t) {
            return Err(ExtError::refuse(
                &manifest_path,
                &format!(
                    "bad transform target `{t}` (expected `fence:<lang>` or `inline:<delim>`)"
                ),
            ));
        }
    }
    if b.tick_ms == Some(0) {
        return Err(ExtError::refuse(
            &manifest_path,
            "`tick_ms` must be positive",
        ));
    }
    let command_path = match resolve_command(entry, command) {
        Some(p) => p,
        None => {
            return Err(ExtError::refuse(
                &manifest_path,
                &format!("command `{command}` not found"),
            ));
        }
    };
    Ok(Manifest {
        name: entry
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        dir: entry.to_path_buf(),
        manifest_path,
        command: command.to_string(),
        command_path,
        args: b.args,
        caps: b.caps,
        kinds: b.kinds,
        tick_ms: b.tick_ms.unwrap_or(DEFAULT_TICK_MS),
        transform: b.transform,
        append_types: b.append_types,
        protocol_ok: b.protocol_v == Some(PROTOCOL_V),
    })
}

/// A transform target is scope-qualified: `fence:<lang>` or
/// `inline:<delim>`, with a non-empty name.
fn is_valid_target(t: &str) -> bool {
    let Some((scope, name)) = t.split_once(':') else {
        return false;
    };
    matches!(scope, "fence" | "inline") && !name.is_empty()
}

/// Resolve the manifest `command` to an absolute executable path.
///
/// - A bare command name (no `/`) is looked up on the TUI process's
///   `PATH`; it must name a real file there.
/// - A path (contains `/`) is used directly when absolute, or
///   resolved against the manifest directory when relative. This lets
///   a layer ship a bundled reference binary (e.g. `ui_extensions/
///   mermaid`'s `target/debug/mermaid-ext`) and address it without
///   putting a build dir on `PATH` (docs/ui-extension.md section 9:
///   the reference layer ships one opt-in binary; the host resolves it).
///
/// Returns the absolute path when the command resolves to a real
/// file, else `None` (fail-loud at scan, docs/ui-extension.md
/// section 6). The caller names the file in the error.
fn resolve_command(dir: &Path, command: &str) -> Option<PathBuf> {
    if command.contains('/') {
        let p = Path::new(command);
        let resolved = if p.is_absolute() {
            p.to_path_buf()
        } else {
            dir.join(command)
        };
        resolved.is_file().then_some(resolved)
    } else {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|d| d.join(command))
                .find(|c| c.is_file())
        })
    }
}

/// One styled span from an extension reply. The host converts the
/// wire style `{fg, bg, bold}` to a `Span` style; no raw ANSI
/// crosses the channel (docs/ui-extension.md section 4).
#[derive(Debug, Clone, PartialEq)]
pub struct ExtSpan {
    pub text: String,
    pub style: Style,
}

/// One line of an extension reply, in display order. A line is a
/// list of spans. The wire shape is a string, a `[text, style]` pair
/// (a single-span line), or an array of `[text, style]` pairs (a
/// multi-span line; docs/ui-extension.md section 4).
#[derive(Debug, Clone, PartialEq)]
pub struct ExtLine {
    /// The span texts joined in order. For a single-span line it is
    /// the full line text.
    pub text: String,
    /// The first span's style. A line-level consumer that needs one
    /// style uses this; the full fidelity lives in `spans`.
    pub style: Style,
    /// Every span of the line, in display order. One entry for a
    /// single-span line.
    pub spans: Vec<ExtSpan>,
}

impl ExtLine {
    /// One span with the terminal default style.
    pub fn plain(text: impl Into<String>) -> Self {
        Self::styled(text, Style::default())
    }

    /// One span with one style.
    pub fn styled(text: impl Into<String>, style: Style) -> Self {
        let t: String = text.into();
        ExtLine {
            spans: vec![ExtSpan {
                text: t.clone(),
                style,
            }],
            text: t,
            style,
        }
    }

    /// A line of several spans. The `text` field joins the span
    /// texts; the `style` field is the first span's style.
    pub fn multi(spans: Vec<ExtSpan>) -> Self {
        let text = spans.iter().map(|s| s.text.clone()).collect();
        let style = spans.first().map(|s| s.style).unwrap_or_default();
        ExtLine { text, style, spans }
    }
}

/// A color value in a wire style: a hex string (`#rgb`, `#rrggbb`)
/// or a theme token. An unknown token degrades to no color (G5: a
/// reply never crashes the TUI).
fn parse_color(v: &Value) -> Option<Color> {
    let s = v.as_str()?.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if !hex.is_ascii() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let parse = |s: &str| u8::from_str_radix(s, 16).ok();
        return match hex.len() {
            3 => {
                let r = parse(&hex[0..1])? * 17;
                let g = parse(&hex[1..2])? * 17;
                let b = parse(&hex[2..3])? * 17;
                Some(Color::Rgb(r, g, b))
            }
            6 => {
                let r = parse(&hex[0..2])?;
                let g = parse(&hex[2..4])?;
                let b = parse(&hex[4..6])?;
                Some(Color::Rgb(r, g, b))
            }
            _ => None,
        };
    }
    Some(match s {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::White,
        "gray" | "grey" | "darkgray" | "dark_gray" => Color::DarkGray,
        _ => return None,
    })
}

/// Validate the `frame_spec` reply payload. The wire shape is an
/// object:
///
/// ```json
/// {"border": "rounded|plain|double|thick",
///  "label": {"lines": [...], "style": {"fg":..., "bg":..., "bold":...}},
///  "height": 2}
/// ```
///
/// Every field is optional; a missing field keeps the host's
/// built-in value. A malformed shape is `None` (G5: the last valid
/// frame survives).
fn frame_spec_value(v: &Value) -> Option<FrameSpec> {
    let obj = v.as_object()?;
    let mut border = None;
    let mut label = None;
    let mut height = None;
    if let Some(b) = obj.get("border").and_then(|x| x.as_str()) {
        let parsed = FrameBorderStyle::parse(b)?;
        border = Some(parsed);
    }
    if let Some(l) = obj.get("label") {
        if !l.is_null() {
            let lines = lines_value(&l["lines"])?;
            let style = wire_style(l.get("style").and_then(|s| s.as_object()));
            label = Some((lines, style));
        }
    }
    if let Some(h) = obj.get("height").and_then(|x| x.as_u64()) {
        height = Some((h.min(64)) as usize);
    }
    Some(FrameSpec {
        border,
        label,
        height,
    })
}

fn wire_style(obj: Option<&Map<String, Value>>) -> Style {
    let mut s = Style::default();
    if let Some(o) = obj {
        if let Some(c) = o.get("fg").and_then(parse_color) {
            s = s.fg(c);
        }
        if let Some(c) = o.get("bg").and_then(parse_color) {
            s = s.bg(c);
        }
        if o.get("bold") == Some(&Value::Bool(true)) {
            s = s.add_modifier(Modifier::BOLD);
        }
    }
    s
}

/// Validate the `lines` payload of a `lines`, `status`, or
/// `transformed` reply. A valid payload is an array of line items.
/// A line item is a bare string, a `[text, style]` pair, or an
/// array of `[text, style]` pairs (a multi-span line; the host
/// draws the spans left to right on one row). An invalid shape is
/// `None`: the per-op G5 fallback applies and the TUI stays up.
fn lines_value(v: &Value) -> Option<Vec<ExtLine>> {
    let arr = v.as_array()?;
    let mut out: Vec<ExtLine> = Vec::with_capacity(arr.len());
    for item in arr {
        match item {
            Value::String(t) => out.push(ExtLine::plain(t.clone())),
            Value::Array(pair) if pair.len() == 2 && pair[0].is_string() => {
                let text = pair[0].as_str()?.to_string();
                let style = match &pair[1] {
                    Value::Object(o) => wire_style(Some(o)),
                    Value::Null => Style::default(),
                    _ => return None,
                };
                out.push(ExtLine::styled(text, style));
            }
            Value::Array(spans) => {
                // A multi-span line: each element is a [text, style] pair.
                let mut parsed: Vec<ExtSpan> = Vec::with_capacity(spans.len());
                for sp in spans {
                    let pair = sp.as_array()?;
                    if pair.len() != 2 {
                        return None;
                    }
                    let text = pair[0].as_str()?.to_string();
                    let style = match &pair[1] {
                        Value::Object(o) => wire_style(Some(o)),
                        Value::Null => Style::default(),
                        _ => return None,
                    };
                    parsed.push(ExtSpan { text, style });
                }
                if parsed.is_empty() {
                    return None;
                }
                out.push(ExtLine::multi(parsed));
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Lower a wire line's styles (line style + every span) to the
/// terminal capability. Named ANSI colors pass through unchanged
/// (`color::lower` maps them to the swatch indices), so this only
/// reshapes free-form RGB from hex wire colors.
fn lower_ext_lines(lines: Vec<ExtLine>, level: crate::color::Level) -> Vec<ExtLine> {
    use crate::color::lower_style;
    lines
        .into_iter()
        .map(|mut l| {
            l.style = lower_style(l.style, level);
            l.spans = l
                .spans
                .into_iter()
                .map(|mut s| {
                    s.style = lower_style(s.style, level);
                    s
                })
                .collect();
            l
        })
        .collect()
}

/// Lower a `frame_spec` label's styles to the terminal capability.
fn lower_frame_spec(mut spec: FrameSpec, level: crate::color::Level) -> FrameSpec {
    use crate::color::lower_style;
    if let Some((lines, style)) = &mut spec.label {
        *lines = lower_ext_lines(lines.clone(), level);
        *style = lower_style(*style, level);
    }
    spec
}

/// One command or setting owned by an extension, as declared in a
/// `commands_list` reply (docs/tui-command-palette.md section 10).
#[derive(Debug, Clone, PartialEq)]
pub struct ExtCommand {
    /// Stable command id, e.g. `"reload"` or `"theme"`.
    pub id: String,
    /// Display label shown in the palette.
    pub label: String,
    /// `true` when the command carries options (a setting).
    pub is_setting: bool,
    /// Optional hint (keybinding, current value, …).
    pub hint: String,
    /// Help text shown in the preview pane.
    pub help: String,
    /// Option values for a setting. Empty for plain commands.
    pub options: Vec<String>,
}

/// Parse a `commands_list` reply payload. Returns `None` if the
/// `commands` field is missing or malformed (G5: keep the last
/// valid list).
fn parse_commands_list(v: &Value) -> Option<Vec<ExtCommand>> {
    let arr = v.get("commands")?.as_array()?;
    let mut out = Vec::new();
    for item in arr {
        let obj = item.as_object()?;
        let id = obj.get("id")?.as_str()?.to_string();
        let label = obj
            .get("label")
            .and_then(|l| l.as_str())
            .unwrap_or(&id)
            .to_string();
        let is_setting = obj.get("kind").and_then(|k| k.as_str()) == Some("set");
        let hint = obj.get("hint").and_then(|h| h.as_str()).unwrap_or("").to_string();
        let help = obj.get("help").and_then(|h| h.as_str()).unwrap_or("").to_string();
        let options = obj
            .get("options")
            .and_then(|o| o.as_array())
            .map(|arr| arr.iter().filter_map(|o| o.as_str().map(String::from)).collect())
            .unwrap_or_default();
        out.push(ExtCommand {
            id,
            label,
            is_setting,
            hint,
            help,
            options,
        });
    }
    Some(out)
}

/// Items the host reports to the TUI main loop. The main loop owns
/// the side effects: log appends go through the port, flashes hit
/// the status row, notify ops hit the terminal the host owns.
#[derive(Debug, Clone)]
pub enum ExtItem {
    /// A valid `lines` reply landed in the cache. The transcript
    /// cache folds the reply version in, so a rebuild picks it up.
    /// The `ext` field is informational for logs; the main loop does
    /// not read it in Stage 1.
    #[allow(dead_code)]
    LinesCached { ext: String },
    /// A valid `status` reply replaced the last valid row.
    /// The `ext` field is informational for logs; the main loop does
    /// not read it in Stage 1.
    #[allow(dead_code)]
    StatusUpdated { ext: String },
    /// A valid `frame_spec` reply replaced the input-area frame.
    /// Informational: the frame re-renders on the next draw.
    #[allow(dead_code)]
    FrameUpdated { ext: String },
    /// A valid `row_spec` reply replaced the host-reserved row
    /// content above the input box. Informational: the row
    /// re-renders on the next draw.
    #[allow(dead_code)]
    RowUpdated { ext: String },
    /// A matching `transformed` reply completed its request.
    /// The `req` field is informational for logs; the main loop does
    /// not read it in Stage 1.
    #[allow(dead_code)]
    TransformedCached { req: u64 },
    /// A whitelisted `append` request. The main loop appends it via
    /// the port; G3 schema validation happens there.
    AppendReq { ext: String, event: Value },
    /// An `append` the host refused. The main loop flashes the
    /// reason (docs/ui-extension.md section 4 fallback).
    AppendRejected { ext: String, reason: String },
    /// A `notify` bell. The host applies it on its own terminal.
    /// The `ext` field is informational for logs; the main loop does
    /// not read it in Stage 1.
    #[allow(dead_code)]
    NotifyBell { ext: String },
    /// A `notify` OSC sequence. The host applies it on its own
    /// terminal. The `ext` field is informational for logs; the main
    /// loop does not read it in Stage 1.
    #[allow(dead_code)]
    NotifyOsc {
        ext: String,
        code: u16,
        args: String,
    },
    /// The restart budget ran out. The owned row shows a dead hint.
    Dead { ext: String },
    /// The host skipped an extension (protocol mismatch or a spawn
    /// failure). The main loop flashes the reason.
    Skipped { ext: String, reason: String },
    /// A `commands_list` reply landed in the cache. The main loop
    /// refreshes the palette items for this extension.
    /// Fields are part of the wire protocol; the handler uses
    /// `host.command_items()` for aggregation so the fields are
    /// informational / future-use.
    #[allow(dead_code)]
    CommandsListUpdated {
        ext: String,
        commands: Vec<ExtCommand>,
    },
    /// An `invoke_reply` completed. `ok` mirrors the extension's own
    /// flag; `message` is a short status string the main loop can
    /// flash.
    InvokeReply {
        ext: String,
        req: u64,
        ok: bool,
        message: String,
    },
    /// The `invoke` request timed out (2 s). The extension's commands
    /// are dropped from the palette.
    InvokeTimeout { ext: String },
}

/// The lifecycle state of one extension slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    Running,
    Restarting,
    Dead,
    Skipped,
}

/// The content of the statusline row (ui-extension-plan stage 1
/// layout: the row reserves one line when a status extension exists).
#[derive(Debug, Clone, PartialEq)]
pub enum StatusRow {
    /// The last valid `status` reply of the status owner.
    Lines(Vec<ExtLine>),
    /// The restart budget ran out; the row shows a hint.
    DeadHint(String),
    /// No status extension to show: the built-in help/status row.
    Builtin,
}

/// The input-area frame spec of a `frame` extension reply
/// (docs/ui-extensions design: the input area is customizable by an
/// external extension, not hardwired into the TUI).
///
/// The host composes the spec with the draft content: the extension
/// owns the *frame* (border style, the label, the height, the label
/// style), never the input state — an extension cannot type into the
/// draft, the same trust boundary as `append`
/// (docs/ui-extension.md section 10).
#[derive(Debug, Clone, PartialEq)]
pub struct FrameSpec {
    /// The border style; `None` uses the host's rounded default.
    pub border: Option<FrameBorderStyle>,
    /// The title label, as a styled line. `None` shows the host's
    /// built-in label (the editor mode).
    pub label: Option<(Vec<ExtLine>, Style)>,
    /// The interior height in rows; `None` uses the host default
    /// (two draft lines).
    pub height: Option<usize>,
}

/// The border shapes a frame reply may ask for (the host renders
/// them with `ratatui::widgets::Block::border_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameBorderStyle {
    Rounded,
    Plain,
    Double,
    Thick,
}

impl FrameBorderStyle {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "rounded" => FrameBorderStyle::Rounded,
            "plain" | "single" => FrameBorderStyle::Plain,
            "double" => FrameBorderStyle::Double,
            "thick" => FrameBorderStyle::Thick,
            _ => return None,
        })
    }
}

/// The payload of one `tick` op (docs/ui-extension.md section 4).
#[derive(Debug)]
pub struct TickPayload<'a> {
    pub width: usize,
    pub session: Option<&'a str>,
    pub model: Option<&'a str>,
    pub loop_running: bool,
    /// The active model's thinking level (the `model_thinking`
    /// ext_status value). Frame extensions recolor the border to
    /// correlate with it.
    pub thinking: u32,
    /// Latest `ext_status` values, id to value. The statusline
    /// consumes shared UI state through this map.
    pub statuses: &'a HashMap<String, Value>,
}

struct TickClock {
    next_at: Instant,
    seq: u64,
}

/// One transform request in flight (docs/ui-extension.md section 4:
/// replies carry a `req` id; a reply whose req is no longer current
/// is dropped).
#[derive(Debug, Clone)]
struct TReq {
    owner: usize,
    scope: String,
    text: String,
    /// The width the request was sent at. Kept for logs and for the
    /// stale-reply check; not read by the render path in Stage 1.
    #[allow(dead_code)]
    width: usize,
    sent_at: Instant,
    state: TState,
    lines: Option<Vec<ExtLine>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TState {
    /// Waiting for a `transformed` reply.
    Pending,
    /// A reply landed; the result lines replace the raw span.
    Done,
    /// The 2 s timeout passed; the raw block shows instead.
    Stale,
    /// A resize superseded the request; late replies drop.
    Replaced,
}

struct TransformRegistry {
    next: u64,
    reqs: HashMap<u64, TReq>,
    /// One request per extracted span, keyed by (event log index,
    /// span index within the event). The key is stable across
    /// transcript rebuilds; the value is the current request id.
    /// A resize re-requests every live request and remaps these keys
    /// to the new ids (ui-extension-plan stage 3).
    span_reqs: HashMap<(u64, u32), u64>,
}

impl TransformRegistry {
    fn new() -> Self {
        TransformRegistry {
            next: 1,
            reqs: HashMap::new(),
            span_reqs: HashMap::new(),
        }
    }

    fn clear(&mut self) {
        self.reqs.clear();
        self.span_reqs.clear();
    }
}

/// In-flight `invoke` requests (docs/tui-command-palette.md §10).
/// One in-flight `invoke` request.
#[derive(Debug, Clone)]
struct InvokeReq {
    owner: usize,
    sent_at: Instant,
    state: InvokeState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvokeState {
    Pending,
    Done,
    Stale,
}

/// Mirrors the transform pattern: request id, 2 s timeout, G5
/// fallback. A dead or stale extension is dropped from the list and
/// the host flashes the reason.
#[derive(Debug, Clone)]
struct InvokeRegistry {
    next: u64,
    reqs: HashMap<u64, InvokeReq>,
}

impl InvokeRegistry {
    fn new() -> Self {
        InvokeRegistry {
            next: 1,
            reqs: HashMap::new(),
        }
    }

    fn clear(&mut self) {
        self.reqs.clear();
    }
}

/// Per-extension shared state. The monitor thread owns the child
/// wait; the writer thread owns the stdin writes; the reader thread
/// owns the stdout pump.
struct SlotShared {
    pub name: String,
    pub manifest: Manifest,
    pub pid: AtomicI32,
    pub state: Mutex<SlotState>,
    /// The current generation's stdin. `None` between generations.
    pub stdin: Mutex<Option<BufWriter<std::fs::File>>>,
    /// Ops queue to the writer thread. A full queue drops the op:
    /// the host never blocks on a stuck extension.
    pub send_tx: mpsc::SyncSender<String>,
    pub send_rx: Mutex<Option<mpsc::Receiver<String>>>,
    /// Valid `lines` replies, keyed by event id. FIFO-bounded by
    /// [`LINES_CACHE_CAP`]: overflow evicts the oldest entry so a
    /// live in-progress stream (the newest entry) is never wiped.
    pub lines_cache: Mutex<LinesCache>,
    /// The last valid `status` reply (G5: a bad reply keeps it).
    pub last_status: Mutex<Option<Vec<ExtLine>>>,
    /// The last valid `frame` reply (G5: a bad reply keeps it).
    pub last_frame: Mutex<Option<FrameSpec>>,
    /// The last valid `row_spec` reply (G5: a bad reply keeps it).
    /// `None` when no `row` extension has replied yet: the host
    /// shows no row.
    pub last_row: Mutex<Option<Vec<ExtLine>>>,
    /// When the last valid `status` reply landed (or `None` since
    /// the generation started). The staleness check reads it.
    pub last_status_reply: Mutex<Option<Instant>>,
    /// When this generation started. A status extension that has
    /// not replied by `gen_started + 3 x tick_ms` is stale.
    pub gen_started: Mutex<Instant>,
    /// Set by [`ExtHost::poll_status`] when the status replies stop
    /// for the bound (ui-extension-plan stage 4 open items).
    pub status_stale: AtomicBool,
    /// Set when the generation is dead, before the cache clear.
    /// The reader checks it while holding the `lines_cache` lock,
    /// so a reply that arrives between the dead mark and the cache
    /// clear cannot re-populate the cleared cache (the check and the
    /// insert share one lock scope; no lock nesting, no deadlock
    /// with [`mark_dead`]).
    pub dead: AtomicBool,
    pub tick: Mutex<TickClock>,
    /// The `frame` op's own deadline. A frame owner may also declare
    /// the `status` cap; one shared clock would let `pump_ticks`
    /// re-arm the deadline every cycle, so `pump_frame` always sees a
    /// future deadline and the `frame` op is never sent.
    pub frame_tick: Mutex<TickClock>,
    /// The `row` op's own deadline. A row owner may also declare the
    /// `status` or `frame` cap; one shared clock would let the
    /// competing pump re-arm the deadline every cycle, so
    /// `pump_row` always sees a future deadline and the `row` op is
    /// never sent.
    pub row_tick: Mutex<TickClock>,
}

struct HostInner {
    slots: Vec<Arc<SlotShared>>,
    transform: Mutex<TransformRegistry>,
    /// Bumped when a reply changes what the transcript shows. The
    /// transcript cache key folds this version in.
    replies_version: AtomicU64,
    out_tx: mpsc::SyncSender<ExtItem>,
    /// Set by [`ExtHost::start`] from the host-level value.
    transform_timeout: Mutex<Duration>,
    /// The terminal color capability the cached extension styles are
    /// lowered to on storage (truecolor by default; 256/16 when the
    /// terminal is less capable or `[tui] color` forces it). Named
    /// ANSI colors pass through unchanged, so this only reshapes
    /// free-form RGB from extension hex colors.
    color_level: crate::color::Level,
    /// Per-slot cached `commands_list` replies, keyed by slot index.
    commands_cache: Mutex<HashMap<usize, (Instant, Vec<ExtCommand>)>>,
    /// One in-flight `invoke` request. The `req` id is the key; the
    /// value tracks the owner slot and when it was sent.
    invoke: Mutex<InvokeRegistry>,
    /// Timeout for `invoke` requests (same as transform timeout).
    invoke_timeout: Mutex<Duration>,
}

/// The extension host: one supervised process per enabled extension
/// (docs/ui-extension.md section 7). The TUI main loop drives ticks,
/// transforms, and resize re-requests, and drains the item outbox.
pub struct ExtHost {
    inner: Arc<HostInner>,
    disc: Discovery,
    config_path: PathBuf,
    out_rx: mpsc::Receiver<ExtItem>,
    stop_flag: Arc<AtomicBool>,
    /// Backoff between restart attempts. The spec values are 1 s /
    /// 2 s / 4 s; tests shorten them.
    pub restart_delays: [Duration; 3],
}

impl ExtHost {
    pub fn new(disc: &Discovery, cfg: &TuiConfig) -> Self {
        let slots: Vec<Arc<SlotShared>> = disc
            .exts
            .iter()
            .map(|le| {
                let (send_tx, send_rx) = mpsc::sync_channel::<String>(256);
                let manifest = le.manifest.clone();
                let name = manifest.name.clone();
                Arc::new(SlotShared {
                    name,
                    manifest,
                    pid: AtomicI32::new(-1),
                    state: Mutex::new(SlotState::Running),
                    stdin: Mutex::new(None),
                    send_tx,
                    send_rx: Mutex::new(Some(send_rx)),
                    lines_cache: Mutex::new(LinesCache::new()),
                    last_status: Mutex::new(None),
                    last_frame: Mutex::new(None),
                    last_row: Mutex::new(None),
                    last_status_reply: Mutex::new(None),
                    gen_started: Mutex::new(Instant::now()),
                    status_stale: AtomicBool::new(false),
                    dead: AtomicBool::new(false),
                    tick: Mutex::new(TickClock {
                        // The first tick fires on the first pump, so a
                        // status row shows as soon as the process is up.
                        next_at: Instant::now(),
                        seq: 0,
                    }),
                    frame_tick: Mutex::new(TickClock {
                        next_at: Instant::now(),
                        seq: 0,
                    }),
                    row_tick: Mutex::new(TickClock {
                        next_at: Instant::now(),
                        seq: 0,
                    }),
                })
            })
            .collect();
        let (out_tx, out_rx) = mpsc::sync_channel::<ExtItem>(1024);
        ExtHost {
            inner: Arc::new(HostInner {
                slots,
                transform: Mutex::new(TransformRegistry::new()),
                replies_version: AtomicU64::new(0),
                out_tx,
                transform_timeout: Mutex::new(TRANSFORM_TIMEOUT),
                color_level: cfg.color.unwrap_or_else(crate::color::Level::detect),
                commands_cache: Mutex::new(HashMap::new()),
                invoke: Mutex::new(InvokeRegistry::new()),
                invoke_timeout: Mutex::new(TRANSFORM_TIMEOUT),
            }),
            disc: disc.clone(),
            config_path: cfg.config_path.clone(),
            out_rx,
            stop_flag: Arc::new(AtomicBool::new(false)),
            restart_delays: RESTART_DELAYS,
        }
    }

    /// Test seam: shorten the restart budget before [`ExtHost::start`].
    #[allow(dead_code)]
    pub fn set_restart_delays(&mut self, delays: [Duration; 3]) {
        self.restart_delays = delays;
    }

    /// Test seam: shorten the transform timeout before [`ExtHost::start`].
    #[allow(dead_code)]
    pub fn set_transform_timeout(&mut self, timeout: Duration) {
        *self.inner.transform_timeout.lock().unwrap() = timeout;
    }

    /// Spawn every enabled extension and start its threads. Returns
    /// the items for extensions the host could not start (protocol
    /// mismatch or a spawn failure); the main loop flashes them.
    pub fn start(&self) -> Vec<ExtItem> {
        let mut items = Vec::new();
        for (i, le) in self.disc.exts.iter().enumerate() {
            let m = le.manifest.clone();
            let slot = self.inner.slots[i].clone();
            if !m.protocol_ok {
                *slot.state.lock().unwrap() = SlotState::Skipped;
                items.push(ExtItem::Skipped {
                    ext: m.name.clone(),
                    reason: "protocol_v missing or not 1; the host speaks 1".to_string(),
                });
                continue;
            }
            // The writer thread keeps a stuck extension from
            // blocking the main loop; it is persistent across
            // restart generations.
            {
                let wslot = slot.clone();
                std::thread::Builder::new()
                    .name(format!("tui-ext-write-{}", m.name))
                    .spawn(move || writer_thread(wslot))
                    .ok();
            }
            match spawn_gen(&slot, &m, &self.config_path) {
                Ok(gen) => {
                    ext_log(&format!(
                        "spawn {} gen=0 pid={}",
                        m.name, gen.0.pid
                    ));
                    *slot.state.lock().unwrap() = SlotState::Running;
                    // A fresh generation: the staleness clock and
                    // flag reset with it.
                    *slot.gen_started.lock().unwrap() = Instant::now();
                    *slot.last_status_reply.lock().unwrap() = None;
                    slot.status_stale.store(false, Ordering::SeqCst);
                    let mon_slot = slot.clone();
                    let mon_inner = self.inner.clone();
                    let stop = self.stop_flag.clone();
                    let delays = self.restart_delays;
                    let m2 = m.clone();
                    let cfg_path = self.config_path.clone();
                    std::thread::Builder::new()
                        .name(format!("tui-ext-mon-{}", m.name))
                        .spawn(move || {
                            monitor_thread()
                                .slot(mon_slot)
                                .inner(mon_inner)
                                .stop(stop)
                                .delays(delays)
                                .manifest(m2)
                                .config_path(cfg_path)
                                .idx(i)
                                .first(gen)
                                .call()
                        })
                        .ok();
                }
                Err(e) => {
                    ext_log(&format!("spawn {} gen=0 failed: {}", m.name, e));
                    *slot.state.lock().unwrap() = SlotState::Skipped;
                    items.push(ExtItem::Skipped {
                        ext: m.name.clone(),
                        reason: format!("command failed to start: {e}"),
                    });
                }
            }
        }
        items
    }

    /// The next host item, if one is queued. The main loop drains
    /// these between frames.
    pub fn drain(&self) -> Option<ExtItem> {
        self.out_rx.try_recv().ok()
    }

    /// Forward one new log event to the extensions whose `kinds`
    /// list matches (an empty list means all).
    pub fn forward_event(&self, id: u64, e: &Event) {
        let Some(obj) = e.obj() else {
            return;
        };
        let ty = e.type_name();
        for (i, s) in self.inner.slots.iter().enumerate() {
            if *s.state.lock().unwrap() == SlotState::Skipped {
                continue;
            }
            let m = &s.manifest;
            if !m.kinds.is_empty() {
                let t = match ty {
                    Some(t) => t,
                    None => continue,
                };
                if !m.kinds.iter().any(|k| k == t) {
                    continue;
                }
            }
            self.send_op(i, &json!({ "v": 1, "op": "event", "id": id, "event": obj }));
        }
    }

    /// Resend history at start (docs/ui-extension.md section 4
    /// history rule). Render-capable extensions see the visible
    /// transcript for their kinds. Status extensions see every
    /// `assistant_message` that carries `usage`, uncapped, so
    /// cumulative stats survive a restart from the log alone.
    pub fn send_history(&self, events: &[Event]) {
        let cap = crate::render::TRANSCRIPT_EVENT_CAP;
        let base = events.len().saturating_sub(cap);
        for (i, s) in self.inner.slots.iter().enumerate() {
            if *s.state.lock().unwrap() == SlotState::Skipped {
                continue;
            }
            let m = s.manifest.clone();
            if m.caps.iter().any(|c| c == "status") {
                // The re-send set (docs/auto-compact-plan.md
                // section 4.6): every usage-bearing assistant
                // message, plus the compaction markers that carry
                // the summary call's usage. The summary usage joins
                // the cumulative totals; the marker without a usage
                // object is not resent.
                for (gi, e) in events.iter().enumerate() {
                    let is_usage_msg =
                        e.kind() == EventKind::AssistantMessage && e.get("usage").is_some();
                    let is_compaction =
                        e.kind() == EventKind::CompactionSummary && e.get("usage").is_some();
                    if is_usage_msg || is_compaction {
                        if let Some(obj) = e.obj() {
                            self.send_op(
                                i,
                                &json!({ "v": 1, "op": "event", "id": gi as u64, "event": obj }),
                            );
                        }
                    }
                }
            } else {
                for (offset, e) in events[base..].iter().enumerate() {
                    let gi = base + offset;
                    let ty = e.type_name();
                    if !m.kinds.is_empty() {
                        let t = match ty {
                            Some(t) => t,
                            None => continue,
                        };
                        if !m.kinds.iter().any(|k| k == t) {
                            continue;
                        }
                    }
                    if let Some(obj) = e.obj() {
                        self.send_op(
                            i,
                            &json!({ "v": 1, "op": "event", "id": gi as u64, "event": obj }),
                        );
                    }
                }
            }
        }
    }

    /// Session switch: event ids restart per session, so the reply
    /// caches and transform requests reset too.
    pub fn clear_replies(&self) {
        for s in &self.inner.slots {
            s.lines_cache.lock().unwrap().clear();
        }
        self.inner.transform.lock().unwrap().clear();
        self.inner.commands_cache.lock().unwrap().clear();
        self.inner.invoke.lock().unwrap().clear();
        self.inner.replies_version.fetch_add(1, Ordering::SeqCst);
    }

    /// Send due ticks to the status extensions. The main loop calls
    /// this every frame; the host tracks each extension's deadline.
    pub fn pump_ticks(&self, p: &TickPayload) {
        let now = Instant::now();
        for (i, s) in self.inner.slots.iter().enumerate() {
            if !s.manifest.caps.iter().any(|c| c == "status") {
                continue;
            }
            if *s.state.lock().unwrap() == SlotState::Skipped {
                continue;
            }
            let seq = {
                let mut t = s.tick.lock().unwrap();
                if now < t.next_at {
                    continue;
                }
                t.seq += 1;
                t.next_at = now + Duration::from_millis(s.manifest.tick_ms);
                t.seq
            };
            let mut obj = json!({
                "v": 1,
                "op": "tick",
                "seq": seq,
                "width": p.width,
                "thinking": p.thinking,
                "loop_running": p.loop_running,
                "color": self.inner.color_level.name(),
                "statuses": Value::Object(Map::from_iter(
                    p.statuses.iter().map(|(k, v)| (k.clone(), v.clone()))
                )),
            });
            if let Some(s) = p.session {
                obj["session"] = json!(s);
            }
            if let Some(m) = p.model {
                obj["model"] = json!(m);
            }
            self.send_op(i, &obj);
        }
    }

    /// Send a `frame` op to the frame owner on its tick cadence. The
    /// frame extension replies `frame_spec` to customize the input
    /// area (border, label, height). A missing or dead owner sends
    /// nothing; the built-in frame shows.
    pub fn pump_frame(&self, p: &TickPayload, mode: &str) {
        let Some(i) = self.disc.frame_owner else {
            return;
        };
        let s = &self.inner.slots[i];
        if *s.state.lock().unwrap() == SlotState::Skipped {
            return;
        }
        let now = Instant::now();
        let seq = {
            let mut t = s.frame_tick.lock().unwrap();
            if now < t.next_at {
                return;
            }
            t.seq += 1;
            t.next_at = now + Duration::from_millis(s.manifest.tick_ms);
            t.seq
        };
        let mut obj = json!({
            "v": 1,
            "op": "frame",
            "seq": seq,
            "width": p.width,
            "thinking": p.thinking,
            "mode": mode,
            "loop_running": p.loop_running,
        });
        if let Some(ses) = p.session {
            obj["session"] = json!(ses);
        }
        if let Some(m) = p.model {
            obj["model"] = json!(m);
        }
        self.send_op(i, &obj);
    }

    /// Send a `row` op to the row owner on its tick cadence. The row
    /// extension replies `row_spec` with the content of the
    /// host-reserved row above the input box (between the working
    /// row and the input area): the goal extension uses it for the
    /// goal status line and the armed hint. A missing or dead owner
    /// sends nothing; the row shows nothing (the bare TUI has no
    /// row, docs/ui-extension.md section 4).
    pub fn pump_row(&self, p: &TickPayload, mode: &str) {
        let Some(i) = self.disc.row_owner else {
            return;
        };
        let s = &self.inner.slots[i];
        if *s.state.lock().unwrap() == SlotState::Skipped {
            return;
        }
        let now = Instant::now();
        let seq = {
            let mut t = s.row_tick.lock().unwrap();
            if now < t.next_at {
                return;
            }
            t.seq += 1;
            t.next_at = now + Duration::from_millis(s.manifest.tick_ms);
            t.seq
        };
        let mut obj = json!({
            "v": 1,
            "op": "row",
            "seq": seq,
            "width": p.width,
            "thinking": p.thinking,
            "mode": mode,
            "loop_running": p.loop_running,
        });
        if let Some(ses) = p.session {
            obj["session"] = json!(ses);
        }
        if let Some(m) = p.model {
            obj["model"] = json!(m);
        }
        self.send_op(i, &obj);
    }

    /// Mark a status extension stale: no valid `status` reply for
    /// three tick intervals (3 x tick_ms) drops the row to a stale
    /// hint. A valid reply or a restart clears it. The first reply
    /// of a generation gets the wider initial grace: a cold start
    /// (a git spawn on a cold cache) is not a stuck extension. The
    /// bound is the status staleness of docs/ui-extension.md
    /// section 11 (ui-extension-plan stage 4).
    pub fn poll_status(&self) {
        let now = Instant::now();
        for s in &self.inner.slots {
            if !s.manifest.caps.iter().any(|c| c == "status") {
                continue;
            }
            if !matches!(
                *s.state.lock().unwrap(),
                SlotState::Running | SlotState::Restarting
            ) {
                continue;
            }
            let has_reply = s.last_status_reply.lock().unwrap().is_some();
            let since = {
                let l = *s.last_status_reply.lock().unwrap();
                l.unwrap_or_else(|| *s.gen_started.lock().unwrap())
            };
            let steady = Duration::from_millis(s.manifest.tick_ms.saturating_mul(3));
            let bound = if has_reply {
                steady
            } else {
                steady.max(INITIAL_STATUS_GRACE)
            };
            if now.duration_since(since) > bound {
                s.status_stale.store(true, Ordering::SeqCst);
            }
        }
    }

    /// Expire pending transform requests that hit the 2 s timeout.
    /// The fallback is the raw block.
    pub fn poll_transforms(&self) {
        let mut reg = self.inner.transform.lock().unwrap();
        let timeout = *self.inner.transform_timeout.lock().unwrap();
        let now = Instant::now();
        for r in reg.reqs.values_mut() {
            if matches!(r.state, TState::Pending) && now.duration_since(r.sent_at) > timeout {
                r.state = TState::Stale;
            }
        }
    }

    /// Send a `commands` op to every extension that declared the
    /// `commands` cap (docs/tui-command-palette.md section 10).
    /// The extension replies with a `commands_list` that the host
    /// caches and exposes via [`ExtHost::command_items`].
    pub fn request_commands(&self, session: Option<&str>, loop_running: bool) {
        for (i, s) in self.inner.slots.iter().enumerate() {
            if !s.manifest.caps.iter().any(|c| c == "commands") {
                continue;
            }
            if *s.state.lock().unwrap() == SlotState::Skipped {
                continue;
            }
            let mut obj = json!({
                "v": 1,
                "op": "commands",
                "loop_running": loop_running,
            });
            if let Some(ses) = session {
                obj["session"] = json!(ses);
            }
            self.send_op(i, &obj);
            // Track the pending request so poll_commands can time out.
            self.inner
                .commands_cache
                .lock()
                .unwrap()
                .insert(i, (Instant::now(), Vec::new()));
        }
    }

    /// Send an `invoke` op to the extension that owns the given
    /// command. Returns the request id, or `None` when no extension
    /// is alive.
    pub fn request_invoke(&self, ext: &str, id: &str, value: Option<&str>) -> Option<u64> {
        let i = self.disc.index_by_name.get(ext)?;
        let s = &self.inner.slots[*i];
        if !matches!(
            *s.state.lock().unwrap(),
            SlotState::Running | SlotState::Restarting
        ) {
            return None;
        }
        let mut reg = self.inner.invoke.lock().unwrap();
        let req = reg.next;
        reg.next += 1;
        reg.reqs.insert(
            req,
            InvokeReq {
                owner: *i,
                sent_at: Instant::now(),
                state: InvokeState::Pending,
            },
        );
        drop(reg);
        let mut obj = json!({
            "v": 1,
            "op": "invoke",
            "req": req,
            "id": id,
        });
        if let Some(v) = value {
            obj["value"] = json!(v);
        }
        self.send_op(*i, &obj);
        Some(req)
    }

    /// Expire pending `invoke` requests that hit the 2 s timeout.
    /// For each expired request the extension's commands are dropped
    /// (G5 fallback).
    pub fn poll_invokes(&self) {
        let mut reg = self.inner.invoke.lock().unwrap();
        let timeout = *self.inner.invoke_timeout.lock().unwrap();
        let now = Instant::now();
        let stale_owners: Vec<usize> = reg
            .reqs
            .values()
            .filter(|r| matches!(r.state, InvokeState::Pending))
            .filter(|r| now.duration_since(r.sent_at) > timeout)
            .map(|r| r.owner)
            .collect();
        for r in reg.reqs.values_mut() {
            if matches!(r.state, InvokeState::Pending)
                && now.duration_since(r.sent_at) > timeout
            {
                r.state = InvokeState::Stale;
            }
        }
        drop(reg);
        for owner in stale_owners {
            let name = self.inner.slots.get(owner).map(|s| s.name.clone());
            self.drop_commands_for(owner);
            if let Some(name) = name {
                let _ = self.inner.out_tx.try_send(ExtItem::InvokeTimeout {
                    ext: name,
                });
            }
        }
    }

    /// Remove the cached commands and in-flight invokes for one slot
    /// (used by [`mark_dead`] and by `poll_invokes` on timeout).
    fn drop_commands_for(&self, idx: usize) {
        self.inner
            .commands_cache
            .lock()
            .unwrap()
            .remove(&idx);
        {
            let mut reg = self.inner.invoke.lock().unwrap();
            for r in reg.reqs.values_mut() {
                if r.owner == idx {
                    r.state = InvokeState::Stale;
                }
            }
        }
    }

    /// Collect the cached `commands_list` payloads from every live
    /// extension. Extensions that have not replied yet, or whose
    /// commands have been dropped, contribute nothing.
    pub fn command_items(&self) -> Vec<crate::palette::items::PaletteItem> {
        let cache = self.inner.commands_cache.lock().unwrap();
        let mut out: Vec<crate::palette::items::PaletteItem> = Vec::new();
        for i in cache.keys() {
            let s = &self.inner.slots[*i];
            if !matches!(
                *s.state.lock().unwrap(),
                SlotState::Running | SlotState::Restarting
            ) {
                continue;
            }
            let cmds = cache.get(i).map(|(_, c)| c.clone()).unwrap_or_default();
            out.extend(crate::palette::items::from_extension(
                &s.name, &cmds,
            ));
        }
        out
    }


    /// A resize re-requests every transform block: one new request
    /// per block, and the superseded request id stops matching. The
    /// span index map remaps to the new request ids, so the next
    /// render rebuild asks for nothing and waits for the new replies
    /// (ui-extension-plan stage 3 acceptance: one re-transform per
    /// block on a resize).
    /// Re-send every transform request at the new width. A resize
    /// that supersedes a request remaps its spans, so each span
    /// re-transforms once per resize. Timed-out requests are
    /// re-sent too: bounded waste, no display effect, because the
    /// re-send is what the spans point at after the remap.
    pub fn on_resize(&self, width: usize) {
        let mut reg = self.inner.transform.lock().unwrap();
        let resends: Vec<(u64, TReq)> = reg
            .reqs
            .iter()
            .filter(|(_, r)| !matches!(r.state, TState::Replaced))
            .map(|(id, r)| (*id, r.clone()))
            .collect();
        for (old, r) in resends {
            if let Some(r2) = reg.reqs.get_mut(&old) {
                r2.state = TState::Replaced;
            }
            let id = reg.next;
            reg.next += 1;
            reg.reqs.insert(
                id,
                TReq {
                    owner: r.owner,
                    scope: r.scope.clone(),
                    text: r.text.clone(),
                    width,
                    sent_at: Instant::now(),
                    state: TState::Pending,
                    lines: None,
                },
            );
            // Remap the spans that pointed at the superseded request.
            for req in reg.span_reqs.values_mut() {
                if *req == old {
                    *req = id;
                }
            }
            self.send_op(
                r.owner,
                &json!({
                    "v": 1,
                    "op": "transform",
                    "req": id,
                    "text": r.text,
                    "width": width,
                    "scope": r.scope,
                }),
            );
        }
    }

    /// Send one transform request for a span of `scope`, deduplicated
    /// per (event log index, span index). The first call for a span
    /// sends the request; a live or finished request for the same span
    /// reuses its id and sends nothing (a resend would bump the reply
    /// version on every rebuild: an infinite rebuild loop). A stale
    /// request re-requests. `None` when no extension declares the
    /// target: the raw block stays.
    pub fn request_span(
        &self,
        event_id: u64,
        span_idx: u32,
        scope: &str,
        text: &str,
        width: usize,
    ) -> Option<u64> {
        let owner = *self.disc.transform_owners.get(scope)?;
        let key = (event_id, span_idx);
        let mut reg = self.inner.transform.lock().unwrap();
        if let Some(&old) = reg.span_reqs.get(&key) {
            if let Some(r) = reg.reqs.get(&old) {
                if matches!(r.state, TState::Pending | TState::Done) {
                    return Some(old);
                }
            }
            reg.span_reqs.remove(&key);
        }
        let id = reg.next;
        reg.next += 1;
        reg.span_reqs.insert(key, id);
        reg.reqs.insert(
            id,
            TReq {
                owner,
                scope: scope.to_string(),
                text: text.to_string(),
                width,
                sent_at: Instant::now(),
                state: TState::Pending,
                lines: None,
            },
        );
        drop(reg);
        self.send_op(
            owner,
            &json!({
                "v": 1,
                "op": "transform",
                "req": id,
                "text": text,
                "width": width,
                "scope": scope,
            }),
        );
        Some(id)
    }

    /// The finished result for one extracted span, for in-place
    /// replacement at render time. `None` while the request is in
    /// flight, after the timeout, or after a resize superseded it:
    /// the raw span shows in the meantime.
    pub fn span_lines(&self, event_id: u64, span_idx: u32) -> Option<Vec<ExtLine>> {
        let reg = self.inner.transform.lock().unwrap();
        let req = *reg.span_reqs.get(&(event_id, span_idx))?;
        match reg.reqs.get(&req) {
            Some(r) if matches!(r.state, TState::Done) => r.lines.clone(),
            _ => None,
        }
    }

    /// Send one transform request for a span of `scope`. Returns the
    /// request id. `None` when no extension declares the target. The
    /// renderer path uses [`ExtHost::request_span`], which dedupes per
    /// span; this plain form re-sends on every call.
    #[allow(dead_code)]
    pub fn request_transform(&self, scope: &str, text: &str, width: usize) -> Option<u64> {
        let owner = *self.disc.transform_owners.get(scope)?;
        let mut reg = self.inner.transform.lock().unwrap();
        let id = reg.next;
        reg.next += 1;
        reg.reqs.insert(
            id,
            TReq {
                owner,
                scope: scope.to_string(),
                text: text.to_string(),
                width,
                sent_at: Instant::now(),
                state: TState::Pending,
                lines: None,
            },
        );
        drop(reg);
        self.send_op(
            owner,
            &json!({
                "v": 1,
                "op": "transform",
                "req": id,
                "text": text,
                "width": width,
                "scope": scope,
            }),
        );
        Some(id)
    }

    /// The finished result of a transform request, for in-place span
    /// replacement at render time. A stale or superseded request
    /// has no result: the raw block shows.
    /// The unit tests read these; the Stage 1 main loop does not.
    #[allow(dead_code)]
    pub fn transform_lines(&self, req: u64) -> Option<Vec<ExtLine>> {
        let reg = self.inner.transform.lock().unwrap();
        match reg.reqs.get(&req) {
            Some(r) if matches!(r.state, TState::Done) => r.lines.clone(),
            _ => None,
        }
    }

    /// The valid `lines` reply for (owner extension, event id), for
    /// in-place replacement at render time. `None` falls back to the
    /// built-in render (per-op G5 fallback).
    pub fn lookup_lines(&self, owner: &str, event_id: u64) -> Option<Vec<ExtLine>> {
        let i = *self.disc.index_by_name.get(owner)?;
        self.inner.slots[i]
            .lines_cache
            .lock()
            .unwrap()
            .get(&event_id)
            .cloned()
    }

    /// The input-area frame spec (the `frame` owner's last valid
    /// `frame_spec` reply, or the built-in rendering when no frame
    /// extension exists or its reply is missing).
    pub fn frame_spec(&self) -> Option<FrameSpec> {
        let i = self.disc.frame_owner?;
        let s = &self.inner.slots[i];
        if !matches!(
            *s.state.lock().unwrap(),
            SlotState::Running | SlotState::Restarting
        ) {
            return None;
        }
        s.last_frame.lock().unwrap().clone()
    }

    /// The host-reserved row content above the input box (the `row`
    /// owner's last valid `row_spec` reply, docs/ui-extension.md
    /// section 4). `None` when no row extension is installed, when
    /// its reply is still missing, or when it died: the row is
    /// transient content, so a dead or absent owner clears it —
    /// unlike `status`, which keeps its last valid row with a dead
    /// hint. An empty `lines` array is a live "no row" from the
    /// owner (the goal extension hides the row when no goal is open
    /// and nothing is armed).
    pub fn row_spec(&self) -> Option<Vec<ExtLine>> {
        let i = self.disc.row_owner?;
        let s = &self.inner.slots[i];
        if !matches!(
            *s.state.lock().unwrap(),
            SlotState::Running | SlotState::Restarting
        ) {
            return None;
        }
        s.last_row.lock().unwrap().clone()
    }

    /// The statusline row content (docs/ui-extension-plan stage 1
    /// layout).
    pub fn status_row(&self) -> StatusRow {
        let Some(i) = self.disc.status_owner else {
            return StatusRow::Builtin;
        };
        let s = &self.inner.slots[i];
        match *s.state.lock().unwrap() {
            SlotState::Dead => StatusRow::DeadHint(format!("ext {} dead after 3 restarts", s.name)),
            SlotState::Skipped => StatusRow::Builtin,
            SlotState::Running | SlotState::Restarting => {
                if s.status_stale.load(Ordering::SeqCst) {
                    return StatusRow::DeadHint(format!(
                        "ext {} status stale: no reply for 3 ticks",
                        s.name
                    ));
                }
                match s.last_status.lock().unwrap().clone() {
                    Some(l) => StatusRow::Lines(l),
                    None => StatusRow::Builtin,
                }
            }
        }
    }

    /// The extension that owns an event kind, if any (first in the
    /// composed sequence that lists the kind).
    pub fn owner_for_kind(&self, kind: EventKind) -> Option<&str> {
        self.disc
            .kind_owners
            .get(&kind)
            .map(|&i| self.disc.owner_name(i))
    }

    /// The unit tests read these; the Stage 1 main loop does not.
    #[allow(dead_code)]
    pub fn kind_owners(&self) -> &HashMap<EventKind, usize> {
        &self.disc.kind_owners
    }

    /// The transform owner of a target scope, if any.
    /// The Stage 3 span extraction reads this; not read in Stage 1.
    #[allow(dead_code)]
    pub fn transform_owner(&self, scope: &str) -> Option<&str> {
        self.disc
            .transform_owners
            .get(scope)
            .map(|&i| self.disc.owner_name(i))
    }

    /// The transcript cache key folds this version in, so a new
    /// extension reply rebuilds the transcript (ui-extension-plan
    /// stage 1: the cache folds in extension replies).
    pub fn replies_version(&self) -> u64 {
        self.inner.replies_version.load(Ordering::SeqCst)
    }

    /// The extension names, in composed order.
    /// The Stage 1 main loop does not read them; the flash names come
    /// from the items themselves.
    #[allow(dead_code)]
    pub fn ext_names(&self) -> Vec<String> {
        self.disc
            .exts
            .iter()
            .map(|e| e.manifest.name.clone())
            .collect()
    }

    /// Quit path: SIGTERM every extension process group, wait the
    /// grace window, SIGKILL the survivors, and wait for the
    /// deaths, like the loop stop. The stop flag keeps the monitors
    /// from restarting anything. The escalation is synchronous,
    /// not a detached thread: a detached thread dies with the
    /// process, and a group that ignores SIGTERM would orphan
    /// (docs/tui.md section 13.3). After the escalation, the pids
    /// are re-collected: a monitor that passed its stop check just
    /// before the flag was set can still start a new generation, and
    /// that new group would otherwise survive (docs/ui-extension.md
    /// section 7).
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        let mut pids: Vec<i32> = Vec::new();
        for s in &self.inner.slots {
            let pid = s.pid.load(Ordering::SeqCst);
            if pid > 0 {
                pids.push(pid);
                unsafe {
                    libc::kill(-pid, libc::SIGTERM);
                }
            }
        }
        // The grace: most extensions die on SIGTERM; the SIGKILL
        // escalation covers the rest.
        let deadline = Instant::now() + STOP_GRACE;
        while !pids.is_empty() {
            pids.retain(|&pid| group_alive(pid));
            if pids.is_empty() {
                break;
            }
            if Instant::now() >= deadline {
                for pid in &pids {
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // A generation that started during the wait is a new group
        // not in the list above. The monitor cannot start another
        // one: its loop checks the stop flag before each spawn. Let
        // an in-flight spawn store its pid, collect again, and kill
        // the new pids. The collection loops for a bounded window:
        // a spawn can land just after one pass, and the monitor's
        // post-spawn check kills its own racy generation as a backstop.
        let mut empty_rounds: u32 = 0;
        for _ in 0..5 {
            std::thread::sleep(Duration::from_millis(300));
            let known: std::collections::HashSet<i32> = pids.iter().copied().collect();
            let mut fresh: Vec<i32> = Vec::new();
            for s in &self.inner.slots {
                let pid = s.pid.load(Ordering::SeqCst);
                if pid > 0 && !known.contains(&pid) {
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                    fresh.push(pid);
                }
            }
            pids.extend(fresh.clone());
            if fresh.is_empty() {
                empty_rounds += 1;
                if empty_rounds >= 2 {
                    break;
                }
            } else {
                empty_rounds = 0;
            }
        }
    }

    /// One `ext.toml`-driven extension's shared slot, for tests.
    #[allow(dead_code)]
    pub fn slot_state(&self, idx: usize) -> SlotState {
        *self.inner.slots[idx].state.lock().unwrap()
    }

    /// Test seam: feed one reply line as the stdout pump would.
    #[doc(hidden)]
    #[allow(dead_code)]
    pub fn reply_line(&self, idx: usize, line: &str) {
        self.inner.handle_reply(idx, line);
    }

    fn send_op(&self, idx: usize, value: &Value) {
        let line = value.to_string();
        let _ = self.inner.slots[idx].send_tx.try_send(line);
    }
}

impl Drop for ExtHost {
    /// Kill the extension groups when the host leaves scope, so a
    /// dropped host (a failed test, a panic between start and stop)
    /// leaves no orphan extension process behind. `stop` is idempotent:
    /// a second call re-kills dead pids (a no-op) and reaps nothing
    /// new.
    fn drop(&mut self) {
        self.stop();
    }
}

impl HostInner {
    /// Parse one reply line and apply the per-op G5 fallback.
    /// Nothing here can fail the TUI: a bad line is dropped, a bad
    /// `status` keeps the last valid row, a stale `transformed`
    /// drops, a bad `append` rejects with a flash reason.
    fn handle_reply(&self, idx: usize, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            // Malformed JSONL: per-op fallback is "no state change".
            // The built-in render and the last valid row survive.
            Err(_) => return,
        };
        if v.get("v").and_then(|x| x.as_u64()) != Some(1) {
            return;
        }
        let op = match v.get("op").and_then(|o| o.as_str()) {
            Some(o) => o,
            None => return,
        };
        let slot = &self.slots[idx];
        match op {
            "lines" => {
                let Some(id) = v.get("event_id").and_then(|x| x.as_u64()) else {
                    return;
                };
                let Some(lines) =
                    lines_value(&v["lines"]).map(|l| lower_ext_lines(l, self.color_level))
                else {
                    // G5: fall back to the built-in render.
                    return;
                };
                // A dead extension's buffered replies must not
                // re-populate the cache that [`mark_dead`] cleared.
                // The mark check runs while holding the cache lock,
                // so the mark and the insert cannot interleave
                // (a stale render resurfacing after death).
                let mut cache = slot.lines_cache.lock().unwrap();
                if slot.dead.load(Ordering::SeqCst) {
                    return;
                }
                // Bounded FIFO eviction: when full the oldest entry is
                // dropped. The live stream is always the newest entry,
                // so it is never evicted by overflow.
                cache.upsert(id, lines.clone());
                drop(cache);
                self.replies_version.fetch_add(1, Ordering::SeqCst);
                let _ = self.out_tx.try_send(ExtItem::LinesCached {
                    ext: slot.name.clone(),
                });
            }
            "status" => {
                let Some(lines) =
                    lines_value(&v["lines"]).map(|l| lower_ext_lines(l, self.color_level))
                else {
                    // G5: keep the last valid row.
                    return;
                };
                let mut last = slot.last_status.lock().unwrap();
                // Unchanged content: no version bump. The tick reply
                // is usually identical, and a bump would rewrap the
                // whole transcript once per tick. The staleness
                // clock still advances: a live reply keeps the
                // extension fresh even when the row text does not
                // change (a stable row freezing the clock would
                // show the stale hint on a live extension).
                if *last == Some(lines.clone()) {
                    drop(last);
                    *slot.last_status_reply.lock().unwrap() = Some(Instant::now());
                    slot.status_stale.store(false, Ordering::SeqCst);
                    return;
                }
                *last = Some(lines.clone());
                drop(last);
                // A live reply clears the staleness flag and re-
                // starts the staleness clock.
                *slot.last_status_reply.lock().unwrap() = Some(Instant::now());
                slot.status_stale.store(false, Ordering::SeqCst);
                self.replies_version.fetch_add(1, Ordering::SeqCst);
                let _ = self.out_tx.try_send(ExtItem::StatusUpdated {
                    ext: slot.name.clone(),
                });
            }
            "frame_spec" => {
                let Some(spec) = frame_spec_value(&v["spec"]) else {
                    // G5: keep the last valid frame.
                    return;
                };
                let spec = lower_frame_spec(spec, self.color_level);
                *slot.last_frame.lock().unwrap() = Some(spec);
                self.replies_version.fetch_add(1, Ordering::SeqCst);
                let _ = self.out_tx.try_send(ExtItem::FrameUpdated {
                    ext: slot.name.clone(),
                });
            }
            "row_spec" => {
                let Some(lines) =
                    lines_value(&v["lines"]).map(|l| lower_ext_lines(l, self.color_level))
                else {
                    // G5: keep the last valid row.
                    return;
                };
                let mut last = slot.last_row.lock().unwrap();
                // Unchanged content: no version bump. A row reply is
                // usually identical tick to tick, and a bump would
                // rewrap the whole transcript (same rule as `status`).
                if *last == Some(lines.clone()) {
                    return;
                }
                *last = Some(lines);
                drop(last);
                self.replies_version.fetch_add(1, Ordering::SeqCst);
                let _ = self.out_tx.try_send(ExtItem::RowUpdated {
                    ext: slot.name.clone(),
                });
            }
            "transformed" => {
                let Some(req) = v.get("req").and_then(|x| x.as_u64()) else {
                    return;
                };
                let Some(lines) =
                    lines_value(&v["lines"]).map(|l| lower_ext_lines(l, self.color_level))
                else {
                    return;
                };
                let ok = {
                    let mut reg = self.transform.lock().unwrap();
                    let timeout = *self.transform_timeout.lock().unwrap();
                    let now = Instant::now();
                    match reg.reqs.get_mut(&req) {
                        Some(r)
                            if matches!(r.state, TState::Pending)
                                && now.duration_since(r.sent_at) <= timeout =>
                        {
                            r.state = TState::Done;
                            r.lines = Some(lines);
                            true
                        }
                        // A req that is no longer current drops: the
                        // raw block stays.
                        _ => false,
                    }
                };
                if ok {
                    self.replies_version.fetch_add(1, Ordering::SeqCst);
                    let _ = self.out_tx.try_send(ExtItem::TransformedCached { req });
                }
            }
            "append" => {
                let Some(ev) = v.get("event") else {
                    let _ = self.out_tx.try_send(ExtItem::AppendRejected {
                        ext: slot.name.clone(),
                        reason: "missing `event` payload".to_string(),
                    });
                    return;
                };
                if !ev.is_object() {
                    let _ = self.out_tx.try_send(ExtItem::AppendRejected {
                        ext: slot.name.clone(),
                        reason: "`event` is not a JSON object".to_string(),
                    });
                    return;
                }
                let ty = ev.get("type").and_then(|t| t.as_str());
                let m = &slot.manifest;
                if m.append_types.is_empty() {
                    let _ = self.out_tx.try_send(ExtItem::AppendRejected {
                        ext: slot.name.clone(),
                        reason: "the manifest declares no append_types".to_string(),
                    });
                } else {
                    match ty {
                        Some(t) if m.append_types.iter().any(|a| a == t) => {
                            let _ = self.out_tx.try_send(ExtItem::AppendReq {
                                ext: slot.name.clone(),
                                event: ev.clone(),
                            });
                        }
                        Some(t) => {
                            let _ = self.out_tx.try_send(ExtItem::AppendRejected {
                                ext: slot.name.clone(),
                                reason: format!("type `{t}` is not in its append_types"),
                            });
                        }
                        None => {
                            let _ = self.out_tx.try_send(ExtItem::AppendRejected {
                                ext: slot.name.clone(),
                                reason: "the event has no string `type`".to_string(),
                            });
                        }
                    }
                }
            }
            "notify" => match v.get("kind").and_then(|k| k.as_str()) {
                Some("bell") => {
                    let _ = self.out_tx.try_send(ExtItem::NotifyBell {
                        ext: slot.name.clone(),
                    });
                }
                Some("osc") => {
                    let Some(code) = v.get("code").and_then(|c| c.as_u64()) else {
                        return;
                    };
                    let args = v
                        .get("args")
                        .and_then(|a| a.as_str())
                        .unwrap_or("")
                        .to_string();
                    let _ = self.out_tx.try_send(ExtItem::NotifyOsc {
                        ext: slot.name.clone(),
                        code: code.min(u16::MAX as u64) as u16,
                        args,
                    });
                }
                _ => {}
            },
            // ── commands / invoke (docs/tui-command-palette.md §10) ──
            "commands_list" => {
                let cmds = match parse_commands_list(&v) {
                    Some(cmds) => cmds,
                    None => return, // G5: bad payload, keep last valid
                };
                {
                    let mut cache = self.commands_cache.lock().unwrap();
                    cache.insert(idx, (Instant::now(), cmds.clone()));
                }
                let _ = self.out_tx.try_send(ExtItem::CommandsListUpdated {
                    ext: slot.name.clone(),
                    commands: cmds,
                });
            }
            "invoke_reply" => {
                let Some(req) = v.get("req").and_then(|x| x.as_u64()) else {
                    return;
                };
                let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false);
                let message = v
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut reg = self.invoke.lock().unwrap();
                let timeout = *self.invoke_timeout.lock().unwrap();
                let now = Instant::now();
                match reg.reqs.get_mut(&req) {
                    Some(r)
                        if matches!(r.state, InvokeState::Pending)
                            && now.duration_since(r.sent_at) <= timeout =>
                    {
                        r.state = InvokeState::Done;
                        drop(reg);
                        let _ = self.out_tx.try_send(ExtItem::InvokeReply {
                            ext: slot.name.clone(),
                            req,
                            ok,
                            message,
                        });
                    }
                    _ => {}
                }
            }
            // An unknown op never crashes the TUI (G5).
            _ => {}
        }
    }
}

/// The stdin writer thread. It owns the blocking writes so a stuck
/// extension (full pipe, dead reader) can never block the main loop.
fn writer_thread(slot: Arc<SlotShared>) {
    let Some(rx) = slot.send_rx.lock().unwrap().take() else {
        return;
    };
    while let Ok(msg) = rx.recv() {
        let mut guard = slot.stdin.lock().unwrap();
        let Some(w) = guard.as_mut() else {
            // Between generations: the op has no target. Drop it.
            continue;
        };
        if writeln!(w, "{}", msg).is_err() || w.flush().is_err() {
            // The generation is over; the next generation gets a
            // fresh stdin.
            *guard = None;
        }
    }
}

/// The stdout pump for one generation: JSONL lines to the host.
fn reader_thread(inner: Arc<HostInner>, idx: usize, stdout: std::fs::File) {
    let reader = std::io::BufReader::new(stdout);
    for line in reader.lines() {
        match line {
            Ok(l) => inner.handle_reply(idx, &l),
            Err(_) => break,
        }
    }
}

/// One extension process: the pid plus the parent's pipe ends.
pub struct ExtChild {
    pid: i32,
}

impl ExtChild {
    /// Wait for the extension to exit. Returns the exit code, or
    /// `-1` when killed by a signal. The monitor thread is the only
    /// caller, so the child is reaped there.
    fn wait(&self) -> i32 {
        let mut status = 0i32;
        let r = unsafe { libc::waitpid(self.pid, &mut status, 0) };
        if r != self.pid {
            return -1;
        }
        if libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status)
        } else {
            -1
        }
    }
}

/// Build the exec argv for one manifest. Null bytes in an argument
/// are a broken manifest; the discovery step has passed, so this
/// only fails on `String` to `CString` conversion of valid input.
fn build_argv(m: &Manifest) -> Result<Vec<CString>, String> {
    let mut v: Vec<CString> = Vec::with_capacity(m.args.len() + 1);
    let cmd_bytes: Vec<u8> = m.command_path.as_os_str().as_encoded_bytes().to_vec();
    v.push(CString::new(cmd_bytes.as_slice()).map_err(|e| e.to_string())?);
    for a in &m.args {
        v.push(CString::new(a.as_bytes()).map_err(|e| e.to_string())?);
    }
    Ok(v)
}

/// The extension-host debug log. One line per extension process event
/// (spawn, death, respawn, stop) is appended to a log file.
///
/// The trace never writes to stderr: the TUI owns the terminal
/// (alt-screen), and a raw write to stderr bypasses the renderer and
/// pollutes the frame. The line format is `pid ms msg`.
fn ext_log(msg: &str) {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let line = format!("{} {} {}\n", std::process::id(), ms, msg);
    let Some(path) = ext_log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// The ext-host log file. `TUI_EXT_LOG` when set (the override, per
/// docs/ui-extension.md section 7); otherwise the default
/// `<cache>/tui/ext-host.log` under `$XDG_CACHE_HOME` or
/// `$HOME/.cache`. `None` when neither resolves.
fn ext_log_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("TUI_EXT_LOG") {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|d| d.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")));
    cache.map(|d| d.join("tui").join("ext-host.log"))
}

/// Spawn one extension generation. The child joins its own process
/// group via `setsid`, so [`ExtHost::stop`] kills the whole group,
/// not just the top process (docs/tui.md section 13.3). The working
/// directory is the manifest dir; `CONFIG` and `EXT_DIR` are
/// exported. Stderr is not part of the protocol: it goes to
/// /dev/null.
fn spawn_gen(
    slot: &Arc<SlotShared>,
    m: &Manifest,
    config_path: &Path,
) -> Result<(ExtChild, Option<std::fs::File>), String> {
    let mut in_pipe = [0i32; 2];
    let mut out_pipe = [0i32; 2];
    unsafe {
        if libc::pipe(in_pipe.as_mut_ptr()) != 0 || libc::pipe(out_pipe.as_mut_ptr()) != 0 {
            for fd in [in_pipe[0], in_pipe[1], out_pipe[0], out_pipe[1]] {
                let _ = libc::close(fd);
            }
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    let argv = build_argv(m)?;
    // The child must not take a lock after fork: the host may be
    // multithreaded, and a fork that copies locked futexes deadlocks
    // the child on its first allocation. The CString args and the
    // argv pointer table are built here, in the parent; the child
    // path below is libc calls only, no heap.
    let argv_ptrs: Vec<*const libc::c_char> = {
        let mut v: Vec<*const libc::c_char> = argv.iter().map(|s| s.as_ptr()).collect();
        v.push(std::ptr::null());
        v
    };
    let cwd_c = CString::new(m.dir.as_os_str().as_bytes().to_vec()).map_err(|e| e.to_string())?;
    let config_c =
        CString::new(config_path.as_os_str().as_bytes().to_vec()).map_err(|e| e.to_string())?;
    let ext_dir_c =
        CString::new(m.dir.as_os_str().as_bytes().to_vec()).map_err(|e| e.to_string())?;

    unsafe {
        let pid = libc::fork();
        match pid {
            -1 => {
                for fd in [in_pipe[0], in_pipe[1], out_pipe[0], out_pipe[1]] {
                    let _ = libc::close(fd);
                }
                Err(std::io::Error::last_os_error().to_string())
            }
            0 => {
                // Child: new session, redirected stdio, then exec.
                let _ = libc::setsid();
                let _ = libc::dup2(in_pipe[0], 0);
                let _ = libc::dup2(out_pipe[1], 1);
                let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR, 0);
                if devnull >= 0 {
                    let _ = libc::dup2(devnull, 2);
                    let _ = libc::close(devnull);
                }
                for fd in [in_pipe[0], in_pipe[1], out_pipe[0], out_pipe[1]] {
                    let _ = libc::close(fd);
                }
                let _ = libc::chdir(cwd_c.as_ptr());
                libc::setenv(c"CONFIG".as_ptr(), config_c.as_ptr(), 1);
                libc::setenv(c"EXT_DIR".as_ptr(), ext_dir_c.as_ptr(), 1);
                libc::execvp(argv[0].as_ptr(), argv_ptrs.as_ptr());
                // execvp failed: 127 marks a generation that never
                // started; the monitor counts it as a failed attempt.
                libc::_exit(127);
            }
            n => {
                let _ = libc::close(in_pipe[0]);
                let _ = libc::close(out_pipe[1]);
                let stdin = std::fs::File::from_raw_fd(in_pipe[1]);
                let stdout = std::fs::File::from_raw_fd(out_pipe[0]);
                slot.pid.store(n, Ordering::SeqCst);
                *slot.stdin.lock().unwrap() = Some(BufWriter::new(stdin));
                Ok((ExtChild { pid: n }, Some(stdout)))
            }
        }
    }
}

/// The monitor thread: wait, restart with the backoff budget, then
/// dead. The stop flag ends the loop without a restart.
#[builder]
fn monitor_thread(
    slot: Arc<SlotShared>,
    inner: Arc<HostInner>,
    stop: Arc<AtomicBool>,
    delays: [Duration; 3],
    manifest: Manifest,
    config_path: PathBuf,
    idx: usize,
    first: (ExtChild, Option<std::fs::File>),
) {
    let mut gen: Option<(ExtChild, Option<std::fs::File>)> = Some(first);
    let mut attempt: usize = 0;
    loop {
        if let Some((child, so)) = gen.take() {
            if let Some(so) = so {
                let rinner = inner.clone();
                std::thread::Builder::new()
                    .name(format!("tui-ext-read-{}", slot.name))
                    .spawn(move || reader_thread(rinner, idx, so))
                    .ok();
            }
            let code = child.wait();
            ext_log(&format!(
                "death {} gen={} exit={}",
                slot.name, attempt, code
            ));
        }
        // The generation ended: it exited, or the last spawn failed.
        if stop.load(Ordering::SeqCst) {
            ext_log(&format!("stop {} gen={}", slot.name, attempt));
            break;
        }
        if attempt >= delays.len() {
            ext_log(&format!("dead {} (budget spent)", slot.name));
            mark_dead(&slot, &inner, idx);
            break;
        }
        let d = delays[attempt];
        attempt += 1;
        *slot.state.lock().unwrap() = SlotState::Restarting;
        interruptible_sleep(d, &stop);
        if stop.load(Ordering::SeqCst) {
            break;
        }
        // A failed spawn consumes the attempt; the loop backs off
        // again on the next pass and dies when the budget is spent.
        gen = match spawn_gen(&slot, &manifest, &config_path) {
            Ok(g) => {
                ext_log(&format!(
                    "respawn {} gen={} pid={}",
                    manifest.name, attempt, g.0.pid
                ));
                // A fresh generation: the staleness clock and flag
                // reset with it.
                *slot.gen_started.lock().unwrap() = Instant::now();
                *slot.last_status_reply.lock().unwrap() = None;
                slot.status_stale.store(false, Ordering::SeqCst);
                Some(g)
            }
            Err(e) => {
                ext_log(&format!(
                    "respawn {} gen={} failed: {}",
                    manifest.name, attempt, e
                ));
                None
            }
        };
        // A spawn that raced the host stop: the stop pass may have
        // finished its pid collection just before this generation
        // stored its pid. Kill the new group and break, so no orphan
        // generation outlives the stop (docs/ui-extension.md section 7).
        if stop.load(Ordering::SeqCst) {
            if let Some((child, _)) = &gen {
                let p = child.pid;
                unsafe {
                    libc::kill(-p, libc::SIGKILL);
                }
            }
            break;
        }
        if gen.is_none() && attempt >= delays.len() {
            mark_dead(&slot, &inner, idx);
            break;
        }
    }
}

fn mark_dead(slot: &Arc<SlotShared>, inner: &Arc<HostInner>, idx: usize) {
    // Set the dead mark before clearing the cache: a reader that
    // passes the mark check under the `lines_cache` lock inserts
    // before the clear; the clear then wipes the insert. A reader
    // that checks after the mark sees it and skips.
    slot.dead.store(true, Ordering::SeqCst);
    *slot.state.lock().unwrap() = SlotState::Dead;
    // A dead extension cannot produce new replies. Its cached `lines`
    // replies are stale views: drop them so the built-in render
    // returns (ui-extension-plan stage 2 acceptance: kill the
    // extension, the built-in render comes back).
    slot.lines_cache.lock().unwrap().clear();
    {
        let mut reg = inner.transform.lock().unwrap();
        for r in reg.reqs.values_mut() {
            if r.owner == idx {
                r.state = TState::Stale;
            }
        }
    }
    inner.replies_version.fetch_add(1, Ordering::SeqCst);
    // Also clear any cached commands and in-flight invokes for this slot.
    inner.commands_cache.lock().unwrap().remove(&idx);
    {
        let mut reg = inner.invoke.lock().unwrap();
        for r in reg.reqs.values_mut() {
            if r.owner == idx {
                r.state = InvokeState::Stale;
            }
        }
    }
    let _ = inner.out_tx.try_send(ExtItem::Dead {
        ext: slot.name.clone(),
    });
}

/// Sleep `d`, waking early when the stop flag is set.
fn interruptible_sleep(d: Duration, stop: &AtomicBool) {
    let end = Instant::now() + d;
    while !stop.load(Ordering::SeqCst) {
        let now = Instant::now();
        if now >= end {
            break;
        }
        std::thread::sleep((end - now).min(Duration::from_millis(50)));
    }
}

fn group_alive(pid: i32) -> bool {
    // Signal 0 checks group existence without delivering anything.
    let r = unsafe { libc::kill(-pid, 0) };
    r == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::produce;
    use serde_json::json;
    use tempfile::TempDir;

    /// Write one extension entry (dir + ext.toml) into `dir`.
    fn write_ext(dir: &Path, name: &str, toml_body: &str) {
        let d = dir.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("ext.toml"), toml_body).unwrap();
    }

    fn cfg_for(root: &Path) -> TuiConfig {
        TuiConfig {
            sessions_root: root.join("sessions"),
            schemas_dir: None,
            loop_cmd: None,
            config_dir: root.to_path_buf(),
            config_path: root.join("config.toml"),
            ext_dir: None,
            active_model: None,
            color: None,
            color_scheme: None,
            custom_schemes: std::collections::HashMap::new(),
            tool_display: crate::tool_display::ToolDisplay::preset(
                crate::tool_display::Preset::OpenCode,
            ),
        }
    }

    const OK_MANIFEST: &str = r#"
[ext]
command = "bash"
args = ["echo", "hi"]
caps = ["render"]
kinds = ["tool_result"]
tick_ms = 250
transform = ["fence:mermaid"]
append_types = ["ext_status"]
protocol_v = 1
"#;

    // ── manifest parse ────────────────────────────────────────────

    #[test]
    fn manifest_parses_with_defaults() {
        let dir = TempDir::new().unwrap();
        write_ext(
            dir.path(),
            "a",
            "[ext]\ncommand = \"bash\"\nprotocol_v = 1\n",
        );
        let m = load_manifest(&dir.path().join("a")).unwrap();
        assert_eq!(m.name, "a");
        assert_eq!(m.command, "bash");
        assert_eq!(m.tick_ms, DEFAULT_TICK_MS, "tick_ms defaults to 1000");
        assert!(m.protocol_ok);
        assert!(m.kinds.is_empty(), "no kinds means all");
    }

    #[test]
    fn manifest_full_fields() {
        let dir = TempDir::new().unwrap();
        write_ext(dir.path(), "a", OK_MANIFEST);
        let m = load_manifest(&dir.path().join("a")).unwrap();
        assert_eq!(m.caps, vec!["render"]);
        assert_eq!(m.kinds, vec!["tool_result"]);
        assert_eq!(m.tick_ms, 250);
        assert_eq!(m.transform, vec!["fence:mermaid"]);
        assert_eq!(m.append_types, vec!["ext_status"]);
        assert_eq!(m.args, vec!["echo", "hi"]);
    }

    #[test]
    fn manifest_relative_command_resolves_against_manifest_dir() {
        let dir = TempDir::new().unwrap();
        // Create a fake binary at target/debug/my-ext inside the entry dir.
        let ext_dir = dir.path().join("myext");
        std::fs::create_dir_all(ext_dir.join("target").join("debug")).unwrap();
        let bin_path = ext_dir.join("target").join("debug").join("my-ext");
        std::fs::write(&bin_path, "#!/bin/sh\n").unwrap();
        // Write manifest with a relative command path.
        let ext_toml = "[ext]\ncommand = \"target/debug/my-ext\"\nprotocol_v = 1\n";
        std::fs::write(ext_dir.join("ext.toml"), ext_toml).unwrap();
        let m = load_manifest(&ext_dir).unwrap();
        // The resolved command_path must be absolute and point to the binary.
        assert!(m.command_path.is_absolute(), "resolved path must be absolute");
        assert_eq!(m.command_path, bin_path,
            "relative command resolves against the manifest dir");
        assert!(m.command_path.is_file(), "resolved path points to a real file");
    }

    #[test]
    fn manifest_relative_command_missing_binary_refuses() {
        let dir = TempDir::new().unwrap();
        let ext_dir = dir.path().join("ghost");
        std::fs::create_dir_all(&ext_dir).unwrap();
        let ext_toml = "[ext]\ncommand = \"target/debug/ghost-ext\"\nprotocol_v = 1\n";
        std::fs::write(ext_dir.join("ext.toml"), ext_toml).unwrap();
        let err = load_manifest(&ext_dir).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ghost-ext"), "error names the command");
        assert!(msg.contains("not found"), "error says not found");
    }

    #[test]
    fn manifest_missing_command_refuses_with_the_file() {
        let dir = TempDir::new().unwrap();
        write_ext(dir.path(), "a", "[ext]\nprotocol_v = 1\n");
        let err = load_manifest(&dir.path().join("a")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("a/ext.toml"), "{msg}");
        assert!(msg.contains("missing `command`"), "{msg}");
    }

    #[test]
    fn manifest_unknown_cap_refuses_with_the_file() {
        let dir = TempDir::new().unwrap();
        write_ext(
            dir.path(),
            "a",
            "[ext]\ncommand = \"bash\"\ncaps = [\"telepathy\"]\nprotocol_v = 1\n",
        );
        let err = load_manifest(&dir.path().join("a")).unwrap_err();
        assert!(err.to_string().contains("unknown cap"), "{err}");
    }

    #[test]
    fn manifest_unknown_kind_refuses_with_the_file() {
        let dir = TempDir::new().unwrap();
        write_ext(
            dir.path(),
            "a",
            "[ext]\ncommand = \"bash\"\nkinds = [\"flux_capacitor\"]\nprotocol_v = 1\n",
        );
        let err = load_manifest(&dir.path().join("a")).unwrap_err();
        assert!(err.to_string().contains("unknown event kind"), "{err}");
    }

    #[test]
    fn manifest_bad_transform_target_refuses() {
        let dir = TempDir::new().unwrap();
        write_ext(
            dir.path(),
            "a",
            "[ext]\ncommand = \"bash\"\ntransform = [\"mermaid\"]\nprotocol_v = 1\n",
        );
        let err = load_manifest(&dir.path().join("a")).unwrap_err();
        assert!(err.to_string().contains("bad transform target"), "{err}");
        // The scope-qualified forms pass.
        write_ext(
            dir.path(),
            "b",
            "[ext]\ncommand = \"bash\"\ntransform = [\"fence:mermaid\", \"inline:latex\"]\nprotocol_v = 1\n",
        );
        assert!(load_manifest(&dir.path().join("b")).is_ok());
    }

    #[test]
    fn manifest_bad_toml_refuses_with_the_file() {
        let dir = TempDir::new().unwrap();
        write_ext(dir.path(), "a", "[ext\nnot toml");
        let err = load_manifest(&dir.path().join("a")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("a/ext.toml"), "{msg}");
        assert!(msg.contains("invalid TOML"), "{msg}");
    }

    #[test]
    fn manifest_broken_command_refuses_with_the_file() {
        let dir = TempDir::new().unwrap();
        write_ext(
            dir.path(),
            "a",
            "[ext]\ncommand = \"definitely-not-a-cmd-zz\"\nprotocol_v = 1\n",
        );
        let err = load_manifest(&dir.path().join("a")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("a/ext.toml"), "{msg}");
        assert!(msg.contains("not found"), "{msg}");
    }

    #[test]
    fn manifest_protocol_v_mismatch_marks_unsupported() {
        let dir = TempDir::new().unwrap();
        write_ext(
            dir.path(),
            "a",
            "[ext]\ncommand = \"bash\"\nprotocol_v = 2\n",
        );
        let m = load_manifest(&dir.path().join("a")).unwrap();
        assert!(!m.protocol_ok, "v2 is not what this host speaks");
        write_ext(dir.path(), "b", "[ext]\ncommand = \"bash\"\n");
        let m = load_manifest(&dir.path().join("b")).unwrap();
        assert!(!m.protocol_ok, "a missing protocol_v is a mismatch too");
    }

    // ── discovery and layering ────────────────────────────────────

    #[test]
    fn discovery_orders_layers_and_overrides_by_name() {
        let dir = TempDir::new().unwrap();
        let global = dir.path().join("ui_extensions");
        let project = dir.path().join(".pi").join("ui_extensions");
        write_ext(
            &global,
            "alpha",
            "[ext]\ncommand = \"bash\"\nkinds = [\"tool_result\"]\nprotocol_v = 1\n",
        );
        write_ext(
            &global,
            "beta",
            "[ext]\ncommand = \"bash\"\nprotocol_v = 1\n",
        );
        write_ext(&project, "alpha", "[ext]\ncommand = \"bash\"\ncaps = [\"status\"]\nkinds = [\"tool_result\"]\nprotocol_v = 1\n");

        let mut cfg = cfg_for(dir.path());
        cfg.ext_dir = Some(global.clone());
        let d = discover(&cfg).unwrap();
        // Global layer first (alpha replaced by the project entry, so
        // only beta remains), then the project layer.
        assert_eq!(
            d.exts
                .iter()
                .map(|e| e.manifest.name.clone())
                .collect::<Vec<_>>(),
            vec!["beta", "alpha"]
        );
        assert_eq!(
            d.index_by_name["alpha"], 1,
            "the project entry overrides the global one"
        );
        // Kind ownership: alpha (project) is the only tool_result owner.
        assert_eq!(d.kind_owners.get(&EventKind::ToolResult), Some(&1));
        assert_eq!(d.status_owner, Some(1));
    }

    #[test]
    fn discovery_kind_owner_is_first_in_sequence() {
        let dir = TempDir::new().unwrap();
        let global = dir.path().join("ui_extensions");
        write_ext(
            &global,
            "a",
            "[ext]\ncommand = \"bash\"\nkinds = [\"tool_result\"]\nprotocol_v = 1\n",
        );
        write_ext(
            &global,
            "b",
            "[ext]\ncommand = \"bash\"\nkinds = [\"tool_result\", \"error\"]\nprotocol_v = 1\n",
        );
        let mut cfg = cfg_for(dir.path());
        cfg.ext_dir = Some(global);
        let d = discover(&cfg).unwrap();
        assert_eq!(
            d.kind_owners.get(&EventKind::ToolResult),
            Some(&0),
            "the first extension that lists the kind owns it"
        );
        assert_eq!(d.kind_owners.get(&EventKind::Error), Some(&1));
    }

    #[test]
    fn discovery_entry_without_manifest_refuses() {
        let dir = TempDir::new().unwrap();
        let global = dir.path().join("ui_extensions");
        std::fs::create_dir_all(global.join("empty-entry")).unwrap();
        let mut cfg = cfg_for(dir.path());
        cfg.ext_dir = Some(global.clone());
        let err = discover(&cfg).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("empty-entry"), "{msg}");
        assert!(msg.contains("no ext.toml"), "{msg}");
    }

    #[test]
    fn discovery_status_conflict_refuses_and_names_both() {
        let dir = TempDir::new().unwrap();
        let global = dir.path().join("ui_extensions");
        write_ext(
            &global,
            "s1",
            "[ext]\ncommand = \"bash\"\ncaps = [\"status\"]\nprotocol_v = 1\n",
        );
        write_ext(
            &global,
            "s2",
            "[ext]\ncommand = \"bash\"\ncaps = [\"status\"]\nprotocol_v = 1\n",
        );
        let mut cfg = cfg_for(dir.path());
        cfg.ext_dir = Some(global);
        let err = discover(&cfg).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("s1/ext.toml"), "{msg}");
        assert!(msg.contains("s2/ext.toml"), "{msg}");
    }

    // ── frame capability ───────────────────────────────────────

    #[test]
    fn discovery_frame_owner_resolves_and_conflict_refuses() {
        let dir = TempDir::new().unwrap();
        let global = dir.path().join("ui_extensions");
        write_ext(
            &global,
            "f1",
            "[ext]\ncommand = \"bash\"\ncaps = [\"frame\"]\nprotocol_v = 1\n",
        );
        let mut cfg = cfg_for(dir.path());
        cfg.ext_dir = Some(global.clone());
        let disc = discover(&cfg).unwrap();
        assert_eq!(disc.frame_owner, Some(0), "a single frame owner resolves");
        // A second frame owner refuses the start, like the status row.
        write_ext(
            &global,
            "f2",
            "[ext]\ncommand = \"bash\"\ncaps = [\"frame\"]\nprotocol_v = 1\n",
        );
        let err = discover(&cfg).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("f1/ext.toml"), "{msg}");
        assert!(msg.contains("f2/ext.toml"), "{msg}");
    }

    // ── row capability ─────────────────────────────────────────

    #[test]
    fn discovery_row_owner_resolves_and_conflict_refuses() {
        let dir = TempDir::new().unwrap();
        let global = dir.path().join("ui_extensions");
        write_ext(
            &global,
            "r1",
            "[ext]\ncommand = \"bash\"\ncaps = [\"row\"]\nprotocol_v = 1\n",
        );
        let mut cfg = cfg_for(dir.path());
        cfg.ext_dir = Some(global.clone());
        let disc = discover(&cfg).unwrap();
        assert_eq!(disc.row_owner, Some(0), "a single row owner resolves");
        // A second row owner refuses the start, like the status row.
        write_ext(
            &global,
            "r2",
            "[ext]\ncommand = \"bash\"\ncaps = [\"row\"]\nprotocol_v = 1\n",
        );
        let err = discover(&cfg).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("r1/ext.toml"), "{msg}");
        assert!(msg.contains("r2/ext.toml"), "{msg}");
    }

    #[test]
    fn row_caps_are_valid_manifest_caps() {
        // `row` is a known capability: a manifest listing it does
        // not refuse the start (docs/ui-extension.md section 3).
        let dir = TempDir::new().unwrap();
        let global = dir.path().join("ui_extensions");
        write_ext(
            &global,
            "r1",
            "[ext]\ncommand = \"bash\"\ncaps = [\"row\"]\nprotocol_v = 1\n",
        );
        let mut cfg = cfg_for(dir.path());
        cfg.ext_dir = Some(global);
        discover(&cfg).expect("a `row` manifest is valid");
    }

    #[test]
    fn frame_spec_value_parses_full_and_partial() {
        let v = json!({
            "border": "double",
            "label": {"lines": ["compose"], "style": {"fg": "cyan", "bold": true}},
            "height": 3,
        });
        let spec = frame_spec_value(&v).unwrap();
        assert_eq!(spec.border, Some(FrameBorderStyle::Double));
        assert_eq!(spec.height, Some(3));
        let (lines, style) = spec.label.unwrap();
        assert_eq!(lines.len(), 1);
        // A styled label keeps its wire style; the host renders it.
        assert_eq!(
            style,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        );
        // A partial spec: missing fields keep the host built-in.
        let v = json!({"border": "rounded"});
        let spec = frame_spec_value(&v).unwrap();
        assert_eq!(spec.border, Some(FrameBorderStyle::Rounded));
        assert_eq!(spec.label, None);
        assert_eq!(spec.height, None);
        // An unknown border shape is malformed: G5 keeps the last
        // valid frame.
        let v = json!({"border": "circular"});
        assert!(frame_spec_value(&v).is_none());
    }

    #[test]
    fn frame_reply_caches_the_spec() {
        let dir = TempDir::new().unwrap();
        let global = dir.path().join("ui_extensions");
        write_ext(
            &global,
            "f1",
            "[ext]\ncommand = \"cat\"\ncaps = [\"frame\"]\nprotocol_v = 1\n",
        );
        let mut cfg = cfg_for(dir.path());
        cfg.ext_dir = Some(global);
        let disc = discover(&cfg).unwrap();
        let host = ExtHost::new(&disc, &cfg);
        let i = disc.frame_owner.expect("a frame owner");
        // Before a reply: no frame spec (the built-in frame shows).
        assert!(host.frame_spec().is_none());
        // A valid frame_spec reply lands in the slot.
        host.reply_line(
            i,
            r#"{"v":1,"op":"frame_spec","spec":{"border":"thick","height":4}}"#,
        );
        let spec = host.frame_spec().expect("a valid frame_spec reply caches");
        assert_eq!(spec.border, Some(FrameBorderStyle::Thick));
        assert_eq!(spec.height, Some(4));
        // A bad frame_spec reply keeps the last valid one (G5).
        host.reply_line(i, r#"{"v":1,"op":"frame_spec","spec":"nonsense"}"#);
        assert!(host
            .frame_spec()
            .expect("G5: the last valid frame survives")
            .height
            .is_some());
        let _ = dir;
        let _ = host;
    }

    #[test]
    fn dual_cap_extension_gets_frame_ops_despite_tick_cadence() {
        let tmp = TempDir::new().unwrap();
        // One extension declares both caps. `pump_ticks` consumes the
        // status cadence on every pump; the `frame` op must ride its
        // own clock. On a shared clock, `pump_ticks` re-arms the
        // deadline microseconds before `pump_frame` checks it, so the
        // frame op is never sent and no frame_spec reply ever lands.
        let manifest = "[ext]\ncommand = \"bash\"\nargs = [\"dual.sh\"]\ncaps = [\"status\",\"frame\"]\ntick_ms = 50\nprotocol_v = 1\n";
        let script = r#"while IFS= read -r line; do
  case "$line" in
    *'"op":"frame"'*)
      printf '{"v":1,"op":"frame_spec","spec":{"border":"rounded"}}\n'
      ;;
    *'"op":"tick"'*)
      printf '{"v":1,"op":"status","lines":[["on",null]]}\n'
      ;;
  esac
done
"#;
        let host = host_with(&tmp, "dual", manifest, script);
        host.start();
        let empty = std::collections::HashMap::new();
        let p = crate::ext::TickPayload {
            width: 80,
            session: Some("s1"),
            model: None,
            thinking: 0,
            loop_running: false,
            statuses: &empty,
        };
        let due = Instant::now() + Duration::from_millis(2000);
        let mut saw_frame = false;
        while Instant::now() < due {
            host.pump_ticks(&p);
            host.pump_frame(&p, "INSERT");
            if host.frame_spec().is_some() {
                saw_frame = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(saw_frame, "the frame owner's frame_spec reply lands");
        host.stop();
    }

    // ── wire style and lines payloads ─────────────────────────────

    #[test]
    fn style_hex_and_tokens() {
        let s = wire_style(Some(
            json!({"fg": "#f00", "bg": "#00ff00", "bold": true})
                .as_object()
                .unwrap(),
        ));
        assert_eq!(
            s,
            Style::default()
                .fg(Color::Rgb(255, 0, 0))
                .bg(Color::Rgb(0, 255, 0))
                .add_modifier(Modifier::BOLD)
        );
        let s = wire_style(Some(json!({"fg": "cyan"}).as_object().unwrap()));
        assert_eq!(s, Style::default().fg(Color::Cyan));
        // An unknown token degrades to no color.
        let s = wire_style(Some(json!({"fg": "chartreuse"}).as_object().unwrap()));
        assert_eq!(s, Style::default());
    }

    #[test]
    fn lines_payload_validation() {
        let ok = json!([["row one", {"fg": "red", "bold": true}], "row two", ["row three", null]]);
        let lines = lines_value(&ok).unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "row one");
        assert_eq!(
            lines[0].style,
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        );
        assert_eq!(lines[2].style, Style::default(), "null style is plain");

        // Bad shapes are all `None` (G5 fallback, never a crash).
        assert!(lines_value(&json!("not an array")).is_none());
        assert!(lines_value(&json!(42)).is_none());
        assert!(
            lines_value(&json!([["text", 7]])).is_none(),
            "a non-object style is invalid"
        );
        assert!(
            lines_value(&json!(["text", {}, "extra"])).is_none(),
            "a 3-tuple is invalid"
        );
        assert!(
            lines_value(&json!([7])).is_none(),
            "a bare number is invalid"
        );
    }

    #[test]
    fn multi_span_line_payload() {
        // A powerline footer line: three styled spans on one row.
        let ok = json!([["⎖", {"fg": "#24273a", "bg": "#24273a"}],
                        [["⎖", {"fg": "#24273a", "bg": "#24273a"}],
                         [" dir ", {"fg": "#cad3f5", "bg": "#24273a", "bold": true}],
                         ["⎗", null]]]);
        let lines = lines_value(&ok).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans.len(), 1, "a one-item span list is one span");
        let multi = &lines[1];
        assert_eq!(multi.spans.len(), 3);
        assert_eq!(multi.text, "⎖ dir ⎗", "the text field joins the span texts");
        assert_eq!(
            multi.spans[0].style,
            Style::default()
                .fg(Color::Rgb(0x24, 0x27, 0x3a))
                .bg(Color::Rgb(0x24, 0x27, 0x3a))
        );
        assert_eq!(
            multi.spans[1].style,
            Style::default()
                .fg(Color::Rgb(0xca, 0xd3, 0xf5))
                .bg(Color::Rgb(0x24, 0x27, 0x3a))
                .add_modifier(Modifier::BOLD)
        );
        assert_eq!(multi.spans[2].text, "⎗");
        assert_eq!(
            multi.spans[2].style,
            Style::default(),
            "a null span style is plain"
        );
        // The legacy single-span form parses to a one-span line.
        let one = lines_value(&json!([[["text", null]]])).unwrap();
        assert_eq!(one[0].spans.len(), 1);
        assert_eq!(one[0].text, "text");
        // Invalid multi-span shapes (G5 fallback, never a crash).
        assert!(
            lines_value(&json!([[]])).is_none(),
            "an empty span list is invalid"
        );
        assert!(
            lines_value(&json!([[[["text", null]]]])).is_none(),
            "a bare string inside the span list is invalid"
        );
        assert!(
            lines_value(&json!([[[["text", null, 3]]]])).is_none(),
            "a 3-tuple span is invalid"
        );
    }

    // ── host process behavior ─────────────────────────────────────

    /// A discovery with one extension whose command is a bash
    /// script. `script` is written to `<entry>/<name>.sh`; the
    /// manifest (with `args = ["<name>.sh"]`) is written as given.
    fn host_with(tmp: &TempDir, name: &str, manifest: &str, script: &str) -> ExtHost {
        let root = tmp.path().to_path_buf();
        let global = root.join("ui_extensions");
        let entry = global.join(name);
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(
            entry.join(format!("{name}.sh")),
            format!("#!/usr/bin/env bash\n{script}\n"),
        )
        .unwrap();
        std::fs::write(entry.join("ext.toml"), manifest).unwrap();
        let mut cfg = cfg_for(&root);
        cfg.ext_dir = Some(global);
        let disc = discover(&cfg).unwrap();
        ExtHost::new(&disc, &cfg)
    }

    /// Pump host outbox items until the predicate matches or the
    /// deadline passes.
    fn wait_item(host: &ExtHost, what: &str, mut pred: impl FnMut(&ExtItem) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            while let Some(item) = host.drain() {
                if pred(&item) {
                    return true;
                }
            }
            if Instant::now() >= deadline {
                eprintln!("wait_item({what}) timed out");
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn lines_reply_replaces_the_builtin_render() {
        let tmp = TempDir::new().unwrap();
        let manifest =
            "[ext]\ncommand = \"bash\"\nargs = [\"renderer.sh\"]\nkinds = [\"tool_result\"]\nprotocol_v = 1\n";
        let script = r#"while IFS= read -r line; do
  case "$line" in
    *'"op":"event"'*)
      ev_id=$(printf '%s' "$line" | sed 's/.*"id":\([0-9]*\).*/\1/')
      printf '{"v":1,"op":"lines","event_id":%s,"lines":[["EXT RENDERED",{"fg":"green"}]]}\n' "$ev_id"
      ;;
  esac
done
"#;
        let host = host_with(&tmp, "renderer", manifest, script);
        host.start();
        // History resend of one tool_result event (log index 0).
        host.send_history(&[Event::parse_line(
            r#"{"v":1,"type":"tool_result","ts":"t","id":"c1","value":{"exit_code":0},"is_error":false}"#,
        )
        .unwrap()]);
        assert!(
            wait_item(&host, "lines reply", |i| matches!(
                i,
                ExtItem::LinesCached { .. }
            )),
            "the valid lines reply must reach the host"
        );
        let got = {
            let cache = host.inner.slots[0].lines_cache.lock().unwrap();
            cache.get(&0u64).cloned()
        };
        let got = got.expect("the reply must be cached by (ext, event_id)");
        assert_eq!(got[0].text, "EXT RENDERED");
        host.stop();
    }

    #[test]
    fn malformed_lines_reply_keeps_the_builtin_render() {
        let tmp = TempDir::new().unwrap();
        let manifest = "[ext]\ncommand = \"bash\"\nargs = [\"junk.sh\"]\nprotocol_v = 1\n";
        let script = "echo 'this is not json'
echo '{\"v\":1,\"op\":\"lines\",\"event_id\":0,\"lines\":BROKEN}'
echo '{\"v\":1,\"op\":\"lines\",\"event_id\":0,\"lines\":42}'";
        let host = host_with(&tmp, "junk", manifest, script);
        host.start();
        std::thread::sleep(Duration::from_millis(300));
        let cache = host.inner.slots[0].lines_cache.lock().unwrap();
        assert!(
            cache.get(&0u64).is_none(),
            "a bad lines reply must not be cached"
        );
        host.stop();
    }

    #[test]
    fn shape_invalid_replies_keep_state() {
        // Valid JSON with a bad payload shape. The per-op G5 fallback
        // applies: status keeps the last valid row, lines cache stays
        // empty (docs/ui-extension.md section 4).
        let tmp = TempDir::new().unwrap();
        let manifest =
            "[ext]\ncommand = \"bash\"\nargs = [\"stat.sh\"]\ncaps = [\"status\"]\nprotocol_v = 1\n";
        let host = host_with(&tmp, "stat", manifest, "sleep 30");
        host.start();
        host.reply_line(0, r#"{"v":1,"op":"status","lines":[["good",{}]]}"#);
        host.reply_line(0, r#"{"v":1,"op":"status","lines":42}"#);
        let row = host.status_row();
        assert!(
            matches!(row, StatusRow::Lines(ref l) if l[0].text == "good"),
            "a bad-shape status reply keeps the last valid row: {row:?}"
        );
        host.reply_line(0, r#"{"v":1,"op":"lines","event_id":7,"lines":"nope"}"#);
        assert!(
            host.inner.slots[0]
                .lines_cache
                .lock()
                .unwrap()
                .get(&7u64)
                .is_none(),
            "a bad-shape lines reply must not be cached"
        );
        host.stop();
    }

    #[test]
    fn status_reply_and_dead_hint() {
        let tmp = TempDir::new().unwrap();
        let manifest =
            "[ext]\ncommand = \"bash\"\nargs = [\"stat.sh\"]\ncaps = [\"status\"]\ntick_ms = 100\nprotocol_v = 1\n";
        let script = r#"while IFS= read -r line; do
  case "$line" in
    *'"op":"tick"'*)
      printf '{"v":1,"op":"status","lines":[["stat row",{"bold":true}]]}\n'
      ;;
  esac
done
"#;
        let host = host_with(&tmp, "stat", manifest, script);
        host.start();
        // No tick yet: the row is still the built-in one.
        assert!(matches!(host.status_row(), StatusRow::Builtin));
        let mut statuses = HashMap::new();
        statuses.insert("vim_mode".into(), json!("insert"));
        let due = Instant::now() + Duration::from_millis(2000);
        let mut saw_tick = false;
        while Instant::now() < due {
            let p = TickPayload {
                width: 80,
                session: Some("s1"),
                model: Some("test-model"),
                thinking: 0,
                loop_running: false,
                statuses: &statuses,
            };
            host.pump_ticks(&p);
            if host.status_row()
                == StatusRow::Lines(vec![ExtLine::styled(
                    "stat row",
                    Style::default().add_modifier(Modifier::BOLD),
                )])
            {
                saw_tick = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(saw_tick, "the status row shows the last valid row");
        host.stop();
    }

    #[test]
    fn bad_status_reply_keeps_the_last_valid_row() {
        let tmp = TempDir::new().unwrap();
        let manifest =
            "[ext]\ncommand = \"bash\"\nargs = [\"stat.sh\"]\ncaps = [\"status\"]\nprotocol_v = 1\n";
        let script = r#"printf '{"v":1,"op":"status","lines":[["good row",{}]]}\n'
printf '{"v":1,"op":"status","lines":NOT_ARRAY}\n'
printf '{"v":1,"op":"status","lines":42}\n'
"#;
        let host = host_with(&tmp, "stat", manifest, script);
        host.start();
        let deadline = Instant::now() + Duration::from_millis(3000);
        while Instant::now() < deadline {
            if matches!(
                host.status_row(),
                StatusRow::Lines(ref l) if l[0].text == "good row"
            ) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            matches!(host.status_row(), StatusRow::Lines(ref l) if l[0].text == "good row"),
            "a bad status reply keeps the last valid row"
        );
        host.stop();
    }

    #[test]
    fn append_whitelist_rejects_and_accepts() {
        let tmp = TempDir::new().unwrap();
        let manifest =
            "[ext]\ncommand = \"bash\"\nargs = [\"appender.sh\"]\nappend_types = [\"ext_status\"]\nprotocol_v = 1\n";
        let script = r#"printf '{"v":1,"op":"append","event":{"v":1,"type":"tool_result","ts":"t","id":"x","value":{}}}\n'
printf '{"v":1,"op":"append","event":{"v":1,"type":"ext_status","ts":"t","id":"probe","value":"ok"}}\n'
"#;
        let host = host_with(&tmp, "appender", manifest, script);
        host.start();
        assert!(
            wait_item(
                &host,
                "reject",
                |i| matches!(i, ExtItem::AppendRejected { ext, reason } if ext == "appender" && reason.contains("tool_result")),
            ),
            "an append outside the whitelist must reject with the reason"
        );
        assert!(
            wait_item(
                &host,
                "accept",
                |i| matches!(i, ExtItem::AppendReq { ext, event } if ext == "appender" && event.get("type").and_then(|t| t.as_str()) == Some("ext_status")),
            ),
            "a whitelisted append must reach the main loop for the port"
        );
        host.stop();
    }

    #[test]
    fn notify_bell_and_osc_items() {
        let tmp = TempDir::new().unwrap();
        let manifest =
            "[ext]\ncommand = \"bash\"\nargs = [\"beller.sh\"]\ncaps = [\"notify\"]\nprotocol_v = 1\n";
        let script = r#"printf '{"v":1,"op":"notify","kind":"bell"}\n'
printf '{"v":1,"op":"notify","kind":"osc","code":0,"args":"job done"}\n'
"#;
        let host = host_with(&tmp, "beller", manifest, script);
        host.start();
        assert!(wait_item(&host, "bell", |i| matches!(
            i,
            ExtItem::NotifyBell { .. }
        )));
        assert!(wait_item(&host, "osc", |i| matches!(
            i,
            ExtItem::NotifyOsc { code: 0, .. }
        )));
        host.stop();
    }

    #[test]
    fn restart_budget_ends_in_dead() {
        let tmp = TempDir::new().unwrap();
        let manifest = "[ext]\ncommand = \"bash\"\nargs = [\"dying.sh\"]\nprotocol_v = 1\n";
        let mut host = host_with(&tmp, "dying", manifest, "exit 1");
        host.set_restart_delays([
            Duration::from_millis(20),
            Duration::from_millis(30),
            Duration::from_millis(40),
        ]);
        host.start();
        assert!(
            wait_item(
                &host,
                "dead",
                |i| matches!(i, ExtItem::Dead { ext } if ext == "dying")
            ),
            "three restart attempts with the backoff, then dead"
        );
        assert_eq!(host.slot_state(0), SlotState::Dead);
        host.stop();
    }

    #[test]
    fn stop_kills_the_group() {
        let tmp = TempDir::new().unwrap();
        let manifest = "[ext]\ncommand = \"bash\"\nargs = [\"sleeper.sh\"]\nprotocol_v = 1\n";
        let host = host_with(&tmp, "sleeper", manifest, "sleep 30");
        host.start();
        std::thread::sleep(Duration::from_millis(200));
        let pid = host.inner.slots[0].pid.load(Ordering::SeqCst);
        assert!(pid > 0, "the extension is running");
        host.stop();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the group did not die after stop"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn skipped_extension_reports_the_reason() {
        let tmp = TempDir::new().unwrap();
        let manifest = "[ext]\ncommand = \"bash\"\nargs = [\"old.sh\"]\nprotocol_v = 2\n";
        let host = host_with(&tmp, "old", manifest, "sleep 30");
        let items = host.start();
        assert!(
            items.iter().any(|i| matches!(i, ExtItem::Skipped { ext, reason } if ext == "old" && reason.contains("protocol_v"))),
            "a protocol mismatch skips with a flash reason: {items:?}"
        );
        assert_eq!(host.slot_state(0), SlotState::Skipped);
        host.stop();
    }

    // ── transform registry ────────────────────────────────────────

    #[test]
    fn transform_reply_requires_the_current_req() {
        let tmp = TempDir::new().unwrap();
        let manifest =
            "[ext]\ncommand = \"bash\"\nargs = [\"mm.sh\"]\ntransform = [\"fence:mermaid\"]\nprotocol_v = 1\n";
        let mut host = host_with(&tmp, "mm", manifest, "sleep 30");
        host.set_transform_timeout(Duration::from_millis(200));
        host.start();
        let req = host.request_transform("fence:mermaid", "graph TD; A-->B", 80);
        assert!(req.is_some(), "a declared target has an owner");
        let req = req.unwrap();
        // A reply for a req nobody sent drops (stale rule).
        host.reply_line(
            0,
            r#"{"v":1,"op":"transformed","req":999,"lines":[["X",{}]]}"#,
        );
        assert!(host.transform_lines(999).is_none());
        // The right req lands and caches.
        host.reply_line(
            0,
            &format!(r#"{{"v":1,"op":"transformed","req":{req},"lines":[["MERMAID ART",{{}}]]}}"#),
        );
        assert_eq!(
            host.transform_lines(req)
                .as_ref()
                .map(|l| l[0].text.as_str()),
            Some("MERMAID ART")
        );
        // A resize re-requests: the old result is superseded.
        host.on_resize(120);
        assert!(
            host.transform_lines(req).is_none(),
            "a superseded req has no result; the raw block shows"
        );
        // The new req is the current one; its reply lands.
        let new_req = host.inner.transform.lock().unwrap().next - 1;
        host.reply_line(
            0,
            &format!(r#"{{"v":1,"op":"transformed","req":{new_req},"lines":[["WIDER",{{}}]]}}"#),
        );
        assert_eq!(
            host.transform_lines(new_req)
                .as_ref()
                .map(|l| l[0].text.as_str()),
            Some("WIDER")
        );
        host.stop();
    }

    #[test]
    fn transform_timeout_drops_the_block() {
        let tmp = TempDir::new().unwrap();
        let manifest =
            "[ext]\ncommand = \"bash\"\nargs = [\"slow.sh\"]\ntransform = [\"fence:mermaid\"]\nprotocol_v = 1\n";
        let mut host = host_with(&tmp, "slow", manifest, "sleep 30");
        host.set_transform_timeout(Duration::from_millis(50));
        host.start();
        let req = host
            .request_transform("fence:mermaid", "graph TD", 80)
            .unwrap();
        std::thread::sleep(Duration::from_millis(120));
        host.poll_transforms();
        assert!(
            host.transform_lines(req).is_none(),
            "a 2 s (here 50 ms) timeout shows the raw block"
        );
        // No extension declares the target: no request at all.
        assert!(host.request_transform("inline:latex", "$x$", 80).is_none());
        host.stop();
    }

    #[test]
    fn span_requests_dedupe_supersede_and_reuse() {
        let tmp = TempDir::new().unwrap();
        let manifest =
            "[ext]\ncommand = \"bash\"\nargs = [\"mm.sh\"]\ntransform = [\"fence:mermaid\"]\nprotocol_v = 1\n";
        let host = host_with(&tmp, "mm", manifest, "sleep 30");
        host.start();
        // First request for (event, span): it sends and caches the
        // id.
        let req = host
            .request_span(7, 0, "fence:mermaid", "graph TD", 64)
            .expect("the declared target has an owner");
        // While pending: dedupe, no resend.
        assert_eq!(
            host.request_span(7, 0, "fence:mermaid", "graph TD", 64),
            Some(req)
        );
        // A reply lands: the span is done.
        host.reply_line(
            0,
            &format!(
                r#"{{"v":1,"op":"transformed","req":{req},"lines":[["ART",{{}}]]}}"#,
                req = req
            ),
        );
        assert!(host.span_lines(7, 0).is_some(), "a done span has a result");
        // Done: reuse, no resend (a resend would bump the reply
        // version on every rebuild: an infinite loop).
        assert_eq!(
            host.request_span(7, 0, "fence:mermaid", "graph TD", 64),
            Some(req)
        );
        // A different span index is a different request.
        let req2 = host
            .request_span(7, 1, "fence:mermaid", "graph LR", 64)
            .expect("a declared target has an owner");
        assert_ne!(req2, req, "a new span gets its own request");
        // A resize supersedes: the span remaps to a new id and the
        // result is gone until the new reply lands.
        host.on_resize(100);
        assert!(
            host.span_lines(7, 0).is_none(),
            "a superseded span shows the raw block"
        );
        let cur = *host
            .inner
            .transform
            .lock()
            .unwrap()
            .span_reqs
            .get(&(7u64, 0u32))
            .expect("the span keeps its index key");
        host.reply_line(
            0,
            &format!(
                r#"{{"v":1,"op":"transformed","req":{cur},"lines":[["WIDE",{{}}]]}}"#,
                cur = cur
            ),
        );
        assert!(
            host.span_lines(7, 0).is_some(),
            "the re-answered span shows again"
        );
        host.stop();
    }

    #[test]
    fn status_reply_bound_marks_the_row_stale() {
        let tmp = TempDir::new().unwrap();
        // One reply, then silence: the process stays alive, but the
        // row must drop to the stale hint after 3 x tick_ms.
        let script = r#"read -r line
printf '{"v":1,"op":"status","lines":[["once",null]]}\n'
exec sleep 30
"#;
        let manifest = "[ext]\ncommand = \"bash\"\nargs = [\"stale.sh\"]\ncaps = [\"status\"]\ntick_ms = 100\nprotocol_v = 1\n";
        let host = host_with(&tmp, "stale", manifest, script);
        host.start();
        // Drive the ticks: the script answers the first one, then
        // stays silent. Two 100 ms ticks: the reply lands, and the
        // staleness clock (3 x tick_ms = 300 ms) is not yet past.
        let empty = std::collections::HashMap::new();
        for _ in 0..2 {
            let p = crate::ext::TickPayload {
                width: 80,
                session: Some("s"),
                model: None,
                thinking: 0,
                loop_running: false,
                statuses: &empty,
            };
            host.pump_ticks(&p);
            std::thread::sleep(Duration::from_millis(100));
        }
        host.poll_status();
        assert!(
            matches!(host.status_row(), StatusRow::Lines(_)),
            "the first reply shows: {:?}",
            host.status_row()
        );
        // Four more missed ticks cross the 300 ms bound: the row
        // drops to the stale hint.
        for _ in 0..4 {
            let p = crate::ext::TickPayload {
                width: 80,
                session: Some("s"),
                model: None,
                thinking: 0,
                loop_running: false,
                statuses: &empty,
            };
            host.pump_ticks(&p);
            std::thread::sleep(Duration::from_millis(100));
        }
        host.poll_status();
        assert!(
            matches!(host.status_row(), StatusRow::DeadHint(h) if h.contains("status stale")),
            "the stale hint shows: {:?}",
            host.status_row()
        );
        host.stop();
    }

    #[test]
    fn initial_status_grace_covers_a_slow_first_reply() {
        let tmp = TempDir::new().unwrap();
        // The first valid reply lands 4 s after start: past the
        // steady bound of 3 x tick_ms (3 s), but inside the 10 s
        // initial grace. A cold start is not a stuck extension, so
        // the row must not drop to the stale hint.
        let script = "read -r line\nsleep 4\nprintf '{\"v\":1,\"op\":\"status\",\"lines\":[[\"late\",null]]}\\n'\nexec sleep 30\n";
        let manifest = "[ext]\ncommand = \"bash\"\nargs = [\"late.sh\"]\ncaps = [\"status\"]\ntick_ms = 1000\nprotocol_v = 1\n";
        let host = host_with(&tmp, "late", manifest, script);
        host.start();
        let empty = std::collections::HashMap::new();
        for _ in 0..5 {
            let p = crate::ext::TickPayload {
                width: 80,
                session: Some("s"),
                model: None,
                thinking: 0,
                loop_running: false,
                statuses: &empty,
            };
            host.pump_ticks(&p);
            std::thread::sleep(Duration::from_millis(1000));
        }
        host.poll_status();
        assert!(
            matches!(host.status_row(), StatusRow::Lines(ref l) if l[0].text == "late"),
            "the late first reply shows: {:?}",
            host.status_row()
        );
        host.stop();
    }

    #[test]
    fn unchanged_status_replies_advance_the_staleness_clock() {
        let tmp = TempDir::new().unwrap();
        // The extension answers every tick with the same line. A
        // stable row must keep the extension fresh: the staleness
        // clock advances on every live reply, so the hint never
        // shows. Before the fix, the unchanged-content early return
        // skipped the clock update, and the hint fired 3 s after
        // the first reply even though the extension was alive.
        let script = r#"while IFS= read -r line; do
  case "$line" in
    *'"op":"tick"'*)
      printf '{"v":1,"op":"status","lines":[["same",null]]}\n'
      ;;
  esac
done
"#;
        let manifest = "[ext]\ncommand = \"bash\"\nargs = [\"same.sh\"]\ncaps = [\"status\"]\ntick_ms = 100\nprotocol_v = 1\n";
        let host = host_with(&tmp, "same", manifest, script);
        host.start();
        let empty = std::collections::HashMap::new();
        // Twelve 100 ms ticks: 1.2 s of identical replies. Far past
        // the 300 ms steady bound, measured from the first reply.
        for _ in 0..12 {
            let p = crate::ext::TickPayload {
                width: 80,
                session: Some("s"),
                model: None,
                thinking: 0,
                loop_running: false,
                statuses: &empty,
            };
            host.pump_ticks(&p);
            std::thread::sleep(Duration::from_millis(100));
        }
        host.poll_status();
        assert!(
            matches!(host.status_row(), StatusRow::Lines(ref l) if l[0].text == "same"),
            "a stable row keeps the extension fresh: {:?}",
            host.status_row()
        );
        host.stop();
    }

    #[test]
    fn span_request_after_timeout_re_requests() {
        let tmp = TempDir::new().unwrap();
        let manifest =
            "[ext]\ncommand = \"bash\"\nargs = [\"mm.sh\"]\ntransform = [\"fence:mermaid\"]\nprotocol_v = 1\n";
        let mut host = host_with(&tmp, "mm", manifest, "sleep 30");
        host.set_transform_timeout(Duration::from_millis(50));
        host.start();
        let req = host
            .request_span(9, 0, "fence:mermaid", "graph TD", 64)
            .unwrap();
        std::thread::sleep(Duration::from_millis(120));
        host.poll_transforms();
        assert!(
            host.span_lines(9, 0).is_none(),
            "a timed-out span shows the raw block"
        );
        // The stale entry re-requests: a new id, a fresh timer.
        let again = host.request_span(9, 0, "fence:mermaid", "graph TD", 64);
        assert!(
            again.is_some() && again != Some(req),
            "the stale span re-requests"
        );
        host.stop();
    }

    // ── history and kinds forwarding ──────────────────────────────

    /// A script that records the `type` of every forwarded event op
    /// into `$EXT_DIR/received.log`.
    const RECORD_SCRIPT: &str = r#"while IFS= read -r line; do
  case "$line" in
    *'"op":"event"'*)
      ty=$(printf '%s' "$line" | sed 's/.*"type":"\([a-z_]*\)".*/\1/')
      echo "$ty" >> "$EXT_DIR/received.log"
      ;;
  esac
done
"#;

    #[test]
    fn history_resends_visible_transcript_and_usage_only() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let global = root.join("ui_extensions");
        let entry = global.join("stat");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("stat.sh"), RECORD_SCRIPT).unwrap();
        std::fs::write(
            entry.join("ext.toml"),
            "[ext]\ncommand = \"bash\"\nargs = [\"stat.sh\"]\ncaps = [\"status\"]\nprotocol_v = 1\n",
        )
        .unwrap();
        let mut cfg = cfg_for(root);
        cfg.ext_dir = Some(global);
        let disc = discover(&cfg).unwrap();
        let host = ExtHost::new(&disc, &cfg);
        host.start();

        let usage = Event::parse_line(
            r#"{"v":1,"type":"assistant_message","ts":"t","content":"done","usage":{"input_tokens":10,"output_tokens":5}}"#,
        )
        .unwrap();
        let no_usage = Event::parse_line(
            r#"{"v":1,"type":"assistant_message","ts":"t","content":"no stats"}"#,
        )
        .unwrap();
        let user = produce::user_message("hi");
        // The status extension gets only the usage-bearing assistant
        // messages, not the user message.
        host.send_history(&[user.clone(), usage.clone(), no_usage.clone()]);
        // A long deadline: the extension is a real bash process, and
        // the spawn can be slow under a loaded, parallel test suite.
        let deadline = Instant::now() + Duration::from_millis(15000);
        loop {
            let got = std::fs::read_to_string(entry.join("received.log")).unwrap_or_default();
            if got.lines().count() == 1 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the usage resend did not arrive: {got:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let got = std::fs::read_to_string(entry.join("received.log")).unwrap();
        assert_eq!(
            got.lines().collect::<Vec<_>>(),
            vec!["assistant_message"],
            "the status extension sees exactly the usage-bearing message"
        );
        host.stop();
    }

    #[test]
    fn forward_event_respects_the_kind_filter() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let global = root.join("ui_extensions");
        let entry = global.join("only-tr");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("only-tr.sh"), RECORD_SCRIPT).unwrap();
        std::fs::write(
            entry.join("ext.toml"),
            "[ext]\ncommand = \"bash\"\nargs = [\"only-tr.sh\"]\nkinds = [\"tool_result\"]\nprotocol_v = 1\n",
        )
        .unwrap();
        let mut cfg = cfg_for(root);
        cfg.ext_dir = Some(global);
        let disc = discover(&cfg).unwrap();
        let host = ExtHost::new(&disc, &cfg);
        host.start();
        let tr = Event::parse_line(
            r#"{"v":1,"type":"tool_result","ts":"t","id":"c1","value":{"exit_code":0}}"#,
        )
        .unwrap();
        let um = produce::user_message("no forward for me");
        host.forward_event(5, &um);
        host.forward_event(6, &tr);
        // A long deadline: the extension is a real bash process, and
        // the spawn can be slow under a loaded, parallel test suite.
        let deadline = Instant::now() + Duration::from_millis(15000);
        loop {
            let got = std::fs::read_to_string(entry.join("received.log")).unwrap_or_default();
            if got.lines().count() == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
            assert!(
                Instant::now() < deadline,
                "event forwarding did not arrive: {got:?}"
            );
        }
        let got = std::fs::read_to_string(entry.join("received.log")).unwrap();
        assert_eq!(
            got.lines().collect::<Vec<_>>(),
            vec!["tool_result"],
            "the kind filter forwards only the listed kind"
        );
        host.stop();
    }

    #[test]
    fn clear_replies_resets_the_cache_and_version() {
        let tmp = TempDir::new().unwrap();
        let manifest = "[ext]\ncommand = \"bash\"\nargs = [\"x.sh\"]\nprotocol_v = 1\n";
        let host = host_with(&tmp, "x", manifest, "sleep 30");
        host.start();
        let v1 = host.replies_version();
        host.reply_line(
            0,
            "{\"v\":1,\"op\":\"lines\",\"event_id\":1,\"lines\":[[\"a\",{}]]}",
        );
        let v2 = host.replies_version();
        assert!(v2 > v1, "a reply bumps the version the transcript folds in");
        host.clear_replies();
        assert!(
            host.inner.slots[0].lines_cache.lock().unwrap().is_empty(),
            "a session switch clears the reply cache"
        );
        host.stop();
    }

    // ── ext_log file sink (FT-016) ────────────────────────────────────

    /// `ext_log` appends one `pid ms msg` line to the `TUI_EXT_LOG`
    /// file and never touches stderr (FT-016). A single test mutates
    /// the process-global `TUI_EXT_LOG` env var; both the path
    /// resolution and the file write are verified in one place to
    /// avoid a race with parallel tests.
    #[test]
    fn ext_log_writes_to_the_tui_ext_log_file() {
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("ext-host.log");
        std::env::set_var("TUI_EXT_LOG", log.as_os_str());

        // The env override wins over the default cache path.
        let p = ext_log_path().unwrap();
        assert_eq!(p, log, "TUI_EXT_LOG wins over the default cache path");

        ext_log("test-spawn");
        let body = std::fs::read_to_string(&log).unwrap();
        assert!(
            body.lines().any(|l| l.ends_with("test-spawn")),
            "the trace line lands in the file: {body:?}"
        );
        // Every line is `pid ms msg`: three space-separated fields.
        let l = body.lines().next().unwrap();
        assert!(
            l.split(' ').count() >= 3,
            "the line is pid ms msg: {l:?}"
        );

        std::env::remove_var("TUI_EXT_LOG");
    }
}
