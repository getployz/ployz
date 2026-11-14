//! Observer-local ingress derivation, watching, and shared filesystem state.

use ployz_core::{
    CertificateHost, ContainerAddress, ContainerId, HttpProtocol, Machine, MachineId, ProxyRoutes,
    QualifiedService, ServiceContainer, proxy_routes, reachable_routes, service_containers,
};
use ployz_decide::caddy_load::{self as load, Applied, CaddyFile, Input, Probe};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{
    corrosion::{
        AdminClient, CertificateRow, Error as CorrosionError, IngressRouteAck, IngressRouteChange,
        MachineView, ReplicatedStore, Subscription,
    },
    filesystem::set_ployz_group,
    route_acks::{DownMachines, load_down},
};

/// The run dir's subdirectory the Ingress Proxy's runtime mount shows.
const RUNTIME_DIR: &str = "ingress";
/// Where the Ingress Proxy's runtime mount holds the access log, relative to that mount.
const ACCESS_LOG: &str = "requests/access.log";

/// The access log the Ingress Proxy writes, as the daemon sees it under its `run_dir`.
pub(crate) fn access_log_path(run_dir: &Path) -> PathBuf {
    run_dir.join(RUNTIME_DIR).join(ACCESS_LOG)
}
const WATCH_DEBOUNCE: Duration = Duration::from_millis(50);
const WATCH_RETRY: Duration = Duration::from_secs(1);
const WATCH_RESNAPSHOT: Duration = Duration::from_secs(5);

pub(crate) mod caddy;
mod certificates;
mod error_page;
mod projection;

use caddy::CaddyAdmin;
use certificates::{
    certificate_file_stem, remove_stale_certificate_files, write_certificate_files,
};
use projection::{IngressProjection, IngressPublication, IngressSite, prepare_directory};

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
    caddy::prepare_origin_directory(&runtime_dir.join("tunnel"))?;
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
        .filter(|observation| observation.machine_id == *machine && observation.is_system_ingress())
        .peekable();
    if local.peek().is_none() {
        return !takes_ingress;
    }
    local.all(|observation| observation.runtime.at_rest())
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
            observation.machine_id == machine.id && observation.is_system_ingress()
        })
        .max_by_key(|container| container.as_observation().recency_key())
}

/// What one pass applies: the projection, and the newest local ingress container.
#[derive(Eq, PartialEq)]
struct CaddyInput {
    projection: IngressProjection,
    caddy: Option<ContainerId>,
}

impl Input for CaddyInput {
    type Caddy = ContainerId;

    /// Whether `other` serves what this does. The trace page's values are left out: a
    /// Container's start or removal changes them while every route stays, and a reload
    /// for them alone would clear Caddy's passive health counts, so the page keeps its
    /// values until a route changes (#2522).
    fn serves_as(&self, other: &Self) -> bool {
        let Self { projection, caddy } = self;
        let IngressProjection { machine, sites } = projection;
        *caddy == other.caddy
            && *machine == other.projection.machine
            && sites.len() == other.projection.sites.len()
            && sites
                .iter()
                .zip(&other.projection.sites)
                .all(|(site, other)| site.serves_as(other))
    }

    fn caddy(&self) -> Option<&ContainerId> {
        self.caddy.as_ref()
    }
}

impl IngressSite {
    fn serves_as(&self, other: &Self) -> bool {
        let Self {
            hostname,
            publication,
            certificate,
        } = self;
        *hostname == other.hostname
            && *certificate == other.certificate
            && match (publication, &other.publication) {
                (Some(publication), Some(other)) => publication.serves_as(other),
                (None, None) => true,
                (Some(_), None) | (None, Some(_)) => false,
            }
    }
}

impl IngressPublication {
    fn serves_as(&self, other: &Self) -> bool {
        let Self {
            owner,
            http,
            https,
            trace: _,
        } = self;
        *owner == other.owner && *http == other.http && *https == other.https
    }
}

type RoutedAddresses = BTreeMap<QualifiedService, BTreeMap<ContainerId, ContainerAddress>>;

type CaddyLoad = load::CaddyLoad<CaddyInput, RoutedAddresses>;
type PassOutcome = load::PassOutcome<CaddyInput, RoutedAddresses>;

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

/// The file stem of the certificate the Caddyfile pins for `site`.
fn pinned_stem(site: &IngressSite) -> Option<String> {
    site.pinned()
        .map(|material| certificate_file_stem(&site.hostname, material))
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
    let mut load = CaddyLoad::Unapplied;
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
                    let input = CaddyInput { projection, caddy };
                    // A stopped Caddy no longer serves what it loaded.
                    let (pass, result) = if !no_caddy && load.skip(&input, true) {
                        (PassOutcome::Cached { addresses }, Ok(()))
                    } else {
                        match connect().await {
                            Ok(admin) if load.skip(&input, admin.is_some()) => {
                                let pass = if admin.is_some() {
                                    PassOutcome::Cached { addresses }
                                } else {
                                    PassOutcome::Written { input }
                                };
                                (pass, Ok(()))
                            }
                            // Caddy starts from a file written without its admin, so that file
                            // pins only certificates this Caddy has loaded.
                            Ok(None) => {
                                let validated = certificates.loaded_only(&input.projection);
                                let applied =
                                    apply_caddy::<A>(&validated, &config_file, None).await;
                                let pass = if applied.is_ok() {
                                    PassOutcome::Written { input }
                                } else {
                                    PassOutcome::Unknown
                                };
                                (pass, applied)
                            }
                            // A failure after the load leaves its certificates loaded.
                            Ok(Some(admin)) => {
                                let result =
                                    apply_caddy(&input.projection, &config_file, Some(&admin))
                                        .await;
                                let applied = match &result {
                                    Ok(()) => {
                                        certificates.accept(&input.projection);
                                        Applied::Ok
                                    }
                                    Err(ApplyError::AfterServing(_, file)) => {
                                        certificates.accept(&input.projection);
                                        Applied::AfterServing(*file)
                                    }
                                    Err(ApplyError::BeforeServing(caddy::Error::Admin(_))) => {
                                        Applied::BeforeServing(
                                            certificates
                                                .find_refused(&input.projection, &admin)
                                                .await,
                                        )
                                    }
                                    Err(ApplyError::BeforeServing(_)) => {
                                        Applied::BeforeServing(Probe::Unchanged)
                                    }
                                };
                                let pass = PassOutcome::Loaded {
                                    input,
                                    addresses,
                                    applied,
                                };
                                (pass, result)
                            }
                            Err(error) => {
                                (PassOutcome::Unknown, Err(ApplyError::BeforeServing(error)))
                            }
                        }
                    };
                    let through_admin = pass.through_admin();
                    let refused = pass.found_refusal();
                    load = load.after(pass);
                    let addresses = load.claim(through_admin, no_caddy);
                    route_publishes.send_if_modified(|publishes| {
                        let changed = publishes.addresses != addresses;
                        publishes.addresses = addresses;
                        changed
                    });
                    // Derive again without the refused.
                    if refused {
                        continue;
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

/// A failed Caddy apply, split by whether Caddy already serves the new config, and after
/// it, whether the Caddyfile a restart reads holds it.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ApplyError {
    #[error(transparent)]
    BeforeServing(caddy::Error),
    #[error("{0}")]
    AfterServing(#[source] caddy::Error, CaddyFile),
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
    caddy::write_caddyfile(config_file, &caddyfile).map_err(|error| {
        if admin.is_some() {
            ApplyError::AfterServing(error, CaddyFile::Stale)
        } else {
            ApplyError::BeforeServing(error)
        }
    })?;
    remove_stale_certificate_files(config_file, &projection.sites)
        .map_err(|error| ApplyError::AfterServing(error.into(), CaddyFile::Current))?;
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

#[cfg(test)]
#[path = "ingress_tests.rs"]
pub(crate) mod tests;
