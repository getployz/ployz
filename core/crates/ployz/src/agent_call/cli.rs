//! The CLI's one wrapper: a reason is required when a coding agent runs a command, and the
//! run is reported once it ends. A run with no agent behind it is neither.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{ArgMatches, Command};
use ployz_core::Hint;

use super::agent;
use super::{CallError, CallReport, Client, Kind, Started, Surface};
use crate::cloud_account::{self, Credential};
use crate::cloud_login::CredentialStore;
use crate::cloud_origin::CloudOrigin;
use crate::failure::Failure;
use crate::handlers::verbs::{self, Surface as VerbSurface};

/// The `--why` flag.
pub(crate) const WHY: &str = "why";

const REASON_REQUIRED: &str =
    "Say why you are running this. Add `--why \"<the goal this command serves>\"`.";
const PLACEHOLDER: &str = "<the goal this command serves>";

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
    /// Look at the parsed command line before it runs.
    pub(crate) fn begin(matches: &ArgMatches, command: &Command) -> Self {
        let started = Started::now();
        let tool = crate::handlers::command_path(matches);
        let why = matches
            .try_get_one::<String>(WHY)
            .ok()
            .flatten()
            .map(|why| why.trim().to_owned())
            .filter(|why| !why.is_empty());
        let agent = agent::name().filter(|_| !exempt(matches, command, &tool));
        let separate = ["connect", "context"]
            .into_iter()
            .any(|flag| matches.try_get_one::<String>(flag).ok().flatten().is_some());
        let report = agent.filter(|_| !separate).and_then(|agent| {
            let config =
                crate::handlers::config_path(crate::handlers::leaf_matches(matches)).ok()?;
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

    /// The refusal when a reason is required and none was given, with the same command
    /// line shown with `--why` added.
    pub(crate) fn refusal(&self, args: &[OsString]) -> Option<Failure> {
        if !self.required || self.why.is_some() {
            return None;
        }
        Some(Failure::usage(REASON_REQUIRED).hint(Hint::Next(with_why(args))))
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
/// calls each carry their own. Internal verbs are Ployz running itself.
fn exempt(matches: &ArgMatches, command: &Command, path: &str) -> bool {
    if matches.get_flag("version") || matches.subcommand().is_none() {
        return true;
    }
    if path == "mcp" || path.split(' ').next() == Some("completion") {
        return true;
    }
    if verbs::find(path).is_some_and(|verb| verb.surface == VerbSurface::Internal) {
        return true;
    }
    // A group without its subcommand prints its help.
    path.split(' ')
        .try_fold(command, |command, name| command.find_subcommand(name))
        .is_some_and(Command::has_subcommands)
}

/// The command line as typed, with `--why` added.
fn with_why(args: &[OsString]) -> String {
    let mut line = vec!["ployz".to_owned()];
    let mut args = args.iter().map(|arg| arg.to_string_lossy().into_owned());
    while let Some(arg) = args.next() {
        if arg == "--why" {
            args.next();
            continue;
        }
        if arg.starts_with("--why=") {
            continue;
        }
        line.push(if arg.contains(char::is_whitespace) || arg.is_empty() {
            format!("'{arg}'")
        } else {
            arg
        });
    }
    line.push(format!("--why \"{PLACEHOLDER}\""));
    line.join(" ")
}

/// The credential this device holds, without going to Cloud for it: `PLOYZ_TOKEN`, else
/// the sign-in stored beside the config.
async fn credential(config: &std::path::Path) -> Option<Credential> {
    let store = CredentialStore::beside(config);
    match cloud_account::env_token() {
        Some(token) => cloud_account::credential(&store, Some(token), CloudOrigin::named_by_env())
            .await
            .ok(),
        None => store.stored_sign_in().map(Credential::Device),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_next_line_is_the_same_command_with_why_added() {
        let args = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            with_why(&args(&["status", "--json"])),
            "ployz status --json --why \"<the goal this command serves>\""
        );
        assert_eq!(
            with_why(&args(&["--why", "", "set", "web.image=x y"])),
            "ployz set 'web.image=x y' --why \"<the goal this command serves>\""
        );
        assert_eq!(
            with_why(&args(&["--why=", "docs"])),
            "ployz docs --why \"<the goal this command serves>\""
        );
    }
}
