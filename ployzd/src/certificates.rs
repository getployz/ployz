//! Obtain certificates for https Ingress Hostnames.

use std::{
    collections::BTreeSet,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    time::Duration,
};

use bytes::Bytes;
use http_body_util::BodyExt;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, BytesResponse, ChallengeType, HttpClient,
    Identifier, NewAccount, NewOrder, OrderStatus, RetryPolicy,
};
use ployz_core::{
    ContainerKind, ContainerObservation, HttpProtocol, IngressHost, IngressHostname,
    PortPublication,
};
use reqwest::{Client, redirect::Policy};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    corrosion::{CertificateChallenge, CertificateMaterial, ReplicatedStore},
    filesystem::{atomic_write, set_ployz_group},
};

pub(crate) const DIRECTORY_ENV: &str = "PLOYZ_ACME_DIRECTORY";
const ACCOUNT_FILE: &str = "account.json";
const CHALLENGE_WAIT: Duration = Duration::from_secs(30);
const CHALLENGE_POLL: Duration = Duration::from_millis(200);
const RETRY_INTERVAL: Duration = Duration::from_secs(60);

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
    directory: Option<String>,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let Some(directory) = directory else {
        shutdown.cancelled().await;
        return Ok(());
    };
    let account_dir = data_dir.join("acme");
    let mut changes = store
        .subscribe_container_changes()
        .await
        .map_err(io::Error::other)?;
    loop {
        if let Err(error) = issue_wanted(&store, &directory, &account_dir).await {
            eprintln!("failed to obtain certificates: {error}");
        }
        tokio::select! {
            changed = changes.changed() => changed.map_err(io::Error::other)?,
            () = tokio::time::sleep(RETRY_INTERVAL) => {}
            () = shutdown.cancelled() => return Ok(()),
        }
    }
}

async fn issue_wanted(
    store: &ReplicatedStore,
    directory: &str,
    account_dir: &Path,
) -> Result<(), Error> {
    let containers = store.containers().await?;
    let wanted = wanted_certificate_hosts(containers.observations.iter());
    let rows = store.certificate_state().await?;
    for hostname in wanted {
        if rows.get(&hostname).and_then(|row| row.material()).is_some() {
            continue;
        }
        if let Err(error) = obtain(store, &hostname, directory, account_dir).await {
            eprintln!("failed to obtain certificate for {hostname}: {error}");
        }
    }
    Ok(())
}

async fn obtain(
    store: &ReplicatedStore,
    hostname: &IngressHost,
    directory: &str,
    account_dir: &Path,
) -> Result<(), Error> {
    let material = order_certificate(hostname, directory, account_dir, |challenge| {
        let store = store.clone();
        let hostname = hostname.clone();
        async move {
            store
                .publish_certificate_challenge(&hostname, &challenge)
                .await?;
            wait_for_http01(&hostname, &challenge).await
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
    directory: &str,
    account_dir: &Path,
    mut present: F,
) -> Result<CertificateMaterial, Error>
where
    F: FnMut(CertificateChallenge) -> Fut,
    Fut: Future<Output = Result<(), Error>>,
{
    let account = account(directory, account_dir).await?;
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
    let private_key = order.finalize().await?;
    let certificate = order.poll_certificate(&RetryPolicy::default()).await?;
    CertificateMaterial::new(certificate, private_key).ok_or(Error::MissingMaterial)
}

async fn account(directory: &str, account_dir: &Path) -> Result<Account, Error> {
    let http: Box<dyn HttpClient> = Box::new(ReqwestAcmeClient::new()?);
    let path = account_dir.join(ACCOUNT_FILE);
    if path.exists() {
        let credentials: AccountCredentials = serde_json::from_slice(&std::fs::read(&path)?)?;
        return Ok(Account::builder_with_http(http)
            .from_credentials(credentials)
            .await?);
    }
    std::fs::create_dir_all(account_dir)?;
    let (account, credentials) = Account::builder_with_http(http)
        .create(
            &NewAccount {
                contact: &[],
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            directory.to_owned(),
            None,
        )
        .await?;
    atomic_write(&path, &serde_json::to_vec(&credentials)?, 0o600)?;
    set_ployz_group(&path)?;
    Ok(account)
}

async fn wait_for_http01(
    hostname: &IngressHost,
    challenge: &CertificateChallenge,
) -> Result<(), Error> {
    let client = Client::builder()
        .redirect(Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?;
    let url = format!(
        "http://127.0.0.1/.well-known/acme-challenge/{}",
        challenge.token()
    );
    let deadline = tokio::time::Instant::now() + CHALLENGE_WAIT;
    loop {
        if let Ok(response) = client
            .get(&url)
            .header(reqwest::header::HOST, hostname.as_str())
            .send()
            .await
            && response.status().is_success()
            && response.text().await.ok().as_deref() == Some(challenge.response())
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::ChallengeNotServed);
        }
        tokio::time::sleep(CHALLENGE_POLL).await;
    }
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
    use std::collections::{BTreeMap, BTreeSet};

    use ployz_core::{
        ContainerAddress, ContainerId, ContainerKind, ContainerObservation,
        ContainerRuntimeObservation, HealthObservation, HttpProtocol, IngressHost, IngressHostname,
        MachineId, PortPublication, ResolvedServiceSpec, ServiceId, ServiceName,
    };
    use serde_json::json;

    use super::{directory_from_env, order_certificate, wanted_certificate_hosts};

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
        let material =
            order_certificate(&hostname, &ca.directory_url(), &account_dir, |challenge| {
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
    async fn order_fails_when_the_directory_is_unreachable() {
        let hostname = host("app.example.com");
        let account_dir = std::env::temp_dir().join(format!(
            "ployzd-acme-unreachable-{}-{}",
            std::process::id(),
            hostname
        ));
        let error = order_certificate(
            &hostname,
            "http://127.0.0.1:1/directory",
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
