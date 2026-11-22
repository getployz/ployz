//! Which specs this Machine makes a Container from. A spec that reaches data needs a
//! signature this Machine checks itself: a signed version a trusted key signed, an
//! automatic deploy rule's proof of its build, or a caller at Full access.
//! A spec that reaches no data runs for any caller. Who admitted a Container, and when, is
//! kept with it, and an Operator's start of it is checked against the keys trusted then and
//! its Service's floor. A hook runs once, when it is created.

use std::{io, sync::Arc};

use ployz_core::{
    ClientKey, ContainerId, ContainerKind, ContainerLocator, ContainerNow, ContainerWant,
    DeviceCode, ManagementScope, Namespace, NewContainer, QualifiedService, ResolvedServiceSpec,
    RunTarget, Sealing, Start,
    authority::{
        Approver, AuthorityRefusal, Budget, RuleId, RuleProof, ServiceVersion, Signed,
        SigningPublicKey, Timestamp, TrustedKeys, VerifiedBuild, VersionFacts, deploy::Creation,
        needs_signature, refusal::field, version,
    },
};
use ployz_decide::service_floor::{self, Refused, Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{Caller, ConsumptionLog, Error, LocalMachine, RecordOwner};
use crate::docker::ContainerRuntime;

/// Who admitted a Container, and when, kept with it for as long as it exists.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub(crate) enum AdmittedBy {
    /// The keys whose signature admitted it, never empty, each with the newest time it
    /// signed: the signers of its versions, or the Device whose own connection asked, at
    /// the time it asked.
    Keys {
        service: QualifiedService,
        keys: Vec<(KeptKey, Timestamp)>,
    },
    /// An automatic deploy rule at one generation, signed by `signer`. It admits while
    /// that key is trusted and no newer generation of the rule has reached this Machine.
    Rule {
        service: QualifiedService,
        signer: KeptKey,
        rule: RuleId,
        generation: u64,
        issued_at: Timestamp,
    },
    /// This Server's own socket, or the daemon itself, at the time it asked.
    Local {
        service: QualifiedService,
        at: Timestamp,
    },
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
                .passkeys()
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
            Approver::Signing(key) => Self::Device(key),
            Approver::Passkey(code) => Self::Passkey(code),
        }
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
    /// Whether `by` still admits here: one of its keys is trusted and signed at or after
    /// its Service's floor, and for a rule its signer is trusted, its generation is still
    /// the newest this Machine has seen and it was issued at or after the floor.
    ///
    /// # Errors
    /// The outer error is `unread`'s, when the record cannot be read. The inner one is the
    /// refusal: the first key's when none is trusted, the floor when every trusted one is
    /// below it, a generation for a rule a newer one replaced, an admission for a rule
    /// this Machine has no mark of, and no Cluster identity when the Sealing Key file can't
    /// be read on a Machine that keeps a floor.
    pub(crate) fn admits<E>(
        &self,
        by: &AdmittedBy,
        unread: fn(io::Error) -> E,
    ) -> Result<Result<(), AuthorityRefusal>, E> {
        if *by == AdmittedBy::Free {
            return Ok(Ok(()));
        }
        let record = self.owner.record();
        let trusted = record.trusted_keys();
        // Floors are kept by Cluster, so without the Sealing Key file this Machine can't
        // tell which apply, and refuses rather than read none.
        let cluster = match super::held_key(&self.owner, record.phase()) {
            Some(cluster) => Some(cluster),
            None if self.rules.keeps_floors(unread)? => {
                return Ok(Err(AuthorityRefusal::NoClusterIdentity));
            }
            None => None,
        };
        let floor = |service| match cluster {
            Some(cluster) => Ok(self
                .rules
                .floor(cluster, service, Timestamp::now(), unread)?
                .0),
            None => Ok(None),
        };
        let stands = |at, floor: Option<Timestamp>| match floor {
            Some(below) if !service_floor::keys_stand([(true, at)], floor) => {
                Err(AuthorityRefusal::Forgotten { floor: below })
            }
            _ => Ok(()),
        };
        Ok(match by {
            AdmittedBy::Keys { service, keys } => {
                let floor = floor(service)?;
                let held = keys.iter().map(|(key, at)| (key.trusted(&trusted), *at));
                match floor {
                    _ if service_floor::keys_stand(held, floor) => Ok(()),
                    Some(floor) if keys.iter().any(|(key, _)| key.trusted(&trusted)) => {
                        Err(AuthorityRefusal::Forgotten { floor })
                    }
                    _ => Err(keys
                        .first()
                        .map_or(AuthorityRefusal::UnknownPasskey, |(key, _)| key.untrusted())),
                }
            }
            AdmittedBy::Rule {
                service,
                signer,
                rule,
                generation,
                issued_at,
            } => match self.rules.rule_generation(*rule, unread)? {
                _ if !signer.trusted(&trusted) => Err(signer.untrusted()),
                Some(newest) if newest == *generation => stands(*issued_at, floor(service)?),
                Some(_) => Err(AuthorityRefusal::Mismatch {
                    field: "a generation",
                }),
                None => Err(AuthorityRefusal::Mismatch {
                    field: "an admission",
                }),
            },
            AdmittedBy::Local { service, at } => stands(*at, floor(service)?),
            AdmittedBy::Free => Ok(()),
        })
    }
}

/// The refusal for a time `admits` refused.
pub(super) fn floor_refusal(refused: Refused) -> AuthorityRefusal {
    match refused {
        Refused::Forgotten { floor } => AuthorityRefusal::Forgotten { floor },
        Refused::Ahead => AuthorityRefusal::Mismatch {
            field: field::AHEAD,
        },
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
    /// What this Machine issued for a rule build it checked online to admit the spec.
    verified: Option<VerifiedBuild>,
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
            by: AdmittedBy::Local {
                service: QualifiedService::new(namespace.clone(), spec.name.clone()),
                at: Timestamp::now(),
            },
            standing: None,
            verified: None,
        })
    }

    /// Whether a signature or Full access stands behind the spec.
    pub(crate) fn signed(&self) -> bool {
        self.by != AdmittedBy::Free
    }

    pub(crate) fn by(&self) -> &AdmittedBy {
        &self.by
    }

    pub(crate) fn verified(&self) -> Option<VerifiedBuild> {
        self.verified
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
                AdmittedBy::Local {
                    service: QualifiedService::parse("app/web").unwrap(),
                    at: Timestamp::now(),
                }
            } else {
                AdmittedBy::Free
            },
            standing: None,
            verified: None,
        }
    }
}

/// What a caller brought with a Container change.
#[derive(Clone, Copy)]
pub(crate) enum Presented<'a> {
    /// A call at Full access: this Server's own socket, or a Device's own connection,
    /// with the versions it carries. A version that admits the spec records its signers,
    /// so removing them stops a later start, as for an Operator's creation.
    FullAccess {
        versions: &'a [Signed<ServiceVersion>],
    },
    /// An Operator's call, with the statements it carries.
    Operator {
        rule: Option<&'a RuleProof>,
        versions: &'a [Signed<ServiceVersion>],
    },
}

impl LocalMachine {
    /// Decide whether `want` may make its Container. `None` is the answer for a want that
    /// makes none. A hook that already exists is never started again for an Operator.
    ///
    /// # Errors
    /// Returns [`Error::DeployRefused`] or [`Error::RuleDeployRefused`] naming what the
    /// creation lacks, [`Error::ApprovalRecord`] when the rule record cannot be read or
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
        let admitted = |by, verified| {
            Ok(Some(AdmittedSpec {
                spec,
                by,
                standing: Some(self.standing()),
                verified,
            }))
        };
        let (rule, versions) = match presented {
            Presented::FullAccess { versions } => (None, versions),
            Presented::Operator { rule, versions } => (rule, versions),
        };
        // One budget for every signature this attempt opens, whichever proof carries it.
        let mut budget = Budget::default();
        let mut unapproved = None;
        // Docker makes the spec's own Service, so its floor is the one a later start reads.
        let service =
            QualifiedService::new(new.slot.namespace.clone(), new.resolved_spec.name.clone());
        if !versions.is_empty() {
            match self.approved_versions(new, &service, versions, &mut budget)? {
                Ok(keys) => return admitted(AdmittedBy::Keys { service, keys }, None),
                // A creation at Full access would hide that the forget failed.
                Err(refusal) if asks_to_forget(versions, &self.record().trusted_keys()) => {
                    return Err(refused(refusal));
                }
                Err(refusal) => unapproved = Some(refusal),
            }
        }
        if matches!(presented, Presented::FullAccess { .. }) {
            return admitted(self.full_access(service)?, None);
        }
        if let Some(rule) = rule {
            let admitted_rule = self
                .admit_rule_deploy_online(want, rule, &mut budget)
                .await?;
            return admitted(admitted_rule.by, admitted_rule.verified);
        }
        match needs_signature(creation(new), new.registry_auth.is_some()) {
            Some(reach) => Err(refused(
                unapproved.unwrap_or(AuthorityRefusal::NeedsSignature { reach }),
            )),
            // A spec that reaches no data runs whatever came with it.
            None => admitted(AdmittedBy::Free, None),
        }
    }

    /// Every key that signed one of `versions` for `new`'s spec, among those this Machine
    /// trusts, with the newest time it signed one that `service`'s floor and this
    /// Machine's clock admit. A version marked to forget raises the floor here first, in
    /// the transaction that reads it.
    ///
    /// # Errors
    /// The outer error is [`Error::ApprovalRecord`], when the floor cannot be read or
    /// written. The inner one is the refusal of the versions, or of the first time
    /// refused.
    fn approved_versions(
        &self,
        new: &NewContainer,
        service: &QualifiedService,
        versions: &[Signed<ServiceVersion>],
        budget: &mut Budget,
    ) -> Result<Result<Vec<(KeptKey, Timestamp)>, AuthorityRefusal>, Error> {
        let record = self.record();
        let cluster = self.cluster_key(record.phase());
        let approved = match version::verify(
            versions,
            &VersionFacts {
                cluster,
                trusted: &record.trusted_keys(),
                namespace: &new.slot.namespace,
                creation: creation(new),
            },
            budget,
        ) {
            Ok(approved) => approved,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let Some(cluster) = cluster else {
            return Ok(Err(AuthorityRefusal::NoClusterIdentity));
        };
        let signatures: Vec<_> = approved
            .by
            .into_iter()
            .map(|approved| Signature {
                key: KeptKey::from(approved.by),
                at: approved.signed_at,
                forget: approved.forget_older,
            })
            .collect();
        let keys =
            match self
                .approvals
                .admit_versions(cluster, service, &signatures, Timestamp::now())?
            {
                Ok(keys) => keys,
                Err(refused) => return Ok(Err(floor_refusal(refused))),
            };
        tracing::info!(
            kind = %new.slot.kind,
            "{} admitted by signed version from {:?} digest={}",
            new.slot.service,
            keys,
            approved.digest.short(),
        );
        Ok(Ok(keys))
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
    /// Machine kept no admission for it. Docker's start refuses it beside another running
    /// holder of its Provisioned Volume, as [`crate::docker::holder::start`] decides.
    ///
    /// # Errors
    /// Returns [`Error::DeployRefused`] for a refused start, [`Error::ApprovalRecord`]
    /// when the rule marks cannot be read, [`crate::docker::Error::VolumeHeld`] beside a
    /// running holder, and the Docker error of a start that fails.
    pub(super) async fn start_checked(
        &self,
        containers: &ContainerRuntime,
        id: &ContainerId,
    ) -> Result<(), Error> {
        if self.scope == ManagementScope::Operator {
            let refusal = match containers.lifecycle().admitted_by(id).await? {
                Some(by) => self.standing().admits(&by, Error::ApprovalRecord)?.err(),
                None => Some(AuthorityRefusal::Mismatch {
                    field: "a kept admission",
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

    /// Who a Full access creation of `service` is kept under: the Device whose own
    /// connection asked, so removing that Device ends it, or this Server's own socket. Its
    /// time is this Machine's floored clock, or the Service's floor when that is later, so
    /// a creation after a forget is never below it.
    ///
    /// # Errors
    /// Returns [`Error::NeedsFullScope`] when the connection's key is no longer a Device
    /// here: it was removed after the call arrived, and [`Error::ApprovalRecord`] when the
    /// floor cannot be read.
    fn full_access(&self, service: QualifiedService) -> Result<AdmittedBy, Error> {
        let record = self.record();
        let at = match self.cluster_key(record.phase()) {
            Some(cluster) => {
                let (floor, now) = self.approvals.floor(
                    cluster,
                    &service,
                    Timestamp::now(),
                    Error::ApprovalRecord,
                )?;
                floor.map_or(now, |floor| floor.max(now))
            }
            None => Timestamp::now(),
        };
        let Caller::Management(remote) = self.caller else {
            return Ok(AdmittedBy::Local { service, at });
        };
        record
            .devices()
            .find(|device| ClientKey::from(*device) == remote)
            .map(|device| AdmittedBy::Keys {
                service,
                keys: vec![(KeptKey::Device(device.into()), at)],
            })
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
                let service = QualifiedService::new(namespace.clone(), resolved_spec.name.clone());
                Ok(Some(AdmittedSpec {
                    spec,
                    by: self.full_access(service)?,
                    standing: Some(self.standing()),
                    verified: None,
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
            .admit_spec(&want, Presented::FullAccess { versions: &[] })
            .await?;
        self.ensure_container(want, admitted).await
    }
}

/// Whether any of `versions` is a forget a key trusted here signed.
fn asks_to_forget(versions: &[Signed<ServiceVersion>], trusted: &TrustedKeys) -> bool {
    versions
        .iter()
        .any(|version| version::claims_forget(version, trusted))
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
            field: "a kept admission",
        } => {
            "This container was made before this server's upgrade, so the server starts it only from a deploy an approved device signed. Deploy the service again with a current CLI."
        }
        AuthorityRefusal::Mismatch {
            field: "a generation",
        } => {
            "A newer approval of the automatic deploy rule replaced the one this container ran under."
        }
        AuthorityRefusal::Overspent => {
            "This deploy carries more signatures than a server checks at once. Deploy again."
        }
        AuthorityRefusal::Forgotten { floor } => {
            return format!(
                "This version was approved before {}, when this service's older versions were forgotten. Deploy a version signed since.",
                floor.utc()
            );
        }
        AuthorityRefusal::Mismatch {
            field: field::AHEAD,
        } => {
            "This deploy was signed more than 10 minutes ahead of this server's clock. Check the clock of the device that signed it, and this server's."
        }
        AuthorityRefusal::Mismatch {
            field: field::SYSTEM,
        } => "Versions in the system namespace can't be forgotten.",
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
        AuthorityRefusal::OutOfScope { code, kind, what } => {
            return format!(
                "The {kind} key {code} that approved this deploy may not approve {what} here."
            );
        }
        AuthorityRefusal::Malformed { .. }
        | AuthorityRefusal::BadSignature { .. }
        | AuthorityRefusal::Mismatch { .. }
        | AuthorityRefusal::NoClusterIdentity
        | AuthorityRefusal::ProofReplayed => {
            return format!("This server can't check the deploy's approval: {refusal}.");
        }
    };
    text.to_owned()
}
