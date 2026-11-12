//! A Device's approval of a Mirror or a Move. When a Server of the Cluster enforces Device
//! approval, `ployz volume mirror` and `ployz volume sync --full` sign the Mirror they ask
//! Cloud for: the Volume, the Server that writes it and the Server that keeps the copy. The
//! Mirror's Server then pulls only from that Writer, and the Writer's send stream serves only
//! that Mirror, for this sync and every later one until the Mirror is removed.
//!
//! `ployz volume move` signs the Move: the Volume, the Server that writes it, the target,
//! the Mirrors that follow, and the spec its Service starts with on the target, which is
//! the one it runs with now. It shows that spec and asks before it signs, as a deploy does.

use std::collections::BTreeMap;

use clap::ArgMatches;
use ployz_core::authority::{
    ApprovedChange, ApprovedVolumeChange, Signed, Signer, SignerKey, Timestamp, VolumeApproval,
    VolumeWorkflow, deploy::content_addressed, validity::MAX_LIFETIME_MILLIS,
};
use ployz_core::{
    ContainerKind, ContainerObservation, ContainerRuntimeObservation, DockerVolumeName,
    InspectContainerRequest, MachineName, MachineTarget, Namespace, OperationId,
    ProvisionedVolumeMaximumBytes, PullPolicy, ResolvedServiceSpec, RpcErrorCode, SlotRole,
    VolumeCopy, op,
};
use ployz_store::{EnvironmentSummary, VolumeId, VolumeName};
use serde_json::json;

use super::super::deploy::approve::{Enforcing, Probe, probe};
use super::super::{Error, leaf_matches, store};
use super::restore::{Seen, copies, docker_volume};
use crate::cluster::{Call, Deadline};
use crate::connect::TARGET_RPC_TIMEOUT;
use crate::ui::Hint;

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
    let Some((signer, enforcing)) = device(matches, "the Mirror")? else {
        return Ok(None);
    };
    let (namespace, docker) = docker_volume(store, volume, volume_id, environment)?;
    let seen = super::super::runtime()?.block_on(async {
        let client = super::super::server::connect(matches, context(matches)).await?;
        copies(&client, &docker).await
    })?;
    let (writer, mirror) = parties(volume, &seen, on)?;
    let body = approval(
        &signer,
        &enforcing,
        namespace,
        docker,
        VolumeWorkflow::Mirror {
            writer: writer.id,
            mirror: mirror.id,
            replace,
        },
    );
    let signed = Signed::sign(&body, &signer).map_err(Error::command)?;
    let change = ApprovedVolumeChange::of(&body, volume.as_str(), named(&seen))
        .expect("a Mirror approval names a change");
    crate::ui::stream(change.line());
    Ok(Some(MirrorApproval { signed, change }))
}

/// A signed Move approval, the change it approved, and the spec its Service starts with on
/// the target.
pub(super) struct MoveApproval {
    pub(super) signed: Signed<VolumeApproval>,
    pub(super) change: ApprovedVolumeChange,
    pub(super) service: ResolvedServiceSpec,
}

/// Sign the Move of `volume` onto `to` with this CLI's Device key, when it holds one and a
/// Server of the Cluster enforces Device approval, after showing the change and asking. The Service starts
/// on `to` with the spec its one Container on the Writer runs, from the image already there,
/// at the size the Store records (`recorded`), as Cloud would start it. None otherwise.
///
/// # Errors
/// Refuses as [`sign`] does, and when `to` already writes the Volume, when the Writer runs no
/// Container of it or more than one, when its image is not pinned by digest, and when the
/// person says no or can't be asked without `--yes`; returns the
/// Store's, a Server's or the key's error.
pub(super) fn sign_move(
    root: &ArgMatches,
    store: &store::Store<'_>,
    (volume, volume_id, environment): (&VolumeName, &VolumeId, &EnvironmentSummary),
    to: &MachineName,
    recorded: Option<ProvisionedVolumeMaximumBytes>,
) -> Result<Option<MoveApproval>, Error> {
    let matches = leaf_matches(root);
    let Some((signer, enforcing)) = device(matches, "the Move")? else {
        return Ok(None);
    };
    let (namespace, docker) = docker_volume(store, volume, volume_id, environment)?;
    let (seen, (writer, target), service) = super::super::runtime()?.block_on(async {
        let client = super::super::server::connect(matches, context(matches)).await?;
        let seen = copies(&client, &docker).await?;
        let (writer, target) = parties(volume, &seen, MirrorOn::Server(to))?;
        let service = running_spec(&client, volume, &docker, writer).await?;
        let ids = (writer.id, target.id);
        Ok::<_, Error>((seen, ids, service))
    })?;
    let service = starting(service, &docker, recorded)?;
    // The approval names its Mirrors sorted by ID, as every Server reads them.
    let mirrors: Vec<_> = seen
        .iter()
        .filter(|seen| seen.id != target && keeps_mirror(seen))
        .map(|seen| seen.id)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let [change] = <[ApprovedChange; 1]>::try_from(
        ApprovedChange::of(&[service.to_requested()], &namespace, &BTreeMap::new())
            .map_err(Error::command)?,
    )
    .expect("one spec makes one change");
    let body = approval(
        &signer,
        &enforcing,
        namespace,
        docker,
        VolumeWorkflow::Move {
            from: writer,
            to: target,
            service: change.approved(),
            mirrors,
        },
    );
    let change = ApprovedVolumeChange::moving(&body, volume.as_str(), named(&seen), change)
        .expect("a Move approval names the change it was made from");
    for line in change.lines() {
        crate::ui::note(format_args!("{line}"));
    }
    let yes = matches
        .try_get_one::<bool>("yes")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false);
    consent(volume, yes, |question, refusal, declined| {
        crate::ui::confirm(question, || refusal, declined)
    })?;
    let signed = Signed::sign(&body, &signer).map_err(Error::command)?;
    Ok(Some(MoveApproval {
        signed,
        change,
        service,
    }))
}

/// What a person says to signing the Move of `volume`, asked after its lines show: Cloud
/// relays the Writer's spec the Move starts on the target, so a setting it planted must be
/// refusable before this Device signs it. `--yes` answers for a run nobody watches; without
/// it, a run that can't ask refuses before signing, and a person who says no moves nothing.
fn consent(
    volume: &VolumeName,
    yes: bool,
    confirm: impl FnOnce(&str, Error, &'static str) -> Result<(), Error>,
) -> Result<(), Error> {
    if yes {
        return Ok(());
    }
    let refusal = Error::detailed(
        RpcErrorCode::ConfirmationRequired,
        "Your servers enforce device approval, so signing this move needs your yes, and \
         there's no terminal to ask in. Read the lines above, then add --yes to sign them. \
         Nothing moved.",
        json!({ "volume": volume.as_str() }),
    )
    .hint(Hint::Retry(super::super::typed_with(&["--yes"])));
    confirm(
        &format!("Approve the move of {volume}?"),
        refusal,
        "Cancelled. Nothing signed or moved.",
    )
}

/// This CLI's Device signer and what the Servers report of enforcement, when it holds a key
/// and a Server enforces. `what` names the run in the refusal and the warning.
pub(super) fn device(
    matches: &ArgMatches,
    what: &str,
) -> Result<Option<(Signer, Enforcing)>, Error> {
    let key = super::super::device::key::path(&super::super::config_path(matches)?);
    let Ok(Some(secret)) = super::super::device::key::load(&key) else {
        return Ok(None);
    };
    match probe(matches) {
        Probe::Off => Ok(None),
        Probe::Unknown => {
            crate::ui::warn(format!(
                "Couldn't ask your Servers whether they enforce device approval, so this \
                 device didn't approve {what}. Servers that enforce will refuse it."
            ));
            Ok(None)
        }
        Probe::NoClusterKey => Err(Error::coded(
            RpcErrorCode::Unavailable,
            format!(
                "Your Servers enforce device approval, but none reported the Cluster's key, \
                 so this device can't approve {what}. Run ployz status, then try again."
            ),
        )),
        Probe::Enforcing(enforcing) => Ok(Some((Signer::from_seed(&secret), enforcing))),
    }
}

pub(super) fn context(matches: &ArgMatches) -> Option<&str> {
    matches
        .try_get_one::<String>("context")
        .ok()
        .flatten()
        .map(String::as_str)
}

pub(super) fn approval(
    signer: &Signer,
    enforcing: &Enforcing,
    namespace: Namespace,
    volume: DockerVolumeName,
    workflow: VolumeWorkflow,
) -> VolumeApproval {
    let issued_at = Timestamp::now();
    VolumeApproval {
        cluster: enforcing.cluster,
        operation: OperationId::random(),
        namespace,
        volume,
        workflow,
        issued_at,
        expires_at: issued_at.plus(MAX_LIFETIME_MILLIS),
        signer: SignerKey::Device(signer.public().into()),
    }
}

/// Each Server's name as it reported it, or its ID when it reported none.
pub(super) fn named(seen: &[Seen]) -> impl Fn(&ployz_core::MachineId) -> String + '_ {
    |id| {
        seen.iter()
            .find(|seen| seen.id == *id)
            .map_or_else(|| id.to_string(), |seen| seen.name.clone())
    }
}

/// Whether `seen` keeps a Mirror of the Volume, rather than an old copy a Move left.
fn keeps_mirror(seen: &Seen) -> bool {
    matches!(&seen.copy, Ok(Some(VolumeCopy::Slot { role, .. })) if *role != Some(SlotRole::OldCopy))
}

/// The spec of the one Service Container on `writer` that mounts `docker`, as its Server
/// keeps it.
async fn running_spec(
    client: &crate::connect::Client,
    volume: &VolumeName,
    docker: &DockerVolumeName,
    writer: &Seen,
) -> Result<ResolvedServiceSpec, Error> {
    let frame = client.runtime_frame().await?;
    let holder = match holders(&frame.containers, &writer.id, docker).as_slice() {
        [holder] => *holder,
        [] => {
            return Err(Error::coded(
                RpcErrorCode::Unavailable,
                format!(
                    "No Service running on {} uses {volume}, so this device can't approve moving it. \
                     Run ployz status, then try again.",
                    writer.name
                ),
            ));
        }
        several => {
            return Err(Error::coded(
                RpcErrorCode::Conflict,
                format!(
                    "{} running Containers on {} use {volume}; a Move starts exactly one on the new Server.",
                    several.len(),
                    writer.name
                ),
            ));
        }
    };
    let details = client
        .call_shared::<op::InspectContainer>(
            InspectContainerRequest {
                container_id: holder.container_id,
            },
            Call::once()
                .to(&MachineTarget::from(&writer.id))
                .within(Deadline::Target(TARGET_RPC_TIMEOUT)),
        )
        .await
        .map_err(crate::connect::rpc_error)?;
    Ok(details.container.into_parts().resolved_spec)
}

/// The Service Containers running on `writer` that mount `docker`. A stopped one, such as
/// the Container an earlier Move's cut left behind on a Server that enforces, runs nothing
/// the Move starts.
fn holders<'a>(
    containers: &'a [ContainerObservation],
    writer: &ployz_core::MachineId,
    docker: &DockerVolumeName,
) -> Vec<&'a ContainerObservation> {
    containers
        .iter()
        .filter(|container| {
            container.machine_id == *writer
                && container.kind == ContainerKind::ServiceContainer
                && matches!(
                    container.runtime,
                    ContainerRuntimeObservation::Running { .. }
                )
                && container
                    .resolved_spec
                    .volumes()
                    .iter()
                    .any(|mounted| mounted.source.docker_volume_name() == Some(docker))
        })
        .collect()
}

/// `running` as the target starts it: from the image already copied there, with `docker`
/// at the size the Store records when it records one.
///
/// # Errors
/// Refuses an image not pinned by digest, which no Server would start under approval.
fn starting(
    running: ResolvedServiceSpec,
    docker: &DockerVolumeName,
    recorded: Option<ProvisionedVolumeMaximumBytes>,
) -> Result<ResolvedServiceSpec, Error> {
    if !content_addressed(&running.container.image) {
        return Err(Error::coded(
            RpcErrorCode::Conflict,
            format!(
                "{} runs {}, which isn't pinned by digest, so this device can't approve \
                 starting it on another Server. Deploy it again, then move the Volume.",
                running.name, running.container.image
            ),
        ));
    }
    let mut spec = serde_json::to_value(&running).map_err(Error::command)?;
    if let Some(recorded) = recorded
        && let Some(volumes) = spec
            .get_mut("volumes")
            .and_then(|volumes| volumes.as_array_mut())
    {
        for mounted in volumes {
            let source = &mut mounted["source"];
            if source["kind"] == "provisioned" && source["name"] == docker.as_str() {
                source["maximum_bytes"] = serde_json::json!(recorded);
            }
        }
    }
    let mut spec: ResolvedServiceSpec = serde_json::from_value(spec).map_err(Error::command)?;
    spec.container.pull_policy = PullPolicy::Never;
    Ok(spec)
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
