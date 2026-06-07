//! Small CLI command contracts.

use std::fmt;

pub mod deploy;
pub mod init;
pub mod machine;
pub mod ops;

pub const USAGE: &str = "\
ployzctl [--nats <url>] <command>

ployzctl init --node <id> [--gateway] [--emit-keeper-install --ployzd-version <version> --ployzd-source <path> --ployzd-sha256 <sha256> --ployzd-install-path <path> --nats-binary <path> --nats-config <path>]
ployzctl deploy --detach --service <id> --revision <id> --image <ref> --replicas <n> --operation <id> --idempotency-key <key>
ployzctl machine add --node <id> --name <name> --operation <id> --idempotency-key <key> [--gateway]
ployzctl ops watch <operation_id>";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PloyzctlInvocation {
    pub nats_url: Option<String>,
    pub command: PloyzctlCommand,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PloyzctlCommand {
    Deploy(deploy::DetachedDeployCommand),
    Init(init::FirstNodeInitCommand),
    MachineAdd(machine::MachineAddCommand),
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
        [command, rest @ ..] if command == "init" => {
            init::parse_init_command(rest).map(PloyzctlCommand::Init)
        }
        [command, subcommand, rest @ ..] if command == "machine" && subcommand == "add" => {
            machine::parse_machine_add_command(rest).map(PloyzctlCommand::MachineAdd)
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
    MissingRequiredArgument { flag: &'static str },
    MissingValue { flag: &'static str },
    DuplicateArgument { flag: &'static str },
    InvalidValue { flag: &'static str, message: String },
    UnexpectedArgument { value: String },
}

impl fmt::Display for PloyzctlCliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRequiredArgument { flag } => write!(formatter, "{flag} is required"),
            Self::MissingValue { flag } => write!(formatter, "{flag} requires a value"),
            Self::DuplicateArgument { flag } => write!(formatter, "{flag} was provided twice"),
            Self::InvalidValue { flag, message } => {
                write!(formatter, "{flag} has an invalid value: {message}")
            }
            Self::UnexpectedArgument { value } => write!(formatter, "unexpected argument: {value}"),
        }
    }
}

impl std::error::Error for PloyzctlCliError {}
