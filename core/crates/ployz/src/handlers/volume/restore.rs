//! `ployz volume restore` without Ployz Cloud: the CLI sends the one `SetVolumeCopy` Cloud's
//! Restore sends, after the same checks. Every member Server reports its copy first, since a
//! copy on one that didn't could become a second writer. The source's own Machine then admits
//! the step against the copy it holds, under its switch lock (DESIGN.md bet 6). Nothing is
//! recorded: the next Deploy reads the new writer off the Servers.

use std::time::{Duration, Instant};

use chrono::DateTime;
use clap::ArgMatches;
use futures_util::{StreamExt, stream};
use ployz_core::{
    CopyRefusal, DockerVolumeName, ExpectedCopy, InspectVolumeCopyRequest, MachineId, MachineName,
    MachineStorageObservation, MachineTarget, RpcErrorCode, SetVolumeCopyRefused,
    SetVolumeCopyRequest, Snapshot, VOLUME_FLOOR_CAPABILITY, VolumeCopy, VolumeGen, WantedCopy,
    WriterEpoch, op,
};
use ployz_store::{EnvironmentSummary, NamespacesQuery, VolumeId, VolumeName};
use serde::Serialize;

use super::super::store::{self, Next};
use super::super::{Error, leaf_matches};
use crate::connect::TARGET_RPC_TIMEOUT;
use crate::ui::Hint;

/// How long a Restore waits out a Machine that answers `busy`, as Cloud's does.
const BUSY_PATIENCE: Duration = Duration::from_secs(60);
const INSPECTS_AT_ONCE: usize = 8;
const BUSY_POLL: Duration = if cfg!(test) {
    Duration::from_millis(10)
} else {
    Duration::from_secs(5)
};

/// What one member Server reported for the Volume, or why it didn't. A Machine the entry
/// lists only by ID goes by that ID.
#[derive(Debug)]
pub(super) struct Seen {
    pub(super) id: MachineId,
    pub(super) name: String,
    pub(super) copy: Result<Option<VolumeCopy>, Unseen>,
}

#[derive(Debug)]
pub(super) enum Unseen {
    /// Its copy is unknown: it did not answer, is down, or is still arriving at the entry.
    Silent(String),
    /// It holds a copy but advertises no Volume floor, so it would ignore the run's `gen`.
    Unfenced,
}

#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "one plan per command; the restore carries the request it sends"
)]
pub(super) enum Planned {
    Restore {
        machine: MachineId,
        request: SetVolumeCopyRequest,
        /// The newest snapshot of a slot restored: writes after it are lost. A root holds
        /// every write, so restoring it loses none.
        lost_after: Option<Snapshot>,
    },
    AlreadyWriter,
}

/// Cloud's `planRestore`: the source must hold the only copy and a snapshot to restore. A slot
/// becomes the writer in an epoch above every one the Servers show; a read-only root keeps its
/// own.
pub(super) fn plan(
    volume: &VolumeName,
    docker: &DockerVolumeName,
    from: &MachineName,
    seen: &[Seen],
) -> Result<Planned, Error> {
    let silent: Vec<String> = seen
        .iter()
        .filter_map(|seen| match &seen.copy {
            Err(Unseen::Silent(why)) => Some(format!("{} {why}", seen.name)),
            Ok(_) | Err(Unseen::Unfenced) => None,
        })
        .collect();
    if !silent.is_empty() {
        return Err(Error::unavailable(format!(
            "Restore needs every Server to answer, since a copy on one that didn't would become a second writer. {}",
            silent.join("; ")
        )));
    }
    let unfenced: Vec<&str> = seen
        .iter()
        .filter(|seen| matches!(seen.copy, Err(Unseen::Unfenced)))
        .map(|seen| seen.name.as_str())
        .collect();
    if !unfenced.is_empty() {
        return Err(Error::coded(
            RpcErrorCode::Unsupported,
            format!(
                "{} {} a copy of {volume} but {} a Ployz too old for Restore; upgrade Ployz there first",
                unfenced.join(", "),
                if unfenced.len() == 1 { "holds" } else { "hold" },
                if unfenced.len() == 1 { "runs" } else { "run" },
            ),
        ));
    }
    let copies: Vec<(&Seen, &VolumeCopy)> = seen
        .iter()
        .filter_map(|seen| Some((seen, seen.copy.as_ref().ok()?.as_ref()?)))
        .collect();
    let Some(source) = seen.iter().find(|seen| seen.name == from.as_str()) else {
        return Err(
            Error::not_found(format!("{from} is not a Server of this Cluster"))
                .hint(Hint::valid(seen.iter().map(|seen| seen.name.clone()))),
        );
    };
    let Some(copy) = source.copy.as_ref().ok().and_then(Option::as_ref) else {
        return Err(Error::not_found(format!(
            "{from} holds no copy of {volume}"
        )));
    };
    let others: Vec<&str> = copies
        .iter()
        .filter(|(seen, _)| seen.id != source.id)
        .map(|(seen, _)| seen.name.as_str())
        .collect();
    if !others.is_empty() {
        let (a_copy, them) = match others.as_slice() {
            [_] => ("a copy", "its copy"),
            _ => ("copies", "one of theirs"),
        };
        let others = others.join(", ");
        return Err(Error::conflict(format!(
            "{volume} also has {a_copy} on {others}; Restore needs {from}'s copy to be the only one. Remove {others} from the Cluster with ployz server rm first, or keep {them} and restore from it instead"
        )));
    }
    // Only once no other copy exists: a rerun after a lost answer finds its own restore.
    if matches!(
        copy,
        VolumeCopy::Root {
            readonly: false,
            ..
        }
    ) {
        return Ok(Planned::AlreadyWriter);
    }
    let Some(newest) = newest(copy) else {
        return Err(Error::conflict(format!(
            "{from}'s copy of {volume} holds no snapshot to restore"
        )));
    };
    let (expect, epoch, lost_after) = match copy {
        VolumeCopy::Root { epoch, .. } => (ExpectedCopy::Writer { epoch: *epoch }, *epoch, None),
        VolumeCopy::Slot { slot, .. } => {
            let above = copies
                .iter()
                .map(|(_, copy)| observed_epoch(copy))
                .max()
                .unwrap_or(0);
            (
                ExpectedCopy::Mirror {
                    newest: newest.guid,
                    slot: *slot,
                    role: None,
                },
                WriterEpoch::new(above.saturating_add(1)),
                Some(newest.clone()),
            )
        }
    };
    Ok(Planned::Restore {
        machine: source.id,
        request: SetVolumeCopyRequest {
            name: docker.clone(),
            expect,
            want: WantedCopy::Writer {
                at: newest.guid,
                epoch,
                start: None,
            },
            r#gen: VolumeGen::default(),
        },
        lost_after,
    })
}

fn newest(copy: &VolumeCopy) -> Option<&Snapshot> {
    match copy {
        VolumeCopy::Root { newest, .. } | VolumeCopy::Slot { newest, .. } => newest.as_ref(),
    }
}

/// The highest epoch a copy shows: its root's, or the run that named its newest snapshot,
/// `w-<run>-<round>` or `f-<run>`, read as Cloud's `observedEpoch` reads it.
fn observed_epoch(copy: &VolumeCopy) -> u32 {
    let root = match copy {
        VolumeCopy::Root { epoch, .. } => epoch.get(),
        VolumeCopy::Slot { .. } => 0,
    };
    let named = newest(copy)
        .and_then(|newest| {
            let name = newest.name.as_str();
            let rest = name
                .strip_prefix("w-")
                .or_else(|| name.strip_prefix("f-"))?;
            let digits = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            rest.get(..digits)?.parse().ok()
        })
        .unwrap_or(0);
    root.max(named)
}

#[derive(Serialize)]
struct Restored<'a> {
    volume: &'a VolumeName,
    server: &'a MachineName,
    /// Unix seconds of the snapshot restored from; writes after it are lost.
    #[serde(skip_serializing_if = "Option::is_none")]
    lost_after_unix_seconds: Option<i64>,
    already_writer: bool,
}

/// `ployz volume restore VOLUME --from SERVER` on a Cluster with no Cloud.
pub(super) fn standalone(
    root: &ArgMatches,
    store: &store::Store<'_>,
    volume: &VolumeName,
    volume_id: &VolumeId,
    environment: &EnvironmentSummary,
    from: &MachineName,
) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let context = matches
        .try_get_one::<String>("context")
        .ok()
        .flatten()
        .map(String::as_str);
    let namespace = store
        .read(&NamespacesQuery {})?
        .namespaces
        .into_iter()
        .find(|owned| owned.project == environment.project && owned.environment == environment.name)
        .map(|owned| owned.namespace)
        .ok_or_else(|| {
            Error::not_found(format!(
                "{}/{} has no deployed Namespace, so no Server holds {volume}",
                environment.project, environment.name
            ))
        })?;
    let docker = ployz_store::docker_volume(&namespace, volume_id.as_str())?;
    let runtime = super::super::runtime()?;
    let planned =
        runtime.block_on(async {
            let client = super::super::server::connect(matches, context).await?;
            let frame = client.runtime_frame().await?;
            let client = &client;
            let docker = &docker;
            let mut seen: Vec<Seen> =
                stream::iter(frame.machines.iter().filter(|observed| {
                    observed.storage != Some(MachineStorageObservation::Stateless)
                }))
                .map(|observed| async move {
                    let copy = if observed.invites_rpc() {
                        let inspected = client
                            .invoke::<op::InspectVolumeCopy>(
                                InspectVolumeCopyRequest {
                                    name: docker.clone(),
                                },
                                &MachineTarget::from(&observed.machine.id),
                                Some(TARGET_RPC_TIMEOUT),
                            )
                            .await;
                        let capabilities = &observed.machine.runtime.capabilities;
                        // An empty list is a record that predates it, which claims nothing.
                        let unfenced = !capabilities.is_empty()
                            && !capabilities.contains(VOLUME_FLOOR_CAPABILITY);
                        match inspected {
                            Ok(view) if unfenced && view.copy.is_some() => Err(Unseen::Unfenced),
                            Ok(view) => Ok(view.copy),
                            Err(error) => Err(Unseen::Silent(format!(
                                "did not answer ({})",
                                error.message
                            ))),
                        }
                    } else {
                        Err(Unseen::Silent("is down or hasn't joined yet".to_owned()))
                    };
                    Seen {
                        id: observed.machine.id,
                        name: observed.machine.name.to_string(),
                        copy,
                    }
                })
                .buffer_unordered(INSPECTS_AT_ONCE)
                .collect()
                .await;
            seen.extend(frame.incomplete_ids.machines.iter().map(|id| Seen {
                id: *id,
                name: id.to_string(),
                copy: Err(Unseen::Silent(
                    "is still arriving at the Server this connects through".to_owned(),
                )),
            }));
            seen.sort_by(|a, b| a.name.cmp(&b.name));
            let planned = plan(volume, docker, from, &seen)?;
            if let Planned::Restore {
                machine, request, ..
            } = &planned
            {
                set(client, machine, request.clone(), from).await?;
            }
            Ok::<_, Error>(planned)
        })?;
    let lost_after = match &planned {
        Planned::Restore { lost_after, .. } => lost_after.clone(),
        Planned::AlreadyWriter => None,
    };
    let deploy = store::next(matches, &["deploy"]);
    let restored = Restored {
        volume,
        server: from,
        lost_after_unix_seconds: lost_after
            .as_ref()
            .map(|newest| newest.created_unix_seconds),
        already_writer: matches!(planned, Planned::AlreadyWriter),
    };
    crate::ui::finish(&Next::new(&restored, Some(deploy.clone())), || {
        match (&planned, &lost_after) {
            (Planned::AlreadyWriter, _) => crate::ui::stream(format_args!(
                "{volume}'s writer is already on {from}; nothing to restore."
            )),
            (Planned::Restore { .. }, None) => {
                crate::ui::stream(format_args!("Restored {volume} on {from}."));
            }
            (Planned::Restore { .. }, Some(newest)) => crate::ui::stream(format_args!(
                "Restored {volume} on {from} from {}; writes after that time are lost.",
                DateTime::from_timestamp(newest.created_unix_seconds, 0).map_or_else(
                    || newest.created_unix_seconds.to_string(),
                    |time| time.format("%Y-%m-%d %H:%M:%S UTC").to_string()
                )
            )),
        }
        crate::ui::note("Deploy to start the Services that use it.");
        crate::ui::hint(&Hint::Next(deploy.clone()));
    })
}

/// Send `request`, resending while the Machine is `busy` and once at its floor when it
/// answers `stale`: an equal `gen` is admitted and raises no floor, so no Cloud run on this
/// Volume is fenced out by it.
async fn set(
    client: &crate::connect::Client,
    machine: &MachineId,
    mut request: SetVolumeCopyRequest,
    from: &MachineName,
) -> Result<(), Error> {
    let until = Instant::now() + BUSY_PATIENCE;
    let mut at_floor = false;
    loop {
        let error = match client
            .invoke::<op::SetVolumeCopy>(
                request.clone(),
                &MachineTarget::from(machine),
                Some(TARGET_RPC_TIMEOUT),
            )
            .await
        {
            Ok(_) => return Ok(()),
            Err(error) => error,
        };
        let code = error.code.clone();
        match SetVolumeCopyRefused::from_details(&error.details) {
            Some(SetVolumeCopyRefused {
                reason: CopyRefusal::Busy,
                ..
            }) if Instant::now() < until => tokio::time::sleep(BUSY_POLL).await,
            Some(SetVolumeCopyRefused {
                reason: CopyRefusal::Stale,
                floor: Some(floor),
                ..
            }) if !at_floor && request.r#gen < floor => {
                request.r#gen = floor;
                at_floor = true;
            }
            Some(_) => {
                return Err(Error::caused(
                    code,
                    format!("{from} refused to make its copy the writer"),
                    error,
                ));
            }
            // No refusal came back, so the Machine may have taken it: a rerun says so.
            None => {
                return Err(Error::caused(
                    code,
                    format!(
                        "Making {from}'s copy the writer did not finish; it may have happened anyway, and running this again reports which"
                    ),
                    error,
                ));
            }
        }
    }
}

#[cfg(test)]
#[path = "restore_tests.rs"]
mod tests;
