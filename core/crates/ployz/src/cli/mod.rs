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
    pub const ENVIRONMENT: &str = "PLOYZ_ENV";
    pub const PROJECT: &str = "PLOYZ_PROJECT";
    /// Hidden test mode: host the Config Store in-process (`sqlite:PATH`). Not for users.
    pub const STORE: &str = "PLOYZ_STORE";
}

#[must_use]
pub fn command() -> Command {
    base("ployz", "Manage Ployz machines, services, and volumes")
        .arg(switch("version", Some('V')).help("Print version"))
        .arg(
            switch("json", None)
                .global(true)
                .help("Print the result as one JSON object on stdout"),
        )
        .subcommand(handlers::account::billing_command())
        .subcommand(handlers::build::command())
        .subcommand(handlers::cloud::command())
        .subcommand(handlers::context::command())
        .subcommand(handlers::review::diff_command())
        .subcommand(handlers::review::discard_command())
        .subcommand(handlers::env::command())
        .subcommand(handlers::catalog::explain_command())
        .subcommand(handlers::config::get_command())
        .subcommand(handlers::ingress::command())
        .subcommand(handlers::login::login_command())
        .subcommand(handlers::login::logout_command())
        .subcommand(handlers::machine::command())
        .subcommand(handlers::account::org_command())
        .subcommand(handlers::server::command())
        .subcommand(handlers::service::command())
        .subcommand(handlers::account::token_command())
        .subcommand(handlers::project::command())
        .subcommand(handlers::review::publish_command())
        .subcommand(handlers::catalog::schema_command())
        .subcommand(handlers::config::set_command())
        .subcommand(handlers::config::unset_command())
        .subcommand(handlers::volume::command())
        .subcommand(completion())
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
            .default_value("5")
            .global(true),
        value("ployz-config", None)
            .env(env::CONFIG)
            .default_value("~/.config/ployz/config.yaml")
            .value_hint(ValueHint::FilePath)
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
        .arg(switch("follow", Some('f')))
        .arg(many("machine", Some('m')))
        .arg(value("since", None))
        .arg(value("tail", Some('n')).default_value("100"))
        .arg(value("until", None))
        .arg(switch("utc", None))
}

pub(crate) fn machine_policy_flags(command: Command) -> Command {
    command
        .arg(many("label-add", None).value_name("KEY=VALUE"))
        .args(
            ["accepts-builds", "accepts-services", "accepts-ingress"]
                .map(|name| value(name, None).value_parser(clap::value_parser!(bool))),
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
    fn machine_rm_no_reset_help_says_when_to_skip_reset() {
        let help = super::command()
            .find_subcommand("machine")
            .expect("machine")
            .find_subcommand("rm")
            .expect("rm")
            .get_arguments()
            .find(|arg| arg.get_id() == "no-reset")
            .expect("no-reset")
            .get_help()
            .map(ToString::to_string)
            .expect("help text");
        assert_eq!(
            help,
            "Remove the Machine from the Cluster without resetting it; use when the Machine is unreachable"
        );
    }

    #[test]
    fn removal_acceptance_requires_explicit_repeatable_volume_flags() {
        for args in [
            vec![
                "ployz",
                "machine",
                "rm",
                "worker",
                "--accept-volume-loss",
                "data",
            ],
            vec![
                "ployz",
                "service",
                "rm",
                "app/db",
                "--volumes",
                "--accept-volume-loss",
                "data",
            ],
        ] {
            assert!(super::command().try_get_matches_from(args).is_ok());
        }
        for args in [
            vec![
                "ployz",
                "machine",
                "rm",
                "worker",
                "--no-reset",
                "--accept-volume-loss",
                "data",
            ],
            vec!["ployz", "volume", "rm", "--force", "--yes"],
        ] {
            assert!(super::command().try_get_matches_from(args).is_err());
        }
    }

    #[test]
    fn machine_rm_takes_data_loss_names_as_arguments_and_yes_still_parses() {
        let matches = super::command()
            .try_get_matches_from([
                "ployz",
                "machine",
                "rm",
                "worker",
                "--accept-volume-loss",
                "data",
                "--accept-volume-loss",
                "logs",
                "--yes",
            ])
            .unwrap();
        let rm = matches
            .subcommand_matches("machine")
            .unwrap()
            .subcommand_matches("rm")
            .unwrap();
        assert!(rm.get_flag("yes"));
        assert_eq!(
            rm.get_many::<String>("accept-volume-loss")
                .unwrap()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["data", "logs"]
        );
        let yes_only = super::command()
            .try_get_matches_from(["ployz", "machine", "rm", "worker", "--yes"])
            .unwrap();
        let rm = yes_only
            .subcommand_matches("machine")
            .unwrap()
            .subcommand_matches("rm")
            .unwrap();
        assert!(rm.get_flag("yes"));
        assert!(rm.get_many::<String>("accept-volume-loss").is_none());
    }

    #[test]
    fn service_rm_volumes_takes_long_data_loss_and_yes() {
        let matches = super::command()
            .try_get_matches_from([
                "ployz",
                "service",
                "rm",
                "db",
                "--volumes",
                "--yes",
                "--accept-volume-loss",
                "app_data",
            ])
            .unwrap();
        let rm = rm_matches(&matches);
        assert_eq!(
            rm.get_one::<String>("service").map(String::as_str),
            Some("db")
        );
        assert!(rm.get_flag("volumes"));
        assert!(rm.get_flag("yes"));
        assert_eq!(
            rm.get_many::<String>("accept-volume-loss")
                .unwrap()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["app_data"]
        );

        let comma = super::command()
            .try_get_matches_from([
                "ployz",
                "service",
                "rm",
                "db",
                "--volumes",
                "--accept-volume-loss",
                "app_data,app_logs",
                "--yes",
            ])
            .unwrap();
        let rm = rm_matches(&comma);
        assert_eq!(
            rm.get_many::<String>("accept-volume-loss")
                .unwrap()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["app_data,app_logs"]
        );

        assert!(
            super::command()
                .try_get_matches_from([
                    "ployz",
                    "service",
                    "rm",
                    "db",
                    "--accept-volume-loss",
                    "app_data"
                ])
                .is_err()
        );
        assert!(
            super::command()
                .try_get_matches_from(["ployz", "service", "start", "db", "--volumes"])
                .is_err()
        );
        assert!(
            super::command()
                .try_get_matches_from(["ployz", "service", "rm", "db", "--volumes", "-v"])
                .is_err()
        );

        let repeated = super::command()
            .try_get_matches_from([
                "ployz",
                "service",
                "rm",
                "db",
                "--volumes",
                "--accept-volume-loss",
                "app_data",
                "--accept-volume-loss",
                "app_logs",
                "--yes",
            ])
            .unwrap();
        let rm = rm_matches(&repeated);
        assert_eq!(
            rm.get_many::<String>("accept-volume-loss")
                .unwrap()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["app_data", "app_logs"]
        );

        let trailing = super::command()
            .try_get_matches_from([
                "ployz",
                "service",
                "rm",
                "db",
                "--volumes",
                "--accept-volume-loss",
                "app_data",
                "api",
                "--yes",
            ])
            .unwrap();
        let rm = rm_matches(&trailing);
        assert_eq!(
            rm.get_many::<String>("service")
                .unwrap()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["db", "api"]
        );
        assert_eq!(
            rm.get_many::<String>("accept-volume-loss")
                .unwrap()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["app_data"]
        );
    }

    fn rm_matches(matches: &clap::ArgMatches) -> &clap::ArgMatches {
        matches
            .subcommand_matches("service")
            .unwrap()
            .subcommand_matches("rm")
            .unwrap()
    }

    #[test]
    fn volume_rm_still_takes_volume_names_with_yes() {
        let matches = super::command()
            .try_get_matches_from(["ployz", "volume", "rm", "data", "logs", "--yes"])
            .unwrap();
        let rm = matches
            .subcommand_matches("volume")
            .unwrap()
            .subcommand_matches("rm")
            .unwrap();
        assert!(rm.get_flag("yes"));
        assert_eq!(
            rm.get_many::<String>("volume-name")
                .unwrap()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["data", "logs"]
        );
    }
}
