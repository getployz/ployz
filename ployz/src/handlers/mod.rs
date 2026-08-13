use clap::{ArgMatches, Command};
use clap_complete::{Shell, generate};

pub type Error = String;

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
    if name == "machine"
        && child.subcommand_name() == Some("init")
        && child
            .subcommand_matches("init")
            .and_then(|init| init.get_one::<String>("destination"))
            .is_none()
    {
        return Err(
            "local machine initialisation is not implemented; specify a remote machine".into(),
        );
    }
    let path = command_path(matches);
    dispatch_stub(&path).ok_or_else(|| format!("no handler declared for ployz {path}"))?
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

macro_rules! stub_handlers {
    ($($function:ident => $path:literal),+ $(,)?) => {
        $(fn $function() -> Result<(), Error> {
            not_implemented($path)
        })+

        fn dispatch_stub(path: &str) -> Option<Result<(), Error>> {
            match path {
                $($path => Some($function()),)+
                _ => None,
            }
        }
    };
}

stub_handlers! {
    build => "build",
    caddy_config => "caddy config",
    caddy_deploy => "caddy deploy",
    caddy_logs => "caddy logs",
    context => "ctx",
    context_connection => "ctx connection",
    context_list => "ctx ls",
    context_show => "ctx show",
    context_use => "ctx use",
    deploy => "deploy",
    dns_release => "dns release",
    dns_reserve => "dns reserve",
    dns_show => "dns show",
    exec => "exec",
    image_list => "image ls",
    image_push => "image push",
    images => "images",
    inspect => "inspect",
    logs => "logs",
    list => "ls",
    machine_add => "machine add",
    machine_init => "machine init",
    machine_logs => "machine logs",
    machine_list => "machine ls",
    machine_rename => "machine rename",
    machine_remove => "machine rm",
    machine_rtt => "machine rtt",
    machine_update => "machine update",
    proxy => "proxy",
    process_list => "ps",
    remove => "rm",
    run_service => "run",
    scale => "scale",
    service_exec => "service exec",
    service_inspect => "service inspect",
    service_logs => "service logs",
    service_list => "service ls",
    service_remove => "service rm",
    service_run => "service run",
    service_scale => "service scale",
    service_start => "service start",
    service_stop => "service stop",
    start => "start",
    stop => "stop",
    version => "version",
    volume_create => "volume create",
    volume_inspect => "volume inspect",
    volume_list => "volume ls",
    volume_remove => "volume rm",
    wireguard_show => "wg show",
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
            .try_get_matches_from(["ployz", "service", "inspect", "api"])
            .unwrap();
        assert_eq!(
            dispatch(&matches, &mut command),
            Err("ployz service inspect is not implemented yet".into())
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
            assert!(dispatch_stub(&path).is_some(), "no handler for {path}");
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
