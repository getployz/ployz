//! Obtain certificates for https Ingress Hostnames.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    io,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    pin::Pin,
    time::Duration,
};

use bytes::Bytes;
use futures_util::future::join_all;
use http_body_util::BodyExt;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, BytesResponse, ChallengeType,
    ExternalAccountKey, HttpClient, Identifier, NewAccount, NewOrder, OrderStatus, RetryPolicy,
};
use ployz_core::{
    CertificateKeyType, CertificatePolicy, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, HealthObservation, HttpProtocol, IngressHost, IngressHostname,
    Machine, MachineId, PortPublication, resolve_certificate_policy,
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
}

/// Rank among Machine identifiers. Lowest id is 0 and may order immediately.
#[must_use]
pub(crate) fn machine_rank<'id>(
    this: &MachineId,
    machines: impl IntoIterator<Item = &'id MachineId>,
) -> usize {
    machines.into_iter().filter(|id| *id < this).count()
}

/// Whether this Machine should order now.
///
/// `step` must cover the HTTP-01 probe wait so a later rank cannot start
/// a competing order while an earlier rank is still presenting.
#[must_use]
pub(crate) fn issuance_action(
    row: Option<&CertificateRow>,
    rank: usize,
    elapsed: Duration,
    step: Duration,
) -> IssuanceAction {
    if row.and_then(CertificateRow::material).is_some() {
        return IssuanceAction::Nothing;
    }
    let delay = step.saturating_mul(u32::try_from(rank).unwrap_or(u32::MAX));
    if elapsed < delay {
        IssuanceAction::Nothing
    } else {
        IssuanceAction::Order
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
        .filter(|observation| caddy_is_running(observation))
        .map(|observation| observation.machine_id)
        .collect();
    machines
        .iter()
        .filter(|machine| caddy.contains(&machine.id))
        .flat_map(machine_challenge_ips)
        .collect()
}

fn caddy_is_running(observation: &ContainerObservation) -> bool {
    matches!(
        observation.runtime,
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy | HealthObservation::NotConfigured
        }
    )
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
        if let Err(error) = issue_wanted(
            &store,
            &directory_default,
            &account_dir,
            &machine_id,
            &mut first_seen,
        )
        .await
        {
            eprintln!("failed to obtain certificates: {error}");
        }
        let wait = if first_seen.is_empty() {
            RETRY_INTERVAL
        } else {
            RANK_STEP
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
) -> Result<(), Error> {
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
            return Ok(());
        }
    };
    let Some(directory) = policy.directory_url() else {
        return Ok(());
    };
    let containers = store.containers().await?;
    let wanted = wanted_certificate_hosts(containers.observations.iter());
    let rows = store.certificate_state().await?;
    let machines = store.machines().await?;
    let rank = machine_rank(
        machine_id,
        machines.observations.iter().map(|machine| &machine.id),
    );
    let now = Instant::now();
    first_seen.retain(|hostname, _| wanted.contains(hostname));
    let mut to_order = Vec::new();
    for hostname in &wanted {
        let row = rows.get(hostname);
        if row.and_then(CertificateRow::material).is_some() {
            first_seen.remove(hostname);
            continue;
        }
        let seen = if let Some(&seen) = first_seen.get(hostname) {
            seen
        } else {
            first_seen.insert(hostname.clone(), now);
            now
        };
        let elapsed = now.saturating_duration_since(seen);
        if issuance_action(row, rank, elapsed, RANK_STEP.max(policy.probe_timeout()))
            == IssuanceAction::Order
        {
            to_order.push(hostname);
        }
    }
    if to_order.is_empty() {
        return Ok(());
    }
    account(directory, &policy, account_dir).await?;
    let results = join_all(
        to_order
            .iter()
            .map(|hostname| obtain(store, hostname, &policy, account_dir)),
    )
    .await;
    for (hostname, result) in to_order.iter().zip(results) {
        if let Err(error) = result {
            eprintln!("failed to obtain certificate for {hostname}: {error}");
        }
    }
    Ok(())
}

async fn obtain(
    store: &ReplicatedStore,
    hostname: &IngressHost,
    policy: &CertificatePolicy,
    account_dir: &Path,
) -> Result<(), Error> {
    if store.certificate(hostname).await?.is_some() {
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
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        net::{IpAddr, SocketAddr},
        time::Duration,
    };

    use ployz_core::{
        CertificateKeyType, CertificatePolicy, ContainerAddress, ContainerId, ContainerKind,
        ContainerObservation, ContainerRuntimeObservation, HealthObservation, HttpProtocol,
        IngressHost, IngressHostname, Machine, MachineId, PortPublication, ResolvedServiceSpec,
        ServiceId, ServiceName, resolve_certificate_policy,
    };
    use serde_json::json;

    use super::{
        CHALLENGE_WAIT, IssuanceAction, RANK_STEP, caddy_challenge_ips, challenge_probe_addresses,
        directory_from_env, issuance_action, machine_rank, order_certificate, wait_for_http01,
        wanted_certificate_hosts,
    };
    use crate::corrosion::{CertificateChallenge, CertificateMaterial, CertificateRow};

    fn policy_for(directory: &str) -> CertificatePolicy {
        CertificatePolicy::built_in(Some(directory.to_owned()))
    }

    #[test]
    fn directory_empty_disables_issuance() {
        assert_eq!(directory_from_env(Some("")), None);
        assert_eq!(
            directory_from_env(Some("http://ca.test/directory")).as_deref(),
            Some("http://ca.test/directory")
        );
        assert_eq!(
            directory_from_env(None).as_deref(),
            Some("https://acme-v02.api.letsencrypt.org/directory")
        );
    }

    #[test]
    fn wanted_hosts_are_https_ingress_only() {
        let observations = [
            observation(
                1,
                "api",
                vec![
                    ingress("app.example.com", HttpProtocol::Https),
                    ingress("plain.example.com", HttpProtocol::Http),
                    ingress("web.opaque.ployz.example", HttpProtocol::Https),
                ],
            ),
            observation(2, "www", vec![ingress_assign(HttpProtocol::Https)]),
            {
                let mut hook = observation(
                    3,
                    "api",
                    vec![ingress("hook.example.com", HttpProtocol::Https)],
                );
                hook.kind = ContainerKind::PreDeployHook;
                hook
            },
        ];

        assert_eq!(
            wanted_certificate_hosts(observations.iter()),
            BTreeSet::from([host("app.example.com"), host("web.opaque.ployz.example"),])
        );
        assert_eq!(
            wanted_certificate_hosts(
                [observation(
                    1,
                    "api",
                    vec![ingress("plain.example.com", HttpProtocol::Http)]
                )]
                .iter()
            ),
            BTreeSet::new()
        );
        assert_eq!(
            wanted_certificate_hosts([observation(1, "api", Vec::new())].iter()),
            BTreeSet::new()
        );
    }

    #[test]
    fn lowest_machine_id_is_rank_zero() {
        let low = MachineId::parse("a".repeat(32)).unwrap();
        let high = MachineId::parse("f".repeat(32)).unwrap();
        assert_eq!(machine_rank(&low, &[high, low]), 0);
        assert_eq!(machine_rank(&high, &[low]), 1);
        assert_eq!(machine_rank(&low, &[]), 0);
    }

    #[test]
    fn only_rank_zero_orders_immediately() {
        assert_eq!(RANK_STEP, Duration::from_secs(30));
        assert_eq!(
            issuance_action(None, 0, Duration::ZERO, RANK_STEP),
            IssuanceAction::Order
        );
        assert_eq!(
            issuance_action(
                None,
                1,
                Duration::from_secs(30) - Duration::from_millis(1),
                RANK_STEP
            ),
            IssuanceAction::Nothing
        );
        assert_eq!(
            issuance_action(None, 1, Duration::from_secs(30), RANK_STEP),
            IssuanceAction::Order
        );
        assert_eq!(
            issuance_action(None, 2, Duration::from_secs(30), RANK_STEP),
            IssuanceAction::Nothing
        );
        assert_eq!(
            issuance_action(None, 2, Duration::from_secs(60), RANK_STEP),
            IssuanceAction::Order
        );
        assert_eq!(
            issuance_action(None, 1, Duration::from_secs(30), Duration::from_secs(120)),
            IssuanceAction::Nothing
        );
    }

    #[test]
    fn issued_material_means_nothing_to_do() {
        let material = CertificateMaterial::new("CERT", "KEY").unwrap();
        let row = CertificateRow::from_parts(Some(material), None);
        assert_eq!(
            issuance_action(Some(&row), 0, Duration::from_secs(60), RANK_STEP),
            IssuanceAction::Nothing
        );
    }

    #[test]
    fn probe_addresses_are_the_caddy_intersection() {
        let caddy_ips = BTreeSet::from([ip("192.0.2.1"), ip("192.0.2.2")]);
        assert_eq!(
            challenge_probe_addresses(&[ip("192.0.2.2"), ip("198.51.100.10")], &caddy_ips),
            vec![socket("192.0.2.2")]
        );
        assert_eq!(
            challenge_probe_addresses(&[ip("198.51.100.10")], &caddy_ips),
            Vec::<SocketAddr>::new()
        );
        assert_eq!(
            challenge_probe_addresses(&[], &caddy_ips),
            Vec::<SocketAddr>::new()
        );
    }

    #[test]
    fn caddy_challenge_ips_come_from_running_caddy_machines() {
        let local = machine_with_endpoint("a", "192.0.2.1");
        let remote = machine_with_endpoint("b", "192.0.2.2");
        let mut caddy = observation(1, "caddy", Vec::new());
        caddy.machine_id = local.id;
        caddy.service_name = ServiceName::parse("caddy").unwrap();
        let mut down = observation(2, "caddy", Vec::new());
        down.machine_id = remote.id;
        down.service_name = ServiceName::parse("caddy").unwrap();
        down.runtime = ContainerRuntimeObservation::Exited { code: 1 };
        assert_eq!(
            caddy_challenge_ips(&[local, remote], &[caddy, down]),
            BTreeSet::from([ip("192.0.2.1")])
        );
    }

    #[tokio::test]
    async fn challenge_must_be_answerable_on_every_probe_address() {
        let hostname = host("app.example.com");
        let challenge = CertificateChallenge::new("tok", "tok.thumb").unwrap();
        let answers = std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::from([(
            "tok".to_owned(),
            "tok.thumb".to_owned(),
        )])));
        let (first_stop, first_port) =
            ployz_testkit::fake_acme::serve_http01(std::sync::Arc::clone(&answers));
        let (second_stop, second_port) = ployz_testkit::fake_acme::serve_http01(answers);
        wait_for_http01(
            &hostname,
            &challenge,
            &[
                SocketAddr::from(([127, 0, 0, 1], first_port)),
                SocketAddr::from(([127, 0, 0, 1], second_port)),
            ],
            CHALLENGE_WAIT,
        )
        .await
        .unwrap();
        drop((first_stop, second_stop));
    }

    #[tokio::test]
    async fn empty_probe_addresses_fail_without_waiting() {
        let hostname = host("app.example.com");
        let challenge = CertificateChallenge::new("tok", "tok.thumb").unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_http01(&hostname, &challenge, &[], CHALLENGE_WAIT),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(matches!(error, super::Error::ChallengeNotServed));
    }

    #[tokio::test]
    async fn custom_https_hostname_obtains_a_certificate_from_a_fake_ca() {
        let hostname = host("app.example.com");
        let answers = std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::new()));
        let (http01, validation_port) =
            ployz_testkit::fake_acme::serve_http01(std::sync::Arc::clone(&answers));
        let ca = ployz_testkit::fake_acme::FakeCa::bind("127.0.0.1:0")
            .await
            .unwrap();
        ca.set_validation("127.0.0.1", validation_port);
        let account_dir =
            std::env::temp_dir().join(format!("ployzd-acme-{}-{}", std::process::id(), hostname));
        let policy = resolve_certificate_policy(
            Some(&format!(r#"{{"directory_url":"{}"}}"#, ca.directory_url())),
            &CertificatePolicy::built_in(None),
        )
        .unwrap();
        let material = order_certificate(&hostname, &policy, &account_dir, |challenge| {
            let answers = std::sync::Arc::clone(&answers);
            async move {
                answers.lock().unwrap().insert(
                    challenge.token().to_owned(),
                    challenge.response().to_owned(),
                );
                Ok(())
            }
        })
        .await
        .unwrap();

        assert!(material.certificate().contains("BEGIN CERTIFICATE"));
        assert!(material.private_key().contains("BEGIN"));
        assert_eq!(ca.ordered(), vec!["app.example.com".to_owned()]);
        drop(http01);
        let _ = std::fs::remove_dir_all(account_dir);
    }

    #[tokio::test]
    async fn policy_key_type_and_eab_issue_against_a_fake_ca() {
        let hostname = host("rsa.example.com");
        let answers = std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::new()));
        let (http01, validation_port) =
            ployz_testkit::fake_acme::serve_http01(std::sync::Arc::clone(&answers));
        let ca = ployz_testkit::fake_acme::FakeCa::bind("127.0.0.1:0")
            .await
            .unwrap();
        ca.set_validation("127.0.0.1", validation_port);
        let account_dir = std::env::temp_dir().join(format!(
            "ployzd-acme-policy-{}-{}",
            std::process::id(),
            hostname
        ));
        let policy = resolve_certificate_policy(
            Some(&format!(
                r#"{{
                    "directory_url":"{}",
                    "eab":{{"kid":"kid-1","hmac_key":"dGVzdA"}},
                    "key_type":"ecdsa-p384",
                    "probe_timeout":5
                }}"#,
                ca.directory_url()
            )),
            &CertificatePolicy::built_in(None),
        )
        .unwrap();
        assert_eq!(policy.key_type(), &CertificateKeyType::EcdsaP384);
        assert_eq!(policy.eab().unwrap().kid(), "kid-1");
        let material = order_certificate(&hostname, &policy, &account_dir, |challenge| {
            let answers = std::sync::Arc::clone(&answers);
            async move {
                answers.lock().unwrap().insert(
                    challenge.token().to_owned(),
                    challenge.response().to_owned(),
                );
                Ok(())
            }
        })
        .await
        .unwrap();

        assert!(material.certificate().contains("BEGIN CERTIFICATE"));
        assert!(material.private_key().contains("BEGIN"));
        assert_eq!(ca.ordered(), vec!["rsa.example.com".to_owned()]);
        drop(http01);
        let _ = std::fs::remove_dir_all(account_dir);
    }

    #[tokio::test]
    async fn order_fails_when_the_directory_is_unreachable() {
        let hostname = host("app.example.com");
        let account_dir = std::env::temp_dir().join(format!(
            "ployzd-acme-unreachable-{}-{}",
            std::process::id(),
            hostname
        ));
        let error = order_certificate(
            &hostname,
            &policy_for("http://127.0.0.1:1/directory"),
            &account_dir,
            |_| async { Ok(()) },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, super::Error::Acme(_) | super::Error::Http(_)),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(account_dir);
    }

    fn host(name: &str) -> IngressHost {
        IngressHost::parse(name).unwrap()
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    fn socket(value: &str) -> SocketAddr {
        SocketAddr::new(ip(value), 80)
    }

    fn machine_with_endpoint(seed: &str, address: &str) -> Machine {
        serde_json::from_value(json!({
            "id": seed.repeat(32),
            "name": format!("machine-{seed}"),
            "subnet": "10.210.1.0/24",
            "management_address": "fdcc::1",
            "public_key": vec![3; 32],
            "advertised_endpoints": [format!("{address}:51000")],
        }))
        .unwrap()
    }

    fn ingress(hostname: &str, http_protocol: HttpProtocol) -> PortPublication {
        PortPublication::Ingress {
            hostname: IngressHostname::explicit(hostname).unwrap(),
            load_balancer_port: 443.try_into().unwrap(),
            container_port: 8080.try_into().unwrap(),
            http_protocol,
        }
    }

    fn ingress_assign(http_protocol: HttpProtocol) -> PortPublication {
        PortPublication::Ingress {
            hostname: IngressHostname::AssignFromClusterDomain,
            load_balancer_port: 443.try_into().unwrap(),
            container_port: 8080.try_into().unwrap(),
            http_protocol,
        }
    }

    fn observation(
        suffix: u8,
        service_name: &str,
        ports: Vec<PortPublication>,
    ) -> ContainerObservation {
        let service_id = ServiceId::parse(format!("{suffix:x}").repeat(32)).unwrap();
        let service_name = ServiceName::parse(service_name).unwrap();
        let resolved_spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": service_id,
            "name": service_name,
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "example.test/image", "pull_policy": "missing" },
            "ports": ports,
        }))
        .unwrap();
        ContainerObservation {
            container_id: ContainerId::parse(format!("{suffix:x}").repeat(64)).unwrap(),
            display_name: format!("{service_name}-{suffix}"),
            created_at_unix_nanos: 0,
            machine_id: MachineId::parse("a".repeat(32)).unwrap(),
            service_id,
            service_name,
            kind: ContainerKind::ServiceContainer,
            runtime: ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            effective_healthcheck: None,
            resolved_spec,
            address: Some(ContainerAddress([10, 210, 1, 2].into())),
            labels: BTreeMap::new(),
        }
    }
}
