//! Minimal keeper command-line contract.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use crate::join::{JoinTokenFileError, consume_join_token_file};
use crate::steps::JoinToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperStartup {
    pub join_token: Option<JoinToken>,
}

pub fn load_startup(
    args: impl IntoIterator<Item = OsString>,
) -> Result<KeeperStartup, KeeperCliError> {
    let command = parse_keeper_args(args)?;
    let join_token = match command.join_token_file {
        Some(path) => Some(consume_join_token_file(&path)?),
        None => None,
    };

    Ok(KeeperStartup { join_token })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KeeperCommand {
    join_token_file: Option<PathBuf>,
}

fn parse_keeper_args(
    args: impl IntoIterator<Item = OsString>,
) -> Result<KeeperCommand, KeeperCliError> {
    let args = args.into_iter().collect::<Vec<_>>();
    match args.as_slice() {
        [] => Ok(KeeperCommand {
            join_token_file: None,
        }),
        [flag, path] if flag == "--join-token-file" => Ok(KeeperCommand {
            join_token_file: Some(PathBuf::from(path)),
        }),
        [flag, _path, extra, ..] if flag == "--join-token-file" => {
            Err(KeeperCliError::UnexpectedArgument {
                value: extra.to_string_lossy().into_owned(),
            })
        }
        [flag] if flag == "--join-token-file" => Err(KeeperCliError::MissingJoinTokenFile),
        [flag] if flag == "--help" || flag == "-h" => Err(KeeperCliError::HelpRequested),
        [unknown, ..] => Err(KeeperCliError::UnexpectedArgument {
            value: unknown.to_string_lossy().into_owned(),
        }),
    }
}

#[derive(Debug)]
pub enum KeeperCliError {
    HelpRequested,
    MissingJoinTokenFile,
    UnexpectedArgument { value: String },
    JoinTokenFile(JoinTokenFileError),
}

impl From<JoinTokenFileError> for KeeperCliError {
    fn from(value: JoinTokenFileError) -> Self {
        Self::JoinTokenFile(value)
    }
}

impl fmt::Display for KeeperCliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HelpRequested => formatter.write_str(KEEPER_USAGE),
            Self::MissingJoinTokenFile => {
                formatter.write_str("--join-token-file requires a file path")
            }
            Self::UnexpectedArgument { value } => {
                write!(formatter, "unexpected keeper argument: {value}")
            }
            Self::JoinTokenFile(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for KeeperCliError {}

pub const KEEPER_USAGE: &str = "usage: ployz-keeper [--join-token-file <path>]";

#[cfg(test)]
mod tests {
    use super::{KeeperCliError, parse_keeper_args};

    #[test]
    fn parser_accepts_no_args() {
        let command = parse_keeper_args([]).expect("no args are valid");

        assert_eq!(command.join_token_file, None);
    }

    #[test]
    fn parser_accepts_join_token_file() {
        let command = parse_keeper_args(["--join-token-file".into(), "/tmp/join".into()])
            .expect("join token file is valid");

        assert_eq!(command.join_token_file, Some("/tmp/join".into()));
    }

    #[test]
    fn parser_rejects_missing_join_token_file() {
        assert!(matches!(
            parse_keeper_args(["--join-token-file".into()]),
            Err(KeeperCliError::MissingJoinTokenFile)
        ));
    }

    #[test]
    fn parser_rejects_extra_args() {
        assert!(matches!(
            parse_keeper_args(["--join-token-file".into(), "/tmp/join".into(), "extra".into()]),
            Err(KeeperCliError::UnexpectedArgument { value }) if value == "extra"
        ));
    }
}
