//! The approval a Device, or a browser key under a passkey's session grant, signs for one
//! Deploy, and the checks a Machine runs before it creates a Container under one.
//!
//! The approval names one [`PayloadDigest`] per Service. A Machine recomputes the digest
//! from the spec it is asked to create, so Cloud can carry the approval but cannot change
//! the image, command, environment, Sealed values, mounts, Config bytes, ports,
//! privileges or hook it approves. The image must also be content-addressed, since a tag
//! names whatever Cloud last wrote under it.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use ts_rs::TS;

use super::{
    canonical::{self, CanonicalError, Timestamp},
    keys::{Approver, Purpose, Signed, Statement},
    proposal::{self, ProposalRef, StepBody, StepKind},
    refusal::{AuthorityRefusal, field, first_mismatch},
    restart::SignerKey,
    rule_draft::{Signs, settings},
    session::{ActionFamily, GrantScope},
    shown::Shown,
    trusted_key::TrustedKeys,
};
use crate::value::hex_newtype;
use crate::{
    ContainerKind, EnvValue, MachineId, Namespace, OperationId, PortPublication, RawVolumeSource,
    RequestedServiceSpec, ResolvedServiceSpec, SealingPublicKey, ServiceName,
};

hex_newtype!(
    /// SHA-256 of one Service's requested spec in canonical JSON, without its replica count
    /// and rollout order, written as 64 lowercase hex digits.
    PayloadDigest, "payload digest", 32, "", "64 lowercase hex digits"
);

impl PayloadDigest {
    /// The digest of `spec` as it runs in `namespace`. Managed Volumes are scoped to
    /// `namespace` first, as the Machine's resolved spec carries them, and `mode` and
    /// `update` are left out: they change how many copies run and in what order, never
    /// what one runs. A field serde writes as `null` is left out, as absent.
    ///
    /// # Errors
    /// Returns a [`CanonicalError`] when the spec has no canonical form, or when scoping
    /// its Volumes to `namespace` makes two of them conflict.
    pub fn of(spec: &RequestedServiceSpec, namespace: &Namespace) -> Result<Self, CanonicalError> {
        let mut spec = spec.clone();
        spec.mount_graph = spec
            .mount_graph
            .scope_to_namespace(namespace)
            .map_err(|error| CanonicalError::Parse(error.to_string()))?;
        Self::of_scoped(&spec, |_| {})
    }

    /// The digest of `spec` with its Volumes already scoped, after `edit` changed its JSON
    /// form, such as a preview template's placeholders.
    ///
    /// # Errors
    /// Returns a [`CanonicalError`] when the spec has no canonical form.
    pub(super) fn of_scoped(
        spec: &RequestedServiceSpec,
        edit: impl FnOnce(&mut Value),
    ) -> Result<Self, CanonicalError> {
        let mut value =
            serde_json::to_value(spec).map_err(|error| CanonicalError::Parse(error.to_string()))?;
        if let Value::Object(fields) = &mut value {
            fields.remove("mode");
            fields.remove("update");
        }
        drop_nulls(&mut value);
        edit(&mut value);
        let body = canonical::to_canonical(&value)?;
        Ok(Self(Sha256::digest(body.as_bytes()).into()))
    }

    /// The first 12 hex digits, for a log line or a person to compare.
    #[must_use]
    pub fn short(&self) -> String {
        let mut text = self.to_string();
        text.truncate(12);
        text
    }
}

pub(super) fn drop_nulls(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.retain(|_, field| !field.is_null());
            fields.values_mut().for_each(drop_nulls);
        }
        Value::Array(items) => items.iter_mut().for_each(drop_nulls),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// Whether `image` names its content: `name@sha256:<64 hex>`, or a bare `sha256:<64 hex>`
/// image ID. Any other reference resolves through a tag the image store's writers choose.
#[must_use]
pub fn content_addressed(image: &str) -> bool {
    let digest = match image.rsplit_once('@') {
        Some((name, digest)) if !name.is_empty() => digest,
        Some(_) => return false,
        None => image,
    };
    digest.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// One Service a Deploy approves, by the digest of the spec its Containers run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ApprovedService {
    pub name: ServiceName,
    pub digest: PayloadDigest,
    /// The content-addressed reference the approver's client pinned the Service's image
    /// to, when the Saved State names a tag. The runner writes it into the spec before
    /// planning, and a Machine creates the Service only from this exact image. None when
    /// the person wrote the digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub image: Option<String>,
}

/// One Service as the approver's client shows it before signing: the digest the approval
/// names, the image, what the spec asks of the host beyond an ordinary Container, and
/// every other setting it signs. The CLI and the browser show these, computed from the
/// specs they sign.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct ApprovedChange {
    pub service: ServiceName,
    /// The image the Servers run, always by digest.
    pub image: String,
    /// The tag the Saved State names, when the client pinned it to `image`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tag: Option<String>,
    pub digest: PayloadDigest,
    /// Privileges, devices, capabilities, PID mode, sysctls, host ports, Bind Mounts and
    /// the hook.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub host: Vec<String>,
    /// Every field the digest covers that holds a value, beside the image, from
    /// [`settings`]. Cloud relays the spec, so a field it planted shows here.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub settings: Vec<String>,
}

impl ApprovedChange {
    /// One change per Service of `target`, sorted by name and distinct, as a
    /// [`DeployApproval`] lists them. A Service in `pins` is approved with its image set
    /// to the pinned reference, so the digest covers the spec the runner sends.
    ///
    /// # Errors
    /// Returns a [`CanonicalError`] when a spec has no [`PayloadDigest`] in `namespace`.
    pub fn of(
        target: &[RequestedServiceSpec],
        namespace: &Namespace,
        pins: &BTreeMap<ServiceName, String>,
    ) -> Result<Vec<Self>, CanonicalError> {
        let mut changes = target
            .iter()
            .map(|spec| {
                let mut pinned = spec.clone();
                let tag = pins
                    .get(&spec.name)
                    .map(|pin| std::mem::replace(&mut pinned.container.image, pin.clone()));
                Ok(Self {
                    service: spec.name.clone(),
                    digest: PayloadDigest::of(&pinned, namespace)?,
                    image: pinned.container.image,
                    tag,
                    host: host_sensitive(spec).into_iter().map(String::from).collect(),
                    settings: settings(spec, Signs::Deploy)
                        .into_iter()
                        .map(String::from)
                        .collect(),
                })
            })
            .collect::<Result<Vec<_>, CanonicalError>>()?;
        changes.sort_by(|left, right| left.service.cmp(&right.service));
        changes.dedup_by(|left, right| left.service == right.service);
        Ok(changes)
    }

    /// `Approved web: nginx@sha256:<64 hex>`, or `nginx:1.29-alpine → @sha256:0123456789ab…`
    /// for a tag this client pinned, then what the spec asks of the host. A digest the
    /// spec names shows whole, since Cloud relays it and a short prefix is cheap to match.
    /// A pin is this client's own registry read of the tag shown, so its prefix is enough.
    #[must_use]
    pub fn line(&self) -> String {
        let image = match (&self.tag, self.image.rsplit_once("@sha256:")) {
            (Some(tag), Some((_, hex))) => Shown::text(tag)
                .and_words(" → @sha256:")
                .and_text(hex.get(..12).unwrap_or(hex))
                .and_words("…"),
            _ => Shown::text(&self.image),
        };
        let mut line: String = Shown::words("Approved ")
            .and_text(self.service.as_str())
            .and_words(": ")
            .and(image)
            .into();
        if !self.host.is_empty() {
            let _ = write!(line, " ({})", self.host.join(", "));
        }
        line
    }

    /// [`Self::line`], then each setting on its own line, indented, as the approver reads
    /// them before signing.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        std::iter::once(self.line())
            .chain(self.settings.iter().map(|setting| format!("  {setting}")))
            .collect()
    }

    #[must_use]
    pub fn approved(&self) -> ApprovedService {
        ApprovedService {
            name: self.service.clone(),
            digest: self.digest,
            image: self.tag.is_some().then(|| self.image.clone()),
        }
    }
}

/// What `spec` asks of the host beyond an ordinary Container.
pub(super) fn host_sensitive(spec: &RequestedServiceSpec) -> Vec<Shown> {
    let container = &spec.container;
    let mut host = Vec::new();
    if container.privileged {
        host.push(Shown::words("privileged"));
    }
    host.extend(container.resources.devices.iter().map(|device| {
        Shown::words("device ")
            .and_text(device.machine_path.as_str())
            .and_words(":")
            .and_text(device.container_path.as_str())
    }));
    host.extend(
        container
            .cap_add
            .iter()
            .map(|cap| Shown::words("adds ").and_text(cap)),
    );
    if let Some(pid_mode) = &container.pid_mode {
        host.push(Shown::words("pid ").and_text(&pid_mode.to_string()));
    }
    host.extend(
        container
            .sysctls
            .keys()
            .map(|key| Shown::words("sysctl ").and_text(key)),
    );
    host.extend(spec.ports.iter().filter_map(|port| match port {
        PortPublication::Host { published_port, .. } => {
            Some(Shown::words("host port ").and_text(&published_port.to_string()))
        }
        PortPublication::Ingress { .. } => None,
    }));
    host.extend(spec.volume_graph().volumes().iter().filter_map(
        |volume| match volume.source.kind() {
            RawVolumeSource::Bind { machine_path, .. } => {
                Some(Shown::words("binds ").and_text(machine_path.as_str()))
            }
            RawVolumeSource::External { .. }
            | RawVolumeSource::Ordinary { .. }
            | RawVolumeSource::Provisioned { .. }
            | RawVolumeSource::Tmpfs { .. } => None,
        },
    ));
    if let Some(hook) = &spec.pre_deploy {
        host.push(
            hook.command
                .as_slice()
                .iter()
                .fold(Shown::words("hook"), |shown, word| {
                    shown.and_words(" ").and_text(word)
                }),
        );
        if hook.privileged == Some(true) {
            host.push(Shown::words("privileged hook"));
        }
    }
    host
}

/// A Device, or a browser key under a session grant, approves one Deploy: the Services it
/// may create in one Cluster and Namespace, on named Machines, for a bounded lifetime.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeployApproval {
    /// The Cluster's Sealing public key, which every Machine holds and Cloud cannot mint.
    pub cluster: SealingPublicKey,
    /// The Deployment.
    pub operation: OperationId,
    pub namespace: Namespace,
    /// Sorted and distinct.
    #[serde(deserialize_with = "canonical::sorted_distinct")]
    pub machines: Vec<MachineId>,
    /// Sorted by name and distinct.
    #[serde(deserialize_with = "services_by_name")]
    pub services: Vec<ApprovedService>,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    /// The key that signed, which the signature must come from.
    pub signer: SignerKey,
    /// Which step of a signed proposal this is, when it is one. Absent from a single
    /// approval, which keeps the bytes a daemon from before deploy steps reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub proposal: Option<ProposalRef>,
}

impl Statement for DeployApproval {
    const PURPOSE: Purpose = Purpose::Workflow;
}

/// A Deploy's approval as a browser key signs it, from [`DeployApproval::browser_draft`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BrowserDraft {
    /// The canonical body to sign as it is.
    pub body: String,
    pub changes: Vec<ApprovedChange>,
    /// `Approved web: nginx@sha256:0123456789ab…` per Service, then its settings, as
    /// [`ApprovedChange::lines`] writes them.
    pub lines: Vec<String>,
}

/// Why a browser can't sign a Deploy.
#[derive(Debug, thiserror::Error)]
pub enum BrowserDraftError {
    #[error(
        "{0} builds from source, and servers that enforce device approval can't run a build \
         yet. Deploy it from an image pinned by digest."
    )]
    Built(ServiceName),
    /// Every Service named by a tag, so one refusal lists each to write by digest.
    #[error("{}", tagged_message(.0))]
    Tag(Vec<TaggedImage>),
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
}

/// A Service whose image a tag names, as [`BrowserDraftError::Tag`] lists it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaggedImage {
    pub service: ServiceName,
    pub image: String,
}

fn tagged_message(tagged: &[TaggedImage]) -> String {
    match tagged {
        [TaggedImage { service, image }] => format!(
            "The dashboard can't check what {service}'s tag {image} points to. Write {service}'s \
             image by digest, which docker buildx imagetools inspect {image} prints."
        ),
        _ => format!(
            "The dashboard can't check what these tags point to: {}. Write each image by digest, \
             which docker buildx imagetools inspect IMAGE prints.",
            tagged
                .iter()
                .map(|TaggedImage { service, image }| format!("{service}'s {image}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// What an approver's client sets around the Services a plan ships.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeployTerms {
    pub cluster: SealingPublicKey,
    /// The Deployment's ID, without its dashes.
    pub operation: OperationId,
    pub machines: Vec<MachineId>,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    pub signer: SignerKey,
    /// The proposal step this approval is, if it is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub proposal: Option<ProposalRef>,
}

impl StepBody for DeployApproval {
    fn kind(&self) -> StepKind {
        StepKind::Deploy
    }
    fn place(&self) -> Option<ProposalRef> {
        self.proposal
    }
    fn signer(&self) -> SignerKey {
        self.signer
    }
    fn cluster(&self) -> SealingPublicKey {
        self.cluster
    }
    fn namespace(&self) -> &Namespace {
        &self.namespace
    }
    fn window(&self) -> (Timestamp, Timestamp) {
        (self.issued_at, self.expires_at)
    }
}

impl DeployApproval {
    /// The approval of every Service of `target` in `namespace` under `terms`, with each
    /// Service in `pins` pinned to its reference, and what each Service shows the
    /// approver. The CLI and Cloud's SDK both build it here, so the browser signs a body
    /// it never hashed.
    ///
    /// # Errors
    /// Returns a [`CanonicalError`] when a spec has no [`PayloadDigest`] in `namespace`.
    pub fn draft(
        terms: DeployTerms,
        namespace: Namespace,
        target: &[RequestedServiceSpec],
        pins: &BTreeMap<ServiceName, String>,
    ) -> Result<(Self, Vec<ApprovedChange>), CanonicalError> {
        let changes = ApprovedChange::of(target, &namespace, pins)?;
        let mut machines = terms.machines;
        machines.sort_unstable();
        machines.dedup();
        let body = Self {
            cluster: terms.cluster,
            operation: terms.operation,
            namespace,
            machines,
            services: changes.iter().map(ApprovedChange::approved).collect(),
            issued_at: terms.issued_at,
            expires_at: terms.expires_at,
            signer: terms.signer,
            proposal: terms.proposal,
        };
        Ok((body, changes))
    }

    /// What a browser key signs for a Deploy of `target` under `terms`: the canonical
    /// body, what each Service shows the approver, and one line per Service. The browser
    /// runs this itself on the plan it read, so it signs no digest Cloud computed. It
    /// can't ask a registry, so a tag is refused rather than pinned.
    ///
    /// # Errors
    /// Refuses a Service built from source, an image named by a tag, and a spec with no
    /// [`PayloadDigest`] in `namespace`.
    pub fn browser_draft(
        terms: DeployTerms,
        namespace: Namespace,
        target: &[RequestedServiceSpec],
    ) -> Result<BrowserDraft, BrowserDraftError> {
        if let Some(spec) = target
            .iter()
            .find(|spec| crate::is_built_image(&spec.container.image))
        {
            return Err(BrowserDraftError::Built(spec.name.clone()));
        }
        let tagged: Vec<TaggedImage> = target
            .iter()
            .filter(|spec| !content_addressed(&spec.container.image))
            .map(|spec| TaggedImage {
                service: spec.name.clone(),
                image: spec.container.image.clone(),
            })
            .collect();
        if !tagged.is_empty() {
            return Err(BrowserDraftError::Tag(tagged));
        }
        let (body, changes) = Self::draft(terms, namespace, target, &BTreeMap::new())?;
        Ok(BrowserDraft {
            body: canonical::to_canonical(&body)?,
            lines: changes.iter().flat_map(ApprovedChange::lines).collect(),
            changes,
        })
    }

    /// What this approval names for `service`.
    #[must_use]
    pub fn service(&self, service: &ServiceName) -> Option<&ApprovedService> {
        self.services
            .binary_search_by(|approved| approved.name.cmp(service))
            .ok()
            .and_then(|at| self.services.get(at))
    }

    /// The digest this approval names for `service`.
    #[must_use]
    pub fn digest(&self, service: &ServiceName) -> Option<PayloadDigest> {
        self.service(service).map(|approved| approved.digest)
    }

    /// Write the pinned image this approval names for `spec`'s Service into `spec`, so
    /// it is the spec the approval's digest was taken over. A Service the approver wrote
    /// by digest keeps its image.
    pub fn pin(&self, spec: &mut RequestedServiceSpec) {
        if let Some(image) = self
            .service(&spec.name)
            .and_then(|approved| approved.image.as_ref())
        {
            spec.container.image.clone_from(image);
        }
    }
}

fn services_by_name<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<ApprovedService>, D::Error> {
    canonical::sorted_by(deserializer, |service: &ApprovedService| &service.name)
}

/// A Container this Machine is asked to create.
#[derive(Clone, Copy, Debug)]
pub struct Creation<'a> {
    pub service: &'a ServiceName,
    pub kind: ContainerKind,
    pub spec: &'a ResolvedServiceSpec,
}

/// What a Machine knows when it checks a deploy approval against one request.
#[derive(Clone, Debug)]
pub struct DeployFacts<'a> {
    /// The Sealing public key this Machine holds, or `None` while it holds none.
    pub cluster: Option<SealingPublicKey>,
    pub machine: &'a MachineId,
    pub trusted: &'a TrustedKeys,
    /// The Namespace of the Container the request creates or acts on.
    pub namespace: &'a Namespace,
    /// The creation asked for, or `None` for a step on a Container that exists.
    pub creation: Option<Creation<'a>>,
    pub now: Timestamp,
}

/// A deploy approval a Machine checked, who approved it, and the digest of the creation
/// it admitted, if any.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeployApproved {
    pub body: DeployApproval,
    pub by: Approver,
    pub digest: Option<PayloadDigest>,
}

/// Check `approval` approves this request, signed by one of this Machine's Devices or by
/// a browser key a trusted passkey lent a grant covering deploys, or riding unsigned as a
/// step of a proposal bundle one of them signed.
///
/// For a creation it recomputes the [`PayloadDigest`] of the requested spec and requires
/// the one the approval names for that Service, a hook in that spec for a hook creation,
/// and a content-addressed image. For a step on an existing Container it checks the
/// signature, Cluster, Machine, Namespace and window only. Consumption is the Machine's.
///
/// # Errors
/// Returns the first [`AuthorityRefusal`] found, a [`AuthorityRefusal::Mismatch`] naming
/// the field for any part of the request the approval does not cover.
pub fn verify(
    approval: &Signed<DeployApproval>,
    facts: &DeployFacts<'_>,
) -> Result<DeployApproved, AuthorityRefusal> {
    let (body, by, cluster) = match &approval.proposal {
        Some(bundle) => {
            proposal::open_step(bundle, approval, facts.trusted, facts.cluster, facts.now)?
        }
        None => open_signed(approval, facts)?,
    };
    first_mismatch([
        (field::CLUSTER, body.cluster == cluster),
        (
            field::MACHINES,
            body.machines.binary_search(facts.machine).is_ok(),
        ),
        (field::ENVIRONMENT, &body.namespace == facts.namespace),
    ])?;
    if let Approver::Browser { grant, .. } = &by {
        grant.body.covers(&GrantScope {
            cluster,
            environment: &body.namespace,
            machines: &body.machines,
            family: ActionFamily::Deploy,
            issued_at: body.issued_at,
            expires_at: body.expires_at,
        })?;
    }
    super::validity::verify(body.issued_at, body.expires_at, facts.now)?;
    if let Some(creation) = facts.creation {
        refuse_preview_parts(facts.namespace, creation.spec)?;
    }
    let digest = facts
        .creation
        .map(|creation| creation_digest(&body, creation))
        .transpose()?;
    Ok(DeployApproved { body, by, digest })
}

/// Refuse a creation, outside a preview rule, in a reserved preview Namespace or carrying a
/// parent's value a preview inherits: only a preview rule may admit either, so preview code
/// shares a Namespace with nothing else and an inherited value opens nowhere else.
///
/// # Errors
/// Returns [`AuthorityRefusal::Mismatch`] naming the Namespace or the Sealed value.
pub fn refuse_preview_parts(
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
) -> Result<(), AuthorityRefusal> {
    if super::preview::is_preview_namespace(namespace) {
        return Err(AuthorityRefusal::Mismatch {
            field: "a preview Namespace",
        });
    }
    if sealed_values(spec).any(|value| value.environment.is_some()) {
        return Err(AuthorityRefusal::Mismatch {
            field: super::preview::SEALED_VALUE,
        });
    }
    Ok(())
}

fn sealed_values(spec: &ResolvedServiceSpec) -> impl Iterator<Item = &crate::SealedValue> {
    let hook = spec
        .pre_deploy
        .iter()
        .flat_map(|hook| hook.environment.values());
    spec.container
        .environment
        .values()
        .chain(hook)
        .flat_map(EnvValue::sealed_parts)
}

/// Whether `by` may approve a Container that opens the Sealed values `spec` carries, in its
/// environment or its hook's. A Device opens any. A browser key under a session grant
/// opens only values sealed for its grant's Environment, so a dashboard holding a session
/// in one Environment cannot deploy a Container that opens another's.
#[must_use]
pub fn sealed_scope_allows(by: &Approver, spec: &ResolvedServiceSpec) -> bool {
    let Approver::Browser { grant, .. } = by else {
        return true;
    };
    let hook = spec
        .pre_deploy
        .iter()
        .flat_map(|hook| hook.environment.values());
    let sealed = spec
        .container
        .environment
        .values()
        .chain(hook)
        .any(EnvValue::is_sealed);
    !sealed
        || spec
            .container
            .sealed_identity
            .as_ref()
            .is_some_and(|identity| identity.environment == grant.body.environment_id)
}

fn open_signed(
    approval: &Signed<DeployApproval>,
    facts: &DeployFacts<'_>,
) -> Result<(DeployApproval, Approver, SealingPublicKey), AuthorityRefusal> {
    let approvers = approval.approvers(facts.trusted, facts.now)?;
    let body = approval.open()?;
    let Some(cluster) = facts.cluster else {
        return Err(AuthorityRefusal::NoClusterIdentity);
    };
    if body.proposal.is_some() {
        return Err(AuthorityRefusal::Mismatch {
            field: "a proposal",
        });
    }
    let by = approvers
        .into_iter()
        .find(|approver| approver.signs_as(body.signer))
        .ok_or(AuthorityRefusal::Mismatch {
            field: "the signer",
        })?;
    Ok((body, by, cluster))
}

fn creation_digest(
    body: &DeployApproval,
    creation: Creation<'_>,
) -> Result<PayloadDigest, AuthorityRefusal> {
    let approved = body
        .service(creation.service)
        .ok_or(AuthorityRefusal::Mismatch { field: "a Service" })?;
    approved_creation(approved, &body.namespace, creation)
}

/// Check `creation` is the Service `approved` names, in `namespace`: the same name, the
/// pinned image when there is one, a content-addressed image, a hook for a hook creation,
/// and a spec whose [`PayloadDigest`] is the approved one. A Deploy's approval checks each
/// creation here, and so does a Volume Move's start.
///
/// # Errors
/// Returns [`AuthorityRefusal::Mismatch`] naming the Service, the hook, the image or the
/// payload that differs.
pub fn approved_creation(
    approved: &ApprovedService,
    namespace: &Namespace,
    creation: Creation<'_>,
) -> Result<PayloadDigest, AuthorityRefusal> {
    let payload = AuthorityRefusal::Mismatch { field: "a payload" };
    if &approved.name != creation.service || &creation.spec.name != creation.service {
        return Err(AuthorityRefusal::Mismatch { field: "a Service" });
    }
    if creation.kind == ContainerKind::PreDeployHook && creation.spec.pre_deploy.is_none() {
        return Err(AuthorityRefusal::Mismatch { field: "a hook" });
    }
    if approved
        .image
        .as_ref()
        .is_some_and(|image| image != &creation.spec.container.image)
    {
        return Err(AuthorityRefusal::Mismatch { field: "an image" });
    }
    let digest =
        PayloadDigest::of(&creation.spec.to_requested(), namespace).map_err(|_| payload.clone())?;
    if digest != approved.digest {
        return Err(payload);
    }
    if !content_addressed(&creation.spec.container.image) {
        return Err(AuthorityRefusal::Mismatch { field: "an image" });
    }
    Ok(digest)
}

#[cfg(test)]
#[path = "deploy_tests.rs"]
mod tests;
