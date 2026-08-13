//! `ployz init` drivers and presentation.

use std::io::IsTerminal as _;
use std::path::PathBuf;

use crate::commands::{InitCommand, InitDriver};
use crate::init::orchestration::FoundingFailure;
use crate::mesh::context::OperatorContextStore;

pub mod cloud;
pub mod http;
pub mod interactive;
pub mod on_host;
mod orchestration;
pub mod presentation;
pub mod ssh;

pub async fn execute(mut command: InitCommand) -> Result<(), InitExecutionError> {
    let terminal = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    interactive::resolve_answers(&mut command, terminal, &mut interactive::TerminalPrompt)?;
    match command.driver.clone() {
        InitDriver::SshTarget(target) => {
            ssh::resolve_command_endpoint(&mut command).await?;
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or(InitExecutionError::MissingHome)?;
            let peer_name = std::env::var("USER")
                .ok()
                .filter(|value| !value.is_empty())
                .map_or_else(
                    || "operator-laptop".to_owned(),
                    |user| format!("{user}-laptop"),
                );
            let key = ssh::SshPeerKey::load_or_create(
                &ssh::default_config_home(&home),
                &target,
                peer_name,
            )?;
            let handoff = key.run(&target, &command)?;
            OperatorContextStore::new(ssh::default_config_home(&home))
                .persist(&target, handoff, &key)?;
            Ok(())
        }
        InitDriver::OnHost | InitDriver::Cloud(_) | InitDriver::SshPeer(_) => {
            let emit_ssh_handoff = matches!(&command.driver, InitDriver::SshPeer(_));
            match on_host::execute(command).await {
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
                    if emit_ssh_handoff {
                        print!("{}", presentation::ssh_context_handoff(&success)?);
                    }
                    Ok(())
                }
                Err(on_host::OnHostInitError::Founding(FoundingFailure::Refused(refusal))) => Err(
                    InitExecutionError::Refused(presentation::refusal_summary(&refusal)),
                ),
                Err(on_host::OnHostInitError::Refused(refusal)) => Err(
                    InitExecutionError::Refused(presentation::refusal_summary(&refusal)),
                ),
                Err(error) => Err(InitExecutionError::OnHost(error)),
            }
        }
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
    #[error(transparent)]
    Context(#[from] crate::mesh::context::OperatorContextError),
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    OnHost(#[from] on_host::OnHostInitError),
}
