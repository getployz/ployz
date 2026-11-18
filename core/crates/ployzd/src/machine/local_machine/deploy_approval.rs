//! The gate an Operator's Container change passes. A new Container is created only when
//! its spec matches a digest a trusted Device approved, or an automatic deploy rule
//! admits its build. A step on a Container that already exists is served, except a start
//! of a hook: a hook runs once, when it is created.

use std::time::Instant;

use ployz_core::{
    ContainerKind, ContainerLocator, ContainerNow, ContainerWant, RunTarget, Sealing, Start,
    authority::{
        AuthorityRefusal, DeployApproval, DeployFacts, Purpose, RuleProof, Signed, Timestamp,
        deploy::{self, Creation},
    },
};

use super::{Error, LocalMachine};

impl LocalMachine {
    /// Let an Operator's Container change through. A new Container, adoption included,
    /// needs a deploy approval of its spec or an automatic deploy rule's proof of its
    /// build. Holding, routing out, stopping, starting and removing a Container that
    /// already exists need neither, and no Operator starts a hook again.
    ///
    /// # Errors
    /// Returns [`Error::DeployRefused`] or [`Error::RuleGrantRefused`] naming what the
    /// creation lacks, and [`Error::DockerUnavailable`] or [`Error::Docker`] when a
    /// Container to start cannot be read.
    pub(crate) async fn admit_operator_change(
        &self,
        want: &ContainerWant,
        deploy: Option<&Signed<DeployApproval>>,
        rule: Option<&RuleProof>,
    ) -> Result<(), Error> {
        let ContainerWant::Running { container, .. } = want else {
            return Ok(());
        };
        if let RunTarget::Existing(locator) = container {
            return self.refuse_hook_start(locator).await;
        }
        match (deploy, rule) {
            (None, Some(rule)) => self.admit_rule_deploy_online(want, rule).await,
            (deploy, _) => self.admit_operator_creation(want, deploy),
        }
    }

    /// Let an Operator's Writer start through. Starting a Container the Machine already
    /// holds is served, unless it is a hook. Creating one is refused: its spec comes from
    /// the Operator, and no statement approves a spec outside a deploy.
    ///
    /// # Errors
    /// Returns [`Error::DeployRefused`] for a creation and for a hook, and
    /// [`Error::DockerUnavailable`] or [`Error::Docker`] when the Container cannot be read.
    pub(crate) async fn admit_operator_volume_start(&self, start: &Start) -> Result<(), Error> {
        match start {
            Start::Kept { container_id } => {
                self.refuse_hook_start(&ContainerLocator::Id(*container_id))
                    .await
            }
            Start::Create { resolved_spec, .. } => {
                tracing::warn!(
                    "Volume start refused for {}: it creates a Container no deploy approved",
                    resolved_spec.name,
                );
                Err(Error::DeployRefused(AuthorityRefusal::Mismatch {
                    field: "a Volume start",
                }))
            }
        }
    }

    /// A hook runs once, when it is created: no Operator starts one again.
    async fn refuse_hook_start(&self, locator: &ContainerLocator) -> Result<(), Error> {
        match self.container_now(locator).await? {
            ContainerNow::Present(observed) if observed.kind == ContainerKind::PreDeployHook => {
                Err(Error::DeployRefused(AuthorityRefusal::Mismatch {
                    field: "a hook run",
                }))
            }
            ContainerNow::Present(_) | ContainerNow::Absent { .. } => Ok(()),
        }
    }

    /// Admit an Operator's new creation only with a deploy approval, inside its window,
    /// whose digest for the slot's Service matches the spec.
    ///
    /// # Errors
    /// Returns [`Error::DeployRefused`] naming what the approval lacks.
    fn admit_operator_creation(
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
                now: Timestamp::now(),
            },
        )
        .map_err(refused)?;
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

    pub(super) fn cluster_key(
        &self,
        phase: ployz_core::LocalMachinePhase,
    ) -> Option<ployz_core::SealingPublicKey> {
        self.held_sealing(phase).and_then(Sealing::held_key)
    }
}

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
        AuthorityRefusal::Mismatch { field: "a hook" } => {
            "The device approved no pre-deploy command for this service."
        }
        AuthorityRefusal::Mismatch {
            field: "a hook run",
        } => "This deploy already ran its pre-deploy command here.",
        AuthorityRefusal::Mismatch {
            field: "a Volume start",
        } => {
            "This server starts a service on a moved or restored Volume only from a deploy an approved device signed."
        }
        AuthorityRefusal::Mismatch {
            field: "the source Server",
        } => "This server copies a volume's data only from another server of its cluster.",
        AuthorityRefusal::Mismatch {
            field: "an Environment",
        } => "The device approved a deploy to another environment.",
        AuthorityRefusal::Mismatch { field: "Machines" } => {
            "The device approved this deploy for other servers, before this one joined."
        }
        AuthorityRefusal::Mismatch { field: "a Cluster" } => {
            "The device approved a deploy to another cluster."
        }
        AuthorityRefusal::Expired => "The device's approval expired. Deploy again.",
        AuthorityRefusal::RetiredBuilder { .. } => {
            "This automatic deploy rule pins a builder Ployz no longer trusts. Approve the rule again to pin the current builder."
        }
        AuthorityRefusal::UnknownKey { .. } | AuthorityRefusal::UnknownPasskey => {
            "This server doesn't trust the device that approved this deploy."
        }
        AuthorityRefusal::Malformed { .. }
        | AuthorityRefusal::BadSignature { .. }
        | AuthorityRefusal::ApproveOnly { .. }
        | AuthorityRefusal::Mismatch { .. }
        | AuthorityRefusal::NoClusterIdentity
        | AuthorityRefusal::ProofReplayed => {
            return format!("This server can't check the deploy's approval: {refusal}.");
        }
    };
    text.to_owned()
}
