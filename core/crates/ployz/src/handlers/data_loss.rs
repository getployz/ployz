//! CLI removal acceptance over a fixed, observer-relative deletion list.

use super::{Error, leaf_matches, string_values};
use crate::{cli::Rerun, connect::Client, context::ConnectionSource};
use clap::ArgMatches;
use ployz_core::{DataLoss, DataLossConfirmation, ObservedDataLoss, RpcError, UnconfirmedDataLoss};
use std::collections::{BTreeMap, BTreeSet};

use crate::ui::{self, Tree};

/// The Volume name each Docker Volume keeps a Volume's data for, by Docker name.
pub(super) type VolumeLabels = BTreeMap<String, String>;

/// What a person names `loss` by: its Volume's name, else its Docker name.
pub(super) fn volume_label<'a>(labels: &'a VolumeLabels, loss: &'a DataLoss) -> &'a str {
    labels.get(loss.name()).map_or(loss.name(), String::as_str)
}

/// The Server a removal takes and the word typed to confirm it.
#[derive(Clone, Copy)]
pub(super) struct Typed<'a> {
    server: &'a str,
    word: &'a str,
}

impl<'a> Typed<'a> {
    /// Confirmed by the Server's own name.
    pub(super) fn name(server: &'a str) -> Self {
        Self {
            server,
            word: server,
        }
    }

    /// Confirmed by `dead`: a Server that does not answer.
    pub(super) fn dead(server: &'a str) -> Self {
        Self {
            server,
            word: "dead",
        }
    }
}

/// Accept removing `typed`'s Server and the Volumes it takes. `--confirm` with its
/// word and every lost Volume named by `--accept-volume-loss` accepts outright;
/// without `--confirm` a terminal shows the loss and asks for the word, which
/// accepts them all. Anywhere else, `refusal` gets the full retry.
pub(super) fn confirm_removal(
    root: &ArgMatches,
    client: &Client,
    removed_through: &ConnectionSource,
    observed: &ObservedDataLoss,
    typed: Typed<'_>,
    labels: &VolumeLabels,
    refusal: impl FnOnce(Rerun) -> Error,
) -> Result<(DataLossConfirmation, Rerun), Error> {
    let Typed { server, word } = typed;
    let leaf = leaf_matches(root);
    let request = Request {
        observed,
        labels,
        server,
        word,
        named: &string_values(leaf, "accept-volume-loss"),
        typed: leaf.get_one::<String>("confirm").map(String::as_str),
        retry: retry_args(root, client.connection_source()),
    };
    if observed.data_loss.is_empty() && !request.named.is_empty() {
        return Err(Error::usage(format!(
            "Server {server} has no Volumes requiring loss acceptance; drop --accept-volume-loss. No changes made."
        )));
    }
    if !request.check()? {
        let retry = request.accepting();
        if ui::can_prompt() {
            ui::note_inline(loss(observed, labels, server, removed_through));
        }
        ui::confirm_name(word, || refusal(retry), "Cancelled. Nothing was removed.")?;
    }
    Ok((request.accept()?, request.accepting()))
}

/// This removal again, reaching the Server through the context it was reached through.
/// Any approval drops: the one a retry needs is for what it accepts.
pub(super) fn retry_args(root: &ArgMatches, source: &ConnectionSource) -> Rerun {
    let again = Rerun::again(root).without("approval");
    match source {
        ConnectionSource::Context(name) => again.set("context", name),
        ConnectionSource::Direct | ConnectionSource::LocalSocket | ConnectionSource::Cloud => again,
    }
}

fn loss(
    observed: &ObservedDataLoss,
    labels: &VolumeLabels,
    server: &str,
    source: &ConnectionSource,
) -> String {
    let context = match source {
        ConnectionSource::Context(name) => format!("context {name}"),
        ConnectionSource::Direct => "a direct connection".into(),
        ConnectionSource::LocalSocket => "the local socket".into(),
        ConnectionSource::Cloud => "Ployz Cloud".into(),
    };
    let volumes = if observed.data_loss.is_empty() {
        Tree::leaf("No Volumes are lost.")
    } else {
        Tree::new(
            "Volumes only this Server holds, lost with it:",
            observed
                .data_loss
                .iter()
                .map(|loss| Tree::leaf(volume_label(labels, loss)))
                .collect(),
        )
    };
    let tree = Tree::new(
        format!("Removing Server {server} through {context}:"),
        vec![volumes],
    );
    match source {
        ConnectionSource::Cloud => tree.to_string(),
        ConnectionSource::Direct | ConnectionSource::Context(_) | ConnectionSource::LocalSocket => {
            format!("{tree}{CAVEAT}\n")
        }
    }
}

const CAVEAT: &str = "Based on what the connected Server can see; other Servers may hold more.";

/// What the flags of one `server rm` say.
struct Request<'a> {
    observed: &'a ObservedDataLoss,
    labels: &'a VolumeLabels,
    server: &'a str,
    /// What `--confirm` must say: the Server's name, or `dead`.
    word: &'a str,
    named: &'a [String],
    typed: Option<&'a str>,
    retry: Rerun,
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
    fn accepting(&self) -> Rerun {
        self.retry
            .clone()
            .set("confirm", self.word)
            .set_all("accept-volume-loss", self.names())
    }

    /// Whether the flags accept on their own; `false` means the word must be
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
            && typed != self.word
        {
            let message = if self.word == self.server {
                format!(
                    "--confirm {} does not match Server {}. No changes made.",
                    typed.escape_debug(),
                    self.server
                )
            } else {
                format!(
                    "--confirm {} is not {}: Server {} does not answer. No changes made.",
                    typed.escape_debug(),
                    self.word,
                    self.server
                )
            };
            return Err(Error::usage(message).hint(self.accepting()));
        }
        let missing = names.difference(&supplied).copied().collect::<Vec<_>>();
        if !missing.is_empty() && (self.typed.is_some() || !supplied.is_empty()) {
            return Err(Error::usage(format!(
                "Missing volume acceptance: {}. No changes made.",
                missing.join(", ")
            ))
            .hint(self.accepting()));
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

fn pass_data_loss_names_message(missing: &[DataLoss]) -> String {
    format!(
        "Additional volume loss is not covered by the confirmation: {}. Rerun to review the updated volume list.",
        missing
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

pub(super) fn refusal_from_rpc(error: RpcError) -> Error {
    match UnconfirmedDataLoss::from_rpc_error(&error) {
        Some(unconfirmed) => Error::usage(pass_data_loss_names_message(&unconfirmed.missing)),
        None => error.into(),
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
            word: "worker",
            named,
            typed,
            retry: removing(&[]),
        }
    }

    /// `ployz server rm worker` with `flags`, again.
    fn removing(flags: &[&str]) -> Rerun {
        let words = ["ployz", "server", "rm", "worker"].iter().chain(flags);
        Rerun::again(&crate::cli::command().try_get_matches_from(words).unwrap())
    }

    fn retry_of(error: &Error) -> String {
        let hints = error.hints();
        let [ployz_core::Hint::Retry(retry)] = hints.as_slice() else {
            panic!("{hints:?}");
        };
        retry.command().to_owned()
    }

    #[test]
    fn the_loss_names_the_volumes_only_this_server_holds() {
        let observed = ObservedDataLoss {
            data_loss: vec![loss('a', "shop-production_vol-1")],
        };
        let labels = VolumeLabels::from([("shop-production_vol-1".into(), "pgdata".into())]);
        let lost = loss_text(&observed, &labels);
        assert!(
            lost.contains("Removing Server worker through context prod:"),
            "{lost}"
        );
        assert!(
            lost.contains("Volumes only this Server holds, lost with it:"),
            "{lost}"
        );
        assert!(lost.contains("└─ pgdata\n"), "{lost}");
        assert!(!lost.contains(&"a".repeat(32)), "ids are machinery: {lost}");
        assert!(lost.ends_with(&format!("{CAVEAT}\n")), "{lost}");
    }

    fn loss_text(observed: &ObservedDataLoss, labels: &VolumeLabels) -> String {
        loss_through(observed, labels, &ConnectionSource::Context("prod".into()))
    }

    fn loss_through(
        observed: &ObservedDataLoss,
        labels: &VolumeLabels,
        through: &ConnectionSource,
    ) -> String {
        let shown = super::loss(observed, labels, "worker", through);
        anstream::adapter::strip_str(&shown).to_string()
    }

    #[test]
    fn a_removal_cloud_runs_says_cloud_removes_it() {
        let observed = ObservedDataLoss {
            data_loss: Vec::new(),
        };
        let lost = loss_through(&observed, &VolumeLabels::new(), &ConnectionSource::Cloud);
        assert!(
            lost.starts_with("Removing Server worker through Ployz Cloud:"),
            "{lost}"
        );
        assert!(
            !lost.contains(CAVEAT),
            "Cloud reads the loss itself, not the connected Server: {lost}"
        );
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
        for named in [vec![], vec!["data".into()]] {
            let error = Request {
                retry: removing(&["--context", "prod-west"]),
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
                    .contains(&"prod-west".into())
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
        let error =
            refusal_from_rpc(changed.require(&confirmed).unwrap_err().into_rpc_error()).to_string();
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
        let text = loss_text(&observed, &labels);
        assert!(text.contains("No Volumes are lost."), "{text}");
    }

    #[test]
    fn retry_preserves_effective_selection_and_quotes_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("socket name");
        let config = dir.path().join("config's file");
        std::fs::write(&socket, "").unwrap();
        std::fs::write(&config, "").unwrap();
        let connect = format!("unix://{}", socket.display());
        let config = config.to_str().unwrap();
        let root = crate::cli::command()
            .try_get_matches_from([
                "ployz",
                "server",
                "forget",
                "edge",
                "--connect",
                &connect,
                "--ployz-config",
                config,
            ])
            .unwrap();
        let line = retry_args(&root, &ConnectionSource::Direct).line();
        let parsed = crate::cli::command()
            .try_get_matches_from(shell_words::split(&line).unwrap())
            .unwrap();
        let leaf = leaf_matches(&parsed);
        assert_eq!(crate::cli::command_path(&parsed), "server forget", "{line}");
        assert_eq!(string_values(leaf, "server"), ["edge"]);
        assert_eq!(crate::cli::connection(leaf).unwrap(), connect);
        assert_eq!(leaf.get_one::<String>("ployz-config").unwrap(), config);
    }
}
