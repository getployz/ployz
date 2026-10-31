//! Observer-local ingress derivation, watching, and shared filesystem state.

use ployz_core::{
    CertificateHost, ContainerAddress, ContainerId, ContainerObservation,
    ContainerRuntimeObservation, DeploymentLogId, HttpProtocol, IngressHost, Machine, MachineId,
    PortPublication, ProxyRoutes, QualifiedService, ServiceContainer, hostname_owners,
    proxy_routes, reachable_routes, service_containers,
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
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{
    corrosion::{
        AdminClient, CertificateChallenge, CertificateMaterial, CertificateRow,
        Error as CorrosionError, IngressRouteAck, IngressRouteChange, MachineView, ReplicatedStore,
        Subscription, published_cover,
    },
    docker::LABEL_DEPLOYMENT,
    filesystem::{atomic_write, set_ployz_group},
    route_acks::{DownMachines, load_down},
};

const CERTS_DIR: &str = "certs";
pub(crate) const ACCESS_LOG: &str = "requests/access.log";
const WATCH_DEBOUNCE: Duration = Duration::from_millis(50);
const WATCH_RETRY: Duration = Duration::from_secs(1);
const WATCH_RESNAPSHOT: Duration = Duration::from_secs(5);

/// True when this observation is the reserved Ingress Proxy Service.
#[must_use]
pub(crate) fn is_system_ingress(observation: &ContainerObservation) -> bool {
    observation.identity() == QualifiedService::system_ingress()
}

pub(crate) mod caddy;

use caddy::CaddyAdmin;

/// What this process's ingress has published since it started: how many writes
/// succeeded, and the Containers the last one acked per Service. A row already in
/// Corrosion may predate a power-off, so only these count as this Machine's acks.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PublishedRoutes {
    pub(crate) count: u64,
    pub(crate) routes: BTreeMap<QualifiedService, BTreeSet<ContainerId>>,
    /// `None` while this process can't confirm what Caddy serves.
    pub(crate) addresses:
        Option<BTreeMap<QualifiedService, BTreeMap<ContainerId, ContainerAddress>>>,
}

/// Run the Caddy watcher.
#[expect(
    clippy::too_many_arguments,
    reason = "each is a distinct daemon handle the watcher owns"
)]
pub(crate) async fn run(
    machine: Machine,
    replicated: ReplicatedStore,
    machines: MachineView,
    admin: AdminClient,
    data_dir: PathBuf,
    runtime_dir: PathBuf,
    route_publishes: watch::Sender<PublishedRoutes>,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let local = machine.id;
    caddy::run(
        machine,
        replicated,
        move || {
            let machines = machines.clone();
            let admin = admin.clone();
            async move { load_down(&machines, &admin, &local).await }
        },
        caddy::config_path(&data_dir),
        runtime_dir.join("caddy/admin.sock"),
        route_publishes,
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
    pub(super) address: ContainerAddress,
    /// Published container port.
    pub(super) port: NonZeroU16,
}

/// Certificate state that can affect ingress behavior or generated output.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct ProjectedCertificate {
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

    /// The HTTPS route with the certificate Caddy serves it under. Without material,
    /// Caddy renders no HTTPS site, so the route proxies nothing.
    #[must_use]
    pub(super) fn https(&self) -> Option<(&CertificateMaterial, &[IngressEndpoint])> {
        self.material().zip(self.route(HttpProtocol::Https))
    }

    /// Ordered endpoints Caddy proxies to for one HTTP protocol.
    #[must_use]
    pub(super) fn proxied(&self, protocol: HttpProtocol) -> Option<&[IngressEndpoint]> {
        match protocol {
            HttpProtocol::Http => self.route(HttpProtocol::Http),
            HttpProtocol::Https => self.https().map(|(_, endpoints)| endpoints),
        }
    }

    #[must_use]
    pub(super) fn challenge(&self) -> Option<&CertificateChallenge> {
        self.certificate
            .as_ref()
            .and_then(|certificate| certificate.challenge.as_ref())
    }

    #[must_use]
    pub(super) fn material(&self) -> Option<&CertificateMaterial> {
        self.certificate
            .as_ref()
            .and_then(|certificate| certificate.material.as_ref())
    }
}

impl IngressProjection {
    #[must_use]
    pub(crate) fn derive(
        machine: &Machine,
        containers: &[ServiceContainer],
        certificates: &BTreeMap<CertificateHost, CertificateRow>,
        routes: &ProxyRoutes,
    ) -> Self {
        let owners = hostname_owners(containers.iter().map(ServiceContainer::as_observation));
        let mut sites = BTreeMap::<IngressHost, SiteBuilder>::new();
        for container in containers {
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

        let mut serving: Vec<_> = routes.values().flatten().collect();
        serving.sort_by_key(|serving| container_order(&machine.id, serving.as_container()));
        for serving in serving {
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
        let traces = PublicationTrace::by_service(containers);
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

/// True when no Caddy can serve on this Machine: every ingress container here is stopped,
/// or it has none and takes no ingress.
fn serves_nothing(
    machine: &MachineId,
    takes_ingress: bool,
    containers: &[ServiceContainer],
) -> bool {
    let mut local = containers
        .iter()
        .map(ServiceContainer::as_observation)
        .filter(|observation| observation.machine_id == *machine && is_system_ingress(observation))
        .peekable();
    if local.peek().is_none() {
        return !takes_ingress;
    }
    local.all(|observation| at_rest(&observation.runtime))
}

/// True when a container's process isn't running and can serve nothing.
pub(crate) fn at_rest(runtime: &ContainerRuntimeObservation) -> bool {
    matches!(
        runtime,
        ContainerRuntimeObservation::Created
            | ContainerRuntimeObservation::Exited { .. }
            | ContainerRuntimeObservation::Dead
    )
}

fn newest_local_ingress(machine: &Machine, containers: &[ServiceContainer]) -> Option<ContainerId> {
    local_ingress(machine, containers).map(|container| container.as_observation().container_id)
}

fn local_ingress<'container>(
    machine: &Machine,
    containers: &'container [ServiceContainer],
) -> Option<&'container ServiceContainer> {
    containers
        .iter()
        .filter(|container| {
            let observation = container.as_observation();
            observation.machine_id == machine.id && is_system_ingress(observation)
        })
        .max_by(|left, right| creation_key(left).cmp(&creation_key(right)))
}

/// What the Caddyfile was last written from. A write while Caddy's admin socket is down
/// isn't `through_admin`, since a starting Caddy may have read the Caddyfile before it.
#[derive(Eq, PartialEq)]
struct CaddyInput {
    projection: IngressProjection,
    caddy: Option<ContainerId>,
    through_admin: bool,
}

impl CaddyInput {
    fn loaded(&self, projection: &IngressProjection, caddy: Option<&ContainerId>) -> bool {
        self.through_admin && self.projection == *projection && self.caddy.as_ref() == caddy
    }
}

/// Certificates by file stem: those Caddy loaded last, and those it refused with its reason.
/// A stem names one site's material, so new material for a refused site is tried again.
#[derive(Default)]
struct CaddyCertificates {
    accepted: BTreeSet<String>,
    refused: BTreeMap<String, String>,
}

impl CaddyCertificates {
    /// `projection` without the certificates Caddy has not loaded.
    fn loaded_only(&self, projection: &IngressProjection) -> IngressProjection {
        let unloaded = projection
            .sites
            .iter()
            .filter_map(pinned_stem)
            .filter(|stem| !self.accepted.contains(stem))
            .collect();
        without(projection, &unloaded)
    }

    /// Leave each refused site out, as if its certificate were still pending, and name the
    /// refusal in its place, so one site's certificate never blocks the rest of the config.
    fn quarantine(&mut self, projection: &mut IngressProjection) {
        let mut current = BTreeSet::new();
        for site in &mut projection.sites {
            let Some(stem) = pinned_stem(site) else {
                continue;
            };
            if let Some(error) = self.refused.get(&stem)
                && let Some(certificate) = &mut site.certificate
            {
                certificate.material = None;
                certificate.last_error = Some(format!("Caddy refused it: {error}"));
            }
            current.insert(stem);
        }
        self.refused.retain(|stem, _| current.contains(stem));
    }

    fn accept(&mut self, projection: &IngressProjection) {
        self.accepted = projection.sites.iter().filter_map(pinned_stem).collect();
    }

    /// After Caddy refuses `projection`, load it without the certificates Caddy has not loaded
    /// before, then add those back one at a time.
    async fn find_refused<A: CaddyAdmin>(
        &mut self,
        projection: &IngressProjection,
        admin: &A,
    ) -> Probe {
        // What Caddy runs now is what it accepted, so only other material is suspect.
        if let Ok(loaded) = admin.loaded_certificates().await {
            self.accepted = loaded;
        }
        let suspects: Vec<_> = projection
            .sites
            .iter()
            .filter_map(pinned_stem)
            .filter(|stem| !self.accepted.contains(stem))
            .collect();
        let mut excluded: BTreeSet<_> = suspects.iter().cloned().collect();
        if excluded.is_empty()
            || caddy::load(&without(projection, &excluded), Some(admin))
                .await
                .is_err()
        {
            return Probe::Unchanged;
        }
        let mut refused = false;
        for stem in suspects {
            excluded.remove(&stem);
            let candidate = without(projection, &excluded);
            let mut loaded = caddy::load(&candidate, Some(admin)).await;
            if matches!(loaded, Err(caddy::Error::Admin(_))) {
                // A refusal is quarantined until the material changes, so a one-off must repeat.
                loaded = caddy::load(&candidate, Some(admin)).await;
            }
            match loaded {
                Ok(_) => {}
                Err(caddy::Error::Admin(error)) => {
                    tracing::warn!(
                        certificate = %stem,
                        %error,
                        "Caddy refused a certificate; its site is left out until the material changes"
                    );
                    excluded.insert(stem.clone());
                    self.refused.insert(stem, error);
                    refused = true;
                }
                Err(_) => {
                    excluded.insert(stem);
                    break;
                }
            }
        }
        self.accept(&without(projection, &excluded));
        if refused {
            Probe::Refused
        } else {
            Probe::Reloaded
        }
    }
}

/// What a probe left Caddy serving.
enum Probe {
    /// Caddy still serves the config it had.
    Unchanged,
    /// Caddy serves a probe config, though it refused no certificate.
    Reloaded,
    /// Caddy refused at least one certificate, now quarantined.
    Refused,
}

/// The file stem of the certificate the Caddyfile pins for `site`.
fn pinned_stem(site: &IngressSite) -> Option<String> {
    site.https()
        .map(|(material, _)| certificate_file_stem(&site.hostname, material))
}

/// `projection` with the pinned certificates named in `excluded` left out.
fn without(projection: &IngressProjection, excluded: &BTreeSet<String>) -> IngressProjection {
    let mut projection = projection.clone();
    for site in &mut projection.sites {
        if pinned_stem(site).is_some_and(|stem| excluded.contains(&stem))
            && let Some(certificate) = &mut site.certificate
        {
            certificate.material = None;
        }
    }
    projection
}

/// Watch replicated ingress inputs and concretely apply changed projections to Caddy.
///
/// # Errors
///
/// Returns when connecting to Caddy administration fails. Replicated-store
/// subscription failures are retried until shutdown.
pub(crate) async fn watch_caddy<A: CaddyAdmin, Connect, ConnectFuture, LoadDown, DownFuture>(
    machine: Machine,
    replicated: ReplicatedStore,
    mut load_down: LoadDown,
    config_file: PathBuf,
    route_publishes: watch::Sender<PublishedRoutes>,
    shutdown: CancellationToken,
    mut connect: Connect,
) -> io::Result<()>
where
    Connect: FnMut() -> ConnectFuture,
    ConnectFuture: Future<Output = Result<Option<A>, caddy::Error>>,
    LoadDown: FnMut() -> DownFuture,
    DownFuture: Future<Output = Result<BTreeSet<MachineId>, CorrosionError>>,
{
    // Every Caddy reload clears passive health counts and re-arms a dead upstream, so an
    // unchanged input is never reloaded.
    let mut last_input = None;
    // The Caddy whose admin confirmed the published addresses.
    let mut recorded_by = None;
    let mut acked = BTreeMap::new();
    // Kept while every copy is out of reach.
    let mut routed = ProxyRoutes::new();
    let mut down = DownMachines::new(machine.id);
    let mut certificates = CaddyCertificates::default();
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
            down.update(load_down().await);
            // Read each pass, since turning the role off doesn't restart the daemon.
            let takes_ingress = !matches!(
                replicated.machine(machine.id.as_str()).await,
                Ok(Some(Machine {
                    accepts_ingress: false,
                    ..
                }))
            );
            let input = match (
                replicated.containers().await,
                replicated.certificate_state().await,
            ) {
                (Ok(containers), Ok(rows)) => {
                    let containers = service_containers(containers.observations);
                    let (routes, mut projection) = next_pass(
                        &machine,
                        &containers,
                        &rows,
                        |machine| down.contains(machine),
                        &routed,
                    );
                    certificates.quarantine(&mut projection);
                    Ok((
                        newest_local_ingress(&machine, &containers),
                        serves_nothing(&machine.id, takes_ingress, &containers),
                        routed_addresses(&projection, &routes),
                        projection,
                        routes,
                    ))
                }
                (Err(error), _) | (_, Err(error)) => Err(io::Error::other(error)),
            };
            match input {
                Ok((caddy, no_caddy, addresses, projection, routes)) => {
                    // A stopped Caddy no longer serves what it loaded.
                    let loaded = !no_caddy
                        && last_input.as_ref().is_some_and(|last: &CaddyInput| {
                            last.loaded(&projection, caddy.as_ref())
                        });
                    let (through_admin, result) = if loaded {
                        (true, Ok(()))
                    } else {
                        match connect().await {
                            Ok(admin) => {
                                let input = CaddyInput {
                                    projection,
                                    caddy,
                                    through_admin: admin.is_some(),
                                };
                                let result = if last_input.as_ref() == Some(&input) {
                                    Ok(())
                                } else {
                                    // Caddy starts from a file written without its admin, so
                                    // that file pins only certificates this Caddy has loaded.
                                    let validated;
                                    let applying = if admin.is_some() {
                                        &input.projection
                                    } else {
                                        validated = certificates.loaded_only(&input.projection);
                                        &validated
                                    };
                                    let applied =
                                        apply_caddy(applying, &config_file, admin.as_ref()).await;
                                    let probe = match (&applied, admin.as_ref()) {
                                        (
                                            Err(ApplyError::BeforeServing(caddy::Error::Admin(_))),
                                            Some(admin),
                                        ) => {
                                            certificates
                                                .find_refused(&input.projection, admin)
                                                .await
                                        }
                                        _ => Probe::Unchanged,
                                    };
                                    match probe {
                                        Probe::Unchanged => {}
                                        // Caddy no longer serves what was last loaded or recorded.
                                        Probe::Reloaded => {
                                            last_input = None;
                                            recorded_by = None;
                                        }
                                        // Derive again without the refused.
                                        Probe::Refused => {
                                            last_input = None;
                                            recorded_by = None;
                                            continue;
                                        }
                                    }
                                    // A refused load leaves the earlier one serving. A failure
                                    // after the load leaves its certificates loaded. Any other
                                    // failure may have replaced the file Caddy restarts from.
                                    match (&applied, admin.is_some()) {
                                        (Ok(()), true) => {
                                            certificates.accept(&input.projection);
                                            last_input = Some(input);
                                        }
                                        (Ok(()), false) => last_input = Some(input),
                                        (Err(ApplyError::BeforeServing(_)), true) => {}
                                        (Err(ApplyError::AfterServing(_)), true) => {
                                            certificates.accept(&input.projection);
                                            last_input = None;
                                        }
                                        (Err(_), false) => last_input = None,
                                    }
                                    applied
                                };
                                (admin.is_some(), result)
                            }
                            Err(error) => (false, Err(ApplyError::BeforeServing(error))),
                        }
                    };
                    // A refused load leaves the record standing only while the newest observed
                    // ingress container is the one whose admin confirmed it. A new Caddy serves
                    // its Caddyfile. An in-place restart under the same id goes unseen (#2787).
                    let refused =
                        through_admin && matches!(result, Err(ApplyError::BeforeServing(_)));
                    if !(refused && recorded_by.is_some() && recorded_by == caddy) {
                        // Without the admin, a running Caddy serves what it last loaded or, after
                        // a restart, the Caddyfile, so the record claims nothing.
                        let addresses = if through_admin {
                            (!refused).then_some(addresses)
                        } else {
                            no_caddy.then(BTreeMap::new)
                        };
                        recorded_by = caddy.filter(|_| through_admin && addresses.is_some());
                        route_publishes.send_if_modified(|publishes| {
                            let changed = publishes.addresses != addresses;
                            publishes.addresses = addresses;
                            changed
                        });
                    }
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
                    let acks = route_acks(&routes);
                    let published = acks
                        .iter()
                        .map(|(service, ack)| (service.clone(), ack.containers.clone()))
                        .collect();
                    if publish_route_acks(&replicated, &machine.id, &mut acked, acks).await {
                        route_publishes.send_modify(|publishes| {
                            publishes.count += 1;
                            publishes.routes = published;
                        });
                    }
                    routed = routes;
                }
                Err(error) => {
                    eprintln!(
                        "failed to rebuild ingress projection: {error}",
                        error = ployz_core::error_chain::inline(&error),
                    );
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

/// One watch pass: the route this proxy carries and acks, and what Caddy loads.
///
/// Caddy dials only the copies a client can reach now. The route keeps every copy
/// [`proxy_routes`] keeps, so the acks and the next pass match a proxy that
/// dialed them all.
fn next_pass(
    machine: &Machine,
    containers: &[ServiceContainer],
    certificates: &BTreeMap<CertificateHost, CertificateRow>,
    is_down: impl Fn(&MachineId) -> bool,
    routed: &ProxyRoutes,
) -> (ProxyRoutes, IngressProjection) {
    let routes = proxy_routes(containers, &is_down, routed);
    let reachable = reachable_routes(
        &routes,
        containers.iter().map(ServiceContainer::as_observation),
        &is_down,
    );
    let projection = IngressProjection::derive(machine, containers, certificates, &reachable);
    (routes, projection)
}

fn route_acks(routes: &ProxyRoutes) -> BTreeMap<QualifiedService, IngressRouteAck> {
    routes
        .iter()
        .map(|(service, route)| {
            let ack = IngressRouteAck {
                containers: route
                    .iter()
                    .map(|serving| serving.as_observation().container_id)
                    .collect(),
            };
            (service.clone(), ack)
        })
        .collect()
}

fn routed_addresses(
    projection: &IngressProjection,
    routes: &ProxyRoutes,
) -> BTreeMap<QualifiedService, BTreeMap<ContainerId, ContainerAddress>> {
    let proxied: BTreeSet<_> = projection
        .sites
        .iter()
        .filter_map(|site| {
            let owner = &site.publication.as_ref()?.owner;
            let endpoints = [HttpProtocol::Http, HttpProtocol::Https]
                .into_iter()
                .filter_map(|protocol| site.proxied(protocol))
                .flatten();
            Some(endpoints.map(move |endpoint| (owner, endpoint.address)))
        })
        .flatten()
        .collect();
    routes
        .iter()
        .filter_map(|(service, route)| {
            let addresses: BTreeMap<_, _> = route
                .iter()
                .filter(|serving| proxied.contains(&(service, serving.address())))
                .map(|serving| (serving.as_observation().container_id, serving.address()))
                .collect();
            (!addresses.is_empty()).then(|| (service.clone(), addresses))
        })
        .collect()
}

async fn publish_route_acks(
    replicated: &ReplicatedStore,
    machine_id: &MachineId,
    acked: &mut BTreeMap<QualifiedService, Option<IngressRouteAck>>,
    acks: BTreeMap<QualifiedService, IngressRouteAck>,
) -> bool {
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
                .collect();
            true
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
            false
        }
    }
}

/// A failed Caddy apply, split by whether Caddy already serves the new config.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ApplyError {
    #[error(transparent)]
    BeforeServing(caddy::Error),
    #[error(transparent)]
    AfterServing(caddy::Error),
}

/// Write shared certificate state and concretely reconcile Caddy. Without a running
/// Caddy, the new config serves once the Caddyfile Caddy starts from is written.
///
/// # Errors
///
/// Returns when certificate filesystem operations or Caddy reconciliation fail.
pub(crate) async fn apply_caddy<A: CaddyAdmin>(
    projection: &IngressProjection,
    config_file: &Path,
    admin: Option<&A>,
) -> Result<(), ApplyError> {
    write_certificate_files(
        config_file,
        &projection.sites,
        0o600,
        prepare_directory,
        set_ployz_group,
    )
    .map_err(|error| ApplyError::BeforeServing(error.into()))?;
    let caddyfile = caddy::load(projection, admin)
        .await
        .map_err(ApplyError::BeforeServing)?;
    caddy::write_caddyfile(config_file, &caddyfile).map_err(if admin.is_some() {
        ApplyError::AfterServing
    } else {
        ApplyError::BeforeServing
    })?;
    remove_stale_certificate_files(config_file, &projection.sites)
        .map_err(|error| ApplyError::AfterServing(error.into()))?;
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
        let Some((material, _)) = site.https() else {
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
            site.https().map(|(material, _)| {
                let stem = certificate_file_stem(&site.hostname, material);
                [format!("{stem}.crt"), format!("{stem}.key")]
            })
        })
        .flatten()
        .collect();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            continue;
        }
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

/// Stable filename stem shared by certificate writing and renderers. It is a fixed-length
/// digest of the hostname and material, so a 253-character hostname stays under NAME_MAX.
#[must_use]
pub(crate) fn certificate_file_stem(
    hostname: &IngressHost,
    material: &CertificateMaterial,
) -> String {
    use hex::encode as hex_encode;
    use sha2::{Digest, Sha256};

    let mut digest = Sha256::new();
    digest.update((hostname.as_str().len() as u64).to_le_bytes());
    digest.update(hostname.as_str().as_bytes());
    digest.update((material.certificate().len() as u64).to_le_bytes());
    digest.update(material.certificate().as_bytes());
    digest.update((material.private_key().len() as u64).to_le_bytes());
    digest.update(material.private_key().as_bytes());
    hex_encode(digest.finalize())
}

fn prepare_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o750))?;
    set_ployz_group(path)
}

#[cfg(test)]
#[path = "ingress_tests.rs"]
pub(crate) mod tests;
