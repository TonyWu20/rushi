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
/// error like the other keys).
#[derive(Debug, Default, Deserialize)]
struct RawTui {
    #[serde(default)]
    color: Option<String>,
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

        Ok(TuiConfig {
            sessions_root,
            schemas_dir,
            loop_cmd,
            config_dir,
            config_path: canonical,
            ext_dir,
            active_model,
            color,
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
}
