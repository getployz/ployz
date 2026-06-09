//! Small CLI command contracts.

use std::fmt;

pub mod backup;
pub mod deploy;
pub mod init;
pub mod logs;
pub mod machine;
pub mod ops;
pub mod service;

pub const USAGE: &str = "\
ployzctl [--nats <url>] <command>

ployzctl init --node <id> [--gateway] [--activate-first-node]
ployzctl init --node <id> [--gateway] (--emit-keeper-install | --run-keeper-install) --ployzd-version <version> --ployzd-source <path> --ployzd-sha256 <sha256> --ployzd-install-path <path> --nats-version <version> --nats-source <path> --nats-sha256 <sha256> --nats-binary <path> --nats-config <path> [--node-public-ip <ip>] [--machine-bootstrap-url <url>] [--machine-join-template-file <path>] [--keeper-binary <path>]
ployzctl init join-template --cluster <name> --runtime-nats-url <url> --trusted-first-node <node_id> --core-iroh-public-key <key> [--core-iroh-direct-address <addr>] [--core-iroh-relay-url <url>] --ployzd-version <version> --ployzd-source <path> --ployzd-sha256 <sha256> --ployzd-install-path <path> --ebpf-bytecode-version <version> --ebpf-bytecode-source <path> --ebpf-bytecode-sha256 <sha256> --ebpf-bytecode-install-path <path> --ebpf-ctl-version <version> --ebpf-ctl-source <path> --ebpf-ctl-sha256 <sha256> --ebpf-ctl-install-path <path> --secret-delivery-file <path>
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
ployzctl ops watch <operation_id>";

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
    InitJoinTemplate(init::join_template::MachineJoinTemplateCommand),
    MachineAdd(machine::MachineAddCommand),
    MachineList(machine::MachineListCommand),
    MachineInspect(machine::MachineInspectCommand),
    ServiceList(service::ServiceListCommand),
    ServiceInspect(service::ServiceInspectCommand),
    LogsTail(logs::LogsTailCommand),
    OpsStatus(ops::OpsStatusCommand),
    OpsWatch(ops::OpsWatchCommand),
    Help,
}

pub fn parse_invocation(
    args: impl IntoIterator<Item = String>,
) -> Result<PloyzctlInvocation, PloyzctlCliError> {
    let args = args.into_iter().collect::<Vec<_>>();
    let mut nats_url = None;
    let mut command_start = 0;

    while let Some(flag) = args.get(command_start) {
        if flag == "--nats" {
            let Some(value) = args.get(command_start + 1) else {
                return Err(PloyzctlCliError::MissingValue { flag: "--nats" });
            };
            set_once(&mut nats_url, flag_value("--nats", value)?, "--nats")?;
            command_start += 2;
            continue;
        }
        break;
    }

    Ok(PloyzctlInvocation {
        nats_url,
        command: parse_command(args.into_iter().skip(command_start))?,
    })
}

pub fn parse_command(
    args: impl IntoIterator<Item = String>,
) -> Result<PloyzctlCommand, PloyzctlCliError> {
    let args = args.into_iter().collect::<Vec<_>>();
    match args.as_slice() {
        [] => Ok(PloyzctlCommand::Help),
        [flag] if flag == "--help" || flag == "-h" => Ok(PloyzctlCommand::Help),
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
        [unknown, ..] => Err(PloyzctlCliError::UnexpectedArgument {
            value: unknown.clone(),
        }),
    }
}

pub(crate) fn flag_value(flag: &'static str, value: &str) -> Result<String, PloyzctlCliError> {
    if value.starts_with('-') {
        return Err(PloyzctlCliError::MissingValue { flag });
    }
    Ok(value.to_owned())
}

pub(crate) fn set_once<T>(
    slot: &mut Option<T>,
    value: T,
    flag: &'static str,
) -> Result<(), PloyzctlCliError> {
    if slot.is_some() {
        return Err(PloyzctlCliError::DuplicateArgument { flag });
    }
    *slot = Some(value);
    Ok(())
}

pub(crate) fn required<T>(value: Option<T>, flag: &'static str) -> Result<T, PloyzctlCliError> {
    value.ok_or(PloyzctlCliError::MissingRequiredArgument { flag })
}

pub(crate) fn invalid_value(flag: &'static str, error: impl fmt::Display) -> PloyzctlCliError {
    PloyzctlCliError::InvalidValue {
        flag,
        message: error.to_string(),
    }
}

pub(crate) struct ArgCursor<'a> {
    rest: &'a [String],
}

impl<'a> ArgCursor<'a> {
    pub(crate) const fn new(rest: &'a [String]) -> Self {
        Self { rest }
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }

    pub(crate) fn take_flag(&mut self, flag: &'static str) -> bool {
        match self.rest {
            [head, tail @ ..] if head == flag => {
                self.rest = tail;
                true
            }
            _ => false,
        }
    }

    pub(crate) fn take_value(
        &mut self,
        flag: &'static str,
    ) -> Result<Option<String>, PloyzctlCliError> {
        match self.rest {
            [head] if head == flag => Err(PloyzctlCliError::MissingValue { flag }),
            [head, value, tail @ ..] if head == flag => {
                let value = flag_value(flag, value)?;
                self.rest = tail;
                Ok(Some(value))
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn unexpected(&self) -> PloyzctlCliError {
        let [value, ..] = self.rest else {
            unreachable!("unexpected is only called for non-empty args");
        };
        PloyzctlCliError::UnexpectedArgument {
            value: value.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PloyzctlCliError {
    MissingRequiredArgument {
        flag: &'static str,
    },
    MissingValue {
        flag: &'static str,
    },
    DuplicateArgument {
        flag: &'static str,
    },
    ConflictingArguments {
        first: &'static str,
        second: &'static str,
    },
    InvalidValue {
        flag: &'static str,
        message: String,
    },
    UnexpectedArgument {
        value: String,
    },
}

impl fmt::Display for PloyzctlCliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRequiredArgument { flag } => write!(formatter, "{flag} is required"),
            Self::MissingValue { flag } => write!(formatter, "{flag} requires a value"),
            Self::DuplicateArgument { flag } => write!(formatter, "{flag} was provided twice"),
            Self::ConflictingArguments { first, second } => {
                write!(formatter, "{first} and {second} cannot be used together")
            }
            Self::InvalidValue { flag, message } => {
                write!(formatter, "{flag} has an invalid value: {message}")
            }
            Self::UnexpectedArgument { value } => write!(formatter, "unexpected argument: {value}"),
        }
    }
}

impl std::error::Error for PloyzctlCliError {}
