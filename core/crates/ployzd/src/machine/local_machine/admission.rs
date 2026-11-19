//! Which specs this Machine makes a Container from. A spec that reaches data needs a
//! signature this Machine checks itself: a deploy approval a trusted key signed, an
//! automatic deploy rule's proof of its build, or a caller at Full access. A spec that
//! reaches no data runs for any caller. A step on a Container that already exists is
//! served, except a start of a hook: a hook runs once, when it is created.

use ployz_core::{
    ClientKey, ContainerKind, ContainerLocator, ContainerNow, ContainerWant, Namespace,
    NewContainer, ResolvedServiceSpec, RunTarget, Sealing, Start,
    authority::{
        AuthorityRefusal, DeployApproval, DeployFacts, PayloadDigest, RuleProof, Signed,
        deploy::{self, Creation, content_addressed},
        needs_signature,
    },
};

use sha2::{Digest as _, Sha256};

use super::{Caller, Error, LocalMachine, consumption::SpecSigner};

/// A spec this Machine admitted for a new Container, and whether a signature stands
/// behind it. A Container opens Sealed values, and runs an image this Machine did not
/// pull itself with no login, only under a signed one.
///
/// Only [`LocalMachine::admit_spec`] and [`LocalMachine::admit_volume_start`] make one
/// for a caller's spec.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AdmittedSpec {
    /// A hash of the namespace and the whole spec. Every spec has one, where a spec
    /// holding a negative number has no [`PayloadDigest`].
    spec: [u8; 32],
    signed: bool,
}

impl AdmittedSpec {
    fn of(
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
        signed: bool,
    ) -> Result<Self, AuthorityRefusal> {
        let body = serde_json::to_vec(&(namespace, spec))
            .map_err(|_| AuthorityRefusal::Mismatch { field: "a payload" })?;
        Ok(Self {
            spec: Sha256::digest(&body).into(),
            signed,
        })
    }

    /// A reserved spec this daemon built itself, which no caller chose.
    ///
    /// # Errors
    /// Returns [`Error::DeployRefused`] when the spec cannot be written as JSON.
    pub(super) fn own(namespace: &Namespace, spec: &ResolvedServiceSpec) -> Result<Self, Error> {
        Self::of(namespace, spec, true).map_err(Error::DeployRefused)
    }

    /// Whether a signature stands behind the spec.
    pub(crate) fn signed(self) -> bool {
        self.signed
    }

    /// Whether this is the admission of `spec` in `namespace`.
    pub(crate) fn covers(self, namespace: &Namespace, spec: &ResolvedServiceSpec) -> bool {
        Self::of(namespace, spec, self.signed) == Ok(self)
    }

    #[cfg(test)]
    pub(crate) fn test_signed(namespace: &Namespace, spec: &ResolvedServiceSpec) -> Self {
        Self::of(namespace, spec, true).unwrap()
    }

    /// An admission of no spec in particular, for a test that opens values directly.
    #[cfg(test)]
    pub(crate) fn test_unbound(signed: bool) -> Self {
        Self {
            spec: [0; 32],
            signed,
        }
    }
}

/// What a caller brought with a Container change.
#[derive(Clone, Copy)]
pub(crate) enum Presented<'a> {
    /// A call at Full access: this Server's own socket, or a Device's own connection.
    FullAccess {
        deploy: Option<&'a Signed<DeployApproval>>,
    },
    /// An Operator's call, with the statements it carries.
    Operator {
        deploy: Option<&'a Signed<DeployApproval>>,
        rule: Option<&'a RuleProof>,
    },
}

impl LocalMachine {
    /// Decide whether `want` may make its Container. `None` is the answer for a want that
    /// makes none. A hook that already exists is never started again for an Operator.
    ///
    /// A spec this Machine admitted signed before, for the same Service, is admitted again
    /// with nothing presented: a restart, a scale or a rollback needs no new signature.
    ///
    /// # Errors
    /// Returns [`Error::DeployRefused`] or [`Error::RuleDeployRefused`] naming what the
    /// creation lacks, [`Error::ApprovalRecord`] when the record cannot be read or
    /// written, and [`Error::DockerUnavailable`] or [`Error::Docker`] when a Container to
    /// start cannot be read.
    pub(crate) async fn admit_spec(
        &self,
        want: &ContainerWant,
        presented: Presented<'_>,
    ) -> Result<Option<AdmittedSpec>, Error> {
        let ContainerWant::Running { container, .. } = want else {
            return Ok(None);
        };
        let new = match container {
            RunTarget::New(new) => new,
            RunTarget::Existing(locator) => {
                if matches!(presented, Presented::Operator { .. }) {
                    self.refuse_hook_start(locator).await?;
                }
                return Ok(None);
            }
        };
        let refused = |refusal: AuthorityRefusal| {
            tracing::warn!(
                kind = %new.slot.kind,
                "spec refused for {}: {refusal}",
                new.slot.service,
            );
            Error::DeployRefused(refusal)
        };
        let signed =
            AdmittedSpec::of(&new.slot.namespace, &new.resolved_spec, true).map_err(refused)?;
        // A spec holding a negative number has no digest: no key can sign it, and this
        // Machine remembers nothing of it.
        let digest = PayloadDigest::of(&new.resolved_spec.to_requested(), &new.slot.namespace).ok();
        let (full_access, deploy, rule) = match presented {
            Presented::FullAccess { deploy } => (true, deploy, None),
            Presented::Operator { deploy, rule } => (false, deploy, rule),
        };
        // A signature presented is checked and kept whatever else would admit the spec,
        // so the spec stays admitted for as long as any key that signed it is trusted.
        let unapproved = match deploy.map(|approval| self.approved_deploy(new, approval)) {
            Some(Ok(by)) => {
                self.remember(new, digest, by)?;
                return Ok(Some(signed));
            }
            Some(Err(refusal)) => Some(refusal),
            None => None,
        };
        if full_access {
            self.remember(new, digest, self.full_access_signer())?;
            return Ok(Some(signed));
        }
        if self.admitted_before(new, digest)? {
            return Ok(Some(signed));
        }
        if deploy.is_none()
            && let Some(rule) = rule
        {
            self.admit_rule_deploy_online(want, rule).await?;
            return Ok(Some(signed));
        }
        match needs_signature(creation(new), new.registry_auth.is_some()) {
            Some(reach) => Err(refused(
                unapproved.unwrap_or(AuthorityRefusal::NeedsSignature { reach }),
            )),
            // A spec that reaches no data runs whatever came with it.
            None => Ok(Some(AdmittedSpec {
                signed: false,
                ..signed
            })),
        }
    }

    /// Who signed `approval` for `new`'s spec, when a key this Machine trusts did.
    fn approved_deploy(
        &self,
        new: &NewContainer,
        approval: &Signed<DeployApproval>,
    ) -> Result<SpecSigner, AuthorityRefusal> {
        let record = self.record();
        let approved = deploy::verify(
            approval,
            &DeployFacts {
                cluster: self.cluster_key(record.phase()),
                trusted: &record.trusted_keys(),
                namespace: &new.slot.namespace,
                creation: creation(new),
            },
        )?;
        tracing::info!(
            kind = %new.slot.kind,
            proposal = ?approved.body.proposal,
            "deploy {} approved by {} for {} digest={}",
            approved.body.operation,
            approved.by,
            new.slot.service,
            approved.digest.short(),
        );
        Ok(approved.by.into())
    }

    /// Who a Full access creation is kept under: the Device whose own connection asked,
    /// so removing that Device ends it, or Full access for this Server's own socket.
    fn full_access_signer(&self) -> SpecSigner {
        let Caller::Management(remote) = self.caller else {
            return SpecSigner::FullAccess;
        };
        self.record()
            .devices()
            .find(|device| ClientKey::from(*device) == remote)
            .map_or(SpecSigner::FullAccess, |device| {
                SpecSigner::Device(device.into())
            })
    }

    /// Let a Writer's start through. Starting a Container the Machine already holds is
    /// served, unless it is a hook an Operator names. Creating one is served at Full
    /// access only: an Operator's `SetVolumeCopy` carries no signature of the spec.
    ///
    /// # Errors
    /// Returns [`Error::DeployRefused`] for an Operator's creation and for a hook, and
    /// [`Error::DockerUnavailable`] or [`Error::Docker`] when the Container cannot be read.
    pub(crate) async fn admit_volume_start(
        &self,
        start: &Start,
        operator: bool,
    ) -> Result<Option<AdmittedSpec>, Error> {
        match start {
            Start::Kept { container_id } => {
                if operator {
                    self.refuse_hook_start(&ContainerLocator::Id(*container_id))
                        .await?;
                }
                Ok(None)
            }
            Start::Create { resolved_spec, .. } if operator => {
                tracing::warn!(
                    "Volume start refused for {}: it creates a Container nothing signed",
                    resolved_spec.name,
                );
                Err(Error::DeployRefused(AuthorityRefusal::Mismatch {
                    field: "a Volume start",
                }))
            }
            Start::Create {
                namespace,
                resolved_spec,
                ..
            } => AdmittedSpec::of(namespace, resolved_spec, true)
                .map(Some)
                .map_err(Error::DeployRefused),
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

    /// Whether this Machine admitted `new`'s spec signed before, for the Service its slot
    /// names, by a key it still trusts.
    fn admitted_before(
        &self,
        new: &NewContainer,
        digest: Option<PayloadDigest>,
    ) -> Result<bool, Error> {
        let Some(digest) = digest else {
            return Ok(false);
        };
        if new.resolved_spec.name != new.slot.service {
            return Ok(false);
        }
        self.approvals.spec_admitted(
            &new.slot.namespace,
            &new.slot.service,
            digest,
            &self.record().trusted_keys(),
        )
    }

    /// Keep `new`'s spec as one this Machine admitted signed, so a later creation of it
    /// needs nothing presented. A spec whose image is a tag is not kept: whoever writes
    /// the image store chooses what a tag runs.
    fn remember(
        &self,
        new: &NewContainer,
        digest: Option<PayloadDigest>,
        by: SpecSigner,
    ) -> Result<(), Error> {
        let Some(digest) = digest else {
            return Ok(());
        };
        if new.resolved_spec.name != new.slot.service
            || !content_addressed(&new.resolved_spec.container.image)
        {
            return Ok(());
        }
        self.approvals
            .admit_spec(&new.slot.namespace, &new.slot.service, digest, by)
    }

    pub(super) fn cluster_key(
        &self,
        phase: ployz_core::LocalMachinePhase,
    ) -> Option<ployz_core::SealingPublicKey> {
        self.held_sealing(phase).and_then(Sealing::held_key)
    }
}

#[cfg(test)]
impl LocalMachine {
    /// Ensure `want` as a full-access caller does.
    pub(crate) async fn ensure_full_access(
        &self,
        want: ContainerWant,
    ) -> Result<ployz_core::EnsuredContainer, Error> {
        let admitted = self
            .admit_spec(&want, Presented::FullAccess { deploy: None })
            .await?;
        self.ensure_container(want, admitted).await
    }
}

fn creation(new: &NewContainer) -> Creation<'_> {
    Creation {
        service: &new.slot.service,
        kind: new.slot.kind,
        spec: &new.resolved_spec,
    }
}

/// What a refused deploy says to the person who ran it.
pub(super) fn refusal_text(refusal: &AuthorityRefusal) -> String {
    let text = match refusal {
        AuthorityRefusal::NeedsSignature { reach } => {
            return format!(
                "This service {reach}, so this server runs it only from a deploy an approved device signed."
            );
        }
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
