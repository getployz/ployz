//! Small CLI command contracts.

use std::fmt;

use clap::{Args, Parser, Subcommand};

pub mod deploy;
pub mod init;
pub mod logs;
pub mod machine;
pub mod ops;
pub mod role_policy;
pub mod service;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PloyzctlInvocation {
    pub nats_url: Option<String>,
    pub command: PloyzctlCommand,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PloyzctlCommand {
    Deploy(deploy::DeployCommand),
    InternalInit(Box<init::FirstMachineInitCommand>),
    InitFirstMachineActivate(init::FirstMachineActivateCommand),
    InitJoinTemplate(init::join_template::MachineJoinTemplateCommand),
    MachineInit(machine::MachineInitCommand),
    MachineAdd(machine::MachineAddCommand),
    MachineAddRemote(machine::MachineAddRemoteCommand),
    MachineUpdate(machine::MachineUpdateCommand),
    MachineDrain(machine::MachineDrainCommand),
    MachineResume(machine::MachineResumeCommand),
    MachineList(machine::MachineListCommand),
    MachineInspect(machine::MachineInspectCommand),
    ServiceList(service::ServiceListCommand),
    ServiceInspect(service::ServiceInspectCommand),
    LogsTail(logs::LogsTailCommand),
    OpsList(ops::OpsListCommand),
    OpsStatus(ops::OpsStatusCommand),
    OpsWatch(ops::OpsWatchCommand),
}

pub fn parse_invocation(
    args: impl IntoIterator<Item = String>,
) -> Result<PloyzctlInvocation, PloyzctlCliError> {
    let parsed = InvocationCli::try_parse_from(std::iter::once("ployzctl".to_owned()).chain(args))
        .map_err(PloyzctlCliError::Clap)?;
    let Some(command) = parsed.command else {
        return Err(PloyzctlCliError::Clap(no_subcommand_help()));
    };

    Ok(PloyzctlInvocation {
        nats_url: parsed.nats,
        command: command_from_cli(command)?,
    })
}

/// Bare `ployzctl` renders help and exits successfully, exactly like
/// `ployzctl --help`. A missing nested subcommand (`ployzctl service`)
/// stays a usage error, so this cannot be `arg_required_else_help` — that
/// error kind is shared with the nested case.
fn no_subcommand_help() -> clap::Error {
    InvocationCli::try_parse_from(["ployzctl", "--help"])
        .expect_err("--help always parses as a DisplayHelp error")
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
    #[command(name = "ls", alias = "list")]
    List(service::EmptyCli),
    Inspect(service::ServiceInspectCli),
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
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        command: InternalCli,
    },
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
struct InitRootCli {
    #[command(flatten)]
    init: init::InitCli,
    #[command(subcommand)]
    command: Option<InitCli>,
}

#[derive(Debug, Subcommand)]
enum InitCli {
    ActivateFirstMachine(init::FirstMachineActivateCli),
    JoinTemplate(init::join_template::MachineJoinTemplateCli),
}

#[derive(Debug, Subcommand)]
enum MachineCli {
    Init(machine::MachineInitCli),
    Add(machine::MachineAddRemoteCli),
    Update(machine::MachineUpdateCli),
    Drain(machine::MachineLifecycleCli),
    Resume(machine::MachineLifecycleCli),
    #[command(alias = "ls")]
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
    #[command(alias = "ls")]
    List(ops::OpsListCli),
    Status(ops::OpsStatusCli),
    Watch(ops::OpsWatchCli),
}

#[derive(Debug, Subcommand)]
enum InternalCli {
    Init(InitRootCli),
    MachineAdd(machine::MachineAddCli),
}

fn command_from_cli(command: CommandCli) -> Result<PloyzctlCommand, PloyzctlCliError> {
    match command {
        CommandCli::Deploy(command) => deploy::deploy_command(command).map(PloyzctlCommand::Deploy),
        CommandCli::List(command) => Ok(PloyzctlCommand::ServiceList(
            service::service_list_command(command),
        )),
        CommandCli::Inspect(command) => {
            service::service_inspect_command(command).map(PloyzctlCommand::ServiceInspect)
        }
        CommandCli::Machine { command } => match command {
            MachineCli::Init(command) => {
                machine::machine_init_command(command).map(PloyzctlCommand::MachineInit)
            }
            MachineCli::Add(command) => {
                machine::machine_add_remote_command(command).map(PloyzctlCommand::MachineAddRemote)
            }
            MachineCli::Update(command) => {
                machine::machine_update_command(command).map(PloyzctlCommand::MachineUpdate)
            }
            MachineCli::Drain(command) => {
                machine::machine_drain_command(command).map(PloyzctlCommand::MachineDrain)
            }
            MachineCli::Resume(command) => {
                machine::machine_resume_command(command).map(PloyzctlCommand::MachineResume)
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
            OpsCli::List(command) => Ok(PloyzctlCommand::OpsList(ops::ops_list_command(command))),
            OpsCli::Status(command) => {
                ops::ops_status_command(command).map(PloyzctlCommand::OpsStatus)
            }
            OpsCli::Watch(command) => {
                ops::ops_watch_command(command).map(PloyzctlCommand::OpsWatch)
            }
        },
        CommandCli::Internal { command } => match command {
            InternalCli::Init(command) => init_command_from_cli(command),
            InternalCli::MachineAdd(command) => {
                machine::machine_add_command(command).map(|parsed| match parsed {
                    machine::ParsedMachineAdd::Explicit(command) => {
                        PloyzctlCommand::MachineAdd(command)
                    }
                    machine::ParsedMachineAdd::Remote(command) => {
                        PloyzctlCommand::MachineAddRemote(command)
                    }
                })
            }
        },
    }
}

fn init_command_from_cli(command: InitRootCli) -> Result<PloyzctlCommand, PloyzctlCliError> {
    match command.command {
        Some(InitCli::ActivateFirstMachine(subcommand)) => {
            init::first_machine_activate_command(subcommand)
                .map(PloyzctlCommand::InitFirstMachineActivate)
        }
        Some(InitCli::JoinTemplate(subcommand)) => {
            init::join_template::machine_join_template_command(subcommand)
                .map(PloyzctlCommand::InitJoinTemplate)
        }
        None => init::init_command(command.init)
            .map(|command| PloyzctlCommand::InternalInit(Box::new(command))),
    }
}

pub(crate) fn invalid_value(flag: &'static str, error: impl fmt::Display) -> PloyzctlCliError {
    PloyzctlCliError::InvalidValue {
        flag,
        message: error.to_string(),
    }
}

pub(crate) fn cli_error(message: impl Into<String>) -> PloyzctlCliError {
    PloyzctlCliError::Usage {
        message: message.into(),
    }
}

#[derive(Debug)]
pub enum PloyzctlCliError {
    InvalidValue { flag: &'static str, message: String },
    Usage { message: String },
    Clap(clap::Error),
}

impl PloyzctlCliError {
    /// Help requests parse as errors so the binary can route the rendered
    /// help to stdout and exit successfully.
    #[must_use]
    pub fn is_help_requested(&self) -> bool {
        match self {
            Self::Clap(error) => error.kind() == clap::error::ErrorKind::DisplayHelp,
            Self::InvalidValue { .. } | Self::Usage { .. } => false,
        }
    }
}

impl fmt::Display for PloyzctlCliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidValue { flag, message } => {
                write!(formatter, "{flag} has an invalid value: {message}")
            }
            Self::Usage { message } => formatter.write_str(message),
            Self::Clap(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for PloyzctlCliError {}
