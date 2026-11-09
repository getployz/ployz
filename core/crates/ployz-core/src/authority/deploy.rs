//! The approval a Device, or a browser key under a passkey's session grant, signs for one
//! Deploy, and the checks a Machine runs before it creates a Container under one.
//!
//! The approval names one [`PayloadDigest`] per Service. A Machine recomputes the digest
//! from the spec it is asked to create, so Cloud can carry the approval but cannot change
//! the image, command, environment, Sealed values, mounts, Config bytes, ports,
//! privileges or hook it approves. The image must also be content-addressed, since a tag
//! names whatever Cloud last wrote under it.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use ts_rs::TS;

use super::{
    canonical::{self, CanonicalError, Timestamp, hex_bytes, write_hex},
    keys::{Approver, Purpose, Signed, Statement},
    proposal::{self, ProposalRef, StepBody, StepKind},
    refusal::AuthorityRefusal,
    restart::SignerKey,
    session::{ActionFamily, GrantScope},
    trusted_key::TrustedKeys,
};
use crate::{
    ContainerKind, MachineId, Namespace, OperationId, PortPublication, RawVolumeSource,
    RequestedServiceSpec, ResolvedServiceSpec, SealingPublicKey, ServiceName,
};

/// SHA-256 of one Service's requested spec in canonical JSON, without its replica count
/// and rollout order, written as 64 lowercase hex digits.
#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct PayloadDigest([u8; 32]);

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
        let mut value = serde_json::to_value(&spec)
            .map_err(|error| CanonicalError::Parse(error.to_string()))?;
        if let Value::Object(fields) = &mut value {
            fields.remove("mode");
            fields.remove("update");
        }
        drop_nulls(&mut value);
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

    /// # Errors
    /// Returns a [`crate::ValueError`] unless `value` is 64 lowercase hex digits.
    pub fn parse(value: &str) -> Result<Self, crate::ValueError> {
        hex_bytes(value).map(Self).ok_or_else(|| {
            crate::ValueError::new("payload digest", value, "64 lowercase hex digits")
        })
    }
}

fn drop_nulls(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.retain(|_, field| !field.is_null());
            fields.values_mut().for_each(drop_nulls);
        }
        Value::Array(items) => items.iter_mut().for_each(drop_nulls),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

impl fmt::Display for PayloadDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(&self.0, formatter)
    }
}

impl fmt::Debug for PayloadDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PayloadDigest({self})")
    }
}

impl TryFrom<String> for PayloadDigest {
    type Error = crate::ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<PayloadDigest> for String {
    fn from(digest: PayloadDigest) -> Self {
        digest.to_string()
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
/// names, the image, and what the spec asks of the host beyond an ordinary Container.
/// The CLI and the browser show these, computed from the specs they sign.
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
                    host: host_sensitive(spec),
                })
            })
            .collect::<Result<Vec<_>, CanonicalError>>()?;
        changes.sort_by(|left, right| left.service.cmp(&right.service));
        changes.dedup_by(|left, right| left.service == right.service);
        Ok(changes)
    }

    /// `Approved web: nginx@sha256:0123456789ab…`, or `nginx:1.29-alpine → @sha256:0123456789ab…`
    /// for a pinned tag, then what the spec asks of the host.
    #[must_use]
    pub fn line(&self) -> String {
        let image = match (&self.tag, self.image.rsplit_once("@sha256:")) {
            (Some(tag), Some((_, hex))) => {
                format!("{tag} → @sha256:{}…", hex.get(..12).unwrap_or(hex))
            }
            (None, Some((name, hex))) => {
                format!("{name}@sha256:{}…", hex.get(..12).unwrap_or(hex))
            }
            (_, None) => self.image.clone(),
        };
        let mut line = format!("Approved {}: {image}", self.service);
        if !self.host.is_empty() {
            let _ = write!(line, " ({})", self.host.join(", "));
        }
        line
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
fn host_sensitive(spec: &RequestedServiceSpec) -> Vec<String> {
    let container = &spec.container;
    let mut host = Vec::new();
    if container.privileged {
        host.push("privileged".to_owned());
    }
    host.extend(container.resources.devices.iter().map(|device| {
        format!(
            "device {}:{}",
            device.machine_path.as_str(),
            device.container_path.as_str()
        )
    }));
    host.extend(container.cap_add.iter().map(|cap| format!("adds {cap}")));
    if let Some(pid_mode) = &container.pid_mode {
        host.push(format!("pid {pid_mode}"));
    }
    host.extend(container.sysctls.keys().map(|key| format!("sysctl {key}")));
    host.extend(spec.ports.iter().filter_map(|port| match port {
        PortPublication::Host { published_port, .. } => Some(format!("host port {published_port}")),
        PortPublication::Ingress { .. } => None,
    }));
    host.extend(spec.volume_graph().volumes().iter().filter_map(
        |volume| match volume.source.kind() {
            RawVolumeSource::Bind { machine_path, .. } => {
                Some(format!("binds {}", machine_path.as_str()))
            }
            RawVolumeSource::External { .. }
            | RawVolumeSource::Ordinary { .. }
            | RawVolumeSource::Provisioned { .. }
            | RawVolumeSource::Tmpfs { .. } => None,
        },
    ));
    if let Some(hook) = &spec.pre_deploy {
        host.push(format!("hook {}", hook.command.as_slice().join(" ")));
        if hook.privileged == Some(true) {
            host.push("privileged hook".to_owned());
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
    #[serde(deserialize_with = "sorted_distinct")]
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

fn sorted_distinct<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<MachineId>, D::Error> {
    let machines = Vec::<MachineId>::deserialize(deserializer)?;
    if !machines.is_sorted_by(|left, right| left < right) {
        return Err(serde::de::Error::custom(
            "Machines must be sorted and distinct",
        ));
    }
    Ok(machines)
}

fn services_by_name<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<ApprovedService>, D::Error> {
    let services = Vec::<ApprovedService>::deserialize(deserializer)?;
    if !services.is_sorted_by(|left, right| left.name < right.name) {
        return Err(serde::de::Error::custom(
            "Services must be sorted by name and distinct",
        ));
    }
    Ok(services)
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
    let checks = [
        ("a Cluster", body.cluster == cluster),
        (
            "Machines",
            body.machines.binary_search(facts.machine).is_ok(),
        ),
        ("an Environment", &body.namespace == facts.namespace),
    ];
    if let Some((field, _)) = checks.iter().find(|(_, holds)| !holds) {
        return Err(AuthorityRefusal::Mismatch { field });
    }
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
    let digest = facts
        .creation
        .map(|creation| creation_digest(&body, creation))
        .transpose()?;
    Ok(DeployApproved { body, by, digest })
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
    let payload = AuthorityRefusal::Mismatch { field: "a payload" };
    let approved = body
        .service(creation.service)
        .filter(|_| &creation.spec.name == creation.service)
        .ok_or(AuthorityRefusal::Mismatch { field: "a Service" })?;
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
    let digest = PayloadDigest::of(&creation.spec.to_requested(), &body.namespace)
        .map_err(|_| payload.clone())?;
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
