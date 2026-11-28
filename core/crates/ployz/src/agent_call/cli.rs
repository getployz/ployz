//! The CLI's one wrapper: a reason is required when a coding agent runs a command, and the
//! run is reported once it ends. A run with no agent behind it is neither.

use std::path::PathBuf;

use clap::{ArgMatches, Command};
use ployz_core::Hint;

use super::agent;
use super::{CallError, CallReport, Client, Kind, Started, Surface};
use crate::cloud;
use crate::cloud::account::Credential;
use crate::cloud::login::CredentialStore;
use crate::cloud::origin::CloudOrigin;
use crate::failure::Failure;

pub(crate) const WHY: &str = "why";

const REASON_REQUIRED: &str =
    "Say why you are running this. Add `--why \"<the goal this command serves>\"`.";
const PLACEHOLDER: &str = "the goal this command serves";

/// One CLI run, from before its command dispatches to after it ends.
pub(crate) struct Run {
    tool: String,
    why: Option<String>,
    /// A reason is required: a coding agent is running this, and the command is not one
    /// that needs none.
    required: bool,
    /// Where the report goes once the run ends, when it is reported at all.
    report: Option<Reported>,
    started: Started,
}

struct Reported {
    config: PathBuf,
    agent: &'static str,
}

impl Run {
    /// Look at the parsed command line before it runs. `internal` is a command only Ployz
    /// runs on itself, which needs no reason.
    pub(crate) fn begin(matches: &ArgMatches, command: &Command, internal: bool) -> Self {
        let started = Started::now();
        let tool = crate::cli::command_path(matches);
        let why = matches
            .try_get_one::<String>(WHY)
            .ok()
            .flatten()
            .map(|why| why.trim().to_owned())
            // The refusal's own placeholder, pasted as printed, gives no reason either.
            .filter(|why| !why.is_empty() && *why != format!("<{PLACEHOLDER}>"));
        let agent = agent::name().filter(|_| !internal && !exempt(matches, command, &tool));
        let separate = crate::cli::connection(matches).is_some()
            || matches!(matches.try_get_one::<String>("context"), Ok(Some(_)));
        let report = agent.filter(|_| !separate).and_then(|agent| {
            let config = crate::cli::config_path(crate::cli::leaf_matches(matches)).ok()?;
            Some(Reported { config, agent })
        });
        Self {
            tool,
            why,
            required: agent.is_some(),
            report,
            started,
        }
    }

    /// The refusal when a reason is required and none was given, with the command line
    /// `command` parsed into `matches` shown again with `--why` added.
    pub(crate) fn refusal(&self, matches: &ArgMatches, command: &Command) -> Option<Failure> {
        if !self.required || self.why.is_some() {
            return None;
        }
        let again =
            crate::cli::Rerun::again_in(command.clone(), matches).placeholder(WHY, PLACEHOLDER);
        Some(Failure::usage(REASON_REQUIRED).hint(Hint::Next(again.line())))
    }

    /// Report the run, when it counts: after the command ended, within one short wait.
    pub(crate) fn finish(self, result: &Result<(), Failure>) {
        let Some(reported) = self.report else {
            return;
        };
        let error = result.as_ref().err().and_then(CallError::of_failure);
        let report = CallReport {
            tool: self.tool,
            intent: self.why,
            error,
            client: Client {
                name: Some(reported.agent.to_owned()),
                version: None,
            },
            ..CallReport::new(
                Surface::Cli,
                Kind::Call,
                self.started,
                agent::cli_session(reported.agent),
            )
        };
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(async {
            if let Some(credential) = credential(&reported.config).await {
                super::send(&credential, &report).await;
            }
        });
    }
}

/// Commands that need no reason: version, help, shell completion, and `ployz mcp`, whose
/// calls each carry their own.
fn exempt(matches: &ArgMatches, command: &Command, path: &str) -> bool {
    if matches.get_flag("version") || matches.subcommand().is_none() {
        return true;
    }
    if path == "mcp" || path.split(' ').next() == Some("completion") {
        return true;
    }
    // A group without its subcommand prints its help.
    path.split(' ')
        .try_fold(command, |command, name| command.find_subcommand(name))
        .is_some_and(Command::has_subcommands)
}

/// The credential this device holds, without going to Cloud for it: `PLOYZ_TOKEN`, else
/// the sign-in stored beside the config.
async fn credential(config: &std::path::Path) -> Option<Credential> {
    let store = CredentialStore::beside(config);
    match cloud::account::env_token() {
        Some(token) => cloud::account::credential(&store, Some(token), CloudOrigin::named_by_env())
            .await
            .ok(),
        None => store.stored_sign_in().map(Credential::Device),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(words: &[&str]) -> String {
        let command = crate::cli::command();
        let matches = command
            .clone()
            .try_get_matches_from(std::iter::once("ployz").chain(words.iter().copied()))
            .unwrap();
        let run = Run {
            tool: crate::cli::command_path(&matches),
            why: None,
            required: true,
            report: None,
            started: Started::now(),
        };
        let hints = run.refusal(&matches, &command).unwrap().hints();
        let [Hint::Next(line)] = hints.as_slice() else {
            panic!("{hints:?}");
        };
        line.clone()
    }

    /// The line keeps every flag typed and never repeats a secret it carried.
    #[test]
    fn the_next_line_is_the_same_command_with_why_added_and_no_secret() {
        assert_eq!(
            refused(&["set", "web.env.TOKEN=hunter2", "--env", "staging"]),
            "ployz --why '<the goal this command serves>' set 'web.env.TOKEN=<VALUE>' --env staging"
        );
        assert_eq!(
            refused(&["--connect", "ssh://root:hunter2@203.0.113.7", "status"]),
            "ployz --connect '<CONNECT>' --why '<the goal this command serves>' status"
        );
        assert_eq!(
            refused(&[
                "exec",
                "web",
                "--",
                "env",
                "AUTH_TOKEN=hunter2",
                "--why",
                "x"
            ]),
            "ployz --why '<the goal this command serves>' exec web -- '<COMMAND>'"
        );
    }
}
