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
//!
//! The built-in palette and the `catppuccin macchiato` scheme are the
//! element colors of the `pi` TUI (docs/tui-color-pi-alignment.md):
//! the no-scheme palette mirrors pi's built-in `dark` theme, and the
//! scheme mirrors the pi `catppuccin-macchiato` theme JSON
//! (`~/.pi/agent/themes` lookup). One color role per pi theme token
//! the TUI paints, so a scheme table can carry the pi theme values
//! verbatim.

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
    /// message bodies, the input draft). The pi built-in `dark` theme
    /// text var (`#d4d4d4`): 16-color snaps to the `Gray` swatch
    /// (xterm 7, `#c0c0c0`).
    pub fn plain_text(self) -> Color {
        match self {
            Level::Rgb => Color::Rgb(0xd4, 0xd4, 0xd4),
            Level::C256 => lower(Color::Rgb(0xd4, 0xd4, 0xd4), Level::C256),
            Level::C16 => Color::Gray,
        }
    }

    /// The color for tool/command output (bash and tool-result bodies).
    /// The pi `toolOutput` role (`#808080` gray in the built-in `dark`
    /// theme): distinct from the prose tone, the 16-color `DarkGray`
    /// swatch exactly.
    pub fn tool_output(self) -> Color {
        match self {
            Level::Rgb => Color::Rgb(0x80, 0x80, 0x80),
            Level::C256 => lower(Color::Rgb(0x80, 0x80, 0x80), Level::C256),
            Level::C16 => Color::DarkGray,
        }
    }

    /// The color for the *command* text of a tool call — what was
    /// invoked (a bash line, a tool name + args). The pi `toolTitle`
    /// role: the `dark` theme text var (`#d4d4d4`), the same tone as
    /// the prose. The `catppuccin macchiato` scheme lifts it to the
    /// theme `mauve`, where title and prose read distinct.
    pub fn tool_command(self) -> Color {
        match self {
            Level::Rgb => Color::Rgb(0xd4, 0xd4, 0xd4),
            Level::C256 => lower(Color::Rgb(0xd4, 0xd4, 0xd4), Level::C256),
            Level::C16 => Color::Gray,
        }
    }

    /// The color for the model's thinking (reasoning) block. The pi
    /// `thinkingText` role: the `dark` theme gray (`#808080`), the
    /// `catppuccin macchiato` scheme the theme `subtext1` (`#b8c0e0`).
    pub fn thinking(self) -> Color {
        match self {
            Level::Rgb => Color::Rgb(0x80, 0x80, 0x80),
            Level::C256 => lower(Color::Rgb(0x80, 0x80, 0x80), Level::C256),
            Level::C16 => Color::DarkGray,
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

/// One color role the TUI paints. Each role is one `pi` theme token
/// the TUI element consumes (docs/tui-color-pi-alignment.md):
/// the built-in tones, the markdown and syntax highlight styles
/// (the pi `syntax*` token set, which the JSON token walk colors
/// through, like pi's highlight.js JSON scope mapping), the
/// thinking-level border palette, the status row, the diff accents,
/// the warning accent, and the tool-result box backgrounds (one per
/// pi box state).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    /// Transcript prose (model messages, user message bodies, the
    /// input draft). pi `text` / `userMessageText`.
    PlainText,
    /// Tool and command output bodies. pi `toolOutput`.
    ToolOutput,
    /// The command text of a tool call. pi `toolTitle`.
    ToolCommand,
    /// The pi accent: the `user` message label, the tool-call path
    /// and the list bullets of the `catppuccin macchiato` theme.
    /// pi `accent`.
    Accent,
    /// Fenced-code content (the known-language fallback tone). pi
    /// `mdCodeBlock`.
    Code,
    /// A ``` / ~~~ fence marker line. pi `mdCodeBlockBorder`.
    Fence,
    /// A markdown heading line (the marker run drops; the style
    /// stays). pi `mdHeading`.
    Heading,
    /// A blockquote line. pi `mdQuote`.
    Quote,
    /// A list marker (`-`, `1.`). pi `mdListBullet`.
    List,
    /// An inline `code` span (the backticks drop; the style stays).
    /// pi `mdCode`.
    InlineCode,
    /// The text of a markdown link. pi `mdLink`.
    Link,
    /// The URL of a markdown link (dimmed). pi `mdLinkUrl`.
    LinkUrl,
    /// A comment token. pi `syntaxComment`.
    SyntaxComment,
    /// A keyword token. pi `syntaxKeyword`.
    SyntaxKeyword,
    /// A function-call token. pi `syntaxFunction`.
    SyntaxFunction,
    /// A variable token; the JSON object key colors through it (the
    /// pi highlight.js `attr` scope mapping). pi `syntaxVariable`.
    SyntaxVariable,
    /// A string token; the JSON string value colors through it. pi
    /// `syntaxString`.
    SyntaxString,
    /// A number token; the JSON number and the `true` / `false` /
    /// `null` literals color through it (the pi `literal` scope
    /// mapping). pi `syntaxNumber`.
    SyntaxNumber,
    /// A type token. pi `syntaxType`.
    SyntaxType,
    /// An operator token. pi `syntaxOperator`.
    SyntaxOperator,
    /// Structural punctuation token; the JSON braces, colons, commas
    /// and brackets color through it. pi `syntaxPunctuation`.
    SyntaxPunctuation,
    /// The model thinking block. pi `thinkingText`.
    Thinking,
    /// Fold/expand hints and other muted text (`… +N more lines`).
    /// pi `muted`.
    Hint,
    /// The built-in status/help row and the last-loop-line row. pi
    /// `dim`.
    Status,
    /// Error accents (error status, failed lines). pi `error`.
    Error,
    /// Success accents (allow decisions, added diff lines). pi
    /// `success`.
    Success,
    /// Warning accents (truncation marks, the approval banner). pi
    /// `warning`.
    Warning,
    /// An added diff line. pi `toolDiffAdded`.
    DiffAdded,
    /// A removed diff line. pi `toolDiffRemoved`.
    DiffRemoved,
    /// A diff context line. pi `toolDiffContext`.
    DiffContext,
    /// The input-area border of thinking level 0 (no thinking). pi
    /// `thinkingOff`.
    Border0,
    /// The input-area border of thinking level 1 (low). pi
    /// `thinkingLow`.
    Border1,
    /// The input-area border of thinking level 2 (medium). pi
    /// `thinkingMedium`.
    Border2,
    /// The input-area border of thinking level 3 (high). pi
    /// `thinkingHigh`.
    Border3,
    /// The input-area border of thinking level 4+ (highest). pi
    /// `thinkingXhigh`.
    Border4,
    /// The background of a running (pending) tool-result box. pi
    /// `toolPendingBg`.
    ToolBoxBg,
    /// The light background of a successful tool-result box. pi
    /// `toolSuccessBg`.
    ToolBoxBgSuccess,
    /// The light background of a failed tool-result box. pi
    /// `toolErrorBg`.
    ToolBoxBgError,
}

impl Role {
    /// Every role, in declaration order. A scheme may leave any role
    /// unset: the unset roles keep the built-in palette value.
    pub const ALL: &[Role] = &[
        Role::PlainText,
        Role::ToolOutput,
        Role::ToolCommand,
        Role::Accent,
        Role::Code,
        Role::Fence,
        Role::Heading,
        Role::Quote,
        Role::List,
        Role::InlineCode,
        Role::Link,
        Role::LinkUrl,
        Role::SyntaxComment,
        Role::SyntaxKeyword,
        Role::SyntaxFunction,
        Role::SyntaxVariable,
        Role::SyntaxString,
        Role::SyntaxNumber,
        Role::SyntaxType,
        Role::SyntaxOperator,
        Role::SyntaxPunctuation,
        Role::Thinking,
        Role::Hint,
        Role::Status,
        Role::Error,
        Role::Success,
        Role::Warning,
        Role::DiffAdded,
        Role::DiffRemoved,
        Role::DiffContext,
        Role::Border0,
        Role::Border1,
        Role::Border2,
        Role::Border3,
        Role::Border4,
        Role::ToolBoxBg,
        Role::ToolBoxBgSuccess,
        Role::ToolBoxBgError,
    ];

    /// The built-in value of the role at every capability level. The
    /// default palette (no scheme selected) mirrors the pi built-in
    /// `dark` theme element colors (docs/tui-color-pi-alignment.md):
    /// every role is a truecolor target, lowered to the active
    /// capability level (quantized at 256, snapped at 16).
    pub fn builtin(self, level: Level) -> Color {
        use Role::*;
        let c = match self {
            PlainText => level.plain_text(),
            ToolOutput => level.tool_output(),
            ToolCommand => level.tool_command(),
            Accent => Color::Rgb(0x8a, 0xbe, 0xb7),
            Code => Color::Rgb(0xb5, 0xbd, 0x68),
            Fence => Color::Rgb(0x80, 0x80, 0x80),
            Heading => Color::Rgb(0xf0, 0xc6, 0x74),
            Quote => Color::Rgb(0x80, 0x80, 0x80),
            List | InlineCode => Color::Rgb(0x8a, 0xbe, 0xb7),
            Link => Color::Rgb(0x81, 0xa2, 0xbe),
            LinkUrl => Color::Rgb(0x66, 0x66, 0x66),
            SyntaxComment => Color::Rgb(0x6a, 0x99, 0x55),
            SyntaxKeyword => Color::Rgb(0x56, 0x9c, 0xd6),
            SyntaxFunction => Color::Rgb(0xdc, 0xdc, 0xaa),
            SyntaxVariable => Color::Rgb(0x9c, 0xdc, 0xfe),
            SyntaxString => Color::Rgb(0xce, 0x91, 0x78),
            SyntaxNumber => Color::Rgb(0xb5, 0xce, 0xa8),
            SyntaxType => Color::Rgb(0x4e, 0xc9, 0xb0),
            SyntaxOperator => Color::Rgb(0xd4, 0xd4, 0xd4),
            SyntaxPunctuation => Color::Rgb(0xd4, 0xd4, 0xd4),
            Thinking => level.thinking(),
            Hint => Color::Rgb(0x80, 0x80, 0x80),
            Status => Color::Rgb(0x66, 0x66, 0x66),
            Error | DiffRemoved => Color::Rgb(0xcc, 0x66, 0x66),
            Success | DiffAdded => Color::Rgb(0xb5, 0xbd, 0x68),
            Warning => Color::Rgb(0xff, 0xff, 0x00),
            DiffContext => Color::Rgb(0x80, 0x80, 0x80),
            Border0 => Color::Rgb(0x50, 0x50, 0x50),
            Border1 => Color::Rgb(0x5f, 0x87, 0xaf),
            Border2 => Color::Rgb(0x81, 0xa2, 0xbe),
            Border3 => Color::Rgb(0xb2, 0x94, 0xbb),
            Border4 => Color::Rgb(0xd1, 0x83, 0xe8),
            ToolBoxBg => Color::Rgb(0x28, 0x28, 0x32),
            ToolBoxBgSuccess => Color::Rgb(0x28, 0x32, 0x28),
            ToolBoxBgError => Color::Rgb(0x3c, 0x28, 0x28),
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
            Accent => "accent",
            Code => "code",
            Fence => "fence",
            Heading => "heading",
            Quote => "quote",
            List => "list",
            InlineCode => "inline_code",
            Link => "link",
            LinkUrl => "link_url",
            SyntaxComment => "syntax_comment",
            SyntaxKeyword => "syntax_keyword",
            SyntaxFunction => "syntax_function",
            SyntaxVariable => "syntax_variable",
            SyntaxString => "syntax_string",
            SyntaxNumber => "syntax_number",
            SyntaxType => "syntax_type",
            SyntaxOperator => "syntax_operator",
            SyntaxPunctuation => "syntax_punctuation",
            Thinking => "thinking",
            Hint => "hint",
            Status => "status",
            Error => "error",
            Success => "success",
            Warning => "warning",
            DiffAdded => "diff_added",
            DiffRemoved => "diff_removed",
            DiffContext => "diff_context",
            Border0 => "border0",
            Border1 => "border1",
            Border2 => "border2",
            Border3 => "border3",
            Border4 => "border4",
            ToolBoxBg => "tool_box_bg",
            ToolBoxBgSuccess => "tool_box_bg_success",
            ToolBoxBgError => "tool_box_bg_error",
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

/// The role-to-hex table of the `catppuccin macchiato` scheme:
/// the pi `catppuccin-macchiato` theme JSON values, role by role
/// (docs/tui-color-pi-alignment.md). A scheme maps every color
/// role to a hex value; the TUI ships this one as the first named
/// internal scheme.
pub fn catppuccin_macchiato() -> std::collections::HashMap<Role, &'static str> {
    use Role::*;
    let pairs: [(Role, &str); 38] = [
        (PlainText, "#cad3f5"),
        (ToolOutput, "#cad3f5"),
        (ToolCommand, "#c6a0f6"),
        (Accent, "#c6a0f6"),
        (Code, "#cad3f5"),
        (Fence, "#5b6078"),
        (Heading, "#f5bde6"),
        (Quote, "#a5adcb"),
        (List, "#c6a0f6"),
        (InlineCode, "#91d7e3"),
        (Link, "#8aadf4"),
        (LinkUrl, "#a5adcb"),
        (SyntaxComment, "#8087a2"),
        (SyntaxKeyword, "#c6a0f6"),
        (SyntaxFunction, "#8aadf4"),
        (SyntaxVariable, "#cad3f5"),
        (SyntaxString, "#a6da95"),
        (SyntaxNumber, "#f5a97f"),
        (SyntaxType, "#eed49f"),
        (SyntaxOperator, "#91d7e3"),
        (SyntaxPunctuation, "#939ab7"),
        (Thinking, "#b8c0e0"),
        (Hint, "#a5adcb"),
        (Status, "#8087a2"),
        (Error, "#ed8796"),
        (Success, "#a6da95"),
        (Warning, "#eed49f"),
        (DiffAdded, "#a6da95"),
        (DiffRemoved, "#ed8796"),
        (DiffContext, "#a5adcb"),
        (Border0, "#8087a2"),
        (Border1, "#8bd5ca"),
        (Border2, "#a6da95"),
        (Border3, "#eed49f"),
        (Border4, "#f5a97f"),
        (ToolBoxBg, "#363a4f"),
        (ToolBoxBgSuccess, "#363a4f"),
        (ToolBoxBgError, "#363a4f"),
    ];
    pairs.iter().cloned().collect()
}

/// The resolved palette of one TUI run: every role lowered to the
/// active capability level. The no-scheme default is the `catppuccin
/// macchiato` scheme (the reference pi theme, docs/tui-color-pi-
/// alignment.md). The `builtin` constructor is the pi built-in `dark`
/// theme element colors, the fallback for an unset role in a user
/// table; a scheme constructor maps roles to hex values and lowers
/// them, an unset role keeps the built-in value of the level.
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
/// section 3, the scheme switch point: config load). A `None` scheme
/// picks the default scheme, `catppuccin macchiato` (the reference
/// pi theme, docs/tui-color-pi-alignment.md). A named scheme is a
/// built-in name or a user table; the values lower to `level`, and a
/// bad hex is a hard error like the `[tui] color` level.
pub fn palette_from_config(
    level: Level,
    scheme: Option<&str>,
    custom: &std::collections::HashMap<String, std::collections::HashMap<Role, String>>,
) -> Result<Palette, String> {
    let Some(name) = scheme else {
        // The default scheme: the reference pi theme.
        return Ok(Palette::named(level, SCHEME_CATPPUCCIN_MACCHIATO)
            .unwrap_or_else(|| Palette::builtin(level)));
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
        // Transcript prose / the input draft. The pi `dark` theme
        // text var (#d4d4d4), not a saturated swatch.
        assert_eq!(Level::Rgb.plain_text(), Color::Rgb(212, 212, 212));
        // 16-color lands on the light-gray swatch (xterm 7), never the dim one.
        assert_eq!(Level::C16.plain_text(), Color::Gray);
        // 256-color quantizes the same gray target to a 256-palette index.
        assert!(matches!(Level::C256.plain_text(), Color::Indexed(..)));
    }

    #[test]
    fn tool_output_palette() {
        // Tool/command output: the pi `toolOutput` gray (#808080),
        // distinct from prose at every level so bash results are
        // visually separable from plain text.
        assert_eq!(Level::Rgb.tool_output(), Color::Rgb(128, 128, 128));
        assert_eq!(Level::C16.tool_output(), Color::DarkGray);
        assert!(matches!(Level::C256.tool_output(), Color::Indexed(..)));
        for lvl in [Level::Rgb, Level::C256, Level::C16] {
            assert_ne!(lvl.plain_text(), lvl.tool_output());
        }
    }

    #[test]
    fn tool_command_palette() {
        // The command text of a tool call: the pi `toolTitle` tone
        // (#d4d4d4), lighter than the result body (tool_output), so
        // command and output read as two different voices.
        assert_eq!(Level::Rgb.tool_command(), Color::Rgb(212, 212, 212));
        assert_eq!(Level::C16.tool_command(), Color::Gray);
        assert!(matches!(Level::C256.tool_command(), Color::Indexed(..)));
        for lvl in [Level::Rgb, Level::C256, Level::C16] {
            // Command is lighter than the result body at every level.
            assert_ne!(lvl.tool_output(), lvl.tool_command());
        }
    }

    // ── color schemes (docs/tui-color-scheme.md) ────────────

    #[test]
    fn builtin_palette_mirrors_the_pi_dark_theme() {
        // The no-scheme palette is the pi built-in `dark` theme
        // element colors, lowered to the level.
        let p = Palette::builtin(Level::C16);
        assert_eq!(p.color(Role::PlainText), Color::Gray);
        assert_eq!(p.color(Role::ToolOutput), Color::DarkGray);
        assert_eq!(p.color(Role::ToolCommand), Color::Gray);
        // #cc6666 error snaps to the nearest swatch (the dark swatch
        // beats the light red on squared distance).
        assert_eq!(p.color(Role::Error), Color::DarkGray);
        assert_eq!(p.color(Role::Border0), Color::DarkGray);
        // #d183e8 snaps to the gray swatch (xterm 7, #c0c0c0), the
        // closest of the 16 swatches on squared distance.
        assert_eq!(p.color(Role::Border4), Color::Gray);
        // The box backgrounds snap to the black swatch at C16.
        assert!(matches!(
            p.color(Role::ToolBoxBg),
            Color::Black | Color::DarkGray
        ));
        let p = Palette::builtin(Level::Rgb);
        assert_eq!(p.color(Role::ToolBoxBg), Color::Rgb(0x28, 0x28, 0x32));
        assert_eq!(p.color(Role::ToolBoxBgSuccess), Color::Rgb(0x28, 0x32, 0x28));
        assert_eq!(p.color(Role::ToolBoxBgError), Color::Rgb(0x3c, 0x28, 0x28));
        assert_eq!(p.level(), Level::Rgb);
    }

    #[test]
    fn builtin_role_table_is_complete() {
        // Every role has a built-in value at every level: a new role
        // that misses the table fails here, not at render time.
        for level in [Level::Rgb, Level::C256, Level::C16] {
            assert_eq!(Role::ALL.len(), 38, "the role list grows: update the table");
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
        // The documented mapping (docs/tui-color-pi-alignment.md):
        // the pi catppuccin-macchiato theme values, role to hex.
        assert_eq!(table[&Role::PlainText], "#cad3f5", "text");
        assert_eq!(table[&Role::Error], "#ed8796", "red");
        assert_eq!(table[&Role::Success], "#a6da95", "green");
        assert_eq!(table[&Role::ToolBoxBg], "#363a4f", "surface0");
    }

    #[test]
    fn named_scheme_resolves_at_the_level() {
        let p = Palette::named(Level::Rgb, SCHEME_CATPPUCCIN_MACCHIATO)
            .expect("the built-in scheme resolves");
        assert_eq!(p.color(Role::PlainText), Color::Rgb(0xca, 0xd3, 0xf5));
        assert_eq!(p.color(Role::Heading), Color::Rgb(0xf5, 0xbd, 0xe6));
        // 256-color lowers the same hex to an index.
        let p = Palette::named(Level::C256, SCHEME_CATPPUCCIN_MACCHIATO).unwrap();
        assert!(matches!(p.color(Role::PlainText), Color::Indexed(..)));
        // 16-color snaps to the nearest swatch.
        let p = Palette::named(Level::C16, SCHEME_CATPPUCCIN_MACCHIATO).unwrap();
        // #cad3f5 snaps to the gray swatch (the closest of the light
        // swatches; never the terminal default).
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
    fn no_scheme_default_is_the_macchiato_scheme() {
        // The default (no scheme in config) is the reference pi
        // theme: catppuccin macchiato, not the pi built-in dark.
        let custom = std::collections::HashMap::new();
        let p = palette_from_config(Level::Rgb, None, &custom).unwrap();
        let m = Palette::named(Level::Rgb, SCHEME_CATPPUCCIN_MACCHIATO).unwrap();
        for role in Role::ALL {
            assert_eq!(p.color(*role), m.color(*role), "default role {role:?}");
        }
        // The tool-result box is the macchiato surface0, a light
        // blue-gray, not the pi built-in dark green (#283228).
        assert_eq!(p.color(Role::ToolBoxBgSuccess), Color::Rgb(0x36, 0x3a, 0x4f));
        // The tool name is the macchiato mauve, the box text is the
        // macchiato text var (docs/tui-color-pi-alignment.md).
        assert_eq!(p.color(Role::ToolCommand), Color::Rgb(0xc6, 0xa0, 0xf6));
        assert_eq!(p.color(Role::ToolOutput), Color::Rgb(0xca, 0xd3, 0xf5));
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
        assert_eq!(
            p.color(Role::SyntaxPunctuation),
            Color::Rgb(0xab, 0xcd, 0xef)
        );
    }

    #[test]
    fn palette_thinking_border_tracks_the_levels() {
        let p = Palette::named(Level::Rgb, SCHEME_CATPPUCCIN_MACCHIATO).unwrap();
        // The border palette of the Macchiato scheme: the pi
        // thinkingOff/Low/Medium/High/Xhigh colors (gray base, teal,
        // green, yellow, peach).
        assert_eq!(p.thinking_border(0), p.color(Role::Border0));
        assert_eq!(p.thinking_border(4), p.color(Role::Border4));
        assert_eq!(p.thinking_border(9), p.color(Role::Border4));
        assert_eq!(p.color(Role::Border0), Color::Rgb(0x80, 0x87, 0xa2));
        assert_eq!(p.color(Role::Border4), Color::Rgb(0xf5, 0xa9, 0x7f));
    }
}
