//! Which specs this Machine makes a Container from. A spec that reaches data needs a
//! signature this Machine checks itself: a signed version or a deploy approval a trusted
//! key signed, an automatic deploy rule's proof of its build, or a caller at Full access.
//! A spec that reaches no data runs for any caller. Who admitted a Container is kept with
//! it, and an Operator's start of it is checked against the keys trusted then. A hook runs
//! once, when it is created.

use std::{io, sync::Arc};

use ployz_core::{
    ClientKey, ContainerId, ContainerKind, ContainerLocator, ContainerNow, ContainerWant,
    DeviceCode, ManagementScope, Namespace, NewContainer, ResolvedServiceSpec, RunTarget, Sealing,
    ServiceName, Start,
    authority::{
        Approver, AuthorityRefusal, Budget, DeployApproval, DeployFacts, PayloadDigest, RuleId,
        RuleProof, ServiceVersion, Signed, SigningPublicKey, TrustedKeys, VersionFacts,
        deploy::{self, Creation, content_addressed},
        needs_signature, version,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{Caller, ConsumptionLog, Error, LocalMachine, RecordOwner, consumption::SpecSigner};
use crate::docker::ContainerRuntime;

/// Who admitted a Container, kept with it for as long as it exists.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub(crate) enum AdmittedBy {
    /// The keys whose signature admitted it, never empty: the signers of its versions or
    /// its deploy approval, or the Device whose own connection asked.
    Keys { keys: Vec<KeptKey> },
    /// An automatic deploy rule at one generation, signed by `signer`. It admits while
    /// that key is trusted and no newer generation of the rule has reached this Machine.
    Rule {
        signer: KeptKey,
        rule: RuleId,
        generation: u64,
    },
    /// This Server's own socket, or the daemon itself.
    Local,
    /// A spec that reaches no data.
    Free,
}

/// A key that admitted a Container.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum KeptKey {
    Device(SigningPublicKey),
    Passkey(DeviceCode),
}

impl KeptKey {
    fn trusted(self, trusted: &TrustedKeys) -> bool {
        match self {
            Self::Device(key) => trusted.signing_keys().any(|trusted| trusted == key),
            Self::Passkey(code) => trusted
                .passkeys
                .iter()
                .any(|trusted| trusted.passkey.code() == code),
        }
    }

    fn untrusted(self) -> AuthorityRefusal {
        match self {
            Self::Device(key) => AuthorityRefusal::UnknownKey { key },
            Self::Passkey(_) => AuthorityRefusal::UnknownPasskey,
        }
    }
}

impl From<Approver> for KeptKey {
    fn from(approver: Approver) -> Self {
        match approver {
            Approver::Device(key) => Self::Device(key),
            Approver::Passkey(code) => Self::Passkey(code),
        }
    }
}

impl From<KeptKey> for SpecSigner {
    fn from(key: KeptKey) -> Self {
        match key {
            KeptKey::Device(key) => Self::Device(key),
            KeptKey::Passkey(code) => Self::Passkey(code),
        }
    }
}

impl AdmittedBy {
    /// What the record of specs admitted before says about one: Full access when a Full
    /// caller admitted it, else the keys that signed it.
    fn of_signers(signers: Vec<SpecSigner>) -> Option<Self> {
        let mut keys = Vec::new();
        for signer in signers {
            match signer {
                SpecSigner::FullAccess => return Some(Self::Local),
                SpecSigner::Device(key) => keys.push(KeptKey::Device(key)),
                SpecSigner::Passkey(code) => keys.push(KeptKey::Passkey(code)),
            }
        }
        (!keys.is_empty()).then_some(Self::Keys { keys })
    }
}

/// What a kept admission is checked against each time it is used: the keys this Machine
/// trusts and its marks of automatic deploy rules, both read at the check.
#[derive(Clone)]
pub(crate) struct Standing {
    owner: RecordOwner,
    rules: Arc<ConsumptionLog>,
}

impl Standing {
    /// Whether `by` still admits here: one of its keys is trusted, and for a rule its
    /// signer is trusted and its generation is still the newest this Machine has seen.
    ///
    /// # Errors
    /// The outer error is `unread`'s, when the rule marks cannot be read. The inner one
    /// is the refusal: the first key's when none is trusted, a generation for a rule a
    /// newer one replaced, and an admission for a rule this Machine has no mark of.
    pub(crate) fn admits<E>(
        &self,
        by: &AdmittedBy,
        unread: fn(io::Error) -> E,
    ) -> Result<Result<(), AuthorityRefusal>, E> {
        let trusted = self.owner.record().trusted_keys();
        Ok(match by {
            AdmittedBy::Keys { keys } => match keys.iter().find(|key| key.trusted(&trusted)) {
                Some(_) => Ok(()),
                None => Err(keys
                    .first()
                    .map_or(AuthorityRefusal::UnknownPasskey, |key| key.untrusted())),
            },
            AdmittedBy::Rule {
                signer,
                rule,
                generation,
            } => match self.rules.rule_generation(*rule, unread)? {
                _ if !signer.trusted(&trusted) => Err(signer.untrusted()),
                Some(newest) if newest == *generation => Ok(()),
                Some(_) => Err(AuthorityRefusal::Mismatch {
                    field: "a generation",
                }),
                None => Err(AuthorityRefusal::Mismatch {
                    field: "an admission",
                }),
            },
            AdmittedBy::Local | AdmittedBy::Free => Ok(()),
        })
    }
}

impl From<SpecSigner> for AdmittedBy {
    fn from(signer: SpecSigner) -> Self {
        Self::of_signers(vec![signer]).unwrap_or(Self::Local)
    }
}

/// A spec this Machine admitted for a new Container, and who admitted it. A Container
/// opens Sealed values, and runs an image this Machine did not pull itself with no login,
/// only under a signature or Full access.
///
/// Only [`LocalMachine::admit_spec`] and [`LocalMachine::admit_volume_start`] make one
/// for a caller's spec.
#[derive(Clone)]
pub(crate) struct AdmittedSpec {
    /// A hash of the namespace and the whole spec. Every spec has one, where a spec
    /// holding a negative number has no [`PayloadDigest`].
    spec: [u8; 32],
    by: AdmittedBy,
    standing: Option<Standing>,
}

impl std::fmt::Debug for AdmittedSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AdmittedSpec")
            .field("by", &self.by)
            .finish_non_exhaustive()
    }
}

impl AdmittedSpec {
    fn hash(
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
    ) -> Result<[u8; 32], AuthorityRefusal> {
        let body = serde_json::to_vec(&(namespace, spec))
            .map_err(|_| AuthorityRefusal::Mismatch { field: "a payload" })?;
        Ok(Sha256::digest(&body).into())
    }

    /// A reserved spec this daemon built itself, which no caller chose.
    ///
    /// # Errors
    /// Returns [`Error::DeployRefused`] when the spec cannot be written as JSON.
    pub(super) fn own(namespace: &Namespace, spec: &ResolvedServiceSpec) -> Result<Self, Error> {
        Ok(Self {
            spec: Self::hash(namespace, spec).map_err(Error::DeployRefused)?,
            by: AdmittedBy::Local,
            standing: None,
        })
    }

    /// Whether a signature or Full access stands behind the spec.
    pub(crate) fn signed(&self) -> bool {
        self.by != AdmittedBy::Free
    }

    pub(crate) fn by(&self) -> &AdmittedBy {
        &self.by
    }

    /// Whether the admission still holds, checked again right before Docker creates the
    /// Container: a key that admitted it may have been removed, or a newer generation of
    /// its rule may have arrived, during the image pull.
    ///
    /// # Errors
    /// As [`Standing::admits`].
    pub(crate) fn still_holds<E>(
        &self,
        unread: fn(io::Error) -> E,
    ) -> Result<Result<(), AuthorityRefusal>, E> {
        match &self.standing {
            Some(standing) => standing.admits(&self.by, unread),
            None => Ok(Ok(())),
        }
    }

    /// Whether this is the admission of `spec` in `namespace`.
    pub(crate) fn covers(&self, namespace: &Namespace, spec: &ResolvedServiceSpec) -> bool {
        Self::hash(namespace, spec) == Ok(self.spec)
    }

    #[cfg(test)]
    pub(crate) fn test_signed(namespace: &Namespace, spec: &ResolvedServiceSpec) -> Self {
        Self::own(namespace, spec).unwrap()
    }

    /// An admission of no spec in particular, for a test that opens values directly.
    #[cfg(test)]
    pub(crate) fn test_unbound(signed: bool) -> Self {
        Self {
            spec: [0; 32],
            by: if signed {
                AdmittedBy::Local
            } else {
                AdmittedBy::Free
            },
            standing: None,
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
        versions: &'a [Signed<ServiceVersion>],
    },
}

impl LocalMachine {
    /// Decide whether `want` may make its Container. `None` is the answer for a want that
    /// makes none. A hook that already exists is never started again for an Operator.
    ///
    /// A spec this Machine admitted signed before, for the same Service, is admitted again
    /// with nothing presented while a key that signed it is trusted.
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
        let spec = AdmittedSpec::hash(&new.slot.namespace, &new.resolved_spec).map_err(refused)?;
        let admitted = |by| {
            Ok(Some(AdmittedSpec {
                spec,
                by,
                standing: Some(self.standing()),
            }))
        };
        // A spec holding a negative number has no digest: no key can sign it, and this
        // Machine remembers nothing of it.
        let digest = PayloadDigest::of(&new.resolved_spec.to_requested(), &new.slot.namespace).ok();
        let (full_access, deploy, rule, versions) = match presented {
            Presented::FullAccess { deploy } => (true, deploy, None, &[][..]),
            Presented::Operator {
                deploy,
                rule,
                versions,
            } => (false, deploy, rule, versions),
        };
        // One budget for every signature this attempt opens, whichever proof carries it.
        let mut budget = Budget::default();
        let mut unapproved = None;
        if !versions.is_empty() {
            match self.approved_versions(new, versions, &mut budget) {
                Ok(keys) => {
                    for key in &keys {
                        self.remember(new, digest, (*key).into())?;
                    }
                    return admitted(AdmittedBy::Keys { keys });
                }
                Err(refusal) => unapproved = Some(refusal),
            }
        }
        // A signature presented is checked and kept whatever else would admit the spec,
        // so the spec stays admitted for as long as any key that signed it is trusted.
        match deploy.map(|approval| self.approved_deploy(new, approval, &mut budget)) {
            Some(Ok(key)) => {
                self.remember(new, digest, key.into())?;
                return admitted(AdmittedBy::Keys { keys: vec![key] });
            }
            // A proof left unopened says more than an earlier proof's refusal.
            Some(Err(AuthorityRefusal::Overspent)) => {
                unapproved = Some(AuthorityRefusal::Overspent);
            }
            Some(Err(refusal)) => unapproved = unapproved.or(Some(refusal)),
            None => {}
        }
        if full_access {
            let signer = self.full_access_signer()?;
            self.remember(new, digest, signer)?;
            return admitted(signer.into());
        }
        if let Some(by) = self.admitted_before(new, digest)? {
            return admitted(by);
        }
        if deploy.is_none()
            && let Some(rule) = rule
        {
            let by = self
                .admit_rule_deploy_online(want, rule, &mut budget)
                .await?;
            return admitted(by);
        }
        match needs_signature(creation(new), new.registry_auth.is_some()) {
            Some(reach) => Err(refused(
                unapproved.unwrap_or(AuthorityRefusal::NeedsSignature { reach }),
            )),
            // A spec that reaches no data runs whatever came with it.
            None => admitted(AdmittedBy::Free),
        }
    }

    /// Every key that signed one of `versions` for `new`'s spec, among those this Machine
    /// trusts.
    fn approved_versions(
        &self,
        new: &NewContainer,
        versions: &[Signed<ServiceVersion>],
        budget: &mut Budget,
    ) -> Result<Vec<KeptKey>, AuthorityRefusal> {
        let record = self.record();
        let approved = version::verify(
            versions,
            &VersionFacts {
                cluster: self.cluster_key(record.phase()),
                trusted: &record.trusted_keys(),
                namespace: &new.slot.namespace,
                creation: creation(new),
            },
            budget,
        )?;
        let keys: Vec<KeptKey> = approved.by.into_iter().map(KeptKey::from).collect();
        tracing::info!(
            kind = %new.slot.kind,
            "{} admitted by signed version from {:?} digest={}",
            new.slot.service,
            keys,
            approved.digest.short(),
        );
        Ok(keys)
    }

    /// Who signed `approval` for `new`'s spec, when a key this Machine trusts did.
    fn approved_deploy(
        &self,
        new: &NewContainer,
        approval: &Signed<DeployApproval>,
        budget: &mut Budget,
    ) -> Result<KeptKey, AuthorityRefusal> {
        let record = self.record();
        let trusted = record.trusted_keys();
        budget.reserve(approval, &trusted)?;
        let approved = deploy::verify(
            approval,
            &DeployFacts {
                cluster: self.cluster_key(record.phase()),
                trusted: &trusted,
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

    /// What a kept admission is checked against when it is used.
    pub(super) fn standing(&self) -> Standing {
        Standing {
            owner: self.owner.clone(),
            rules: Arc::clone(&self.approvals),
        }
    }

    /// Start `id`. An Operator's start is refused when the admission kept with the
    /// Container no longer stands here, as [`Standing::admits`] decides, and when this
    /// Machine kept no admission for it.
    ///
    /// # Errors
    /// Returns [`Error::DeployRefused`] for a refused start, [`Error::ApprovalRecord`]
    /// when the rule marks cannot be read, and the Docker error of a start that fails.
    pub(super) async fn start_checked(
        &self,
        containers: &ContainerRuntime,
        id: &ContainerId,
    ) -> Result<(), Error> {
        if self.scope == ManagementScope::Operator {
            let refusal = match containers.lifecycle().admitted_by(id).await? {
                Some(by) => self.standing().admits(&by, Error::ApprovalRecord)?.err(),
                None => Some(AuthorityRefusal::Mismatch {
                    field: "an admission",
                }),
            };
            if let Some(refusal) = refusal {
                tracing::warn!(container = %id, "start refused: {refusal}");
                return Err(Error::DeployRefused(refusal));
            }
        }
        containers.lifecycle().start(id).await?;
        Ok(())
    }

    /// Who a Full access creation is kept under: the Device whose own connection asked,
    /// so removing that Device ends it, or Full access for this Server's own socket.
    ///
    /// # Errors
    /// Returns [`Error::NeedsFullScope`] when the connection's key is no longer a Device
    /// here: it was removed after the call arrived.
    fn full_access_signer(&self) -> Result<SpecSigner, Error> {
        let Caller::Management(remote) = self.caller else {
            return Ok(SpecSigner::FullAccess);
        };
        self.record()
            .devices()
            .find(|device| ClientKey::from(*device) == remote)
            .map(|device| SpecSigner::Device(device.into()))
            .ok_or(Error::NeedsFullScope)
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
            } => {
                let spec =
                    AdmittedSpec::hash(namespace, resolved_spec).map_err(Error::DeployRefused)?;
                let signer = self.full_access_signer()?;
                self.remember_spec(
                    namespace,
                    &resolved_spec.name,
                    resolved_spec,
                    PayloadDigest::of(&resolved_spec.to_requested(), namespace).ok(),
                    signer,
                )?;
                Ok(Some(AdmittedSpec {
                    spec,
                    by: signer.into(),
                    standing: Some(self.standing()),
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

    /// Who admitted `new`'s spec signed here before, for the Service its slot names,
    /// among the keys this Machine still trusts.
    fn admitted_before(
        &self,
        new: &NewContainer,
        digest: Option<PayloadDigest>,
    ) -> Result<Option<AdmittedBy>, Error> {
        let Some(digest) = digest else {
            return Ok(None);
        };
        if new.resolved_spec.name != new.slot.service {
            return Ok(None);
        }
        let signers = self.approvals.spec_admitted(
            &new.slot.namespace,
            &new.slot.service,
            digest,
            &self.record().trusted_keys(),
        )?;
        Ok(AdmittedBy::of_signers(signers))
    }

    fn remember(
        &self,
        new: &NewContainer,
        digest: Option<PayloadDigest>,
        by: SpecSigner,
    ) -> Result<(), Error> {
        self.remember_spec(
            &new.slot.namespace,
            &new.slot.service,
            &new.resolved_spec,
            digest,
            by,
        )
    }

    /// Keep `spec` as one this Machine admitted signed for `service`, so a later creation
    /// of it needs nothing presented. A spec whose image is a tag is not kept: whoever
    /// writes the image store chooses what a tag runs.
    fn remember_spec(
        &self,
        namespace: &Namespace,
        service: &ServiceName,
        spec: &ResolvedServiceSpec,
        digest: Option<PayloadDigest>,
        by: SpecSigner,
    ) -> Result<(), Error> {
        let Some(digest) = digest else {
            return Ok(());
        };
        if spec.name != *service || !content_addressed(&spec.container.image) {
            return Ok(());
        }
        self.approvals
            .admit_spec(namespace, service, digest, by, || {
                self.record().trusted_keys()
            })
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
            field: "an admission",
        } => "This server kept no record of who approved this container. Deploy it again.",
        AuthorityRefusal::Mismatch {
            field: "a generation",
        } => {
            "A newer approval of the automatic deploy rule replaced the one this container ran under."
        }
        AuthorityRefusal::Overspent => {
            "This deploy carries more signatures than a server checks at once. Deploy again."
        }
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
