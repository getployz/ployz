//! A Device's approval of a change that loses data. When a Server of the Cluster enforces
//! Device approval, `ployz volume restore` through Cloud signs the copy it makes the writer,
//! at which snapshot, and the time after which writes are lost. `ployz volume mirror rm`
//! signs the copy it deletes, its role, and the Server that keeps writing, or that none does,
//! so the copy may be the last. The Server then promotes or destroys that copy and no other.

use chrono::{Local, SecondsFormat, TimeZone};
use clap::ArgMatches;
use ployz_core::authority::{
    ApprovedVolumeChange, Signed, Timestamp, VolumeApproval, VolumeWorkflow,
};
use ployz_core::{Hint, MachineName, RpcErrorCode, SlotRole, VolumeCopy, WantedCopy};
use ployz_store::{EnvironmentSummary, VolumeId, VolumeName};
use serde_json::json;

use super::super::{Error, leaf_matches, store};
use super::approve::{approval, context, device, named};
use super::restore::{Planned, Seen, Unseen, copies, docker_volume, plan};

/// A signed approval and the change it approved.
pub(super) struct LossApproval {
    pub(super) signed: Signed<VolumeApproval>,
    pub(super) change: ApprovedVolumeChange,
}

/// Sign the Restore of `volume` from `from` with this CLI's Device key, when it holds one and
/// a Server of the Cluster enforces Device approval, after the person reads what it loses and
/// says yes. The approval names the snapshot the copy becomes the writer at, and for a slot the
/// time of that snapshot: writes after it are not in the copy. None when there is nothing to sign: no
/// Device key, no Server enforcing, or `from` already writes the Volume.
///
/// # Errors
/// Refuses as a cloudless Restore plans: a Server that didn't answer, another copy, or no
/// snapshot to restore; and before signing when the person declines or no terminal can ask
/// and `--yes` wasn't given. Returns the Store's, a Server's or the key's error.
pub(super) fn sign_restore(
    root: &ArgMatches,
    store: &store::Store<'_>,
    (volume, volume_id, environment): (&VolumeName, &VolumeId, &EnvironmentSummary),
    from: &MachineName,
) -> Result<Option<LossApproval>, Error> {
    let matches = leaf_matches(root);
    let Some((signer, enforcing)) = device(matches, "the Restore")? else {
        return Ok(None);
    };
    let (namespace, docker) = docker_volume(store, volume, volume_id, environment)?;
    let seen = super::super::runtime()?.block_on(async {
        let client = super::super::server::connect(matches, context(matches)).await?;
        copies(&client, &docker).await
    })?;
    let Some(workflow) = restore_workflow(from, plan(volume, &docker, from, &seen)?)? else {
        return Ok(None);
    };
    let body = approval(&signer, &enforcing, namespace, docker, workflow);
    let change = ApprovedVolumeChange::restoring(&body, volume.as_str(), named(&seen), local_time)
        .expect("a Restore approval names a change");
    crate::ui::note(format_args!("{}", change.line()));
    consent(
        matches,
        &format!("Approve restoring {volume} from {from}?"),
        "restore",
        volume,
    )?;
    let signed = Signed::sign(&body, &signer).map_err(Error::command)?;
    Ok(Some(LossApproval { signed, change }))
}

/// Sign the removal of `volume`'s copy on `slot` with this CLI's Device key, when it holds one
/// and a Server of the Cluster enforces Device approval. The approval names the copy's role,
/// and the Server that writes the Volume, or none. With no writer the copy may be the Volume's
/// last, so `confirm` must be the Volume's name before anything is signed, as Cloud asks. None
/// when there is nothing to sign: no Device key, no Server enforcing, or a slot that holds no
/// snapshot, which its Server deletes unasked.
///
/// # Errors
/// Refuses a Server not in the Cluster or holding no Mirror, a copy that is no longer an old
/// one under `--if-old-copy`, a Server that didn't answer, a Volume with several writers, a last copy without `--confirm`,
/// and before signing a person who declines or can't be asked without `--yes`. Returns the
/// Store's, a Server's or the key's error.
pub(super) fn sign_removal(
    root: &ArgMatches,
    store: &store::Store<'_>,
    (volume, volume_id, environment): (&VolumeName, &VolumeId, &EnvironmentSummary),
    slot: &MachineName,
    (confirm, if_old_copy): (Option<&str>, bool),
) -> Result<Option<LossApproval>, Error> {
    let matches = leaf_matches(root);
    let Some((signer, enforcing)) = device(matches, "deleting the Mirror")? else {
        return Ok(None);
    };
    let (namespace, docker) = docker_volume(store, volume, volume_id, environment)?;
    let seen = super::super::runtime()?.block_on(async {
        let client = super::super::server::connect(matches, context(matches)).await?;
        copies(&client, &docker).await
    })?;
    let mirror = format!("{volume}-{slot}");
    let workflow = match removal_workflow(volume, slot, &seen, (confirm, if_old_copy)) {
        Ok(Some(workflow)) => workflow,
        Ok(None) => return Ok(None),
        Err(error) if error.report().code == RpcErrorCode::ConfirmationRequired => {
            // The command as typed keeps --yes and --wait; a wrong name is replaced instead.
            let retry = if confirm.is_none() {
                super::super::typed_with(&["--confirm", volume.as_str()])
            } else {
                let words =
                    super::run::removal_words(volume, slot, if_old_copy, Some(volume.as_str()));
                let words: Vec<&str> = words.iter().map(String::as_str).collect();
                store::next(matches, &words)
            };
            return Err(error.hint(Hint::Retry(retry)));
        }
        Err(error) => return Err(error),
    };
    let body = approval(&signer, &enforcing, namespace, docker, workflow);
    let change = ApprovedVolumeChange::of(&body, volume.as_str(), named(&seen))
        .expect("a Mirror removal names a change");
    crate::ui::note(format_args!("{}", change.line()));
    consent(
        matches,
        &format!("Approve deleting {mirror}?"),
        "delete",
        volume,
    )?;
    let signed = Signed::sign(&body, &signer).map_err(Error::command)?;
    Ok(Some(LossApproval { signed, change }))
}

/// The Restore a cloudless plan makes, as an approval names it: the copy, the snapshot it
/// becomes the writer at, and for a slot that snapshot's time, after which writes are lost. A
/// root keeps every write it took, so its Restore names no loss. None when `from` already
/// writes the Volume.
fn restore_workflow(from: &MachineName, planned: Planned) -> Result<Option<VolumeWorkflow>, Error> {
    let Planned::Restore {
        machine,
        request,
        lost_after,
    } = planned
    else {
        return Ok(None);
    };
    let WantedCopy::Writer { at, .. } = request.want else {
        unreachable!("a Restore asks for a Writer")
    };
    let loses_after = lost_after
        .map(|newest| {
            u64::try_from(newest.created_unix_seconds)
                .ok()
                .and_then(|seconds| Timestamp::from_millis(seconds.saturating_mul(1000)).ok())
                .ok_or_else(|| {
                    Error::conflict(format!("{from}'s newest snapshot has no valid time"))
                })
        })
        .transpose()?;
    Ok(Some(VolumeWorkflow::Restore {
        machine,
        at,
        loses_after,
    }))
}

/// The Mirror removal of `volume`'s copy on `slot` that `seen` calls for: its role, and the
/// Server that writes the Volume, or none. None for a slot holding no snapshot.
///
/// # Errors
/// Refuses as [`sign_removal`] does; a last copy without `confirm` refuses
/// `confirmation_required`.
fn removal_workflow(
    volume: &VolumeName,
    slot: &MachineName,
    seen: &[Seen],
    (confirm, if_old_copy): (Option<&str>, bool),
) -> Result<Option<VolumeWorkflow>, Error> {
    let mirror = format!("{volume}-{slot}");
    let Some(removed) = seen.iter().find(|seen| seen.name == slot.as_str()) else {
        return Err(Error::not_found(format!(
            "No Server named {slot} is in this Cluster."
        )));
    };
    let role = match &removed.copy {
        Ok(Some(VolumeCopy::Slot { newest: None, .. })) => return Ok(None),
        Ok(Some(VolumeCopy::Slot {
            role: Some(role), ..
        })) => *role,
        Ok(Some(VolumeCopy::Slot { role: None, .. })) => {
            return Err(Error::coded(
                RpcErrorCode::Unsupported,
                format!(
                    "{slot} runs a Ployz too old to say what {mirror} holds, so this device \
                     can't approve deleting it. Upgrade Ployz on {slot} first."
                ),
            ));
        }
        Ok(Some(VolumeCopy::Root { .. }) | None) => {
            return Err(Error::not_found(format!(
                "{volume} has no mirror on {slot}"
            )));
        }
        Err(_) => {
            return Err(Error::unavailable(format!(
                "{slot} did not answer, so this device can't tell what {mirror} holds."
            )));
        }
    };
    if if_old_copy && role != SlotRole::OldCopy {
        return Err(Error::conflict(format!(
            "{mirror} is no longer an old copy, so nothing was deleted"
        )));
    }
    // A Server whose copy is unknown may be the writer, and a removal signed without it would
    // say the copy may be the last.
    for other in seen {
        match &other.copy {
            Ok(_) => {}
            Err(Unseen::Silent(_)) => {
                return Err(Error::unavailable(format!(
                    "{} did not answer, so this device can't tell who writes {volume}. Try again \
                     once it answers, or forget it if it is gone.",
                    other.name
                )));
            }
            Err(Unseen::Unfenced) => {
                return Err(Error::coded(
                    RpcErrorCode::Unsupported,
                    format!(
                        "{} runs a Ployz too old to say what it holds of {volume}, so this device \
                         can't approve deleting {mirror}. Upgrade Ployz on {} first.",
                        other.name, other.name
                    ),
                ));
            }
        }
    }
    let roots: Vec<&Seen> = seen
        .iter()
        .filter(|seen| matches!(seen.copy, Ok(Some(VolumeCopy::Root { .. }))))
        .collect();
    let writer = match roots.as_slice() {
        [] => None,
        [writer] => Some(writer.id),
        several => {
            let names: Vec<&str> = several.iter().map(|seen| seen.name.as_str()).collect();
            return Err(Error::conflict(format!(
                "{volume} has two writers, {}",
                names.join(" and ")
            )));
        }
    };
    if writer.is_none() && confirm != Some(volume.as_str()) {
        return Err(Error::detailed(
            RpcErrorCode::ConfirmationRequired,
            format!(
                "{mirror} is {volume}'s only copy; volume mirror rm {mirror} --confirm {volume}"
            ),
            json!({ "volume": volume, "mirror": mirror }),
        ));
    }
    Ok(Some(VolumeWorkflow::MirrorRemoval {
        mirror: removed.id,
        writer,
        role,
    }))
}

/// What a person says to signing, asked after the approval's line shows. `--yes` answers for a
/// run nobody watches; without it, a run that can't ask refuses before signing.
fn consent(
    matches: &ArgMatches,
    question: &str,
    verb: &str,
    volume: &VolumeName,
) -> Result<(), Error> {
    let yes = matches
        .try_get_one::<bool>("yes")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false);
    if yes {
        return Ok(());
    }
    let refusal = Error::detailed(
        RpcErrorCode::ConfirmationRequired,
        format!(
            "Your servers enforce device approval, so signing this {verb} needs your yes, and \
             there's no terminal to ask in. Read the line above, then add --yes to sign it. \
             Nothing changed."
        ),
        json!({ "volume": volume }),
    )
    .hint(Hint::Retry(super::super::typed_with(&["--yes"])));
    crate::ui::confirm(
        question,
        || refusal,
        "Cancelled. Nothing signed or changed.",
    )
}

/// A signed time in this machine's zone, to the second.
fn local_time(at: Timestamp) -> String {
    i64::try_from(at.as_millis() / 1000)
        .ok()
        .and_then(|seconds| Local.timestamp_opt(seconds, 0).single())
        .map_or_else(
            || at.as_millis().to_string(),
            |time| time.to_rfc3339_opts(SecondsFormat::Secs, false),
        )
}

#[cfg(test)]
#[path = "loss_approval_tests.rs"]
mod tests;
