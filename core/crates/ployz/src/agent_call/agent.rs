//! Which coding agent is driving this process, read from the environment alone.
//!
//! `AI_AGENT` names the agent, or turns detection off. Without it, the markers the
//! `is-ai-agent` crate knows decide. Detection never fails a command: an unreadable
//! variable reads as unset.

use std::sync::OnceLock;

use sha2::{Digest, Sha256};

const MAX_NAME: usize = 64;

pub(crate) fn name() -> Option<&'static str> {
    static AGENT: OnceLock<Option<String>> = OnceLock::new();
    AGENT
        .get_or_init(|| detect(|name| std::env::var(name).ok()))
        .as_deref()
}

fn detect(var: impl Fn(&str) -> Option<String>) -> Option<String> {
    if let Some(declared) = var("AI_AGENT") {
        let declared = declared.trim();
        if ["0", "false", "no", "off"]
            .iter()
            .any(|off| declared.eq_ignore_ascii_case(off))
        {
            return None;
        }
        let name: String = declared
            .chars()
            .filter(|char| !char.is_control())
            .take(MAX_NAME)
            .collect();
        if !name.is_empty() {
            return Some(name);
        }
    }
    // `AGENT` is too generic a name to trust, and the files the crate also checks are
    // not the environment.
    is_ai_agent::detect_with(|name| var(name).filter(|_| name != "AGENT"), |_| false)
        .map(|agent| agent.id.as_str().to_owned())
}

/// `ses_` and 32 hex digits of SHA-256 over the agent's name and today's UTC date, so one
/// agent's commands in a day group together. Cloud adds the credential on its side.
pub(crate) fn cli_session(agent: &str) -> String {
    let day = chrono::Utc::now().format("%Y-%m-%d");
    let digest = Sha256::digest(format!("{agent}:{day}"));
    format!("ses_{}", &hex::encode(digest)[..32])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detected(pairs: &[(&str, &str)]) -> Option<String> {
        detect(|name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    #[test]
    fn ai_agent_names_the_agent_and_off_beats_every_marker() {
        assert_eq!(detected(&[]), None);
        assert_eq!(detected(&[("AI_AGENT", "pi")]).as_deref(), Some("pi"));
        assert_eq!(
            detected(&[("AI_AGENT", "my\u{1b}[31magent")]).as_deref(),
            Some("my[31magent")
        );
        assert_eq!(
            detected(&[("AI_AGENT", &"a".repeat(200))]).map(|name| name.len()),
            Some(MAX_NAME)
        );
        for off in ["0", "false", "No", " OFF "] {
            assert_eq!(detected(&[("AI_AGENT", off), ("CLAUDECODE", "1")]), None);
        }
    }

    #[test]
    fn the_crate_names_the_agents_harnesses_mark() {
        for (marker, agent) in [
            ("CLAUDECODE", "claude-code"),
            ("CURSOR_AGENT", "cursor"),
            ("CODEX_THREAD_ID", "codex"),
            ("GEMINI_CLI", "gemini-cli"),
            ("OPENCODE", "opencode"),
            ("AMP_CURRENT_THREAD_ID", "amp"),
        ] {
            assert_eq!(detected(&[(marker, "1")]).as_deref(), Some(agent));
        }
        assert_eq!(detected(&[("CLAUDECODE", "")]), None);
        assert_eq!(detected(&[("AGENT", "1")]), None);
    }

    #[test]
    fn a_session_is_stable_for_an_agent_and_differs_between_agents() {
        let session = cli_session("codex");
        assert!(session.starts_with("ses_"), "{session}");
        assert_eq!(session.len(), 36, "{session}");
        assert_eq!(session, cli_session("codex"));
        assert_ne!(session, cli_session("cursor"));
    }
}
