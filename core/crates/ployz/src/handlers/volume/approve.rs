//! A Device's approval of a Mirror. When a Server of the Cluster enforces Device approval,
//! `ployz volume mirror` and `ployz volume sync --full` sign the Mirror they ask Cloud for:
//! the Volume, the Server that writes it and the Server that keeps the copy. The Mirror's
//! Server then pulls only from that Writer, and the Writer's send stream serves only that
//! Mirror, for this sync and every later one until the Mirror is removed.

use clap::ArgMatches;
use ployz_core::authority::{
    ApprovedVolumeChange, Signed, Signer, SignerKey, Timestamp, VolumeApproval, VolumeWorkflow,
    validity::MAX_LIFETIME_MILLIS,
};
use ployz_core::{MachineName, OperationId, RpcErrorCode, SlotRole, VolumeCopy};
use ployz_store::{EnvironmentSummary, VolumeId, VolumeName};

use super::super::deploy::approve::{Probe, probe};
use super::super::{Error, leaf_matches, store};
use super::restore::{Seen, copies, docker_volume};

/// A signed Mirror approval and the change it approved.
pub(super) struct MirrorApproval {
    pub(super) signed: Signed<VolumeApproval>,
    pub(super) change: ApprovedVolumeChange,
}

/// The Server that keeps the Mirror: one named on the command line, or the Volume's current
/// Mirror, which a full Sync copies again.
#[derive(Clone, Copy)]
pub(super) enum MirrorOn<'a> {
    Server(&'a MachineName),
    Current,
}

/// Sign the Mirror of `volume` onto `on` with this CLI's Device key, when it holds one and
/// a Server of the Cluster enforces Device approval, and show the change. None otherwise:
/// the run goes on as before, and a Server that enforces refuses a copy nobody approved.
/// With `replace`, the approval lets the Mirror's Server destroy the copy it holds first.
///
/// # Errors
/// Refuses when a Server enforces but none reports the Cluster's key, when no Server or more
/// than one reports a Writer of the Volume, when the Mirror's Server is not in the Cluster or
/// is the Writer, and when the Volume has no Mirror to copy again; returns the Store's or the
/// key's error.
pub(super) fn sign(
    root: &ArgMatches,
    store: &store::Store<'_>,
    (volume, volume_id, environment): (&VolumeName, &VolumeId, &EnvironmentSummary),
    on: MirrorOn<'_>,
    replace: bool,
) -> Result<Option<MirrorApproval>, Error> {
    let matches = leaf_matches(root);
    let key = super::super::device::key::path(&super::super::config_path(matches)?);
    let Ok(Some(secret)) = super::super::device::key::load(&key) else {
        return Ok(None);
    };
    let enforcing = match probe(matches) {
        Probe::Off => return Ok(None),
        Probe::Unknown => {
            crate::ui::warn(
                "Couldn't ask your Servers whether they enforce device approval, so this \
                 device didn't approve the Mirror. Servers that enforce will refuse it.",
            );
            return Ok(None);
        }
        Probe::NoClusterKey => {
            return Err(Error::coded(
                RpcErrorCode::Unavailable,
                "Your Servers enforce device approval, but none reported the Cluster's key, \
                 so this device can't approve the Mirror. Run ployz status, then try again.",
            ));
        }
        Probe::Enforcing(enforcing) => enforcing,
    };
    let (namespace, docker) = docker_volume(store, volume, volume_id, environment)?;
    let context = matches
        .try_get_one::<String>("context")
        .ok()
        .flatten()
        .map(String::as_str);
    let seen = super::super::runtime()?.block_on(async {
        let client = super::super::server::connect(matches, context).await?;
        copies(&client, &docker).await
    })?;
    let (writer, mirror) = parties(volume, &seen, on)?;
    let signer = Signer::from_seed(&secret);
    let issued_at = Timestamp::now();
    let body = VolumeApproval {
        cluster: enforcing.cluster,
        operation: OperationId::random(),
        namespace,
        volume: docker,
        workflow: VolumeWorkflow::Mirror {
            writer: writer.id,
            mirror: mirror.id,
            replace,
        },
        issued_at,
        expires_at: issued_at.plus(MAX_LIFETIME_MILLIS),
        signer: SignerKey::Device(signer.public().into()),
    };
    let signed = Signed::sign(&body, &signer).map_err(Error::command)?;
    let change = ApprovedVolumeChange::of(&body, |id| {
        seen.iter()
            .find(|seen| seen.id == *id)
            .map_or_else(|| id.to_string(), |seen| seen.name.clone())
    })
    .expect("a Mirror approval names a change");
    crate::ui::stream(change.line());
    Ok(Some(MirrorApproval { signed, change }))
}

/// The Server that writes `volume` and the one that keeps its Mirror, from what each Server
/// reported.
fn parties<'seen>(
    volume: &VolumeName,
    seen: &'seen [Seen],
    on: MirrorOn<'_>,
) -> Result<(&'seen Seen, &'seen Seen), Error> {
    let holds = |seen: &&Seen, wanted: fn(&VolumeCopy) -> bool| matches!(&seen.copy, Ok(Some(copy)) if wanted(copy));
    let writers: Vec<&Seen> = seen
        .iter()
        .filter(|seen| {
            holds(seen, |copy| {
                matches!(
                    copy,
                    VolumeCopy::Root {
                        readonly: false,
                        ..
                    }
                )
            })
        })
        .collect();
    let writer = match writers.as_slice() {
        [writer] => *writer,
        [] => {
            return Err(Error::coded(
                RpcErrorCode::Unavailable,
                format!(
                    "No Server reports a writer of {volume}, so this device can't approve a \
                     Mirror of it. Run ployz status, then try again."
                ),
            ));
        }
        several => {
            let names: Vec<&str> = several.iter().map(|seen| seen.name.as_str()).collect();
            return Err(Error::coded(
                RpcErrorCode::Conflict,
                format!(
                    "{volume} has writers on {}, so this device can't tell which one to mirror.",
                    names.join(" and ")
                ),
            ));
        }
    };
    let mirror = match on {
        MirrorOn::Server(name) => seen
            .iter()
            .find(|seen| seen.name == name.as_str())
            .ok_or_else(|| Error::not_found(format!("No Server named {name} is in this Cluster.")))?,
        MirrorOn::Current => seen
            .iter()
            .find(|seen| {
                holds(seen, |copy| {
                    matches!(copy, VolumeCopy::Slot { role, .. } if *role != Some(SlotRole::OldCopy))
                })
            })
            .ok_or_else(|| Error::not_found(format!("{volume} has no Mirror to copy again.")))?,
    };
    if mirror.id == writer.id {
        return Err(Error::coded(
            RpcErrorCode::InvalidArgument,
            format!(
                "{} writes {volume}, so it can't also keep its Mirror. Pick another Server.",
                mirror.name
            ),
        ));
    }
    Ok((writer, mirror))
}

#[cfg(test)]
#[path = "approve_tests.rs"]
mod tests;
