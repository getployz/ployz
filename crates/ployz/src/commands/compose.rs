use std::path::PathBuf;

use clap::{Args, Subcommand};
use ployz_core::ids::NamespaceId;

use super::{PloyzctlCliError, invalid_value};
use crate::compose::{UnsupportedFieldMode, parse_compose_file};

#[derive(Debug, Args)]
pub(crate) struct ComposeRootCli {
    #[command(subcommand)]
    command: ComposeSubcommand,
}

#[derive(Debug, Subcommand)]
enum ComposeSubcommand {
    Check(ComposeCheckCli),
}

#[derive(Debug, Args)]
struct ComposeCheckCli {
    file: PathBuf,
    #[arg(short = 'n', long = "namespace")]
    namespace: Option<String>,
    #[arg(long)]
    allow_unsupported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeCheckCommand {
    pub diagnostics: Vec<String>,
}

pub(crate) fn compose_command(
    parsed: ComposeRootCli,
) -> Result<ComposeCheckCommand, PloyzctlCliError> {
    match parsed.command {
        ComposeSubcommand::Check(check) => compose_check_command(check),
    }
}

fn compose_check_command(parsed: ComposeCheckCli) -> Result<ComposeCheckCommand, PloyzctlCliError> {
    let namespace_override = parsed
        .namespace
        .map(NamespaceId::try_new)
        .transpose()
        .map_err(|error| invalid_value("--namespace", error))?;
    let (_parsed, warnings) = parse_compose_file(
        &parsed.file,
        namespace_override,
        if parsed.allow_unsupported {
            UnsupportedFieldMode::AllowUnsupported
        } else {
            UnsupportedFieldMode::Strict
        },
    )?;
    Ok(ComposeCheckCommand {
        diagnostics: warnings.into_iter().map(|warning| warning.0).collect(),
    })
}
