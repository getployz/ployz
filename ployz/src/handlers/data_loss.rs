//! CLI removal acceptance over a fixed, observer-relative deletion list.

use super::{Error, leaf_matches, string_values};
use crate::{connect::Client, context::ConnectionSource};
use clap::ArgMatches;
use ployz_core::{DataLossConfirmation, ObservedDataLoss};
use std::{
    collections::BTreeSet,
    io::{self, IsTerminal, Write},
};

#[derive(Clone, Copy)]
pub(super) enum VolumeEffect {
    Preserve,
    Delete,
    LoseAccess,
}

pub(super) fn confirm_removal(
    root: &ArgMatches,
    client: &Client,
    observed: &ObservedDataLoss,
    operation: &str,
    targets: &[String],
    volume_effect: VolumeEffect,
) -> Result<Option<DataLossConfirmation>, Error> {
    let leaf = leaf_matches(root);
    let context = match client.connection_source() {
        ConnectionSource::Context(name) => name.as_str(),
        ConnectionSource::Direct => "direct connection",
        ConnectionSource::LocalSocket => "local socket",
    };
    println!("{operation}: {}\nContext: {context}", targets.join(" "));
    let retry = retry_args(root, client.connection_source());
    confirm_with(
        observed,
        &string_values(leaf, "accept-volume-loss"),
        targets,
        ConfirmationOptions {
            volume_effect,
            yes: leaf.get_flag("yes"),
            tty: io::stdin().is_terminal() && io::stdout().is_terminal(),
        },
        &retry,
        &mut io::stdout(),
        read_answer,
    )
}

fn retry_args(root: &ArgMatches, source: &ConnectionSource) -> Vec<String> {
    let mut args = vec!["ployz".into()];
    let mut leaf = root;
    while let Some((name, child)) = leaf.subcommand() {
        args.push(name.into());
        leaf = child;
    }
    for id in ["project", "service", "volume-name"] {
        args.extend(string_values(leaf, id));
    }
    // Machine is positional for machine rm, but a selector flag for volume rm.
    for machine in string_values(leaf, "machine") {
        if super::command_path(root).starts_with("volume ") {
            args.push("--machine".into());
        }
        args.push(machine);
    }
    for id in ["volumes", "no-reset", "force"] {
        if leaf.try_get_one::<bool>(id).ok().flatten() == Some(&true) {
            args.push(format!("--{id}"));
        }
    }
    for id in ["connect", "ployz-config", "project-name"] {
        if let Some(value) = leaf.try_get_one::<String>(id).ok().flatten() {
            args.extend([format!("--{id}"), value.clone()]);
        }
    }
    if let ConnectionSource::Context(name) = source {
        args.extend(["--context".into(), name.clone()]);
    }
    args
}

struct ConfirmationOptions {
    volume_effect: VolumeEffect,
    yes: bool,
    tty: bool,
}

fn confirm_with(
    observed: &ObservedDataLoss,
    named: &[String],
    targets: &[String],
    options: ConfirmationOptions,
    retry: &[String],
    output: &mut dyn Write,
    mut read: impl FnMut(&str) -> io::Result<Option<String>>,
) -> Result<Option<DataLossConfirmation>, Error> {
    let ConfirmationOptions {
        volume_effect,
        yes,
        tty,
    } = options;
    writeln!(
        output,
        "Live Observation from one observer; not a globally complete Cluster view."
    )?;
    match volume_effect {
        VolumeEffect::Preserve => writeln!(output, "Volumes will be kept.")?,
        VolumeEffect::Delete if observed.data_loss.is_empty() => {
            writeln!(output, "No volumes to delete.")?
        }
        VolumeEffect::Delete => writeln!(
            output,
            "Permanently delete {} volumes:",
            observed.data_loss.len()
        )?,
        VolumeEffect::LoseAccess => writeln!(
            output,
            "Volumes losing Cluster access: {}. Reset does not erase their data:",
            observed.data_loss.len()
        )?,
    }
    for loss in &observed.data_loss {
        writeln!(output, "  {loss}")?;
    }
    let names = observed
        .data_loss
        .iter()
        .map(|loss| loss.name())
        .collect::<BTreeSet<_>>();
    let supplied = named.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let unknown = supplied.difference(&names).copied().collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(Error::usage(format!(
            "Unknown volume acceptance: {}. Actual affected volumes: {}. No changes made.",
            unknown.join(", "),
            observed
                .data_loss
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let missing = names.difference(&supplied).copied().collect::<Vec<_>>();
    if !missing.is_empty() && (!tty || !named.is_empty()) {
        let mut command = retry.to_vec();
        for name in &names {
            command.extend(["--accept-volume-loss".into(), (*name).into()]);
        }
        return Err(Error::usage(format!(
            "Missing volume acceptance: {}. No changes made.\nRetry: {}",
            missing.join(", "),
            shell_words::join(command)
        )));
    }
    let confirmation = || {
        observed
            .confirm_names(names.iter().copied())
            .map_err(|error| Error::usage(error.to_string()))
    };
    if !named.is_empty() || (names.is_empty() && yes) {
        return confirmation().map(Some);
    }
    if !tty {
        let mut command = retry.to_vec();
        command.push("--yes".into());
        return Err(Error::usage(format!(
            "Confirmation requires a terminal; pass --yes. No changes made.\nRetry: {}",
            shell_words::join(command)
        )));
    }
    if prompt(
        if names.is_empty() { &[] } else { targets },
        volume_effect,
        output,
        &mut read,
    )? {
        confirmation().map(Some)
    } else {
        Ok(None)
    }
}

pub(super) fn confirm_ordinary(root: &ArgMatches, client: &Client) -> Result<bool, Error> {
    if leaf_matches(root).get_flag("yes") {
        return Ok(true);
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        let mut args = retry_args(root, client.connection_source());
        args.push("--yes".into());
        return Err(Error::usage(format!(
            "Confirmation requires a terminal; pass --yes. No changes made.\nRetry: {}",
            shell_words::join(args)
        )));
    }
    prompt(&[], VolumeEffect::Preserve, &mut io::stdout(), read_answer)
}

fn prompt(
    targets: &[String],
    volume_effect: VolumeEffect,
    output: &mut dyn Write,
    mut read: impl FnMut(&str) -> io::Result<Option<String>>,
) -> Result<bool, Error> {
    loop {
        let question = if targets.is_empty() {
            "Remove the listed targets? [y/N] (Enter cancels): ".to_owned()
        } else {
            let consequence = match volume_effect {
                VolumeEffect::Delete => "permanently delete the listed volumes",
                VolumeEffect::LoseAccess => {
                    "lose Cluster access to the listed volumes; their data will not be erased"
                }
                VolumeEffect::Preserve => "keep the listed volumes",
            };
            format!(
                "Type {} (space-separated target names) to remove the targets and {consequence} (Enter cancels): ",
                targets.join(" ")
            )
        };
        output.flush()?;
        let Some(answer) = read(&question)? else {
            break;
        };
        let answer = answer.trim();
        if answer.is_empty() {
            break;
        }
        if targets.is_empty() {
            if matches!(answer, "y" | "Y" | "yes" | "YES") {
                return Ok(true);
            }
            break;
        }
        if answer
            .split_whitespace()
            .eq(targets.iter().map(String::as_str))
        {
            return Ok(true);
        }
        writeln!(
            output,
            "Names did not match. Type exactly: {}. No changes made.",
            targets.join(" ")
        )?;
    }
    writeln!(output, "Cancelled. No changes made.")?;
    Ok(false)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "confirmation ignores non-text terminal keys"
)]
fn read_answer(question: &str) -> io::Result<Option<String>> {
    use crossterm::{
        event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
        execute,
    };
    let _raw = super::operator::RawTerminal::enable().map_err(io::Error::other)?;
    let mut output = io::stdout();
    write!(output, "{question}")?;
    output.flush()?;
    let mut answer = String::new();
    loop {
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                write!(output, "\r\n")?;
                return Ok(None);
            }
            KeyCode::Enter | KeyCode::Char('j')
                if key.code == KeyCode::Enter || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                write!(output, "\r\n")?;
                return Ok(Some(answer));
            }
            KeyCode::Backspace if answer.pop().is_some() => {
                execute!(
                    output,
                    crossterm::cursor::MoveLeft(1),
                    crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine)
                )?;
            }
            KeyCode::Char(ch)
                if !ch.is_control() && !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                answer.push(ch);
                write!(output, "{ch}")?;
            }
            _ => {}
        }
        output.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::{DataLoss, DockerVolumeId, DockerVolumeName, MachineId};

    fn loss(machine: char, name: &str) -> DataLoss {
        DataLoss::DockerVolume {
            id: DockerVolumeId {
                machine_id: MachineId::parse(machine.to_string().repeat(32)).unwrap(),
                name: DockerVolumeName::parse(name).unwrap(),
            },
        }
    }

    #[test]
    fn machine_reset_confirmation_names_access_loss_without_promising_erasure() {
        let observed = ObservedDataLoss {
            data_loss: vec![loss('a', "data")],
        };
        let mut output = Vec::new();
        let confirmation = confirm_with(
            &observed,
            &[],
            &["worker".into()],
            ConfirmationOptions {
                volume_effect: VolumeEffect::LoseAccess,
                yes: false,
                tty: true,
            },
            &[],
            &mut output,
            |question| {
                assert!(question.contains("lose Cluster access"), "{question}");
                assert!(question.contains("data will not be erased"), "{question}");
                assert!(!question.contains("delete"), "{question}");
                Ok(Some("worker".into()))
            },
        )
        .unwrap()
        .unwrap();
        assert!(observed.require(&confirmation).is_ok());
        let output = String::from_utf8(output).unwrap();
        assert!(
            output.contains("Reset does not erase their data"),
            "{output}"
        );
        assert!(!output.contains("Permanently delete"), "{output}");
    }

    #[test]
    fn unknown_names_refuse_even_when_no_volumes_exist() {
        for observed in [
            ObservedDataLoss { data_loss: vec![] },
            ObservedDataLoss {
                data_loss: vec![loss('a', "data")],
            },
        ] {
            let error = confirm_with(
                &observed,
                &["gone".into()],
                &["app".into()],
                ConfirmationOptions {
                    volume_effect: VolumeEffect::Delete,
                    yes: true,
                    tty: true,
                },
                &[],
                &mut Vec::new(),
                |_| panic!("must not prompt"),
            )
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("Unknown volume acceptance: gone")
            );
        }
    }

    #[test]
    fn non_tty_requires_all_names_even_with_yes_and_never_reads_piped_input() {
        let observed = ObservedDataLoss {
            data_loss: vec![loss('a', "data"), loss('b', "data"), loss('a', "logs")],
        };
        for named in [vec![], vec!["data".into()]] {
            for yes in [false, true] {
                let error = confirm_with(
                    &observed,
                    &named,
                    &["app".into()],
                    ConfirmationOptions {
                        volume_effect: VolumeEffect::Delete,
                        yes,
                        tty: false,
                    },
                    &[
                        "ployz".into(),
                        "project".into(),
                        "rm".into(),
                        "app".into(),
                        "--volumes".into(),
                        "--context".into(),
                        "prod west".into(),
                    ],
                    &mut Vec::new(),
                    |_| panic!("must not read stdin"),
                )
                .unwrap_err()
                .to_string();
                assert!(
                    error.contains("--accept-volume-loss data --accept-volume-loss logs"),
                    "{error}"
                );
                let command = error.split("Retry: ").nth(1).unwrap();
                assert!(
                    shell_words::split(command)
                        .unwrap()
                        .contains(&"prod west".into())
                );
            }
        }
        for tty in [false, true] {
            let confirmed = confirm_with(
                &observed,
                &["data".into(), "logs".into(), "data".into()],
                &["app".into()],
                ConfirmationOptions {
                    volume_effect: VolumeEffect::Delete,
                    yes: false,
                    tty,
                },
                &[],
                &mut Vec::new(),
                |_| panic!("must not prompt"),
            )
            .unwrap()
            .unwrap();
            assert!(observed.require(&confirmed).is_ok());
            let changed = ObservedDataLoss {
                data_loss: vec![loss('c', "data")],
            };
            let error = crate::failure::refusal_from_rpc(
                changed.require(&confirmed).unwrap_err().into_rpc_error(),
            )
            .to_string();
            assert!(error.contains(&"c".repeat(32)) && error.contains("Rerun"));
        }
    }

    #[test]
    fn tty_retries_target_names_and_cancels_without_acceptance() {
        let observed = ObservedDataLoss {
            data_loss: vec![loss('a', "data")],
        };
        let targets = vec!["app/db".into(), "app/api".into()];
        let mut input = [Some("db".into()), Some("app/db app/api".into())].into_iter();
        let mut output = Vec::new();
        let confirmed = confirm_with(
            &observed,
            &[],
            &targets,
            ConfirmationOptions {
                volume_effect: VolumeEffect::Delete,
                yes: true,
                tty: true,
            },
            &[],
            &mut output,
            |question| {
                assert!(question.contains("space-separated"));
                assert!(!question.contains("[y/N]"));
                Ok(input.next().unwrap())
            },
        )
        .unwrap()
        .unwrap();
        assert!(observed.require(&confirmed).is_ok());
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Names did not match"));
        assert!(!output.contains("[y/N]"));
        for answer in [None, Some(String::new())] {
            let mut output = Vec::new();
            assert!(
                confirm_with(
                    &observed,
                    &[],
                    &targets,
                    ConfirmationOptions {
                        volume_effect: VolumeEffect::Delete,
                        yes: false,
                        tty: true
                    },
                    &[],
                    &mut output,
                    |_| Ok(answer.clone())
                )
                .unwrap()
                .is_none()
            );
            assert!(
                String::from_utf8(output)
                    .unwrap()
                    .contains("Cancelled. No changes made.")
            );
        }
        assert!(
            confirm_with(
                &observed,
                &["wrong".into()],
                &targets,
                ConfirmationOptions {
                    volume_effect: VolumeEffect::Delete,
                    yes: false,
                    tty: true
                },
                &[],
                &mut Vec::new(),
                |_| panic!("invalid flags never fall back to prompt")
            )
            .is_err()
        );
    }

    #[test]
    fn no_volume_and_preserved_volume_removal_need_ordinary_confirmation() {
        let observed = ObservedDataLoss { data_loss: vec![] };
        for destroy in [false, true] {
            let mut output = Vec::new();
            assert!(
                confirm_with(
                    &observed,
                    &[],
                    &["app".into()],
                    ConfirmationOptions {
                        volume_effect: if destroy {
                            VolumeEffect::Delete
                        } else {
                            VolumeEffect::Preserve
                        },
                        yes: false,
                        tty: false
                    },
                    &[],
                    &mut output,
                    |_| panic!("no tty")
                )
                .is_err()
            );
            assert!(String::from_utf8(output).unwrap().contains(if destroy {
                "No volumes to delete"
            } else {
                "Volumes will be kept"
            }));
            assert!(
                confirm_with(
                    &observed,
                    &[],
                    &["app".into()],
                    ConfirmationOptions {
                        volume_effect: if destroy {
                            VolumeEffect::Delete
                        } else {
                            VolumeEffect::Preserve
                        },
                        yes: true,
                        tty: false
                    },
                    &[],
                    &mut Vec::new(),
                    |_| panic!("yes")
                )
                .unwrap()
                .is_some()
            );
            assert!(
                confirm_with(
                    &observed,
                    &[],
                    &["app".into()],
                    ConfirmationOptions {
                        volume_effect: if destroy {
                            VolumeEffect::Delete
                        } else {
                            VolumeEffect::Preserve
                        },
                        yes: false,
                        tty: true
                    },
                    &[],
                    &mut Vec::new(),
                    |_| Ok(Some("y".into()))
                )
                .unwrap()
                .is_some()
            );
        }
    }

    #[test]
    fn retry_preserves_effective_selection_and_quotes_arguments() {
        let root = crate::cli::command()
            .try_get_matches_from([
                "ployz",
                "service",
                "rm",
                "db",
                "api",
                "--project-name",
                "app",
                "--volumes",
                "--connect",
                "unix:///tmp/socket name",
                "--ployz-config",
                "/tmp/config's file",
            ])
            .unwrap();
        let args = retry_args(&root, &ConnectionSource::Direct);
        let command = shell_words::join(&args);
        assert_eq!(shell_words::split(&command).unwrap(), args);
        let parsed = crate::cli::command().try_get_matches_from(args).unwrap();
        let leaf = leaf_matches(&parsed);
        assert_eq!(string_values(leaf, "service"), ["db", "api"]);
        assert_eq!(leaf.get_one::<String>("project-name").unwrap(), "app");
        assert_eq!(
            leaf.get_one::<String>("connect").unwrap(),
            "unix:///tmp/socket name"
        );
    }
}
