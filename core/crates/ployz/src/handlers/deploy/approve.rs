//! A Device's approval of one Deploy: when a Server of the Cluster enforces Device
//! approval, `ployz deploy` lowers the plan as Cloud's runner will, signs the digest of
//! each Service's spec with this Device's key, and admits the Deploy with it. Each
//! Server then creates only the specs this Device approved.

use std::fmt::Write as _;

use clap::ArgMatches;
use ployz_core::authority::{
    ApprovedService, DeployApproval, PayloadDigest, Signed, Signer, SignerKey, Timestamp,
    deploy::content_addressed,
};
use ployz_core::{
    MachineObservation, OperationId, RequestedServiceSpec, RpcErrorCode, Sealing, SealingPublicKey,
    ServiceName,
};
use ployz_store::{DeploymentId, EnvironmentRef, PlanQuery};
use serde::Serialize;
use serde_json::json;

use super::super::store::Store;
use super::super::{Error, server};
use crate::ui::Hint;

/// What this Device approved for one Service, as `ployz deploy` prints it.
#[derive(Clone, Debug, Serialize)]
pub(in crate::handlers) struct Approved {
    pub(in crate::handlers) service: ServiceName,
    pub(in crate::handlers) image: String,
    pub(in crate::handlers) digest: PayloadDigest,
    /// What the spec asks of the host: privileges, host ports, Bind Mounts, the hook.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(in crate::handlers) host: Vec<String>,
}

/// A signed approval, the plan version it approves, and what it approved.
pub(in crate::handlers) struct DeviceApproval {
    pub(in crate::handlers) signed: Signed<DeployApproval>,
    pub(in crate::handlers) version: String,
    pub(in crate::handlers) approved: Vec<Approved>,
}

/// The Cluster as this Device sees it: the Sealing key every Server holds and the
/// Servers, when any of them enforces Device approval.
struct Enforcing {
    cluster: SealingPublicKey,
    machines: Vec<ployz_core::MachineId>,
}

/// Sign Deployment `id`'s approval with this CLI's Device key, when it holds one and a
/// Server of the Cluster enforces Device approval. None otherwise: the Deploy runs as
/// before, and a Server that enforces refuses what nobody approved.
///
/// # Errors
/// Refuses before admission when a Service's image is not pinned by digest, and
/// returns the plan's or the key's error.
pub(in crate::handlers) fn sign(
    matches: &ArgMatches,
    store: &Store,
    id: &DeploymentId,
    (environment, services): (&EnvironmentRef, &[ServiceName]),
) -> Result<Option<DeviceApproval>, Error> {
    let key = super::super::device::key::path(&super::super::config_path(matches)?);
    let Ok(Some(secret)) = super::super::device::key::load(&key) else {
        return Ok(None);
    };
    let Some(enforcing) = enforcing(matches) else {
        return Ok(None);
    };
    let plan = store.read(&PlanQuery {
        environment: environment.clone(),
        services: services.to_vec(),
        target: true,
    })?;
    let target = plan.target.unwrap_or_default();
    if let Some(tagged) = target
        .iter()
        .find(|spec| !content_addressed(&spec.container.image))
    {
        return Err(unpinned(matches, tagged));
    }
    let mut approved = target
        .iter()
        .map(|spec| {
            let digest = PayloadDigest::of(spec, &plan.namespace).map_err(Error::command)?;
            Ok(Approved {
                service: spec.name.clone(),
                image: spec.container.image.clone(),
                digest,
                host: host_sensitive(spec),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    approved.sort_by(|left, right| left.service.cmp(&right.service));
    approved.dedup_by(|left, right| left.service == right.service);
    let signer = Signer::from_seed(&secret);
    let issued_at = Timestamp::now();
    let body = DeployApproval {
        cluster: enforcing.cluster,
        operation: OperationId::from_uuid(id.as_str()).map_err(Error::command)?,
        namespace: plan.namespace,
        machines: enforcing.machines,
        services: approved
            .iter()
            .map(|approved| ApprovedService {
                name: approved.service.clone(),
                digest: approved.digest,
            })
            .collect(),
        issued_at,
        expires_at: issued_at.plus(ployz_core::authority::restart::MAX_LIFETIME_MILLIS),
        signer: SignerKey::Device(signer.public().into()),
    };
    let signed = Signed::sign(&body, &signer).map_err(Error::command)?;
    for approved in &approved {
        crate::ui::stream(format_args!("{}", line(approved)));
    }
    Ok(Some(DeviceApproval {
        signed,
        version: plan.version,
        approved,
    }))
}

/// The Cluster's Sealing key and Servers when one of them enforces Device approval.
/// Servers this CLI cannot reach say nothing: they refuse what nobody approved.
fn enforcing(matches: &ArgMatches) -> Option<Enforcing> {
    let context = matches
        .try_get_one::<String>("context")
        .ok()
        .flatten()
        .map(String::as_str);
    let observed = super::super::runtime().ok()?.block_on(async {
        let mut client = server::connect(matches, context).await.ok()?;
        client.machines().await.ok()
    })?;
    enforced(&observed)
}

fn enforced(observed: &[MachineObservation]) -> Option<Enforcing> {
    if !observed
        .iter()
        .any(|observed| observed.machine.runtime.device_enforcement == Some(true))
    {
        return None;
    }
    let cluster = observed
        .iter()
        .find_map(|observed| match observed.machine.runtime.sealing {
            Some(Sealing::Held { public_key }) => Some(public_key),
            _ => None,
        })?;
    let mut machines: Vec<_> = observed
        .iter()
        .map(|observed| observed.machine.id)
        .collect();
    machines.sort_unstable();
    machines.dedup();
    Some(Enforcing { cluster, machines })
}

fn unpinned(matches: &ArgMatches, spec: &RequestedServiceSpec) -> Error {
    let next = super::super::store::next(
        matches,
        &["set", &format!("{}.image=…", spec.name)],
    );
    Error::detailed(
        RpcErrorCode::InvalidArgument,
        format!(
            "Pin {}'s image by digest to deploy to Servers that enforce.",
            spec.name
        ),
        json!({ "service": spec.name, "image": spec.container.image }),
    )
    .hint(Hint::Next(next))
}

/// `Approved web: nginx@sha256:0123456789ab…`, then what the spec asks of the host.
pub(in crate::handlers) fn line(approved: &Approved) -> String {
    let image = match approved.image.rsplit_once("@sha256:") {
        Some((name, hex)) => format!("{name}@sha256:{}…", hex.get(..12).unwrap_or(hex)),
        None => approved.image.clone(),
    };
    let mut line = format!("Approved {}: {image}", approved.service);
    if !approved.host.is_empty() {
        let _ = write!(line, " ({})", approved.host.join(", "));
    }
    line
}

/// What `spec` asks of the host beyond an ordinary Container.
fn host_sensitive(spec: &RequestedServiceSpec) -> Vec<String> {
    let container = &spec.container;
    let mut host = Vec::new();
    if container.privileged {
        host.push("privileged".to_owned());
    }
    host.extend(container.cap_add.iter().map(|cap| format!("adds {cap}")));
    if let Some(pid_mode) = &container.pid_mode {
        host.push(format!("pid {pid_mode}"));
    }
    host.extend(container.sysctls.keys().map(|key| format!("sysctl {key}")));
    host.extend(spec.ports.iter().filter_map(|port| match port {
        ployz_core::PortPublication::Host { published_port, .. } => {
            Some(format!("host port {published_port}"))
        }
        ployz_core::PortPublication::Ingress { .. } => None,
    }));
    host.extend(spec.volume_graph().volumes().iter().filter_map(
        |volume| match volume.source.kind() {
            ployz_core::RawVolumeSource::Bind { machine_path, .. } => {
                Some(format!("binds {}", machine_path.as_str()))
            }
            ployz_core::RawVolumeSource::External { .. }
            | ployz_core::RawVolumeSource::Ordinary { .. }
            | ployz_core::RawVolumeSource::Provisioned { .. }
            | ployz_core::RawVolumeSource::Tmpfs { .. } => None,
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

#[cfg(test)]
#[path = "approve_tests.rs"]
mod tests;
