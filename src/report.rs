//! What every command prints: one envelope, as text for people or JSON for
//! agents.
//!
//! The JSON shape is the agent contract. Every command returns the same
//! envelope, so an agent can always read `ok`, `error.code` and `next`
//! without knowing which command it ran.

use serde::Serialize;
use serde_json::Value;

use crate::error::Error;

/// The single action tether recommends next, and why.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Next {
    pub command: String,
    pub why: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ErrorInfo {
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// tether's version, so a stored report says which contract produced it.
    pub tether: &'static str,
    pub command: &'static str,
    pub ok: bool,
    /// One plain sentence a person can read without the data.
    pub summary: String,
    pub data: Value,
    pub next: Option<Next>,
    pub error: Option<ErrorInfo>,
    /// The command worked but found something a person or agent must act on.
    #[serde(skip)]
    pub action_needed: bool,
}

impl Report {
    pub fn success(command: &'static str, summary: impl Into<String>, data: Value) -> Self {
        Report {
            tether: env!("CARGO_PKG_VERSION"),
            command,
            ok: true,
            summary: summary.into(),
            data,
            next: None,
            error: None,
            action_needed: false,
        }
    }

    pub fn failure(command: &'static str, error: &Error) -> Self {
        Report {
            tether: env!("CARGO_PKG_VERSION"),
            command,
            ok: false,
            summary: error.to_string(),
            data: Value::Null,
            next: None,
            error: Some(ErrorInfo {
                code: error.code(),
                message: error.to_string(),
            }),
            action_needed: false,
        }
    }

    /// Mark that the command found something to act on (exit code 2).
    pub fn needs_action(mut self) -> Self {
        self.action_needed = true;
        self
    }

    pub fn with_next(mut self, command: impl Into<String>, why: impl Into<String>) -> Self {
        self.next = Some(Next {
            command: command.into(),
            why: why.into(),
        });
        self
    }

    /// Process exit code: 0 done, 1 failed, 2 worked but action is needed.
    pub fn exit_code(&self) -> u8 {
        match (self.ok, self.action_needed) {
            (false, _) => 1,
            (true, true) => 2,
            (true, false) => 0,
        }
    }

    pub fn render(&self, json: bool) -> String {
        if json {
            let mut text =
                serde_json::to_string_pretty(self).expect("a report is always serialisable");
            text.push('\n');
            return text;
        }
        let mut text = format!("{}\n", self.summary);
        if let Some(next) = &self.next {
            text.push_str(&format!("next: {}  ({})\n", next.command, next.why));
        }
        text
    }
}
