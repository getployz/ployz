use clap::{Arg, ArgAction, Command, ValueHint};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("nightly is not a supported release channel")]
struct UnsupportedDaemonChannel;

pub mod env {
    pub const AUTO_CONFIRM: &str = "PLOYZ_AUTO_CONFIRM";
    pub const COMPOSE_FILE: &str = "COMPOSE_FILE";
    pub const COMPOSE_PROJECT_NAME: &str = "COMPOSE_PROJECT_NAME";
    pub const CONFIG: &str = "PLOYZ_CONFIG";
    pub const CONNECT: &str = "PLOYZ_CONNECT";
    pub const CONTEXT: &str = "PLOYZ_CONTEXT";
    pub const DAEMON_VERSION: &str = "PLOYZ_DAEMON_VERSION";
    pub const HEALTH_MONITOR_PERIOD: &str = "PLOYZ_HEALTH_MONITOR_PERIOD";
    pub const SSH_CONTROL_PERSIST: &str = "PLOYZ_SSH_CONTROL_PERSIST";
}

#[must_use]
pub fn command() -> Command {
    base("ployz", "Manage Ployz machines, services, and volumes")
        .arg(switch("version", Some('V')).help("Print version"))
        .subcommand(build())
        .subcommand(changes())
        .subcommand(ingress())
        .subcommand(ctx())
        .subcommand(deploy())
        .subcommand(dns())
        .subcommand(exec("exec"))
        .subcommand(image())
        .subcommand(images())
        .subcommand(cloud())
        .subcommand(inspect("inspect"))
        .subcommand(logs("logs", true))
        .subcommand(service_ls("ls"))
        .subcommand(machine())
        .subcommand(project())
        .subcommand(proxy())
        .subcommand(ps())
        .subcommand(service_rm("rm"))
        .subcommand(run("run"))
        .subcommand(scale("scale"))
        .subcommand(service())
        .subcommand(start("start"))
        .subcommand(stop("stop"))
        .subcommand(version())
        .subcommand(volume())
        .subcommand(wg())
        .subcommand(completion())
}

fn base(name: &'static str, about: &'static str) -> Command {
    Command::new(name).about(about).args(connection_args(true))
}

fn connection_args(include_context: bool) -> Vec<Arg> {
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

fn value(name: &'static str, short: Option<char>) -> Arg {
    let arg = Arg::new(name).long(name).action(ArgAction::Set);
    match short {
        Some(short) => arg.short(short),
        None => arg,
    }
}

fn json_output() -> Arg {
    value("output", Some('o')).value_parser(["json"])
}

fn many(name: &'static str, short: Option<char>) -> Arg {
    value(name, short)
        .action(ArgAction::Append)
        .value_delimiter(',')
}

fn repeated(name: &'static str) -> Arg {
    value(name, None).action(ArgAction::Append)
}

fn switch(name: &'static str, short: Option<char>) -> Arg {
    let arg = Arg::new(name).long(name).action(ArgAction::SetTrue);
    match short {
        Some(short) => arg.short(short),
        None => arg,
    }
}

fn positional(name: &'static str, required: bool) -> Arg {
    Arg::new(name).required(required).action(ArgAction::Set)
}

fn daemon_version(value: &str) -> Result<String, UnsupportedDaemonChannel> {
    if value == "nightly" {
        Err(UnsupportedDaemonChannel)
    } else {
        Ok(value.to_owned())
    }
}

fn trailing(name: &'static str) -> Arg {
    Arg::new(name)
        .num_args(0..)
        .action(ArgAction::Append)
        .allow_hyphen_values(true)
        .trailing_var_arg(true)
}

fn project_name(short: Option<char>) -> Arg {
    value("project-name", short).env(env::COMPOSE_PROJECT_NAME)
}

fn build_remote() -> Arg {
    Arg::new("remote")
        .long("remote")
        .num_args(0..=1)
        .require_equals(true)
        .default_missing_value(crate::build_location::AUTOMATIC)
        .value_name("MACHINE")
        .conflicts_with("local")
}

fn build() -> Command {
    base("build", "Build service images")
        .arg(build_remote())
        .arg(switch("local", None))
        .arg(repeated("build-arg"))
        .arg(switch("check", None))
        .arg(switch("deps", None))
        .arg(many("file", Some('f')).default_value("compose.yaml"))
        .arg(many("machine", Some('m')).requires("push"))
        .arg(switch("no-cache", None))
        .arg(many("profile", Some('p')))
        .arg(switch("pull", None))
        .arg(switch("push", None).conflicts_with("push-registry"))
        .arg(switch("push-registry", None))
        .arg(Arg::new("service").num_args(0..).action(ArgAction::Append))
}

fn deploy() -> Command {
    base("deploy", "Deploy services from a Compose file")
        .arg(build_remote().conflicts_with("no-build"))
        .arg(switch("local", None))
        .arg(repeated("build-arg"))
        .arg(switch("build-pull", None))
        .arg(many("file", Some('f')).default_value("compose.yaml"))
        .arg(switch("no-build", None))
        .arg(switch("no-cache", None))
        .arg(many("profile", None))
        .arg(project_name(Some('p')))
        .arg(switch("recreate", None))
        .arg(switch("skip-health", None))
        .arg(switch("yes", Some('y')).env(env::AUTO_CONFIRM))
        .arg(Arg::new("service").num_args(0..).action(ArgAction::Append))
}

fn changes() -> Command {
    base(
        "changes",
        "Review captured Compose settings against live observations (command only)",
    )
    .arg(many("file", Some('f')).default_value("compose.yaml"))
    .arg(many("profile", None))
    .arg(project_name(Some('p')))
    .arg(json_output())
    .arg(Arg::new("service").num_args(0..).action(ArgAction::Append))
}

fn ingress() -> Command {
    base("ingress", "Manage the Ingress Proxy")
        .arg_required_else_help(true)
        .subcommand(
            base("config", "Print Ingress Proxy configuration").arg(value("machine", Some('m'))),
        )
        .subcommand(
            base("deploy", "Deploy the Ingress Proxy")
                .arg(value("caddyfile", None).value_hint(ValueHint::FilePath))
                .arg(value("image", None))
                .arg(many("machine", Some('m')))
                .arg(switch("recreate", None))
                .arg(switch("skip-health", None)),
        )
        .subcommand(log_flags(base("logs", "Show Ingress Proxy logs"), false).visible_alias("log"))
}

fn ctx() -> Command {
    base("ctx", "Manage local contexts")
        .visible_alias("context")
        .subcommand(
            base("connection", "Show or select the default connection")
                .visible_alias("conn")
                .arg(positional("connection", false)),
        )
        .subcommand(base("ls", "List contexts").visible_alias("list"))
        .subcommand(base("show", "Show a context"))
        .subcommand(base("use", "Select a context").arg(positional("context-name", false)))
        .subcommand(
            base("rm", "Remove a local context")
                .visible_aliases(["remove", "delete"])
                .arg(positional("context-name", true)),
        )
}

fn dns() -> Command {
    base("dns", "Manage the cluster domain")
        .arg_required_else_help(true)
        .subcommand(base("release", "Release the cluster domain"))
        .subcommand(
            base("reserve", "Reserve a cluster domain")
                .arg(value("endpoint", None).default_value(crate::dns::HOSTED_DNS_ENDPOINT)),
        )
        .subcommand(base("show", "Show the cluster domain"))
}

fn exec(name: &'static str) -> Command {
    base(name, "Execute a command in a service container")
        .arg(value("container", None))
        .arg(switch("detach", Some('d')))
        .arg(switch("no-tty", Some('T')))
        .arg(positional("service", true))
        .arg(trailing("command"))
}

fn image() -> Command {
    base("image", "Manage images")
        .arg_required_else_help(true)
        .subcommand(
            base("ls", "List images")
                .visible_alias("list")
                .arg(many("machine", Some('m')))
                .arg(json_output())
                .arg(positional("image", false)),
        )
        .subcommand(
            base("push", "Push an image")
                .arg(many("machine", Some('m')))
                .arg(value("platform", None))
                .arg(positional("image", true)),
        )
}

fn images() -> Command {
    base("images", "List images")
        .arg(many("machine", Some('m')))
        .arg(value("output", Some('o')).value_parser(["json"]))
        .arg(positional("image", false))
}

fn cloud() -> Command {
    Command::new("cloud")
        .about("Manage Cloud")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .arg(
            value("cloud-url", None)
                .default_value("ployz.dev")
                .global(true),
        )
        .subcommand(cloud_enroll())
}

fn cloud_enroll() -> Command {
    base("enroll", "Found or join a Cluster through Cloud")
        .arg(positional("token", true))
        .arg(value("name", Some('n')))
        .arg(
            value("network", None)
                .default_value("10.210.0.0/16")
                .value_parser(clap::value_parser!(ipnet::Ipv4Net)),
        )
        .arg(
            value("storage", None)
                .default_value("none")
                .value_parser(clap::value_parser!(ployz_core::StorageChoice)),
        )
        .arg(switch("no-ingress", None))
        .arg(
            value("ingress-image", None)
                .help("Caddy image to deploy when founding a Cluster")
                .conflicts_with("no-ingress"),
        )
        .arg(switch("no-dns", None))
        .arg(switch("reset", None).help("Reset an initialized Machine before enrollment"))
        .arg(value("wg-mtu", None).value_parser(clap::value_parser!(u32).range(1..)))
        .arg(switch("yes", Some('y')).env(env::AUTO_CONFIRM))
}

fn inspect(name: &'static str) -> Command {
    base(name, "Inspect a service").arg(positional("service", true))
}

fn log_flags(command: Command, include_file: bool) -> Command {
    let command = if include_file {
        command.arg(many("file", None).default_value("compose.yaml"))
    } else {
        command
    };
    command
        .arg(switch("follow", Some('f')))
        .arg(many("machine", Some('m')))
        .arg(value("since", None))
        .arg(value("tail", Some('n')).default_value("100"))
        .arg(value("until", None))
        .arg(switch("utc", None))
}

fn logs(name: &'static str, include_file: bool) -> Command {
    log_flags(base(name, "Show logs"), include_file).arg(
        Arg::new("service-or-container")
            .num_args(0..)
            .action(ArgAction::Append),
    )
}

fn machine() -> Command {
    base("machine", "Manage machines")
        .visible_alias("m")
        .arg_required_else_help(true)
        .subcommand(machine_add())
        .subcommand(machine_init())
        .subcommand(base("build-cache-clear", "Clear this execution host user's Ployz build cache")
            .long_about("Clear this execution host user's Ployz build cache. Run on the build host as the user running its Builds (including the daemon). Refuses active or quarantined builder ownership; preserves completed images and unrelated Docker data. No daemon is required.\n\nHost configuration: ~/.ployz/build.yaml. Optional cpu_cores and memory_bytes limit BuildKit and Railpack preparation, independently of Service runtime limits. Both are disabled when omitted. Optional cache_bytes and min_free_bytes are retention/GC targets, not hard peak disk quotas. Unconfigured GC uses pinned BuildKit defaults."))
        .subcommand(base("inspect", "Inspect a machine").arg(positional("machine", true)))
        .subcommand(
            log_flags(
                base("logs", "Show machine logs").visible_alias("log"),
                false,
            )
            .arg(Arg::new("service").num_args(0..).action(ArgAction::Append)),
        )
        .subcommand(
            base("ls", "List machines")
                .visible_alias("list")
                .arg(value("output", Some('o')).value_parser(["json"])),
        )
        .subcommand(
            base("rename", "Rename a machine")
                .arg(positional("old-name", true))
                .arg(positional("new-name", true)),
        )
        .subcommand(
            base("rm", "Remove a machine")
                .visible_aliases(["remove", "delete"])
                .arg(switch("no-reset", None).help(
                    "Remove the Machine from the Cluster without resetting it; use when the Machine is unreachable",
                ))
                .arg(switch("yes", Some('y')).env(env::AUTO_CONFIRM))
                .arg(positional("machine", true))
                .arg(
                    volume_acceptance().conflicts_with("no-reset").help("Accept loss of Cluster access: repeat once per exact volume name; reset does not erase volume data on the host; --yes cannot bypass this"),
                ),
        )
        .subcommand(base("rtt", "Show round-trip times"))
        .subcommand(machine_upgrade())
        .subcommand(
            base("update", "Update machine configuration")
                .arg(value("name", None))
                .arg(value("public-ip", None))
                .arg(many("wg-endpoint", None))
                .arg(positional("machine", true)),
        )
}

fn machine_upgrade() -> Command {
    base("upgrade", "Upgrade explicitly selected machines")
        .arg_required_else_help(true)
        .subcommand_negates_reqs(true)
        .args_conflicts_with_subcommands(true)
        .subcommand_precedence_over_arg(true)
        .arg(
            positional("version", true)
                .value_name("VERSION")
                .value_parser(clap::value_parser!(ployz_core::MachineRelease)),
        )
        .arg(
            many("machine", Some('m'))
                .required(true)
                .help("Machine name or ID; repeat for an explicit sequence"),
        )
        .subcommand(
            base("inspect", "Inspect one Machine upgrade attempt")
                .arg(positional("machine", true))
                .arg(
                    value("attempt", None)
                        .value_parser(clap::value_parser!(ployz_core::MachineUpgradeAttemptId)),
                )
                .arg(json_output()),
        )
}

fn provisioning_flags(command: Command) -> Command {
    command
        .arg(value("name", Some('n')))
        .arg(switch("no-ingress", None))
        .arg(switch("no-install", None))
        .arg(
            value("storage", None)
                .value_parser(clap::value_parser!(ployz_core::StorageChoice))
                .help("Prepare ZFS storage or keep this Machine currently stateless"),
        )
        .arg(value("public-ip", None).default_value("auto"))
        .arg(
            value("ssh-key", Some('i'))
                .default_value("~/.ssh/id_ed25519")
                .value_hint(ValueHint::FilePath),
        )
        .arg(
            value("version", None)
                .env(env::DAEMON_VERSION)
                .default_value(env!("CARGO_PKG_VERSION"))
                .value_parser(daemon_version),
        )
        .arg(many("wg-endpoint", None))
        .arg(value("wg-mtu", None).value_parser(clap::value_parser!(u32).range(1..)))
        .arg(
            value("wg-port", None)
                .default_value("51820")
                .value_parser(clap::value_parser!(u16).range(51820..=51820)),
        )
        .arg(switch("yes", Some('y')).env(env::AUTO_CONFIRM))
}

fn machine_add() -> Command {
    provisioning_flags(base("add", "Add a remote machine")).arg(positional("destination", true))
}

fn machine_init() -> Command {
    let command = Command::new("init")
        .about("Initialise a cluster on this Machine or a remote machine")
        .args(connection_args(false))
        .arg(value("context", Some('c')).default_value("default"))
        .arg(value("dns-endpoint", None).default_value(crate::dns::HOSTED_DNS_ENDPOINT))
        .arg(value("network", None).default_value("10.210.0.0/16"))
        .arg(switch("no-dns", None));
    provisioning_flags(command).arg(positional("destination", false))
}

fn project() -> Command {
    base("project", "Manage projects")
        .arg_required_else_help(true)
        .subcommand(
            base("ls", "List projects")
                .visible_alias("list")
                .arg(json_output()),
        )
        .subcommand(
            base("rm", "Remove a project")
                .visible_aliases(["remove", "delete"])
                .arg(switch("volumes", None).help(
                    "Also remove this Project's visible managed volumes after the plan identifies each one",
                ))
                .arg(switch("yes", Some('y')).env(env::AUTO_CONFIRM))
                .arg(positional("project", true))
                .arg(
                    volume_acceptance().requires("volumes"),
                ),
        )
}

fn proxy() -> Command {
    base("proxy", "Proxy a local port to a service")
        .arg(positional("service", true))
        .arg(positional("port", true))
}

fn ps() -> Command {
    base("ps", "List service containers")
        .arg(
            value("sort", Some('s'))
                .default_value("service")
                .value_parser(["service", "machine", "health"]),
        )
        .arg(json_output())
}

fn service_ls(name: &'static str) -> Command {
    base(name, "List services").arg(json_output())
}

fn volume_acceptance() -> Arg {
    repeated("accept-volume-loss")
        .num_args(1)
        .value_name("name")
        .help("Accept permanent deletion: repeat once per exact volume name in the full deletion list; --yes cannot bypass this")
}

fn service_rm(name: &'static str) -> Command {
    base(name, "Remove services")
        .arg(project_name(Some('p')))
        .arg(switch("volumes", None).help(
            "Also remove this Service's named Docker Volumes after the containers are removed",
        ))
        .arg(switch("yes", Some('y')).env(env::AUTO_CONFIRM))
        .arg(volume_acceptance().requires("volumes"))
        .arg(
            Arg::new("service")
                .required(true)
                .num_args(1..)
                .action(ArgAction::Append),
        )
}

fn run(name: &'static str) -> Command {
    base(name, "Run a service")
        .arg(value("caddyfile", None).value_hint(ValueHint::FilePath))
        .arg(value("cpu", None))
        .arg(value("entrypoint", None))
        .arg(many("env", Some('e')))
        .arg(many("machine", Some('m')))
        .arg(value("memory", None))
        .arg(value("mode", None).default_value("replicated"))
        .arg(value("name", Some('n')))
        .arg(switch("privileged", None))
        .arg(project_name(None))
        .arg(many("publish", Some('p')))
        .arg(value("pull", None).default_value("missing"))
        .arg(value("replicas", None).default_value("1"))
        .arg(value("shm-size", None))
        .arg(many("ulimit", None))
        .arg(switch("recreate", None))
        .arg(switch("skip-health", None))
        .arg(value("user", Some('u')))
        .arg(many("volume", Some('v')))
        .arg(positional("image", true))
        .arg(trailing("command"))
}

fn scale(name: &'static str) -> Command {
    base(name, "Scale a service")
        .arg(project_name(Some('p')))
        .arg(switch("skip-health", None))
        .arg(switch("yes", Some('y')).env(env::AUTO_CONFIRM))
        .arg(positional("service", true))
        .arg(positional("replicas", true))
}

fn start(name: &'static str) -> Command {
    base(name, "Start services").arg(
        Arg::new("service")
            .required(true)
            .num_args(1..)
            .action(ArgAction::Append),
    )
}

fn stop(name: &'static str) -> Command {
    start(name)
        .about("Stop services")
        .arg(value("signal", Some('s')).default_value("SIGTERM"))
        .arg(value("timeout", Some('t')).default_value("10"))
}

fn service() -> Command {
    base("service", "Manage services")
        .visible_alias("svc")
        .arg_required_else_help(true)
        .subcommand(exec("exec"))
        .subcommand(inspect("inspect"))
        .subcommand(service_ls("ls").visible_alias("list"))
        .subcommand(logs("logs", true).visible_alias("log"))
        .subcommand(service_rm("rm").visible_aliases(["remove", "delete"]))
        .subcommand(run("run"))
        .subcommand(scale("scale"))
        .subcommand(start("start"))
        .subcommand(stop("stop"))
}

fn version() -> Command {
    base("version", "Show version information").arg(value("output", Some('o')))
}

fn volume() -> Command {
    base("volume", "Manage volumes")
        .arg_required_else_help(true)
        .subcommand(
            base("create", "Create a volume")
                .arg(value("driver", Some('d')).default_value("local"))
                .arg(many("label", Some('l')))
                .arg(value("machine", Some('m')))
                .arg(many("opt", Some('o')))
                .arg(
                    value("size", None)
                        .value_parser(crate::volume::ProvisionedVolumeSize::parse)
                        .conflicts_with_all(["driver", "opt"]),
                )
                .arg(positional("volume-name", true)),
        )
        .subcommand(
            base("inspect", "Inspect a volume")
                .arg(value("machine", Some('m')))
                .arg(positional("volume-name", true)),
        )
        .subcommand(
            base("ls", "List volumes")
                .visible_alias("list")
                .arg(many("machine", Some('m')))
                .arg(switch("quiet", Some('q')))
                .arg(json_output()),
        )
        .subcommand(
            base("rm", "Remove volumes")
                .visible_aliases(["remove", "delete"])
                .arg(switch("force", Some('f')))
                .arg(many("machine", Some('m')))
                .arg(switch("yes", Some('y')).env(env::AUTO_CONFIRM))
                .arg(
                    Arg::new("volume-name")
                        .required(true)
                        .num_args(1..)
                        .action(ArgAction::Append),
                ),
        )
}

fn wg() -> Command {
    base("wg", "Inspect WireGuard")
        .arg_required_else_help(true)
        .subcommand(base("show", "Show WireGuard configuration").arg(value("machine", Some('m'))))
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
        for command in ["add", "init"] {
            for version in [None, Some("stable"), Some("beta"), Some("1.2.3")] {
                let mut args = vec!["ployz", "machine", command, "root@example.com"];
                if let Some(version) = version {
                    args.extend(["--version", version]);
                }
                let matches = super::command().try_get_matches_from(args).unwrap();
                let matches = matches
                    .subcommand_matches("machine")
                    .unwrap()
                    .subcommand_matches(command)
                    .unwrap();
                assert_eq!(
                    matches.get_one::<String>("version").map(String::as_str),
                    Some(version.unwrap_or(env!("CARGO_PKG_VERSION")))
                );
            }
        }
    }

    #[test]
    fn build_push_destinations_are_validated() {
        assert!(
            super::command()
                .try_get_matches_from(["ployz", "build", "--push", "--push-registry"])
                .is_err()
        );
        assert!(
            super::command()
                .try_get_matches_from(["ployz", "build", "--machine", "machine-1"])
                .is_err()
        );
        assert!(
            super::command()
                .try_get_matches_from(["ployz", "build", "--push", "--machine", "machine-1"])
                .is_ok()
        );
        assert!(
            super::command()
                .try_get_matches_from(["ployz", "build", "--check", "--push"])
                .is_ok()
        );
    }

    #[test]
    fn compose_short_flags_are_bound_per_command() {
        let deploy = super::command()
            .try_get_matches_from(["ployz", "deploy", "-p", "shop", "--profile", "prod"])
            .unwrap();
        let deploy = deploy.subcommand().unwrap().1;
        assert_eq!(
            deploy.get_one::<String>("project-name").map(String::as_str),
            Some("shop")
        );
        assert_eq!(
            deploy
                .get_many::<String>("profile")
                .unwrap()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["prod"]
        );

        let run = super::command()
            .try_get_matches_from(["ployz", "run", "-p", "8080/https", "alpine"])
            .unwrap();
        let run = run.subcommand().unwrap().1;
        assert!(run.get_one::<String>("project-name").is_none());
        assert_eq!(
            run.get_many::<String>("publish")
                .unwrap()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["8080/https"]
        );
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
                "project",
                "rm",
                "app",
                "--volumes",
                "--accept-volume-loss",
                "data",
                "--accept-volume-loss",
                "logs",
            ],
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
                "project",
                "rm",
                "app",
                "--accept-volume-loss",
                "data",
            ],
            vec![
                "ployz",
                "machine",
                "rm",
                "worker",
                "--no-reset",
                "--accept-volume-loss",
                "data",
            ],
            vec!["ployz", "project", "rm", "app", "--volumes", "data"],
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
    fn project_commands_use_list_remove_aliases_and_long_only_volumes() {
        let listed = super::command()
            .try_get_matches_from(["ployz", "project", "list"])
            .unwrap();
        assert_eq!(
            listed
                .subcommand_matches("project")
                .unwrap()
                .subcommand_name(),
            Some("ls")
        );
        let removed = super::command()
            .try_get_matches_from(["ployz", "project", "delete", "shop", "--volumes", "--yes"])
            .unwrap();
        let rm = removed
            .subcommand_matches("project")
            .unwrap()
            .subcommand_matches("rm")
            .unwrap();
        assert_eq!(
            rm.get_one::<String>("project").map(String::as_str),
            Some("shop")
        );
        assert!(rm.get_flag("volumes"));
        assert!(rm.get_flag("yes"));
        let named = super::command()
            .try_get_matches_from([
                "ployz",
                "project",
                "rm",
                "shop",
                "--volumes",
                "--accept-volume-loss",
                "shop_data",
                "--accept-volume-loss",
                "shop_logs",
                "--yes",
            ])
            .unwrap();
        let rm = named
            .subcommand_matches("project")
            .unwrap()
            .subcommand_matches("rm")
            .unwrap();
        assert_eq!(
            rm.get_many::<String>("accept-volume-loss")
                .unwrap()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["shop_data", "shop_logs"]
        );
        assert!(
            super::command()
                .try_get_matches_from(["ployz", "project", "rm", "shop", "-v"])
                .is_err()
        );
    }

    #[test]
    fn service_rm_volumes_takes_long_data_loss_and_yes() {
        for args in [
            vec![
                "ployz",
                "rm",
                "db",
                "--volumes",
                "--yes",
                "--accept-volume-loss",
                "app_data",
            ],
            vec![
                "ployz",
                "service",
                "rm",
                "db",
                "--volumes",
                "--yes",
                "--accept-volume-loss",
                "app_data",
            ],
        ] {
            let matches = super::command().try_get_matches_from(args.clone()).unwrap();
            let mut leaf = &matches;
            while let Some((_, child)) = leaf.subcommand() {
                leaf = child;
            }
            assert_eq!(
                leaf.get_one::<String>("service").map(String::as_str),
                Some("db"),
                "{args:?}"
            );
            assert!(leaf.get_flag("volumes"), "{args:?}");
            assert!(leaf.get_flag("yes"), "{args:?}");
            assert_eq!(
                leaf.get_many::<String>("accept-volume-loss")
                    .unwrap()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                ["app_data"],
                "{args:?}"
            );
        }

        let comma = super::command()
            .try_get_matches_from([
                "ployz",
                "rm",
                "db",
                "--volumes",
                "--accept-volume-loss",
                "app_data,app_logs",
                "--yes",
            ])
            .unwrap();
        let rm = comma.subcommand_matches("rm").unwrap();
        assert_eq!(
            rm.get_many::<String>("accept-volume-loss")
                .unwrap()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["app_data,app_logs"]
        );

        assert!(
            super::command()
                .try_get_matches_from(["ployz", "rm", "db", "--accept-volume-loss", "app_data"])
                .is_err()
        );
        assert!(
            super::command()
                .try_get_matches_from(["ployz", "start", "db", "--volumes"])
                .is_err()
        );
        assert!(
            super::command()
                .try_get_matches_from(["ployz", "rm", "db", "--volumes", "-v"])
                .is_err()
        );

        let repeated = super::command()
            .try_get_matches_from([
                "ployz",
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
        let rm = repeated.subcommand_matches("rm").unwrap();
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
                "rm",
                "db",
                "--volumes",
                "--accept-volume-loss",
                "app_data",
                "api",
                "--yes",
            ])
            .unwrap();
        let rm = trailing.subcommand_matches("rm").unwrap();
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
