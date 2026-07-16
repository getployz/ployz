//! Small CLI command contracts.

use std::fmt;

use clap::{Args, Parser, Subcommand};
use ployz_core::machine::MachineLifecycle;

use crate::build::command as build;
use crate::core::command as core;
use crate::deploy::{command as deploy, compose_command as compose};
use crate::ingress::command as ingress;
use crate::logs::command as logs;
use crate::machine::{command as machine, founder as init};
use crate::namespace::command as namespace;
use crate::network::command as network;
use crate::operation::command as ops;
use crate::service::command as service;
use crate::volume::command as volume;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PloyzctlInvocation {
    pub nats_url: Option<String>,
    pub command: PloyzctlCommand,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PloyzctlCommand {
    Login,
    Telemetry(TelemetryCommand),
    BuildSubmit(build::BuildSubmitCommand),
    BuildCancel(build::BuildCancelCommand),
    CorePromote(core::CorePromoteCommand),
    CoreReplace(core::CoreReplaceCommand),
    ComposeCheck(compose::ComposeCheckCommand),
    Deploy(deploy::DeployCommand),
    DeployHistory(deploy::DeployHistoryCommand),
    DeployRollback(deploy::DeployRollbackCommand),
    InternalInit(Box<init::FirstMachineInitCommand>),
    InitFirstMachineActivate(init::FirstMachineActivateCommand),
    InitJoinTemplate(Box<init::join_template::MachineJoinTemplateCommand>),
    IngressConfigure(ingress::IngressConfigureCommand),
    MachineInit(machine::MachineInitCommand),
    MachineAdd(machine::MachineAddCommand),
    MachineAddRemote(machine::MachineAddRemoteCommand),
    MachineUpdate(machine::MachineUpdateCommand),
    MachineStoragePrepare(machine::MachineStoragePrepareCommand),
    MachineLifecycle(machine::MachineLifecycleCommand),
    MachineList(machine::MachineListCommand),
    MachineInspect(machine::MachineInspectCommand),
    NetworkStatus(network::NetworkStatusCommand),
    NetworkResolve(network::NetworkResolveCommand),
    NetworkRepair(network::NetworkRepairCommand),
    ServiceList(service::ServiceListCommand),
    ServiceInspect(service::ServiceInspectCommand),
    ServiceRestart(service::ServiceRestartCommand),
    NamespaceRemove(namespace::NamespaceRemoveCommand),
    VolumeList(volume::VolumeListCommand),
    VolumeRemove(volume::VolumeRemoveCommand),
    LogsTail(logs::LogsTailCommand),
    OpsList(ops::OpsListCommand),
    OpsStatus(ops::OpsStatusCommand),
    OpsWatch(ops::OpsWatchCommand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryCommand {
    Enable,
    Disable,
}

impl PloyzctlCommand {
    #[must_use]
    pub const fn telemetry_name(&self) -> Option<&'static str> {
        match self {
            Self::Login => Some("login"),
            Self::Telemetry(_) => None,
            Self::BuildSubmit(_) => Some("build submit"),
            Self::BuildCancel(_) => Some("build cancel"),
            Self::CorePromote(_) => Some("core promote"),
            Self::CoreReplace(_) => Some("core demote"),
            Self::ComposeCheck(_) => Some("compose check"),
            Self::Deploy(_) => Some("deploy"),
            Self::DeployHistory(_) => Some("deploy history"),
            Self::DeployRollback(_) => Some("deploy rollback"),
            Self::InternalInit(_) => Some("internal init"),
            Self::InitFirstMachineActivate(_) => Some("internal init activate-first-machine"),
            Self::InitJoinTemplate(_) => Some("internal init join-template"),
            Self::IngressConfigure(_) => Some("ingress configure"),
            Self::MachineInit(_) => Some("init"),
            Self::MachineAdd(_) => Some("internal machine-add"),
            Self::MachineAddRemote(_) => Some("machine add"),
            Self::MachineUpdate(_) => Some("machine update"),
            Self::MachineStoragePrepare(_) => Some("machine storage-prepare"),
            Self::MachineLifecycle(command) => match command.target {
                MachineLifecycle::Draining => Some("machine drain"),
                MachineLifecycle::Active => Some("machine resume"),
            },
            Self::MachineList(_) => Some("machine list"),
            Self::MachineInspect(_) => Some("machine inspect"),
            Self::NetworkStatus(_) => Some("network status"),
            Self::NetworkResolve(_) => Some("network resolve"),
            Self::NetworkRepair(_) => Some("network repair"),
            Self::ServiceList(_) => Some("service list"),
            Self::ServiceInspect(_) => Some("service inspect"),
            Self::ServiceRestart(_) => Some("service restart"),
            Self::NamespaceRemove(_) => Some("namespace rm"),
            Self::VolumeList(_) => Some("volume list"),
            Self::VolumeRemove(_) => Some("volume rm"),
            Self::LogsTail(_) => Some("logs"),
            Self::OpsList(_) => Some("ops list"),
            Self::OpsStatus(_) => Some("ops status"),
            Self::OpsWatch(_) => Some("ops watch"),
        }
    }
}

pub fn parse_invocation(
    args: impl IntoIterator<Item = String>,
) -> Result<PloyzctlInvocation, PloyzctlCliError> {
    let parsed = InvocationCli::try_parse_from(std::iter::once("ployz".to_owned()).chain(args))
        .map_err(PloyzctlCliError::Clap)?;
    let Some(command) = parsed.command else {
        return Err(PloyzctlCliError::Clap(no_subcommand_help()));
    };

    Ok(PloyzctlInvocation {
        nats_url: parsed.nats,
        command: command_from_cli(command)?,
    })
}

/// Bare `ployz` renders help and exits successfully, exactly like
/// `ployz --help`. A missing nested subcommand (`ployz service`)
/// stays a usage error, so this cannot be `arg_required_else_help` — that
/// error kind is shared with the nested case.
fn no_subcommand_help() -> clap::Error {
    InvocationCli::try_parse_from(["ployz", "--help"])
        .expect_err("--help always parses as a DisplayHelp error")
}

#[derive(Debug, Parser)]
#[command(name = "ployz", disable_help_subcommand = true)]
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
    Login(EmptyCli),
    Telemetry {
        #[command(subcommand)]
        command: TelemetryCli,
    },
    Build {
        #[command(subcommand)]
        command: BuildCli,
    },
    Init(machine::MachineInitCli),
    Deploy(deploy::DeployRootCli),
    Compose(compose::ComposeRootCli),
    Core {
        #[command(subcommand)]
        command: CoreCli,
    },
    #[command(name = "ls", alias = "list")]
    List(service::EmptyCli),
    Inspect(service::ServiceInspectCli),
    Machine {
        #[command(subcommand)]
        command: MachineCli,
    },
    Network {
        #[command(subcommand)]
        command: NetworkCli,
    },
    Ingress {
        #[command(subcommand)]
        command: IngressCli,
    },
    Service {
        #[command(subcommand)]
        command: ServiceCli,
    },
    Namespace {
        #[command(subcommand)]
        command: NamespaceCli,
    },
    Volume {
        #[command(subcommand)]
        command: VolumeCli,
    },
    Logs(logs::LogsCli),
    Ops {
        #[command(subcommand)]
        command: OpsCli,
    },
    Host,
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        command: InternalCli,
    },
}

#[derive(Debug, Args)]
struct EmptyCli {}

#[derive(Debug, Subcommand)]
enum TelemetryCli {
    Enable,
    Disable,
}

#[derive(Debug, Subcommand)]
enum BuildCli {
    Submit(build::BuildSubmitCli),
    Cancel(build::BuildCancelCli),
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
    StoragePrepare(machine::MachineStoragePrepareCli),
    Drain(machine::MachineLifecycleCli),
    Resume(machine::MachineLifecycleCli),
    #[command(alias = "ls")]
    List(machine::EmptyCli),
    Inspect(machine::MachineInspectCli),
}

#[derive(Debug, Subcommand)]
enum CoreCli {
    Promote(core::CorePromoteCli),
    Demote(core::CoreReplaceCli),
}

#[derive(Debug, Subcommand)]
enum ServiceCli {
    List(service::EmptyCli),
    Inspect(service::ServiceInspectCli),
    Restart(service::ServiceRestartCli),
}

#[derive(Debug, Subcommand)]
enum NetworkCli {
    Status(network::NetworkStatusCli),
    Resolve(network::NetworkResolveCli),
    Repair(network::NetworkRepairCli),
}

#[derive(Debug, Subcommand)]
enum IngressCli {
    Configure(ingress::IngressConfigureCli),
}

#[derive(Debug, Subcommand)]
enum NamespaceCli {
    Rm(namespace::NamespaceRemoveCli),
}

#[derive(Debug, Subcommand)]
enum VolumeCli {
    #[command(alias = "ls")]
    List(volume::VolumeListCli),
    Rm(volume::VolumeRemoveCli),
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
        CommandCli::Login(_) => Ok(PloyzctlCommand::Login),
        CommandCli::Telemetry { command } => Ok(PloyzctlCommand::Telemetry(match command {
            TelemetryCli::Enable => TelemetryCommand::Enable,
            TelemetryCli::Disable => TelemetryCommand::Disable,
        })),
        CommandCli::Build { command } => match command {
            BuildCli::Submit(command) => {
                build::build_submit_command(command).map(PloyzctlCommand::BuildSubmit)
            }
            BuildCli::Cancel(command) => {
                build::build_cancel_command(command).map(PloyzctlCommand::BuildCancel)
            }
        },
        CommandCli::Init(command) => {
            machine::machine_init_command(command).map(PloyzctlCommand::MachineInit)
        }
        CommandCli::Core { command } => match command {
            CoreCli::Promote(command) => {
                core::core_promote_command(command).map(PloyzctlCommand::CorePromote)
            }
            CoreCli::Demote(command) => {
                core::core_replace_command(command).map(PloyzctlCommand::CoreReplace)
            }
        },
        CommandCli::Deploy(command) => {
            deploy::deploy_command(command).map(|command| match command {
                deploy::ParsedDeployCommand::Deploy(command) => PloyzctlCommand::Deploy(command),
                deploy::ParsedDeployCommand::History(command) => {
                    PloyzctlCommand::DeployHistory(command)
                }
                deploy::ParsedDeployCommand::Rollback(command) => {
                    PloyzctlCommand::DeployRollback(command)
                }
            })
        }
        CommandCli::Compose(command) => {
            compose::compose_command(command).map(PloyzctlCommand::ComposeCheck)
        }
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
            MachineCli::StoragePrepare(command) => {
                machine::machine_storage_prepare_command(command)
                    .map(PloyzctlCommand::MachineStoragePrepare)
            }
            MachineCli::Drain(command) => {
                machine::machine_lifecycle_command(MachineLifecycle::Draining, command)
                    .map(PloyzctlCommand::MachineLifecycle)
            }
            MachineCli::Resume(command) => {
                machine::machine_lifecycle_command(MachineLifecycle::Active, command)
                    .map(PloyzctlCommand::MachineLifecycle)
            }
            MachineCli::List(command) => Ok(PloyzctlCommand::MachineList(
                machine::machine_list_command(command),
            )),
            MachineCli::Inspect(command) => {
                machine::machine_inspect_command(command).map(PloyzctlCommand::MachineInspect)
            }
        },
        CommandCli::Network { command } => match command {
            NetworkCli::Status(command) => Ok(PloyzctlCommand::NetworkStatus(
                network::network_status_command(command),
            )),
            NetworkCli::Resolve(command) => Ok(PloyzctlCommand::NetworkResolve(
                network::network_resolve_command(command),
            )),
            NetworkCli::Repair(command) => {
                network::network_repair_command(command).map(PloyzctlCommand::NetworkRepair)
            }
        },
        CommandCli::Ingress { command } => match command {
            IngressCli::Configure(command) => {
                ingress::ingress_configure_command(command).map(PloyzctlCommand::IngressConfigure)
            }
        },
        CommandCli::Service { command } => match command {
            ServiceCli::List(command) => Ok(PloyzctlCommand::ServiceList(
                service::service_list_command(command),
            )),
            ServiceCli::Inspect(command) => {
                service::service_inspect_command(command).map(PloyzctlCommand::ServiceInspect)
            }
            ServiceCli::Restart(command) => {
                service::service_restart_command(command).map(PloyzctlCommand::ServiceRestart)
            }
        },
        CommandCli::Namespace { command } => match command {
            NamespaceCli::Rm(command) => {
                namespace::namespace_remove_command(command).map(PloyzctlCommand::NamespaceRemove)
            }
        },
        CommandCli::Volume { command } => match command {
            VolumeCli::List(command) => Ok(PloyzctlCommand::VolumeList(
                volume::volume_list_command(command),
            )),
            VolumeCli::Rm(command) => {
                volume::volume_remove_command(command).map(PloyzctlCommand::VolumeRemove)
            }
        },
        CommandCli::Logs(command) => {
            logs::logs_tail_command(command).map(PloyzctlCommand::LogsTail)
        }
        CommandCli::Ops { command } => match command {
            OpsCli::List(command) => ops::ops_list_command(command).map(PloyzctlCommand::OpsList),
            OpsCli::Status(command) => {
                ops::ops_status_command(command).map(PloyzctlCommand::OpsStatus)
            }
            OpsCli::Watch(command) => {
                ops::ops_watch_command(command).map(PloyzctlCommand::OpsWatch)
            }
        },
        CommandCli::Host => Err(cli_error("run Host Runner commands as `ployz host ...`")),
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
                .map(Box::new)
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

#[derive(Debug, thiserror::Error)]
pub enum PloyzctlCliError {
    #[error("{flag} has an invalid value: {message}")]
    InvalidValue { flag: &'static str, message: String },
    #[error("{rendered}")]
    ComposeRejected { rendered: String },
    #[error("{message}")]
    Usage { message: String },
    #[error("{0}")]
    Clap(clap::Error),
}

impl PloyzctlCliError {
    /// Help requests parse as errors so the binary can route the rendered
    /// help to stdout and exit successfully.
    #[must_use]
    pub fn is_help_requested(&self) -> bool {
        match self {
            Self::Clap(error) => error.kind() == clap::error::ErrorKind::DisplayHelp,
            Self::InvalidValue { .. } | Self::ComposeRejected { .. } | Self::Usage { .. } => false,
        }
    }
}
