//! Deploy approvals under Device enforcement: an Operator's new Container is created only
//! when its spec matches a digest a trusted key approved, and the approval also admits the
//! Deploy's steps on Containers in its Namespace.

use std::time::Instant;

use ployz_core::{
    ContainerKind, ContainerNow, ContainerWant, RunTarget, Sealing,
    authority::{
        AuthorityRefusal, DeployApproval, DeployFacts, Purpose, RestartStep, Signed, Timestamp,
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
        let approval = approval.ok_or(Error::DeployRefused(AuthorityRefusal::Unsigned {
            purpose: Purpose::Workflow,
        }))?;
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
        .map_err(Error::DeployRefused)?;
        if new.slot.kind == ContainerKind::PreDeployHook {
            self.approvals
                .consume_hook(&approved.body, &new.slot.service, now)?;
        }
        tracing::info!(
            kind = %new.slot.kind,
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
    /// Machine it names, inside its window. Nothing is consumed: the step changes which
    /// approved copy runs, never what runs. Returns the want pinned to the Container seen.
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
        let approved = deploy::verify(
            approval,
            &DeployFacts {
                cluster: self.cluster_key(record.phase()),
                machine: &machine,
                trusted: &trusted,
                namespace: &identity.namespace,
                creation: None,
                now: Timestamp::now(),
            },
        )
        .map_err(Error::DeployRefused)?;
        tracing::info!(
            ?step,
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
