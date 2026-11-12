//! What a Deploy hands the Client it runs through: the log it writes under, the pull
//! credentials of its Services, and the approvals every Server gets with a Container change.

use std::{collections::BTreeMap, sync::Arc};

use ployz_core::{
    ContainerLocator, ContainerWant, DeploymentLogId, RegistryAuth, RunTarget, ServiceName,
    authority::{
        DeployApproval, PreviewProof, RestartApproval, RuleProof, Signed, TeardownApproval,
    },
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
    /// The Device-signed approval of a restart step.
    restart: Option<Signed<RestartApproval>>,
    /// The approval of the Deploy every Container this client creates or steps belongs to.
    deploy: Option<Signed<DeployApproval>>,
    /// A rule's proof for each Service it built, when a rule deploys in place of a
    /// person's approval.
    proofs: RuleProofs,
    /// The teardown that takes what the Deploy removes off each Server: its Containers,
    /// when nothing else approves the step, and the Volumes it deletes.
    teardown: Option<TeardownApproval>,
}

/// The proof each Service a rule built carries to every Server, in place of a person's
/// deploy approval: an automatic deploy rule's, or a preview rule's.
#[derive(Clone, Debug, Default)]
pub struct RuleProofs {
    pub rules: BTreeMap<ServiceName, RuleProof>,
    pub previews: BTreeMap<ServiceName, PreviewProof>,
}

/// What a Container change carries to the Server: the approvals and the proofs for `want`.
pub(super) struct Carried {
    pub(super) rule: Option<Box<RuleProof>>,
    pub(super) preview: Option<Box<PreviewProof>>,
    pub(super) restart: Option<Signed<RestartApproval>>,
    pub(super) deploy: Option<Signed<DeployApproval>>,
    pub(super) teardown: Option<Box<TeardownApproval>>,
}

impl DeployContext {
    pub(super) fn set_restart(&mut self, approval: Option<Signed<RestartApproval>>) {
        Arc::make_mut(&mut self.approvals).restart = approval;
    }

    pub(super) fn set_deploy(&mut self, approval: Option<Signed<DeployApproval>>) {
        Arc::make_mut(&mut self.approvals).deploy = approval;
    }

    pub(super) fn set_proofs(&mut self, proofs: RuleProofs) {
        Arc::make_mut(&mut self.approvals).proofs = proofs;
    }

    pub(super) fn set_teardown(&mut self, teardown: Option<TeardownApproval>) {
        Arc::make_mut(&mut self.approvals).teardown = teardown;
    }

    /// The teardown a Volume this client deletes carries.
    pub(crate) fn teardown(&self) -> Option<Box<TeardownApproval>> {
        self.approvals.teardown.clone().map(Box::new)
    }

    /// What a change of `want` carries.
    pub(super) fn carried(&self, want: &ContainerWant) -> Carried {
        Carried {
            rule: proof_for(&self.approvals.proofs.rules, want),
            preview: proof_for(&self.approvals.proofs.previews, want),
            restart: self.approvals.restart.clone(),
            deploy: self.approvals.deploy.clone(),
            teardown: self.teardown(),
        }
    }
}

/// The proof in `proofs` a change of `want` carries: its Service's, or for a step on a
/// Container named only by ID, any of the Deploy's, since such a step needs only the
/// rule's Environment.
fn proof_for<P: Clone>(proofs: &BTreeMap<ServiceName, P>, want: &ContainerWant) -> Option<Box<P>> {
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
