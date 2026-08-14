use std::{
    future::Future,
    io::{self, IsTerminal, Write},
    path::Path,
    pin::Pin,
};

use clap::{ArgMatches, Command};
use clap_complete::{Shell, generate};
use thiserror::Error as ThisError;

mod build;
mod context;
mod image;
mod operator;
mod service;
mod volume;
mod workflow;

#[derive(Debug, Eq, PartialEq, ThisError)]
pub enum Error {
    #[error("{0}")]
    Message(String),
    #[error("remote command exited with status {0}")]
    Exit(u8),
}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Self::Message(message.to_owned())
    }
}

pub fn run() -> Result<(), Error> {
    let mut command = crate::cli::command();
    let matches = command.clone().get_matches();
    dispatch(&matches, &mut command)
}

fn dispatch(matches: &ArgMatches, command: &mut Command) -> Result<(), Error> {
    let Some((name, child)) = matches.subcommand() else {
        command.print_help().map_err(|error| error.to_string())?;
        println!();
        return Ok(());
    };
    if name == "completion" {
        let shell = child
            .get_one::<Shell>("shell")
            .copied()
            .ok_or_else(|| "completion shell is required".to_owned())?;
        generate(shell, command, "ployz", &mut std::io::stdout());
        return Ok(());
    }
    let path = command_path(matches);
    let handler =
        handler_for(&path).ok_or_else(|| format!("no handler declared for ployz {path}"))?;
    handler(matches)
}

fn command_path(mut matches: &ArgMatches) -> String {
    let mut parts = Vec::new();
    while let Some((name, child)) = matches.subcommand() {
        parts.push(name);
        matches = child;
    }
    parts.join(" ")
}

fn leaf_matches(mut matches: &ArgMatches) -> &ArgMatches {
    while let Some((_, child)) = matches.subcommand() {
        matches = child;
    }
    matches
}

fn not_implemented(command: &str) -> Result<(), Error> {
    Err(format!("ployz {command} is not implemented yet").into())
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
        .ok_or_else(|| format!("{name} is required").into())
}

fn confirm() -> Result<bool, Error> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("confirmation requires a terminal; pass --yes to continue".into());
    }
    print!("Continue? [y/N] ");
    io::stdout().flush().map_err(|error| error.to_string())?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|error| error.to_string())?;
    Ok(matches!(input.trim(), "y" | "Y" | "yes" | "YES"))
}

fn runtime() -> Result<tokio::runtime::Runtime, Error> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?)
}

async fn connect_client(
    matches: &ArgMatches,
    context: Option<&str>,
) -> Result<crate::connect::Client, Error> {
    let config = matches
        .get_one::<String>("ployz-config")
        .map(Path::new)
        .map(crate::context::expand_home)
        .ok_or_else(|| "Ployz config path is required".to_owned())?;
    Ok(crate::connect::connect(
        &config,
        matches.get_one::<String>("connect").map(String::as_str),
        context,
    )
    .await
    .map_err(|error| error.to_string())?)
}

fn with_client<F>(root: &ArgMatches, work: F) -> Result<(), Error>
where
    F: for<'a> FnOnce(
        &'a mut crate::connect::Client,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + 'a>>,
{
    with_client_context(root, None, work)
}

fn with_client_context<F>(
    root: &ArgMatches,
    context_override: Option<&str>,
    work: F,
) -> Result<(), Error>
where
    F: for<'a> FnOnce(
        &'a mut crate::connect::Client,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + 'a>>,
{
    let leaf = leaf_matches(root);
    let context =
        context_override.or_else(|| leaf.get_one::<String>("context").map(String::as_str));
    runtime()?.block_on(async {
        let mut client = connect_client(leaf, context).await?;
        work(&mut client).await
    })
}

fn provision_then_continue(matches: &ArgMatches, command: &str) -> Result<(), Error> {
    if !matches.get_flag("no-install") {
        crate::provisioning::provision(matches)?;
    }
    not_implemented(command)
}

type Handler = fn(&ArgMatches) -> Result<(), Error>;

macro_rules! declare_handler {
    ($function:ident => $path:literal) => {
        fn $function(_matches: &ArgMatches) -> Result<(), Error> {
            not_implemented($path)
        }
    };
    ($function:ident => $path:literal, $matches:ident $body:block) => {
        fn $function($matches: &ArgMatches) -> Result<(), Error> $body
    };
}

macro_rules! stub_handlers {
    ($($function:ident $(($matches:ident) $body:block)? => $path:literal);+ $(;)?) => {
        $(declare_handler!($function => $path $(, $matches $body)?);)+

        fn handler_for(path: &str) -> Option<Handler> {
            match path {
                $($path => Some($function),)+
                _ => None,
            }
        }
    };
}

stub_handlers! {
    build(root) { build::run(root) } => "build";
    caddy_config => "caddy config";
    caddy_deploy => "caddy deploy";
    caddy_logs => "caddy logs";
    context(root) { context::select(root, None) } => "ctx";
    context_connection(root) { context::select_connection(root) } => "ctx connection";
    context_list(root) { context::list(root) } => "ctx ls";
    context_show(root) { context::show(root) } => "ctx show";
    context_use(root) {
        context::select(
            root,
            leaf_matches(root)
                .get_one::<String>("context-name")
                .map(String::as_str),
        )
    } => "ctx use";
    deploy(root) { workflow::deploy(root) } => "deploy";
    dns_release => "dns release";
    dns_reserve => "dns reserve";
    dns_show => "dns show";
    exec(root) { operator::exec(root) } => "exec";
    image_list(root) { image::list(root) } => "image ls";
    image_push(root) { image::push(root) } => "image push";
    images(root) { image::list(root) } => "images";
    inspect(root) { service::inspect(root) } => "inspect";
    logs(root) {
        operator::service_logs(root)
    } => "logs";
    list(root) { service::list(root) } => "ls";
    machine_add(root) { provision_then_continue(leaf_matches(root), "machine add") } => "machine add";
    machine_init(root) {
        let matches = leaf_matches(root);
        if matches.get_one::<String>("destination").is_none() {
            Err("local machine initialisation is not implemented; specify a remote machine".into())
        } else {
            provision_then_continue(matches, "machine init")
        }
    } => "machine init";
    machine_logs(root) { operator::machine_logs(root) } => "machine logs";
    machine_list => "machine ls";
    machine_rename => "machine rename";
    machine_remove => "machine rm";
    machine_rtt => "machine rtt";
    machine_update => "machine update";
    proxy(root) { operator::proxy(root) } => "proxy";
    process_list(root) { service::processes(root) } => "ps";
    remove(root) { service::change(root, ployz_core::ContainerAction::Remove) } => "rm";
    run_service(root) { workflow::run(root) } => "run";
    scale(root) { workflow::scale(root) } => "scale";
    service_exec(root) { operator::exec(root) } => "service exec";
    service_inspect(root) { service::inspect(root) } => "service inspect";
    service_logs(root) { operator::service_logs(root) } => "service logs";
    service_list(root) { service::list(root) } => "service ls";
    service_remove(root) { service::change(root, ployz_core::ContainerAction::Remove) } => "service rm";
    service_run(root) { workflow::run(root) } => "service run";
    service_scale(root) { workflow::scale(root) } => "service scale";
    service_start(root) { service::change(root, ployz_core::ContainerAction::Start) } => "service start";
    service_stop(root) { service::change(root, ployz_core::ContainerAction::Stop) } => "service stop";
    start(root) { service::change(root, ployz_core::ContainerAction::Start) } => "start";
    stop(root) { service::change(root, ployz_core::ContainerAction::Stop) } => "stop";
    version(matches) {
        let version = env!("CARGO_PKG_VERSION");
        if let Some(template) = matches.get_one::<String>("output") {
            println!("{}", template.replace("{{.Version}}", version));
        } else {
            println!("{version}");
        }
        Ok(())
    } => "version";
    volume_create(root) { volume::create(root) } => "volume create";
    volume_inspect(root) { volume::inspect(root) } => "volume inspect";
    volume_list(root) { volume::list(root) } => "volume ls";
    volume_remove(root) { volume::remove(root) } => "volume rm";
    wireguard_show => "wg show";
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn local_machine_initialisation_remains_explicitly_unimplemented() {
        let mut command = crate::cli::command();
        let matches = command
            .clone()
            .try_get_matches_from(["ployz", "machine", "init"])
            .unwrap();
        assert_eq!(
            dispatch(&matches, &mut command),
            Err("local machine initialisation is not implemented; specify a remote machine".into())
        );
    }

    #[test]
    fn ordinary_commands_report_their_full_path() {
        let mut command = crate::cli::command();
        let matches = command
            .clone()
            .try_get_matches_from(["ployz", "caddy", "config"])
            .unwrap();
        assert_eq!(
            dispatch(&matches, &mut command),
            Err("ployz caddy config is not implemented yet".into())
        );
    }

    #[test]
    fn malformed_volume_assignments_fail_before_connecting() {
        let mut command = crate::cli::command();
        let matches = command
            .clone()
            .try_get_matches_from([
                "ployz",
                "volume",
                "create",
                "data",
                "--opt",
                "missing-delimiter",
                "--connect",
                "tcp://127.0.0.1:1",
            ])
            .unwrap();
        assert_eq!(
            dispatch(&matches, &mut command),
            Err("expected KEY=VALUE, got \"missing-delimiter\"".into())
        );
    }

    #[test]
    fn scale_zero_fails_before_connecting() {
        let mut command = crate::cli::command();
        let matches = command
            .clone()
            .try_get_matches_from([
                "ployz",
                "--connect",
                "tcp://127.0.0.1:1",
                "scale",
                "api",
                "0",
            ])
            .unwrap();
        assert_eq!(
            dispatch(&matches, &mut command),
            Err("replicas must be greater than zero".into())
        );
    }

    #[test]
    fn nightly_daemon_channel_is_rejected() {
        let error = crate::cli::command()
            .try_get_matches_from([
                "ployz",
                "machine",
                "add",
                "root@example.com",
                "--version",
                "nightly",
            ])
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("nightly is not a supported release channel")
        );
    }

    #[test]
    fn every_actionable_clap_command_has_an_explicit_handler() {
        let mut command = crate::cli::command();
        command.build();
        let mut paths = BTreeSet::new();
        collect_actionable_paths(&command, "", &mut paths);
        paths.remove("completion");
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
        if path != "ployz" && (children.is_empty() || path == "ployz ctx") {
            paths.insert(path.trim_start_matches("ployz ").to_owned());
        }
        for child in children {
            collect_actionable_paths(child, &path, paths);
        }
    }
}
