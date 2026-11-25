//! What an agent may do alone.
//!
//! The policy lives in `.tether/config.toml` (`[agent] needs_human`). It is a
//! guardrail, not a security boundary: an agent identifies itself by running
//! with `TETHER_ACTOR=agent` (AGENT.md tells it to), and a reserved action
//! then refuses with `needs_human` instead of happening.

use crate::config::Config;
use crate::error::{Error, Result};

pub const ACTOR_ENV: &str = "TETHER_ACTOR";

pub fn is_agent() -> bool {
    std::env::var(ACTOR_ENV).is_ok_and(|actor| actor.eq_ignore_ascii_case("agent"))
}

/// Refuse `action` when an agent runs it and policy reserves it for a person.
pub fn require(config: &Config, action: &str) -> Result<()> {
    if is_agent() && config.needs_human(action) {
        return Err(Error::NeedsHuman(action.to_owned()));
    }
    Ok(())
}
