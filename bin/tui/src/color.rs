//! Terminal color capability: detection and palette lowering.
//!
//! The TUI is built around the native 16 ANSI colors (`Color::Green`,
//! `Color::Cyan`, ...). crossterm emits those as 256-color SGR indices
//! (`38;5;0..15`), which is safe on every 16/256/truecolor terminal and
//! resolves to the ANSI swatches on each.
//!
//! Free-form RGB only appears from ui-extension hex colors
//! (`"style": {"fg": "#c3e88d"}` — docs/ui-extension.md wire color
//! grammar). Those must be lowered to what the terminal can actually
//! show:
//!
//! - `Rgb` — emit 24-bit SGR (`38;2;r;g;b`); needs a truecolor terminal.
//! - `C256` — quantize to the 6x6x6 cube + grayscale ramp
//!   (`38;5;N`).
//! - `C16` — snap to the 16 ANSI swatches (`38;5;0..15`).
//!
//! Default is `Rgb` (truecolor on). `Level::detect()` falls back to
//! what the terminal environment says: `COLORTERM=truecolor|24bit`
//! forces `Rgb`; a `TERM` advertising 256 colors (`*256*`) drops to
//! `C256`; a plain-16 TERM (`xterm`, `vt100`, `linux`, `ansi`,
//! `screen`, `tmux`, `dumb`) drops to `C16`; anything else (alacritty,
//! kitty, `xterm-256color` absent) keeps the `Rgb` default. The
//! harness config `[tui] color` overrides the detection (the user
//! knows their terminal).

use ratatui::style::{Color, Modifier, Style};

/// The terminal's color capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    /// 24-bit truecolor: RGB colors emit as-is (`38;2`).
    Rgb,
    /// 256-color: RGB colors quantize to the 256 palette.
    C256,
    /// 16-color: RGB colors snap to the 16 ANSI swatches.
    C16,
}

impl Level {
    /// The default: truecolor on.
    pub const DEFAULT: Level = Level::Rgb;

    /// Detect from the environment: `COLORTERM` first, then `TERM`
    /// (see the module docs).
    pub fn detect() -> Level {
        Self::detect_with(
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        )
    }

    /// The pure decision table (env var access split out for tests).
    fn detect_with(colorterm: Option<&str>, term: Option<&str>) -> Level {
        let colorterm_lc = colorterm.map(|s| s.to_ascii_lowercase());
        if matches!(colorterm_lc.as_deref(), Some("truecolor") | Some("24bit")) {
            return Level::Rgb;
        }
        let term = term.unwrap_or_default().to_ascii_lowercase();
        if term.contains("256") {
            return Level::C256;
        }
        const PLAIN16: [&str; 8] = [
            "xterm", "vt100", "vt102", "linux", "ansi", "screen", "tmux", "dumb",
        ];
        if PLAIN16.contains(&term.as_str()) {
            return Level::C16;
        }
        Self::DEFAULT
    }

    /// The harness config override (`[tui] color = "truecolor"`).
    pub fn from_cfg(v: &str) -> Option<Level> {
        Some(match v.to_ascii_lowercase().as_str() {
            "rgb" | "truecolor" | "24bit" | "24-bit" => Level::Rgb,
            "256" | "256color" | "256-color" => Level::C256,
            "16" | "8" | "16color" | "8color" | "16-color" | "8-color" => Level::C16,
            _ => return None,
        })
    }

    /// The wire name the status row advertises: what the TUI is
    /// actually emitting.
    pub fn name(self) -> &'static str {
        match self {
            Level::Rgb => "truecolor",
            Level::C256 => "256",
            Level::C16 => "16",
        }
    }

    /// The color for unstyled transcript prose (model messages, user
    /// message bodies, the input draft). Truecolor gets a soft light
    /// gray that reads as "default text" on a dark theme without
    /// colliding with any named role color; 256-color quantizes that
    /// same target through the 256-palette; 16-color falls back to the
    /// `Gray` swatch (xterm 7, `#c0c0c0`).
    pub fn plain_text(self) -> Color {
        match self {
            Level::Rgb => Color::Rgb(212, 212, 212),
            Level::C256 => lower(Color::Rgb(212, 212, 212), Level::C256),
            Level::C16 => Color::Gray,
        }
    }

    /// The color for tool/command output (bash and tool-result bodies).
    /// Chosen distinct from [`Level::plain_text`] at every level: a
    /// muted mauve in truecolor so results no longer read as the same
    /// gray as prose.
    pub fn tool_output(self) -> Color {
        match self {
            Level::Rgb => Color::Rgb(191, 181, 205),
            Level::C256 => lower(Color::Rgb(191, 181, 205), Level::C256),
            Level::C16 => Color::DarkGray,
        }
    }

    /// The color for the *command* text of a tool call — what was
    /// invoked (a bash line, a tool name + args), as opposed to the
    /// result it produced ([`Level::tool_output`]). Chosen lighter
    /// than the result at every level so a command and its output read
    /// as two different voices, not one repeated gray. Distinct from
    /// both [`Level::plain_text`] and [`Level::tool_output`]: a light
    /// blue in truecolor, the brightest blue swatch in 16-color.
    pub fn tool_command(self) -> Color {
        match self {
            Level::Rgb => Color::Rgb(170, 200, 240),
            Level::C256 => lower(Color::Rgb(170, 200, 240), Level::C256),
            Level::C16 => Color::LightBlue,
        }
    }

    /// The color for the model's thinking (reasoning) block. pi renders
    /// thinking in its `subtext1` tone (catppuccin-macchiato `#b8c0e0`),
    /// a light readable blue, not a dim gray. Aligned to that: a light
    /// lavender-blue in truecolor, distinct from prose and tool voices.
    pub fn thinking(self) -> Color {
        match self {
            Level::Rgb => Color::Rgb(184, 192, 224),
            Level::C256 => lower(Color::Rgb(184, 192, 224), Level::C256),
            Level::C16 => Color::LightCyan,
        }
    }
}

/// Lower one color to `level`.
///
/// Native named colors (the 16 ANSI swatches) pass through unchanged:
/// crossterm emits them as `38;5;0..15`-style SGR, which is valid at
/// every level and resolves to the ANSI swatches. Only free-form RGB
/// needs work, and only downward.
pub fn lower(c: Color, level: Level) -> Color {
    match (c, level) {
        (Color::Rgb(..), Level::Rgb) => c,
        (Color::Rgb(r, g, b), Level::C256) => Color::Indexed(nearest_256(r, g, b)),
        (Color::Rgb(r, g, b), Level::C16) => nearest_16(r, g, b),
        _ => c,
    }
}

/// Lower the colors of a whole style (fg + bg); modifiers stay.
pub fn lower_style(s: Style, level: Level) -> Style {
    Style {
        fg: s.fg.map(|c| lower(c, level)),
        bg: s.bg.map(|c| lower(c, level)),
        ..s
    }
}

// ── color schemes (docs/tui-color-scheme.md) ──────────────────

/// One color role the TUI paints. The role list is the open design
/// question of docs/tui-color-scheme.md section 4, answered: the
/// three built-in tones, the markdown and JSON highlight styles, the
/// thinking-level border palette, the built-in status row, the
/// thinking block, the fold/expand hints, the error and success
/// accents, and the tool-result box background.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    /// Transcript prose (model messages, user message bodies, the
    /// input draft).
    PlainText,
    /// Tool and command output bodies.
    ToolOutput,
    /// The command text of a tool call.
    ToolCommand,
    /// Fenced-code content.
    Code,
    /// A ``` / ~~~ fence marker line.
    Fence,
    /// A markdown heading line (the marker run drops; the style
    /// stays).
    Heading,
    /// A blockquote line.
    Quote,
    /// A list marker (`-`, `1.`).
    List,
    /// An inline `code` span (the backticks drop; the style stays).
    InlineCode,
    /// The text of a markdown link.
    Link,
    /// The URL of a markdown link (dimmed).
    LinkUrl,
    /// A JSON object key.
    JsonKey,
    /// A JSON string value.
    JsonString,
    /// A JSON number.
    JsonNumber,
    /// `true` / `false`.
    JsonLiteral,
    /// `null`.
    JsonNull,
    /// JSON structural punctuation.
    JsonPunct,
    /// The model thinking block (dimmed).
    Thinking,
    /// Fold/expand hints and other muted text (`… +N more lines`).
    Hint,
    /// The built-in status/help row and the last-loop-line row.
    Status,
    /// Error accents (error status, failed lines).
    Error,
    /// Success accents (allow decisions, added diff lines).
    Success,
    /// The input-area border of thinking level 0 (no thinking).
    Border0,
    /// The input-area border of thinking level 1 (low).
    Border1,
    /// The input-area border of thinking level 2 (medium).
    Border2,
    /// The input-area border of thinking level 3 (high).
    Border3,
    /// The input-area border of thinking level 4+ (highest).
    Border4,
    /// The light background of a tool-result box.
    ToolBoxBg,
}

impl Role {
    /// Every role, in declaration order. A scheme may leave any role
    /// unset: the unset roles keep the built-in palette value.
    pub const ALL: &[Role] = &[
        Role::PlainText,
        Role::ToolOutput,
        Role::ToolCommand,
        Role::Code,
        Role::Fence,
        Role::Heading,
        Role::Quote,
        Role::List,
        Role::InlineCode,
        Role::Link,
        Role::LinkUrl,
        Role::JsonKey,
        Role::JsonString,
        Role::JsonNumber,
        Role::JsonLiteral,
        Role::JsonNull,
        Role::JsonPunct,
        Role::Thinking,
        Role::Hint,
        Role::Status,
        Role::Error,
        Role::Success,
        Role::Border0,
        Role::Border1,
        Role::Border2,
        Role::Border3,
        Role::Border4,
        Role::ToolBoxBg,
    ];

    /// The built-in value of the role at every capability level. The
    /// default palette (no scheme selected) is this table, lowered:
    /// the named 16-color roles pass through `lower` unchanged; the
    /// truecolor targets quantize at 256 and snap at 16.
    pub fn builtin(self, level: Level) -> Color {
        use Role::*;
        let c = match self {
            PlainText => level.plain_text(),
            ToolOutput => level.tool_output(),
            ToolCommand => level.tool_command(),
            Code | JsonKey | Success => Color::Green,
            Fence | Link => Color::Cyan,
            Heading | JsonLiteral => Color::Blue,
            Quote | JsonNull | JsonPunct | Hint | Status => Color::DarkGray,
            Thinking => level.thinking(),
            List | InlineCode | JsonString => Color::Yellow,
            JsonNumber | Error => Color::Red,
            Border0 => Color::DarkGray,
            Border1 => Color::Blue,
            Border2 => Color::Cyan,
            Border3 => Color::Green,
            Border4 => Color::Yellow,
            ToolBoxBg => Color::Rgb(30, 32, 48),
            LinkUrl => Color::DarkGray,
        };
        lower(c, level)
    }

    /// The wire name of the role in the config scheme table
    /// (`[tui] color_schemes.<name>`).
    pub fn key(self) -> &'static str {
        use Role::*;
        match self {
            PlainText => "plain_text",
            ToolOutput => "tool_output",
            ToolCommand => "tool_command",
            Code => "code",
            Fence => "fence",
            Heading => "heading",
            Quote => "quote",
            List => "list",
            InlineCode => "inline_code",
            Link => "link",
            LinkUrl => "link_url",
            JsonKey => "json_key",
            JsonString => "json_string",
            JsonNumber => "json_number",
            JsonLiteral => "json_literal",
            JsonNull => "json_null",
            JsonPunct => "json_punct",
            Thinking => "thinking",
            Hint => "hint",
            Status => "status",
            Error => "error",
            Success => "success",
            Border0 => "border0",
            Border1 => "border1",
            Border2 => "border2",
            Border3 => "border3",
            Border4 => "border4",
            ToolBoxBg => "tool_box_bg",
        }
    }
}

/// Parse one hex color value (`#rgb` or `#rrggbb`) of a scheme
/// table. The same grammar as the extension hex wire colors
/// (ext.rs `parse_color`); a bad value is a hard error at load, like
/// the `[tui] color` level.
pub fn parse_scheme_color(v: &str) -> Option<Color> {
    let hex = v.trim().strip_prefix('#')?;
    if !hex.is_ascii() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |s: &str| u8::from_str_radix(s, 16).ok();
    match hex.len() {
        3 => {
            let r = byte(&hex[0..1])? * 17;
            let g = byte(&hex[1..2])? * 17;
            let b = byte(&hex[2..3])? * 17;
            Some(Color::Rgb(r, g, b))
        }
        6 => {
            let r = byte(&hex[0..2])?;
            let g = byte(&hex[2..4])?;
            let b = byte(&hex[4..6])?;
            Some(Color::Rgb(r, g, b))
        }
        _ => None,
    }
}

/// The built-in named scheme. The first internal scheme is
/// `catppuccin macchiato` (docs/tui-color-scheme.md section 3):
/// every role maps to a Macchiato hex value. The values lower to
/// the active capability level, like the extension hex wire colors.
pub const SCHEME_CATPPUCCIN_MACCHIATO: &str = "catppuccin macchiato";

/// The role-to-hex table of the `catppuccin macchiato` scheme
/// (docs/tui-color-scheme.md section 3). A scheme maps every color
/// role to a hex value; the TUI ships this one as the first named
/// internal scheme.
pub fn catppuccin_macchiato() -> std::collections::HashMap<Role, &'static str> {
    use Role::*;
    let pairs: [(Role, &str); 28] = [
        (PlainText, "#cdd6f4"),
        (ToolOutput, "#8f92ac"),
        (ToolCommand, "#89dceb"),
        (Code, "#8bd5ca"),
        (Fence, "#7dcfff"),
        (Heading, "#babcfc"),
        (Quote, "#7c7f96"),
        (List, "#fab387"),
        (InlineCode, "#f0c674"),
        (Link, "#74c7ec"),
        (LinkUrl, "#7c7f96"),
        (JsonKey, "#a6e3a1"),
        (JsonString, "#f0c674"),
        (JsonNumber, "#fab387"),
        (JsonLiteral, "#babcfc"),
        (JsonNull, "#7c7f96"),
        (JsonPunct, "#515676"),
        (Thinking, "#b8c0e0"),
        (Hint, "#7c7f96"),
        (Status, "#7c7f96"),
        (Error, "#ed8796"),
        (Success, "#a6e3a1"),
        (Border0, "#7c7f96"),
        (Border1, "#74c7ec"),
        (Border2, "#89dceb"),
        (Border3, "#a6e3a1"),
        (Border4, "#f0c674"),
        (ToolBoxBg, "#363a4f"),
    ];
    pairs.iter().cloned().collect()
}

/// The resolved palette of one TUI run: every role lowered to the
/// active capability level. The `builtin` constructor is the current
/// palette (docs/tui-color-tones.md section 3: three capability-
/// aware tones plus the 16-color-safe highlight styles). A scheme
/// constructor maps roles to hex values and lowers them; an unset
/// role keeps the built-in value of the level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    level: Level,
    colors: std::collections::HashMap<Role, Color>,
}

impl Palette {
    /// The built-in palette of `level`: no scheme selected.
    pub fn builtin(level: Level) -> Self {
        let mut colors = std::collections::HashMap::new();
        for role in Role::ALL {
            colors.insert(*role, role.builtin(level));
        }
        Palette { level, colors }
    }

    /// The palette of a named internal scheme at `level`. `None`
    /// when the name is not a built-in scheme.
    pub fn named(level: Level, name: &str) -> Option<Self> {
        let hexes: &std::collections::HashMap<Role, &'static str> = match name {
            SCHEME_CATPPUCCIN_MACCHIATO => &catppuccin_macchiato(),
            _ => return None,
        };
        let hexes: std::collections::HashMap<Role, String> =
            hexes.iter().map(|(r, h)| (*r, h.to_string())).collect();
        Some(Self::custom(level, &hexes))
    }

    /// The palette of a user-supplied role-to-hex table at `level`
    /// (docs/tui-color-scheme.md section 3: custom scheme input).
    /// Every value is a scheme hex (`#rgb`, `#rrggbb`), validated by
    /// the config layer before this call. An unset role keeps the
    /// built-in value of the level: a partial table overlays the
    /// built-in palette.
    pub fn custom(level: Level, hexes: &std::collections::HashMap<Role, String>) -> Self {
        let mut colors = std::collections::HashMap::new();
        for role in Role::ALL {
            let c = match hexes.get(role) {
                Some(h) => {
                    let c = parse_scheme_color(h)
                        .expect("the config layer validates the scheme hex values");
                    lower(c, level)
                }
                None => role.builtin(level),
            };
            colors.insert(*role, c);
        }
        Palette { level, colors }
    }

    /// The capability level the palette lowers to.
    pub fn level(&self) -> Level {
        self.level
    }

    /// The lowered color of one role.
    pub fn color(&self, role: Role) -> Color {
        self.colors[&role]
    }

    /// The style of one role: the color plus extra modifiers.
    pub fn style(&self, role: Role, mods: Modifier) -> Style {
        Style::default().fg(self.color(role)).add_modifier(mods)
    }

    /// The thinking-border color of a level (0-4): one palette
    /// lookup instead of a match table in the render code.
    pub fn thinking_border(&self, level: u32) -> Color {
        use Role::*;
        match level {
            0 => self.color(Border0),
            1 => self.color(Border1),
            2 => self.color(Border2),
            3 => self.color(Border3),
            _ => self.color(Border4),
        }
    }
}

/// The palette of the loaded TUI config (docs/tui-color-scheme.md
/// section 3, the scheme switch point: config load). `None` scheme
/// keeps the built-in palette of `level`. A named scheme is a
/// built-in name or a user table; the values lower to `level`, and a
/// bad hex is a hard error like the `[tui] color` level.
pub fn palette_from_config(
    level: Level,
    scheme: Option<&str>,
    custom: &std::collections::HashMap<String, std::collections::HashMap<Role, String>>,
) -> Result<Palette, String> {
    let Some(name) = scheme else {
        return Ok(Palette::builtin(level));
    };
    if name == SCHEME_CATPPUCCIN_MACCHIATO {
        return Ok(Palette::named(level, SCHEME_CATPPUCCIN_MACCHIATO)
            .unwrap_or_else(|| Palette::builtin(level)));
    }
    let table = custom.get(name).ok_or_else(|| {
        format!(
            "color scheme {name:?} is not a built-in scheme \
             (expected {SCHEME_CATPPUCCIN_MACCHIATO}) and has no [tui] color_schemes table"
        )
    })?;
    Ok(Palette::custom(level, table))
}

/// The xterm 256-palette: indices 0-15 (the ANSI swatches, `#c0c0c0`
/// for 7 and `#808080` for 8 per xterm), 16-231 the 6x6x6 cube with
/// channel values `{55,95,135,175,215,255}`, 232-255 the grayscale
/// ramp `8 + 10n`.
fn palette256(idx: u8) -> (u8, u8, u8) {
    let cube = |v: u8| match v {
        0 => 0,
        1 => 95,
        2 => 135,
        3 => 175,
        4 => 215,
        _ => 255,
    };
    match idx {
        0..=15 => {
            const V: [(u8, u8, u8); 16] = [
                (0, 0, 0),
                (128, 0, 0),
                (0, 128, 0),
                (128, 128, 0),
                (0, 0, 128),
                (128, 0, 128),
                (0, 128, 128),
                (192, 192, 192),
                (128, 128, 128),
                (255, 0, 0),
                (0, 255, 0),
                (255, 255, 0),
                (0, 0, 255),
                (255, 0, 255),
                (0, 255, 255),
                (255, 255, 255),
            ];
            V[idx as usize]
        }
        16..=231 => {
            let i = idx as u32 - 16;
            (
                cube((i / 36) as u8),
                cube(((i / 6) % 6) as u8),
                cube((i % 6) as u8),
            )
        }
        _ => {
            let n = idx as u32 - 232;
            let v = (8 + 10 * n) as u8;
            (v, v, v)
        }
    }
}

/// Nearest index in the 256 palette.
///
/// Free-form RGB is quantized into the 6x6x6 cube (16-231) and the
/// grayscale ramp (232-255); the basic 0-15 swatches are reserved for
/// the native ANSI colors, so a true-color value never collapses onto
/// a basic swatch even when one happens to be an exact match. Ties
/// keep the lower index. Exact ramp values (the 24 grays) win with a
/// distance of 0, since no cube corner coincides with them.
fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    let mut best: (u8, u32) = (16, u32::MAX);
    for idx in 16u32..256u32 {
        let (pr, pg, pb) = palette256(idx as u8);
        let d = dist(r, g, b, pr, pg, pb);
        // A strict improvement: ties keep the earlier (lower) index.
        if d < best.1 {
            best = (idx as u8, d);
        }
    }
    best.0
}

/// The 16 ANSI swatches (xterm's 0-15) for `C16` lowering.
fn swatch16(i: u8) -> (u8, u8, u8) {
    palette256(i)
}

/// Nearest of the 16 ANSI swatches (ties: the lower index).
fn nearest_16(r: u8, g: u8, b: u8) -> Color {
    let mut best: (u8, u32) = (0, u32::MAX);
    for i in 0..16u32 {
        let (pr, pg, pb) = swatch16(i as u8);
        let d = dist(r, g, b, pr, pg, pb);
        if d < best.1 {
            best = (i as u8, d);
        }
    }
    match best.0 {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        _ => Color::White,
    }
}

fn dist(r: u8, g: u8, b: u8, pr: u8, pg: u8, pb: u8) -> u32 {
    let dr = r as i32 - pr as i32;
    let dg = g as i32 - pg as i32;
    let db = b as i32 - pb as i32;
    // Squared Euclidean distance; the sum of squares is non-negative.
    (dr * dr + dg * dg + db * db) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_table() {
        assert_eq!(Level::detect_with(Some("truecolor"), None), Level::Rgb);
        assert_eq!(Level::detect_with(Some("24bit"), None), Level::Rgb);
        // COLORTERM without truecolor/24bit says nothing usable: the
        // TERM fallback decides.
        assert_eq!(Level::detect_with(Some("dumb"), Some("xterm")), Level::C16);
        assert_eq!(
            Level::detect_with(None, Some("xterm-256color")),
            Level::C256
        );
        assert_eq!(Level::detect_with(None, Some("st-256color")), Level::C256);
        assert_eq!(Level::detect_with(None, Some("xterm")), Level::C16);
        assert_eq!(Level::detect_with(None, Some("screen")), Level::C16);
        assert_eq!(Level::detect_with(None, Some("tmux")), Level::C16);
        assert_eq!(Level::detect_with(None, Some("vt100")), Level::C16);
        assert_eq!(Level::detect_with(None, Some("linux")), Level::C16);
        assert_eq!(Level::detect_with(None, Some("ansi")), Level::C16);
        assert_eq!(Level::detect_with(None, Some("dumb")), Level::C16);
        // Unknown modern terms keep the truecolor default.
        assert_eq!(Level::detect_with(None, Some("alacritty")), Level::Rgb);
        assert_eq!(Level::detect_with(None, Some("kitty")), Level::Rgb);
        assert_eq!(Level::detect_with(None, None), Level::Rgb);
        // Case-insensitive.
        assert_eq!(
            Level::detect_with(None, Some("XTERM-256COLOR")),
            Level::C256
        );
        assert_eq!(Level::detect_with(Some("TrueColor"), None), Level::Rgb);
    }

    #[test]
    fn cfg_override_table() {
        assert_eq!(Level::from_cfg("truecolor"), Some(Level::Rgb));
        assert_eq!(Level::from_cfg("rgb"), Some(Level::Rgb));
        assert_eq!(Level::from_cfg("24bit"), Some(Level::Rgb));
        assert_eq!(Level::from_cfg("256"), Some(Level::C256));
        assert_eq!(Level::from_cfg("256color"), Some(Level::C256));
        assert_eq!(Level::from_cfg("16"), Some(Level::C16));
        assert_eq!(Level::from_cfg("8"), Some(Level::C16));
        assert_eq!(Level::from_cfg("16color"), Some(Level::C16));
        assert_eq!(Level::from_cfg("bogus"), None);
    }

    #[test]
    fn level_names() {
        assert_eq!(Level::Rgb.name(), "truecolor");
        assert_eq!(Level::C256.name(), "256");
        assert_eq!(Level::C16.name(), "16");
    }

    #[test]
    fn lowering_passes_named_colors_through() {
        for level in [Level::Rgb, Level::C256, Level::C16] {
            assert_eq!(lower(Color::Green, level), Color::Green);
            assert_eq!(lower(Color::DarkGray, level), Color::DarkGray);
            assert_eq!(lower(Color::Indexed(99), level), Color::Indexed(99));
            assert_eq!(lower(Color::Reset, level), Color::Reset);
        }
    }

    #[test]
    fn lowering_keeps_rgb_at_truecolor() {
        assert_eq!(
            lower(Color::Rgb(0x24, 0x27, 0x3a), Level::Rgb),
            Color::Rgb(0x24, 0x27, 0x3a)
        );
    }

    #[test]
    fn lowering_quantizes_rgb_at_256() {
        assert_eq!(
            lower(Color::Rgb(255, 0, 0), Level::C256),
            Color::Indexed(196)
        );
        assert_eq!(
            lower(Color::Rgb(0, 255, 255), Level::C256),
            Color::Indexed(51)
        );
        assert_eq!(
            lower(Color::Rgb(255, 255, 255), Level::C256),
            Color::Indexed(231)
        );
        assert_eq!(lower(Color::Rgb(0, 0, 0), Level::C256), Color::Indexed(16));
        // Near-gray hits the ramp, not the cube.
        assert_eq!(
            lower(Color::Rgb(10, 10, 10), Level::C256),
            Color::Indexed(232)
        );
        // Mid gray: ramp 118 (idx 243, dist^2=48) beats cube 135 (idx 145,
        // dist^2=507).
        assert_eq!(
            lower(Color::Rgb(122, 122, 122), Level::C256),
            Color::Indexed(243)
        );
    }

    #[test]
    fn lowering_snaps_rgb_at_16() {
        assert_eq!(lower(Color::Rgb(0, 0, 0), Level::C16), Color::Black);
        assert_eq!(lower(Color::Rgb(255, 0, 0), Level::C16), Color::LightRed);
        assert_eq!(lower(Color::Rgb(255, 255, 255), Level::C16), Color::White);
        assert_eq!(
            lower(Color::Rgb(255, 255, 0), Level::C16),
            Color::LightYellow
        );
        // Mid-gray lands on the dark swatch (128,128,128).
        assert_eq!(
            lower(Color::Rgb(128, 128, 128), Level::C16),
            Color::DarkGray
        );
    }

    #[test]
    fn lowering_styles() {
        let s = Style::default()
            .fg(Color::Rgb(255, 255, 255))
            .bg(Color::Rgb(0, 0, 0))
            .bold();
        let s16 = lower_style(s, Level::C16);
        assert_eq!(s16.fg, Some(Color::White));
        assert_eq!(s16.bg, Some(Color::Black));
        assert!(s16.add_modifier.contains(ratatui::style::Modifier::BOLD));
    }

    #[test]
    fn palette256_ramp() {
        assert_eq!(palette256(232), (8, 8, 8));
        assert_eq!(palette256(255), (238, 238, 238));
        assert_eq!(palette256(16), (0, 0, 0));
        assert_eq!(palette256(231), (255, 255, 255));
        assert_eq!(palette256(196), (255, 0, 0));
        assert_eq!(palette256(59), (95, 95, 95)); // cube (1,1,1)
    }

    #[test]
    fn plain_text_palette() {
        // Transcript prose / the input draft. Truecolor target is a soft
        // light gray that reads as "default text", not a saturated swatch.
        assert_eq!(Level::Rgb.plain_text(), Color::Rgb(212, 212, 212));
        // 16-color lands on the light-gray swatch (xterm 7), never the dim one.
        assert_eq!(Level::C16.plain_text(), Color::Gray);
        // 256-color quantizes the same gray target to a 256-palette index.
        assert!(matches!(Level::C256.plain_text(), Color::Indexed(..)));
    }

    #[test]
    fn tool_output_palette() {
        // Tool/command output: a muted mauve, distinct from prose at every
        // level so bash results are visually separable from plain text.
        assert_eq!(Level::Rgb.tool_output(), Color::Rgb(191, 181, 205));
        assert_eq!(Level::C16.tool_output(), Color::DarkGray);
        assert!(matches!(Level::C256.tool_output(), Color::Indexed(..)));
        for lvl in [Level::Rgb, Level::C256, Level::C16] {
            assert_ne!(lvl.plain_text(), lvl.tool_output());
        }
    }

    #[test]
    fn tool_command_palette() {
        // The command text of a tool call: a light blue, lighter than the
        // result (tool_output) so the two read as different voices.
        assert_eq!(Level::Rgb.tool_command(), Color::Rgb(170, 200, 240));
        assert_eq!(Level::C16.tool_command(), Color::LightBlue);
        assert!(matches!(Level::C256.tool_command(), Color::Indexed(..)));
        for lvl in [Level::Rgb, Level::C256, Level::C16] {
            // Command is its own voice: distinct from prose and from
            // the result body at every level.
            assert_ne!(lvl.plain_text(), lvl.tool_command());
            assert_ne!(lvl.tool_output(), lvl.tool_command());
        }
    }

    // ── color schemes (docs/tui-color-scheme.md) ────────────

    #[test]
    fn builtin_palette_keeps_the_current_palette() {
        // The no-scheme palette is the pre-scheme behavior at every
        // level: the three tones and the named 16-color roles.
        let p = Palette::builtin(Level::C16);
        assert_eq!(p.color(Role::PlainText), Color::Gray);
        assert_eq!(p.color(Role::ToolOutput), Color::DarkGray);
        assert_eq!(p.color(Role::ToolCommand), Color::LightBlue);
        assert_eq!(p.color(Role::Error), Color::Red);
        assert_eq!(p.color(Role::Border0), Color::DarkGray);
        assert_eq!(p.color(Role::Border4), Color::Yellow);
        // The box background snaps to a 16-color swatch at C16.
        assert!(matches!(
            p.color(Role::ToolBoxBg),
            Color::DarkGray | Color::Black
        ));
        let p = Palette::builtin(Level::Rgb);
        assert_eq!(p.color(Role::ToolBoxBg), Color::Rgb(30, 32, 48));
        assert_eq!(p.level(), Level::Rgb);
    }

    #[test]
    fn builtin_role_table_is_complete() {
        // Every role has a built-in value at every level: a new role
        // that misses the table fails here, not at render time.
        for level in [Level::Rgb, Level::C256, Level::C16] {
            assert_eq!(Role::ALL.len(), 28, "the role list grows: update the table");
            for role in Role::ALL {
                let _ = role.builtin(level);
            }
        }
    }

    #[test]
    fn scheme_hex_parsing() {
        assert_eq!(
            parse_scheme_color("#8f92ac"),
            Some(Color::Rgb(0x8f, 0x92, 0xac))
        );
        assert_eq!(
            parse_scheme_color("#abc"),
            Some(Color::Rgb(0xaa, 0xbb, 0xcc))
        );
        assert_eq!(
            parse_scheme_color("#8f92ac "),
            Some(Color::Rgb(0x8f, 0x92, 0xac))
        );
        assert_eq!(parse_scheme_color("8f92ac"), None, "the # is required");
        assert_eq!(parse_scheme_color("#12345"), None);
        assert_eq!(parse_scheme_color("#1234567"), None);
        assert_eq!(parse_scheme_color("#zzzzzz"), None);
        assert_eq!(parse_scheme_color(""), None);
    }

    #[test]
    fn macchiato_scheme_maps_every_role() {
        let table = catppuccin_macchiato();
        assert_eq!(
            table.len(),
            Role::ALL.len(),
            "every role has a Macchiato hex"
        );
        for role in Role::ALL {
            let hex = table.get(role).expect("role has a hex");
            assert!(
                parse_scheme_color(hex).is_some(),
                "bad hex for {role:?}: {hex}"
            );
        }
        // The documented mapping (docs/tui-color-scheme.md section 3):
        // the Macchiato base colors, role to hex.
        assert_eq!(table[&Role::PlainText], "#cdd6f4", "text");
        assert_eq!(table[&Role::Error], "#ed8796", "red");
        assert_eq!(table[&Role::Success], "#a6e3a1", "green");
        assert_eq!(table[&Role::ToolBoxBg], "#363a4f", "surface1");
    }

    #[test]
    fn named_scheme_resolves_at_the_level() {
        let p = Palette::named(Level::Rgb, SCHEME_CATPPUCCIN_MACCHIATO)
            .expect("the built-in scheme resolves");
        assert_eq!(p.color(Role::PlainText), Color::Rgb(0xcd, 0xd6, 0xf4));
        assert_eq!(p.color(Role::Heading), Color::Rgb(0xba, 0xbc, 0xfc));
        // 256-color lowers the same hex to an index.
        let p = Palette::named(Level::C256, SCHEME_CATPPUCCIN_MACCHIATO).unwrap();
        assert!(matches!(p.color(Role::PlainText), Color::Indexed(..)));
        // 16-color snaps to the nearest swatch.
        let p = Palette::named(Level::C16, SCHEME_CATPPUCCIN_MACCHIATO).unwrap();
        // #cdd6f4 snaps to the nearest named swatch (gray, the
        // closest to the light swatches; never the terminal
        // default).
        assert!(
            matches!(
                p.color(Role::PlainText),
                Color::Gray | Color::White | Color::LightCyan | Color::Cyan
            ),
            "got {:?}",
            p.color(Role::PlainText)
        );
        assert!(Palette::named(Level::Rgb, "no such scheme").is_none());
    }

    #[test]
    fn custom_scheme_overlays_the_builtins() {
        // A partial table overlays the built-in palette: the unset
        // roles keep their built-in values at the level.
        let mut hexes = std::collections::HashMap::new();
        hexes.insert(Role::PlainText, "#123456".to_string());
        hexes.insert(Role::Error, "#654321".to_string());
        let p = Palette::custom(Level::Rgb, &hexes);
        assert_eq!(p.color(Role::PlainText), Color::Rgb(0x12, 0x34, 0x56));
        assert_eq!(p.color(Role::Error), Color::Rgb(0x65, 0x43, 0x21));
        // Unset roles keep the built-in value of the level.
        assert_eq!(p.color(Role::Success), Role::Success.builtin(Level::Rgb));
        // The full custom table reaches every role.
        let mut full: std::collections::HashMap<Role, String> = Role::ALL
            .iter()
            .map(|r| (*r, "#abcdef".to_string()))
            .collect();
        full.insert(Role::PlainText, "#000001".to_string());
        let p = Palette::custom(Level::Rgb, &full);
        assert_eq!(p.color(Role::PlainText), Color::Rgb(0, 0, 1));
        assert_eq!(p.color(Role::JsonPunct), Color::Rgb(0xab, 0xcd, 0xef));
    }

    #[test]
    fn palette_thinking_border_tracks_the_levels() {
        let p = Palette::named(Level::Rgb, SCHEME_CATPPUCCIN_MACCHIATO).unwrap();
        // The border palette of the Macchiato scheme: blue, sky,
        // green, yellow, on the subtle gray base.
        assert_eq!(p.thinking_border(0), p.color(Role::Border0));
        assert_eq!(p.thinking_border(4), p.color(Role::Border4));
        assert_eq!(p.thinking_border(9), p.color(Role::Border4));
    }
}
