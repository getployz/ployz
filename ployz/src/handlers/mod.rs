use clap::{ArgMatches, Command};
use clap_complete::{Shell, generate};

pub type Error = String;

pub fn run() -> Result<(), Error> {
    let mut command = crate::cli::command();
    let matches = command.clone().get_matches();
    dispatch(&matches, &mut command)
}

fn dispatch(matches: &ArgMatches, command: &mut Command) -> Result<(), Error> {
    match Action::from_matches(matches)? {
        Action::Help => {
            command.print_help().map_err(|error| error.to_string())?;
            println!();
            Ok(())
        }
        Action::Completion(shell) => {
            generate(shell, command, "ployz", &mut std::io::stdout());
            Ok(())
        }
        Action::LocalMachineInit => {
            Err("local machine initialisation is not implemented; specify a remote machine".into())
        }
        Action::Stub(command) => not_implemented(command.path()),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action {
    Help,
    Completion(Shell),
    LocalMachineInit,
    Stub(StubCommand),
}

impl Action {
    fn from_matches(matches: &ArgMatches) -> Result<Self, Error> {
        let Some((name, child)) = matches.subcommand() else {
            return Ok(Self::Help);
        };
        if name == "completion" {
            let shell = child
                .get_one::<Shell>("shell")
                .copied()
                .ok_or_else(|| "completion shell is required".to_owned())?;
            return Ok(Self::Completion(shell));
        }
        if name == "machine"
            && child.subcommand_name() == Some("init")
            && child
                .subcommand_matches("init")
                .and_then(|init| init.get_one::<String>("destination"))
                .is_none()
        {
            return Ok(Self::LocalMachineInit);
        }
        StubCommand::from_matches(name, child)
            .map(Self::Stub)
            .ok_or_else(|| format!("no handler declared for ployz {}", command_path(matches)))
    }
}

macro_rules! stub_commands {
    ($($variant:ident => $path:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        enum StubCommand {
            $($variant,)+
        }

        impl StubCommand {
            #[cfg(test)]
            const ALL: &[Self] = &[$(Self::$variant,)+];

            const fn path(self) -> &'static str {
                match self {
                    $(Self::$variant => $path,)+
                }
            }
        }
    };
}

stub_commands! {
    Build => "build",
    CaddyConfig => "caddy config",
    CaddyDeploy => "caddy deploy",
    CaddyLogs => "caddy logs",
    Context => "ctx",
    ContextConnection => "ctx connection",
    ContextList => "ctx ls",
    ContextShow => "ctx show",
    ContextUse => "ctx use",
    Deploy => "deploy",
    DnsRelease => "dns release",
    DnsReserve => "dns reserve",
    DnsShow => "dns show",
    Exec => "exec",
    ImageList => "image ls",
    ImagePush => "image push",
    Images => "images",
    Inspect => "inspect",
    Logs => "logs",
    List => "ls",
    MachineAdd => "machine add",
    MachineInit => "machine init",
    MachineLogs => "machine logs",
    MachineList => "machine ls",
    MachineRename => "machine rename",
    MachineRemove => "machine rm",
    MachineRtt => "machine rtt",
    MachineUpdate => "machine update",
    Proxy => "proxy",
    ProcessList => "ps",
    Remove => "rm",
    Run => "run",
    Scale => "scale",
    ServiceExec => "service exec",
    ServiceInspect => "service inspect",
    ServiceLogs => "service logs",
    ServiceList => "service ls",
    ServiceRemove => "service rm",
    ServiceRun => "service run",
    ServiceScale => "service scale",
    ServiceStart => "service start",
    ServiceStop => "service stop",
    Start => "start",
    Stop => "stop",
    Version => "version",
    VolumeCreate => "volume create",
    VolumeInspect => "volume inspect",
    VolumeList => "volume ls",
    VolumeRemove => "volume rm",
    WireGuardShow => "wg show",
}

impl StubCommand {
    fn from_matches(name: &str, matches: &ArgMatches) -> Option<Self> {
        let child = matches.subcommand_name();
        match (name, child) {
            ("build", None) => Some(Self::Build),
            ("caddy", Some("config")) => Some(Self::CaddyConfig),
            ("caddy", Some("deploy")) => Some(Self::CaddyDeploy),
            ("caddy", Some("logs")) => Some(Self::CaddyLogs),
            ("ctx", None) => Some(Self::Context),
            ("ctx", Some("connection")) => Some(Self::ContextConnection),
            ("ctx", Some("ls")) => Some(Self::ContextList),
            ("ctx", Some("show")) => Some(Self::ContextShow),
            ("ctx", Some("use")) => Some(Self::ContextUse),
            ("deploy", None) => Some(Self::Deploy),
            ("dns", Some("release")) => Some(Self::DnsRelease),
            ("dns", Some("reserve")) => Some(Self::DnsReserve),
            ("dns", Some("show")) => Some(Self::DnsShow),
            ("exec", None) => Some(Self::Exec),
            ("image", Some("ls")) => Some(Self::ImageList),
            ("image", Some("push")) => Some(Self::ImagePush),
            ("images", None) => Some(Self::Images),
            ("inspect", None) => Some(Self::Inspect),
            ("logs", None) => Some(Self::Logs),
            ("ls", None) => Some(Self::List),
            ("machine", Some("add")) => Some(Self::MachineAdd),
            ("machine", Some("init")) => Some(Self::MachineInit),
            ("machine", Some("logs")) => Some(Self::MachineLogs),
            ("machine", Some("ls")) => Some(Self::MachineList),
            ("machine", Some("rename")) => Some(Self::MachineRename),
            ("machine", Some("rm")) => Some(Self::MachineRemove),
            ("machine", Some("rtt")) => Some(Self::MachineRtt),
            ("machine", Some("update")) => Some(Self::MachineUpdate),
            ("proxy", None) => Some(Self::Proxy),
            ("ps", None) => Some(Self::ProcessList),
            ("rm", None) => Some(Self::Remove),
            ("run", None) => Some(Self::Run),
            ("scale", None) => Some(Self::Scale),
            ("service", Some("exec")) => Some(Self::ServiceExec),
            ("service", Some("inspect")) => Some(Self::ServiceInspect),
            ("service", Some("logs")) => Some(Self::ServiceLogs),
            ("service", Some("ls")) => Some(Self::ServiceList),
            ("service", Some("rm")) => Some(Self::ServiceRemove),
            ("service", Some("run")) => Some(Self::ServiceRun),
            ("service", Some("scale")) => Some(Self::ServiceScale),
            ("service", Some("start")) => Some(Self::ServiceStart),
            ("service", Some("stop")) => Some(Self::ServiceStop),
            ("start", None) => Some(Self::Start),
            ("stop", None) => Some(Self::Stop),
            ("version", None) => Some(Self::Version),
            ("volume", Some("create")) => Some(Self::VolumeCreate),
            ("volume", Some("inspect")) => Some(Self::VolumeInspect),
            ("volume", Some("ls")) => Some(Self::VolumeList),
            ("volume", Some("rm")) => Some(Self::VolumeRemove),
            ("wg", Some("show")) => Some(Self::WireGuardShow),
            _ => None,
        }
    }
}

fn command_path(mut matches: &ArgMatches) -> String {
    let mut parts = Vec::new();
    while let Some((name, child)) = matches.subcommand() {
        parts.push(name);
        matches = child;
    }
    parts.join(" ")
}

fn not_implemented(command: &str) -> Result<(), Error> {
    Err(format!("ployz {command} is not implemented yet"))
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
    fn every_actionable_clap_command_has_a_handler() {
        let mut command = crate::cli::command();
        command.build();
        let mut actual = BTreeSet::new();
        collect_actionable_paths(&command, "", &mut actual);
        actual.remove("completion");

        let expected = StubCommand::ALL
            .iter()
            .map(|command| command.path())
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected);
    }

    fn collect_actionable_paths(
        command: &Command,
        parent: &str,
        paths: &mut BTreeSet<&'static str>,
    ) {
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
            let relative = path.strip_prefix("ployz ").unwrap();
            let registered = StubCommand::ALL
                .iter()
                .find_map(|command| (command.path() == relative).then_some(command.path()))
                .unwrap_or_else(|| {
                    if relative == "completion" {
                        "completion"
                    } else {
                        panic!("no handler registered for {relative}")
                    }
                });
            paths.insert(registered);
        }
        for child in children {
            collect_actionable_paths(child, &path, paths);
        }
    }
}
