//! Observer-local ingress derivation, watching, and shared filesystem state.

use ployz_core::{
    ContainerAddress, ContainerId, ContainerObservation, HttpProtocol, IngressHost,
    IngressProxyBackend, IngressProxyFragment, Machine, MachineId, PortPublication,
    QualifiedService, ServiceContainer, hostname_owners, service_containers, serving_containers,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    future::Future,
    io,
    num::NonZeroU16,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

use crate::{
    corrosion::{
        CertificateChallenge, CertificateMaterial, CertificateRow, Error as CorrosionError,
        ReplicatedStore, Subscription,
    },
    filesystem::{atomic_write, set_ployz_group},
};

const CERTS_DIR: &str = "certs";
const WATCH_DEBOUNCE: Duration = Duration::from_millis(300);
const WATCH_RETRY: Duration = Duration::from_secs(1);
const WATCH_RESNAPSHOT: Duration = Duration::from_secs(5);

/// True when this observation is the reserved Ingress Proxy Service.
#[must_use]
pub(crate) fn is_system_ingress(observation: &ContainerObservation) -> bool {
    observation.identity() == QualifiedService::system_ingress()
}

pub(crate) mod caddy;
pub(crate) mod envoy;
pub(crate) mod zentinel;

use caddy::CaddyAdmin;

/// Run the watcher for the immutable Cluster Ingress Proxy Backend.
pub(crate) async fn run(
    machine: Machine,
    replicated: ReplicatedStore,
    data_dir: PathBuf,
    runtime_dir: PathBuf,
    docker: Option<crate::docker::LocalDocker>,
    shutdown: CancellationToken,
) -> io::Result<()> {
    match replicated
        .ingress_proxy_backend()
        .await
        .map_err(io::Error::other)?
    {
        IngressProxyBackend::Caddy => {
            caddy::run(
                machine,
                replicated,
                caddy::config_path(&data_dir),
                runtime_dir.join("caddy/admin.sock"),
                shutdown,
            )
            .await
        }
        IngressProxyBackend::Zentinel => {
            let docker =
                docker.ok_or_else(|| io::Error::other("Zentinel Ingress Proxy requires Docker"))?;
            zentinel::watch(
                machine,
                replicated,
                zentinel::config_path(&data_dir),
                docker,
                shutdown,
            )
            .await
        }
        IngressProxyBackend::Envoy => {
            let docker =
                docker.ok_or_else(|| io::Error::other("Envoy Ingress Proxy requires Docker"))?;
            envoy::watch(
                machine,
                replicated,
                envoy::config_path(&data_dir),
                docker,
                shutdown,
            )
            .await
        }
    }
}

/// Fully derived observer-local input to ingress rendering and application.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct IngressProjection {
    /// Local Machine whose ingress process consumes this projection.
    pub(super) machine: Machine,
    /// Hostname-grouped ingress routes in stable hostname order.
    pub(super) sites: Vec<IngressSite>,
    /// Ordered Serving Container addresses available to tagged fragments.
    pub(super) upstreams: BTreeMap<QualifiedService, Vec<ContainerAddress>>,
    /// Newest healthy local reserved-service fragment.
    pub(super) global_fragment: Option<IngressProxyFragment>,
    /// Newest healthy tagged fragment for each user Service.
    pub(super) service_fragments: BTreeMap<QualifiedService, IngressProxyFragment>,
}

/// All projected ingress state for one hostname.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct IngressSite {
    /// Published hostname or certificate hostname.
    pub(super) hostname: IngressHost,
    /// Singular Hostname Owner and its protocol publications, when published.
    pub(super) publication: Option<IngressPublication>,
    /// Ployz-owned certificate state for this hostname, when present.
    pub(super) certificate: Option<ProjectedCertificate>,
}

/// A hostname's singular owner and at-most-one publication per protocol.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct IngressPublication {
    /// Qualified Service that owns the hostname.
    pub(super) owner: QualifiedService,
    /// Ordered Serving Container endpoints published over HTTP.
    pub(super) http: Option<Vec<IngressEndpoint>>,
    /// Ordered Serving Container endpoints published over HTTPS.
    pub(super) https: Option<Vec<IngressEndpoint>>,
}

/// One Serving Container address and container port.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct IngressEndpoint {
    /// Serving Container address.
    pub(super) address: ContainerAddress,
    /// Published container port.
    pub(super) port: NonZeroU16,
}

/// Certificate state that can affect ingress behavior or generated output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct ProjectedCertificate {
    /// Pending HTTP-01 challenge.
    pub(super) challenge: Option<CertificateChallenge>,
    /// Certificate and private key served as given.
    pub(super) material: Option<CertificateMaterial>,
    /// Operator-visible issuance refusal included in generated output.
    pub(super) last_error: Option<String>,
}

impl IngressSite {
    /// Ordered endpoints published for one HTTP protocol, when published.
    #[must_use]
    pub(super) fn route(&self, protocol: HttpProtocol) -> Option<&[IngressEndpoint]> {
        let publication = self.publication.as_ref()?;
        match protocol {
            HttpProtocol::Http => publication.http.as_deref(),
            HttpProtocol::Https => publication.https.as_deref(),
        }
    }

    /// Pending HTTP-01 challenge, when present.
    #[must_use]
    pub(super) fn challenge(&self) -> Option<&CertificateChallenge> {
        self.certificate
            .as_ref()
            .and_then(|certificate| certificate.challenge.as_ref())
    }

    /// Ployz-owned certificate material, when present.
    #[must_use]
    pub(super) fn material(&self) -> Option<&CertificateMaterial> {
        self.certificate
            .as_ref()
            .and_then(|certificate| certificate.material.as_ref())
    }
}

impl IngressProjection {
    /// Derive the complete ingress-relevant value from replicated observations.
    #[must_use]
    pub(crate) fn derive(
        machine: &Machine,
        observations: &[ContainerObservation],
        certificates: &BTreeMap<IngressHost, CertificateRow>,
    ) -> Self {
        // TODO: keep the ingress projection membership-blind until the membership model is
        // intentionally changed across replicated projections.
        let containers = service_containers(observations.iter().cloned());
        let owners = hostname_owners(containers.iter().map(ServiceContainer::as_observation));
        let mut sites = BTreeMap::<IngressHost, SiteBuilder>::new();
        for container in &containers {
            let observation = container.as_observation();
            let owner = observation.identity();
            for port in &observation.resolved_spec.ports {
                let PortPublication::Ingress {
                    hostname,
                    http_protocol,
                    ..
                } = port
                else {
                    continue;
                };
                let Some(hostname) = hostname.as_explicit_host() else {
                    continue;
                };
                if owners.get(hostname) == Some(&owner) {
                    sites
                        .entry(hostname.clone())
                        .or_default()
                        .publish(owner.clone(), *http_protocol);
                }
            }
        }

        let mut serving = serving_containers(&containers);
        serving.sort_by_key(|serving| container_order(&machine.id, serving.as_container()));
        let mut upstreams = BTreeMap::<QualifiedService, Vec<ContainerAddress>>::new();
        for serving in &serving {
            let observation = serving.as_observation();
            let address = serving.address();
            let owner = observation.identity();
            upstreams.entry(owner.clone()).or_default().push(address);
            for port in &observation.resolved_spec.ports {
                let PortPublication::Ingress {
                    hostname,
                    container_port,
                    http_protocol,
                    ..
                } = port
                else {
                    continue;
                };
                let Some(hostname) = hostname.as_explicit_host() else {
                    continue;
                };
                if owners.get(hostname) == Some(&owner) {
                    sites
                        .get_mut(hostname)
                        .expect("published hostname was collected above")
                        .publish(owner.clone(), *http_protocol)
                        .push(IngressEndpoint {
                            address,
                            port: *container_port,
                        });
                }
            }
        }

        for (hostname, row) in certificates {
            sites.entry(hostname.clone()).or_default().certificate = Some(ProjectedCertificate {
                challenge: row.challenge().cloned(),
                material: row.material().cloned(),
                last_error: row.last_error().map(ToOwned::to_owned),
            });
        }
        let sites = sites
            .into_iter()
            .map(|(hostname, site)| site.into_site(hostname))
            .collect();
        let global_fragment = containers
            .iter()
            .filter(|container| {
                let observation = container.as_observation();
                observation.runtime.is_healthy()
                    && is_system_ingress(observation)
                    && observation.machine_id == machine.id
            })
            .max_by_key(|container| creation_key(container))
            .and_then(|container| {
                container
                    .as_observation()
                    .resolved_spec
                    .ingress_proxy_fragment
                    .clone()
            });
        let mut newest = BTreeMap::<QualifiedService, &ServiceContainer>::new();
        for container in containers
            .iter()
            .filter(|container| container.as_observation().runtime.is_healthy())
        {
            let identity = container.as_observation().identity();
            if is_system_ingress(container.as_observation()) {
                continue;
            }
            newest
                .entry(identity)
                .and_modify(|current| {
                    if creation_key(container) > creation_key(current) {
                        *current = container;
                    }
                })
                .or_insert(container);
        }
        let service_fragments: BTreeMap<_, _> = newest
            .into_iter()
            .filter_map(|(identity, container)| {
                container
                    .as_observation()
                    .resolved_spec
                    .ingress_proxy_fragment
                    .clone()
                    .map(|fragment| (identity, fragment))
            })
            .collect();
        if global_fragment.is_none() && service_fragments.is_empty() {
            upstreams.clear();
        }
        Self {
            machine: machine.clone(),
            sites,
            upstreams,
            global_fragment,
            service_fragments,
        }
    }
}

#[derive(Default)]
struct SiteBuilder {
    owner: Option<QualifiedService>,
    http: Option<Vec<IngressEndpoint>>,
    https: Option<Vec<IngressEndpoint>>,
    certificate: Option<ProjectedCertificate>,
}

impl SiteBuilder {
    fn publish(
        &mut self,
        owner: QualifiedService,
        protocol: HttpProtocol,
    ) -> &mut Vec<IngressEndpoint> {
        debug_assert!(self.owner.as_ref().is_none_or(|current| current == &owner));
        self.owner.get_or_insert(owner);
        match protocol {
            HttpProtocol::Http => self.http.get_or_insert_default(),
            HttpProtocol::Https => self.https.get_or_insert_default(),
        }
    }

    fn into_site(self, hostname: IngressHost) -> IngressSite {
        let publication = self.owner.map(|owner| IngressPublication {
            owner,
            http: self.http,
            https: self.https,
        });
        IngressSite {
            hostname,
            publication,
            certificate: self.certificate,
        }
    }
}

fn container_order<'container>(
    local_machine: &MachineId,
    container: &'container ServiceContainer,
) -> (bool, QualifiedService, i64, &'container str) {
    let observation = container.as_observation();
    (
        observation.machine_id != *local_machine,
        observation.identity(),
        observation.created_at_unix_nanos,
        observation.container_id.as_str(),
    )
}

fn creation_key(container: &ServiceContainer) -> (i64, &str) {
    let observation = container.as_observation();
    (
        observation.created_at_unix_nanos,
        observation.container_id.as_str(),
    )
}

fn newest_local_ingress(
    machine: &Machine,
    observations: &[ContainerObservation],
) -> Option<ServiceContainer> {
    service_containers(observations.iter().cloned())
        .into_iter()
        .filter(|container| {
            let observation = container.as_observation();
            observation.machine_id == machine.id && is_system_ingress(observation)
        })
        .max_by(|left, right| creation_key(left).cmp(&creation_key(right)))
}

/// Watch replicated ingress inputs and concretely apply changed projections to Caddy.
///
/// # Errors
///
/// Returns when connecting to Caddy administration fails. Replicated-store
/// subscription failures are retried until shutdown.
pub(crate) async fn watch_caddy<A: CaddyAdmin, Connect, ConnectFuture>(
    machine: Machine,
    replicated: ReplicatedStore,
    config_file: PathBuf,
    shutdown: CancellationToken,
    mut connect: Connect,
) -> io::Result<()>
where
    Connect: FnMut() -> ConnectFuture,
    ConnectFuture: Future<Output = Result<Option<A>, caddy::Error>>,
{
    watch(
        machine,
        replicated,
        shutdown,
        |machine, observations, certificates| {
            let process = newest_local_ingress(machine, observations)
                .map(|container| container.as_observation().container_id);
            Ok((
                IngressProjection::derive(machine, observations, certificates),
                process,
            ))
        },
        async move |(projection, _process): &(IngressProjection, Option<ContainerId>)| {
            let applied = async {
                let admin = connect().await.map_err(io::Error::other)?;
                apply_caddy(projection, &config_file, admin.as_ref())
                    .await
                    .map_err(io::Error::other)
            }
            .await;
            match applied {
                Ok(()) => WatchApply::Applied,
                Err(error) => WatchApply::Retry(error),
            }
        },
    )
    .await
}

async fn watch<Input, Derive, Apply>(
    machine: Machine,
    replicated: ReplicatedStore,
    shutdown: CancellationToken,
    mut derive: Derive,
    mut apply: Apply,
) -> io::Result<()>
where
    Input: Eq,
    Derive: FnMut(
        &Machine,
        &[ContainerObservation],
        &BTreeMap<IngressHost, CertificateRow>,
    ) -> io::Result<Input>,
    Apply: for<'input> AsyncFnMut(&'input Input) -> WatchApply,
{
    let mut last_input = None;
    'watch: loop {
        let subscriptions = async {
            tokio::try_join!(
                replicated.subscribe_container_changes(),
                replicated.subscribe_certificate_changes(),
            )
        };
        let changes = tokio::select! {
            changes = subscriptions => changes,
            () = shutdown.cancelled() => return Ok(()),
        };
        let (mut container_changes, mut certificate_changes) = match changes {
            Ok(changes) => changes,
            Err(error) => {
                if wait_to_retry(&error, &shutdown).await {
                    continue;
                }
                return Ok(());
            }
        };
        loop {
            let input = match (
                replicated.containers().await,
                replicated.certificate_state().await,
            ) {
                (Ok(containers), Ok(certificates)) => {
                    derive(&machine, &containers.observations, &certificates)
                }
                (Err(error), _) | (_, Err(error)) => Err(io::Error::other(error)),
            };
            match input {
                Ok(input) if last_input.as_ref() != Some(&input) => match apply(&input).await {
                    WatchApply::Applied => last_input = Some(input),
                    WatchApply::Retry(error) => {
                        eprintln!("failed to update Ingress Proxy configuration: {error}");
                        if wait_before_retry(&shutdown).await {
                            continue;
                        }
                        return Ok(());
                    }
                    WatchApply::WaitForChange(error) => {
                        eprintln!("Ingress Proxy configuration awaits changed input: {error}");
                        last_input = Some(input);
                    }
                },
                Ok(_) => {}
                Err(error) => {
                    last_input = None;
                    eprintln!("failed to rebuild ingress projection: {error}");
                }
            }
            match wait_for_debounced_change(
                &mut container_changes,
                &mut certificate_changes,
                &shutdown,
            )
            .await
            {
                Ok(DebouncedChange::Changed) => {}
                Ok(DebouncedChange::Resubscribe) => continue 'watch,
                Ok(DebouncedChange::Shutdown) => return Ok(()),
                Err(error) if wait_to_retry(&error, &shutdown).await => continue 'watch,
                Err(_) => return Ok(()),
            }
        }
    }
}

/// Write shared certificate state and concretely reconcile Caddy.
///
/// # Errors
///
/// Returns when certificate filesystem operations or Caddy reconciliation fail.
pub(crate) async fn apply_caddy<A: CaddyAdmin>(
    projection: &IngressProjection,
    config_file: &Path,
    admin: Option<&A>,
) -> Result<(), caddy::Error> {
    write_certificate_files(
        config_file,
        &projection.sites,
        0o600,
        prepare_directory,
        set_ployz_group,
    )?;
    caddy::reconcile(projection, config_file, admin).await?;
    remove_stale_certificate_files(config_file, &projection.sites)?;
    Ok(())
}

enum DebouncedChange {
    Changed,
    Resubscribe,
    Shutdown,
}

enum WatchApply {
    Applied,
    Retry(io::Error),
    WaitForChange(io::Error),
}

async fn wait_for_debounced_change(
    container_changes: &mut Subscription,
    certificate_changes: &mut Subscription,
    shutdown: &CancellationToken,
) -> Result<DebouncedChange, CorrosionError> {
    tokio::select! {
        changed = container_changes.changed() => changed?,
        changed = certificate_changes.changed() => changed?,
        () = tokio::time::sleep(WATCH_RESNAPSHOT) => {
            return Ok(if container_changes.snapshot_in_progress()
                || certificate_changes.snapshot_in_progress()
            {
                DebouncedChange::Resubscribe
            } else {
                DebouncedChange::Changed
            });
        }
        () = shutdown.cancelled() => return Ok(DebouncedChange::Shutdown),
    }
    let quiet = tokio::time::sleep(WATCH_DEBOUNCE);
    tokio::pin!(quiet);
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => return Ok(DebouncedChange::Shutdown),
            changed = container_changes.changed() => changed?,
            changed = certificate_changes.changed() => changed?,
            () = &mut quiet => {
                if !container_changes.snapshot_in_progress()
                    && !certificate_changes.snapshot_in_progress()
                {
                    return Ok(DebouncedChange::Changed);
                }
                tokio::select! {
                    () = shutdown.cancelled() => return Ok(DebouncedChange::Shutdown),
                    changed = container_changes.changed() => changed?,
                    changed = certificate_changes.changed() => changed?,
                    () = tokio::time::sleep(WATCH_RESNAPSHOT) => {
                        return Ok(DebouncedChange::Resubscribe);
                    }
                }
            }
        }
        quiet
            .as_mut()
            .reset(tokio::time::Instant::now() + WATCH_DEBOUNCE);
    }
}

async fn wait_to_retry(error: &CorrosionError, shutdown: &CancellationToken) -> bool {
    tracing::warn!(error = %error, "ingress watcher failed, retrying");
    wait_before_retry(shutdown).await
}

async fn wait_before_retry(shutdown: &CancellationToken) -> bool {
    tokio::select! {
        () = tokio::time::sleep(WATCH_RETRY) => true,
        () = shutdown.cancelled() => false,
    }
}

pub(crate) fn write_certificate_files(
    config_file: &Path,
    sites: &[IngressSite],
    key_mode: u32,
    prepare: fn(&Path) -> io::Result<()>,
    set_group: fn(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let directory = certificate_directory(config_file)?;
    prepare(&directory)?;
    for site in sites {
        let Some(certificate) = &site.certificate else {
            continue;
        };
        let Some(material) = &certificate.material else {
            continue;
        };
        let stem = certificate_file_stem(&site.hostname, material);
        let cert_path = directory.join(format!("{stem}.crt"));
        let key_path = directory.join(format!("{stem}.key"));
        atomic_write(&cert_path, material.certificate().as_bytes(), 0o644)?;
        set_group(&cert_path)?;
        atomic_write(&key_path, material.private_key().as_bytes(), key_mode)?;
        set_group(&key_path)?;
    }
    Ok(())
}

pub(crate) fn remove_stale_certificate_files(
    config_file: &Path,
    sites: &[IngressSite],
) -> io::Result<()> {
    let directory = certificate_directory(config_file)?;
    if !directory.exists() {
        return Ok(());
    }
    let keep: BTreeSet<String> = sites
        .iter()
        .filter_map(|site| {
            site.certificate
                .as_ref()
                .and_then(|certificate| certificate.material.as_ref())
                .map(|material| {
                    let stem = certificate_file_stem(&site.hostname, material);
                    [format!("{stem}.crt"), format!("{stem}.key")]
                })
        })
        .flatten()
        .collect();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !keep.contains(name) {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn certificate_directory(config_file: &Path) -> io::Result<std::path::PathBuf> {
    Ok(config_file
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "config file has no parent"))?
        .join(CERTS_DIR))
}

/// Stable material-derived filename stem shared by certificate writing and renderers.
#[must_use]
pub(crate) fn certificate_file_stem(
    hostname: &IngressHost,
    material: &CertificateMaterial,
) -> String {
    use hex::encode as hex_encode;
    use sha2::{Digest, Sha256};

    let mut digest = Sha256::new();
    digest.update((material.certificate().len() as u64).to_le_bytes());
    digest.update(material.certificate().as_bytes());
    digest.update((material.private_key().len() as u64).to_le_bytes());
    digest.update(material.private_key().as_bytes());
    format!("{hostname}-{}", hex_encode(digest.finalize()))
}

fn prepare_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o750))?;
    set_ployz_group(path)
}

#[cfg(test)]
#[path = "ingress_tests.rs"]
pub(crate) mod tests;
