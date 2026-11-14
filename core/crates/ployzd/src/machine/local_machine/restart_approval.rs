//! Device-signed restart approvals: the switch that requires them, the gate an Operator
//! caller's restart step passes under it, and the record of steps already taken. A new
//! creation, or a step carrying only a deploy approval, goes to `deploy_approval`.

use ployz_core::{
    ContainerId, ContainerLocator, ContainerNow, ContainerWant, DeviceEnforcementSet,
    ManagementScope, RunTarget, Sealing, SetDeviceEnforcementRequest, SetPrivateAccessRequest,
    authority::{
        AuthorityRefusal, DeployApproval, PrivateAccessFacts, Purpose, RestartApproval,
        RestartFacts, RestartStep, Signed, Timestamp, private_access, restart,
    },
};

use super::{Error, LocalMachine};

mod consumption;

pub(crate) use consumption::{ConsumptionLog, JoinReply, JoinResult};

impl LocalMachine {
    /// Activate this Machine under a person's starting set, at Full scope only (see
    /// [`LocalMachine::activate`]). There is no off.
    ///
    /// # Errors
    /// Returns [`Error::NeedsFullScope`] for an Operator caller,
    /// [`Error::EnforcementRefused`] for a missing starting set, [`Error::ActivationRefused`]
    /// for off and for a set activation refuses, and what [`LocalMachine::activate`] returns.
    pub async fn set_device_enforcement(
        &self,
        request: SetDeviceEnforcementRequest,
    ) -> Result<DeviceEnforcementSet, Error> {
        let record = self.record();
        if self
            .management_client
            .is_some_and(|remote| record.management_scope(&remote) != Some(ManagementScope::Full))
        {
            return Err(Error::NeedsFullScope);
        }
        let capability = ployz_core::SET_DEVICE_ENFORCEMENT_CAPABILITY;
        match (request.enforced, request.starting_set) {
            (true, Some(signed)) => self.activate(signed, capability).await,
            (true, None) => Err(Error::EnforcementRefused(AuthorityRefusal::Unsigned {
                purpose: Purpose::Enforce,
            })),
            (false, _) => Err(Error::ActivationRefused {
                capability,
                reason: super::activation::Refusal::NoOff,
            }),
        }
    }

    /// An Operator's `SetPrivateAccess` turns Private Access on only with an approval, by a
    /// trusted Device or a trusted passkey's own prompt, of the sealed auth key it carries.
    /// The key decides which tailnet reaches the whole Cluster network. Turning off needs
    /// none, and neither does a Full caller.
    ///
    /// # Errors
    /// Returns [`Error::PrivateAccessRefused`] naming what the approval lacks.
    pub(crate) fn admit_operator_private_access(
        &self,
        request: &SetPrivateAccessRequest,
    ) -> Result<(), Error> {
        let record = self.record();
        if !request.enabled {
            return Ok(());
        }
        let approval = request
            .approval
            .as_ref()
            .ok_or(Error::PrivateAccessRefused(AuthorityRefusal::Unsigned {
                purpose: Purpose::Workflow,
            }))?;
        let sealed = request
            .sealed_auth_key
            .as_ref()
            .ok_or(Error::PrivateAccessKeyMissing)?;
        let cluster = self
            .held_sealing(record.phase())
            .and_then(Sealing::held_key);
        let approver = private_access::verify(
            approval,
            &PrivateAccessFacts {
                cluster,
                trusted: &record.trusted_keys(),
                sealed,
                now: Timestamp::now(),
            },
        )
        .map_err(Error::PrivateAccessRefused)?;
        tracing::info!(%approver, "Private Access approved");
        Ok(())
    }

    /// Let an Operator caller's restart step through only with
    /// an approved Device's approval of exactly this restart, or a browser key's under a
    /// trusted passkey's session grant that covers it, and record the step so the
    /// same approval never runs it again. A new creation, adoption included, needs a deploy
    /// approval of its spec, and a step that carries only a deploy approval is checked
    /// against it. A change carrying only an automatic deploy rule's proof is checked
    /// against the rule. Returns a want pinned to the verified Container ID. An absent
    /// target is refused.
    ///
    /// # Errors
    /// Returns [`Error::RestartRefused`], [`Error::DeployRefused`] or
    /// [`Error::RuleDeployRefused`] naming what the approval lacks, [`Error::ApprovalRecord`] when the record of taken steps cannot be
    /// read or written, and [`Error::DockerUnavailable`] or [`Error::Docker`] when the
    /// Container cannot be read.
    pub async fn admit_operator_restart(
        &self,
        want: ContainerWant,
        approval: Option<&Signed<RestartApproval>>,
        deploy: Option<&Signed<DeployApproval>>,
        rule: Option<&ployz_core::authority::RuleProof>,
        preview: Option<&ployz_core::authority::PreviewProof>,
    ) -> Result<ContainerWant, Error> {
        let record = self.record();
        match (approval, deploy, rule, preview) {
            (None, None, Some(rule), None) => {
                return self.admit_rule_deploy_online(want, rule).await;
            }
            (None, None, None, Some(preview)) => {
                return self.admit_preview_deploy_online(want, preview).await;
            }
            _ => {}
        }
        let Some((step, locator)) = RestartStep::of(&want) else {
            self.admit_operator_creation(&want, deploy).await?;
            return Ok(want);
        };
        if let (None, Some(deploy)) = (approval, deploy) {
            return self.admit_deploy_step(want, deploy).await;
        }
        let approval = approval.ok_or(AuthorityRefusal::Unsigned {
            purpose: Purpose::Workflow,
        })?;
        let ContainerNow::Present(observed) = self.container_now(locator).await? else {
            return Err(AuthorityRefusal::Mismatch {
                field: "a Container",
            }
            .into());
        };
        let trusted = record.trusted_keys();
        let identity = observed.identity();
        let machine = record.id();
        let now = Timestamp::now();
        let cluster = self
            .held_sealing(record.phase())
            .and_then(Sealing::held_key);
        let approved = restart::verify(
            approval,
            &RestartFacts {
                cluster,
                machine: &machine,
                trusted: &trusted,
                namespace: &identity.namespace,
                service: &identity.name,
                container: &observed.container_id,
                kind: observed.kind,
                spec: &observed.resolved_spec,
                step,
                now,
            },
        )
        .map_err(|refusal| {
            if refusal
                == (AuthorityRefusal::Mismatch {
                    field: ployz_core::authority::deploy::HOST_ACCESS,
                })
            {
                Error::RestartReachesHost
            } else {
                Error::RestartRefused(refusal)
            }
        })?;
        self.approvals
            .consume(&approved.body, &observed.container_id, step, now)?;
        tracing::info!(
            operation = %approved.body.operation,
            proposal = ?approved.body.proposal,
            ?step,
            service = %identity,
            container = observed.container_id.as_str().get(..12).unwrap_or_default(),
            "{} approved by {}",
            approved.body.kind,
            approved.by
        );
        Ok(pinned(want, observed.container_id))
    }
}

/// `want` aimed at `container` by its ID, so the step acts on the Container verified.
pub(super) fn pinned(mut want: ContainerWant, container: ContainerId) -> ContainerWant {
    let locator = match &mut want {
        ContainerWant::Held { container, .. }
        | ContainerWant::Out { container, .. }
        | ContainerWant::Stopped { container, .. }
        | ContainerWant::Gone { container, .. }
        | ContainerWant::Running {
            container: RunTarget::Existing(container),
            ..
        } => container,
        ContainerWant::Running {
            container: RunTarget::New(_),
            ..
        } => unreachable!("a new creation has no Container to pin"),
    };
    *locator = ContainerLocator::Id(container);
    want
}
