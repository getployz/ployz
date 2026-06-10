//! Small CLI command contracts.

use std::fmt;

use clap::{Arg, Command, Parser};

pub mod backup;
pub mod deploy;
pub mod init;
pub mod logs;
pub mod machine;
pub mod ops;
pub mod service;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PloyzctlInvocation {
    pub nats_url: Option<String>,
    pub command: PloyzctlCommand,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PloyzctlCommand {
    Deploy(deploy::DetachedDeployCommand),
    BackupCreate(backup::BackupCreateCommand),
    BackupRestorePlan(backup::BackupRestorePlanCommand),
    Init(init::FirstNodeInitCommand),
    InitFirstNodeActivate(init::FirstNodeActivateCommand),
    InitJoinTemplate(init::join_template::MachineJoinTemplateCommand),
    MachineAdd(machine::MachineAddCommand),
    MachineList(machine::MachineListCommand),
    MachineInspect(machine::MachineInspectCommand),
    ServiceList(service::ServiceListCommand),
    ServiceInspect(service::ServiceInspectCommand),
    LogsTail(logs::LogsTailCommand),
    OpsStatus(ops::OpsStatusCommand),
    OpsWatch(ops::OpsWatchCommand),
    Help(String),
}

pub fn parse_invocation(
    args: impl IntoIterator<Item = String>,
) -> Result<PloyzctlInvocation, PloyzctlCliError> {
    let parsed =
        match InvocationCli::try_parse_from(std::iter::once("ployzctl".to_owned()).chain(args)) {
            Ok(parsed) => parsed,
            Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => {
                return Ok(PloyzctlInvocation {
                    nats_url: None,
                    command: PloyzctlCommand::Help(error.to_string()),
                });
            }
            Err(error) => return Err(clap_error(error)),
        };

    Ok(PloyzctlInvocation {
        nats_url: parsed.nats,
        command: parse_command(parsed.command)?,
    })
}

#[derive(Debug, Parser)]
#[command(name = "ployzctl", disable_help_subcommand = true)]
struct InvocationCli {
    #[arg(long)]
    nats: Option<String>,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

pub fn parse_command(
    args: impl IntoIterator<Item = String>,
) -> Result<PloyzctlCommand, PloyzctlCliError> {
    let args = args.into_iter().collect::<Vec<_>>();
    match args.as_slice() {
        [] => Ok(PloyzctlCommand::Help(help_text())),
        [flag] if flag == "--help" || flag == "-h" => Ok(PloyzctlCommand::Help(help_text())),
        [command, rest @ ..] if command == "deploy" => {
            deploy::parse_deploy_command(rest).map(PloyzctlCommand::Deploy)
        }
        [command, subcommand, rest @ ..] if command == "backup" && subcommand == "create" => {
            backup::parse_backup_create_command(rest).map(PloyzctlCommand::BackupCreate)
        }
        [command, subcommand, rest @ ..] if command == "backup" && subcommand == "restore" => {
            backup::parse_backup_restore_command(rest).map(PloyzctlCommand::BackupRestorePlan)
        }
        [command, subcommand, rest @ ..] if command == "init" && subcommand == "join-template" => {
            init::join_template::parse_machine_join_template_command(rest)
                .map(PloyzctlCommand::InitJoinTemplate)
        }
        [command, subcommand, rest @ ..]
            if command == "init" && subcommand == "activate-first-node" =>
        {
            init::parse_first_node_activate_command(rest)
                .map(PloyzctlCommand::InitFirstNodeActivate)
        }
        [command, rest @ ..] if command == "init" => {
            init::parse_init_command(rest).map(PloyzctlCommand::Init)
        }
        [command, subcommand, rest @ ..] if command == "machine" && subcommand == "add" => {
            machine::parse_machine_add_command(rest).map(PloyzctlCommand::MachineAdd)
        }
        [command, subcommand, rest @ ..] if command == "machine" && subcommand == "list" => {
            machine::parse_machine_list_command(rest).map(PloyzctlCommand::MachineList)
        }
        [command, subcommand, rest @ ..] if command == "machine" && subcommand == "inspect" => {
            machine::parse_machine_inspect_command(rest).map(PloyzctlCommand::MachineInspect)
        }
        [command, subcommand, rest @ ..] if command == "service" && subcommand == "list" => {
            service::parse_service_list_command(rest).map(PloyzctlCommand::ServiceList)
        }
        [command, subcommand, rest @ ..] if command == "service" && subcommand == "inspect" => {
            service::parse_service_inspect_command(rest).map(PloyzctlCommand::ServiceInspect)
        }
        [command, rest @ ..] if command == "logs" => {
            logs::parse_logs_tail_command(rest).map(PloyzctlCommand::LogsTail)
        }
        [command, subcommand, rest @ ..] if command == "ops" && subcommand == "status" => {
            ops::parse_ops_status_command(rest).map(PloyzctlCommand::OpsStatus)
        }
        [command, subcommand, rest @ ..] if command == "ops" && subcommand == "watch" => {
            ops::parse_ops_watch_command(rest).map(PloyzctlCommand::OpsWatch)
        }
        [unknown, ..] => Err(cli_error(format!("unexpected argument: {unknown}"))),
    }
}

#[must_use]
pub fn help_text() -> String {
    let mut command = Command::new("ployzctl")
        .disable_help_subcommand(true)
        .arg(Arg::new("nats").long("nats").value_name("url").global(true))
        .subcommand(Command::new("init"))
        .subcommand(Command::new("backup"))
        .subcommand(Command::new("deploy"))
        .subcommand(Command::new("machine"))
        .subcommand(Command::new("service"))
        .subcommand(Command::new("logs"))
        .subcommand(Command::new("ops"))
        .after_help(
            "Commands:
  ployzctl init activate-first-node --node <id> [--gateway]
  ployzctl init --node <id> [--gateway]
  ployzctl init (--emit-keeper-install | --run-keeper-install) --install-spec <path|-> [--keeper-binary <path>]
  ployzctl init join-template --cluster <name> --runtime-nats-url <url> --trusted-first-node <node_id> --trusted-nats-ca-file <path> --artifact-spec <path|-> --secret-delivery-file <path>
  ployzctl backup create --operation <id> --idempotency-key <key>
  ployzctl backup restore --plan
  ployzctl deploy --detach --service <id> --revision <id> --image <ref> --replicas <n> --operation <id> --idempotency-key <key> [--route-hostname <host> --route-port <port> --endpoint-port <port>]
  ployzctl machine add --node <id> --name <name> --operation <id> --idempotency-key <key> [--gateway]
  ployzctl machine list
  ployzctl machine inspect <node_id>
  ployzctl service list
  ployzctl service inspect <service_id>
  ployzctl logs <container_id> [--node <node_id>] [--tail <n>]
  ployzctl ops status <operation_id>
  ployzctl ops watch [--json] <operation_id>",
        );
    command.render_help().to_string()
}

pub(crate) fn invalid_value(flag: &'static str, error: impl fmt::Display) -> PloyzctlCliError {
    PloyzctlCliError::InvalidValue {
        flag,
        message: error.to_string(),
    }
}

pub(crate) fn cli_error(message: impl Into<String>) -> PloyzctlCliError {
    PloyzctlCliError::Clap {
        message: message.into(),
    }
}

pub(crate) fn clap_error(error: clap::Error) -> PloyzctlCliError {
    PloyzctlCliError::Clap {
        message: error.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PloyzctlCliError {
    InvalidValue { flag: &'static str, message: String },
    Clap { message: String },
}

impl fmt::Display for PloyzctlCliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidValue { flag, message } => {
                write!(formatter, "{flag} has an invalid value: {message}")
            }
            Self::Clap { message } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for PloyzctlCliError {}
