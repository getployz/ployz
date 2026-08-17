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
    CertificateKeyType, CertificatePolicy, ContainerKind, ContainerObservation, HttpProtocol,
    IngressHost, IngressHostname, IssuanceFailure, IssuanceGate, Machine, MachineId,
    PortPublication, cluster_dns_verdict, issuance_failure_clock, issuance_gate,
    issuance_refusal_reason, resolve_certificate_policy,
};
use reqwest::{Client, redirect::Policy};
use thiserror::Error;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    corrosion::{CertificateChallenge, CertificateMaterial, CertificateRow, ReplicatedStore},
    filesystem::{atomic_write, set_ployz_group},
};

pub(crate) const DIRECTORY_ENV: &str = "PLOYZ_ACME_DIRECTORY";
const ACCOUNT_FILE: &str = "account.json";
const CADDY_SERVICE: &str = "caddy";
const CHALLENGE_WAIT: Duration = Duration::from_secs(30);
const CHALLENGE_POLL: Duration = Duration::from_millis(200);
const RETRY_INTERVAL: Duration = Duration::from_secs(60);
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
        machine_jitter(machine_id, remaining),
    ))
}

/// Jitter in `[0, remaining)` from the Machine identity.
#[must_use]
pub(crate) fn machine_jitter(machine_id: &MachineId, remaining: Duration) -> Duration {
    if remaining.is_zero() {
        return Duration::ZERO;
    }
    let seed = machine_id
        .as_str()
        .as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        });
    let nanos = remaining.as_nanos().saturating_mul(u128::from(seed)) / (u128::from(u64::MAX) + 1);
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

/// Addresses the ordering Machine must see the challenge on before validation.
#[must_use]
pub(crate) fn challenge_probe_addresses(
    resolved: &[IpAddr],
    caddy_ips: &BTreeSet<IpAddr>,
) -> Vec<SocketAddr> {
    resolved
        .iter()
        .copied()
        .filter(|address| caddy_ips.contains(address))
        .map(|address| SocketAddr::new(address, 80))
        .collect()
}

fn caddy_challenge_ips(
    machines: &[Machine],
    observations: &[ContainerObservation],
) -> BTreeSet<IpAddr> {
    let caddy: BTreeSet<_> = observations
        .iter()
        .filter(|observation| observation.service_name.as_str() == CADDY_SERVICE)
        .filter(|observation| observation.runtime.is_healthy())
        .map(|observation| observation.machine_id)
        .collect();
    machines
        .iter()
        .filter(|machine| caddy.contains(&machine.id))
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
    #[error("ACME: {0}")]
    Acme(#[from] instant_acme::Error),
    #[error("certificate authority did not issue material")]
    MissingMaterial,
    #[error("HTTP-01 challenge was not served by the proxy")]
    ChallengeNotServed,
    #[error("authorization for {hostname} is {status:?}")]
    Authorization {
        hostname: IngressHost,
        status: AuthorizationStatus,
    },
    #[error("order for {hostname} is {status:?}")]
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

/// https Ingress Hostnames on Service Containers. The daemon does not know a Cluster Domain.
#[must_use]
pub(crate) fn wanted_certificate_hosts<'a>(
    observations: impl IntoIterator<Item = &'a ContainerObservation>,
) -> BTreeSet<IngressHost> {
    let mut wanted = BTreeSet::new();
    for observation in observations {
        if observation.kind != ContainerKind::ServiceContainer {
            continue;
        }
        for port in &observation.resolved_spec.ports {
            let PortPublication::Ingress {
                hostname: IngressHostname::Explicit { hostname },
                http_protocol: HttpProtocol::Https,
                ..
            } = port
            else {
                continue;
            };
            wanted.insert(hostname.clone());
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
) -> io::Result<()> {
    let account_dir = data_dir.join("acme");
    let mut changes = store
        .subscribe_container_changes()
        .await
        .map_err(io::Error::other)?;
    let mut first_seen = BTreeMap::new();
    loop {
        let wait = match issue_wanted(
            &store,
            &directory_default,
            &account_dir,
            &machine_id,
            &mut first_seen,
        )
        .await
        {
            Ok(wait) => wait,
            Err(error) => {
                eprintln!("failed to obtain certificates: {error}");
                RETRY_INTERVAL
            }
        };
        tokio::select! {
            changed = changes.changed() => changed.map_err(io::Error::other)?,
            () = tokio::time::sleep(wait) => {}
            () = shutdown.cancelled() => return Ok(()),
        }
    }
}

async fn issue_wanted(
    store: &ReplicatedStore,
    directory_default: &Option<String>,
    account_dir: &Path,
    machine_id: &MachineId,
    first_seen: &mut BTreeMap<IngressHost, Instant>,
) -> Result<Duration, Error> {
    let policy = match resolve_certificate_policy(
        store.certificate_policy().await?.as_deref(),
        &CertificatePolicy::built_in(directory_default.clone()),
    ) {
        Ok(policy) => policy,
        Err(refusal) => {
            let containers = store.containers().await?;
            for hostname in wanted_certificate_hosts(containers.observations.iter()) {
                store
                    .record_certificate_error(&hostname, refusal.reason())
                    .await?;
            }
            return Ok(RETRY_INTERVAL);
        }
    };
    let Some(directory) = policy.directory_url() else {
        return Ok(RETRY_INTERVAL);
    };
    let containers = store.containers().await?;
    let wanted = wanted_certificate_hosts(containers.observations.iter());
    first_seen.retain(|hostname, _| wanted.contains(hostname));
    if wanted.is_empty() {
        return Ok(RETRY_INTERVAL);
    }
    let mut rows = store.certificate_state().await?;
    let machines = store.machines().await?;
    let cluster = cluster_addresses(&machines.observations);
    let rank = machine_rank(
        machine_id,
        machines.observations.iter().map(|machine| &machine.id),
    );
    let now = Instant::now();
    let wall = SystemTime::now();
    let mut to_order = Vec::new();
    for hostname in &wanted {
        let row = rows.get(hostname);
        let elapsed = match row.and_then(CertificateRow::material) {
            Some(_) => {
                first_seen.remove(hostname);
                Duration::ZERO
            }
            None => {
                let seen = if let Some(&seen) = first_seen.get(hostname) {
                    seen
                } else {
                    first_seen.insert(hostname.clone(), now);
                    now
                };
                now.saturating_duration_since(seen)
            }
        };
        let due = issuance_action(row, rank, elapsed, wall, machine_id, &policy);
        if row.and_then(CertificateRow::material).is_some()
            && !matches!(due, IssuanceAction::Order | IssuanceAction::Renew)
        {
            continue;
        }
        let resolved = resolve_host(hostname).await;
        let verdict = cluster_dns_verdict(&resolved, &cluster);
        let gate = issuance_gate(
            row.and_then(CertificateRow::clock),
            verdict,
            wall,
            policy.backoff_base(),
            policy.backoff_cap(),
        );
        match gate {
            IssuanceGate::Nothing => continue,
            IssuanceGate::Refuse(clock) => {
                first_seen.remove(hostname);
                if rank == 0 && row.and_then(CertificateRow::material).is_none() {
                    let reason = issuance_refusal_reason(hostname, &resolved, &cluster);
                    if let Err(error) = store
                        .record_certificate_failure(hostname, reason, clock)
                        .await
                    {
                        eprintln!("failed to record certificate refusal for {hostname}: {error}");
                    }
                }
                continue;
            }
            IssuanceGate::Order => {}
        }
        if contacts_authority(due, gate) {
            to_order.push(hostname);
        }
    }
    if !to_order.is_empty() {
        account(directory, &policy, account_dir).await?;
        let results = join_all(
            to_order
                .iter()
                .map(|hostname| obtain(store, hostname, &policy, account_dir, rank, machine_id)),
        )
        .await;
        for (hostname, result) in to_order.iter().zip(results) {
            if let Err(error) = result {
                eprintln!("failed to obtain certificate for {hostname}: {error}");
                let clock = issuance_failure_clock(
                    rows.get(*hostname).and_then(CertificateRow::clock),
                    IssuanceFailure::Authority,
                    wall,
                    policy.backoff_base(),
                    policy.backoff_cap(),
                );
                if let Err(record_error) = store
                    .record_certificate_failure(hostname, error.to_string(), clock)
                    .await
                {
                    eprintln!(
                        "failed to record certificate authority failure for {hostname}: {record_error}"
                    );
                }
            }
        }
        rows = store.certificate_state().await?;
    }
    let now = Instant::now();
    let wall = SystemTime::now();
    Ok(wanted
        .iter()
        .map(|hostname| {
            let elapsed = first_seen
                .get(hostname)
                .map(|seen| now.saturating_duration_since(*seen))
                .unwrap_or(Duration::ZERO);
            poll_wait(rows.get(hostname), rank, elapsed, wall, machine_id, &policy)
        })
        .min()
        .unwrap_or(RETRY_INTERVAL))
}

/// Contact the CA only when the scheduler wants work and DNS says the hostname can validate.
#[must_use]
pub(crate) fn contacts_authority(due: IssuanceAction, gate: IssuanceGate) -> bool {
    matches!(due, IssuanceAction::Order | IssuanceAction::Renew)
        && matches!(gate, IssuanceGate::Order)
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
            store
                .publish_certificate_challenge(&hostname, &challenge)
                .await?;
            let resolved = resolve_host(&hostname).await;
            let machines = store.machines().await?;
            let containers = store.containers().await?;
            let caddy_ips = caddy_challenge_ips(&machines.observations, &containers.observations);
            let addresses = challenge_probe_addresses(&resolved, &caddy_ips);
            wait_for_http01(&hostname, &challenge, &addresses, probe_timeout).await
        }
    })
    .await?;
    store.publish_certificate(hostname, &material).await?;
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
        let presented = CertificateChallenge::new(
            challenge.token.clone(),
            challenge.key_authorization().as_str(),
        )
        .ok_or(Error::MissingMaterial)?;
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
    CertificateMaterial::new(certificate, private_key).ok_or(Error::MissingMaterial)
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
    .map_err(|error| Error::Key(error.to_string()))?;
    let mut params = rcgen::CertificateParams::new(vec![hostname.as_str().to_owned()])
        .map_err(|error| Error::Key(error.to_string()))?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    let csr = params
        .serialize_request(&key)
        .map_err(|error| Error::Key(error.to_string()))?;
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
    lookup.map(|address| address.ip()).collect()
}

async fn wait_for_http01(
    hostname: &IngressHost,
    challenge: &CertificateChallenge,
    addresses: &[SocketAddr],
    probe_timeout: Duration,
) -> Result<(), Error> {
    if addresses.is_empty() {
        return Err(Error::ChallengeNotServed);
    }
    let client = Client::builder()
        .redirect(Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(2).min(probe_timeout))
        .build()?;
    let deadline = Instant::now() + probe_timeout;
    loop {
        let answered = join_all(addresses.iter().map(|address| {
            let client = &client;
            async move { challenge_is_served(client, hostname, challenge, *address).await }
        }))
        .await;
        if answered.iter().all(|served| *served) {
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
