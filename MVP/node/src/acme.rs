use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mvp_acme::{
    AccountAcquisition, AcmeAccountCoordinator, AcmeAccountHold, AcmeAccountRecord,
    AcmeAccountStore, AcmeCertificateActivatedFact, AcmeChallengeToken, AcmeHostname,
    AcmeHttp01ChallengeReadiness, AcmeIssuanceError, AcmeIssuer, AcmeIssuerConfig,
    InstantAcmeIssuer,
};
use mvp_acme_command::{PandaAcmeCommandAdapter, PandaAcmeHttp01Publisher};
use mvp_bus::{FactKey, FactKeyPattern, FactPayload, Grant, local::LocalBus};
use mvp_identity::{NodeId, VisibleNodes};
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactStore, PandaSqliteOpenConfig, PandaTrustedAuthorKey,
};
use mvp_projection::{
    FactSource, ProjectionFactPayload, SqliteProjectionStore, reduce_facts,
    write_projection_snapshots,
};
use reqwest::header::HOST;
use serde::Serialize;
use tempfile::NamedTempFile;

use crate::error::{NodeError, NodeResult};
use crate::membership::now_ms;
use crate::serving::{ServingRoleRequest, ServingRoleResponse, ServingRoleSuccess};
use crate::state::{LoadedNodeState, load_node};

const HTTP01_READY_TIMEOUT: Duration = Duration::from_secs(15);
const HTTP01_READY_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcmeIssueOptions {
    pub state_dir: PathBuf,
    pub hostname: String,
    pub gateway_url: String,
    pub gateway_control_socket: Option<PathBuf>,
    pub issuer_holder: Option<String>,
    pub account_path: Option<PathBuf>,
}

impl AcmeIssueOptions {
    #[must_use]
    pub fn new(
        state_dir: impl Into<PathBuf>,
        hostname: impl Into<String>,
        gateway_url: impl Into<String>,
    ) -> Self {
        Self {
            state_dir: state_dir.into(),
            hostname: hostname.into(),
            gateway_url: gateway_url.into(),
            gateway_control_socket: None,
            issuer_holder: None,
            account_path: None,
        }
    }

    #[must_use]
    pub fn with_gateway_control_socket(mut self, control_socket: impl Into<PathBuf>) -> Self {
        self.gateway_control_socket = Some(control_socket.into());
        self
    }

    #[must_use]
    pub fn with_issuer_holder(mut self, issuer_holder: impl Into<String>) -> Self {
        self.issuer_holder = Some(issuer_holder.into());
        self
    }

    #[must_use]
    pub fn with_account_path(mut self, account_path: impl Into<PathBuf>) -> Self {
        self.account_path = Some(account_path.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcmeIssueReport {
    pub hostname: String,
    pub order_url: String,
    pub issued_at_secs: u64,
    pub not_before_secs: Option<u64>,
    pub not_after_secs: Option<u64>,
    pub visible_nodes: usize,
    pub account_path: PathBuf,
    pub projected_certificates: usize,
}

pub async fn issue_product_certificate(options: AcmeIssueOptions) -> NodeResult<AcmeIssueReport> {
    issue_product_certificate_with_config(options, AcmeIssuerConfig::from_env()).await
}

pub async fn issue_product_certificate_with_config(
    options: AcmeIssueOptions,
    config: AcmeIssuerConfig,
) -> NodeResult<AcmeIssueReport> {
    let account_path = options
        .account_path
        .clone()
        .unwrap_or_else(|| options.state_dir.join("acme-accounts.json"));
    let issuer = InstantAcmeIssuer::with_account_coordinator(
        config,
        Box::new(FileAcmeAccountCoordinator::new(
            account_path.with_extension("lock"),
        )),
    );
    let readiness = GatewayHttp01Readiness::new(options.gateway_url.clone());
    issue_product_certificate_with_issuer(options, &issuer, &readiness).await
}

pub async fn issue_product_certificate_with_issuer(
    options: AcmeIssueOptions,
    issuer: &dyn AcmeIssuer,
    readiness: &dyn AcmeHttp01ChallengeReadiness,
) -> NodeResult<AcmeIssueReport> {
    let state = load_node(&options.state_dir)?;
    if let Some(holder) = &options.issuer_holder {
        if holder != state.principal_id() {
            return Err(NodeError::NodeAgentRpc {
                message: format!(
                    "issuer holder '{holder}' does not match local principal '{}'",
                    state.principal_id()
                ),
            });
        }
    }
    let hostname = AcmeHostname::parse(options.hostname).map_err(|source| NodeError::Acme {
        source: AcmeIssuanceError::Operation {
            operation: "acme_hostname",
            message: source.to_string(),
        },
    })?;
    let account_path = options
        .account_path
        .clone()
        .unwrap_or_else(|| state.paths().state_dir.join("acme-accounts.json"));
    let mut account_store = JsonAcmeAccountStore::new(account_path.clone());
    let (_bus, authority, raw_bus) = mvp_bus::local::actor_with_authority();
    let session = authority.grant_in(state.island(), state.principal(), Grant::allow_all());
    let author = state.author()?;
    let certificate_author = state.author()?;
    let visible_nodes = visible_nodes(&state);
    let mut store = open_local_fact_store(&state, Arc::new(raw_bus)).await?;
    let mut adapter = PandaAcmeCommandAdapter::new(session.clone(), author, visible_nodes.clone());
    let started = {
        let mut publisher = PandaAcmeHttp01Publisher::new(&mut adapter, &mut store);
        issuer
            .start_order(
                &mut account_store,
                &mut publisher,
                hostname.clone(),
                now_secs(),
            )
            .await
            .map_err(|source| NodeError::Acme { source })?
    };
    project_store(&store, &state, &session)?;
    if let Some(control_socket) = &options.gateway_control_socket {
        reload_gateway(control_socket)?;
    }
    let certificate = {
        let mut publisher = PandaAcmeHttp01Publisher::new(&mut adapter, &mut store);
        issuer
            .finalize_order(
                &mut account_store,
                readiness,
                &mut publisher,
                hostname,
                started.order_url,
                now_secs(),
            )
            .await
            .map_err(|source| NodeError::Acme { source })?
    };
    let report = AcmeIssueReport {
        hostname: certificate.hostname.to_string(),
        order_url: certificate.order_url.to_string(),
        issued_at_secs: certificate.issued_at_secs,
        not_before_secs: certificate.not_before_secs,
        not_after_secs: certificate.not_after_secs,
        visible_nodes: visible_nodes.len(),
        account_path,
        projected_certificates: 0,
    };
    write_certificate_activation(&mut store, &session, &certificate_author, certificate).await?;
    let projected_certificates = project_store(&store, &state, &session)?;
    Ok(AcmeIssueReport {
        projected_certificates,
        ..report
    })
}

fn reload_gateway(control_socket: &Path) -> NodeResult<()> {
    match control_request(control_socket, &ServingRoleRequest::Reload)? {
        ServingRoleResponse::Success(ServingRoleSuccess::Reloaded(_)) => Ok(()),
        response => Err(NodeError::NodeAgentRpc {
            message: format!("gateway reload before ACME validation failed: {response:?}"),
        }),
    }
}

fn control_request(
    control_socket: &Path,
    request: &ServingRoleRequest,
) -> NodeResult<ServingRoleResponse> {
    let bytes = serde_json::to_vec(request)
        .map_err(|source| NodeError::EncodeServingRoleResponse { source })?;
    let mut stream = std::os::unix::net::UnixStream::connect(control_socket).map_err(|source| {
        NodeError::ServingControlSocket {
            path: control_socket.to_path_buf(),
            operation: "connect",
            source,
        }
    })?;
    stream
        .write_all(&bytes)
        .map_err(|source| NodeError::ServingControlSocket {
            path: control_socket.to_path_buf(),
            operation: "write reload request",
            source,
        })?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|source| NodeError::ServingControlSocket {
            path: control_socket.to_path_buf(),
            operation: "finish reload request",
            source,
        })?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|source| NodeError::ServingControlSocket {
            path: control_socket.to_path_buf(),
            operation: "read reload response",
            source,
        })?;
    serde_json::from_slice(&response).map_err(|source| NodeError::NodeAgentRpc {
        message: format!("decode gateway reload response: {source}"),
    })
}

struct GatewayHttp01Readiness {
    gateway_url: String,
    client: reqwest::Client,
}

impl GatewayHttp01Readiness {
    fn new(gateway_url: impl Into<String>) -> Self {
        Self {
            gateway_url: gateway_url.into(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl AcmeHttp01ChallengeReadiness for GatewayHttp01Readiness {
    async fn wait_ready(
        &self,
        hostname: &AcmeHostname,
        token: &AcmeChallengeToken,
    ) -> Result<(), AcmeIssuanceError> {
        let url = format!(
            "{}/.well-known/acme-challenge/{}",
            self.gateway_url.trim_end_matches('/'),
            token
        );
        let deadline = tokio::time::Instant::now() + HTTP01_READY_TIMEOUT;
        loop {
            match self
                .client
                .get(&url)
                .header(HOST, hostname.as_str())
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(_) | Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(HTTP01_READY_INTERVAL).await;
                }
                Ok(_) | Err(_) => {
                    return Err(AcmeIssuanceError::Http01ChallengeNotVisible {
                        hostname: hostname.clone(),
                        token: token.clone(),
                    });
                }
            }
        }
    }
}

struct JsonAcmeAccountStore {
    path: PathBuf,
}

impl JsonAcmeAccountStore {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn load_all(
        &self,
        directory_url: &str,
    ) -> Result<BTreeMap<String, AcmeAccountRecord>, AcmeIssuanceError> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| {
                AcmeIssuanceError::InvalidAccountCredentials {
                    directory_url: directory_url.to_string(),
                    message: source.to_string(),
                }
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(AcmeIssuanceError::Operation {
                operation: "acme_account_read",
                message: error.to_string(),
            }),
        }
    }

    fn save_all(
        &self,
        directory_url: &str,
        records: &BTreeMap<String, AcmeAccountRecord>,
    ) -> Result<(), AcmeIssuanceError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| AcmeIssuanceError::Operation {
                operation: "acme_account_create_dir",
                message: source.to_string(),
            })?;
        }
        let parent = self
            .path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let mut file =
            NamedTempFile::new_in(parent).map_err(|source| AcmeIssuanceError::Operation {
                operation: "acme_account_tempfile",
                message: source.to_string(),
            })?;
        serde_json::to_writer_pretty(&mut file, records).map_err(|source| {
            AcmeIssuanceError::InvalidAccountCredentials {
                directory_url: directory_url.to_string(),
                message: source.to_string(),
            }
        })?;
        file.persist(&self.path)
            .map_err(|source| AcmeIssuanceError::Operation {
                operation: "acme_account_persist",
                message: source.to_string(),
            })?;
        Ok(())
    }
}

#[async_trait]
impl AcmeAccountStore for JsonAcmeAccountStore {
    async fn load_account(
        &self,
        directory_url: &str,
    ) -> Result<Option<AcmeAccountRecord>, AcmeIssuanceError> {
        Ok(self.load_all(directory_url)?.remove(directory_url))
    }

    async fn save_account(&mut self, record: AcmeAccountRecord) -> Result<(), AcmeIssuanceError> {
        let directory_url = record.directory_url.clone();
        let mut records = self.load_all(&record.directory_url)?;
        records.insert(directory_url.clone(), record);
        self.save_all(&directory_url, &records)?;
        Ok(())
    }
}

struct FileAcmeAccountCoordinator {
    lock_path: PathBuf,
}

impl FileAcmeAccountCoordinator {
    fn new(lock_path: PathBuf) -> Self {
        Self { lock_path }
    }
}

#[async_trait]
impl AcmeAccountCoordinator for FileAcmeAccountCoordinator {
    async fn try_acquire_account(&self, directory_url: &str) -> AccountAcquisition {
        match AccountFileLock::acquire(self.lock_path.clone(), directory_url) {
            Ok(lock) => AccountAcquisition::Allowed(AcmeAccountHold::new(move || async move {
                drop(lock);
            })),
            Err(AcmeIssuanceError::AccountLockFailed { message, .. }) => {
                AccountAcquisition::CoordinationFailed(message)
            }
            Err(error) => AccountAcquisition::CoordinationFailed(error.to_string()),
        }
    }
}

struct AccountFileLock {
    path: PathBuf,
}

impl AccountFileLock {
    fn acquire(path: PathBuf, directory_url: &str) -> Result<Self, AcmeIssuanceError> {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => Ok(Self { path }),
            Err(error) => Err(AcmeIssuanceError::AccountLockFailed {
                directory_url: directory_url.to_string(),
                message: error.to_string(),
            }),
        }
    }
}

impl Drop for AccountFileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

async fn open_local_fact_store(
    state: &LoadedNodeState,
    raw_bus: Arc<LocalBus>,
) -> NodeResult<PandaFactStore> {
    let author = state.author()?;
    let mut store_config =
        PandaSqliteOpenConfig::new(state.paths().fact_store.clone(), vec![state.island()])
            .with_trusted_author_key(PandaTrustedAuthorKey::new(
                state.island(),
                state.principal(),
                author.author_key(),
            ));
    for trusted in state.trusted_fact_authors()? {
        store_config = store_config.with_trusted_author_key(PandaTrustedAuthorKey::new(
            state.island(),
            trusted.principal().clone(),
            trusted.author_key(),
        ));
    }
    PandaFactStore::open_sqlite(raw_bus, store_config)
        .await
        .map_err(|source| NodeError::FactStore { source })
}

async fn write_certificate_activation(
    store: &mut PandaFactStore,
    session: &mvp_bus::BusSession,
    author: &PandaFactAuthor,
    certificate: mvp_acme::IssuedCertificate,
) -> NodeResult<()> {
    let fact = AcmeCertificateActivatedFact::from_issued(certificate);
    let key = FactKey::parse(fact.fact_key()).map_err(|source| NodeError::Bus { source })?;
    let payload = ProjectionFactPayload::AcmeCertificateActivated(fact)
        .to_fact_bytes()
        .map_err(|source| NodeError::EncodeNodeAgentRpc { source })?;
    store
        .write_fact_payload(session, author, key, FactPayload::copy_from_slice(&payload))
        .await
        .map_err(|source| NodeError::FactStore { source })?;
    Ok(())
}

fn project_store(
    store: &PandaFactStore,
    state: &LoadedNodeState,
    session: &mvp_bus::BusSession,
) -> NodeResult<usize> {
    let pattern = FactKeyPattern::parse("/facts/>").map_err(|source| NodeError::Bus { source })?;
    let candidates = store
        .list_candidates(&state.island(), &pattern, session)
        .map_err(|source| NodeError::FactSource { source })?;
    let payloads = store
        .read_payloads(&state.island(), &candidates, session)
        .map_err(|source| NodeError::FactSource { source })?;
    let projection = reduce_facts(&state.island(), &candidates, &payloads);
    SqliteProjectionStore::new(state.paths().projection_db.clone())
        .rebuild(&projection)
        .map_err(|source| NodeError::Projection { source })?;
    write_projection_snapshots(
        &projection,
        state.paths().gateway_snapshot.clone(),
        state.paths().dns_snapshot.clone(),
    )
    .map_err(|source| NodeError::Projection { source })?;
    Ok(projection.certificates.len())
}

fn visible_nodes(state: &LoadedNodeState) -> VisibleNodes {
    let mut nodes = vec![state.node_id()];
    nodes.extend(
        state
            .admitted_peers()
            .into_iter()
            .filter_map(|peer| peer.node_id.map(NodeId::new)),
    );
    VisibleNodes::new(nodes)
}

fn now_secs() -> u64 {
    now_ms() / 1_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InitOptions, init_node};
    use mvp_acme::{
        AcmeHttp01ChallengePublisher, AcmeHttp01OrderChallenge, AcmeKeyAuthorization, AcmeOrderUrl,
        IssuedCertificate, StartedAcmeOrder,
    };
    use mvp_projection::load_gateway_snapshot;

    struct FakeIssuer;

    #[async_trait]
    impl AcmeIssuer for FakeIssuer {
        async fn start_order(
            &self,
            account_store: &mut dyn AcmeAccountStore,
            challenge_publisher: &mut dyn AcmeHttp01ChallengePublisher,
            hostname: AcmeHostname,
            now_secs: u64,
        ) -> Result<StartedAcmeOrder, AcmeIssuanceError> {
            let token = AcmeChallengeToken::parse("tokAcme0123456789abcdef").expect("token");
            if account_store
                .load_account("https://acme.test/dir")
                .await?
                .is_none()
            {
                account_store
                    .save_account(AcmeAccountRecord {
                        account_id: mvp_acme::AcmeAccountId::for_directory_url(
                            "https://acme.test/dir",
                        ),
                        directory_url: "https://acme.test/dir".to_string(),
                        contact_email: None,
                        account_credentials_json: r#"{"fake":true}"#.to_string(),
                        created_at_secs: now_secs,
                        updated_at_secs: now_secs,
                    })
                    .await?;
            }
            let challenge = AcmeHttp01OrderChallenge {
                hostname: hostname.clone(),
                token,
                key_authorization: AcmeKeyAuthorization::parse_for_token(
                    &AcmeChallengeToken::parse("tokAcme0123456789abcdef").expect("token"),
                    "tokAcme0123456789abcdef.thumbprint",
                )
                .expect("key authorization"),
                expires_at_secs: now_secs + 300,
            };
            challenge_publisher
                .publish_http01(challenge.clone())
                .await?;
            Ok(StartedAcmeOrder {
                hostname,
                order_url: AcmeOrderUrl::parse("https://acme.test/order/1").expect("order"),
                challenges: vec![challenge],
            })
        }

        async fn finalize_order(
            &self,
            _account_store: &mut dyn AcmeAccountStore,
            readiness: &dyn AcmeHttp01ChallengeReadiness,
            challenge_publisher: &mut dyn AcmeHttp01ChallengePublisher,
            hostname: AcmeHostname,
            order_url: AcmeOrderUrl,
            now_secs: u64,
        ) -> Result<IssuedCertificate, AcmeIssuanceError> {
            let token = AcmeChallengeToken::parse("tokAcme0123456789abcdef").expect("token");
            readiness.wait_ready(&hostname, &token).await?;
            challenge_publisher
                .clear_http01(&hostname, &token, now_secs)
                .await?;
            Ok(IssuedCertificate {
                hostname,
                order_url,
                fullchain_pem: "fake-chain".to_string(),
                private_key_pem: "fake-key".to_string(),
                issued_at_secs: now_secs,
                not_before_secs: Some(now_secs),
                not_after_secs: Some(now_secs + 60),
            })
        }
    }

    struct AlwaysReady;

    #[async_trait]
    impl AcmeHttp01ChallengeReadiness for AlwaysReady {
        async fn wait_ready(
            &self,
            _hostname: &AcmeHostname,
            _token: &AcmeChallengeToken,
        ) -> Result<(), AcmeIssuanceError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn issue_writes_certificate_activation_into_serving_snapshot() {
        let temp = tempfile::tempdir().expect("tempdir");
        init_node(
            InitOptions::new(temp.path())
                .with_island("prod")
                .with_node_id("node-a"),
        )
        .expect("init");
        let report = issue_product_certificate_with_issuer(
            AcmeIssueOptions::new(temp.path(), "web.example.test", "http://127.0.0.1:1"),
            &FakeIssuer,
            &AlwaysReady,
        )
        .await
        .expect("issue");

        assert_eq!(report.hostname, "web.example.test");
        assert_eq!(report.projected_certificates, 1);
        let snapshot = load_gateway_snapshot(
            temp.path().join("gateway.snapshot"),
            &mvp_bus::IslandId::new("prod"),
        )
        .expect("load gateway snapshot");
        let certificate = snapshot
            .certificates
            .iter()
            .find(|certificate| certificate.hostname.as_str() == "web.example.test")
            .expect("certificate projected");
        assert_eq!(certificate.fullchain_pem, "fake-chain");
        assert!(snapshot.acme_http01.is_empty());
    }
}
