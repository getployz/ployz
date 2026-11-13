//! Claiming a queued Deployment: a runner takes it and receives its frozen Deploy Intent.

use super::*;
use ployz_core::authority::TeardownApproval;

/// A queued Deployment of `who`'s Organization, checked that a runner may still
/// claim it and a Server could run it. Nothing changes: the caller hands it to a runner.
pub(crate) fn start(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &DeploymentId,
    trusted: &crate::Trusted,
) -> Result<DeploymentSummary, RpcError> {
    owned(tx, who, id)?;
    let stored = locked(tx, id)?;
    match stored.summary.status {
        DeploymentStatus::Queued => {
            trusted.runnable()?;
            Ok(stored.summary)
        }
        DeploymentStatus::Running | DeploymentStatus::Cancelling => Err(error::conflict(
            "This Deployment is already running",
            json!({ "deployment": id }),
        )),
        DeploymentStatus::WaitingForCi => Err(waiting(id)),
        DeploymentStatus::Superseded => Err(superseded(id)),
        DeploymentStatus::Applied
        | DeploymentStatus::Failed
        | DeploymentStatus::Unknown
        | DeploymentStatus::Cancelled => Err(ended(id)),
    }
}

/// What a runner's claim of a Deployment came to.
pub(crate) enum Taken {
    /// The runner holds it and has its frozen Deploy Intent.
    Claimed(Box<Claimed>),
    /// It runs nothing, and the Deployment keeps what the refusal says it records.
    Refused(Refusal),
}

/// Why a claim ran nothing, and so what the Deployment keeps of it. Each carries the
/// error the runner is told.
pub(crate) enum Refusal {
    /// It isn't this runner's to claim: another runner's, replaced, waiting for CI or
    /// over. Nothing changes.
    Unclaimable(RpcError),
    /// Its runner came back after preparing it, so what ran is unknown: it reads
    /// `unknown` from here on.
    Lost(RpcError),
    /// It can't run as it stands: it ends Failed, with the refusal as its reason.
    Fails(RpcError),
}

impl Refusal {
    /// The error the runner is told.
    pub(crate) fn into_error(self) -> RpcError {
        match self {
            Self::Unclaimable(error) | Self::Lost(error) | Self::Fails(error) => error,
        }
    }
}

/// Bind a queued Deployment to `runner` and hand over its frozen Deploy Intent.
/// Claiming again as the same runner returns the same until it records a Deploy
/// Preview; after that it may have executed, so a second claim leaves the Deployment
/// `unknown` and refuses: nothing replays silently. A Deployment another runner still
/// holds without an outcome reads `unknown` from here on. Either way its runner is
/// lost then ([`ClaimedRun::lose_runner`]).
///
/// A [`Refusal`] is no error: the transaction commits what it records. The error
/// rolls back.
pub(crate) fn claim(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    runner: &RunnerId,
    sealing: &SealingKey,
) -> Result<Taken, RpcError> {
    let stored = locked(tx, id)?;
    take(tx, stored, runner, sealing)
}

/// Refuse the claim of `stored`, which records what `refusal` says and nothing else.
fn refuse(tx: &mut dyn Tx, mut stored: Stored, refusal: Refusal) -> Result<Taken, RpcError> {
    match &refusal {
        Refusal::Unclaimable(_) => {}
        Refusal::Lost(_) => save(tx, &mut stored)?,
        Refusal::Fails(refused) => {
            finish(
                tx,
                stored,
                Outcome::NotExecuted {
                    reason: refused.message.clone(),
                    cause: Vec::new(),
                    needs_upload: Vec::new(),
                    same_again: false,
                },
                End::Failed,
            )?;
        }
    }
    Ok(Taken::Refused(refusal))
}

fn take(
    tx: &mut dyn Tx,
    mut stored: Stored,
    runner: &RunnerId,
    sealing: &SealingKey,
) -> Result<Taken, RpcError> {
    let id = stored.summary.id.clone();
    let now = now();
    let starting = match stored.summary.status {
        DeploymentStatus::Running | DeploymentStatus::Cancelling
            if stored.summary.runner.as_ref() == Some(runner) =>
        {
            if stored.run.preview.is_some() {
                stored.standing = stored.standing.lose_runner(now);
                return refuse(
                    tx,
                    stored,
                    Refusal::Lost(error::conflict(
                        "This Deployment's runner lost track of it after preparing it, so what \
                         ran is unknown. Start a new Deployment",
                        json!({ "deployment": id }),
                    )),
                );
            }
            stored.standing.renew(now);
            save(tx, &mut stored)?;
            false
        }
        DeploymentStatus::Running | DeploymentStatus::Cancelling => {
            return refuse(tx, stored, Refusal::Unclaimable(owned_elsewhere(&id)));
        }
        DeploymentStatus::WaitingForCi => {
            return refuse(tx, stored, Refusal::Unclaimable(waiting(&id)));
        }
        DeploymentStatus::Superseded => {
            return refuse(tx, stored, Refusal::Unclaimable(superseded(&id)));
        }
        DeploymentStatus::Applied
        | DeploymentStatus::Failed
        | DeploymentStatus::Unknown
        | DeploymentStatus::Cancelled => {
            return refuse(tx, stored, Refusal::Unclaimable(ended(&id)));
        }
        DeploymentStatus::Queued => {
            stored.standing = stored.standing.claim_for(runner, now);
            // A claim-time refusal below ends it from Running.
            stored.show();
            true
        }
    };
    if starting && awaits(tx, &id)? {
        let refused = error::conflict(
            "Nobody approved this redeploy after its builds, so it didn't run. Run ployz \
             deployment redeploy again.",
            json!({ "deployment": id }),
        );
        return refuse(tx, stored, Refusal::Fails(refused));
    }
    let saved = held_revision(tx, &stored)?;
    // An earlier run may have fixed a Volume's storage since this one was admitted.
    if let Err(refused) = crate::volume::check_admitted(tx, &stored.summary.environment_id, &saved)
    {
        if refused.code != ployz_core::RpcErrorCode::Conflict {
            return Err(refused);
        }
        return refuse(tx, stored, Refusal::Fails(refused));
    }
    let Lowered {
        input,
        mut intent,
        warnings,
        ..
    } = match lowered(tx, &stored, &saved) {
        Ok(lowered) => lowered,
        // Every claim lowers it the same way: it fails rather than waits queued,
        // and takes nothing over. A new Deploy may still get past it, since a Branch
        // reference resolves against what its parent has applied since.
        Err(refused) if refused.code == ployz_core::RpcErrorCode::InvalidArgument => {
            let refused = error::conflict(refused.message, refused.details);
            return refuse(tx, stored, Refusal::Fails(refused));
        }
        Err(error) => return Err(error),
    };
    let sources = build::routed(tx, &stored, &saved)?;
    if starting {
        // Every other claimed Deployment of the Environment loses its runner, its runner
        // lease lapsed or not; each keeps its runner column for an Interrupted record.
        let taken_over = tx.query(
            &format!(
                "SELECT id FROM config_deployment WHERE environment_id = ?1 AND {}",
                CLAIMED.as_str()
            ),
            &[stored.summary.environment_id.as_str().into()],
        )?;
        for row in &taken_over {
            let mut taken = load(tx, &row.parse::<DeploymentId>(0, "Deployment")?)?;
            taken.standing = taken.standing.lose_runner(now);
            save(tx, &mut taken)?;
        }
        super::uploads::claimed(tx, &mut stored)?;
        save(tx, &mut stored)?;
    }
    let uploads = build::uploads_of(tx, &stored)?;
    if stored.run.warnings != warnings {
        stored.run.warnings = warnings;
        stored.summary.warnings = stored.run.summary_warnings();
        save(tx, &mut stored)?;
    }
    let credentials = tx.query(
        "SELECT credentials FROM config_deployment WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    let credentials = credentials
        .first()
        .ok_or_else(|| missing(&id))?
        .text(0)
        .and_then(|text| {
            serde_json::from_str(text).map_err(|_| error::corrupt("Deployment credentials"))
        })?;
    intent.registry_auth = registry::unseal(credentials, sealing)?;
    let receipts = receipts(tx, &stored.summary.environment_id)?;
    let deletes = stored
        .nodes
        .iter()
        .filter_map(|node| match node {
            TargetNode::Volume { deletes, .. } => deletes.clone(),
            TargetNode::Service { .. } | TargetNode::Config { .. } => None,
        })
        .flatten()
        .collect();
    let organization = build::organization(tx, &id)?;
    let build_order = crate::builders::order(tx, organization.as_str())?;
    let sealed_names = crate::review::sealed::names(tx, stored.summary.environment_id.as_str())?;
    let ended_runs = ended_runs(tx, &stored.summary)?;
    let approval = device_approval(tx, &id)?;
    let preview = crate::preview_rule::of_deployment(tx, &stored)?;
    // A preview's removal comes off under the rule that made it, when no person signed one.
    let teardown = match teardown(tx, &id)? {
        Some(teardown) => Some(teardown),
        None if stored.summary.remove => preview.clone().map(TeardownApproval::PreviewRule),
        None => None,
    };
    let review = match preview {
        Some(_) => preview_review(tx, &id)?,
        None => None,
    };
    // A person's approval of a commit stands in for the push automation answers to.
    let approvals = match preview {
        Some(_) => BTreeMap::new(),
        None => build::commit_approvals(tx, &id)?,
    };
    let rule = match (&stored.summary.admitted_by, &preview) {
        (None, None) => crate::auto_rule::stored(tx, &stored.summary.environment_id)?,
        (Some(_), None) if !approvals.is_empty() => {
            crate::auto_rule::stored(tx, &stored.summary.environment_id)?
        }
        (Some(_), _) | (None, Some(_)) => None,
    };
    let generation = match (&rule, &preview) {
        (Some(rule), _) => rule.open().ok().map(|rule| rule.generation),
        (None, Some(preview)) => preview.open().ok().map(|preview| preview.generation),
        (None, None) => None,
    };
    let image_tokens = match generation {
        None => BTreeMap::new(),
        // A build that ended before the Store kept its token's terms was built under
        // the rule as it stands, for the push or the review.
        Some(generation) => build::image_tokens(tx, &id, |token| build::ImageToken {
            token,
            image: String::new(),
            generation,
            authority: match &review {
                Some(review) => ployz_core::authority::CommitAuthority::Review(review.digest()),
                None => ployz_core::authority::CommitAuthority::Head,
            },
        })?,
    };
    Ok(Taken::Claimed(Box::new(Claimed {
        deployment: stored.summary,
        intent,
        sealed_names,
        input: json_value(&input),
        receipts,
        sources,
        deletes,
        uploads,
        build_order,
        ended_runs,
        approval,
        rule,
        preview,
        review,
        image_tokens,
        approvals,
        teardown,
    })))
}

/// The Deployments of `claimed`'s Environment, itself aside, that ran and ended.
/// Claiming took over each one still running, so it ended Unknown. One queued or
/// waiting for CI never ran and may yet, so it is not among them.
fn ended_runs(
    tx: &mut dyn Tx,
    claimed: &DeploymentSummary,
) -> Result<BTreeSet<DeploymentId>, RpcError> {
    tx.query(
        &format!(
            "SELECT id FROM config_deployment WHERE environment_id = ?1 AND id <> ?2 AND {}",
            ENDED.as_str()
        ),
        &[
            claimed.environment_id.as_str().into(),
            claimed.id.as_str().into(),
        ],
    )?
    .iter()
    .map(|row| row.parse::<DeploymentId>(0, "Deployment"))
    .collect()
}
