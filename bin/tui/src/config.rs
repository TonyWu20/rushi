//! Config loading for the TUI.
//!
//! The TUI reads two things from the harness config file:
//! - `[paths] sessions_root` — where session directories live
//! - `[loop]` — the opaque loop command (docs/tui.md section 2.3)
//!
//! The TUI source contains no loop script names: they are config values.
//! Relative paths resolve against the config file's directory, so the TUI
//! behaves the same no matter where it is launched from.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// How the loop command receives the session id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgStyle {
    /// Append the session id as the last argument.
    AppendSession,
}

impl ArgStyle {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "append_session" => Some(ArgStyle::AppendSession),
            _ => None,
        }
    }
}

/// The opaque loop command from the config's `[loop]` section.
/// The TUI runs exactly this and treats it as a black box.
#[derive(Debug, Clone)]
pub struct LoopCommand {
    pub command: String,
    pub args: Vec<String>,
    pub arg_style: ArgStyle,
}

impl LoopCommand {
    /// The argv for one session: config args, session id last.
    pub fn argv(&self, session: &crate::port::SessionId) -> Vec<String> {
        let mut argv = vec![self.command.clone()];
        argv.extend(self.args.iter().cloned());
        if self.arg_style == ArgStyle::AppendSession {
            argv.push(session.to_string());
        }
        argv
    }
}

/// Everything the TUI needs from the config file.
#[derive(Debug, Clone)]
pub struct TuiConfig {
    /// Absolute directory that contains session directories.
    pub sessions_root: PathBuf,
    /// Absolute schema directory for producer-side event validation, if
    /// one exists next to the config. `None` skips schema validation.
    pub schemas_dir: Option<PathBuf>,
    /// The opaque loop command, if configured.
    pub loop_cmd: Option<LoopCommand>,
    /// Absolute directory containing the config file.
    pub config_dir: PathBuf,
    /// Absolute config file path (exported to the loop process).
    pub config_path: PathBuf,
    /// The global extension directory, if the `[ext] dir` override is
    /// set. `None` uses `<config_dir>/ui_extensions`
    /// (docs/ui-extension.md section 3).
    pub ext_dir: Option<PathBuf>,
    /// The active model name, for the extension `tick` payload
    /// (docs/ui-extension.md section 4). `None` when unconfigured.
    pub active_model: Option<String>,
    /// The forced terminal color capability level, or `None` to detect
    /// from the environment at startup (see color.rs module docs).
    pub color: Option<crate::color::Level>,
    /// The named color scheme, or `None` for the built-in palette
    /// (docs/tui-color-scheme.md section 3). A built-in name or a
    /// user scheme defined under `custom_schemes`.
    pub color_scheme: Option<String>,
    /// User-defined schemes: name to role-key to hex value. A
    /// missing role keeps the built-in palette value of the level:
    /// a partial table overlays the built-ins (docs/tui-color-
    /// scheme.md section 3).
    pub custom_schemes:
        std::collections::HashMap<String, std::collections::HashMap<crate::color::Role, String>>,
    /// The tool-result display state (docs/tui-tool-display-port.md
    /// section 2, the config part): a preset plus the per-field
    /// overrides. The `opencode` preset is the default: read and
    /// search results stay collapsed.
    pub tool_display: crate::tool_display::ToolDisplay,
}

#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    paths: Option<RawPaths>,
    #[serde(rename = "loop")]
    loop_cmd: Option<RawLoop>,
    ext: Option<RawExt>,
    active: Option<RawActive>,
    tui: Option<RawTui>,
}

/// The optional `[ext]` section: an override for the global extension
/// directory (docs/ui-extension-plan.md stage 1).
#[derive(Debug, Default, Deserialize)]
struct RawExt {
    dir: Option<String>,
}

/// The optional `[active]` section: the active model name.
#[derive(Debug, Default, Deserialize)]
struct RawActive {
    model: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawPaths {
    sessions_root: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawLoop {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default = "default_arg_style")]
    arg_style: String,
}

fn default_arg_style() -> String {
    "append_session".to_string()
}

/// The optional `[tui]` table: `color` forces the color capability
/// level (`truecolor`, `256`, `8`, `16`; unknown names are a hard
/// error like the other keys). `color_scheme` selects a named color
/// scheme (docs/tui-color-scheme.md section 3: the default is the
/// `catppuccin macchiato` scheme, the reference pi theme). `color_schemes` holds user-defined
/// role-to-hex tables; a table name the `color_scheme` value does
/// not name is inert.
#[derive(Debug, Default, Deserialize)]
struct RawTui {
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    color_scheme: Option<String>,
    #[serde(default)]
    color_schemes: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    /// The tool-result display table (docs/tui-tool-display-port.md
    /// section 2). Every field is optional; a missing field keeps
    /// the preset value. The preset defaults to `opencode`.
    #[serde(default)]
    tool_display: Option<RawToolDisplay>,
}

/// The `[tui.tool_display]` table: a preset name plus the per-field
/// overrides (docs/tui-tool-display-port.md section 2, the config
/// part). The presets are `opencode` (the default: read and search
/// hidden, bash collapsed to 10 lines), `balanced` (summaries), and
/// `verbose` (larger previews).
#[derive(Debug, Default, Deserialize)]
struct RawToolDisplay {
    preset: Option<String>,
    /// The read output mode: `hidden`, `summary`, `preview`.
    read: Option<String>,
    /// The search output mode: `hidden`, `count`, `preview`.
    search: Option<String>,
    /// The bash output mode: `hidden`, `summary`, `preview`.
    bash: Option<String>,
    /// The preview line count of the read preview.
    preview_lines: Option<usize>,
    /// The collapsed line count of the bash output.
    bash_collapsed_lines: Option<usize>,
    /// The collapsed line count of the edit/write diff.
    diff_collapsed_lines: Option<usize>,
    /// The expanded preview cap (the `Ctrl+O` expansion).
    expanded_preview_max_lines: Option<usize>,
    /// The diff layout: `auto`, `split`, `unified`.
    diff_view: Option<String>,
}

impl TuiConfig {
    /// Load the config. A missing file yields defaults (sessions root
    /// `sessions` next to the given path, no loop command) so the TUI
    /// stays useful for viewing logs; a corrupt file is a hard error.
    pub fn load(path: &str) -> Result<Self, String> {
        use crate::color::Level;
        let path = PathBuf::from(path);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!(
                    "warning: config file {} not found; using defaults",
                    path.display()
                );
                return Ok(Self::defaults_for(&path));
            }
            Err(e) => {
                return Err(format!("cannot read config {}: {e}", path.display()));
            }
        };
        let raw: RawConfig = toml::from_str(&content)
            .map_err(|e| format!("invalid TOML in {}: {e}", path.display()))?;
        let canonical = std::fs::canonicalize(&path)
            .ok()
            .filter(|p| p.is_file())
            .unwrap_or_else(|| path.clone());
        let config_dir = canonical
            .parent()
            .map(|p| p.to_path_buf())
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or_else(|| PathBuf::from("."));

        let sessions_root = raw
            .paths
            .as_ref()
            .and_then(|p| p.sessions_root.clone())
            .unwrap_or_else(|| "sessions".to_string());
        let sessions_root = resolve(&config_dir, sessions_root);

        let schemas_path = config_dir.join("schemas").join("events").join("v1");
        let schemas_dir = if schemas_path.is_dir() {
            Some(schemas_path)
        } else {
            None
        };

        let loop_cmd = match raw.loop_cmd {
            Some(l) => {
                let arg_style = ArgStyle::parse(&l.arg_style)
                    .ok_or_else(|| format!("config [loop] arg_style \"{}\" is not supported (expected \"append_session\")", l.arg_style))?;
                if l.command.trim().is_empty() {
                    return Err("config [loop] command must not be empty".to_string());
                }
                Some(LoopCommand {
                    command: l.command,
                    args: l.args,
                    arg_style,
                })
            }
            None => None,
        };

        let ext_dir = raw
            .ext
            .as_ref()
            .and_then(|e| e.dir.clone())
            .map(|d| resolve(&config_dir, d));

        let active_model = raw.active.as_ref().and_then(|a| a.model.clone());

        // An explicit `[tui] color` forces the level; unknown names are a
        // hard error, like the other keys. Absent means detect.
        let color = match raw.tui.as_ref().and_then(|t| t.color.clone()) {
            Some(c) => {
                let lvl = Level::from_cfg(&c).ok_or_else(|| {
                    format!(
                        "config [tui] color: unknown value {c:?} \
                         (expected one of truecolor, 256, 8, 16)"
                    )
                })?;
                Some(lvl)
            }
            None => None,
        };

        // The color scheme (docs/tui-color-scheme.md section 3): a
        // named internal scheme (`catppuccin macchiato`) or a user
        // scheme table. Unknown names are a hard error at load, like
        // the other keys. A table value that is not a known role key
        // or is not a parseable hex is a hard error: a scheme must
        // not load half-validated.
        let mut custom_schemes: std::collections::HashMap<
            String,
            std::collections::HashMap<crate::color::Role, String>,
        > = std::collections::HashMap::new();
        for (name, table) in raw
            .tui
            .as_ref()
            .map(|t| t.color_schemes.clone())
            .unwrap_or_default()
        {
            let key_to_role = |k: &str| -> Result<crate::color::Role, String> {
                crate::color::Role::ALL
                    .iter()
                    .find(|r| r.key() == k)
                    .copied()
                    .ok_or_else(|| {
                        format!(
                            "config [tui] color_schemes.{}: unknown role {k:?} \
                             (expected one of {})",
                            name,
                            crate::color::Role::ALL
                                .iter()
                                .map(|r| r.key())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })
            };
            let mut roles: std::collections::HashMap<crate::color::Role, String> =
                std::collections::HashMap::new();
            for (key, hex) in table {
                let role = key_to_role(&key)?;
                if crate::color::parse_scheme_color(&hex).is_none() {
                    return Err(format!(
                        "config [tui] color_schemes.{}: role {key} value {hex:?} \
                         is not a hex color (expected #rgb or #rrggbb)",
                        name
                    ));
                }
                roles.insert(role, hex);
            }
            custom_schemes.insert(name, roles);
        }
        if let Some(name) = raw.tui.as_ref().and_then(|t| t.color_scheme.clone()) {
            let known = name == crate::color::SCHEME_CATPPUCCIN_MACCHIATO
                || custom_schemes.contains_key(&name);
            if !known {
                return Err(format!(
                    "config [tui] color_scheme: unknown scheme {name:?} \
                     (expected the built-in {} or a [tui] color_schemes table name",
                    crate::color::SCHEME_CATPPUCCIN_MACCHIATO
                ));
            }
        }
        let color_scheme = raw.tui.as_ref().and_then(|t| t.color_scheme.clone());

        // The tool-result display table (docs/tui-tool-display-port.md
        // section 2, the config part). The preset table is the base;
        // the per-field overrides win. A bad preset name or a bad
        // mode value is a hard error at load, like the other keys.
        let td = raw.tui.as_ref().and_then(|t| t.tool_display.as_ref());
        let preset = td.and_then(|t| t.preset.as_deref()).unwrap_or("opencode");
        let preset = crate::tool_display::parse_preset(preset).ok_or_else(|| {
            format!(
                "config [tui.tool_display] preset: unknown preset {preset:?} \
                 (expected opencode, balanced, or verbose)"
            )
        })?;
        let mut tool_display = crate::tool_display::ToolDisplay::preset(preset);
        if let Some(t) = td {
            if let Some(m) = t.read.as_deref() {
                tool_display.read_mode =
                    crate::tool_display::parse_output_mode(m).ok_or_else(|| {
                        format!(
                            "config [tui.tool_display] read: unknown mode {m:?} \
                             (expected hidden, summary, or preview)"
                        )
                    })?;
            }
            if let Some(m) = t.search.as_deref() {
                tool_display.search_mode =
                    crate::tool_display::parse_search_mode(m).ok_or_else(|| {
                        format!(
                            "config [tui.tool_display] search: unknown mode {m:?} \
                             (expected hidden, count, or preview)"
                        )
                    })?;
            }
            if let Some(m) = t.bash.as_deref() {
                tool_display.bash_mode =
                    crate::tool_display::parse_output_mode(m).ok_or_else(|| {
                        format!(
                            "config [tui.tool_display] bash: unknown mode {m:?} \
                             (expected hidden, summary, or preview)"
                        )
                    })?;
            }
            if let Some(m) = t.diff_view.as_deref() {
                tool_display.diff_view =
                    crate::tool_display::parse_diff_view(m).ok_or_else(|| {
                        format!(
                            "config [tui.tool_display] diff_view: unknown value {m:?} \
                             (expected auto, split, or unified)"
                        )
                    })?;
            }
            if let Some(n) = t.preview_lines {
                tool_display.preview_lines = n;
            }
            if let Some(n) = t.bash_collapsed_lines {
                tool_display.bash_collapsed_lines = n;
            }
            if let Some(n) = t.diff_collapsed_lines {
                tool_display.diff_collapsed_lines = n;
            }
            if let Some(n) = t.expanded_preview_max_lines {
                tool_display.expanded_preview_max_lines = n;
            }
        }

        Ok(TuiConfig {
            sessions_root,
            schemas_dir,
            loop_cmd,
            config_dir,
            config_path: canonical,
            ext_dir,
            active_model,
            color,
            color_scheme,
            custom_schemes,
            tool_display,
        })
    }

    fn defaults_for(path: &Path) -> Self {
        let canonical = std::fs::canonicalize(path).ok();
        let config_dir = canonical
            .as_ref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or_else(|| {
                Path::new(path)
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."))
            });
        let schemas_path = config_dir.join("schemas").join("events").join("v1");
        let schemas_dir = if schemas_path.is_dir() {
            Some(schemas_path)
        } else {
            None
        };
        TuiConfig {
            sessions_root: config_dir.join("sessions"),
            schemas_dir,
            loop_cmd: None,
            config_dir,
            config_path: path.to_path_buf(),
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
}

fn resolve(base: &Path, p: String) -> PathBuf {
    let p = PathBuf::from(p);
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, name: &str, body: &str) {
        std::fs::write(path.join(name), body).unwrap();
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let cfg = TuiConfig::load(p.to_str().unwrap()).unwrap();
        assert!(cfg.loop_cmd.is_none());
        assert_eq!(cfg.sessions_root, dir.path().join("sessions"));
    }

    #[test]
    fn parses_loop_section_with_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            r#"
[paths]
sessions_root = "my-sessions"

[loop]
command = "bash"
args = ["scripts/loop.sh"]
arg_style = "append_session"
"#,
        );
        let cfg = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap();
        assert_eq!(cfg.sessions_root, dir.path().join("my-sessions"));
        let lc = cfg.loop_cmd.as_ref().unwrap();
        let argv = lc.argv(&crate::port::SessionId::new("s1"));
        assert_eq!(argv, vec!["bash", "scripts/loop.sh", "s1"]);
    }

    #[test]
    fn missing_paths_section_uses_default_sessions_root() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "config.toml", "[loop]\ncommand = \"bash\"\n");
        let cfg = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap();
        assert_eq!(cfg.sessions_root, dir.path().join("sessions"));
    }

    #[test]
    fn bad_arg_style_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            "[loop]\ncommand = \"bash\"\narg_style = \"telepathy\"\n",
        );
        let err = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap_err();
        assert!(err.contains("arg_style"), "{err}");
    }

    #[test]
    fn empty_loop_command_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "config.toml", "[loop]\ncommand = \"\"\n");
        let err = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap_err();
        assert!(err.contains("command"), "{err}");
    }

    #[test]
    fn corrupt_toml_is_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "config.toml", "not toml [");
        let err = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap_err();
        assert!(err.contains("invalid TOML"), "{err}");
    }

    #[test]
    fn ext_dir_override_and_active_model_parse() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            "[ext]\ndir = \"my-exts\"\n\n[active]\nmodel = \"test-model\"\n",
        );
        let cfg = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap();
        assert_eq!(
            cfg.ext_dir,
            Some(dir.path().join("my-exts")),
            "relative dir resolves against the config dir"
        );
        assert_eq!(cfg.active_model.as_deref(), Some("test-model"));
    }

    #[test]
    fn ext_section_is_optional() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "config.toml", "[loop]\ncommand = \"bash\"\n");
        let cfg = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap();
        assert!(cfg.ext_dir.is_none());
        assert!(cfg.active_model.is_none());
    }

    #[test]
    fn color_scheme_selects_the_builtin_name() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            "[tui]\ncolor_scheme = \"catppuccin macchiato\"\n",
        );
        let cfg = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap();
        assert_eq!(
            cfg.color_scheme.as_deref(),
            Some("catppuccin macchiato"),
            "the built-in scheme name loads"
        );
        assert!(cfg.custom_schemes.is_empty());
    }

    #[test]
    fn user_scheme_table_parses_roles_and_hexes() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            r##"
[tui]
color_scheme = "mocha"

[tui.color_schemes.mocha]
plain_text = "#cdd6f4"
tool_output = "#8f92ac"
border1 = "#74c7ec"
"##,
        );
        let cfg = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap();
        let table = cfg.custom_schemes.get("mocha").expect("the table loads");
        assert_eq!(
            table.get(&crate::color::Role::PlainText),
            Some(&"#cdd6f4".to_string())
        );
        assert_eq!(
            table.get(&crate::color::Role::Border1),
            Some(&"#74c7ec".to_string()),
        );
        assert_eq!(table.len(), 3, "every key of the table parses");
    }

    #[test]
    fn unknown_scheme_name_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            "[tui]\ncolor_scheme = \"solarized\"\n",
        );
        let err = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap_err();
        assert!(err.contains("unknown scheme"), "{err}");
    }

    #[test]
    fn user_scheme_name_resolves_against_the_tables() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            r##"
[tui]
color_scheme = "mocha"

[tui.color_schemes.mocha]
plain_text = "#cdd6f4"
"##,
        );
        let cfg = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap();
        assert!(
            cfg.custom_schemes.contains_key("mocha"),
            "the named table resolves"
        );
    }

    #[test]
    fn bad_scheme_hex_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            "[tui.color_schemes.mocha]\nplain_text = \"red\"\n",
        );
        let err = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap_err();
        assert!(err.contains("hex"), "{err}");
    }

    #[test]
    fn unknown_scheme_role_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            "[tui.color_schemes.mocha]\nnope = \"#cdd6f4\"\n",
        );
        let err = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap_err();
        assert!(err.contains("unknown role"), "{err}");
    }

    #[test]
    fn scheme_config_is_optional_and_defaults_empty() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "config.toml", "[loop]\ncommand = \"bash\"\n");
        let cfg = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap();
        assert!(
            cfg.color_scheme.is_none(),
            "no scheme: the built-in palette stands"
        );
        assert!(cfg.custom_schemes.is_empty());
    }

    // ── tool display (docs/tui-tool-display-port.md section 2) ──

    /// The default table is the `opencode` preset: read and search
    /// hidden, bash collapsed to the first 10 lines.
    #[test]
    fn tool_display_defaults_to_the_opencode_preset() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "config.toml", "[loop]\ncommand = \"bash\"\n");
        let cfg = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap();
        use crate::tool_display::*;
        assert_eq!(cfg.tool_display, ToolDisplay::preset(Preset::OpenCode));
    }

    /// A preset table with no overrides keeps the preset value table
    /// and reports the preset name.
    #[test]
    fn tool_display_preset_name_selects_the_table() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            r##"[loop]
command = "bash"

[tui.tool_display]
preset = "verbose"
"##,
        );
        let cfg = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap();
        use crate::tool_display::*;
        assert_eq!(cfg.tool_display, ToolDisplay::preset(Preset::Verbose));
        assert_eq!(cfg.tool_display.preview_lines, 12);
        assert_eq!(cfg.tool_display.bash_collapsed_lines, 20);
    }

    /// A field override wins over the preset table value: the
    /// override stays, the untouched fields keep the preset values.
    #[test]
    fn tool_display_override_wins_over_preset() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            r##"[loop]
command = "bash"

[tui.tool_display]
preset = "opencode"
read = "preview"
preview_lines = 12
"##,
        );
        let cfg = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap();
        use crate::tool_display::*;
        assert_eq!(cfg.tool_display.read_mode, OutputMode::Preview);
        assert_eq!(cfg.tool_display.preview_lines, 12);
        // The untouched fields keep the preset values.
        assert_eq!(cfg.tool_display.search_mode, SearchMode::Hidden);
        assert_eq!(cfg.tool_display.bash_mode, OutputMode::Preview);
    }

    /// An unknown preset name is a hard error at load.
    #[test]
    fn tool_display_unknown_preset_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            r##"[tui.tool_display]
preset = "nope"
"##,
        );
        let err = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap_err();
        assert!(err.contains("preset"), "{err}");
    }

    /// An unknown mode value is a hard error at load.
    #[test]
    fn tool_display_unknown_mode_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            r##"[tui.tool_display]
read = "all"
"##,
        );
        let err = TuiConfig::load(dir.path().join("config.toml").to_str().unwrap()).unwrap_err();
        assert!(err.contains("read"), "{err}");
    }
}
