//! CLI removal acceptance over a fixed, observer-relative deletion list.

use super::{Error, leaf_matches, string_values};
use crate::{connect::Client, context::ConnectionSource};
use clap::ArgMatches;
use ployz_core::{DataLoss, DataLossConfirmation, ObservedDataLoss};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
};

use crate::output::{self, say};
use crate::ui::Hint;

#[derive(Clone, Copy)]
pub(super) enum VolumeEffect {
    Preserve,
    LoseAccess,
}

/// The Volume name each Docker Volume keeps a Volume's data for, by Docker name.
pub(super) type VolumeLabels = BTreeMap<String, String>;

/// What a person names `loss` by: its Volume's name, else its Docker name.
pub(super) fn volume_label<'a>(labels: &'a VolumeLabels, loss: &'a DataLoss) -> &'a str {
    labels.get(loss.name()).map_or(loss.name(), String::as_str)
}

pub(super) fn confirm_removal(
    root: &ArgMatches,
    client: &Client,
    observed: &ObservedDataLoss,
    operation: &str,
    targets: &[String],
    volume_effect: VolumeEffect,
    labels: &VolumeLabels,
) -> Result<Option<DataLossConfirmation>, Error> {
    let leaf = leaf_matches(root);
    let context = match client.connection_source() {
        ConnectionSource::Context(name) => name.as_str(),
        ConnectionSource::Direct => "direct connection",
        ConnectionSource::LocalSocket => "local socket",
        ConnectionSource::Cloud => "Cloud",
    };
    say!("{operation}: {}\nContext: {context}", targets.join(" "));
    let retry = retry_args(root, client.connection_source());
    confirm_with(
        observed,
        &string_values(leaf, "accept-volume-loss"),
        targets,
        ConfirmationOptions {
            volume_effect,
            labels,
            // A typed `--confirm` stands in for `--yes`.
            yes: leaf.try_get_one::<bool>("yes").ok().flatten() == Some(&true)
                || leaf
                    .try_get_one::<String>("confirm")
                    .ok()
                    .flatten()
                    .is_some(),
            tty: output::interactive(),
        },
        &retry,
        &mut output::human(),
        read_answer,
    )
}

pub(super) fn retry_args(root: &ArgMatches, source: &ConnectionSource) -> Vec<String> {
    let mut args = vec!["ployz".into()];
    let mut leaf = root;
    while let Some((name, child)) = leaf.subcommand() {
        args.push(name.into());
        leaf = child;
    }
    args.extend(string_values(leaf, "server"));
    if leaf.try_get_one::<bool>("no-reset").ok().flatten() == Some(&true) {
        args.push("--no-reset".into());
    }
    for id in ["connect", "ployz-config", "confirm"] {
        if let Some(value) = leaf.try_get_one::<String>(id).ok().flatten() {
            args.extend([format!("--{id}"), value.clone()]);
        }
    }
    if let ConnectionSource::Context(name) = source {
        args.extend(["--context".into(), name.clone()]);
    }
    args
}

struct ConfirmationOptions<'a> {
    volume_effect: VolumeEffect,
    labels: &'a VolumeLabels,
    yes: bool,
    tty: bool,
}

fn confirm_with(
    observed: &ObservedDataLoss,
    named: &[String],
    targets: &[String],
    options: ConfirmationOptions<'_>,
    retry: &[String],
    output: &mut dyn Write,
    mut read: impl FnMut(&str) -> io::Result<Option<String>>,
) -> Result<Option<DataLossConfirmation>, Error> {
    let ConfirmationOptions {
        volume_effect,
        labels,
        yes,
        tty,
    } = options;
    let label = |loss| volume_label(labels, loss);
    writeln!(
        output,
        "Based on what the connected machine can see; other machines may have additional resources."
    )?;
    match volume_effect {
        VolumeEffect::Preserve => writeln!(output, "Volumes will be kept.")?,
        VolumeEffect::LoseAccess => writeln!(
            output,
            "Volumes the Cluster loses ({}); only the Server's disk keeps their data:",
            observed.data_loss.len()
        )?,
    }
    for loss in &observed.data_loss {
        let DataLoss::DockerVolume { id } = loss;
        writeln!(output, "  {} (machine ID: {})", label(loss), id.machine_id)?;
    }
    let names = observed
        .data_loss
        .iter()
        .map(label)
        .collect::<BTreeSet<_>>();
    let supplied = named.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let unknown = supplied.difference(&names).copied().collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(Error::usage(format!(
            "Unknown volume acceptance: {}. Actual affected volumes: {}. No changes made.",
            unknown.join(", "),
            names.iter().copied().collect::<Vec<_>>().join(", ")
        )));
    }
    let missing = names.difference(&supplied).copied().collect::<Vec<_>>();
    if !missing.is_empty() && (!tty || !named.is_empty()) {
        let mut command = retry.to_vec();
        for name in &names {
            command.extend(["--accept-volume-loss".into(), (*name).into()]);
        }
        return Err(Error::usage(format!(
            "Missing volume acceptance: {}. No changes made.",
            missing.join(", ")
        ))
        .hint(Hint::Retry(shell_words::join(command))));
    }
    // Past the checks above every observed loss is accepted, by whichever name.
    let confirmation = || {
        observed
            .confirm_names(observed.data_loss.iter().map(DataLoss::name))
            .map_err(|error| Error::usage(error.to_string()))
    };
    if !named.is_empty() || (names.is_empty() && yes) {
        return confirmation().map(Some);
    }
    if !tty {
        let mut command = retry.to_vec();
        command.push("--yes".into());
        return Err(
            Error::usage("Confirmation requires a terminal; pass --yes. No changes made.")
                .hint(Hint::Retry(shell_words::join(command))),
        );
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

fn prompt(
    targets: &[String],
    volume_effect: VolumeEffect,
    output: &mut dyn Write,
    mut read: impl FnMut(&str) -> io::Result<Option<String>>,
) -> Result<bool, Error> {
    if !targets.is_empty() {
        let consequence = match volume_effect {
            VolumeEffect::LoseAccess => {
                "lose the listed volumes: only the Server's disk keeps their data"
            }
            VolumeEffect::Preserve => "keep the listed volumes",
        };
        writeln!(
            output,
            "\nThis will remove {} and {consequence}.",
            targets.join(", ")
        )?;
        writeln!(output, "Press Enter without typing to cancel.")?;
    }
    loop {
        let question = if targets.is_empty() {
            "Continue with removal? Type yes to confirm, or press Enter to cancel: ".to_owned()
        } else {
            format!("Type \"{}\" to confirm: ", targets.join(" "))
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
    fn machine_reset_confirmation_names_the_loss_without_promising_the_data_is_safe() {
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
                labels: &VolumeLabels::new(),
                yes: false,
                tty: true,
            },
            &[],
            &mut output,
            |question| {
                assert_eq!(question, "Type \"worker\" to confirm: ");
                Ok(Some("worker".into()))
            },
        )
        .unwrap()
        .unwrap();
        assert!(observed.require(&confirmation).is_ok());
        let output = String::from_utf8(output).unwrap();
        assert!(
            output.contains("lose the listed volumes: only the Server's disk keeps their data"),
            "{output}"
        );
        assert!(!output.contains("not be erased"), "{output}");
        assert!(!output.contains("Permanently delete"), "{output}");
    }

    #[test]
    fn volumes_are_listed_and_accepted_by_their_volume_name() {
        let observed = ObservedDataLoss {
            data_loss: vec![loss('a', "shop-production_vol-1"), loss('a', "stray")],
        };
        let labels = VolumeLabels::from([("shop-production_vol-1".into(), "pgdata".into())]);
        let options = |tty| ConfirmationOptions {
            volume_effect: VolumeEffect::LoseAccess,
            labels: &labels,
            yes: false,
            tty,
        };
        let mut output = Vec::new();
        let missing = confirm_with(
            &observed,
            &[],
            &["worker".into()],
            options(false),
            &["ployz".into()],
            &mut output,
            |_| panic!("no tty"),
        )
        .unwrap_err();
        let hints = missing.hints();
        let [Hint::Retry(missing)] = hints.as_slice() else {
            panic!("{hints:?}");
        };
        assert!(
            missing.contains("--accept-volume-loss pgdata --accept-volume-loss stray"),
            "{missing}"
        );
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("  pgdata (machine ID:")
        );
        let raw = confirm_with(
            &observed,
            &["shop-production_vol-1".into(), "stray".into()],
            &["worker".into()],
            options(false),
            &[],
            &mut Vec::new(),
            |_| panic!("no tty"),
        )
        .unwrap_err()
        .to_string();
        assert!(
            raw.contains("Unknown volume acceptance: shop-production_vol-1"),
            "{raw}"
        );
        let confirmed = confirm_with(
            &observed,
            &["pgdata".into(), "stray".into()],
            &["worker".into()],
            options(false),
            &[],
            &mut Vec::new(),
            |_| panic!("no tty"),
        )
        .unwrap()
        .unwrap();
        assert!(observed.require(&confirmed).is_ok());
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
                    volume_effect: VolumeEffect::LoseAccess,
                    labels: &VolumeLabels::new(),
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
                        volume_effect: VolumeEffect::LoseAccess,
                        labels: &VolumeLabels::new(),
                        yes,
                        tty: false,
                    },
                    &[
                        "ployz".into(),
                        "server".into(),
                        "rm".into(),
                        "app".into(),
                        "--context".into(),
                        "prod west".into(),
                    ],
                    &mut Vec::new(),
                    |_| panic!("must not read stdin"),
                )
                .unwrap_err();
                let hints = error.hints();
                let [Hint::Retry(command)] = hints.as_slice() else {
                    panic!("{hints:?}");
                };
                assert!(
                    command.contains("--accept-volume-loss data --accept-volume-loss logs"),
                    "{command}"
                );
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
                    volume_effect: VolumeEffect::LoseAccess,
                    labels: &VolumeLabels::new(),
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
                volume_effect: VolumeEffect::LoseAccess,
                labels: &VolumeLabels::new(),
                yes: true,
                tty: true,
            },
            &[],
            &mut output,
            |question| {
                assert_eq!(question, "Type \"app/db app/api\" to confirm: ");
                assert!(!question.contains("[y/N]"));
                Ok(input.next().unwrap())
            },
        )
        .unwrap()
        .unwrap();
        assert!(observed.require(&confirmed).is_ok());
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("This will remove app/db, app/api and lose the listed volumes: only the Server's disk keeps their data."));
        assert!(output.contains("Press Enter without typing to cancel."));
        assert!(output.contains("Volumes the Cluster loses (1);"));
        assert!(output.contains("(machine ID: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)"));
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
                        volume_effect: VolumeEffect::LoseAccess,
                        labels: &VolumeLabels::new(),
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
                    volume_effect: VolumeEffect::LoseAccess,
                    labels: &VolumeLabels::new(),
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
        for (effect, shown) in [
            (VolumeEffect::Preserve, "Volumes will be kept"),
            (VolumeEffect::LoseAccess, "Volumes the Cluster loses (0)"),
        ] {
            let mut output = Vec::new();
            assert!(
                confirm_with(
                    &observed,
                    &[],
                    &["app".into()],
                    ConfirmationOptions {
                        volume_effect: effect,
                        labels: &VolumeLabels::new(),
                        yes: false,
                        tty: false
                    },
                    &[],
                    &mut output,
                    |_| panic!("no tty")
                )
                .is_err()
            );
            assert!(String::from_utf8(output).unwrap().contains(shown));
            assert!(
                confirm_with(
                    &observed,
                    &[],
                    &["app".into()],
                    ConfirmationOptions {
                        volume_effect: effect,
                        labels: &VolumeLabels::new(),
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
                        volume_effect: effect,
                        labels: &VolumeLabels::new(),
                        yes: false,
                        tty: true
                    },
                    &[],
                    &mut Vec::new(),
                    |question| {
                        assert_eq!(
                            question,
                            "Continue with removal? Type yes to confirm, or press Enter to cancel: "
                        );
                        Ok(Some("y".into()))
                    }
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
                "server",
                "rm",
                "edge",
                "--no-reset",
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
        assert_eq!(string_values(leaf, "server"), ["edge"]);
        assert!(leaf.get_flag("no-reset"));
        assert_eq!(
            leaf.get_one::<String>("connect").unwrap(),
            "unix:///tmp/socket name"
        );
    }
}
