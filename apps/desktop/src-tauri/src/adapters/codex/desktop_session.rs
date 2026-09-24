//! Optional Desktop display context.
//!
//! Capturing a Hook must rely only on its supported input and the local policy.
//! Rollout JSONL discovery and Codex CLI account queries are deliberately not
//! part of this module or the helper critical path.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopTurnContext {
    pub reasoning_effort: Option<String>,
    pub turn_confirmed: bool,
}
