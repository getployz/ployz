//! Obtain and renew certificates for https Ingress Hostnames.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    io,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    pin::Pin,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use futures_util::future::join_all;
use http_body_util::BodyExt;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, BytesResponse, ChallengeType,
    ExternalAccountKey, HttpClient, Identifier, NewAccount, NewOrder, OrderStatus, RetryPolicy,
};
use ployz_core::{
    CertificateHost, CertificateKeyType, CertificatePolicy, ClusterRoute, ContainerKind,
    ContainerObservation, HOSTNAME_VERIFY_PATH, HostnameVerdict, HttpProtocol, IngressHost,
    IssuanceClock, IssuanceFailure, IssuanceGate, Machine, MachineId, PortPublication, Refusal,
    VerifyAnswer, hostname_verdict, issuance_failure_clock, issuance_gate, refusal_reason,
    resolve_certificate_policy,
};
use reqwest::{Client, redirect::Policy};
use thiserror::Error;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    corrosion::{
        AcmeWrite, CertificateChallenge, CertificateMaterial, CertificateRow, ReplicatedStore,
        published_cover,
    },
    filesystem::{atomic_write, set_ployz_group},
};

pub(crate) const DIRECTORY_ENV: &str = "PLOYZ_ACME_DIRECTORY";
const ACCOUNT_FILE: &str = "account.json";
const CHALLENGE_WAIT: Duration = Duration::from_secs(30);
const CHALLENGE_POLL: Duration = Duration::from_millis(200);
const RETRY_INTERVAL: Duration = Duration::from_secs(60);
const RESUBSCRIBE_INTERVAL: Duration = Duration::from_secs(1);
/// Must cover `CHALLENGE_WAIT` so rank 1 cannot write a competing token while rank 0 is still probing.
pub(crate) const RANK_STEP: Duration = CHALLENGE_WAIT;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IssuanceAction {
    Nothing,
    Order,
    Renew,
}

/// Rank among Machine identifiers. Lowest id is 0 and may order immediately.
#[must_use]
pub(crate) fn machine_rank<'id>(
    this: &MachineId,
    machines: impl IntoIterator<Item = &'id MachineId>,
) -> usize {
    machines.into_iter().filter(|id| *id < this).count()
}

/// Whether this Machine should order or renew now.
///
/// Rank delay uses `RANK_STEP.max(policy.probe_timeout())` so a later rank
/// cannot start a competing order while an earlier rank is still presenting.
#[must_use]
pub(crate) fn issuance_action(
    row: Option<&CertificateRow>,
    rank: usize,
    elapsed: Duration,
    now: SystemTime,
    machine_id: &MachineId,
    policy: &CertificatePolicy,
) -> IssuanceAction {
    action_due(row, rank, elapsed, now, machine_id, policy).0
}

fn rank_delay(rank: usize, step: Duration) -> Duration {
    step.saturating_mul(u32::try_from(rank).unwrap_or(u32::MAX))
}

fn issuance_step(policy: &CertificatePolicy) -> Duration {
    RANK_STEP.max(policy.probe_timeout())
}

fn action_due(
    row: Option<&CertificateRow>,
    rank: usize,
    elapsed: Duration,
    now: SystemTime,
    machine_id: &MachineId,
    policy: &CertificatePolicy,
) -> (IssuanceAction, Duration) {
    let delay = rank_delay(rank, issuance_step(policy));
    match row.and_then(CertificateRow::material) {
        None => {
            let wait = delay.saturating_sub(elapsed);
            let action = if wait.is_zero() {
                IssuanceAction::Order
            } else {
                IssuanceAction::Nothing
            };
            (action, wait)
        }
        Some(material) => match renew_at(material, machine_id, policy.renew_at_lifetime_fraction())
        {
            Some(renew_at) => {
                let wait = saturating_add(renew_at, delay)
                    .duration_since(now)
                    .unwrap_or(Duration::ZERO);
                let action = if wait.is_zero() {
                    IssuanceAction::Renew
                } else {
                    IssuanceAction::Nothing
                };
                (action, wait)
            }
            None => (IssuanceAction::Nothing, RETRY_INTERVAL),
        },
    }
}

/// Instant `fraction` of the certificate's own lifetime has elapsed.
#[must_use]
pub(crate) fn renewal_window(
    not_before: SystemTime,
    not_after: SystemTime,
    fraction: f64,
) -> Option<SystemTime> {
    let lifetime = not_after.duration_since(not_before).ok()?;
    if lifetime.is_zero() {
        return None;
    }
    Some(saturating_add(not_before, lifetime.mul_f64(fraction)))
}

fn renew_at(
    material: &CertificateMaterial,
    machine_id: &MachineId,
    fraction: f64,
) -> Option<SystemTime> {
    let (not_before, not_after) = material_validity(material.certificate())?;
    let window_start = renewal_window(not_before, not_after, fraction)?;
    let remaining = not_after.duration_since(window_start).ok()?;
    Some(saturating_add(
        window_start,
        machine_jitter(machine_id, remaining.div_f64(RENEWAL_JITTER_DIVISOR)),
    ))
}

/// Machines spread their first renewal over one tenth of the window, so each keeps the rest to retry before expiry.
const RENEWAL_JITTER_DIVISOR: f64 = 10.0;

/// Jitter in `[0, span)` from the Machine identity.
#[must_use]
pub(crate) fn machine_jitter(machine_id: &MachineId, span: Duration) -> Duration {
    if span.is_zero() {
        return Duration::ZERO;
    }
    let seed = machine_id
        .as_str()
        .as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        });
    let nanos = span.as_nanos().saturating_mul(u128::from(seed)) / (u128::from(u64::MAX) + 1);
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

fn material_validity(pem: &str) -> Option<(SystemTime, SystemTime)> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).ok()?;
    let cert = pem.parse_x509().ok()?;
    let validity = cert.validity();
    Some((
        asn1_to_system(validity.not_before)?,
        asn1_to_system(validity.not_after)?,
    ))
}

fn asn1_to_system(time: x509_parser::time::ASN1Time) -> Option<SystemTime> {
    let secs = time.timestamp();
    if secs >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_secs(u64::try_from(secs).ok()?))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs(secs.unsigned_abs()))
    }
}

fn saturating_add(time: SystemTime, duration: Duration) -> SystemTime {
    time.checked_add(duration).unwrap_or(time)
}

fn poll_wait(
    row: Option<&CertificateRow>,
    rank: usize,
    elapsed: Duration,
    now: SystemTime,
    machine_id: &MachineId,
    policy: &CertificatePolicy,
) -> Duration {
    match action_due(row, rank, elapsed, now, machine_id, policy) {
        (IssuanceAction::Order | IssuanceAction::Renew, _) => RANK_STEP,
        (IssuanceAction::Nothing, due_in) => due_in.min(RETRY_INTERVAL),
    }
}

/// This Machine's view of one wanted Ingress Hostname after a pass of its certificate loop.
/// It lives only in the loop's memory: each Machine keeps its own, from its own row, verdict
/// and rank.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostIssuance {
    /// The row holds Certificate Material whose renewal is not due before `renew_due`.
    Issued { renew_due: SystemTime },
    /// The gate let the pass through, and the rank delay runs from `orderable_since`: this
    /// Machine's first pass since the gate held or refused the hostname, or since a pass
    /// stopped before the gate.
    Waiting { orderable_since: Instant },
    /// The shared backoff clock holds the hostname.
    Held(IssuanceClock),
    /// This Machine's Hostname Verdict refuses the hostname, and `clock` is the backoff it
    /// records.
    Refused {
        refusal: Refusal,
        clock: IssuanceClock,
    },
    /// Order a first certificate along the route.
    Order(ClusterRoute),
    /// Renew along the route.
    Renew(ClusterRoute),
}

/// `Issued` when the row holds material whose renewal is not due, so the pass needs no probe.
fn issued(
    row: Option<&CertificateRow>,
    rank: usize,
    wall: SystemTime,
    machine_id: &MachineId,
    policy: &CertificatePolicy,
) -> Option<HostIssuance> {
    row.and_then(CertificateRow::material)?;
    match action_due(row, rank, Duration::ZERO, wall, machine_id, policy) {
        (IssuanceAction::Nothing, wait) => Some(HostIssuance::Issued {
            renew_due: saturating_add(wall, wait),
        }),
        (IssuanceAction::Order | IssuanceAction::Renew, _) => None,
    }
}

/// The next state of a hostname this pass probed: `prev` is its state after this Machine's
/// last pass, and `row` the row this pass read. A hold or a refusal ends the rank delay, so
/// the first pass the gate lets through after one starts it again.
#[expect(
    clippy::too_many_arguments,
    reason = "every input of one transition, each a value the pass read"
)]
fn step(
    prev: Option<HostIssuance>,
    row: Option<&CertificateRow>,
    verdict: HostnameVerdict,
    rank: usize,
    now: Instant,
    wall: SystemTime,
    machine_id: &MachineId,
    policy: &CertificatePolicy,
) -> HostIssuance {
    let route = match issuance_gate(
        row.and_then(CertificateRow::clock),
        verdict,
        wall,
        policy.backoff_base(),
        policy.backoff_cap(),
    ) {
        IssuanceGate::Hold(clock) => return HostIssuance::Held(clock),
        IssuanceGate::Refuse { refusal, clock } => {
            return HostIssuance::Refused { refusal, clock };
        }
        IssuanceGate::Order(route) => route,
    };
    if row.and_then(CertificateRow::material).is_some() {
        return HostIssuance::Renew(route);
    }
    let orderable_since = match prev {
        Some(HostIssuance::Waiting { orderable_since }) => orderable_since,
        Some(HostIssuance::Order(_)) => return HostIssuance::Order(route),
        Some(
            HostIssuance::Issued { .. }
            | HostIssuance::Held(_)
            | HostIssuance::Refused { .. }
            | HostIssuance::Renew(_),
        )
        | None => now,
    };
    let elapsed = now.saturating_duration_since(orderable_since);
    match issuance_action(row, rank, elapsed, wall, machine_id, policy) {
        IssuanceAction::Order | IssuanceAction::Renew => HostIssuance::Order(route),
        IssuanceAction::Nothing => HostIssuance::Waiting { orderable_since },
    }
}

/// How long the loop may sleep before `state` wants another pass. A clock that holds wakes
/// the loop when it passes, and no later than its next probe: `RANK_STEP` after a refusal,
/// which a changed verdict lifts early, and `RETRY_INTERVAL` after an authority failure, which
/// only time lifts.
fn wake(
    state: HostIssuance,
    rank: usize,
    now: Instant,
    wall: SystemTime,
    machine_id: &MachineId,
    policy: &CertificatePolicy,
) -> Duration {
    match state {
        HostIssuance::Issued { renew_due } => renew_due
            .duration_since(wall)
            .unwrap_or(Duration::ZERO)
            .min(RETRY_INTERVAL),
        HostIssuance::Waiting { orderable_since } => poll_wait(
            None,
            rank,
            now.saturating_duration_since(orderable_since),
            wall,
            machine_id,
            policy,
        ),
        HostIssuance::Held(clock) | HostIssuance::Refused { clock, .. } => {
            let probe = match clock.last_failure() {
                IssuanceFailure::Refused(_) => RANK_STEP,
                IssuanceFailure::Authority => RETRY_INTERVAL,
            };
            clock
                .next_attempt_at()
                .duration_since(wall)
                .unwrap_or(Duration::ZERO)
                .min(probe)
        }
        HostIssuance::Order(_) | HostIssuance::Renew(_) => RANK_STEP,
    }
}

/// Addresses the ordering Machine must see the challenge on before validation.
///
/// Direct: the resolved ingress Machines. Via a proxy: every resolved proxy address.
#[must_use]
pub(crate) fn challenge_probe_addresses(
    resolved: &[IpAddr],
    ingress_ips: &BTreeSet<IpAddr>,
    route: ClusterRoute,
) -> Vec<SocketAddr> {
    resolved
        .iter()
        .copied()
        .filter(|address| route == ClusterRoute::ViaProxy || ingress_ips.contains(address))
        .map(|address| SocketAddr::new(address, 80))
        .collect()
}

fn ingress_challenge_ips(
    machines: &[Machine],
    observations: &[ContainerObservation],
) -> BTreeSet<IpAddr> {
    let ingress: BTreeSet<_> = observations
        .iter()
        .filter(|observation| {
            crate::ingress::is_system_ingress(observation) && observation.runtime.may_serve()
        })
        .map(|observation| observation.machine_id)
        .collect();
    machines
        .iter()
        .filter(|machine| ingress.contains(&machine.id))
        .flat_map(machine_challenge_ips)
        .collect()
}

fn machine_challenge_ips(machine: &Machine) -> impl Iterator<Item = IpAddr> {
    machine.public_ip.into_iter().chain(
        machine
            .advertised_endpoints
            .iter()
            .map(|endpoint| endpoint.0.ip()),
    )
}

fn authorization_status(status: AuthorizationStatus) -> &'static str {
    match status {
        AuthorizationStatus::Pending => "pending",
        AuthorizationStatus::Valid => "valid",
        AuthorizationStatus::Invalid => "invalid",
        AuthorizationStatus::Revoked => "revoked",
        AuthorizationStatus::Expired => "expired",
        AuthorizationStatus::Deactivated => "deactivated",
    }
}

fn order_status(status: OrderStatus) -> &'static str {
    match status {
        OrderStatus::Pending => "pending",
        OrderStatus::Ready => "ready",
        OrderStatus::Processing => "processing",
        OrderStatus::Valid => "valid",
        OrderStatus::Invalid => "invalid",
    }
}

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Store(#[from] crate::corrosion::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("ACME request failed")]
    Acme(#[from] instant_acme::Error),
    #[error("certificate authority did not issue material")]
    MissingMaterial,
    #[error("certificate authority returned invalid or mismatched certificate material")]
    InvalidMaterial,
    #[error("certificate authority returned an invalid HTTP-01 challenge")]
    InvalidChallenge,
    #[error("HTTP-01 challenge was not served by the Ingress Proxy")]
    ChallengeNotServed,
    #[error("authorization for {hostname} is {}", authorization_status(*status))]
    Authorization {
        hostname: IngressHost,
        status: AuthorizationStatus,
    },
    #[error("order for {hostname} is {}", order_status(*status))]
    Order {
        hostname: IngressHost,
        status: OrderStatus,
    },
    #[error("no HTTP-01 challenge for {0}")]
    NoHttp01(IngressHost),
    #[error("certificate key type {0} is not supported")]
    UnsupportedKeyType(String),
    #[error("certificate key: {0}")]
    Key(String),
    #[error("external account binding hmac_key is not base64")]
    InvalidEab,
}

/// Built-in directory, or `PLOYZ_ACME_DIRECTORY`. Empty disables issuance.
#[must_use]
pub(crate) fn directory_from_env(value: Option<&str>) -> Option<String> {
    match value {
        Some("") => None,
        Some(url) => Some(url.to_owned()),
        None => Some(instant_acme::LetsEncrypt::Production.url().to_owned()),
    }
}

#[must_use]
pub(crate) fn directory_url() -> Option<String> {
    directory_from_env(std::env::var(DIRECTORY_ENV).ok().as_deref())
}

/// https Ingress Hostnames on Service Containers that ACME must serve: hostnames
/// with published material, or under a published wildcard, are not wanted.
/// The daemon does not know a Cluster Domain.
#[must_use]
pub(crate) fn wanted_certificate_hosts<'a>(
    observations: impl IntoIterator<Item = &'a ContainerObservation>,
    rows: &BTreeMap<CertificateHost, CertificateRow>,
) -> BTreeSet<IngressHost> {
    let mut wanted = BTreeSet::new();
    for observation in observations {
        if observation.kind != ContainerKind::ServiceContainer {
            continue;
        }
        for port in &observation.resolved_spec.ports {
            let PortPublication::Ingress {
                hostname,
                http_protocol: HttpProtocol::Https,
                ..
            } = port
            else {
                continue;
            };
            if published_cover(hostname, rows).is_none() {
                wanted.insert(hostname.clone());
            }
        }
    }
    wanted
}

pub(crate) async fn run(
    store: ReplicatedStore,
    data_dir: PathBuf,
    directory_default: Option<String>,
    machine_id: MachineId,
    shutdown: CancellationToken,
) {
    let account_dir = data_dir.join("acme");
    let mut hosts = BTreeMap::new();
    loop {
        let subscribed = tokio::select! {
            subscribed = store.subscribe_container_changes() => subscribed,
            () = shutdown.cancelled() => return,
        };
        let mut changes = match subscribed {
            Ok(changes) => changes,
            Err(error) => {
                tracing::warn!(error = %ployz_core::error_chain::inline(&error), "certificate subscription failed, retrying");
                if wait_to_resubscribe(&shutdown).await {
                    continue;
                }
                return;
            }
        };
        loop {
            let wait = match issue_wanted(
                &store,
                &directory_default,
                &account_dir,
                &machine_id,
                &mut hosts,
            )
            .await
            {
                Ok(wait) => wait,
                Err(error) => {
                    eprintln!(
                        "failed to obtain certificates: {error}",
                        error = ployz_core::error_chain::inline(&error),
                    );
                    RETRY_INTERVAL
                }
            };
            tokio::select! {
                changed = changes.changed() => if let Err(error) = changed {
                    tracing::warn!(error = %ployz_core::error_chain::inline(&error), "certificate subscription ended, reopening");
                    break;
                },
                () = tokio::time::sleep(wait) => {}
                () = shutdown.cancelled() => return,
            }
        }
        if !wait_to_resubscribe(&shutdown).await {
            return;
        }
    }
}

async fn wait_to_resubscribe(shutdown: &CancellationToken) -> bool {
    tokio::select! {
        () = tokio::time::sleep(RESUBSCRIBE_INTERVAL) => true,
        () = shutdown.cancelled() => false,
    }
}

/// What a pass reads before it decides any hostname.
struct PassInputs {
    policy: CertificatePolicy,
    directory: String,
    wanted: BTreeSet<IngressHost>,
    rows: BTreeMap<CertificateHost, CertificateRow>,
    machine_ids: Vec<MachineId>,
    cluster: Vec<IpAddr>,
    verify_client: Client,
    rank: usize,
}

/// The pass's reads, or `None` when it stops before deciding any hostname: the Certificate
/// Policy is refused, which every Machine records for every wanted hostname, it has no
/// directory, or no hostname is wanted.
async fn read_pass(
    store: &ReplicatedStore,
    directory_default: &Option<String>,
    machine_id: &MachineId,
) -> Result<Option<PassInputs>, Error> {
    let policy = match resolve_certificate_policy(
        store.certificate_policy().await?.as_deref(),
        &CertificatePolicy::built_in(directory_default.clone()),
    ) {
        Ok(policy) => policy,
        Err(refusal) => {
            let containers = store.containers().await?;
            let rows = store.certificate_state().await?;
            for hostname in wanted_certificate_hosts(containers.observations.iter(), &rows) {
                match store
                    .record_certificate_error(&hostname, refusal.reason())
                    .await?
                {
                    AcmeWrite::Changed | AcmeWrite::Kept => {}
                    AcmeWrite::Moved => eprintln!(
                        "did not record the certificate policy refusal for {hostname}: its row changed on every attempt"
                    ),
                }
            }
            return Ok(None);
        }
    };
    let Some(directory) = policy.directory_url().map(str::to_owned) else {
        return Ok(None);
    };
    let containers = store.containers().await?;
    let rows = store.certificate_state().await?;
    let wanted = wanted_certificate_hosts(containers.observations.iter(), &rows);
    if wanted.is_empty() {
        return Ok(None);
    }
    let machines = store.machines().await?;
    let verify_client = probe_client(policy.probe_timeout())?;
    let rank = machine_rank(
        machine_id,
        machines.observations.iter().map(|machine| &machine.id),
    );
    Ok(Some(PassInputs {
        policy,
        directory,
        wanted,
        rows,
        machine_ids: machines
            .observations
            .iter()
            .map(|machine| machine.id)
            .collect(),
        cluster: cluster_addresses(&machines.observations),
        verify_client,
        rank,
    }))
}

async fn issue_wanted(
    store: &ReplicatedStore,
    directory_default: &Option<String>,
    account_dir: &Path,
    machine_id: &MachineId,
    hosts: &mut BTreeMap<IngressHost, HostIssuance>,
) -> Result<Duration, Error> {
    let read = read_pass(store, directory_default, machine_id).await;
    let Ok(Some(PassInputs {
        policy,
        directory,
        wanted,
        rows,
        machine_ids,
        cluster,
        verify_client,
        rank,
    })) = read
    else {
        // A pass that stops before the gate is a blocked pass, as a hold is: the rank delay
        // starts again at the first pass that reaches the gate.
        hosts.clear();
        return read.map(|_| RETRY_INTERVAL);
    };
    hosts.retain(|hostname, _| wanted.contains(hostname));
    let now = Instant::now();
    let wall = SystemTime::now();
    let mut to_order = Vec::new();
    for hostname in &wanted {
        let row = rows.get(hostname.as_str());
        let state = match issued(row, rank, wall, machine_id, &policy) {
            Some(state) => state,
            None => {
                let addresses = resolve_host(hostname)
                    .await
                    .into_iter()
                    .map(|address| SocketAddr::new(address, 80))
                    .collect::<Vec<_>>();
                let answers = verify_answers(&verify_client, hostname, &addresses).await;
                let verdict = hostname_verdict(&answers, &machine_ids, &cluster);
                let prev = hosts.get(hostname).copied();
                step(prev, row, verdict, rank, now, wall, machine_id, &policy)
            }
        };
        hosts.insert(hostname.clone(), state);
        match state {
            HostIssuance::Refused { refusal, clock } => {
                // Every rank records its own refusal, so the row says why even while the
                // lowest-id Machine runs no certificate loop.
                let reason = refusal_reason(hostname, refusal, &cluster);
                let tried = row.and_then(CertificateRow::material);
                match store
                    .record_certificate_failure(hostname, tried, reason, clock)
                    .await
                {
                    Ok(AcmeWrite::Changed | AcmeWrite::Kept) => {}
                    Ok(AcmeWrite::Moved) if tried.is_some() => eprintln!(
                        "did not record certificate refusal for {hostname}: its row no longer holds the failed material as ACME material"
                    ),
                    Ok(AcmeWrite::Moved) => eprintln!(
                        "did not record certificate refusal for {hostname}: its row changed on every attempt"
                    ),
                    Err(error) => eprintln!(
                        "failed to record certificate refusal for {hostname}: {error}",
                        error = ployz_core::error_chain::inline(&error),
                    ),
                }
            }
            HostIssuance::Order(route) | HostIssuance::Renew(route) => {
                to_order.push((hostname, route));
            }
            HostIssuance::Issued { .. } | HostIssuance::Waiting { .. } | HostIssuance::Held(_) => {}
        }
    }
    if !to_order.is_empty() {
        account(&directory, &policy, account_dir).await?;
        let results = join_all(to_order.iter().map(|(hostname, route)| {
            obtain(
                store,
                hostname,
                *route,
                &policy,
                account_dir,
                rank,
                machine_id,
            )
        }))
        .await;
        for ((hostname, _), result) in to_order.iter().zip(results) {
            if let Err(error) = result {
                eprintln!(
                    "failed to obtain certificate for {hostname}: {error}",
                    error = ployz_core::error_chain::inline(&error),
                );
                let clock = issuance_failure_clock(
                    rows.get(hostname.as_str()).and_then(CertificateRow::clock),
                    IssuanceFailure::Authority,
                    wall,
                    policy.backoff_base(),
                    policy.backoff_cap(),
                );
                let tried = rows
                    .get(hostname.as_str())
                    .and_then(CertificateRow::material);
                match store
                    .record_certificate_failure(
                        hostname,
                        tried,
                        ployz_core::error_chain::inline(&error),
                        clock,
                    )
                    .await
                {
                    Ok(AcmeWrite::Changed | AcmeWrite::Kept) => {}
                    Ok(AcmeWrite::Moved) if tried.is_some() => eprintln!(
                        "did not record certificate authority failure for {hostname}: its row no longer holds the failed material as ACME material"
                    ),
                    Ok(AcmeWrite::Moved) => eprintln!(
                        "did not record certificate authority failure for {hostname}: its row changed on every attempt"
                    ),
                    Err(record_error) => eprintln!(
                        "failed to record certificate authority failure for {hostname}: {record_error}"
                    ),
                }
            }
        }
    }
    let now = Instant::now();
    let wall = SystemTime::now();
    Ok(hosts
        .values()
        .map(|state| wake(*state, rank, now, wall, machine_id, &policy))
        .min()
        .unwrap_or(RETRY_INTERVAL))
}

fn cluster_addresses(machines: &[Machine]) -> Vec<IpAddr> {
    machines
        .iter()
        .filter_map(|machine| machine.public_ip)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

async fn obtain(
    store: &ReplicatedStore,
    hostname: &IngressHost,
    route: ClusterRoute,
    policy: &CertificatePolicy,
    account_dir: &Path,
    rank: usize,
    machine_id: &MachineId,
) -> Result<(), Error> {
    let row = store.certificate_row(hostname).await?;
    if row.material().is_some()
        && issuance_action(
            Some(&row),
            rank,
            Duration::ZERO,
            SystemTime::now(),
            machine_id,
            policy,
        ) == IssuanceAction::Nothing
    {
        return Ok(());
    }
    let probe_timeout = policy.probe_timeout();
    let material = order_certificate(hostname, policy, account_dir, |challenge| {
        let store = store.clone();
        let hostname = hostname.clone();
        async move {
            match store
                .publish_certificate_challenge(&hostname, &challenge)
                .await?
            {
                AcmeWrite::Changed | AcmeWrite::Kept => {}
                AcmeWrite::Moved => eprintln!(
                    "did not publish the HTTP-01 challenge for {hostname}: its row changed on every attempt"
                ),
            }
            let resolved = resolve_host(&hostname).await;
            let machines = store.machines().await?;
            let containers = store.containers().await?;
            let ingress_ips =
                ingress_challenge_ips(&machines.observations, &containers.observations);
            let addresses = challenge_probe_addresses(&resolved, &ingress_ips, route);
            wait_for_http01(&hostname, &challenge, &addresses, route, probe_timeout).await
        }
    })
    .await?;
    match store
        .publish_certificate(hostname, &material, route)
        .await?
    {
        AcmeWrite::Changed | AcmeWrite::Kept => {}
        AcmeWrite::Moved => eprintln!(
            "did not record the certificate issued for {hostname}: its row changed on every attempt"
        ),
    }
    Ok(())
}

/// Order one certificate. `present` publishes the HTTP-01 answer before validation.
///
/// # Errors
///
/// Returns if the directory, account, challenge presentation, or issuance fails.
pub(crate) async fn order_certificate<F, Fut>(
    hostname: &IngressHost,
    policy: &CertificatePolicy,
    account_dir: &Path,
    mut present: F,
) -> Result<CertificateMaterial, Error>
where
    F: FnMut(CertificateChallenge) -> Fut,
    Fut: Future<Output = Result<(), Error>>,
{
    let directory = policy.directory_url().ok_or(Error::MissingMaterial)?;
    let account = account(directory, policy, account_dir).await?;
    let identifiers = [Identifier::Dns(hostname.as_str().to_owned())];
    let mut order = account
        .new_order(&NewOrder::new(identifiers.as_slice()))
        .await?;
    let mut authorizations = order.authorizations();
    while let Some(result) = authorizations.next().await {
        let mut authz = result?;
        match authz.status {
            AuthorizationStatus::Pending => {}
            AuthorizationStatus::Valid => continue,
            status @ (AuthorizationStatus::Invalid
            | AuthorizationStatus::Revoked
            | AuthorizationStatus::Expired
            | AuthorizationStatus::Deactivated) => {
                return Err(Error::Authorization {
                    hostname: hostname.clone(),
                    status,
                });
            }
        }
        let mut challenge = authz
            .challenge(ChallengeType::Http01)
            .ok_or_else(|| Error::NoHttp01(hostname.clone()))?;
        let presented = CertificateChallenge::parse(
            challenge.token.clone(),
            challenge.key_authorization().as_str(),
        )
        .map_err(|_| Error::InvalidChallenge)?;
        present(presented).await?;
        challenge.set_ready().await?;
    }
    let status = order.poll_ready(&RetryPolicy::default()).await?;
    if status != OrderStatus::Ready {
        return Err(Error::Order {
            hostname: hostname.clone(),
            status,
        });
    }
    let private_key = match policy.key_type() {
        CertificateKeyType::EcdsaP256 => order.finalize().await?,
        key_type @ (CertificateKeyType::EcdsaP384
        | CertificateKeyType::Rsa2048
        | CertificateKeyType::Unrecognized(_)) => {
            let (pem, csr) = certificate_request(hostname, key_type)?;
            order.finalize_csr(&csr).await?;
            pem
        }
    };
    let certificate = order.poll_certificate(&RetryPolicy::default()).await?;
    CertificateMaterial::parse(certificate, private_key).map_err(|_| Error::InvalidMaterial)
}

fn certificate_request(
    hostname: &IngressHost,
    key_type: &CertificateKeyType,
) -> Result<(String, Vec<u8>), Error> {
    let key = match key_type {
        CertificateKeyType::EcdsaP256 => {
            rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        }
        CertificateKeyType::EcdsaP384 => {
            rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384)
        }
        CertificateKeyType::Rsa2048 => rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256),
        CertificateKeyType::Unrecognized(kind) => {
            return Err(Error::UnsupportedKeyType(kind.clone()));
        }
    }
    .map_err(|error| Error::Key(ployz_core::error_chain::inline(&error)))?;
    let mut params = rcgen::CertificateParams::new(vec![hostname.as_str().to_owned()])
        .map_err(|error| Error::Key(ployz_core::error_chain::inline(&error)))?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    let csr = params
        .serialize_request(&key)
        .map_err(|error| Error::Key(ployz_core::error_chain::inline(&error)))?;
    Ok((key.serialize_pem(), csr.der().as_ref().to_vec()))
}

fn eab_key(policy: &CertificatePolicy) -> Result<Option<ExternalAccountKey>, Error> {
    let Some(eab) = policy.eab() else {
        return Ok(None);
    };
    let key = eab.to_hmac_key_bytes().map_err(|_| Error::InvalidEab)?;
    Ok(Some(ExternalAccountKey::new(eab.kid().to_owned(), &key)))
}

fn stored_directory(bytes: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Stored {
        directory: Option<String>,
    }
    serde_json::from_slice::<Stored>(bytes)
        .ok()
        .and_then(|stored| stored.directory)
}

async fn account(
    directory: &str,
    policy: &CertificatePolicy,
    account_dir: &Path,
) -> Result<Account, Error> {
    let http: Box<dyn HttpClient> = Box::new(ReqwestAcmeClient::new()?);
    let path = account_dir.join(ACCOUNT_FILE);
    if path.exists() {
        let bytes = std::fs::read(&path)?;
        if stored_directory(&bytes).as_deref() == Some(directory) {
            let credentials: AccountCredentials = serde_json::from_slice(&bytes)?;
            return Ok(Account::builder_with_http(http)
                .from_credentials(credentials)
                .await?);
        }
    }
    std::fs::create_dir_all(account_dir)?;
    let eab = eab_key(policy)?;
    let (account, credentials) = Account::builder_with_http(http)
        .create(
            &NewAccount {
                contact: &[],
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            directory.to_owned(),
            eab.as_ref(),
        )
        .await?;
    atomic_write(&path, &serde_json::to_vec(&credentials)?, 0o600)?;
    set_ployz_group(&path)?;
    Ok(account)
}

async fn resolve_host(hostname: &IngressHost) -> Vec<IpAddr> {
    let Ok(lookup) = tokio::net::lookup_host((hostname.as_str(), 80)).await else {
        return Vec::new();
    };
    lookup
        .map(|address| address.ip())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn probe_client(probe_timeout: Duration) -> Result<Client, Error> {
    Ok(Client::builder()
        .redirect(Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(2).min(probe_timeout))
        .build()?)
}

/// Fetch the verify path from each address with `hostname` as `Host`, without following redirects.
pub(crate) async fn verify_answers(
    client: &Client,
    hostname: &IngressHost,
    addresses: &[SocketAddr],
) -> Vec<(IpAddr, VerifyAnswer)> {
    join_all(addresses.iter().map(|address| async move {
        (
            address.ip(),
            verify_answer(client, hostname, *address).await,
        )
    }))
    .await
}

async fn verify_answer(
    client: &Client,
    hostname: &IngressHost,
    address: SocketAddr,
) -> VerifyAnswer {
    let Ok(response) = client
        .get(format!("http://{address}{HOSTNAME_VERIFY_PATH}"))
        .header(reqwest::header::HOST, hostname.as_str())
        .send()
        .await
    else {
        return VerifyAnswer::NoAnswer;
    };
    let status = response.status().as_u16();
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|location| location.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    VerifyAnswer::Answered {
        status,
        location,
        body: response.text().await.unwrap_or_default(),
    }
}

async fn wait_for_http01(
    hostname: &IngressHost,
    challenge: &CertificateChallenge,
    addresses: &[SocketAddr],
    route: ClusterRoute,
    probe_timeout: Duration,
) -> Result<(), Error> {
    if addresses.is_empty() {
        return Err(Error::ChallengeNotServed);
    }
    let client = probe_client(probe_timeout)?;
    let deadline = Instant::now() + probe_timeout;
    loop {
        let answered = join_all(addresses.iter().map(|address| {
            let client = &client;
            async move { challenge_is_served(client, hostname, challenge, *address).await }
        }))
        .await;
        // Every ingress Machine must serve it; any one proxy address shows the proxy forwards it.
        let served = match route {
            ClusterRoute::Direct => answered.iter().all(|served| *served),
            ClusterRoute::ViaProxy => answered.iter().any(|served| *served),
        };
        if served {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::ChallengeNotServed);
        }
        tokio::time::sleep(CHALLENGE_POLL).await;
    }
}

async fn challenge_is_served(
    client: &Client,
    hostname: &IngressHost,
    challenge: &CertificateChallenge,
    address: SocketAddr,
) -> bool {
    let url = format!(
        "http://{address}/.well-known/acme-challenge/{}",
        challenge.token()
    );
    let Ok(response) = client
        .get(url)
        .header(reqwest::header::HOST, hostname.as_str())
        .send()
        .await
    else {
        return false;
    };
    response.status().is_success()
        && response.text().await.ok().as_deref() == Some(challenge.response())
}

struct ReqwestAcmeClient {
    client: Client,
}

impl ReqwestAcmeClient {
    fn new() -> Result<Self, Error> {
        Ok(Self {
            client: Client::builder()
                .redirect(Policy::none())
                .no_proxy()
                .timeout(Duration::from_secs(30))
                .build()?,
        })
    }
}

impl HttpClient for ReqwestAcmeClient {
    fn request(
        &self,
        req: http::Request<instant_acme::BodyWrapper<Bytes>>,
    ) -> Pin<Box<dyn Future<Output = Result<BytesResponse, instant_acme::Error>> + Send>> {
        let client = self.client.clone();
        Box::pin(async move {
            let (parts, body) = req.into_parts();
            let body = body
                .collect()
                .await
                .map_err(|error| instant_acme::Error::Other(Box::new(error)))?
                .to_bytes();
            let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes())
                .map_err(|error| instant_acme::Error::Other(Box::new(error)))?;
            let mut request = client.request(method, parts.uri.to_string());
            for (name, value) in &parts.headers {
                request = request.header(name.as_str(), value.as_bytes());
            }
            let response = request
                .body(body)
                .send()
                .await
                .map_err(|error| instant_acme::Error::Other(Box::new(error)))?;
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let body = response
                .bytes()
                .await
                .map_err(|error| instant_acme::Error::Other(Box::new(error)))?;
            let mut builder = http::Response::builder().status(status);
            for (name, value) in &headers {
                builder = builder.header(name.as_str(), value.as_bytes());
            }
            let response = builder
                .body(http_body_util::Full::new(body))
                .map_err(|error| instant_acme::Error::Other(Box::new(error)))?;
            Ok(BytesResponse::from(response))
        })
    }
}

#[cfg(test)]
#[path = "certificates_tests.rs"]
mod tests;
