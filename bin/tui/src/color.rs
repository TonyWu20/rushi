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

use ratatui::style::{Color, Style};

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
            (cube((i / 36) as u8), cube(((i / 6) % 6) as u8), cube((i % 6) as u8))
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
        assert_eq!(Level::detect_with(None, Some("xterm-256color")), Level::C256);
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
        assert_eq!(Level::detect_with(None, Some("XTERM-256COLOR")), Level::C256);
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
        assert_eq!(lower(Color::Rgb(0x24, 0x27, 0x3a), Level::Rgb), Color::Rgb(0x24, 0x27, 0x3a));
    }

    #[test]
    fn lowering_quantizes_rgb_at_256() {
        assert_eq!(lower(Color::Rgb(255, 0, 0), Level::C256), Color::Indexed(196));
        assert_eq!(lower(Color::Rgb(0, 255, 255), Level::C256), Color::Indexed(51));
        assert_eq!(lower(Color::Rgb(255, 255, 255), Level::C256), Color::Indexed(231));
        assert_eq!(lower(Color::Rgb(0, 0, 0), Level::C256), Color::Indexed(16));
        // Near-gray hits the ramp, not the cube.
        assert_eq!(lower(Color::Rgb(10, 10, 10), Level::C256), Color::Indexed(232));
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
        assert_eq!(lower(Color::Rgb(255, 255, 0), Level::C16), Color::LightYellow);
        // Mid-gray lands on the dark swatch (128,128,128).
        assert_eq!(lower(Color::Rgb(128, 128, 128), Level::C16), Color::DarkGray);
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
        assert_eq!(
            Level::Rgb.tool_command(),
            Color::Rgb(170, 200, 240)
        );
        assert_eq!(Level::C16.tool_command(), Color::LightBlue);
        assert!(matches!(Level::C256.tool_command(), Color::Indexed(..)));
        for lvl in [Level::Rgb, Level::C256, Level::C16] {
            // Command is its own voice: distinct from prose and from
            // the result body at every level.
            assert_ne!(lvl.plain_text(), lvl.tool_command());
            assert_ne!(lvl.tool_output(), lvl.tool_command());
        }
    }
}
