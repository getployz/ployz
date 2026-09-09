//! Where a Build runs. The requested location comes from the CLI and Compose;
//! the automatic choice is a pure function of the Machines this client observed.
//! The selected Machine still admits or refuses the Build itself.

use std::collections::BTreeSet;

use ployz_core::{MachineId, MachineName, MachineTarget};
use thiserror::Error;

/// `--remote` given without a value. Shared with the clap definition in
/// `cli`, whose `default_missing_value` must stay this exact string for plain
/// `--remote` to mean automatic selection rather than a pin.
pub(crate) const AUTOMATIC: &str = "";

use crate::image::platform_compatible;

/// A `--remote` value that names no Machine.
#[derive(Debug, Error)]
#[error("--remote {0:?} is not a Machine name or ID")]
pub(crate) struct InvalidLocation(String);

/// Execution location a user asked for, before any Machine is observed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Location {
    /// Build in the invoking client's own Docker.
    Local,
    /// Build on a Machine, once one is resolved.
    Remote(Selection),
}

/// How the Build Machine is chosen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Selection {
    /// Choose one compatible Machine from what this client can observe.
    Automatic,
    /// Build on exactly this Machine.
    Pinned(MachineTarget),
}

impl Location {
    /// Resolve the requested location. A Build runs on this host unless a flag
    /// sends it to a Machine.
    ///
    /// `remote` is the `--remote` value: absent when the flag was not given,
    /// [`AUTOMATIC`] for plain `--remote`, otherwise the pinned Machine.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidLocation`] when a pinned target is not a Machine identity.
    pub(crate) fn requested(remote: Option<&str>, local: bool) -> Result<Self, InvalidLocation> {
        if local {
            return Ok(Self::Local);
        }
        match remote {
            None => Ok(Self::Local),
            Some(AUTOMATIC) => Ok(Self::Remote(Selection::Automatic)),
            Some(target) => MachineTarget::parse(target)
                .map(|target| Self::Remote(Selection::Pinned(target)))
                .map_err(|_| InvalidLocation(target.to_owned())),
        }
    }
}

/// What one visible Machine offers a Build, as observed by this client.
#[derive(Debug)]
pub(crate) struct Candidate {
    pub id: MachineId,
    pub name: MachineName,
    pub evidence: Evidence,
}

/// Build capability of one Machine. Unanswered evidence stays visible: it is
/// never read as either support or refusal.
#[derive(Debug)]
pub(crate) enum Evidence {
    /// The Machine advertises remote Builds. `architecture` is the kernel
    /// architecture from the Cluster's discovery record, not part of the
    /// Machine's answer and not Docker's own platform, so it can be stale or
    /// absent.
    Builds { architecture: String },
    /// The Machine answered and does not advertise remote Builds.
    Refuses,
    /// The Machine's membership structurally forbids an RPC, so it was never
    /// asked. That is a known ineligibility, not a failed probe.
    Ineligible(String),
    /// The Machine could not be asked, with the reason it could not.
    Unanswered(String),
}

/// The Machine an automatic Build selected, and what it cannot prove about it.
#[derive(Debug)]
pub(crate) struct Choice<'list> {
    pub machine: &'list Candidate,
    /// Required platforms this client could not confirm the Machine runs
    /// natively. The Machine confirms or refuses them at admission.
    pub unconfirmed: Vec<String>,
    /// Visible Machines that never answered for themselves, so a better
    /// candidate may have been passed over. One line of evidence each.
    pub unresolved: Vec<String>,
}

/// Why no Machine could be chosen. The observed evidence belongs here; the
/// command that could fix it belongs to the CLI.
#[derive(Debug, Error)]
pub(crate) enum NoBuildMachine {
    #[error("no Machine is visible, so no Build Machine can be selected")]
    Invisible,
    /// At least one Machine never spoke for itself, so "none can build" would
    /// claim more than was observed.
    #[error("no Machine that answered can run this Build, and {} did not answer. Observed {}", .unresolved.len(), observed(.refused, .unresolved))]
    Inconclusive {
        refused: Vec<String>,
        unresolved: Vec<String>,
    },
    #[error("no visible Machine can run this Build. Observed {}", .refused.join("; "))]
    Incapable { refused: Vec<String> },
}

/// Pick one Machine for an automatic Build.
///
/// Machines that never invite an RPC, do not answer, or answer without the
/// Build capability are not candidates. The Machine that natively runs the most required platforms wins,
/// and Machine ID breaks every remaining tie, so the choice does not depend on
/// the order the Machines were observed.
///
/// # Errors
///
/// Returns [`NoBuildMachine`] when no observed Machine can build, carrying every
/// Machine and the evidence that excluded it.
pub(crate) fn choose<'list>(
    candidates: &'list [Candidate],
    required: &BTreeSet<String>,
) -> Result<Choice<'list>, NoBuildMachine> {
    let mut eligible = Vec::new();
    let mut refused = Vec::new();
    let mut unresolved = Vec::new();
    for candidate in candidates {
        let name = &candidate.name;
        let id = &candidate.id;
        match &candidate.evidence {
            Evidence::Builds { architecture } => {
                eligible.push((candidate, unconfirmed(architecture, required)));
            }
            Evidence::Refuses => {
                refused.push(format!("{name} ({id}) does not run remote Builds"));
            }
            // Membership is one Machine's stale judgment of another, never the
            // Machine speaking for itself, so it cannot settle capability.
            Evidence::Ineligible(reason) => unresolved.push(format!("{name} ({id}) {reason}")),
            Evidence::Unanswered(reason) => {
                unresolved.push(format!("{name} ({id}) capability unknown: {reason}"));
            }
        }
    }
    let chosen = eligible
        .into_iter()
        .min_by(|(left, left_open), (right, right_open)| {
            left_open
                .len()
                .cmp(&right_open.len())
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        });
    let Some((machine, unconfirmed)) = chosen else {
        // Any silence leaves a Machine that might have built this, so the
        // capability conclusion is only available when every Machine answered.
        return Err(match (refused.is_empty(), unresolved.is_empty()) {
            (true, true) => NoBuildMachine::Invisible,
            (_, false) => NoBuildMachine::Inconclusive {
                refused,
                unresolved,
            },
            (false, true) => NoBuildMachine::Incapable { refused },
        });
    };
    Ok(Choice {
        machine,
        unconfirmed,
        unresolved,
    })
}

/// Every observed line, refusals before silence.
fn observed(refused: &[String], unresolved: &[String]) -> String {
    refused
        .iter()
        .chain(unresolved)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ")
}

/// Required platforms this client cannot confirm the Machine runs natively. An
/// unreported or unrecognised architecture proves nothing, so every required
/// platform stays unconfirmed rather than being called emulated.
pub(crate) fn unconfirmed(architecture: &str, required: &BTreeSet<String>) -> Vec<String> {
    required
        .iter()
        .filter(|platform| !platform_compatible(platform, architecture))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(hex: char, name: &str, evidence: Evidence) -> Candidate {
        Candidate {
            id: MachineId::parse(hex.to_string().repeat(32)).unwrap(),
            name: MachineName::parse(name).unwrap(),
            evidence,
        }
    }

    fn builds(architecture: &str) -> Evidence {
        Evidence::Builds {
            architecture: architecture.into(),
        }
    }

    fn platforms<const N: usize>(values: [&str; N]) -> BTreeSet<String> {
        values.into_iter().map(ToOwned::to_owned).collect()
    }

    #[test]
    fn flags_choose_the_location_and_a_bad_pin_names_the_flag() {
        let requested = Location::requested;
        let pinned = |name: &str| Location::Remote(Selection::Pinned(name.parse().unwrap()));
        assert_eq!(requested(None, false).unwrap(), Location::Local);
        assert_eq!(requested(None, true).unwrap(), Location::Local);
        // Plain --remote asks for a choice; a value pins one Machine.
        assert_eq!(
            requested(Some(AUTOMATIC), false).unwrap(),
            Location::Remote(Selection::Automatic)
        );
        assert_eq!(requested(Some("tower"), false).unwrap(), pinned("tower"));
        // --local wins over a --remote clap already refuses to pair it with.
        assert_eq!(requested(Some("tower"), true).unwrap(), Location::Local);
        // Nothing reserves these words now that no Compose key spells modes.
        assert_eq!(requested(Some("auto"), false).unwrap(), pinned("auto"));
        assert_eq!(requested(Some("local"), false).unwrap(), pinned("local"));
        let rejected = requested(Some("*"), false).unwrap_err().to_string();
        assert!(
            rejected.contains("--remote") && rejected.contains("*"),
            "{rejected}"
        );
    }

    #[test]
    fn automatic_selection_prefers_native_platforms_and_ignores_observation_order() {
        let observed = |order: [char; 3]| {
            order
                .into_iter()
                .map(|hex| match hex {
                    'a' => candidate('a', "amd", builds("x86_64")),
                    'b' => candidate('b', "arm", builds("aarch64")),
                    _ => candidate('c', "old", builds("")),
                })
                .collect::<Vec<_>>()
        };
        for order in [['a', 'b', 'c'], ['c', 'b', 'a'], ['b', 'a', 'c']] {
            let machines = observed(order);
            let native = choose(&machines, &platforms(["linux/arm64"])).unwrap();
            assert_eq!(native.machine.name.as_str(), "arm");
            assert!(native.unconfirmed.is_empty() && native.unresolved.is_empty());
            // No native candidate: the lowest Machine ID wins and the platforms
            // it cannot prove stay visible.
            let emulating = choose(&machines, &platforms(["linux/riscv64"])).unwrap();
            assert_eq!(emulating.machine.name.as_str(), "amd");
            assert_eq!(emulating.unconfirmed, ["linux/riscv64"]);
            // Nothing required: stable Machine-ID order alone decides.
            assert_eq!(
                choose(&machines, &BTreeSet::new())
                    .unwrap()
                    .machine
                    .name
                    .as_str(),
                "amd"
            );
        }
    }

    #[test]
    fn incapable_and_unanswered_machines_are_not_candidates_but_stay_in_the_refusal() {
        let machines = [
            candidate('a', "old", Evidence::Refuses),
            candidate('b', "gone", Evidence::Unanswered("did not answer".into())),
        ];
        let refusal = choose(&machines, &BTreeSet::new()).unwrap_err().to_string();
        assert!(refusal.contains("old"), "{refusal}");
        assert!(refusal.contains("does not run remote Builds"), "{refusal}");
        assert!(refusal.contains("gone"), "{refusal}");
        assert!(refusal.contains("did not answer"), "{refusal}");
        assert!(
            matches!(
                choose(&[], &BTreeSet::new()).unwrap_err(),
                NoBuildMachine::Invisible
            ),
            "an empty Cluster is not a capability refusal"
        );
        // Any silence leaves the capability question open, whether it came
        // from a failed probe or from another Machine's judgment of it.
        for unresolved in [
            Evidence::Unanswered("did not answer".into()),
            Evidence::Ineligible("has membership down and does not invite an RPC".into()),
        ] {
            let quiet = [candidate('b', "gone", unresolved)];
            assert!(matches!(
                choose(&quiet, &BTreeSet::new()).unwrap_err(),
                NoBuildMachine::Inconclusive { .. }
            ));
            // A definite refusal alongside silence still cannot conclude.
            let mixed = [
                candidate('a', "old", Evidence::Refuses),
                quiet.into_iter().next().unwrap(),
            ];
            let error = choose(&mixed, &BTreeSet::new()).unwrap_err();
            assert!(
                matches!(error, NoBuildMachine::Inconclusive { .. }),
                "one silent Machine must not license a capability conclusion"
            );
            assert!(error.to_string().contains("did not answer"), "{error}");
        }
        // Only when every visible Machine answered is the conclusion available.
        assert!(matches!(
            choose(
                &[candidate('a', "old", Evidence::Refuses)],
                &BTreeSet::new()
            )
            .unwrap_err(),
            NoBuildMachine::Incapable { .. }
        ));
        // One capable Machine among them is selected, and the Machine that
        // never answered stays visible on that success.
        let mut machines = Vec::from_iter(machines);
        machines.push(candidate('c', "tower", builds("x86_64")));
        let choice = choose(&machines, &platforms(["linux/amd64"])).unwrap();
        assert_eq!(choice.machine.name.as_str(), "tower");
        assert!(choice.unconfirmed.is_empty());
        assert_eq!(choice.unresolved.len(), 1, "{choice:?}");
        assert!(choice.unresolved.concat().contains("gone"), "{choice:?}");
    }

    #[test]
    fn a_multi_platform_request_prefers_the_machine_that_emulates_least() {
        let machines = [
            // Reports no architecture: it would emulate both.
            candidate('a', "old", builds("")),
            candidate('b', "arm", builds("aarch64")),
        ];
        let choice = choose(&machines, &platforms(["linux/amd64", "linux/arm64"])).unwrap();
        assert_eq!(choice.machine.name.as_str(), "arm");
        assert_eq!(choice.unconfirmed, ["linux/amd64"]);
        // A variant suffix names the same execution platform.
        assert!(
            choose(&machines, &platforms(["linux/arm64/v8"]))
                .unwrap()
                .unconfirmed
                .is_empty()
        );
    }
}
