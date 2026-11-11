//! Deploy approvals under Device enforcement: an Operator's new Container is created only
//! when its spec matches a digest a trusted key approved, and the approval also admits the
//! Deploy's steps on Containers in its Namespace.

use std::time::Instant;

use ployz_core::{
    ContainerKind, ContainerNow, ContainerWant, ResolvedServiceSpec, RunTarget, Sealing,
    ServiceName,
    authority::{
        AuthorityRefusal, DeployApproval, DeployApproved, DeployFacts, Purpose, RestartStep,
        Signed, Timestamp,
        deploy::{self, Creation},
    },
};

use super::{Error, LocalMachine, restart_approval::pinned};

impl LocalMachine {
    /// Admit an Operator's new creation under enforcement only with a deploy approval whose
    /// digest for the slot's Service matches the spec, and take a hook's place in the
    /// consumption log so the same approval never runs that Service's hook twice. A
    /// Service Container is not consumed: retries and replicas repeat it.
    ///
    /// # Errors
    /// Returns [`Error::DeployRefused`] naming what the approval lacks, and
    /// [`Error::ApprovalRecord`] when the consumption log cannot be written.
    pub(super) fn admit_operator_creation(
        &self,
        want: &ContainerWant,
        approval: Option<&Signed<DeployApproval>>,
    ) -> Result<(), Error> {
        let started = Instant::now();
        let ContainerWant::Running {
            container: RunTarget::New(new),
            ..
        } = want
        else {
            unreachable!("only a new creation has no lifecycle step");
        };
        let refused = |refusal: AuthorityRefusal| {
            tracing::warn!(
                kind = %new.slot.kind,
                "deploy refused for {}: {refusal}",
                new.slot.service,
            );
            Error::DeployRefused(refusal)
        };
        let approval = approval.ok_or_else(|| {
            refused(AuthorityRefusal::Unsigned {
                purpose: Purpose::Workflow,
            })
        })?;
        let record = self.record();
        let trusted = record.trusted_keys();
        let machine = record.id();
        let now = Timestamp::now();
        let approved = deploy::verify(
            approval,
            &DeployFacts {
                cluster: self.cluster_key(record.phase()),
                machine: &machine,
                trusted: &trusted,
                namespace: &new.slot.namespace,
                creation: Some(Creation {
                    service: &new.slot.service,
                    kind: new.slot.kind,
                    spec: &new.resolved_spec,
                }),
                now,
            },
        )
        .map_err(refused)?;
        admit_sealed_scope(&approved, &new.slot.service, &new.resolved_spec)?;
        if new.slot.kind == ContainerKind::PreDeployHook {
            self.approvals.consume_hook(
                &approved.body,
                (&new.slot.service, &new.slot.creation_key),
                now,
            )?;
        }
        tracing::info!(
            kind = %new.slot.kind,
            proposal = ?approved.body.proposal,
            admission_us = started.elapsed().as_micros(),
            "deploy {} approved by {} for {} digest={}",
            approved.body.operation,
            approved.by,
            new.slot.service,
            approved.digest.map(|digest| digest.short()).unwrap_or_default(),
        );
        Ok(())
    }

    /// Admit a Deploy's step on an existing Container in the approval's Namespace, on a
    /// Machine it names, inside its window. Hold, Out, Stop and Remove act on any copy, so
    /// a Deploy can retire old ones; Start needs the copy's own spec approved, and never
    /// starts a hook. Nothing is consumed. Returns the want pinned to the Container seen.
    ///
    /// # Errors
    /// Returns [`Error::DeployRefused`] naming what the approval lacks, and
    /// [`Error::DockerUnavailable`] or [`Error::Docker`] when the Container cannot be read.
    pub(super) async fn admit_deploy_step(
        &self,
        want: ContainerWant,
        approval: &Signed<DeployApproval>,
    ) -> Result<ContainerWant, Error> {
        let Some((step, locator)) = RestartStep::of(&want) else {
            unreachable!("a step acts on an existing Container");
        };
        let ContainerNow::Present(observed) = self.container_now(locator).await? else {
            return Err(Error::DeployRefused(AuthorityRefusal::Mismatch {
                field: "a Container",
            }));
        };
        let record = self.record();
        let trusted = record.trusted_keys();
        let machine = record.id();
        let identity = observed.identity();
        let refused = |refusal: AuthorityRefusal| {
            tracing::warn!(?step, "deploy step refused for {identity}: {refusal}");
            Error::DeployRefused(refusal)
        };
        // Starting a copy runs its spec, so it must be one the approval names; a copy
        // made before enforcement, or for another Service, is not. A hook runs once,
        // when it is created, so no deploy starts one again.
        let starts = step == RestartStep::Start;
        if starts && observed.kind == ContainerKind::PreDeployHook {
            return Err(refused(AuthorityRefusal::Mismatch {
                field: "a hook run",
            }));
        }
        let approved = deploy::verify(
            approval,
            &DeployFacts {
                cluster: self.cluster_key(record.phase()),
                machine: &machine,
                trusted: &trusted,
                namespace: &identity.namespace,
                creation: starts.then_some(Creation {
                    service: &identity.name,
                    kind: observed.kind,
                    spec: &observed.resolved_spec,
                }),
                now: Timestamp::now(),
            },
        )
        .map_err(refused)?;
        tracing::info!(
            ?step,
            proposal = ?approved.body.proposal,
            container = observed.container_id.as_str().get(..12).unwrap_or_default(),
            "deploy {} step approved by {} for {}",
            approved.body.operation,
            approved.by,
            identity,
        );
        Ok(pinned(want, observed.container_id))
    }

    fn cluster_key(
        &self,
        phase: ployz_core::LocalMachinePhase,
    ) -> Option<ployz_core::SealingPublicKey> {
        match self.held_sealing(phase) {
            Some(Sealing::Held { public_key }) => Some(public_key),
            _ => None,
        }
    }
}

/// Refuse a creation whose Sealed values its approver may not open, before anything
/// opens them. A start needs no check: the Container opened its values when it was made.
fn admit_sealed_scope(
    approved: &DeployApproved,
    service: &ServiceName,
    spec: &ResolvedServiceSpec,
) -> Result<(), Error> {
    if deploy::sealed_scope_allows(&approved.by, spec) {
        return Ok(());
    }
    tracing::warn!(
        %service,
        "refused deploy {}: Sealed values belong to another Environment",
        approved.body.operation,
    );
    Err(Error::DeployRefused(AuthorityRefusal::Mismatch {
        field: SEALED_VALUES,
    }))
}

const SEALED_VALUES: &str = "Sealed values";

/// What a refused deploy says to the person who ran it.
pub(super) fn refusal_text(refusal: &AuthorityRefusal) -> String {
    let text = match refusal {
        AuthorityRefusal::Unsigned { .. } => {
            "This server runs only deploys an approved device signed."
        }
        AuthorityRefusal::Mismatch { field: "a payload" } => {
            "This service's settings differ from what the device approved."
        }
        AuthorityRefusal::Mismatch { field: "an image" } => {
            "This server runs only images pinned by digest."
        }
        AuthorityRefusal::Mismatch { field: "a Service" } => {
            "The device didn't approve this service."
        }
        AuthorityRefusal::Mismatch {
            field: SEALED_VALUES,
        } => {
            "This service's sealed values belong to another environment, so a passkey approval can't open them."
        }
        AuthorityRefusal::Mismatch { field: "a hook" } => {
            "The device approved no pre-deploy command for this service."
        }
        AuthorityRefusal::Mismatch {
            field: "a hook run",
        }
        | AuthorityRefusal::Replayed { .. } => {
            "This deploy already ran its pre-deploy command here."
        }
        AuthorityRefusal::Mismatch {
            field: "an Environment",
        } => "The device approved a deploy to another environment.",
        AuthorityRefusal::Mismatch { field: "Machines" } => {
            "The device approved this deploy for other servers, before this one joined."
        }
        AuthorityRefusal::Mismatch { field: "a Cluster" } => {
            "The device approved a deploy to another cluster."
        }
        AuthorityRefusal::Mismatch {
            field: "a Container",
        } => "That container isn't on this server.",
        AuthorityRefusal::Expired => "The device's approval expired. Deploy again.",
        AuthorityRefusal::UnknownKey { .. } | AuthorityRefusal::UnknownPasskey => {
            "This server doesn't trust the device that approved this deploy."
        }
        AuthorityRefusal::Malformed { .. }
        | AuthorityRefusal::BadSignature { .. }
        | AuthorityRefusal::ApproveOnly { .. }
        | AuthorityRefusal::Mismatch { .. }
        | AuthorityRefusal::NoClusterIdentity
        | AuthorityRefusal::ProofReplayed
        | AuthorityRefusal::ForeignMachineKey { .. }
        | AuthorityRefusal::BadMachineSignature { .. }
        | AuthorityRefusal::MalformedMachineStatement { .. } => {
            return format!("This server can't check the deploy's approval: {refusal}.");
        }
    };
    text.to_owned()
}
