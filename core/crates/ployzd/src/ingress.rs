//! Observer-local ingress derivation, watching, and shared filesystem state.

use ployz_core::{
    CertificateHost, ContainerAddress, ContainerId, ContainerObservation, DeploymentLogId,
    HttpProtocol, IngressHost, Machine, MachineId, PortPublication, QualifiedService,
    ServiceContainer, hostname_owners, service_containers, serving_containers,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    num::{NonZeroU16, NonZeroUsize},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

use crate::{
    corrosion::{
        CertificateChallenge, CertificateMaterial, CertificateRow, Error as CorrosionError,
        IngressRouteAck, IngressRouteChange, ReplicatedStore, ServingPointers, Subscription,
        published_cover,
    },
    docker::LABEL_DEPLOYMENT,
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

use caddy::CaddyAdmin;

/// Run the Caddy watcher.
pub(crate) async fn run(
    machine: Machine,
    replicated: ReplicatedStore,
    data_dir: PathBuf,
    runtime_dir: PathBuf,
    shutdown: CancellationToken,
) -> io::Result<()> {
    caddy::run(
        machine,
        replicated,
        caddy::config_path(&data_dir),
        runtime_dir.join("caddy/admin.sock"),
        shutdown,
    )
    .await
}

/// Fully derived observer-local input to ingress rendering and application.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct IngressProjection {
    /// Local Machine whose ingress process consumes this projection.
    pub(super) machine: Machine,
    /// Hostname-grouped ingress routes in stable hostname order.
    pub(super) sites: Vec<IngressSite>,
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
    /// What the public trace page reports about the owner.
    pub(super) trace: PublicationTrace,
}

/// The owner facts the trace page shows, independent of health and endpoint eligibility.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct PublicationTrace {
    /// Deployment attempt of the owner's newest Service Container, when it carries a valid one.
    pub(super) deploy: Option<DeploymentLogId>,
    /// Count of the owner's observed Service Containers per Machine, in any runtime state.
    pub(super) placement: BTreeMap<MachineId, NonZeroUsize>,
}

impl PublicationTrace {
    /// Trace facts for every Service, from one pass over its observed Service Containers.
    fn by_service<'a>(
        containers: impl IntoIterator<Item = &'a ServiceContainer>,
    ) -> BTreeMap<QualifiedService, Self> {
        let mut grouped = BTreeMap::<
            QualifiedService,
            (BTreeMap<MachineId, NonZeroUsize>, &'a ServiceContainer),
        >::new();
        for container in containers {
            let observation = container.as_observation();
            let (placement, newest) = grouped
                .entry(observation.identity())
                .or_insert_with(|| (BTreeMap::new(), container));
            placement
                .entry(observation.machine_id)
                .and_modify(|count| *count = count.saturating_add(1))
                .or_insert(NonZeroUsize::MIN);
            if creation_key(container) > creation_key(newest) {
                *newest = container;
            }
        }
        grouped
            .into_iter()
            .map(|(service, (placement, newest))| {
                // Only the newest Container names the attempt; an older sibling's label would be stale.
                let deploy = newest
                    .as_observation()
                    .labels
                    .get(LABEL_DEPLOYMENT)
                    .and_then(|label| DeploymentLogId::parse(label).ok());
                (service, Self { deploy, placement })
            })
            .collect()
    }
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
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
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
        certificates: &BTreeMap<CertificateHost, CertificateRow>,
        pointers: &ServingPointers,
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
                if owners.get(hostname) == Some(&owner) {
                    sites
                        .entry(hostname.clone())
                        .or_default()
                        .publish(owner.clone(), *http_protocol);
                }
            }
        }

        let mut serving = serving_containers(&containers, |service| pointers.get(service));
        serving.sort_by_key(|serving| container_order(&machine.id, serving.as_container()));
        for serving in &serving {
            let observation = serving.as_observation();
            let address = serving.address();
            let owner = observation.identity();
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
            // A wildcard is not a site; it only lends material to the sites it covers.
            let Ok(hostname) = IngressHost::parse(hostname.as_str()) else {
                continue;
            };
            sites.entry(hostname).or_default().certificate = Some(ProjectedCertificate {
                challenge: row.challenge().cloned(),
                material: row.material().cloned(),
                last_error: row.last_error().map(ToOwned::to_owned),
            });
        }
        for (hostname, site) in &mut sites {
            if site.https.is_none() {
                continue;
            }
            // Published wildcard material wins over ACME material, which stops renewing once covered.
            if let Some(material) = published_cover(hostname, certificates) {
                site.certificate.get_or_insert_default().material = Some(material.clone());
            }
        }
        let traces = PublicationTrace::by_service(&containers);
        let sites = sites
            .into_iter()
            .map(|(hostname, site)| {
                let trace = site
                    .owner
                    .as_ref()
                    .and_then(|owner| traces.get(owner))
                    .cloned()
                    .unwrap_or_default();
                site.into_site(hostname, trace)
            })
            .collect();
        Self {
            machine: machine.clone(),
            sites,
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

    fn into_site(self, hostname: IngressHost, trace: PublicationTrace) -> IngressSite {
        let publication = self.owner.map(|owner| IngressPublication {
            owner,
            http: self.http,
            https: self.https,
            trace,
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
) -> Option<ContainerId> {
    service_containers(observations.iter().cloned())
        .into_iter()
        .filter(|container| {
            let observation = container.as_observation();
            observation.machine_id == machine.id && is_system_ingress(observation)
        })
        .max_by(|left, right| creation_key(left).cmp(&creation_key(right)))
        .map(|container| container.as_observation().container_id)
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
    let mut last_input = None;
    let mut acked = BTreeMap::new();
    let mut warned_pointers = BTreeSet::new();
    'watch: loop {
        let subscriptions = async {
            tokio::try_join!(
                replicated.subscribe_container_changes(),
                replicated.subscribe_certificate_changes(),
                replicated.subscribe_serving_pointer_changes(),
            )
        };
        let changes = tokio::select! {
            changes = subscriptions => changes,
            () = shutdown.cancelled() => return Ok(()),
        };
        let (mut container_changes, mut certificate_changes, mut pointer_changes) = match changes {
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
                replicated.serving_pointers().await,
            ) {
                (Ok(containers), Ok(certificates), Ok(pointers)) => {
                    pointers.warn_newly_malformed(&mut warned_pointers);
                    let acks = route_acks(&containers.observations, &pointers);
                    Ok((
                        (
                            IngressProjection::derive(
                                &machine,
                                &containers.observations,
                                &certificates,
                                &pointers,
                            ),
                            newest_local_ingress(&machine, &containers.observations),
                            pointers,
                        ),
                        acks,
                    ))
                }
                (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
                    Err(io::Error::other(error))
                }
            };
            match input {
                Ok((input, acks)) => {
                    if last_input.as_ref() != Some(&input) {
                        let result = async {
                            let admin = connect().await.map_err(io::Error::other)?;
                            apply_caddy(&input.0, &config_file, admin.as_ref())
                                .await
                                .map_err(io::Error::other)
                        }
                        .await;
                        if let Err(error) = result {
                            eprintln!(
                                "failed to update Ingress Proxy configuration: {error}",
                                error = ployz_core::error_chain::inline(&error),
                            );
                            if wait_before_retry(&shutdown).await {
                                continue;
                            }
                            return Ok(());
                        }
                        last_input = Some(input);
                    }
                    publish_route_acks(&replicated, &machine.id, &mut acked, acks).await;
                }
                Err(error) => {
                    last_input = None;
                    eprintln!(
                        "failed to rebuild ingress projection: {error}",
                        error = ployz_core::error_chain::inline(&error),
                    );
                }
            }
            match wait_for_debounced_change(
                &mut container_changes,
                &mut certificate_changes,
                &mut pointer_changes,
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

fn route_acks(
    observations: &[ContainerObservation],
    pointers: &ServingPointers,
) -> BTreeMap<QualifiedService, IngressRouteAck> {
    let mut acks: BTreeMap<_, _> = pointers
        .iter()
        .map(|(service, pointer)| {
            let ack = IngressRouteAck {
                deploy: pointer.deploy.clone(),
                containers: BTreeSet::new(),
            };
            (service.clone(), ack)
        })
        .collect();
    let containers = service_containers(observations.iter().cloned());
    for serving in serving_containers(&containers, |service| pointers.get(service)) {
        let observation = serving.as_observation();
        if let Some(ack) = acks.get_mut(&observation.identity()) {
            ack.containers.insert(observation.container_id);
        }
    }
    acks
}

async fn publish_route_acks(
    replicated: &ReplicatedStore,
    machine_id: &MachineId,
    acked: &mut BTreeMap<QualifiedService, Option<IngressRouteAck>>,
    acks: BTreeMap<QualifiedService, IngressRouteAck>,
) {
    let mut changes: BTreeMap<_, _> = acked
        .keys()
        .filter(|service| !acks.contains_key(*service))
        .map(|service| (service.clone(), IngressRouteChange::Delete))
        .collect();
    for (service, ack) in &acks {
        if acked.get(service).and_then(Option::as_ref) != Some(ack) {
            changes.insert(service.clone(), IngressRouteChange::Upsert(ack.clone()));
        }
    }
    match replicated
        .publish_ingress_routes(machine_id, &changes)
        .await
    {
        Ok(()) => {
            *acked = acks
                .into_iter()
                .map(|(service, ack)| (service, Some(ack)))
                .collect()
        }
        Err(error) => {
            // The write may have committed anyway, so the next pass rewrites or deletes these rows.
            for service in changes.into_keys() {
                acked.insert(service, None);
            }
            tracing::warn!(
                error = %ployz_core::error_chain::inline(&error),
                "failed to publish ingress route acks, retrying next pass"
            );
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

async fn wait_for_debounced_change(
    container_changes: &mut Subscription,
    certificate_changes: &mut Subscription,
    pointer_changes: &mut Subscription,
    shutdown: &CancellationToken,
) -> Result<DebouncedChange, CorrosionError> {
    tokio::select! {
        changed = container_changes.changed() => changed?,
        changed = certificate_changes.changed() => changed?,
        changed = pointer_changes.changed() => changed?,
        () = tokio::time::sleep(WATCH_RESNAPSHOT) => {
            return Ok(if container_changes.snapshot_in_progress()
                || certificate_changes.snapshot_in_progress()
                || pointer_changes.snapshot_in_progress()
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
            changed = pointer_changes.changed() => changed?,
            () = &mut quiet => {
                if !container_changes.snapshot_in_progress()
                    && !certificate_changes.snapshot_in_progress()
                    && !pointer_changes.snapshot_in_progress()
                {
                    return Ok(DebouncedChange::Changed);
                }
                tokio::select! {
                    () = shutdown.cancelled() => return Ok(DebouncedChange::Shutdown),
                    changed = container_changes.changed() => changed?,
                    changed = certificate_changes.changed() => changed?,
                    changed = pointer_changes.changed() => changed?,
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
    tracing::warn!(error = %ployz_core::error_chain::inline(error), "ingress watcher failed, retrying");
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
