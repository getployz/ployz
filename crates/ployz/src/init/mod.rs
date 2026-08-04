//! `ployz init` drivers and presentation.

use std::io::IsTerminal as _;
use std::path::PathBuf;

use ployz_host_runner::lifecycle::founding::FoundingFailure;

use crate::commands::{InitCommand, InitTarget};

pub mod cloud;
pub mod http;
pub mod interactive;
pub mod on_host;
pub mod presentation;
pub mod ssh;

pub async fn execute(mut command: InitCommand) -> Result<(), InitExecutionError> {
    let terminal = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    interactive::resolve_answers(&mut command, terminal, &mut interactive::TerminalPrompt)?;
    match command.target.clone() {
        InitTarget::Ssh(target) => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or(InitExecutionError::MissingHome)?;
            let peer_name = std::env::var("USER")
                .ok()
                .filter(|value| !value.is_empty())
                .map_or_else(
                    || "operator laptop".to_owned(),
                    |user| format!("{user} laptop"),
                );
            let key = ssh::SshPeerKey::load_or_create(
                &ssh::default_config_home(&home),
                &target,
                peer_name,
            )?;
            key.run(&target, &command)?;
            Ok(())
        }
        InitTarget::OnHost => match on_host::execute(command).await {
            Ok(success) => {
                print!(
                    "{}",
                    presentation::success_summary(
                        success.result,
                        &success.cluster_name,
                        &success.machine_name,
                        &success.storage,
                    )
                );
                Ok(())
            }
            Err(on_host::OnHostInitError::Founding(FoundingFailure::Refused(refusal))) => Err(
                InitExecutionError::Refused(presentation::refusal_summary(&refusal)),
            ),
            Err(on_host::OnHostInitError::Refused(refusal)) => Err(InitExecutionError::Refused(
                presentation::refusal_summary(&refusal),
            )),
            Err(error) => Err(InitExecutionError::OnHost(error)),
        },
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InitExecutionError {
    #[error(transparent)]
    Prompt(#[from] interactive::PromptError),
    #[error("HOME is not set; cannot persist the SSH driver peer")]
    MissingHome,
    #[error(transparent)]
    Ssh(#[from] ssh::SshInitError),
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    OnHost(#[from] on_host::OnHostInitError),
}
