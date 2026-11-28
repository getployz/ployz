//! Which specs this Machine makes a Container from. A spec that reaches data needs a
//! signature this Machine checks itself: a signed version a trusted key signed, an
//! automatic deploy rule's proof of its build, or a caller at Full access.
//! A spec that reaches no data runs for any caller. Who admitted a Container, and when, is
//! kept with it, and an Operator's start of it is checked against the keys trusted then and
//! its Service's floor. A hook runs once, when it is created.

use std::{io, sync::Arc};

use ployz_core::{
    AnswerWhen, ClientKey, ContainerId, ContainerKind, ContainerLocator, ContainerNow,
    ContainerWant, DeviceCode, DockerVolumeName, Namespace, NewContainer, QualifiedService,
    ResolvedServiceSpec, RunTarget, Sealing, SlotKey, Start,
    authority::{
        Approver, AuthorityRefusal, Budget, MismatchField, Reach, RuleId, RuleProof,
        ServiceVersion, Signed, SigningPublicKey, Timestamp, TrustedKeys, ValueReach,
        VerifiedBuild, VersionFacts, deploy::Creation, needs_signature, version,
    },
};
use ployz_decide::service_floor::{self, Refused, Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{Caller, Checked, ConsumptionLog, Error, FullCaller, LocalMachine, RecordOwner, Scope};
use crate::docker::ContainerRuntime;

/// Who admitted a Container, and when, kept with it for as long as it exists.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub(crate) enum AdmittedBy {
    /// The keys whose signature admitted it, never empty, each with the newest time it
    /// signed: the signers of its versions, or the Device whose own connection asked, at
    /// the time it asked. Each one covered all of `reach` alone.
    Keys {
        service: QualifiedService,
        keys: Vec<(KeptKey, Timestamp)>,
        /// What the Container reaches, which a key must still cover alone at a start.
        /// `None` for a Container kept before this was.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reach: Option<Reach>,
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

impl AdmittedBy {
    /// What the Container's address part counts. A key, a rule or this Machine's own
    /// caller vouched for every admission but [`Self::Free`].
    pub(crate) fn admission(&self) -> ployz_core::Admission {
        match self {
            Self::Keys { .. } | Self::Rule { .. } | Self::Local { .. } => {
                ployz_core::Admission::Signed
            }
            Self::Free => ployz_core::Admission::Free,
        }
    }

    /// What the Container's observation reports, which names and hostnames read. A key or
    /// a rule keeps its name signed. This Machine's own socket counts as free for names,
    /// since no Device signed it.
    pub(crate) fn name_admission(&self) -> ployz_core::Admission {
        match self {
            Self::Keys { .. } | Self::Rule { .. } => ployz_core::Admission::Signed,
            Self::Local { .. } | Self::Free => ployz_core::Admission::Free,
        }
    }
}

/// Whether an admission keeps what it decided: a floor a forget raised, a rule's newer
/// generation and a hook's place. A check answers the same and keeps none of them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Admit {
    Record,
    Check,
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
        trusted.of(&self.approver()).is_some()
    }

    fn untrusted(self) -> AuthorityRefusal {
        match self {
            Self::Device(key) => AuthorityRefusal::UnknownKey { key },
            Self::Passkey(_) => AuthorityRefusal::UnknownPasskey,
        }
    }

    const fn approver(self) -> Approver {
        match self {
            Self::Device(key) => Approver::Signing(key),
            Self::Passkey(code) => Approver::Passkey(code),
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
    /// Whether `by` still admits here: one of its keys still covers its Service and all
    /// it reaches alone, as [`TrustedKeys::covers_kept`] decides, and signed at or after
    /// its Service's floor, and for a rule its signer is trusted, its generation is still
    /// the newest this Machine has seen and it was issued at or after the floor.
    ///
    /// # Errors
    /// The outer error is `unread`'s, when the record cannot be read. The inner one is the
    /// refusal: the first key's when none covers, the floor when every covering one is
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
            AdmittedBy::Keys {
                service,
                keys,
                reach,
            } => {
                let floor = floor(service)?;
                let covers =
                    |key: &KeptKey| trusted.covers_kept(&key.approver(), service, reach.as_ref());
                let held = keys.iter().map(|(key, at)| (covers(key).is_ok(), *at));
                match floor {
                    _ if service_floor::keys_stand(held, floor) => Ok(()),
                    Some(floor) if keys.iter().any(|(key, _)| covers(key).is_ok()) => {
                        Err(AuthorityRefusal::Forgotten { floor })
                    }
                    _ => Err(gone_key(
                        keys.first()
                            .and_then(|(key, _)| covers(key).err())
                            .unwrap_or(AuthorityRefusal::UnknownPasskey),
                    )),
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
                    field: MismatchField::Generation,
                }),
                None => Err(AuthorityRefusal::Mismatch {
                    field: MismatchField::Admission,
                }),
            },
            AdmittedBy::Local { service, at } => stands(*at, floor(service)?),
            AdmittedBy::Free => Ok(()),
        })
    }
}

/// A kept admission's refusal for keys this Machine no longer trusts, which says they
/// went away rather than that they never approved: a rotation or removal took them.
fn gone_key(refusal: AuthorityRefusal) -> AuthorityRefusal {
    if matches!(
        refusal,
        AuthorityRefusal::UnknownKey { .. } | AuthorityRefusal::UnknownPasskey
    ) {
        return AuthorityRefusal::Mismatch {
            field: MismatchField::KeptKey,
        };
    }
    refusal
}

/// The refusal for a time `admits` refused.
pub(super) fn floor_refusal(refused: Refused) -> AuthorityRefusal {
    match refused {
        Refused::Forgotten { floor } => AuthorityRefusal::Forgotten { floor },
        Refused::Ahead => AuthorityRefusal::Mismatch {
            field: MismatchField::SigningTime,
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
    /// The same hash with the spec's pull policy left out: what the spec runs, not how
    /// its image is placed. A Writer start's recorded image binds to it, so a Release
    /// that resends the Move's spec under another pull policy takes the same image.
    placement: [u8; 32],
    by: AdmittedBy,
    standing: Option<Standing>,
    /// What this Machine issued for a rule build it checked online to admit the spec.
    verified: Option<VerifiedBuild>,
    hook: Option<super::rule_deploy::HookPlace>,
    /// A preview Writer's image, inspected once at its start's admission, which the check
    /// before the promote and the create read again.
    preview_image: Option<crate::docker::PreviewImage>,
    /// The image a registry rule's pull placed here, which every Container this admission
    /// makes, adopts or starts must run.
    pin: Option<ployz_core::ImageDigest>,
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
        let body =
            serde_json::to_vec(&(namespace, spec)).map_err(|_| AuthorityRefusal::Mismatch {
                field: MismatchField::Payload,
            })?;
        Ok(Sha256::digest(&body).into())
    }

    /// [`Self::hash`] of `spec` with its pull policy fixed, so it binds everything else
    /// the spec says.
    fn placement_hash(
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
    ) -> Result<[u8; 32], AuthorityRefusal> {
        let mut placed = spec.clone();
        placed.container.pull_policy = ployz_core::PullPolicy::Never;
        Self::hash(namespace, &placed)
    }

    /// A reserved spec this daemon built itself, which no caller chose.
    ///
    /// # Errors
    /// Returns [`Error::DeployRefused`] when the spec cannot be written as JSON.
    pub(super) fn own(namespace: &Namespace, spec: &ResolvedServiceSpec) -> Result<Self, Error> {
        Ok(Self {
            spec: Self::hash(namespace, spec).map_err(Error::DeployRefused)?,
            placement: Self::placement_hash(namespace, spec).map_err(Error::DeployRefused)?,
            by: AdmittedBy::Local {
                service: QualifiedService::new(namespace.clone(), spec.name.clone()),
                at: Timestamp::now(),
            },
            standing: None,
            verified: None,
            hook: None,
            preview_image: None,
            pin: None,
        })
    }

    /// The hash of the namespace and the spec this admits, its pull policy left out.
    pub(crate) const fn placement(&self) -> [u8; 32] {
        self.placement
    }

    /// Whether a signature or Full access stands behind the spec.
    pub(crate) fn signed(&self) -> bool {
        self.by != AdmittedBy::Free
    }

    pub(crate) fn by(&self) -> &AdmittedBy {
        &self.by
    }

    /// What the Container it makes reports, which names and hostnames read.
    pub(crate) fn admission(&self) -> ployz_core::Admission {
        self.by.name_admission()
    }

    pub(crate) fn verified(&self) -> Option<VerifiedBuild> {
        self.verified
    }

    /// The place a hook's push takes when its create runs, for a hook a rule admitted.
    pub(crate) fn hook(&self) -> Option<&super::rule_deploy::HookPlace> {
        self.hook.as_ref()
    }

    /// The image a preview Writer's start inspected, which its create runs by ID.
    pub(crate) fn preview_image(&self) -> Option<&crate::docker::PreviewImage> {
        self.preview_image.as_ref()
    }

    /// The image this admission's Container must run, as this Machine pulled it for a
    /// registry rule.
    pub(crate) fn pin(&self) -> Option<&ployz_core::ImageDigest> {
        self.pin.as_ref()
    }

    /// Whether the admission covers opening `value`: a version's signers covered the
    /// Sealed parts it kept, and a rule's signer or Full access covers every one.
    pub(crate) fn opens(&self, value: &ValueReach) -> bool {
        match &self.by {
            AdmittedBy::Keys { reach, .. } => reach
                .as_ref()
                .is_some_and(|reach| reach.values.contains(value)),
            AdmittedBy::Rule { .. } | AdmittedBy::Local { .. } => true,
            AdmittedBy::Free => false,
        }
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

    /// `spec` admitted with the image a registry rule's pull placed.
    #[cfg(test)]
    pub(crate) fn test_pinned(
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
        pin: ployz_core::ImageDigest,
    ) -> Self {
        Self {
            pin: Some(pin),
            ..Self::test_signed(namespace, spec)
        }
    }

    /// `spec` admitted by a Device's signature, for a test that needs a Container another
    /// key made.
    #[cfg(test)]
    pub(crate) fn test_keyed(namespace: &Namespace, spec: &ResolvedServiceSpec) -> Self {
        Self {
            by: AdmittedBy::Keys {
                service: QualifiedService::new(namespace.clone(), spec.name.clone()),
                keys: Vec::new(),
                reach: None,
            },
            ..Self::own(namespace, spec).unwrap()
        }
    }

    /// A version's admission of no spec in particular that reaches `reach`, for a test
    /// that opens values directly.
    #[cfg(test)]
    pub(crate) fn test_reaching(reach: Reach) -> Self {
        Self {
            spec: [0; 32],
            placement: [0; 32],
            by: AdmittedBy::Keys {
                service: QualifiedService::parse("app/web").unwrap(),
                keys: Vec::new(),
                reach: Some(reach),
            },
            standing: None,
            verified: None,
            hook: None,
            preview_image: None,
            pin: None,
        }
    }

    /// An admission of no spec in particular, for a test that opens values directly.
    #[cfg(test)]
    pub(crate) fn test_unbound(signed: bool) -> Self {
        Self {
            spec: [0; 32],
            placement: [0; 32],
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
            hook: None,
            preview_image: None,
            pin: None,
        }
    }
}

impl LocalMachine {
    /// Decide whether `want` may make its Container, under the `versions` and the
    /// Operator's `rule` the caller brought, keeping what `admit` says. `None` is the
    /// answer for a want that makes none. A hook that already exists is never started
    /// again for an Operator, checked here and again under the turn of the start, by
    /// [`Checked::start_kept`].
    ///
    /// A version that admits the spec records its signers, so removing them stops a later
    /// start. Short of one, a Full caller's creation is kept under who asked, and `rule`
    /// is read only for an Operator. The scope is the caller's as the keys stand now, and
    /// what this keeps is checked again under the turn right before Docker creates, so a
    /// key removed in between makes nothing.
    ///
    /// # Errors
    /// Returns [`Error::DeployRefused`] or [`Error::RuleDeployRefused`] naming what the
    /// creation lacks, [`Error::ApprovalRecord`] when the rule record cannot be read or
    /// written, and [`Error::DockerUnavailable`] or [`Error::Docker`] when a Container to
    /// start cannot be read.
    pub(crate) async fn admit_spec(
        &self,
        want: &ContainerWant,
        rule: Option<&RuleProof>,
        versions: &[Signed<ServiceVersion>],
        admit: Admit,
    ) -> Result<Option<AdmittedSpec>, Error> {
        let ContainerWant::Running { container, .. } = want else {
            return Ok(None);
        };
        let scope = self.scope_now()?;
        let new = match container {
            RunTarget::New(new) => new,
            RunTarget::Existing(locator) => {
                // Refused here as well as under the turn, so an Operator naming a hook that
                // already runs, which no start reaches, is refused too.
                if let Scope::Operator = scope {
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
        let placement = AdmittedSpec::placement_hash(&new.slot.namespace, &new.resolved_spec)
            .map_err(refused)?;
        let admitted = |by, verified, hook, pin| {
            Ok(Some(AdmittedSpec {
                spec,
                placement,
                by,
                standing: Some(self.standing()),
                verified,
                hook,
                preview_image: None,
                pin,
            }))
        };
        // One budget for every signature this attempt opens, whichever proof carries it.
        let mut budget = Budget::default();
        let mut unapproved = None;
        // Docker makes the spec's own Service, so its floor is the one a later start reads.
        let service =
            QualifiedService::new(new.slot.namespace.clone(), new.resolved_spec.name.clone());
        if !versions.is_empty() {
            match self.approved_versions(new, &service, versions, &mut budget, admit)? {
                Ok(by) => return admitted(by, None, None, None),
                // A creation at Full access would hide that the forget failed.
                Err(refusal) if asks_to_forget(versions, &self.record().trusted_keys()) => {
                    return Err(refused(refusal));
                }
                Err(refusal) => unapproved = Some(refusal),
            }
        }
        // A rule's proof is read before Full access, so a rule deploy through this Server's
        // own socket keeps its name as it does from anywhere else.
        if let Some(rule) = rule {
            match self
                .admit_rule_deploy_online(want, rule, &mut budget, admit)
                .await
            {
                Ok(admitted_rule) => {
                    return admitted(
                        admitted_rule.by,
                        admitted_rule.verified,
                        admitted_rule.hook,
                        admitted_rule.pin,
                    );
                }
                Err(Error::RuleDeployRefused(_)) if matches!(scope, Scope::Full(_)) => {}
                Err(error) => return Err(error),
            }
        }
        let by = if let Scope::Full(full) = scope {
            let reach = Reach::of(new.slot.kind, &new.resolved_spec).map_err(|_| {
                refused(AuthorityRefusal::Mismatch {
                    field: MismatchField::Payload,
                })
            })?;
            self.full_access(full, service.clone(), reach)?
        } else if let Some(reach) = needs_signature(
            creation(new),
            new.registry_auth.is_some() || new.registry_login.is_some(),
        ) {
            return Err(refused(
                unapproved.unwrap_or(AuthorityRefusal::NeedsSignature { reach }),
            ));
        } else {
            // A spec that reaches no data runs whatever came with it.
            AdmittedBy::Free
        };
        if self.name_refuses(&by, new.slot.kind, &service).await? {
            return Err(refused(
                unapproved.unwrap_or(AuthorityRefusal::SignedName { service }),
            ));
        }
        admitted(by, None, None, None)
    }

    /// Whether `service`'s name refuses a `kind` Container admitted `by`. A name that ran
    /// signed answers only for a signed Container, so one no Device or rule signed never
    /// takes it. A free one fails closed with no replica to read. One from this Server's
    /// own socket is root's, so the refusal only saves a copy that would answer for
    /// nothing, and it is skipped with no replica, as it is in the system Namespace.
    ///
    /// # Errors
    /// Returns [`Error::ClusterStoreUnavailable`] for a free create with no replica, and the
    /// replica's error when the name can't be read.
    pub(super) async fn name_refuses(
        &self,
        by: &AdmittedBy,
        kind: ContainerKind,
        service: &QualifiedService,
    ) -> Result<bool, Error> {
        if kind != ContainerKind::ServiceContainer {
            return Ok(false);
        }
        let replicated = match by {
            AdmittedBy::Keys { .. } | AdmittedBy::Rule { .. } => return Ok(false),
            AdmittedBy::Free => self.replicated()?,
            AdmittedBy::Local { .. } if service.namespace == Namespace::system() => {
                return Ok(false);
            }
            AdmittedBy::Local { .. } => match self.replicated() {
                Ok(replicated) => replicated,
                Err(_) => return Ok(false),
            },
        };
        Ok(replicated.signed_service_kept(service).await?)
    }

    /// The admission of `new`'s spec under `versions`: every key that signed one of them,
    /// among those this Machine trusts, with the newest time it signed one that
    /// `service`'s floor and this Machine's clock admit, and what the spec reaches, which
    /// each covers alone. A version marked to forget raises the floor here first, in
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
        admit: Admit,
    ) -> Result<Result<AdmittedBy, AuthorityRefusal>, Error> {
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
                at: approved.floor_at,
                signed: approved.signed_at,
                forget: approved.forget_older,
            })
            .collect();
        let keys = match self.approvals.admit_versions(
            cluster,
            service,
            &signatures,
            Timestamp::now(),
            admit,
        )? {
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
        Ok(Ok(AdmittedBy::Keys {
            service: service.clone(),
            keys,
            reach: Some(approved.reach),
        }))
    }

    /// What a kept admission is checked against when it is used.
    pub(super) fn standing(&self) -> Standing {
        Standing {
            owner: self.owner.clone(),
            rules: Arc::clone(&self.approvals),
        }
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
    fn full_access(
        &self,
        full: FullCaller,
        service: QualifiedService,
        reach: Reach,
    ) -> Result<AdmittedBy, Error> {
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
        let Caller::Management(remote) = full.caller() else {
            return Ok(AdmittedBy::Local { service, at });
        };
        record
            .devices()
            .find(|device| ClientKey::from(*device) == remote)
            .map(|device| AdmittedBy::Keys {
                service,
                keys: vec![(KeptKey::Device(device.into()), at)],
                reach: Some(reach),
            })
            .ok_or(Error::NeedsFullScope)
    }

    /// Let a Writer's start through. Starting a Container the Machine already holds is
    /// served, unless it is a hook an Operator names. A Create is admitted as
    /// [`Self::admit_spec`] admits its spec, at the scope the caller holds now. An
    /// Operator's needs a signature from the versions or rule proof it carries, and a spec
    /// that mounts `volume`, so no signature of a spec opens another Volume. All of it is
    /// checked again under the switch, by [`Checked::recheck_volume_start`]. A `check`
    /// keeps nothing, and refuses a spec this Machine's placement doesn't take.
    ///
    /// # Errors
    /// Returns [`Error::DeployRefused`] or [`Error::RuleDeployRefused`] for a refused start
    /// and a hook, the placement's error for a check this Machine doesn't take, and
    /// [`Error::DockerUnavailable`] or [`Error::Docker`] when the Container cannot be read.
    pub(crate) async fn admit_volume_start(
        &self,
        volume: &DockerVolumeName,
        start: &Start,
    ) -> Result<Option<AdmittedSpec>, Error> {
        let operator = matches!(self.scope_now()?, Scope::Operator);
        let (namespace, resolved_spec, creation_key, r#gen, rotation, versions, rule, login, check) =
            match start {
                Start::Kept { container_id } => {
                    if operator {
                        self.refuse_hook_start(&ContainerLocator::Id(*container_id))
                            .await?;
                    }
                    return Ok(None);
                }
                Start::Create {
                    namespace,
                    resolved_spec,
                    creation_key,
                    r#gen,
                    rotation,
                    versions,
                    rule,
                    registry_login,
                    check,
                } => (
                    namespace,
                    resolved_spec,
                    creation_key,
                    r#gen,
                    rotation,
                    versions,
                    rule,
                    registry_login,
                    *check,
                ),
            };
        if operator {
            operator_start(volume, resolved_spec, versions, rule.as_deref())?;
        }
        let want = ContainerWant::Running {
            container: RunTarget::New(NewContainer {
                slot: SlotKey {
                    namespace: namespace.clone(),
                    service: resolved_spec.name.clone(),
                    kind: ContainerKind::ServiceContainer,
                    creation_key: creation_key.clone(),
                },
                resolved_spec: resolved_spec.clone(),
                r#gen: *r#gen,
                deployment_id: None,
                registry_auth: None,
                // A registry rule's pull opens it only for the image's own registry.
                registry_login: login.clone(),
                image_prepared: false,
                answer: AnswerWhen::default(),
                image_id: None,
                replaces: None,
                free_receipt: None,
            }),
            rotation: rotation.clone(),
        };
        let admit = if check { Admit::Check } else { Admit::Record };
        let admitted = self
            .admit_spec(&want, rule.as_deref(), versions, admit)
            .await?;
        if operator && !admitted.as_ref().is_some_and(AdmittedSpec::signed) {
            return Err(start_refused(resolved_spec, MismatchField::VolumeStart));
        }
        admit_preview_start(
            &self.record().id(),
            namespace,
            resolved_spec,
            admitted.as_ref(),
        )?;
        // Inspected once, here, before the cut or promote: the check under the switch and
        // the create read this result, and the create runs this image by its ID.
        let admitted = match admitted {
            Some(mut admitted) if ployz_core::PreviewNamespace::of(namespace).is_some() => {
                // A Service's login goes only with a signed spec, as its create sends it.
                let login = login.as_ref().filter(|_| admitted.signed());
                admitted.preview_image = self
                    .docker()?
                    .lifecycle()
                    .prepare_preview_image(namespace, resolved_spec, (admitted.pin(), login))
                    .await?;
                if let Some(image) = &admitted.preview_image {
                    image.admit(namespace, resolved_spec)?;
                }
                Some(admitted)
            }
            admitted => admitted,
        };
        if check {
            let record = self.record();
            let machine = record.cluster_machine().ok_or(Error::NotParticipating)?;
            let storage = self.observe_storage().await;
            crate::docker::require_eligible(resolved_spec.placement_eligibility_in_namespace(
                namespace,
                machine,
                storage.as_ref(),
            ))?;
        }
        Ok(admitted)
    }

    /// A hook runs once, when it is created: no Operator starts one again.
    async fn refuse_hook_start(&self, locator: &ContainerLocator) -> Result<(), Error> {
        match self.container_now(locator).await? {
            ContainerNow::Present(observed) if observed.kind == ContainerKind::PreDeployHook => {
                Err(Error::DeployRefused(AuthorityRefusal::Mismatch {
                    field: MismatchField::HookRun,
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

impl Checked {
    /// A Writer's `start`, admitted before the switch, checked again under it before the
    /// promote: an Operator's as [`LocalMachine::admit_volume_start`] checks one, so a
    /// caller that lost its Device slot while it queued starts only what an Operator may,
    /// and a Create's admission only while it still stands.
    ///
    /// # Errors
    /// Returns [`Error::DeployRefused`] for a start the scope held here refuses or an
    /// admission that no longer stands, and [`Error::ApprovalRecord`] when the rule marks
    /// cannot be read.
    pub(super) async fn recheck_volume_start(
        &self,
        volume: &DockerVolumeName,
        start: &Start,
        admitted: Option<&AdmittedSpec>,
    ) -> Result<(), Error> {
        let (resolved_spec, versions, rule) = match start {
            Start::Kept { container_id } => return self.refuse_operator_hook(container_id).await,
            Start::Create {
                namespace,
                resolved_spec,
                versions,
                rule,
                ..
            } => {
                admit_preview_start(&self.record().id(), namespace, resolved_spec, admitted)?;
                if ployz_core::PreviewNamespace::of(namespace).is_some() {
                    admitted
                        .and_then(AdmittedSpec::preview_image)
                        .ok_or_else(|| {
                            Error::PreviewRefused(ployz_core::PreviewRefusal::ImageUnidentified {
                                image: resolved_spec.container.image.clone(),
                            })
                        })?
                        .admit(namespace, resolved_spec)?;
                }
                (resolved_spec, versions, rule)
            }
        };
        let operator = matches!(self.scope(), Scope::Operator);
        if operator {
            operator_start(volume, resolved_spec, versions, rule.as_deref())?;
        }
        let admitted = admitted
            .filter(|admitted| !operator || admitted.signed())
            .ok_or_else(|| start_refused(resolved_spec, MismatchField::VolumeStart))?;
        admitted
            .still_holds(Error::ApprovalRecord)?
            .map_err(Error::DeployRefused)
    }

    /// Start `id`, refused on any Server but its home when it is a preview's. An Operator's
    /// start is also refused when the admission kept with the
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
        if let Some(preview) = containers
            .lifecycle()
            .namespace_of(id)
            .await?
            .as_ref()
            .and_then(ployz_core::PreviewNamespace::of)
        {
            preview
                .admit_on(&self.record().id())
                .map_err(Error::PreviewRefused)?;
            self.admit_preview_runs(preview.namespace())?;
        }
        if let Scope::Operator = self.scope() {
            let refusal = match containers.lifecycle().admitted_by(id).await? {
                Some(by) => self.standing().admits(&by, Error::ApprovalRecord)?.err(),
                None => Some(AuthorityRefusal::Mismatch {
                    field: MismatchField::KeptAdmission,
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

    /// Start `id`, a Container this Machine already holds that no create of this request
    /// made, as [`Self::start_checked`] does, after [`Self::refuse_operator_hook`].
    ///
    /// # Errors
    /// As [`Self::refuse_operator_hook`] and [`Self::start_checked`].
    pub(super) async fn start_kept(
        &self,
        containers: &ContainerRuntime,
        id: &ContainerId,
    ) -> Result<(), Error> {
        self.refuse_operator_hook(id).await?;
        self.start_checked(containers, id).await
    }

    /// A hook runs once, when it is created: no Operator starts one this Machine already
    /// holds again.
    ///
    /// # Errors
    /// Returns [`Error::DeployRefused`] for an Operator's start of a hook, and
    /// [`Error::Docker`] when the Container cannot be read.
    pub(super) async fn refuse_operator_hook(&self, id: &ContainerId) -> Result<(), Error> {
        match self.scope() {
            Scope::Operator => self.refuse_hook_start(&ContainerLocator::Id(*id)).await,
            Scope::Full(_) => Ok(()),
        }
    }
}

#[cfg(test)]
impl LocalMachine {
    pub(crate) async fn ensure_full_access(
        &self,
        want: ContainerWant,
    ) -> Result<ployz_core::EnsuredContainer, Error> {
        let admitted = self.admit_spec(&want, None, &[], Admit::Record).await?;
        self.ensure_container(want, admitted).await
    }
}

/// What an Operator's Writer start carries beyond the admission of its spec: the versions
/// or rule proof a signature comes from, and a spec that mounts `volume`, so no signature
/// of a spec opens another Volume.
///
/// # Errors
/// Returns [`Error::DeployRefused`] naming what the start lacks.
fn operator_start(
    volume: &DockerVolumeName,
    resolved_spec: &ResolvedServiceSpec,
    versions: &[Signed<ServiceVersion>],
    rule: Option<&RuleProof>,
) -> Result<(), Error> {
    if versions.is_empty() && rule.is_none() {
        return Err(start_refused(resolved_spec, MismatchField::VolumeStart));
    }
    if !resolved_spec
        .volume_graph()
        .mounted_provisioned_volumes()
        .any(|mounted| mounted.source.docker_volume_name() == Some(volume))
    {
        return Err(start_refused(resolved_spec, MismatchField::StartedVolume));
    }
    Ok(())
}

fn start_refused(resolved_spec: &ResolvedServiceSpec, field: MismatchField) -> Error {
    tracing::warn!("Volume start refused for {}: {field}", resolved_spec.name);
    Error::DeployRefused(AuthorityRefusal::Mismatch { field })
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
        AuthorityRefusal::Mismatch { field } => match field.deploy_refusal() {
            Some(text) => text,
            None => return format!("This server can't check the deploy's approval: {refusal}."),
        },
        AuthorityRefusal::NeedsSignature { reach } => {
            return format!(
                "This service {reach}, so this server runs it only from a deploy an approved device signed."
            );
        }
        AuthorityRefusal::SignedName { service } => {
            return format!(
                "{} ran from a signed deploy before, so this server runs it only from a deploy an approved device signed.",
                service.name
            );
        }
        AuthorityRefusal::Unsigned { .. } => {
            "This server runs only deploys an approved device signed."
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
        AuthorityRefusal::Expired => "The device's approval expired. Deploy again.",
        AuthorityRefusal::RetiredBuilder { .. } => {
            "This automatic deploy rule pins a builder Ployz no longer trusts. Approve again under Automatic deploys."
        }
        // A CI or agent key is a key too, and a removed one is unknown here by kind.
        AuthorityRefusal::UnknownKey { .. } => {
            "This server doesn't trust the key that approved this deploy."
        }
        AuthorityRefusal::UnknownPasskey => {
            "This server doesn't trust the passkey that approved this deploy."
        }
        AuthorityRefusal::OutOfScope { code, kind, what } => {
            return format!(
                "The {kind} key {code} that approved this deploy may not approve {what} here."
            );
        }
        AuthorityRefusal::Unscoped { code } => {
            return format!(
                "The CI key {code} that approved this deploy names no environment, so this server runs nothing it signs. Make a new key with ployz key add --ci --env, put its PLOYZ_KEY in CI, then remove this one with ployz key rm {code}."
            );
        }
        AuthorityRefusal::VolumeNotGranted { code, kind, volume } => {
            return format!(
                "The {kind} key {code} that approved this deploy may not mount volume {volume}. A person adds the volume to the key's grant, or deploys this version themselves."
            );
        }
        AuthorityRefusal::Malformed { .. }
        | AuthorityRefusal::BadSignature { .. }
        | AuthorityRefusal::NoClusterIdentity
        | AuthorityRefusal::ProofReplayed => {
            return format!("This server can't check the deploy's approval: {refusal}.");
        }
    };
    text.to_owned()
}

/// A Writer's start of a Container in a preview Namespace, refused as
/// [`ployz_core::admit_preview`] refuses the Container itself, so a start its create would
/// refuse promotes nothing first.
///
/// # Errors
/// Returns [`Error::PreviewRefused`] for a refused preview start.
fn admit_preview_start(
    machine: &ployz_core::MachineId,
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
    admitted: Option<&AdmittedSpec>,
) -> Result<(), Error> {
    let admission = admitted.map_or(ployz_core::Admission::Free, |admitted| {
        admitted.by().admission()
    });
    ployz_core::admit_preview(machine, namespace, admission, &spec.to_requested())
        .map_err(Error::PreviewRefused)
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;
