//! Bootstrap-only admission of machine one's initial authority rows.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use hyper::header::{AUTHORIZATION, HeaderValue};
use hyper::{Response, StatusCode};
use ployz_core::ApiRefusal;
use ployz_core::corrosion::{
    CorrosionTable, MachineTransport, SqliteParameter, Statement, StoredRow,
};
use ployz_core::founding::{
    FoundingRefusal, FoundingRequest, FoundingResult, FoundingValidationError,
    ValidatedFoundingRequest,
};
use ployz_core::network::MachineEndpointSubnet;
use serde::Serialize;

use crate::WireGuardMtuPolicy;
use crate::corrosion::{
    CorrosionClient, CorrosionClientError, StoredRowCollectionError, StoredRowLimit,
    collect_stored_rows,
};
use crate::roles::api::execution::docker::runner::DockerManagedContainerRunner;
use crate::roles::api::runner::MachineEndpointNetworkError;

use super::config::{ApiRoleMode, BootstrapSecret};
use super::roster::corrosion_unavailable_refusal;
use super::server::{
    ApiService, HttpBody, corrosion_unavailable_response, json_response, refusal_response,
};
use super::server::{BoundedBodyError, read_bounded_body};

const MAX_FOUNDING_ROWS_PER_TABLE: usize = 1;
const MAX_FOUNDING_REQUEST_BYTES: usize = 1024 * 1024;
const FOUNDING_BODY_READ_TIMEOUT: Duration = Duration::from_secs(10);
const ENDPOINT_NETWORK_TIMEOUT: Duration = Duration::from_secs(30);
const ENDPOINT_BRIDGE_IFNAME: &str = "br-ployz";
const WIREGUARD_IFNAME: &str = "ployz0";

pub(super) async fn handle_founding(
    service: &ApiService,
    peer: SocketAddr,
    request: hyper::Request<hyper::body::Incoming>,
) -> Response<HttpBody> {
    if let Err(refusal) = authorize_founding(
        &service.mode,
        service.listen_addr,
        peer,
        request.headers().get(AUTHORIZATION),
    ) {
        return refusal_response(refusal);
    }
    let body = match read_bounded_founding_body(request.into_body()).await {
        Ok(body) => body,
        Err(FoundingBodyError::TooLarge) => {
            return founding_http_error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
        }
        Err(FoundingBodyError::Deadline { .. }) => {
            return founding_http_error(StatusCode::REQUEST_TIMEOUT, "request_timeout");
        }
        Err(FoundingBodyError::Read) => {
            return founding_http_error(StatusCode::BAD_REQUEST, "invalid_request");
        }
    };
    let request = match serde_json::from_slice::<FoundingRequest>(&body) {
        Ok(request) => request,
        Err(_) => return founding_http_error(StatusCode::BAD_REQUEST, "invalid_request"),
    };
    if request.cluster_id != service.cluster_id || request.machine_id != service.local_machine_id {
        return refusal_response(ApiRefusal::InvalidCluster);
    }
    let MachineTransport::Wireguard { addr_v6, .. } = &request.machine.transport else {
        return founding_refusal_response(FoundingRefusal::from(
            FoundingValidationError::MachineTransportProviderMismatch,
        ));
    };
    if service.listen_addr.ip() != IpAddr::V6(*addr_v6) {
        return refusal_response(ApiRefusal::InvalidCluster);
    }
    let validated = match request.try_validate() {
        Ok(validated) => validated,
        Err(reason) => {
            return founding_refusal_response(FoundingRefusal::InvalidRequest { reason });
        }
    };

    let _guard = service.founding_lock.lock().await;
    match complete_founding(service, &validated).await {
        Ok(result) => founding_result_response(result),
        Err(refusal) => refusal_response(refusal),
    }
}

async fn complete_founding(
    service: &ApiService,
    validated: &ValidatedFoundingRequest,
) -> Result<FoundingResult, ApiRefusal> {
    let write = match write_initial_rows(&service.corrosion, validated).await {
        Ok(write) => write,
        Err(error) if error.is_state_mismatch() => {
            tracing::warn!(error = %error, "founding rows did not match persisted state");
            return Err(ApiRefusal::InvalidCluster);
        }
        Err(error) => {
            tracing::warn!(error = %error, "could not commit founding rows");
            return Err(corrosion_unavailable_refusal());
        }
    };
    ensure_endpoint_network(&write.machine_subnet)
        .await
        .map_err(|error| {
            tracing::warn!(error = %error, "could not fold founding rows into the endpoint network");
            corrosion_unavailable_refusal()
        })?;
    if !service.start_founding_lenses_and_observe_machine().await {
        tracing::warn!("machines lens did not observe machine one after founding");
        return Err(corrosion_unavailable_refusal());
    }
    Ok(write.result)
}

fn bootstrap_authorized(authorization: Option<&HeaderValue>, secret: &BootstrapSecret) -> bool {
    let Some(authorization) = authorization.and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Some(candidate) = authorization.strip_prefix("Bearer ") else {
        return false;
    };
    !candidate.is_empty() && secret.verifies(candidate.as_bytes())
}

pub(super) fn authorize_founding(
    mode: &ApiRoleMode,
    listen_addr: SocketAddr,
    peer: SocketAddr,
    authorization: Option<&HeaderValue>,
) -> Result<(), ApiRefusal> {
    let ApiRoleMode::Founding(secret) = mode else {
        return Err(ApiRefusal::UnsupportedRoute);
    };
    if peer.ip() != listen_addr.ip() || !bootstrap_authorized(authorization, secret) {
        return Err(ApiRefusal::UnknownSource { source: peer.ip() });
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum FoundingBodyError {
    TooLarge,
    Deadline { timeout: Duration },
    Read,
}

async fn read_bounded_founding_body<Payload>(body: Payload) -> Result<Vec<u8>, FoundingBodyError>
where
    Payload: hyper::body::Body<Data = bytes::Bytes> + Unpin,
{
    read_bounded_body(body, MAX_FOUNDING_REQUEST_BYTES, FOUNDING_BODY_READ_TIMEOUT)
        .await
        .map_err(|error| match error {
            BoundedBodyError::TooLarge => FoundingBodyError::TooLarge,
            BoundedBodyError::Deadline => FoundingBodyError::Deadline {
                timeout: FOUNDING_BODY_READ_TIMEOUT,
            },
            BoundedBodyError::Read => FoundingBodyError::Read,
        })
}

#[cfg(test)]
pub(super) fn append_founding_body_chunk(
    body: &mut Vec<u8>,
    chunk: &[u8],
) -> Result<(), FoundingBodyError> {
    super::server::append_bounded_body_chunk(body, chunk, MAX_FOUNDING_REQUEST_BYTES).map_err(
        |error| match error {
            BoundedBodyError::TooLarge => FoundingBodyError::TooLarge,
            BoundedBodyError::Deadline => FoundingBodyError::Deadline {
                timeout: FOUNDING_BODY_READ_TIMEOUT,
            },
            BoundedBodyError::Read => FoundingBodyError::Read,
        },
    )
}

fn founding_http_error(status: StatusCode, kind: &'static str) -> Response<HttpBody> {
    json_response(status, format!("{{\"kind\":\"{kind}\"}}").into_bytes())
}

fn founding_refusal_response(refusal: FoundingRefusal) -> Response<HttpBody> {
    match serde_json::to_vec(&refusal) {
        Ok(body) => json_response(StatusCode::BAD_REQUEST, body),
        Err(error) => {
            tracing::error!(error = %error, "could not encode founding refusal");
            corrosion_unavailable_response()
        }
    }
}

fn founding_result_response(result: FoundingResult) -> Response<HttpBody> {
    match serde_json::to_vec(&result) {
        Ok(body) => json_response(StatusCode::OK, body),
        Err(error) => {
            tracing::error!(error = %error, "could not encode founding result");
            corrosion_unavailable_response()
        }
    }
}

pub(super) struct FoundingWrite {
    pub(super) result: FoundingResult,
    pub(super) machine_subnet: MachineEndpointSubnet,
}

pub(super) async fn write_initial_rows(
    corrosion: &CorrosionClient,
    validated: &ValidatedFoundingRequest,
) -> Result<FoundingWrite, FoundingWriteError> {
    let request = validated.request();
    let before = read_initial_rows(corrosion).await?;
    let plan = FoundingPlan::build(validated, &before)?;
    let result = plan.result;
    if !plan.statements.is_empty() {
        corrosion.execute(&plan.statements).await?;
    }

    let after = read_initial_rows(corrosion).await?;
    FoundingPlan::verify_complete(validated, &after)?;
    let MachineTransport::Wireguard { subnet_v4, .. } = &request.machine.transport else {
        unreachable!("validated founding requests use builtin WireGuard")
    };
    Ok(FoundingWrite {
        result,
        machine_subnet: subnet_v4.clone(),
    })
}

pub(super) async fn ensure_endpoint_network(
    subnet: &MachineEndpointSubnet,
) -> Result<(), FoundingEndpointNetworkError> {
    let runner = DockerManagedContainerRunner::lazy_local_defaults(
        subnet.as_string(),
        ENDPOINT_BRIDGE_IFNAME.to_owned(),
        WIREGUARD_IFNAME.to_owned(),
        WireGuardMtuPolicy::Auto,
    );
    tokio::time::timeout(
        ENDPOINT_NETWORK_TIMEOUT,
        runner.ensure_projection_endpoint_network(subnet),
    )
    .await
    .map_err(|_| FoundingEndpointNetworkError::Timeout {
        timeout: ENDPOINT_NETWORK_TIMEOUT,
    })?
    .map_err(FoundingEndpointNetworkError::Ensure)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FoundingRows {
    cluster: Vec<StoredRow>,
    machines: Vec<StoredRow>,
    peers: Vec<StoredRow>,
}

async fn read_initial_rows(
    corrosion: &CorrosionClient,
) -> Result<FoundingRows, FoundingWriteError> {
    let cluster = query_table(corrosion, CorrosionTable::Cluster);
    let machines = query_table(corrosion, CorrosionTable::Machines);
    let peers = query_table(corrosion, CorrosionTable::Peers);
    let (cluster, machines, peers) = tokio::try_join!(cluster, machines, peers)?;
    Ok(FoundingRows {
        cluster,
        machines,
        peers,
    })
}

async fn query_table(
    corrosion: &CorrosionClient,
    table: CorrosionTable,
) -> Result<Vec<StoredRow>, FoundingWriteError> {
    let mut stream = corrosion
        .query(&Statement::simple(format!(
            "SELECT id, document FROM {}",
            table.as_str()
        )))
        .await?;
    collect_stored_rows(
        &mut stream,
        StoredRowLimit::new(MAX_FOUNDING_ROWS_PER_TABLE),
    )
    .await
    .map_err(FoundingWriteError::Rows)
}

struct FoundingPlan {
    result: FoundingResult,
    statements: Vec<Statement>,
}

impl FoundingPlan {
    fn build(
        validated: &ValidatedFoundingRequest,
        existing: &FoundingRows,
    ) -> Result<Self, FoundingWriteError> {
        let expected = ExpectedRows::new(validated)?;
        let cluster = expected.disposition(&existing.cluster, &expected.cluster)?;
        let machine = expected.disposition(&existing.machines, &expected.machine)?;
        let peer = match &expected.peer {
            Some(peer) => Some(expected.disposition(&existing.peers, peer)?),
            None if existing.peers.is_empty() => None,
            None => return Err(FoundingWriteError::ExistingRowsMismatch),
        };
        let existing_count = usize::from(cluster == RowDisposition::Exact)
            + usize::from(machine == RowDisposition::Exact)
            + usize::from(peer == Some(RowDisposition::Exact));
        let expected_count = 2 + usize::from(expected.peer.is_some());
        let result = if existing_count == 0 {
            FoundingResult::Found
        } else if existing_count == expected_count {
            FoundingResult::NoOp
        } else {
            FoundingResult::Resumed
        };
        let mut statements = Vec::with_capacity(expected_count - existing_count);
        if cluster == RowDisposition::Missing {
            statements.push(expected.cluster.insert_statement());
        }
        if machine == RowDisposition::Missing {
            statements.push(expected.machine.insert_statement());
        }
        if peer == Some(RowDisposition::Missing) {
            let Some(peer) = &expected.peer else {
                unreachable!("a missing peer disposition has an expected peer row")
            };
            statements.push(peer.insert_statement());
        }
        Ok(Self { result, statements })
    }

    fn verify_complete(
        validated: &ValidatedFoundingRequest,
        existing: &FoundingRows,
    ) -> Result<(), FoundingWriteError> {
        let plan = Self::build(validated, existing)?;
        if plan.result == FoundingResult::NoOp {
            Ok(())
        } else {
            Err(FoundingWriteError::ReadbackIncomplete)
        }
    }
}

struct ExpectedRows {
    cluster: ExpectedRow,
    machine: ExpectedRow,
    peer: Option<ExpectedRow>,
}

impl ExpectedRows {
    fn new(validated: &ValidatedFoundingRequest) -> Result<Self, FoundingWriteError> {
        let request = validated.request();
        let peer = request
            .driver
            .enrolled_peer()
            .map(|(id, document)| ExpectedRow::new(CorrosionTable::Peers, id.as_str(), document));
        Ok(Self {
            cluster: ExpectedRow::new(
                CorrosionTable::Cluster,
                request.cluster_id.as_str(),
                &request.cluster,
            )?,
            machine: ExpectedRow::new(
                CorrosionTable::Machines,
                request.machine_id.as_str(),
                &request.machine,
            )?,
            peer: peer.transpose()?,
        })
    }

    fn disposition(
        &self,
        stored: &[StoredRow],
        expected: &ExpectedRow,
    ) -> Result<RowDisposition, FoundingWriteError> {
        match stored {
            [] => Ok(RowDisposition::Missing),
            [row] if row.key == expected.key => {
                let value = serde_json::from_str::<serde_json::Value>(&row.document)
                    .map_err(|_| FoundingWriteError::ExistingRowsMismatch)?;
                if value == expected.document {
                    Ok(RowDisposition::Exact)
                } else {
                    Err(FoundingWriteError::ExistingRowsMismatch)
                }
            }
            [_] | [_, ..] => Err(FoundingWriteError::ExistingRowsMismatch),
        }
    }
}

struct ExpectedRow {
    table: CorrosionTable,
    key: String,
    document: serde_json::Value,
    encoded: String,
}

impl ExpectedRow {
    fn new(
        table: CorrosionTable,
        key: &str,
        document: &impl Serialize,
    ) -> Result<Self, FoundingWriteError> {
        let document = serde_json::to_value(document).map_err(FoundingWriteError::Encode)?;
        let encoded = serde_json::to_string(&document).map_err(FoundingWriteError::Encode)?;
        Ok(Self {
            table,
            key: key.to_owned(),
            document,
            encoded,
        })
    }

    fn insert_statement(&self) -> Statement {
        Statement::with_params(
            format!(
                "INSERT INTO {} (id, document) VALUES (?, ?)",
                self.table.as_str()
            ),
            vec![
                SqliteParameter::Text(self.key.clone()),
                SqliteParameter::Text(self.encoded.clone()),
            ],
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowDisposition {
    Missing,
    Exact,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum FoundingWriteError {
    #[error("could not encode a validated founding row: {0}")]
    Encode(serde_json::Error),
    #[error("local Corrosion request failed: {0}")]
    Client(#[from] CorrosionClientError),
    #[error("local Corrosion founding query was malformed: {0}")]
    Rows(StoredRowCollectionError),
    #[error("existing Corrosion rows do not exactly match this founding request")]
    ExistingRowsMismatch,
    #[error("the founding transaction did not become visible on readback")]
    ReadbackIncomplete,
}

impl FoundingWriteError {
    pub(super) const fn is_state_mismatch(&self) -> bool {
        matches!(
            self,
            Self::ExistingRowsMismatch
                | Self::ReadbackIncomplete
                | Self::Rows(StoredRowCollectionError::RowLimit { .. })
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum FoundingEndpointNetworkError {
    #[error("endpoint network fold exceeded its {timeout:?} deadline")]
    Timeout { timeout: Duration },
    #[error("endpoint network fold failed: {0:?}")]
    Ensure(MachineEndpointNetworkError),
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use bytes::Bytes;
    use futures_util::stream;
    use http_body_util::StreamBody;
    use hyper::body::Frame;
    use ployz_core::corrosion::{
        AutomaticHostnameMode, ClusterDocument, CorrosionDocumentVersion, CorrosionTimestamp,
        MachineDocument, MachineStorageSelection, MachineStorageSelectionReason, MachineTransport,
        MeshProvider, OperationInitiator, OperatorWriteProvenance, PeerDocument, PeerTransport,
        StorageMode, derive_builtin_wireguard_member,
    };
    use ployz_core::founding::{FoundingDriverEnrollment, FoundingRequest};
    use ployz_core::ids::{ClusterId, MachineRowId, PeerId};
    use ployz_core::machine::{MachineLifecycle, MachineName};
    use ployz_core::network::{MachineEndpointSubnet, MachineEndpointSupernet, WireGuardPublicKey};

    use super::*;

    const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const MACHINE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const PEER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
    const MACHINE_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    const PEER_KEY: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";

    fn cluster_id() -> ClusterId {
        ClusterId::try_new(CLUSTER).expect("fixture cluster id")
    }

    fn machine_id() -> MachineRowId {
        MachineRowId::try_new(MACHINE).expect("fixture machine id")
    }

    fn provenance() -> OperatorWriteProvenance {
        OperatorWriteProvenance {
            written_by: OperationInitiator::Machine {
                machine_id: machine_id(),
            },
            written_at: CorrosionTimestamp::try_new("2026-08-04T12:00:00Z")
                .expect("fixture timestamp"),
        }
    }

    fn request(with_peer: bool) -> ValidatedFoundingRequest {
        let cluster_id = cluster_id();
        let machine_key =
            WireGuardPublicKey::try_new(MACHINE_KEY).expect("fixture machine public key");
        let driver = if with_peer {
            let peer_key = WireGuardPublicKey::try_new(PEER_KEY).expect("fixture peer public key");
            FoundingDriverEnrollment::Ssh {
                peer_id: PeerId::try_new(PEER).expect("fixture peer id"),
                document: PeerDocument {
                    v: CorrosionDocumentVersion::V1,
                    cluster_id: cluster_id.clone(),
                    provenance: provenance(),
                    name: "operator-laptop".to_owned(),
                    transport: PeerTransport::Wireguard {
                        addr_v6: derive_builtin_wireguard_member(&cluster_id, &peer_key)
                            .bind_address()
                            .get(),
                        pubkey: peer_key,
                        endpoint: None,
                    },
                },
            }
        } else {
            FoundingDriverEnrollment::OnHost
        };
        FoundingRequest {
            cluster_id: cluster_id.clone(),
            cluster: ClusterDocument {
                v: CorrosionDocumentVersion::V1,
                cluster_id: cluster_id.clone(),
                provenance: provenance(),
                name: "ares".to_owned(),
                storage_default: StorageMode::Plain,
                hostname_mode: AutomaticHostnameMode::Ployz,
                ployz_dns_target: ployz_core::corrosion::PloyzDnsTargetState::Pending,
                prefix: MachineEndpointSupernet::default_v1(),
                provider: MeshProvider::BuiltinWireguard,
                acme_directory_url: "https://acme.example/directory".to_owned(),
                acme_contact: None,
            },
            machine_id: machine_id(),
            machine: MachineDocument {
                v: CorrosionDocumentVersion::V1,
                cluster_id: cluster_id.clone(),
                provenance: provenance(),
                name: MachineName::try_new("ares").expect("fixture machine name"),
                lifecycle: MachineLifecycle::Active,
                transport: MachineTransport::Wireguard {
                    addr_v6: derive_builtin_wireguard_member(&cluster_id, &machine_key)
                        .bind_address()
                        .get(),
                    pubkey: machine_key,
                    endpoint: None,
                    subnet_v4: MachineEndpointSubnet::try_new("10.210.0.0/24")
                        .expect("fixture first subnet"),
                },
                storage: MachineStorageSelection {
                    mode: StorageMode::Plain,
                    reason: MachineStorageSelectionReason::Default,
                },
            },
            driver,
        }
        .try_validate()
        .expect("fixture founding request validates")
    }

    fn stored(expected: &ExpectedRow) -> StoredRow {
        StoredRow::new(expected.key.clone(), expected.encoded.clone())
    }

    #[test]
    fn clean_founding_writes_only_cluster_machine_and_optional_peer() {
        for (with_peer, expected_count) in [(false, 2), (true, 3)] {
            let validated = request(with_peer);
            let rows = FoundingRows {
                cluster: Vec::new(),
                machines: Vec::new(),
                peers: Vec::new(),
            };
            let plan = FoundingPlan::build(&validated, &rows).expect("clean state can found");
            assert_eq!(plan.result, FoundingResult::Found);
            assert_eq!(plan.statements.len(), expected_count);
            let encoded = serde_json::to_string(&plan.statements).expect("statements serialize");
            assert!(!encoded.contains("tokens"));
            assert!(!encoded.contains("namespaces"));
        }
    }

    #[test]
    fn exact_partial_state_resumes_and_exact_complete_state_is_a_no_op() {
        let validated = request(true);
        let expected = ExpectedRows::new(&validated).expect("expected rows encode");
        let partial = FoundingRows {
            cluster: vec![stored(&expected.cluster)],
            machines: Vec::new(),
            peers: Vec::new(),
        };
        let plan = FoundingPlan::build(&validated, &partial).expect("exact partial state resumes");
        assert_eq!(plan.result, FoundingResult::Resumed);
        assert_eq!(plan.statements.len(), 2);
        assert!(matches!(
            FoundingPlan::verify_complete(&validated, &partial),
            Err(FoundingWriteError::ReadbackIncomplete)
        ));

        let complete = FoundingRows {
            cluster: vec![stored(&expected.cluster)],
            machines: vec![stored(&expected.machine)],
            peers: vec![stored(expected.peer.as_ref().expect("peer row"))],
        };
        let plan = FoundingPlan::build(&validated, &complete).expect("exact replay succeeds");
        assert_eq!(plan.result, FoundingResult::NoOp);
        assert!(plan.statements.is_empty());
        FoundingPlan::verify_complete(&validated, &complete)
            .expect("complete readback satisfies the founding barrier");
    }

    #[test]
    fn any_extra_or_mismatched_row_refuses_before_building_a_transaction() {
        let validated = request(false);
        let expected = ExpectedRows::new(&validated).expect("expected rows encode");
        let mismatched = FoundingRows {
            cluster: vec![StoredRow::new(expected.cluster.key.clone(), "{}")],
            machines: Vec::new(),
            peers: Vec::new(),
        };
        assert!(matches!(
            FoundingPlan::build(&validated, &mismatched),
            Err(FoundingWriteError::ExistingRowsMismatch)
        ));

        let unexpected_peer = FoundingRows {
            cluster: Vec::new(),
            machines: Vec::new(),
            peers: vec![StoredRow::new(PEER, "{}")],
        };
        assert!(matches!(
            FoundingPlan::build(&validated, &unexpected_peer),
            Err(FoundingWriteError::ExistingRowsMismatch)
        ));

        let overfull_rows = FoundingRows {
            cluster: vec![
                stored(&expected.cluster),
                StoredRow::new("01ARZ3NDEKTSV4RRFFQ69G5FAZ", "{}"),
            ],
            machines: Vec::new(),
            peers: Vec::new(),
        };
        assert!(matches!(
            FoundingPlan::build(&validated, &overfull_rows),
            Err(FoundingWriteError::ExistingRowsMismatch)
        ));

        let overfull_table = FoundingWriteError::Rows(StoredRowCollectionError::RowLimit {
            limit: MAX_FOUNDING_ROWS_PER_TABLE,
        });
        assert!(overfull_table.is_state_mismatch());
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_request_body_hits_the_whole_body_deadline() {
        let body = StreamBody::new(stream::pending::<Result<Frame<Bytes>, Infallible>>());
        let read = tokio::spawn(read_bounded_founding_body(body));
        tokio::task::yield_now().await;
        tokio::time::advance(FOUNDING_BODY_READ_TIMEOUT).await;

        assert_eq!(
            read.await.expect("body reader joins"),
            Err(FoundingBodyError::Deadline {
                timeout: FOUNDING_BODY_READ_TIMEOUT,
            })
        );
    }

    #[test]
    fn request_body_limit_still_accepts_exactly_one_mebibyte() {
        let mut body = vec![0_u8; MAX_FOUNDING_REQUEST_BYTES - 1];
        append_founding_body_chunk(&mut body, b"x").expect("the exact request bound is accepted");
        assert!(matches!(
            append_founding_body_chunk(&mut body, b"x"),
            Err(FoundingBodyError::TooLarge)
        ));
    }
}
