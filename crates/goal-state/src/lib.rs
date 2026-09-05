//! `goal-state` — shared goal state for the pi-goal port.
//!
//! A goal is a long-running task that the agent pursues across
//! multiple turns. The state persists in `goal.json` in the session
//! directory. Tools and hooks read and write this file to coordinate.
//!
//! See `docs/pi-goal-readiness.md` for the full port plan.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The goal state file.
///
/// Lives at `<session_dir>/goal.json`. Created by the `goal` tool,
/// read by the idle/compact/tool hooks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GoalState {
    /// The goal description (what the user asked for).
    pub goal: String,
    /// Whether the goal is currently being pursued.
    #[serde(default)]
    pub active: bool,
    /// Optional token budget. When set, the hooks use it to decide
    /// when to stop continuing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u64>,
    /// Tokens consumed so far on this goal (updated by the
    /// `model.after` observation or manually).
    #[serde(default)]
    pub used_tokens: u64,
    /// Set to true when the agent signals the goal is done.
    #[serde(default)]
    pub completed: bool,
    /// Set to true when the agent signals the goal is blocked.
    #[serde(default)]
    pub blocked: bool,
    /// Human-readable reason for the block, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<String>,
    /// ISO-8601 timestamp when the goal was opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened_at: Option<String>,
    /// ISO-8601 timestamp when the goal was closed (complete or block).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
}

impl GoalState {
    /// The path to `goal.json` inside a session directory.
    pub fn path(session_dir: &Path) -> PathBuf {
        session_dir.join("goal.json")
    }

    /// Load the goal state from disk. Returns `None` when the file
    /// does not exist or is empty.
    pub fn load(session_dir: &Path) -> Option<GoalState> {
        let p = Self::path(session_dir);
        if !p.exists() {
            return None;
        }
        let data = fs::read_to_string(&p).ok()?;
        if data.trim().is_empty() {
            return None;
        }
        serde_json::from_str(&data).ok()
    }

    /// Persist the goal state to disk. Creates the file if missing.
    pub fn save(&self, session_dir: &Path) -> Result<(), std::io::Error> {
        let p = Self::path(session_dir);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(&p, json)
    }

    /// True when the goal is active and not yet closed.
    pub fn is_open(&self) -> bool {
        self.active && !self.completed && !self.blocked
    }

    /// Remaining token budget, if a budget was set.
    pub fn remaining_budget(&self) -> Option<u64> {
        self.budget_tokens.map(|b| b.saturating_sub(self.used_tokens))
    }

    /// True when the budget is exhausted (used >= budget).
    pub fn budget_exhausted(&self) -> bool {
        self.budget_tokens
            .map(|b| self.used_tokens >= b)
            .unwrap_or(false)
    }

    /// Build the continuation prompt injected at `run.idle`.
    pub fn continuation_prompt(&self) -> String {
        let budget_note = match self.remaining_budget() {
            Some(rem) if rem > 0 => format!(" ({} tokens remaining)", rem),
            Some(_) => " (budget exhausted — wrap up now)".to_string(),
            None => String::new(),
        };
        format!(
            "Goal still in progress: \"{}\"{}. Continue working toward it. \
             If the goal is complete, call goal_complete. If you are blocked, \
             call goal_blocked with a reason.",
            self.goal, budget_note
        )
    }

    /// Create a new active goal state.
    pub fn new(goal: &str, budget_tokens: Option<u64>) -> Self {
        let now = chrono_utc_now();
        Self {
            goal: goal.to_string(),
            active: true,
            budget_tokens,
            used_tokens: 0,
            completed: false,
            blocked: false,
            block_reason: None,
            opened_at: Some(now),
            closed_at: None,
        }
    }

    /// Mark the goal as completed.
    pub fn mark_completed(&mut self) {
        self.completed = true;
        self.active = false;
        self.closed_at = Some(chrono_utc_now());
    }

    /// Mark the goal as blocked with a reason.
    pub fn mark_blocked(&mut self, reason: &str) {
        self.blocked = true;
        self.active = false;
        self.block_reason = Some(reason.to_string());
        self.closed_at = Some(chrono_utc_now());
    }

    /// Re-activate a previously blocked or completed goal.
    ///
    /// Clears the `blocked`/`completed` flags and their metadata,
    /// restores `active` to true, and resets the token budget counter.
    pub fn resume(&mut self) {
        self.active = true;
        self.blocked = false;
        self.completed = false;
        self.block_reason = None;
        self.closed_at = None;
        self.used_tokens = 0;
        self.opened_at = Some(chrono_utc_now());
    }

    /// Update the goal description on an active goal.
    ///
    /// Keeps `used_tokens`, `budget_tokens`, and timestamps intact;
    /// only the goal text changes.
    pub fn edit_goal(&mut self, new_goal: &str) {
        self.goal = new_goal.to_string();
    }

    /// Record that `tokens` more of the goal's budget were spent.
    /// Saturates: `used_tokens` never exceeds the budget when one is set.
    pub fn add_used(&mut self, tokens: u64) {
        self.used_tokens = match self.budget_tokens {
            Some(b) => self.used_tokens.saturating_add(tokens).min(b),
            None => self.used_tokens.saturating_add(tokens),
        };
    }

    /// Read the `events.jsonl` log and return the `output_tokens` of the
    /// most recent `assistant_message` that carries a `usage` reading.
    /// Returns `None` when the log is absent or no measured message exists.
    ///
    /// Used by the `run.idle` hook to advance the goal's token
    /// accounting one step per loop iteration (docs/pi-goal-readiness.md).
    /// Output tokens are the cost of the model's work, which is what
    /// a goal budget should cap.
    pub fn read_last_assistant_output_tokens(events_path: &Path) -> Option<u64> {
        let data = std::fs::read_to_string(events_path).ok()?;
        data.lines().rev().find_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            if v.get("type").and_then(|t| t.as_str()) != Some("assistant_message") {
                return None;
            }
            v.get("usage")
                .and_then(|u| u.get("output_tokens"))
                .and_then(|i| i.as_u64())
        })
    }
}

/// Simple UTC timestamp without adding a chrono dep.
fn chrono_utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("t+{secs}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_goal_is_open() {
        let g = GoalState::new("build a parser", Some(50_000));
        assert!(g.is_open());
        assert_eq!(g.remaining_budget(), Some(50_000));
        assert!(!g.budget_exhausted());
    }

    #[test]
    fn complete_closes() {
        let mut g = GoalState::new("test", None);
        g.mark_completed();
        assert!(!g.is_open());
        assert!(g.completed);
        assert!(g.closed_at.is_some());
    }

    #[test]
    fn block_closes() {
        let mut g = GoalState::new("test", None);
        g.mark_blocked("missing dependency");
        assert!(!g.is_open());
        assert!(g.blocked);
        assert_eq!(g.block_reason.as_deref(), Some("missing dependency"));
    }

    #[test]
    fn budget_exhaustion() {
        let mut g = GoalState::new("test", Some(100));
        g.used_tokens = 100;
        assert!(g.budget_exhausted());
        assert_eq!(g.remaining_budget(), Some(0));
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let g = GoalState::new("roundtrip test", Some(999));
        g.save(dir.path()).unwrap();
        let loaded = GoalState::load(dir.path()).unwrap();
        assert_eq!(g, loaded);
    }

    #[test]
    fn missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(GoalState::load(dir.path()).is_none());
    }

    #[test]
    fn continuation_prompt_mentions_goal() {
        let g = GoalState::new("fix the bug", Some(1000));
        let prompt = g.continuation_prompt();
        assert!(prompt.contains("fix the bug"));
        assert!(prompt.contains("1000 tokens remaining"));
    }

    #[test]
    fn add_used_respects_budget_cap() {
        let mut g = GoalState::new("t", Some(100));
        g.add_used(60);
        assert_eq!(g.used_tokens, 60);
        g.add_used(90);
        // Saturates at the budget, never exceeds it.
        assert_eq!(g.used_tokens, 100);
        assert!(g.budget_exhausted());
    }

    #[test]
    fn add_used_unbounded_without_budget() {
        let mut g = GoalState::new("t", None);
        g.add_used(60);
        g.add_used(90);
        assert_eq!(g.used_tokens, 150);
        assert!(!g.budget_exhausted());
    }

    #[test]
    fn read_last_assistant_output_tokens_reads_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        std::fs::write(
            &log,
            concat!(
                "{\"v\":1,\"type\":\"user_message\",\"ts\":\"t1\",\"content\":\"hi\"}\n",
                "{\"v\":1,\"type\":\"assistant_message\",\"ts\":\"t2\",\"content\":\"a\",\"stop_reason\":\"stop\",\"usage\":{\"input_tokens\":7,\"output_tokens\":3}}\n",
                "{\"v\":1,\"type\":\"assistant_message\",\"ts\":\"t3\",\"content\":\"b\",\"stop_reason\":\"stop\",\"usage\":{\"input_tokens\":42,\"output_tokens\":9}}\n",
            ),
        )
        .unwrap();
        assert_eq!(GoalState::read_last_assistant_output_tokens(&log), Some(9));
    }

    #[test]
    fn read_last_assistant_output_tokens_missing_log_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("events.jsonl");
        assert_eq!(GoalState::read_last_assistant_output_tokens(&log), None);
    }
}
