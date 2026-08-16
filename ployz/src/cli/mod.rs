use clap::{Arg, ArgAction, Command, ValueHint};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("nightly is not a supported release channel")]
struct UnsupportedDaemonChannel;

pub mod env {
    pub const AUTO_CONFIRM: &str = "PLOYZ_AUTO_CONFIRM";
    pub const COMPOSE_DISABLE_ENV_FILE: &str = "COMPOSE_DISABLE_ENV_FILE";
    pub const COMPOSE_FILE: &str = "COMPOSE_FILE";
    pub const CONFIG: &str = "PLOYZ_CONFIG";
    pub const CONNECT: &str = "PLOYZ_CONNECT";
    pub const CONTEXT: &str = "PLOYZ_CONTEXT";
    pub const DAEMON_VERSION: &str = "PLOYZ_DAEMON_VERSION";
    pub const DEBUG: &str = "DEBUG";
    pub const FAILED_CONTAINER_LOGS_TAIL: &str = "PLOYZ_FAILED_CONTAINER_LOGS_TAIL";
    pub const HEALTH_MONITOR_PERIOD: &str = "PLOYZ_HEALTH_MONITOR_PERIOD";
    pub const SSH_CONTROL_PERSIST: &str = "PLOYZ_SSH_CONTROL_PERSIST";

    pub const ALL: &[&str] = &[
        AUTO_CONFIRM,
        COMPOSE_DISABLE_ENV_FILE,
        COMPOSE_FILE,
        CONFIG,
        CONNECT,
        CONTEXT,
        DAEMON_VERSION,
        DEBUG,
        FAILED_CONTAINER_LOGS_TAIL,
        HEALTH_MONITOR_PERIOD,
        SSH_CONTROL_PERSIST,
    ];
}

#[must_use]
pub fn command() -> Command {
    base("ployz", "Manage Ployz machines, services, and volumes")
        .subcommand(build())
        .subcommand(caddy())
        .subcommand(ctx())
        .subcommand(deploy())
        .subcommand(dns())
        .subcommand(exec("exec"))
        .subcommand(image())
        .subcommand(images())
        .subcommand(inspect("inspect"))
        .subcommand(logs("logs", true))
        .subcommand(service_ls("ls"))
        .subcommand(machine())
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

fn value(name: &'static str, short: Option<char>) -> Arg {
    let arg = Arg::new(name).long(name).action(ArgAction::Set);
    match short {
        Some(short) => arg.short(short),
        None => arg,
    }
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

fn build() -> Command {
    base("build", "Build service images")
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
        .arg(repeated("build-arg"))
        .arg(switch("build-pull", None))
        .arg(many("file", Some('f')).default_value("compose.yaml"))
        .arg(switch("no-build", None))
        .arg(switch("no-cache", None))
        .arg(many("profile", Some('p')))
        .arg(switch("recreate", None))
        .arg(switch("skip-health", None))
        .arg(switch("yes", Some('y')).env(env::AUTO_CONFIRM))
        .arg(Arg::new("service").num_args(0..).action(ArgAction::Append))
}

fn caddy() -> Command {
    base("caddy", "Manage Caddy")
        .arg_required_else_help(true)
        .subcommand(base("config", "Print Caddy configuration").arg(value("machine", Some('m'))))
        .subcommand(
            base("deploy", "Deploy Caddy")
                .arg(value("caddyfile", None).value_hint(ValueHint::FilePath))
                .arg(value("image", None))
                .arg(many("machine", Some('m'))),
        )
        .subcommand(log_flags(base("logs", "Show Caddy logs"), false).visible_alias("log"))
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
}

fn dns() -> Command {
    base("dns", "Manage the cluster domain")
        .arg_required_else_help(true)
        .subcommand(base("release", "Release the cluster domain"))
        .subcommand(
            base("reserve", "Reserve a cluster domain")
                .arg(value("endpoint", None).default_value("https://dns.uncloud.run/v1")),
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
        .arg(positional("image", false))
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
                .arg(switch("no-reset", None))
                .arg(switch("yes", Some('y')))
                .arg(positional("machine", true)),
        )
        .subcommand(base("rtt", "Show round-trip times"))
        .subcommand(
            base("update", "Update machine configuration")
                .arg(value("name", None))
                .arg(value("public-ip", None))
                .arg(many("wg-endpoint", None))
                .arg(positional("machine", true)),
        )
}

fn provisioning_flags(command: Command) -> Command {
    command
        .arg(value("name", Some('n')))
        .arg(switch("no-caddy", None))
        .arg(switch("no-install", None))
        .arg(value("public-ip", None).default_value("auto"))
        .arg(
            value("ssh-key", Some('i'))
                .default_value("~/.ssh/id_ed25519")
                .value_hint(ValueHint::FilePath),
        )
        .arg(
            value("version", None)
                .env(env::DAEMON_VERSION)
                .default_value("latest")
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
        .about("Initialise a cluster on a remote machine")
        .args(connection_args(false))
        .arg(value("context", Some('c')).default_value("default"))
        .arg(value("dns-endpoint", None).default_value("https://dns.uncloud.run/v1"))
        .arg(value("network", None).default_value("10.210.0.0/16"))
        .arg(switch("no-dns", None));
    provisioning_flags(command).arg(positional("destination", false))
}

fn proxy() -> Command {
    base("proxy", "Proxy a local port to a service")
        .arg(positional("service", true))
        .arg(positional("port", true))
}

fn ps() -> Command {
    base("ps", "List service containers").arg(
        value("sort", Some('s'))
            .default_value("service")
            .value_parser(["service", "machine", "health"]),
    )
}

fn service_ls(name: &'static str) -> Command {
    base(name, "List services")
}

fn service_rm(name: &'static str) -> Command {
    base(name, "Remove services").arg(
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
        .arg(many("publish", Some('p')))
        .arg(value("pull", None).default_value("missing"))
        .arg(value("replicas", None).default_value("1"))
        .arg(value("shm-size", None))
        .arg(many("ulimit", None))
        .arg(value("user", Some('u')))
        .arg(many("volume", Some('v')))
        .arg(positional("image", true))
        .arg(trailing("command"))
}

fn scale(name: &'static str) -> Command {
    base(name, "Scale a service")
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
                .arg(switch("quiet", Some('q'))),
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
}
