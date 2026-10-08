use clap::{Arg, ArgAction, Command, ValueHint};

use crate::handlers;

pub mod env {
    pub const AUTO_CONFIRM: &str = "PLOYZ_AUTO_CONFIRM";
    pub const BUILD_GRANT: &str = "PLOYZ_BUILD_GRANT";
    pub const CLOUD_URL: &str = "PLOYZ_CLOUD_URL";
    /// Set by the shell hook `ployz completion SHELL` prints, to ask for completions.
    pub const COMPLETE: &str = "PLOYZ_COMPLETE";
    pub const CONFIG: &str = "PLOYZ_CONFIG";
    pub const CONNECT: &str = "PLOYZ_CONNECT";
    pub const CONTEXT: &str = "PLOYZ_CONTEXT";
    pub const DAEMON_VERSION: &str = "PLOYZ_DAEMON_VERSION";
    pub const TOKEN: &str = "PLOYZ_TOKEN";
    /// The Environment authoring commands address, as `--env`.
    pub const ENVIRONMENT: &str = "PLOYZ_ENV";
    /// The Project authoring commands address, as `--project`.
    pub const PROJECT: &str = "PLOYZ_PROJECT";
    /// Hidden test mode: host the Config Store in-process (`sqlite:PATH`). Not for users.
    pub const STORE: &str = "PLOYZ_STORE";
}

#[must_use]
pub fn command() -> Command {
    let command = base("ployz", "Manage Ployz machines, services, and volumes")
        .styles(crate::ui::clap_styles())
        .after_help(handlers::catalog::SIGNPOST)
        .arg(switch("version", Some('V')).help("Print version"))
        .arg(
            switch("json", None)
                .global(true)
                .help("Print the result as one JSON object on stdout"),
        )
        .arg(
            value("color", None)
                .global(true)
                .value_name("WHEN")
                .value_parser(["auto", "always", "never"])
                .default_value("auto")
                .help("When to color output; overrides NO_COLOR and CLICOLOR"),
        )
        .subcommand(handlers::build::command())
        .subcommand(handlers::cloud::command())
        .subcommand(handlers::config_item::command())
        .subcommand(handlers::context::command())
        .subcommand(handlers::deploy::deploy_command())
        .subcommand(handlers::deploy::deployment_command())
        .subcommand(handlers::review::diff_command())
        .subcommand(handlers::review::discard_command())
        .subcommand(handlers::domain::command())
        .subcommand(handlers::env::command())
        .subcommand(handlers::operator::exec_command())
        .subcommand(handlers::catalog::explain_command())
        .subcommand(handlers::config::get_command())
        .subcommand(handlers::link::link_command())
        .subcommand(handlers::github::command())
        .subcommand(handlers::login::login_command())
        .subcommand(handlers::login::logout_command())
        .subcommand(handlers::operator::logs_command())
        .subcommand(handlers::mcp::command())
        .subcommand(handlers::account::org_command())
        .subcommand(handlers::server::command())
        .subcommand(handlers::service::command())
        .subcommand(handlers::account::token_command())
        .subcommand(handlers::project::command())
        .subcommand(handlers::operator::ps_command())
        .subcommand(handlers::review::publish_command())
        .subcommand(handlers::catalog::schema_command())
        .subcommand(handlers::config::set_command())
        .subcommand(handlers::link::status_command())
        .subcommand(handlers::config::unset_command())
        .subcommand(handlers::up::command())
        .subcommand(handlers::volume::command())
        .subcommand(completion());
    handlers::debug::attach(command)
}

pub(crate) fn base(name: &'static str, about: &'static str) -> Command {
    Command::new(name).about(about).args(connection_args(true))
}

pub(crate) fn connection_args(include_context: bool) -> Vec<Arg> {
    let mut args = vec![
        value("connect", None).env(env::CONNECT).global(true),
        value("ssh-timeout", None)
            .value_name("SECONDS")
            .help("SSH setup timeout in seconds (provisioning: network connection only)")
            .long_help("SSH setup timeout in seconds. Management commands use noninteractive authentication. During provisioning, only the network connection is timed; SSH/sudo authentication and installer execution have no deadline.")
            .value_parser(clap::value_parser!(u32).range(1..))
            // A fresh SSH login to a distant Server takes 4–5s on its own (Bali → Sydney measured 4.1–4.7s).
            .default_value("20")
            .global(true),
        value("ployz-config", None)
            .env(env::CONFIG)
            .default_value("~/.config/ployz/config.yaml")
            .value_hint(ValueHint::FilePath)
            // PLOYZ_CONFIG is the way to point elsewhere; the flag stays for tests.
            .hide(true)
            .global(true),
    ];
    if include_context {
        args.push(value("context", Some('c')).env(env::CONTEXT));
    }
    args
}

/// SSH setup budget shared by every CLI connection path.
pub(crate) fn ssh_timeout(matches: &clap::ArgMatches) -> std::time::Duration {
    std::time::Duration::from_secs(u64::from(
        *matches
            .get_one::<u32>("ssh-timeout")
            .expect("ssh-timeout has a default"),
    ))
}

pub(crate) fn value(name: &'static str, short: Option<char>) -> Arg {
    let arg = Arg::new(name).long(name).action(ArgAction::Set);
    match short {
        Some(short) => arg.short(short),
        None => arg,
    }
}

pub(crate) fn many(name: &'static str, short: Option<char>) -> Arg {
    value(name, short)
        .action(ArgAction::Append)
        .value_delimiter(',')
}

pub(crate) fn repeated(name: &'static str) -> Arg {
    value(name, None).action(ArgAction::Append)
}

pub(crate) fn switch(name: &'static str, short: Option<char>) -> Arg {
    let arg = Arg::new(name).long(name).action(ArgAction::SetTrue);
    match short {
        Some(short) => arg.short(short),
        None => arg,
    }
}

pub(crate) fn positional(name: &'static str, required: bool) -> Arg {
    Arg::new(name).required(required).action(ArgAction::Set)
}

pub(crate) fn trailing(name: &'static str) -> Arg {
    Arg::new(name)
        .num_args(0..)
        .action(ArgAction::Append)
        .allow_hyphen_values(true)
        .trailing_var_arg(true)
}

pub(crate) fn log_flags(command: Command) -> Command {
    command
        .arg(switch("follow", Some('f')).help("Keep streaming new lines"))
        .arg(many("machine", Some('m')).help("Only these Servers"))
        .arg(value("since", None).help("Lines after this time: 10m, 2h or an RFC 3339 timestamp"))
        // Like `docker compose logs --tail`: per Container, not overall.
        .arg(
            value("tail", Some('n'))
                .default_value("100")
                .help("Lines from the end of each Container's log; all for everything"),
        )
        .arg(value("until", None).help("Lines before this time: 10m, 2h or an RFC 3339 timestamp"))
        .arg(switch("utc", None).help("Show times in UTC"))
}

pub(crate) fn machine_policy_flags(command: Command) -> Command {
    command
        .arg(many("label-add", None).value_name("KEY=VALUE"))
        .args(
            ["accepts-builds", "accepts-services", "accepts-ingress"]
                .map(|name| value(name, None).value_parser(clap::value_parser!(bool))),
        )
}

/// `--expect-version`: a data-loss refusal's version, which its `--accept-volume-loss` binds to.
pub(crate) fn reviewed_version() -> Arg {
    value("expect-version", None)
        .value_name("VERSION")
        .value_parser(review_version)
        .help("Refuse unless this is still the reviewed version; a data-loss refusal names it")
}

/// A review's version is `REVISION:SAVED:TOKEN`, and a data-loss refusal's adds `:DIGEST`.
fn review_version(version: &str) -> Result<String, &'static str> {
    let parts: Vec<&str> = version.split(':').collect();
    match parts.as_slice() {
        [revision, saved, rest @ ..]
            if !rest.is_empty()
                && revision.parse::<u64>().is_ok()
                && saved.parse::<u64>().is_ok()
                && rest.iter().all(|part| !part.is_empty()) =>
        {
            Ok(version.to_owned())
        }
        _ => Err("Use the version ployz diff --json or a refusal gave, such as 3:1:0.1"),
    }
}

pub(crate) fn approval() -> Arg {
    value("approval", None).value_name("ID").help(
        "Go ahead with the approval a human gave in Ployz Cloud; an approval refusal names it",
    )
}

pub(crate) fn volume_acceptance() -> Arg {
    repeated("accept-volume-loss")
        .num_args(1)
        .value_name("name")
        .help("Accept permanent deletion: repeat once per exact volume name in the full deletion list; --yes cannot bypass this")
}

fn completion() -> Command {
    base("completion", "Generate shell completion")
        .arg(positional("shell", true).value_parser(clap::value_parser!(clap_complete::Shell)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn expect_version_takes_a_review_or_refusal_version_only() {
        for good in ["3:1:0.1", "3:0:0.12:ab12cd"] {
            assert_eq!(super::review_version(good).as_deref(), Ok(good));
        }
        for bad in ["garbage", "3:1", "3:1:", "x:1:0.1", "3:1:0.1:"] {
            assert!(super::review_version(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn root_version_flags_are_accepted() {
        for flag in ["--version", "-V"] {
            let matches = super::command()
                .try_get_matches_from(["ployz", flag])
                .unwrap();
            assert!(matches.get_flag("version"), "{flag}");
        }
    }

    #[test]
    fn machine_provisioning_defaults_to_cli_version_and_accepts_overrides() {
        for command in ["add"] {
            for version in [None, Some("stable"), Some("beta"), Some("1.2.3")] {
                let mut args = vec!["ployz", "server", command, "root@example.com"];
                if let Some(version) = version {
                    args.extend(["--version", version]);
                }
                let matches = super::command().try_get_matches_from(args).unwrap();
                let matches = matches
                    .subcommand_matches("server")
                    .unwrap()
                    .subcommand_matches(command)
                    .unwrap();
                assert_eq!(
                    matches
                        .get_one::<ployz_core::MachineRelease>("version")
                        .map(ToString::to_string)
                        .as_deref(),
                    Some(version.unwrap_or(env!("CARGO_PKG_VERSION")))
                );
            }
        }
    }

    #[test]
    fn removal_acceptance_requires_explicit_repeatable_volume_flags() {
        assert!(
            super::command()
                .try_get_matches_from([
                    "ployz",
                    "server",
                    "rm",
                    "worker",
                    "--accept-volume-loss",
                    "data",
                ])
                .is_ok()
        );
        assert!(
            super::command()
                .try_get_matches_from([
                    "ployz",
                    "server",
                    "rm",
                    "worker",
                    "--no-reset",
                    "--accept-volume-loss",
                    "data",
                ])
                .is_err()
        );
    }
}
