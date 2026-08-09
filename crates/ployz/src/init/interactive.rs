//! TTY-only bootstrap questions; non-TTY callers keep the same safe defaults.

use std::io::{self, BufRead as _, Write as _};

use ployz_core::corrosion::StorageMode;
use ployz_core::founding::{InitStorageChoice, PLAIN_STORAGE_FORFEIT};
use ployz_core::network::MachineEndpointSupernet;

use crate::commands::{InitCommand, parse_service_urls, parse_storage};

pub trait Prompt {
    fn ask(&mut self, question: &str) -> Result<String, PromptError>;
    fn note(&mut self, message: &str) -> Result<(), PromptError>;
}

pub struct TerminalPrompt;

impl Prompt for TerminalPrompt {
    fn ask(&mut self, question: &str) -> Result<String, PromptError> {
        eprint!("{question}");
        io::stderr().flush().map_err(PromptError::Io)?;
        let mut answer = String::new();
        io::stdin()
            .lock()
            .read_line(&mut answer)
            .map_err(PromptError::Io)?;
        Ok(answer.trim().to_owned())
    }

    fn note(&mut self, message: &str) -> Result<(), PromptError> {
        eprintln!("{message}");
        Ok(())
    }
}

pub fn resolve_answers(
    command: &mut InitCommand,
    is_terminal: bool,
    prompt: &mut impl Prompt,
) -> Result<(), PromptError> {
    if !is_terminal {
        return Ok(());
    }
    if command.prompt.storage {
        let answer = prompt.ask(
            "? volume storage [auto — zfs when an imported pool and ≥2 GiB RAM are present]: ",
        )?;
        command.storage = parse_storage(if answer.is_empty() { "auto" } else { &answer })
            .map_err(PromptError::InvalidAnswer)?;
    }
    if command.prompt.container_network {
        let answer = prompt.ask("? container network [10.210.0.0/16]: ")?;
        if !answer.is_empty() {
            command.container_network = MachineEndpointSupernet::try_new(answer)
                .map_err(|error| PromptError::InvalidAnswer(error.to_string()))?;
        }
    }
    if command.prompt.service_urls {
        let answer = prompt.ask("? service URLs [disabled] (custom:<suffix> | disabled): ")?;
        command.service_urls = parse_service_urls(if answer.is_empty() {
            "disabled"
        } else {
            &answer
        })
        .map_err(PromptError::InvalidAnswer)?;
    }
    if command.storage
        == (InitStorageChoice::Flag {
            mode: StorageMode::Plain,
        })
    {
        prompt.note(PLAIN_STORAGE_FORFEIT)?;
    }
    command.prompt.storage = false;
    command.prompt.container_network = false;
    command.prompt.service_urls = false;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum PromptError {
    #[error("could not read bootstrap answer: {0}")]
    Io(std::io::Error),
    #[error("invalid bootstrap answer: {0}")]
    InvalidAnswer(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Command, parse_command};
    use ployz_core::corrosion::AutomaticHostnameMode;

    #[derive(Default)]
    struct Answers {
        values: std::collections::VecDeque<String>,
        notes: Vec<String>,
    }

    impl Prompt for Answers {
        fn ask(&mut self, _question: &str) -> Result<String, PromptError> {
            self.values
                .pop_front()
                .ok_or_else(|| PromptError::InvalidAnswer("missing test answer".to_owned()))
        }

        fn note(&mut self, message: &str) -> Result<(), PromptError> {
            self.notes.push(message.to_owned());
            Ok(())
        }
    }

    fn bare() -> InitCommand {
        let Command::Init(command) = parse_command(["init".to_owned()]).expect("init") else {
            panic!("expected init")
        };
        *command
    }

    #[test]
    fn non_tty_uses_safe_defaults_without_reading_answers() {
        let mut command = bare();
        resolve_answers(&mut command, false, &mut Answers::default()).expect("defaults");
        assert_eq!(command.storage, InitStorageChoice::Automatic);
        assert_eq!(
            command.container_network,
            MachineEndpointSupernet::default_v1()
        );
        assert_eq!(command.service_urls, AutomaticHostnameMode::Disabled);
    }

    #[test]
    fn wizard_answers_equal_the_flag_surface() {
        let mut wizard = bare();
        let mut answers = Answers {
            values: ["plain", "172.30.0.0/16", "custom:apps.example.com"]
                .map(ToOwned::to_owned)
                .into(),
            notes: Vec::new(),
        };
        resolve_answers(&mut wizard, true, &mut answers).expect("wizard");

        let Command::Init(flags) = parse_command(
            [
                "init",
                "--storage",
                "plain",
                "--container-network",
                "172.30.0.0/16",
                "--service-urls",
                "custom:apps.example.com",
            ]
            .map(ToOwned::to_owned),
        )
        .expect("flags") else {
            panic!("expected init")
        };
        assert_eq!(wizard.storage, flags.storage);
        assert_eq!(wizard.container_network, flags.container_network);
        assert_eq!(wizard.service_urls, flags.service_urls);
        assert_eq!(answers.notes, vec![PLAIN_STORAGE_FORFEIT]);
    }
}
