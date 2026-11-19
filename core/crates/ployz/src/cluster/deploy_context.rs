//! What a Deploy hands the Client it runs through: the log it writes under, the pull
//! credentials of its Services, and the approvals every Server gets with a Container change.

use std::{collections::BTreeMap, sync::Arc};

use ployz_core::{
    ContainerLocator, ContainerWant, DeploymentLogId, RegistryAuth, RunTarget, ServiceName,
    authority::{DeployApproval, RuleProof, ServiceVersion, Signed, version::MAX_PRESENTED},
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
    /// The signed versions of each Service this Deploy creates.
    versions: BTreeMap<ServiceName, Vec<Signed<ServiceVersion>>>,
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
    pub(super) versions: Vec<Signed<ServiceVersion>>,
}

impl Carried {
    pub(super) fn none() -> Self {
        Self {
            rule: None,
            deploy: None,
            versions: Vec::new(),
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

    /// Hand each Service's creations the versions signed for it, newest last.
    pub(super) fn set_versions(&mut self, versions: Vec<Signed<ServiceVersion>>) {
        let mut by_service: BTreeMap<ServiceName, Vec<_>> = BTreeMap::new();
        for version in versions {
            if let Ok(body) = version.open() {
                by_service.entry(body.service).or_default().push(version);
            }
        }
        Arc::make_mut(&mut self.approvals).versions = by_service;
    }

    /// What a change of `want` carries. A new creation carries its Service's newest
    /// versions; a step on an existing Container carries none.
    pub(super) fn carried(&self, want: &ContainerWant) -> Carried {
        let versions = match want {
            ContainerWant::Running {
                container: RunTarget::New(new),
                ..
            } => self
                .approvals
                .versions
                .get(&new.slot.service)
                .map(|versions| {
                    let skip = versions.len().saturating_sub(MAX_PRESENTED);
                    versions.iter().skip(skip).cloned().collect()
                })
                .unwrap_or_default(),
            ContainerWant::Running { .. }
            | ContainerWant::Held { .. }
            | ContainerWant::Out { .. }
            | ContainerWant::Stopped { .. }
            | ContainerWant::Gone { .. } => Vec::new(),
        };
        Carried {
            rule: proof_for(&self.approvals.proofs.rules, want),
            deploy: self.approvals.deploy.clone(),
            versions,
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
