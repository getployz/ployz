//! Small CLI command contracts.

use std::fmt;

pub mod deploy;
pub mod init;
pub mod machine;
pub mod ops;

pub const USAGE: &str = "ployzctl init --node <id> [--gateway]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PloyzctlCommand {
    Init(init::FirstNodeInitCommand),
    Help,
}

pub fn parse_command(
    args: impl IntoIterator<Item = String>,
) -> Result<PloyzctlCommand, PloyzctlCliError> {
    let args = args.into_iter().collect::<Vec<_>>();
    match args.as_slice() {
        [] => Ok(PloyzctlCommand::Help),
        [flag] if flag == "--help" || flag == "-h" => Ok(PloyzctlCommand::Help),
        [command, rest @ ..] if command == "init" => {
            init::parse_init_command(rest).map(PloyzctlCommand::Init)
        }
        [unknown, ..] => Err(PloyzctlCliError::UnexpectedArgument {
            value: unknown.clone(),
        }),
    }
}

#[must_use]
pub fn render_command(command: PloyzctlCommand) -> String {
    match command {
        PloyzctlCommand::Init(command) => command.render(),
        PloyzctlCommand::Help => format!("{USAGE}\n"),
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
