//! Recovery-secret environment and terminal handling.

use std::io::IsTerminal;

use super::secret;

pub(super) const RECOVERY_SECRET_ENV: &str = "PLOYZ_RECOVERY_SECRET";

/// Resolve founder recovery authority without putting it in argv or persisting
/// plaintext. An explicit environment value supports automation; otherwise a
/// fresh secret is shown once for the operator to retain.
pub(crate) fn resolve_founder_recovery_secret() -> String {
    match non_empty_environment_secret() {
        Some(secret) => secret,
        None => {
            let generated = secret::generate_recovery_secret();
            eprintln!(
                "cluster recovery secret (save this — required to promote a new core):\n    {generated}"
            );
            generated
        }
    }
}

/// Read promotion recovery authority from the environment or a hidden terminal
/// prompt. The secret is never accepted through argv or written to disk.
pub(super) fn read_promotion_recovery_secret() -> Result<String, String> {
    if let Some(secret) = non_empty_environment_secret() {
        return Ok(secret);
    }
    if std::io::stdin().is_terminal() {
        rpassword::prompt_password("Cluster recovery secret: ")
            .map_err(|error| format!("failed to read recovery secret: {error}"))
    } else {
        Err(format!(
            "set {RECOVERY_SECRET_ENV}, or run interactively to be prompted for it"
        ))
    }
}

fn non_empty_environment_secret() -> Option<String> {
    std::env::var(RECOVERY_SECRET_ENV)
        .ok()
        .filter(|value| !value.is_empty())
}
