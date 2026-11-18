//! What a Deploy hands the Client it runs through: the log it writes under, the pull
//! credentials of its Services, and the approvals every Server gets with a Container change.

use std::{collections::BTreeMap, sync::Arc};

use ployz_core::{
    ContainerLocator, ContainerWant, DeploymentLogId, RegistryAuth, RunTarget, ServiceName,
    authority::{DeployApproval, RuleProof, Signed},
};

/// The Deploy this client executes, shared by its clones as their Deploy's own.
#[derive(Clone, Default)]
pub(crate) struct DeployContext {
    /// The Deploy's log; a replacement joining a named Deploy keeps that Deploy's label.
    pub(crate) id: Option<DeploymentLogId>,
    /// Pull credentials by Service.
    pub(crate) registry_auth: BTreeMap<ServiceName, RegistryAuth>,
    /// What every Container change this client sends carries.
    approvals: Arc<Approvals>,
}

/// The approvals a client hands each Server with a Container change.
#[derive(Clone, Debug, Default)]
struct Approvals {
    /// The approval of the Deploy every Container this client creates or steps belongs to.
    deploy: Option<Signed<DeployApproval>>,
    /// A rule's proof for each Service it built, when a rule deploys in place of a
    /// person's approval.
    proofs: RuleProofs,
}

/// The proof each Service an automatic deploy rule built carries to every Server, in
/// place of a person's deploy approval.
#[derive(Clone, Debug, Default)]
pub struct RuleProofs {
    pub rules: BTreeMap<ServiceName, RuleProof>,
}

/// What a Container change carries to the Server: the approvals and the proofs for `want`.
pub(super) struct Carried {
    pub(super) rule: Option<Box<RuleProof>>,
    pub(super) deploy: Option<Signed<DeployApproval>>,
}

impl Carried {
    pub(super) fn none() -> Self {
        Self {
            rule: None,
            deploy: None,
        }
    }
}

impl DeployContext {
    pub(super) fn set_deploy(&mut self, approval: Option<Signed<DeployApproval>>) {
        Arc::make_mut(&mut self.approvals).deploy = approval;
    }

    pub(super) fn set_proofs(&mut self, proofs: RuleProofs) {
        Arc::make_mut(&mut self.approvals).proofs = proofs;
    }

    /// What a change of `want` carries.
    pub(super) fn carried(&self, want: &ContainerWant) -> Carried {
        Carried {
            rule: proof_for(&self.approvals.proofs.rules, want),
            deploy: self.approvals.deploy.clone(),
        }
    }
}

/// The proof in `proofs` a change of `want` carries: its Service's, or for a step on a
/// Container named only by ID, any of the Deploy's, since such a step needs only the
/// rule's Environment.
fn proof_for(
    proofs: &BTreeMap<ServiceName, RuleProof>,
    want: &ContainerWant,
) -> Option<Box<RuleProof>> {
    let service = match want {
        ContainerWant::Running {
            container: RunTarget::New(new),
            ..
        } => Some(&new.slot.service),
        ContainerWant::Held { container, .. }
        | ContainerWant::Out { container, .. }
        | ContainerWant::Stopped { container, .. }
        | ContainerWant::Gone { container, .. }
        | ContainerWant::Running {
            container: RunTarget::Existing(container),
            ..
        } => match container {
            ContainerLocator::Key(slot) => Some(&slot.service),
            ContainerLocator::Id(_) => None,
        },
    };
    service
        .and_then(|service| proofs.get(service))
        .or_else(|| proofs.values().next())
        .cloned()
        .map(Box::new)
}
