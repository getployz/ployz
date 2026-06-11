//! Small CLI command contracts.

use std::fmt;

use clap::{Args, CommandFactory, Parser, Subcommand};

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
        command: parsed
            .command
            .map(command_from_cli)
            .transpose()?
            .unwrap_or_else(|| PloyzctlCommand::Help(help_text())),
    })
}

#[derive(Debug, Parser)]
#[command(name = "ployzctl", disable_help_subcommand = true)]
struct InvocationCli {
    #[arg(long, global = true)]
    nats: Option<String>,
    #[command(subcommand)]
    command: Option<CommandCli>,
}

pub fn parse_command(
    args: impl IntoIterator<Item = String>,
) -> Result<PloyzctlCommand, PloyzctlCliError> {
    parse_invocation(args).map(|invocation| invocation.command)
}

#[derive(Debug, Subcommand)]
enum CommandCli {
    Deploy(deploy::DeployCli),
    Backup {
        #[command(subcommand)]
        command: BackupCli,
    },
    Init(InitRootCli),
    Machine {
        #[command(subcommand)]
        command: MachineCli,
    },
    Service {
        #[command(subcommand)]
        command: ServiceCli,
    },
    Logs(logs::LogsTailCli),
    Ops {
        #[command(subcommand)]
        command: OpsCli,
    },
}

#[derive(Debug, Subcommand)]
enum BackupCli {
    Create(backup::BackupCreateCli),
    Restore(backup::BackupRestoreCli),
}

#[derive(Debug, Args)]
struct InitRootCli {
    #[command(flatten)]
    init: init::InitCli,
    #[command(subcommand)]
    command: Option<InitCli>,
}

#[derive(Debug, Subcommand)]
enum InitCli {
    ActivateFirstNode(init::FirstNodeActivateCli),
    JoinTemplate(init::join_template::MachineJoinTemplateCli),
}

#[derive(Debug, Subcommand)]
enum MachineCli {
    Add(machine::MachineAddCli),
    List(machine::EmptyCli),
    Inspect(machine::MachineInspectCli),
}

#[derive(Debug, Subcommand)]
enum ServiceCli {
    List(service::EmptyCli),
    Inspect(service::ServiceInspectCli),
}

#[derive(Debug, Subcommand)]
enum OpsCli {
    Status(ops::OpsStatusCli),
    Watch(ops::OpsWatchCli),
}

fn command_from_cli(command: CommandCli) -> Result<PloyzctlCommand, PloyzctlCliError> {
    match command {
        CommandCli::Deploy(command) => deploy::deploy_command(command).map(PloyzctlCommand::Deploy),
        CommandCli::Backup { command } => match command {
            BackupCli::Create(command) => {
                backup::backup_create_command(command).map(PloyzctlCommand::BackupCreate)
            }
            BackupCli::Restore(command) => Ok(PloyzctlCommand::BackupRestorePlan(
                backup::backup_restore_command(command),
            )),
        },
        CommandCli::Init(command) => init_command_from_cli(command),
        CommandCli::Machine { command } => match command {
            MachineCli::Add(command) => {
                machine::machine_add_command(command).map(PloyzctlCommand::MachineAdd)
            }
            MachineCli::List(command) => Ok(PloyzctlCommand::MachineList(
                machine::machine_list_command(command),
            )),
            MachineCli::Inspect(command) => {
                machine::machine_inspect_command(command).map(PloyzctlCommand::MachineInspect)
            }
        },
        CommandCli::Service { command } => match command {
            ServiceCli::List(command) => Ok(PloyzctlCommand::ServiceList(
                service::service_list_command(command),
            )),
            ServiceCli::Inspect(command) => {
                service::service_inspect_command(command).map(PloyzctlCommand::ServiceInspect)
            }
        },
        CommandCli::Logs(command) => {
            logs::logs_tail_command(command).map(PloyzctlCommand::LogsTail)
        }
        CommandCli::Ops { command } => match command {
            OpsCli::Status(command) => {
                ops::ops_status_command(command).map(PloyzctlCommand::OpsStatus)
            }
            OpsCli::Watch(command) => {
                ops::ops_watch_command(command).map(PloyzctlCommand::OpsWatch)
            }
        },
    }
}

fn init_command_from_cli(command: InitRootCli) -> Result<PloyzctlCommand, PloyzctlCliError> {
    match command.command {
        Some(subcommand) => {
            if command.init.has_values() {
                return Err(cli_error(
                    "init command options cannot be combined with init subcommands",
                ));
            }
            match subcommand {
                InitCli::ActivateFirstNode(command) => init::first_node_activate_command(command)
                    .map(PloyzctlCommand::InitFirstNodeActivate),
                InitCli::JoinTemplate(command) => {
                    init::join_template::machine_join_template_command(command)
                        .map(PloyzctlCommand::InitJoinTemplate)
                }
            }
        }
        None => init::init_command(command.init).map(PloyzctlCommand::Init),
    }
}

#[must_use]
pub fn help_text() -> String {
    let mut command = InvocationCli::command();
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
