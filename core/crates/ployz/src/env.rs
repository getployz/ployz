//! The environment variables Ployz reads, and the one parser of a test-mode duration.

pub const AUTO_CONFIRM: &str = "PLOYZ_AUTO_CONFIRM";
pub const BUILD_GRANT: &str = "PLOYZ_BUILD_GRANT";
pub const CLOUD_URL: &str = "PLOYZ_CLOUD_URL";
/// Set by the shell hook `ployz completion SHELL` prints, to ask for completions.
pub const COMPLETE: &str = "PLOYZ_COMPLETE";
pub const CONFIG: &str = "PLOYZ_CONFIG";
pub const CONNECT: &str = "PLOYZ_CONNECT";
pub const CONTEXT: &str = "PLOYZ_CONTEXT";
pub const DAEMON_VERSION: &str = "PLOYZ_DAEMON_VERSION";
/// The enrollment token for `server add`, kept out of argv.
pub const ENROLL_TOKEN: &str = "PLOYZ_ENROLL_TOKEN";
pub const TOKEN: &str = "PLOYZ_TOKEN";
/// The Environment authoring commands address, as `--env`.
pub const ENVIRONMENT: &str = "PLOYZ_ENV";
/// The Project authoring commands address, as `--project`.
pub const PROJECT: &str = "PLOYZ_PROJECT";
/// Hidden test mode: host the Config Store in-process (`sqlite:PATH`). Not for users.
pub const STORE: &str = "PLOYZ_STORE";
/// Hidden test mode: seconds a followed Deployment sits queued before the follow says why.
pub const DEPLOY_UNCLAIMED_SECONDS: &str = "PLOYZ_DEPLOY_UNCLAIMED_SECONDS";
/// Hidden test mode: seconds a read waits on a target Machine before calling it silent.
pub const TARGET_READ_TIMEOUT_SECONDS: &str = "PLOYZ_TARGET_READ_TIMEOUT_SECONDS";

/// A hidden test-mode duration in whole seconds, or `None` when unset or unparsable.
pub(crate) fn seconds(name: &str) -> Option<std::time::Duration> {
    let seconds = std::env::var(name).ok()?.parse().ok()?;
    Some(std::time::Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_names_are_the_ones_users_set() {
        assert_eq!(
            [
                AUTO_CONFIRM,
                BUILD_GRANT,
                CLOUD_URL,
                COMPLETE,
                CONFIG,
                CONNECT,
                CONTEXT,
                DAEMON_VERSION,
                DEPLOY_UNCLAIMED_SECONDS,
                ENROLL_TOKEN,
                ENVIRONMENT,
                PROJECT,
                STORE,
                TARGET_READ_TIMEOUT_SECONDS,
                TOKEN,
            ],
            [
                "PLOYZ_AUTO_CONFIRM",
                "PLOYZ_BUILD_GRANT",
                "PLOYZ_CLOUD_URL",
                "PLOYZ_COMPLETE",
                "PLOYZ_CONFIG",
                "PLOYZ_CONNECT",
                "PLOYZ_CONTEXT",
                "PLOYZ_DAEMON_VERSION",
                "PLOYZ_DEPLOY_UNCLAIMED_SECONDS",
                "PLOYZ_ENROLL_TOKEN",
                "PLOYZ_ENV",
                "PLOYZ_PROJECT",
                "PLOYZ_STORE",
                "PLOYZ_TARGET_READ_TIMEOUT_SECONDS",
                "PLOYZ_TOKEN",
            ]
        );
    }
}
