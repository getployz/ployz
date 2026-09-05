use std::{
    future::Future,
    io::{self, IsTerminal, Write},
    path::Path,
    pin::Pin,
};

use clap::{ArgMatches, Command};
use clap_complete::{Shell, generate};
use tokio_util::sync::CancellationToken;

use crate::failure::Failure;

mod build;
mod cloud;
mod context;
mod data_loss;
mod deploy;
mod dns;
mod image;
mod ingress;
mod machine;
mod operator;
mod project;
mod service;
mod volume;

#[doc(hidden)]
pub use cloud::{EnrollInstaller, enroll_with_installer as cloud_enroll_with_installer};

pub type Error = Failure;

pub fn run() -> Result<(), Error> {
    let mut command = crate::cli::command();
    let matches = command.clone().get_matches();
    dispatch(&matches, &mut command)
}

fn dispatch(matches: &ArgMatches, command: &mut Command) -> Result<(), Error> {
    if matches.get_flag("version") {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let Some((name, child)) = matches.subcommand() else {
        command.print_help()?;
        println!();
        return Ok(());
    };
    if name == "completion" {
        let shell = child
            .get_one::<Shell>("shell")
            .copied()
            .ok_or_else(|| Error::usage("completion shell is required"))?;
        generate(shell, command, "ployz", &mut std::io::stdout());
        return Ok(());
    }
    let path = command_path(matches);
    let handler = handler_for(&path)
        .ok_or_else(|| Error::usage(format!("no handler declared for ployz {path}")))?;
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

fn version_text(output: Option<&str>, version: &str) -> Result<String, Error> {
    let Some(template) = output else {
        return Ok(version.to_owned());
    };
    let rendered = template.replace("{{.Version}}", version);
    if rendered.contains("{{") {
        return Err(Error::usage(format!(
            "unusable output template: {template}"
        )));
    }
    Ok(rendered)
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

fn confirm() -> Result<bool, Error> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(Error::usage(
            "confirmation requires a terminal; pass --yes to continue",
        ));
    }
    print!("Continue? [y/N] ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim(), "y" | "Y" | "yes" | "YES"))
}

fn cancellation_on_ctrl_c() -> CancellationToken {
    let cancellation = CancellationToken::new();
    let signal = cancellation.clone();
    tokio::spawn(async move {
        tokio::select! {
            () = signal.cancelled() => {}
            result = tokio::signal::ctrl_c() => if result.is_ok() {
                signal.cancel();
            }
        }
    });
    cancellation
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

async fn connect_client(
    matches: &ArgMatches,
    context: Option<&str>,
) -> Result<crate::connect::Client, Error> {
    Ok(crate::connect::connect(
        &config_path(matches)?,
        matches.get_one::<String>("connect").map(String::as_str),
        context,
    )
    .await?)
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

type Handler = fn(&ArgMatches) -> Result<(), Error>;

macro_rules! declare_handler {
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
    ingress_config(root) { ingress::config(root) } => "ingress config";
    ingress_deploy(root) { ingress::deploy(root) } => "ingress deploy";
    ingress_logs(root) { operator::ingress_logs(root) } => "ingress logs";
    context(root) { context::select(root, None) } => "ctx";
    context_connection(root) {
        context::connection(
            root,
            leaf_matches(root)
                .get_one::<String>("connection")
                .map(String::as_str),
        )
    } => "ctx connection";
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
    deploy(root) { deploy::deploy(root) } => "deploy";
    dns_release(root) { dns::release(root) } => "dns release";
    dns_reserve(root) { dns::reserve(root) } => "dns reserve";
    dns_show(root) { dns::show(root) } => "dns show";
    exec(root) { operator::exec(root) } => "exec";
    image_list(root) { image::list(root) } => "image ls";
    image_push(root) { image::push(root) } => "image push";
    images(root) { image::list(root) } => "images";
    cloud_enroll(root) { cloud::enroll(root) } => "cloud enroll";
    inspect(root) { service::inspect(root) } => "inspect";
    logs(root) {
        operator::service_logs(root)
    } => "logs";
    list(root) { service::list(root) } => "ls";
    machine_add(root) { machine::add(root) } => "machine add";
    machine_init(root) { machine::init(root) } => "machine init";
    machine_inspect(root) { machine::inspect(root) } => "machine inspect";
    machine_logs(root) { operator::machine_logs(root) } => "machine logs";
    machine_list(root) { machine::list(root) } => "machine ls";
    machine_rename(root) { machine::rename(root) } => "machine rename";
    machine_remove(root) { machine::remove(root) } => "machine rm";
    machine_rtt(root) { machine::rtt(root) } => "machine rtt";
    machine_update(root) { machine::update(root) } => "machine update";
    proxy(root) { operator::proxy(root) } => "proxy";
    project_list(root) { project::list(root) } => "project ls";
    project_remove(root) { project::remove(root) } => "project rm";
    process_list(root) { service::processes(root) } => "ps";
    remove(root) { service::change(root, ployz_core::ContainerAction::Remove) } => "rm";
    run_service(root) { deploy::run(root) } => "run";
    scale(root) { deploy::scale(root) } => "scale";
    service_exec(root) { operator::exec(root) } => "service exec";
    service_inspect(root) { service::inspect(root) } => "service inspect";
    service_logs(root) { operator::service_logs(root) } => "service logs";
    service_list(root) { service::list(root) } => "service ls";
    service_remove(root) { service::change(root, ployz_core::ContainerAction::Remove) } => "service rm";
    service_run(root) { deploy::run(root) } => "service run";
    service_scale(root) { deploy::scale(root) } => "service scale";
    service_start(root) { service::change(root, ployz_core::ContainerAction::Start) } => "service start";
    service_stop(root) { service::change(root, ployz_core::ContainerAction::Stop) } => "service stop";
    start(root) { service::change(root, ployz_core::ContainerAction::Start) } => "start";
    stop(root) { service::change(root, ployz_core::ContainerAction::Stop) } => "stop";
    version(root) {
        println!(
            "{}",
            version_text(
                leaf_matches(root)
                    .get_one::<String>("output")
                    .map(String::as_str),
                env!("CARGO_PKG_VERSION"),
            )?
        );
        Ok(())
    } => "version";
    volume_create(root) { volume::create(root) } => "volume create";
    volume_inspect(root) { volume::inspect(root) } => "volume inspect";
    volume_list(root) { volume::list(root) } => "volume ls";
    volume_remove(root) { volume::remove(root) } => "volume rm";
    wireguard_show(root) { machine::wireguard_show(root) } => "wg show";
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
    fn version_output_template_must_be_usable() {
        let mut command = command();
        let matches = command
            .clone()
            .try_get_matches_from(["ployz", "version", "-o", "{{.Nope}}"])
            .unwrap();
        assert_eq!(
            dispatch(&matches, &mut command).unwrap_err().to_string(),
            "unusable output template: {{.Nope}}",
        );
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
    fn machine_rename_rejects_an_invalid_machine_name_before_connecting() {
        let mut command = command();
        let matches = command
            .clone()
            .try_get_matches_from(["ployz", "machine", "rename", "vultr1", "BAD NAME"])
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
                "machine",
                "init",
                "--yes",
                "--storage",
                "none",
                "--no-dns",
                "--no-ingress",
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
                "machine",
                "init",
                "--no-install",
                "--yes",
                "--storage",
                "none",
                "--no-dns",
                "--no-ingress",
                "--context",
                "local-init-no-install",
            ])
            .unwrap();
        assert_eq!(
            dispatch(&matches, &mut command).unwrap_err().to_string(),
            "all 1 connections from Direct failed",
        );
    }

    #[test]
    fn machine_enrollment_accepts_only_supported_storage_choices() {
        for action in ["add", "init"] {
            for storage in ["none", "zfs"] {
                let parsed = command()
                    .try_get_matches_from([
                        "ployz",
                        "machine",
                        action,
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
                        "machine",
                        action,
                        "root@example.test",
                        "--storage",
                        "other",
                    ])
                    .is_err()
            );
        }
    }

    #[test]
    fn cloud_enroll_takes_a_positional_token() {
        assert!(command().try_get_matches_from(["ployz", "cloud"]).is_err());
        assert!(
            command()
                .try_get_matches_from(["ployz", "init", "--cloud", "pmet_test"])
                .is_err()
        );
        let parsed = command()
            .try_get_matches_from(["ployz", "cloud", "enroll", "pmet_test"])
            .unwrap();
        let cloud = parsed.subcommand_matches("cloud").unwrap();
        assert_eq!(
            cloud.get_one::<String>("cloud-url").map(String::as_str),
            Some("ployz.dev")
        );
        let enroll = cloud.subcommand_matches("enroll").unwrap();
        assert_eq!(
            enroll.get_one::<String>("token").map(String::as_str),
            Some("pmet_test")
        );
        assert_eq!(
            enroll.get_one::<ipnet::Ipv4Net>("network").copied(),
            Some("10.210.0.0/16".parse().unwrap())
        );
        assert!(
            command()
                .try_get_matches_from(["ployz", "cloud", "enroll", "pmet_x", "root@host"])
                .is_err()
        );
        assert!(
            command()
                .try_get_matches_from([
                    "ployz",
                    "cloud",
                    "enroll",
                    "pmet_x",
                    "--ssh-key",
                    "/tmp/key"
                ])
                .is_err()
        );
        let flags = command()
            .try_get_matches_from([
                "ployz",
                "cloud",
                "enroll",
                "pmet_x",
                "--name",
                "edge",
                "--network",
                "10.220.0.0/16",
                "--storage",
                "zfs",
                "--no-ingress",
                "--no-dns",
                "--reset",
                "--yes",
                "--wg-mtu",
                "1400",
                "--cloud-url",
                "example.test",
            ])
            .unwrap();
        let enroll = flags
            .subcommand_matches("cloud")
            .unwrap()
            .subcommand_matches("enroll")
            .unwrap();
        assert_eq!(enroll.get_one::<String>("name").unwrap(), "edge");
        assert_eq!(
            enroll.get_one::<ipnet::Ipv4Net>("network").copied(),
            Some("10.220.0.0/16".parse().unwrap())
        );
        assert_eq!(
            enroll.get_one::<ployz_core::StorageChoice>("storage"),
            Some(&ployz_core::StorageChoice::Zfs)
        );
        assert!(enroll.get_flag("no-ingress"));
        assert!(enroll.get_flag("reset"));
        assert!(enroll.get_flag("no-dns"));
        assert!(enroll.get_flag("yes"));
        assert_eq!(enroll.get_one::<u32>("wg-mtu").copied(), Some(1400));
        assert_eq!(
            enroll.get_one::<String>("cloud-url").map(String::as_str),
            Some("example.test")
        );
    }

    #[test]
    fn cloud_enroll_rejects_an_invalid_cluster_network() {
        assert!(
            command()
                .try_get_matches_from([
                    "ployz",
                    "cloud",
                    "enroll",
                    "pmet_test",
                    "--reset",
                    "--yes",
                    "--network",
                    "not-a-cidr",
                ])
                .is_err()
        );
    }

    #[test]
    fn cloud_enroll_without_a_daemon_requires_sudo() {
        if crate::provisioning::process_is_root() {
            return;
        }
        let mut command = command();
        let matches = command
            .clone()
            .try_get_matches_from(["ployz", "cloud", "enroll", "pmet_test"])
            .unwrap();
        assert_eq!(
            dispatch(&matches, &mut command).unwrap_err().to_string(),
            "run this command with sudo",
        );
    }

    #[test]
    fn founding_commands_reject_the_removed_ingress_backend_option() {
        for arguments in [
            ["ployz", "machine", "init", "--ingress-backend", "caddy"].as_slice(),
            [
                "ployz",
                "cloud",
                "enroll",
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
    fn dns_show_uses_the_real_handler() {
        let mut command = command();
        let matches = command
            .clone()
            .try_get_matches_from(["ployz", "dns", "show"])
            .unwrap();
        assert_eq!(
            dispatch(&matches, &mut command).unwrap_err().to_string(),
            crate::context::ContextError::NoConfig.to_string(),
        );
    }

    #[test]
    fn malformed_volume_assignments_fail_before_connecting() {
        let mut command = command();
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
            dispatch(&matches, &mut command).unwrap_err().to_string(),
            r#"expected KEY=VALUE, got "missing-delimiter""#,
        );
    }

    #[test]
    fn scale_zero_fails_before_connecting() {
        let mut command = command();
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
            dispatch(&matches, &mut command).unwrap_err().to_string(),
            "replicas must be greater than zero",
        );
    }

    #[test]
    fn reserved_and_invalid_project_names_fail_before_connecting() {
        let mut command = command();
        let reserved = command
            .clone()
            .try_get_matches_from(["ployz", "run", "--project-name", "ployz-system", "alpine"])
            .unwrap();
        assert_eq!(
            dispatch(&reserved, &mut command).unwrap_err().to_string(),
            "Project 'ployz-system' is reserved for Ployz infrastructure",
        );
        let deploy = command
            .clone()
            .try_get_matches_from(["ployz", "deploy", "--project-name", "ployz-system"])
            .unwrap();
        assert_eq!(
            dispatch(&deploy, &mut command).unwrap_err().to_string(),
            "Project 'ployz-system' is reserved for Ployz infrastructure",
        );
        let scale = command
            .clone()
            .try_get_matches_from([
                "ployz",
                "scale",
                "--project-name",
                "ployz-system",
                "web",
                "2",
            ])
            .unwrap();
        assert_eq!(
            dispatch(&scale, &mut command).unwrap_err().to_string(),
            "Project 'ployz-system' is reserved for Ployz infrastructure",
        );
        let invalid = command
            .clone()
            .try_get_matches_from(["ployz", "deploy", "--project-name", "My_App"])
            .unwrap();
        assert_eq!(
            dispatch(&invalid, &mut command).unwrap_err().to_string(),
            "invalid Project Name \"My_App\": a 1-63 character lowercase DNS label; underscores and uppercase are not accepted",
        );
        let remove = command
            .clone()
            .try_get_matches_from(["ployz", "rm", "--project-name", "ployz-system", "web"])
            .unwrap();
        assert_eq!(
            dispatch(&remove, &mut command).unwrap_err().to_string(),
            "Project 'ployz-system' is reserved for Ployz infrastructure",
        );
        let service_remove = command
            .clone()
            .try_get_matches_from([
                "ployz",
                "service",
                "rm",
                "--project-name",
                "ployz-system",
                "web",
            ])
            .unwrap();
        assert_eq!(
            dispatch(&service_remove, &mut command)
                .unwrap_err()
                .to_string(),
            "Project 'ployz-system' is reserved for Ployz infrastructure",
        );
        let implicit_default = command
            .clone()
            .try_get_matches_from(["ployz", "run", "alpine"])
            .unwrap();
        assert_eq!(
            dispatch(&implicit_default, &mut command)
                .unwrap_err()
                .to_string(),
            crate::context::ContextError::NoConfig.to_string(),
        );
        let project_remove = command
            .clone()
            .try_get_matches_from(["ployz", "project", "rm", "ployz-system"])
            .unwrap();
        assert_eq!(
            dispatch(&project_remove, &mut command)
                .unwrap_err()
                .to_string(),
            "Project 'ployz-system' is reserved for Ployz infrastructure",
        );
        let invalid_project_remove = command
            .clone()
            .try_get_matches_from(["ployz", "project", "rm", "My_App"])
            .unwrap();
        assert_eq!(
            dispatch(&invalid_project_remove, &mut command)
                .unwrap_err()
                .to_string(),
            "invalid Project Name \"My_App\": a 1-63 character lowercase DNS label; underscores and uppercase are not accepted",
        );
    }

    #[test]
    fn nightly_daemon_channel_is_rejected() {
        let error = command()
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
        let mut command = command();
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
