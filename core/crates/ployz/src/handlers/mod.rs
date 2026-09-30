use std::{future::Future, path::Path, pin::Pin};

use crate::cancellation::on_ctrl_c as cancellation_on_ctrl_c;
use clap::{ArgMatches, Command};
use clap_complete::Shell;
use clap_complete::env::Shells;

use crate::failure::{Failure, USAGE_EXIT};

pub(crate) mod account;
pub(crate) mod build;
pub(crate) mod catalog;
pub(crate) mod cloud;
pub(crate) mod config;
pub(crate) mod context;
mod data_loss;
pub(crate) mod deploy;
pub(crate) mod domain;
pub(crate) mod env;
pub(crate) mod github;
pub(crate) mod link;
pub(crate) mod login;
pub(crate) mod operator;
pub(crate) mod project;
pub(crate) mod review;
pub(crate) mod server;
pub(crate) mod service;
pub(crate) mod setup;
pub(crate) mod store;
mod teardown;
pub(crate) mod up;
pub(crate) mod volume;

#[doc(hidden)]
pub use cloud::enroll_with_installer as cloud_enroll_with_installer;

pub type Error = Failure;

pub fn run() -> Result<(), Error> {
    let mut command = crate::cli::command();
    // Only root help shows the footer, so skip reading the skill otherwise.
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        command = command.after_help(setup::help_footer());
    }
    let matches = command.clone().try_get_matches().map_err(usage_failure)?;
    crate::output::set_json(matches.get_flag("json"));
    dispatch(&matches, &mut command)
}

/// Report a rejected command line: clap's own rendering, or one JSON error under `--json`.
fn usage_failure(error: clap::Error) -> Error {
    use clap::error::ErrorKind;
    let wants_json = std::env::args_os()
        .take_while(|arg| arg != "--")
        .any(|arg| arg == "--json");
    if !wants_json
        || matches!(
            error.kind(),
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
        )
    {
        error.exit();
    }
    crate::output::set_json(true);
    let message = error.render().to_string();
    Error::usage(message.trim().trim_start_matches("error: ").to_owned())
        .with_exit(u8::try_from(error.exit_code()).unwrap_or(USAGE_EXIT))
}

fn dispatch(matches: &ArgMatches, command: &mut Command) -> Result<(), Error> {
    if matches.get_flag("version") {
        return crate::output::finish(
            &serde_json::json!({ "version": env!("CARGO_PKG_VERSION") }),
            || crate::output::say!("{}", env!("CARGO_PKG_VERSION")),
        );
    }
    if matches.subcommand().is_none() {
        if matches.get_flag("json") {
            return Err(Error::usage("a command is required").with_exit(USAGE_EXIT));
        }
        command.print_help()?;
        crate::output::say!();
        return Ok(());
    }
    let path = command_path(matches);
    // Every leaf has a handler, so a missing one means a group without its subcommand.
    let handler = handler_for(&path).ok_or_else(|| {
        Error::usage(format!("ployz {path} requires a subcommand")).with_exit(USAGE_EXIT)
    })?;
    if json_refused(&path) && matches.get_flag("json") {
        return Err(Error::usage(format!(
            "ployz {path} does not support --json"
        )));
    }
    handler(matches)
}

/// Print the shell hook that asks `ployz` itself for completions, so Setting paths
/// and Service names complete from the catalog and the Store.
fn completion(root: &ArgMatches) -> Result<(), Error> {
    let shell = leaf_matches(root)
        .get_one::<Shell>("shell")
        .copied()
        .ok_or_else(|| Error::usage("completion shell is required"))?;
    let shells = Shells::builtins();
    let completer = shells
        .completer(&shell.to_string())
        .ok_or_else(|| Error::usage("unsupported completion shell"))?;
    completer.write_registration(
        crate::cli::env::COMPLETE,
        "ployz",
        "ployz",
        "ployz",
        &mut std::io::stdout(),
    )?;
    Ok(())
}

fn command_path(mut matches: &ArgMatches) -> String {
    let mut parts = Vec::new();
    while let Some((name, child)) = matches.subcommand() {
        parts.push(name);
        matches = child;
    }
    parts.join(" ")
}

/// Items as one line of text: `a, b, c`.
pub(crate) fn joined<T: ToString>(items: &[T]) -> String {
    items
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn leaf_matches(mut matches: &ArgMatches) -> &ArgMatches {
    while let Some((_, child)) = matches.subcommand() {
        matches = child;
    }
    matches
}

fn string_values(matches: &ArgMatches, id: &str) -> Vec<String> {
    if matches.try_contains_id(id).ok() != Some(true) {
        return Vec::new();
    }
    if matches.value_source(id) == Some(clap::parser::ValueSource::DefaultValue) {
        return Vec::new();
    }
    matches
        .try_get_many::<String>(id)
        .ok()
        .flatten()
        .map(|values| values.cloned().collect())
        .unwrap_or_default()
}

fn required(matches: &ArgMatches, name: &str) -> Result<String, Error> {
    matches
        .get_one::<String>(name)
        .cloned()
        .ok_or_else(|| Error::usage(format!("{name} is required")))
}

fn runtime() -> Result<tokio::runtime::Runtime, Error> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

pub(super) fn config_path(matches: &ArgMatches) -> Result<std::path::PathBuf, Error> {
    matches
        .get_one::<String>("ployz-config")
        .map(Path::new)
        .map(crate::context::expand_home)
        .ok_or_else(|| Error::usage("Ployz config path is required"))
}

/// The explicit connection, the selected context, or the local daemon; never Cloud.
async fn connect_context(
    matches: &ArgMatches,
    context: Option<&str>,
) -> Result<crate::connect::Client, Error> {
    Ok(crate::connect::connect_with_ssh_timeout(
        &config_path(matches)?,
        matches.get_one::<String>("connect").map(String::as_str),
        context,
        crate::cli::ssh_timeout(matches),
    )
    .await?)
}

/// Re-establish a management connection after setup has already started.
async fn reconnect_client(
    matches: &ArgMatches,
    context: Option<&str>,
) -> Result<crate::connect::Client, Error> {
    let config = config_path(matches)?;
    let connect = matches.get_one::<String>("connect").map(String::as_str);
    crate::setup_retry::run(
        &mut (),
        "Reconnecting to the Cluster",
        crate::setup_retry::WAIT,
        crate::connect::ConnectError::is_setup_retryable,
        async |_| {
            crate::connect::connect_with_ssh_timeout(
                &config,
                connect,
                context,
                crate::cli::ssh_timeout(matches),
            )
            .await
        },
    )
    .await
    .map_err(Into::into)
}

fn recovery_command(matches: &ArgMatches, context: &str, command: &[&str]) -> String {
    let config = config_path(matches).expect("setup already resolved the config path");
    let config = config.to_string_lossy();
    let args = ["ployz", "--ployz-config", config.as_ref()]
        .into_iter()
        .chain(command.iter().copied())
        .chain(["--context", context]);
    // A hint that names a removed command is worse than no hint.
    debug_assert!(
        crate::cli::command()
            .try_get_matches_from(args.clone())
            .is_ok(),
        "recovery hint does not parse: {command:?}"
    );
    shell_words::join(args)
}

fn with_client<F>(root: &ArgMatches, work: F) -> Result<(), Error>
where
    F: for<'a> FnOnce(
        &'a mut crate::connect::Client,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + 'a>>,
{
    let leaf = leaf_matches(root);
    let context = leaf.get_one::<String>("context").map(String::as_str);
    runtime()?.block_on(async {
        let mut client = server::connect(leaf, context).await?;
        work(&mut client).await
    })
}

pub(crate) type Handler = fn(&ArgMatches) -> Result<(), Error>;

/// Commands that print no `--json` result: a terminal session, shell code, or the
/// Cloud runner's own fixed JSON.
pub(crate) fn json_refused(path: &str) -> bool {
    matches!(path, "build" | "completion" | "exec")
}

/// Each command's handler: each group module declares its own subcommands.
fn handler_for(path: &str) -> Option<Handler> {
    let (group, rest) = path.split_once(' ').unwrap_or((path, ""));
    match (group, rest) {
        ("billing", rest) => account::billing_handler(rest),
        ("build", "") => Some(build::build),
        ("completion", "") => Some(completion),
        ("cloud", rest) => cloud::handler(rest),
        ("ctx", rest) => context::handler(rest),
        ("deploy", "") => Some(deploy::deploy),
        ("deployment", rest) => deploy::deployment_handler(rest),
        ("diff", "") => Some(review::diff),
        ("discard", "") => Some(review::discard),
        ("domain", rest) => domain::handler(rest),
        ("env", rest) => env::handler(rest),
        ("exec", "") => Some(operator::exec),
        ("explain", "") => Some(catalog::explain),
        ("get", "") => Some(config::get),
        ("link", "") => Some(link::link),
        ("github", rest) => github::handler(rest),
        ("login", "") => Some(login::login),
        ("logout", "") => Some(login::logout),
        ("logs", "") => Some(operator::logs),
        ("org", rest) => account::org_handler(rest),
        ("project", rest) => project::handler(rest),
        ("ps", "") => Some(service::processes),
        ("publish", "") => Some(review::publish),
        ("schema", "") => Some(catalog::schema),
        ("server", rest) => server::handler(rest),
        ("service", rest) => service::handler(rest),
        ("setup", rest) => setup::handler(rest),
        ("set", "") => Some(config::set),
        ("status", "") => Some(link::status),
        ("token", rest) => account::token_handler(rest),
        ("unset", "") => Some(config::unset),
        ("up", "") => Some(up::up),
        ("volume", rest) => volume::handler(rest),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn command() -> Command {
        fn isolate(command: Command) -> Command {
            command
                .mut_args(|arg| {
                    if arg.get_id() == "ployz-config" {
                        arg.env(None::<&str>)
                            .default_value("/tmp/ployz-handler-tests/config.yaml")
                    } else {
                        arg
                    }
                })
                .mut_subcommands(isolate)
        }

        isolate(crate::cli::command())
    }

    #[test]
    fn setup_recovery_preserves_config_and_context_and_parses_as_a_command() {
        let matches = command()
            .try_get_matches_from([
                "ployz",
                "--ployz-config",
                "/tmp/a config.yaml",
                "server",
                "add",
                "--standalone",
                "root@host",
                "--context",
                "staging",
            ])
            .unwrap();
        let recovery = recovery_command(
            leaf_matches(&matches),
            "staging",
            &["server", "set", "edge", "--accepts-ingress=true"],
        );
        let args = shell_words::split(&recovery).unwrap();
        let parsed = command().try_get_matches_from(args).unwrap();
        let leaf = leaf_matches(&parsed);
        assert_eq!(
            leaf.get_one::<String>("context").map(String::as_str),
            Some("staging")
        );
        assert_eq!(
            leaf.get_one::<String>("ployz-config").map(String::as_str),
            Some("/tmp/a config.yaml")
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "recovery hint does not parse")]
    fn setup_recovery_refuses_a_command_outside_the_tree() {
        let matches = command()
            .try_get_matches_from(["ployz", "server", "add", "--standalone", "root@host"])
            .unwrap();
        recovery_command(leaf_matches(&matches), "staging", &["no-such", "command"]);
    }

    #[test]
    fn logs_since_rejects_garbage_before_connecting() {
        let mut command = command();
        let cases = [
            (
                "since",
                "notatime",
                r#"invalid log time "notatime": expected a relative duration, RFC 3339 date, or Unix timestamp"#,
            ),
            (
                "until",
                "notatime",
                r#"invalid log time "notatime": expected a relative duration, RFC 3339 date, or Unix timestamp"#,
            ),
            (
                "tail",
                "abc",
                r#"invalid log tail "abc": expected a non-negative integer or all"#,
            ),
        ];
        for (flag, value, expected) in cases {
            let matches = command
                .clone()
                .try_get_matches_from([
                    "ployz",
                    "--connect",
                    "tcp://127.0.0.1:1",
                    "logs",
                    "api",
                    &format!("--{flag}"),
                    value,
                ])
                .unwrap();
            assert_eq!(
                dispatch(&matches, &mut command).unwrap_err().to_string(),
                expected,
                "{flag}",
            );
        }
    }

    #[test]
    fn server_set_rejects_an_invalid_server_name_before_connecting() {
        let mut command = command();
        let matches = command
            .clone()
            .try_get_matches_from(["ployz", "server", "set", "vultr1", "--name", "BAD NAME"])
            .unwrap();
        assert_eq!(
            dispatch(&matches, &mut command).unwrap_err().to_string(),
            ployz_core::MachineName::parse("BAD NAME")
                .unwrap_err()
                .to_string(),
        );
    }

    #[test]
    fn local_machine_init_requires_root_to_install() {
        if crate::provisioning::process_is_root() {
            return;
        }
        let mut command = command();
        let matches = command
            .clone()
            .try_get_matches_from([
                "ployz",
                "server",
                "add",
                "--standalone",
                "--yes",
                "--storage",
                "none",
                "--accepts-ingress=false",
                "--context",
                "local-init",
            ])
            .unwrap();
        assert_eq!(
            dispatch(&matches, &mut command).unwrap_err().to_string(),
            "run this command with sudo",
        );
    }

    #[test]
    fn local_machine_init_without_install_dials_the_unix_socket() {
        if std::path::Path::new(crate::connect::DEFAULT_LOCAL_SOCKET).exists() {
            return;
        }
        let mut command = command();
        let matches = command
            .clone()
            .try_get_matches_from([
                "ployz",
                "server",
                "add",
                "--standalone",
                "--no-install",
                "--yes",
                "--storage",
                "none",
                "--accepts-ingress=false",
                "--context",
                "local-init-no-install",
            ])
            .unwrap();
        assert_eq!(
            dispatch(&matches, &mut command).unwrap_err().to_string(),
            "all 1 connections from the explicit connection failed: connection attempt failed: transport error",
        );
    }

    #[test]
    fn server_add_accepts_only_supported_storage_choices() {
        for storage in ["none", "zfs"] {
            let parsed = command()
                .try_get_matches_from([
                    "ployz",
                    "server",
                    "add",
                    "root@example.test",
                    "--storage",
                    storage,
                ])
                .unwrap();
            assert_eq!(
                leaf_matches(&parsed)
                    .get_one::<ployz_core::StorageChoice>("storage")
                    .map(|choice| choice.as_str()),
                Some(storage),
            );
        }
        assert!(
            command()
                .try_get_matches_from([
                    "ployz",
                    "server",
                    "add",
                    "root@example.test",
                    "--storage",
                    "other",
                ])
                .is_err()
        );
    }

    #[test]
    fn server_add_takes_a_token_or_prints_a_command() {
        assert!(command().try_get_matches_from(["ployz", "cloud"]).is_err());
        let parsed = command()
            .try_get_matches_from(["ployz", "server", "add", "--token", "pmet_test"])
            .unwrap();
        let add = leaf_matches(&parsed);
        assert_eq!(
            add.get_one::<String>("token").map(String::as_str),
            Some("pmet_test")
        );
        assert_eq!(
            add.get_one::<ipnet::Ipv4Net>("network").copied(),
            Some("10.210.0.0/16".parse().unwrap())
        );
        for conflicting in [
            ["ployz", "server", "add", "--token", "pmet_x", "--command"].as_slice(),
            ["ployz", "server", "add", "--command", "root@host"].as_slice(),
            ["ployz", "server", "add", "--wait", "id", "root@host"].as_slice(),
            [
                "ployz",
                "server",
                "add",
                "--standalone",
                "--token",
                "pmet_x",
            ]
            .as_slice(),
            ["ployz", "server", "add", "--network", "not-a-cidr"].as_slice(),
        ] {
            assert!(
                command().try_get_matches_from(conflicting).is_err(),
                "{conflicting:?}"
            );
        }
        let flags = command()
            .try_get_matches_from([
                "ployz",
                "server",
                "add",
                "root@host",
                "--token",
                "pmet_x",
                "--name",
                "edge",
                "--network",
                "10.220.0.0/16",
                "--storage",
                "zfs",
                "--accepts-ingress=false",
                "--reset",
                "--yes",
                "--wg-mtu",
                "1400",
                "--cloud-url",
                "example.test",
            ])
            .unwrap();
        let add = leaf_matches(&flags);
        assert_eq!(add.get_one::<String>("destination").unwrap(), "root@host");
        assert_eq!(add.get_one::<String>("name").unwrap(), "edge");
        assert_eq!(
            add.get_one::<ployz_core::StorageChoice>("storage"),
            Some(&ployz_core::StorageChoice::Zfs)
        );
        assert_eq!(add.get_one::<bool>("accepts-ingress"), Some(&false));
        assert!(add.get_flag("reset"));
        assert_eq!(add.get_one::<u32>("wg-mtu").copied(), Some(1400));
        assert_eq!(
            add.get_one::<String>("cloud-url").map(String::as_str),
            Some("example.test")
        );
    }

    #[test]
    fn server_add_with_a_token_and_no_daemon_requires_sudo() {
        if crate::provisioning::process_is_root() {
            return;
        }
        let mut command = command();
        let matches = command
            .clone()
            .try_get_matches_from(["ployz", "server", "add", "--token", "pmet_test"])
            .unwrap();
        assert_eq!(
            dispatch(&matches, &mut command).unwrap_err().to_string(),
            "run this command with sudo",
        );
    }

    #[test]
    fn cloud_reset_fails_closed_with_the_command_to_confirm() {
        let mut command = command();
        let matches = command
            .clone()
            .try_get_matches_from(["ployz", "cloud", "reset"])
            .unwrap();
        let error = dispatch(&matches, &mut command).unwrap_err().report();
        assert_eq!(
            error
                .details
                .get("next")
                .and_then(serde_json::Value::as_str),
            Some("ployz cloud reset --yes")
        );
    }

    #[test]
    fn founding_commands_reject_the_removed_ingress_backend_option() {
        for arguments in [
            [
                "ployz",
                "server",
                "add",
                "--standalone",
                "--ingress-backend",
                "caddy",
            ]
            .as_slice(),
            [
                "ployz",
                "server",
                "add",
                "--token",
                "pmet_test",
                "--ingress-backend",
                "caddy",
            ]
            .as_slice(),
        ] {
            assert!(command().try_get_matches_from(arguments).is_err());
        }
    }

    #[test]
    fn retired_daemon_channels_are_rejected() {
        for channel in ["latest", "nightly"] {
            let error = command()
                .try_get_matches_from([
                    "ployz",
                    "server",
                    "add",
                    "root@example.com",
                    "--version",
                    channel,
                ])
                .unwrap_err();
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{channel}"
            );
        }
    }

    #[test]
    fn every_actionable_clap_command_has_an_explicit_handler() {
        let mut command = command();
        command.build();
        let mut paths = BTreeSet::new();
        collect_actionable_paths(&command, "", &mut paths);
        for path in paths {
            assert!(handler_for(&path).is_some(), "no handler for {path}");
        }
    }

    fn collect_actionable_paths(command: &Command, parent: &str, paths: &mut BTreeSet<String>) {
        let path = if parent.is_empty() {
            command.get_name().to_owned()
        } else {
            format!("{parent} {}", command.get_name())
        };
        let children = command
            .get_subcommands()
            .filter(|child| child.get_name() != "help")
            .collect::<Vec<_>>();
        if path != "ployz" && children.is_empty() {
            paths.insert(path.trim_start_matches("ployz ").to_owned());
        }
        for child in children {
            collect_actionable_paths(child, &path, paths);
        }
    }
}
