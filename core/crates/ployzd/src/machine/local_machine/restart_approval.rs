//! Device-signed restart approvals: the switch that requires them, the gate an Operator
//! caller's restart step passes under it, and the record of steps already taken. A new
//! creation, or a step carrying only a deploy approval, goes to `deploy_approval`.

use ployz_core::{
    ContainerId, ContainerLocator, ContainerNow, ContainerWant, DeviceEnforcementSet, DeviceKey,
    LocalMachinePhase, ManagementScope, RunTarget, Sealing, SetDeviceEnforcementRequest,
    SetPrivateAccessRequest,
    authority::{
        AuthorityRefusal, DeployApproval, PrivateAccessFacts, Purpose, RestartApproval,
        RestartFacts, RestartStep, Signed, Timestamp, private_access, restart, starting_set,
    },
};

use super::{Error, LocalMachine};

mod consumption;

pub(crate) use consumption::{ConsumptionLog, JoinReply, JoinResult};

impl LocalMachine {
    /// Turn Device enforcement on or off under mutation admission, at Full scope only.
    /// Turning it on trusts only the Members of a Device-signed starting set that names
    /// this Machine, used once.
    ///
    /// # Errors
    /// Returns [`Error::NeedsFullScope`] for an Operator caller, [`Error::NotParticipating`]
    /// when this Machine is not participating, [`Error::EnforcementRefused`] for a missing,
    /// untrusted, expired or reused starting set, admission failures,
    /// [`Error::RecordOwner`] when the record owner has stopped, and [`Error::Store`] when
    /// the record cannot be written.
    pub async fn set_device_enforcement(
        &self,
        request: SetDeviceEnforcementRequest,
    ) -> Result<DeviceEnforcementSet, Error> {
        let client = self.management_client;
        let approvals = self.approvals.clone();
        let owner = self.owner.clone();
        self.finish_mutation(async move {
            owner
                .mutate(move |store| {
                    let record = store.record();
                    if record.phase() != LocalMachinePhase::Participating {
                        return Err(Error::NotParticipating);
                    }
                    if client.is_some_and(|remote| {
                        record.management_scope(&remote) != Some(ManagementScope::Full)
                    }) {
                        return Err(Error::NeedsFullScope);
                    }
                    let starting = match (request.enforced, &request.starting_set) {
                        (false, _) => Vec::new(),
                        (true, None) => {
                            return Err(Error::EnforcementRefused(AuthorityRefusal::Unsigned {
                                purpose: Purpose::Enforce,
                            }));
                        }
                        (true, Some(signed)) => {
                            let own = record.machine().ok_or(Error::NotParticipating)?.into();
                            let devices: Vec<DeviceKey> = record.devices().collect();
                            let now = Timestamp::now();
                            let body = starting_set::verify(signed, &devices, &own, now)
                                .map_err(Error::EnforcementRefused)?;
                            approvals.consume_starting_set(&body, now)?;
                            body.members
                        }
                    };
                    store.set_device_enforcement(request.enforced, starting)?;
                    Ok(DeviceEnforcementSet {
                        enforced: request.enforced,
                    })
                })
                .await?
        })
        .await
    }

    /// Under Device enforcement an Operator's `SetPrivateAccess` turns Private Access on
    /// only with an approval, by a trusted Device or a trusted passkey's own prompt, of the
    /// sealed auth key it carries. The key decides which tailnet reaches the whole Cluster
    /// network. Turning off needs none, and neither does a Full caller or a Machine not
    /// enforcing.
    ///
    /// # Errors
    /// Returns [`Error::PrivateAccessRefused`] naming what the approval lacks.
    pub(crate) fn admit_operator_private_access(
        &self,
        request: &SetPrivateAccessRequest,
    ) -> Result<(), Error> {
        let record = self.record();
        if !record.device_enforcement() || !request.enabled {
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
        let cluster = match self.held_sealing(record.phase()) {
            Some(Sealing::Held { public_key }) => Some(public_key),
            _ => None,
        };
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

    /// Under Device enforcement an Operator caller's `Build` or `MintBuildGrant` is
    /// refused before anything opens. A Build runs recipe steps of the caller's choosing
    /// with Sealed values opened, and a Build Grant hands them to a key the caller names,
    /// so neither is Cloud's to start. `capability` is the RPC refused. A Full caller and a
    /// Machine not enforcing pass untouched.
    ///
    /// # Errors
    /// Returns [`Error::BuildRefused`] naming `capability` under enforcement.
    pub(crate) fn admit_operator_build(&self, capability: &'static str) -> Result<(), Error> {
        if self.record().device_enforcement() {
            return Err(Error::BuildRefused { capability });
        }
        Ok(())
    }

    /// Under Device enforcement, let an Operator caller's restart step through only with
    /// an approved Device's approval of exactly this restart, or a browser key's under a
    /// trusted passkey's session grant that covers it, and record the step so the
    /// same approval never runs it again. A new creation, adoption included, needs a deploy
    /// approval of its spec, and a step that carries only a deploy approval is checked
    /// against it. A change carrying only an automatic deploy rule's proof is checked
    /// against the rule. Returns a want pinned to the verified Container ID. An absent
    /// target is refused. A Machine not enforcing passes untouched.
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
        if !record.device_enforcement() {
            return Ok(want);
        }
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
            self.admit_operator_creation(&want, deploy)?;
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
        let cluster = match self.held_sealing(record.phase()) {
            Some(Sealing::Held { public_key }) => Some(public_key),
            _ => None,
        };
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
                step,
                now,
            },
        )?;
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
