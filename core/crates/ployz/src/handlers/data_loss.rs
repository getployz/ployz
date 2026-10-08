//! CLI removal acceptance over a fixed, observer-relative deletion list.

use super::{Error, leaf_matches, string_values};
use crate::{connect::Client, context::ConnectionSource};
use clap::ArgMatches;
use ployz_core::{DataLoss, DataLossConfirmation, ObservedDataLoss};
use std::collections::{BTreeMap, BTreeSet};

use crate::ui::{self, Hint, Tree};

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

/// Accept removing Server `server` and the Volumes it takes. `--confirm` with
/// every lost Volume named by `--accept-volume-loss` accepts outright; without
/// `--confirm` a terminal shows the loss and asks for the name, which accepts
/// them all. Anywhere else, `refusal` gets the full retry.
pub(super) fn confirm_removal(
    root: &ArgMatches,
    client: &Client,
    observed: &ObservedDataLoss,
    server: &str,
    volume_effect: VolumeEffect,
    labels: &VolumeLabels,
    refusal: impl FnOnce(String) -> Error,
) -> Result<(DataLossConfirmation, Vec<String>), Error> {
    let leaf = leaf_matches(root);
    let request = Request {
        observed,
        labels,
        server,
        named: &string_values(leaf, "accept-volume-loss"),
        typed: leaf.get_one::<String>("confirm").map(String::as_str),
        retry: &retry_args(root, client.connection_source()),
    };
    if !request.check()? {
        let retry = request.retry();
        if ui::can_prompt() {
            ui::note_inline(loss(
                observed,
                labels,
                server,
                client.connection_source(),
                volume_effect,
            ));
            ui::note("Based on what the connected Server can see; other Servers may hold more.");
        }
        ui::confirm_name(server, || refusal(retry), "Cancelled. Nothing was removed.")?;
    }
    Ok((request.accept()?, request.accepting()))
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
    if let Some(connect) = leaf.try_get_one::<String>("connect").ok().flatten() {
        args.extend(["--connect".into(), connect.clone()]);
    }
    args.extend(
        super::config_flag(leaf)
            .into_iter()
            .flatten()
            .map(String::from),
    );
    if let ConnectionSource::Context(name) = source {
        args.extend(["--context".into(), name.clone()]);
    }
    args
}

/// What removing `server` takes, as the tree shown before asking.
fn loss(
    observed: &ObservedDataLoss,
    labels: &VolumeLabels,
    server: &str,
    source: &ConnectionSource,
    volume_effect: VolumeEffect,
) -> Tree {
    let context = match source {
        ConnectionSource::Context(name) => format!("context {name}"),
        ConnectionSource::Direct => "a direct connection".into(),
        ConnectionSource::LocalSocket => "the local socket".into(),
        ConnectionSource::Cloud => "Cloud".into(),
    };
    let volumes = match volume_effect {
        VolumeEffect::Preserve => Tree::leaf("Volumes are kept."),
        VolumeEffect::LoseAccess if observed.data_loss.is_empty() => {
            Tree::leaf("No Volumes are lost.")
        }
        VolumeEffect::LoseAccess => Tree::new(
            "Volumes the Cluster loses; only the Server's disk keeps their data:",
            observed
                .data_loss
                .iter()
                .map(|loss| {
                    let DataLoss::DockerVolume { id } = loss;
                    Tree::leaf(format!(
                        "{} (machine ID: {})",
                        volume_label(labels, loss),
                        id.machine_id
                    ))
                })
                .collect(),
        ),
    };
    Tree::new(
        format!("Removing Server {server} through {context}:"),
        vec![volumes],
    )
}

/// What the flags of one `server rm` say.
struct Request<'a> {
    observed: &'a ObservedDataLoss,
    labels: &'a VolumeLabels,
    server: &'a str,
    named: &'a [String],
    typed: Option<&'a str>,
    retry: &'a [String],
}

impl Request<'_> {
    /// The lost Volumes, by the name a person gives them.
    fn names(&self) -> BTreeSet<&str> {
        self.observed
            .data_loss
            .iter()
            .map(|loss| volume_label(self.labels, loss))
            .collect()
    }

    /// The command that accepts everything observed now.
    fn accepting(&self) -> Vec<String> {
        let mut command = self.retry.to_vec();
        command.extend(["--confirm".into(), self.server.to_owned()]);
        for name in self.names() {
            command.extend(["--accept-volume-loss".into(), name.to_owned()]);
        }
        command
    }

    fn retry(&self) -> String {
        shell_words::join(self.accepting())
    }

    /// Whether the flags accept on their own; `false` means the name must be
    /// typed. Flags that contradict what is observed refuse.
    fn check(&self) -> Result<bool, Error> {
        let names = self.names();
        let supplied = self
            .named
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let unknown = supplied.difference(&names).copied().collect::<Vec<_>>();
        if !unknown.is_empty() {
            return Err(Error::usage(format!(
                "Unknown volume acceptance: {}. Actual affected volumes: {}. No changes made.",
                unknown.join(", "),
                names.iter().copied().collect::<Vec<_>>().join(", ")
            )));
        }
        if let Some(typed) = self.typed
            && typed != self.server
        {
            return Err(Error::usage(format!(
                "--confirm {} does not match Server {}. No changes made.",
                typed.escape_debug(),
                self.server
            ))
            .hint(Hint::Retry(self.retry())));
        }
        let missing = names.difference(&supplied).copied().collect::<Vec<_>>();
        if !missing.is_empty() && (self.typed.is_some() || !supplied.is_empty()) {
            return Err(Error::usage(format!(
                "Missing volume acceptance: {}. No changes made.",
                missing.join(", ")
            ))
            .hint(Hint::Retry(self.retry())));
        }
        Ok(self.typed.is_some())
    }

    /// Every observed loss, accepted by whichever name it was given.
    fn accept(&self) -> Result<DataLossConfirmation, Error> {
        self.observed
            .confirm_names(self.observed.data_loss.iter().map(DataLoss::name))
            .map_err(Error::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::{DockerVolumeId, DockerVolumeName, MachineId};

    fn loss(machine: char, name: &str) -> DataLoss {
        DataLoss::DockerVolume {
            id: DockerVolumeId {
                machine_id: MachineId::parse(machine.to_string().repeat(32)).unwrap(),
                name: DockerVolumeName::parse(name).unwrap(),
            },
        }
    }

    fn request<'a>(
        observed: &'a ObservedDataLoss,
        labels: &'a VolumeLabels,
        named: &'a [String],
        typed: Option<&'a str>,
    ) -> Request<'a> {
        Request {
            observed,
            labels,
            server: "worker",
            named,
            typed,
            retry: &[],
        }
    }

    fn retry_of(error: &Error) -> String {
        let hints = error.hints();
        let [Hint::Retry(retry)] = hints.as_slice() else {
            panic!("{hints:?}");
        };
        retry.clone()
    }

    #[test]
    fn the_loss_names_volumes_without_promising_their_data_is_safe() {
        let observed = ObservedDataLoss {
            data_loss: vec![loss('a', "shop-production_vol-1")],
        };
        let labels = VolumeLabels::from([("shop-production_vol-1".into(), "pgdata".into())]);
        let lost = loss_text(&observed, &labels, VolumeEffect::LoseAccess);
        assert!(
            lost.contains("Removing Server worker through context prod:"),
            "{lost}"
        );
        assert!(
            lost.contains("only the Server's disk keeps their data"),
            "{lost}"
        );
        assert!(
            lost.contains(&format!("pgdata (machine ID: {})", "a".repeat(32))),
            "{lost}"
        );
        assert!(!lost.contains("not be erased"), "{lost}");
        let kept = loss_text(&observed, &labels, VolumeEffect::Preserve);
        assert!(kept.contains("Volumes are kept."), "{kept}");
        assert!(!kept.contains("pgdata"), "{kept}");
    }

    fn loss_text(
        observed: &ObservedDataLoss,
        labels: &VolumeLabels,
        effect: VolumeEffect,
    ) -> String {
        let tree = super::loss(
            observed,
            labels,
            "worker",
            &ConnectionSource::Context("prod".into()),
            effect,
        );
        anstream::adapter::strip_str(&tree.to_string()).to_string()
    }

    #[test]
    fn volumes_are_accepted_by_their_volume_name() {
        let observed = ObservedDataLoss {
            data_loss: vec![loss('a', "shop-production_vol-1"), loss('a', "stray")],
        };
        let labels = VolumeLabels::from([("shop-production_vol-1".into(), "pgdata".into())]);
        let raw = ["shop-production_vol-1".into(), "stray".into()];
        let error = request(&observed, &labels, &raw, Some("worker"))
            .check()
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("Unknown volume acceptance: shop-production_vol-1"),
            "{error}"
        );
        let named = ["pgdata".into(), "stray".into()];
        let accepted = request(&observed, &labels, &named, Some("worker"));
        assert!(accepted.check().unwrap());
        assert!(observed.require(&accepted.accept().unwrap()).is_ok());
    }

    #[test]
    fn a_typed_name_needs_every_volume_named() {
        let observed = ObservedDataLoss {
            data_loss: vec![loss('a', "data"), loss('b', "data"), loss('a', "logs")],
        };
        let labels = VolumeLabels::new();
        let retry = [
            "ployz".into(),
            "server".into(),
            "rm".into(),
            "worker".into(),
            "--context".into(),
            "prod west".into(),
        ];
        for named in [vec![], vec!["data".into()]] {
            let error = Request {
                retry: &retry,
                ..request(&observed, &labels, &named, Some("worker"))
            }
            .check()
            .unwrap_err();
            assert!(
                error.to_string().contains("Missing volume acceptance"),
                "{error}"
            );
            let retry = retry_of(&error);
            assert!(
                retry.contains(
                    "--confirm worker --accept-volume-loss data --accept-volume-loss logs"
                ),
                "{retry}"
            );
            assert!(
                shell_words::split(&retry)
                    .unwrap()
                    .contains(&"prod west".into())
            );
        }
        let named = ["data".into(), "logs".into(), "data".into()];
        let confirmed = request(&observed, &labels, &named, Some("worker"));
        assert!(confirmed.check().unwrap());
        let confirmed = confirmed.accept().unwrap();
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

    #[test]
    fn without_confirm_the_name_is_asked_for_unless_flags_contradict() {
        let observed = ObservedDataLoss {
            data_loss: vec![loss('a', "data"), loss('a', "logs")],
        };
        let labels = VolumeLabels::new();
        assert!(!request(&observed, &labels, &[], None).check().unwrap());
        let all = ["data".into(), "logs".into()];
        assert!(!request(&observed, &labels, &all, None).check().unwrap());
        let partial = ["data".into()];
        let error = request(&observed, &labels, &partial, None)
            .check()
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Missing volume acceptance: logs"),
            "{error}"
        );
        let error = request(&observed, &labels, &[], Some("wroker"))
            .check()
            .unwrap_err();
        assert!(
            error.to_string().contains("does not match Server worker"),
            "{error}"
        );
        assert!(retry_of(&error).contains("--confirm worker"));
        for observed in [ObservedDataLoss { data_loss: vec![] }, observed] {
            let gone = ["gone".into()];
            let error = request(&observed, &labels, &gone, Some("worker"))
                .check()
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("Unknown volume acceptance: gone"),
                "{error}"
            );
        }
    }

    #[test]
    fn no_volumes_lost_still_needs_the_name() {
        let observed = ObservedDataLoss { data_loss: vec![] };
        let labels = VolumeLabels::new();
        assert!(!request(&observed, &labels, &[], None).check().unwrap());
        assert!(
            request(&observed, &labels, &[], Some("worker"))
                .check()
                .unwrap()
        );
        let text = loss_text(&observed, &labels, VolumeEffect::LoseAccess);
        assert!(text.contains("No Volumes are lost."), "{text}");
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
