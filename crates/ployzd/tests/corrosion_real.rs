#![cfg_attr(
    not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )),
    allow(dead_code, unused_imports)
)]

use std::fs::{self, File};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ployz_core::corrosion::{
    AcmeHttp01Document, CertHoldingDocument, ChangeKind, ClusterDocument, ContainerDocument,
    CorrosionDocument, CorrosionTable, MachineDocument, MachineStatusDocument, NamespaceDocument,
    OperationDocument, PeerDocument, RouteBindingDocument, ServiceDocument, SqliteParameter,
    SqliteValue, Statement, TokenDocument, read_named_rows,
};
use ployz_core::ids::{ClusterId, CorrosionUlid, MachineRowId};
use ployz_core::{
    API_MAJOR, ApiFeature, ApiVersion, DoctorDocument, KnownApiFeature, StatusBarrier,
    StatusDocument,
};
use ployzd::corrosion::{
    BearerToken, CorrosionClient, CorrosionClientBounds, CorrosionClientConfig, NameClaimOutcome,
    QueryStreamEvent, SubscriptionStream, SubscriptionStreamEvent,
};
use ployzd::roles::api::http::{ApiRoleConfig, ApiServer};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpSocket;
use tokio::sync::oneshot;

const CLUSTER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const ROW_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const SECOND_ROW_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const RESUME_ROW_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
const RACE_LOWER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
const RACE_HIGHER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB0";
const API_CLUSTER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB1";
const API_MACHINE_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB2";
const API_BUILD: &str = "pinned-corrosion-api-version-test";
const TOKEN: &str = "ployz-stock-corrosion-test";

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
#[test]
fn pinned_corrosion_archive_has_no_supported_asset_for_this_platform() {
    panic!(
        "the pinned Corrosion integration test supports only linux/x86_64 and linux/aarch64; add and pin an official asset before enabling this platform"
    );
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[tokio::test]
async fn pinned_corrosion_executes_schema_rows_queries_and_subscriptions() {
    let mut harness = StockCorrosion::start()
        .await
        .unwrap_or_else(|error| panic!("could not start pinned Corrosion: {error}"));

    let result = async {
        exercise_contract(&harness.client).await?;
        exercise_api_version(&harness).await
    }
    .await;
    if let Err(error) = result {
        let logs = harness.logs();
        panic!("stock Corrosion contract failed: {error}\n--- corrosion.log ---\n{logs}");
    }

    harness.stop();
}

async fn exercise_contract(client: &CorrosionClient) -> Result<(), String> {
    let fixtures = fixtures()?;
    let inserts = fixtures
        .iter()
        .map(Fixture::insert_statement)
        .collect::<Vec<_>>();
    client
        .execute(&inserts)
        .await
        .map_err(|error| error.to_string())?;

    for fixture in &fixtures {
        let selected_columns = std::iter::once(fixture.key_column)
            .chain(std::iter::once("document"))
            .chain(fixture.generated.iter().map(|column| column.name))
            .collect::<Vec<_>>();
        let statement = Statement::with_params(
            format!(
                "SELECT {} FROM {} WHERE {} = ?",
                selected_columns.join(", "),
                fixture.table.as_str(),
                fixture.key_column
            ),
            vec![SqliteParameter::Text(fixture.key.to_owned())],
        );
        let (columns, values) = query_one(client, &statement).await?;
        if columns != selected_columns {
            return Err(format!(
                "{} returned columns {columns:?}, expected {selected_columns:?}",
                fixture.table.as_str()
            ));
        }
        let Some(SqliteValue::Text(key)) = values.first() else {
            return Err(format!("{} returned no text key", fixture.table.as_str()));
        };
        if key != fixture.key {
            return Err(format!(
                "{} returned key {key}, expected {}",
                fixture.table.as_str(),
                fixture.key
            ));
        }
        let Some(SqliteValue::Text(document)) = values.get(1) else {
            return Err(format!(
                "{} returned no text document",
                fixture.table.as_str()
            ));
        };
        if document != &fixture.document {
            return Err(format!(
                "{} did not return the inserted typed document",
                fixture.table.as_str()
            ));
        }
        let generated = values.get(2..).unwrap_or_default();
        let expected = fixture
            .generated
            .iter()
            .map(|column| column.expected.clone())
            .collect::<Vec<_>>();
        if generated != expected {
            return Err(format!(
                "{} generated values were {generated:?}, expected {expected:?}",
                fixture.table.as_str()
            ));
        }
    }

    let namespace_query = Statement::simple("SELECT id, document FROM namespaces");
    let (columns, values) = query_one(client, &namespace_query).await?;
    if columns != ["id", "document"] {
        return Err(format!("namespace query returned columns {columns:?}"));
    }
    let [SqliteValue::Text(key), SqliteValue::Text(document)] = values.as_slice() else {
        return Err(format!("namespace query returned values {values:?}"));
    };
    let cluster_id = ClusterId::try_new(CLUSTER_ID).map_err(|error| error.to_string())?;
    let report = read_named_rows::<NamespaceDocument>(
        &cluster_id,
        [ployz_core::corrosion::StoredRow::new(key, document)],
    );
    if report.accepted.len() != 1 || !report.skipped.is_empty() || !report.shadows.is_empty() {
        return Err("typed namespace reader rejected the stock query row".to_owned());
    }

    let mut subscription = client
        .subscribe(&namespace_query)
        .await
        .map_err(|error| error.to_string())?;
    consume_initial_snapshot(&mut subscription).await?;

    let second_document = encode::<NamespaceDocument>(json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "name": "staging",
        "written_by": {"kind": "peer", "peer_id": ROW_ID},
        "written_at": "2026-08-04T10:00:00Z"
    }))?;
    client
        .execute(&[Statement::with_params(
            "INSERT INTO namespaces (id, document) VALUES (?, ?)",
            vec![
                SqliteParameter::Text(SECOND_ROW_ID.to_owned()),
                SqliteParameter::Text(second_document),
            ],
        )])
        .await
        .map_err(|error| error.to_string())?;

    let change_id = match subscription
        .next()
        .await
        .map_err(|error| error.to_string())?
    {
        Some(SubscriptionStreamEvent::Change(ChangeKind::Insert, _, values, change_id)) => {
            if values.first() != Some(&SqliteValue::Text(SECOND_ROW_ID.to_owned())) {
                return Err(format!(
                    "subscription inserted unexpected namespace values {values:?}"
                ));
            }
            if subscription.last_contiguous_change_id() != Some(change_id) {
                return Err("subscription did not advance its contiguous cursor".to_owned());
            }
            change_id
        }
        event => return Err(format!("subscription returned unexpected event {event:?}")),
    };

    let identity = subscription.identity().clone();
    drop(subscription);
    let mut resumed = client
        .resume(&identity, change_id)
        .await
        .map_err(|error| error.to_string())?;
    let resume_document = encode::<NamespaceDocument>(json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "name": "resumed",
        "written_by": {"kind": "peer", "peer_id": ROW_ID},
        "written_at": "2026-08-04T10:00:00Z"
    }))?;
    client
        .execute(&[Statement::with_params(
            "INSERT INTO namespaces (id, document) VALUES (?, ?)",
            vec![
                SqliteParameter::Text(RESUME_ROW_ID.to_owned()),
                SqliteParameter::Text(resume_document),
            ],
        )])
        .await
        .map_err(|error| error.to_string())?;
    match resumed.next().await.map_err(|error| error.to_string())? {
        Some(SubscriptionStreamEvent::Change(ChangeKind::Insert, _, values, resumed_change_id))
            if values.first() == Some(&SqliteValue::Text(RESUME_ROW_ID.to_owned()))
                && resumed.last_contiguous_change_id() == Some(resumed_change_id) => {}
        event => return Err(format!("resumed subscription returned {event:?}")),
    }
    drop(resumed);

    let race_document = serde_json::from_value::<NamespaceDocument>(json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "name": "race",
        "written_by": {"kind": "peer", "peer_id": ROW_ID},
        "written_at": "2026-08-04T10:00:00Z"
    }))
    .map_err(|error| error.to_string())?;
    let lower_id = CorrosionUlid::try_new(RACE_LOWER_ID).map_err(|error| error.to_string())?;
    let higher_id = CorrosionUlid::try_new(RACE_HIGHER_ID).map_err(|error| error.to_string())?;
    let (lower, higher) = tokio::join!(
        client.claim_named(lower_id.clone(), &race_document),
        client.claim_named(higher_id.clone(), &race_document),
    );
    match lower.map_err(|error| error.to_string())? {
        NameClaimOutcome::Claimed { id, .. } if id == lower_id => {}
        NameClaimOutcome::Claimed { id, .. } => {
            return Err(format!("lower claim reported unexpected winner {id}"));
        }
        NameClaimOutcome::Lost { winner, .. } => {
            return Err(format!("lower claim lost to {}", winner.id));
        }
    }
    match higher.map_err(|error| error.to_string())? {
        NameClaimOutcome::Lost { id, winner, .. } if id == higher_id && winner.id == lower_id => {}
        NameClaimOutcome::Lost { id, winner, .. } => {
            return Err(format!(
                "claim {id} lost to unexpected winner {}",
                winner.id
            ));
        }
        NameClaimOutcome::Claimed { id, .. } => {
            return Err(format!("higher claim {id} won"));
        }
    }
    let (_, values) = query_one(
        client,
        &Statement::with_params(
            "SELECT id, document FROM namespaces WHERE name = ?",
            vec![SqliteParameter::Text("race".to_owned())],
        ),
    )
    .await?;
    if values.first() != Some(&SqliteValue::Text(RACE_LOWER_ID.to_owned())) {
        return Err(format!(
            "claim cleanup retained the wrong namespace row: {values:?}"
        ));
    }

    Ok(())
}

async fn exercise_api_version(harness: &StockCorrosion) -> Result<(), String> {
    let (cluster_id, machine_id) = insert_api_loopback_roster(&harness.client).await?;
    let listen_addr = unused_loopback_addr()?;
    let corrosion = CorrosionClientConfig::new(
        harness.api_addr,
        BearerToken::new(TOKEN).map_err(|error| error.to_string())?,
        CorrosionClientBounds {
            connect_timeout: Duration::from_millis(500),
            request_timeout: Duration::from_secs(2),
            stream_idle_timeout: Duration::from_secs(5),
            max_ndjson_frame_bytes: 1024 * 1024,
            max_error_body_bytes: 64 * 1024,
        },
    )
    .map_err(|error| error.to_string())?;
    let config = ApiRoleConfig::new(
        corrosion,
        cluster_id,
        machine_id,
        listen_addr,
        API_BUILD.to_owned(),
    )
    .map_err(|error| error.to_string())?;
    let server = ApiServer::bind_without_join_door_for_integration_test(config)
        .await
        .map_err(|error| format!("bind API server: {error}"))?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server_task = tokio::spawn(async move {
        server
            .serve(async move {
                let _ = shutdown_rx.await;
            })
            .await
            .expect("API server stops cleanly after controlled shutdown");
    });

    let version_response = get_from_loopback(listen_addr, "/version").await;
    let status_response = get_from_loopback(listen_addr, "/status").await;
    let doctor_response = get_from_loopback(listen_addr, "/doctor").await;
    let shutdown = shutdown_api_server(shutdown_tx, server_task).await;
    let (status, body) = version_response?;
    shutdown?;

    if status != 200 {
        return Err(format!("GET /version returned HTTP {status}: {body}"));
    }
    let version = serde_json::from_str::<ApiVersion>(&body)
        .map_err(|error| format!("decode typed /version response: {error}: {body}"))?;
    if version.major != API_MAJOR {
        return Err(format!(
            "/version major was {}, expected {API_MAJOR}",
            version.major
        ));
    }
    if version.build != API_BUILD {
        return Err(format!(
            "/version build was {:?}, expected {API_BUILD:?}",
            version.build
        ));
    }
    if !version
        .features
        .contains(&ApiFeature::Known(KnownApiFeature::Lenses))
    {
        return Err(format!(
            "/version did not advertise the lenses feature: {:?}",
            version.features
        ));
    }

    let (status, body) = status_response?;
    if status != 200 {
        return Err(format!("GET /status returned HTTP {status}: {body}"));
    }
    let status = serde_json::from_str::<StatusDocument>(&body)
        .map_err(|error| format!("decode typed /status response: {error}: {body}"))?;
    if status.barrier != StatusBarrier::Ready {
        return Err(format!(
            "/status barrier was {:?}, expected ready",
            status.barrier
        ));
    }

    let (status, body) = doctor_response?;
    if status != 200 {
        return Err(format!("GET /doctor returned HTTP {status}: {body}"));
    }
    serde_json::from_str::<DoctorDocument>(&body)
        .map_err(|error| format!("decode typed /doctor response: {error}: {body}"))?;

    Ok(())
}

async fn insert_api_loopback_roster(
    client: &CorrosionClient,
) -> Result<(ClusterId, MachineRowId), String> {
    let cluster_id = ClusterId::try_new(API_CLUSTER_ID).map_err(|error| error.to_string())?;
    let machine_id = MachineRowId::try_new(API_MACHINE_ID).map_err(|error| error.to_string())?;
    let cluster = encode::<ClusterDocument>(json!({
        "v": 1,
        "cluster_id": cluster_id.as_str(),
        "name": "api-loopback-cluster",
        "storage_default": "plain",
        "hostname_mode": {"mode": "disabled"},
        "prefix": "10.211.0.0/16",
        "provider": "tailscale",
        "acme_directory_url": "https://acme.invalid/directory",
        "acme_contact": null,
        "written_by": {"kind": "machine", "machine_id": machine_id.as_str()},
        "written_at": "2026-08-04T10:10:00Z"
    }))?;
    let machine = encode::<MachineDocument>(json!({
        "v": 1,
        "cluster_id": cluster_id.as_str(),
        "name": "api-loopback-machine",
        "lifecycle": "active",
        "transport": {
            "kind": "tailscale",
            "ip": "127.0.0.1",
            "subnet_v4": "10.211.20.0/24"
        },
        "storage": {"mode": "plain", "reason": {"kind": "default"}},
        "written_by": {"kind": "machine", "machine_id": machine_id.as_str()},
        "written_at": "2026-08-04T10:10:00Z"
    }))?;
    client
        .execute(&[
            Statement::with_params(
                "INSERT INTO cluster (id, document) VALUES (?, ?)",
                vec![
                    SqliteParameter::Text(cluster_id.as_str().to_owned()),
                    SqliteParameter::Text(cluster),
                ],
            ),
            Statement::with_params(
                "INSERT INTO machines (id, document) VALUES (?, ?)",
                vec![
                    SqliteParameter::Text(machine_id.as_str().to_owned()),
                    SqliteParameter::Text(machine),
                ],
            ),
        ])
        .await
        .map_err(|error| format!("insert API loopback roster: {error}"))?;
    Ok((cluster_id, machine_id))
}

async fn get_from_loopback(destination: SocketAddr, path: &str) -> Result<(u16, String), String> {
    let socket =
        TcpSocket::new_v4().map_err(|error| format!("create API client socket: {error}"))?;
    socket
        .bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .map_err(|error| format!("bind API client source: {error}"))?;
    let mut stream = tokio::time::timeout(Duration::from_secs(2), socket.connect(destination))
        .await
        .map_err(|_| "connect to API server timed out".to_owned())?
        .map_err(|error| format!("connect to API server: {error}"))?;
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {destination}\r\nConnection: close\r\n\r\n");
    tokio::time::timeout(Duration::from_secs(2), stream.write_all(request.as_bytes()))
        .await
        .map_err(|_| "write API request timed out".to_owned())?
        .map_err(|error| format!("write API request: {error}"))?;
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
        .await
        .map_err(|_| "read API response timed out".to_owned())?
        .map_err(|error| format!("read API response: {error}"))?;
    let response =
        String::from_utf8(response).map_err(|error| format!("API response UTF-8: {error}"))?;
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| "API response omitted HTTP headers".to_owned())?;
    let status = headers
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "API response omitted a status code".to_owned())?
        .parse::<u16>()
        .map_err(|error| format!("parse API response status: {error}"))?;
    Ok((status, body.to_owned()))
}

async fn shutdown_api_server(
    shutdown_tx: oneshot::Sender<()>,
    server_task: tokio::task::JoinHandle<()>,
) -> Result<(), String> {
    shutdown_tx
        .send(())
        .map_err(|_| "API server stopped before controlled shutdown".to_owned())?;
    tokio::time::timeout(Duration::from_secs(3), server_task)
        .await
        .map_err(|_| "API server did not stop within three seconds".to_owned())?
        .map_err(|error| format!("API server task failed: {error}"))
}

async fn consume_initial_snapshot(stream: &mut SubscriptionStream) -> Result<(), String> {
    loop {
        match stream.next().await.map_err(|error| error.to_string())? {
            Some(SubscriptionStreamEvent::Columns(_) | SubscriptionStreamEvent::Row(_, _)) => {}
            Some(SubscriptionStreamEvent::EndOfQuery(_)) => return Ok(()),
            Some(SubscriptionStreamEvent::Change(_, _, _, _)) | None => {
                return Err("subscription left its initial snapshot without eoq".to_owned());
            }
        }
    }
}

async fn query_one(
    client: &CorrosionClient,
    statement: &Statement,
) -> Result<(Vec<String>, Vec<SqliteValue>), String> {
    let mut stream = client
        .query(statement)
        .await
        .map_err(|error| error.to_string())?;
    let mut columns = None;
    let mut row = None;
    while let Some(event) = stream.next().await.map_err(|error| error.to_string())? {
        match event {
            QueryStreamEvent::Columns(value) => columns = Some(value),
            QueryStreamEvent::Row(_, value) if row.is_none() => row = Some(value),
            QueryStreamEvent::Row(_, _) => {
                return Err("query returned more than one row".to_owned());
            }
            QueryStreamEvent::EndOfQuery(_) => break,
        }
    }
    let Some(columns) = columns else {
        return Err("query returned no columns frame".to_owned());
    };
    let Some(row) = row else {
        return Err("query returned no row".to_owned());
    };
    Ok((columns, row))
}

struct GeneratedColumn {
    name: &'static str,
    expected: SqliteValue,
}

struct Fixture {
    table: CorrosionTable,
    key_column: &'static str,
    key: &'static str,
    document: String,
    generated: Vec<GeneratedColumn>,
}

impl Fixture {
    fn insert_statement(&self) -> Statement {
        Statement::with_params(
            format!(
                "INSERT INTO {} ({}, document) VALUES (?, ?)",
                self.table.as_str(),
                self.key_column
            ),
            vec![
                SqliteParameter::Text(self.key.to_owned()),
                SqliteParameter::Text(self.document.clone()),
            ],
        )
    }
}

fn column(name: &'static str, expected: impl Into<String>) -> GeneratedColumn {
    GeneratedColumn {
        name,
        expected: SqliteValue::Text(expected.into()),
    }
}

fn fixture<Document>(
    table: CorrosionTable,
    key_column: &'static str,
    key: &'static str,
    value: Value,
    generated: Vec<GeneratedColumn>,
) -> Result<Fixture, String>
where
    Document: CorrosionDocument,
{
    if Document::TABLE != table {
        return Err(format!(
            "fixture type owns {}, not {}",
            Document::TABLE.as_str(),
            table.as_str()
        ));
    }
    Ok(Fixture {
        table,
        key_column,
        key,
        document: encode::<Document>(value)?,
        generated,
    })
}

fn encode<Document>(value: Value) -> Result<String, String>
where
    Document: CorrosionDocument,
{
    let typed = serde_json::from_value::<Document>(value).map_err(|error| error.to_string())?;
    serde_json::to_string(&typed).map_err(|error| error.to_string())
}

fn fixtures() -> Result<Vec<Fixture>, String> {
    let machine_id = ROW_ID;
    let namespace_id = SECOND_ROW_ID;
    let service_id = RESUME_ROW_ID;
    let operation_id = RACE_LOWER_ID;
    let base = |name: &str| {
        json!({
            "v": 1,
            "cluster_id": CLUSTER_ID,
            "name": name,
            "written_by": {"kind": "peer", "peer_id": ROW_ID},
            "written_at": "2026-08-04T10:00:00Z"
        })
    };

    Ok(vec![
        fixture::<ClusterDocument>(
            CorrosionTable::Cluster,
            "id",
            CLUSTER_ID,
            json!({
                "v": 1,
                "cluster_id": CLUSTER_ID,
                "name": "acme-prod",
                "storage_default": "plain",
                "hostname_mode": {"mode": "disabled"},
                "prefix": "10.210.0.0/16",
                "provider": "builtin_wireguard",
                "acme_directory_url": "https://acme.example/directory",
                "acme_contact": null,
                "written_by": {"kind": "peer", "peer_id": ROW_ID},
                "written_at": "2026-08-04T10:00:00Z"
            }),
            vec![],
        )?,
        fixture::<MachineDocument>(
            CorrosionTable::Machines,
            "id",
            ROW_ID,
            json!({
                "v": 1,
                "cluster_id": CLUSTER_ID,
                "name": "edge-a",
                "lifecycle": "active",
                "transport": {
                    "kind": "wireguard",
                    "pubkey": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                    "addr_v6": "fd00::20",
                    "endpoint": null,
                    "subnet_v4": "10.210.20.0/24"
                },
                "storage": {"mode": "plain", "reason": {"kind": "default"}},
                "written_by": {"kind": "peer", "peer_id": ROW_ID},
                "written_at": "2026-08-04T10:00:00Z"
            }),
            vec![column("name", "edge-a"), column("lifecycle", "active")],
        )?,
        fixture::<PeerDocument>(
            CorrosionTable::Peers,
            "id",
            ROW_ID,
            json!({
                "v": 1,
                "cluster_id": CLUSTER_ID,
                "name": "operator-laptop",
                "transport": {"kind": "tailscale", "ip": "100.64.0.11"},
                "written_by": {"kind": "peer", "peer_id": ROW_ID},
                "written_at": "2026-08-04T10:00:00Z"
            }),
            vec![column("name", "operator-laptop")],
        )?,
        fixture::<TokenDocument>(
            CorrosionTable::Tokens,
            "id",
            ROW_ID,
            json!({
                "v": 1,
                "cluster_id": CLUSTER_ID,
                "secret_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "created_at": "2026-08-04T10:00:00Z",
                "expires_at": "2026-08-05T10:00:00Z",
                "written_by": {"kind": "peer", "peer_id": ROW_ID},
                "written_at": "2026-08-04T10:00:00Z"
            }),
            vec![GeneratedColumn {
                name: "kind",
                expected: SqliteValue::Null,
            }],
        )?,
        fixture::<NamespaceDocument>(
            CorrosionTable::Namespaces,
            "id",
            ROW_ID,
            base("production"),
            vec![column("name", "production")],
        )?,
        fixture::<ServiceDocument>(
            CorrosionTable::Services,
            "id",
            ROW_ID,
            json!({
                "v": 1,
                "cluster_id": CLUSTER_ID,
                "namespace_id": namespace_id,
                "name": "api",
                "image": "ghcr.io/acme/api:2026-08-04",
                "env_fingerprints": {"DATABASE_URL": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},
                "mode": "replicated",
                "replicas": 1,
                "pinned_machines": [machine_id],
                "active_deploy": operation_id,
                "previous_image": null,
                "deployed_at": "2026-08-04T10:05:00Z",
                "operation_id": operation_id,
                "written_by": {"kind": "peer", "peer_id": ROW_ID},
                "written_at": "2026-08-04T10:00:00Z"
            }),
            vec![column("namespace_id", namespace_id), column("name", "api")],
        )?,
        fixture::<RouteBindingDocument>(
            CorrosionTable::RouteBindings,
            "id",
            ROW_ID,
            json!({
                "v": 1,
                "cluster_id": CLUSTER_ID,
                "hostname": "api.example.com",
                "service_id": service_id,
                "namespace_id": namespace_id,
                "endpoint_port": 8080,
                "origin": "declared",
                "ingress_mode": "direct",
                "written_by": {"kind": "peer", "peer_id": ROW_ID},
                "written_at": "2026-08-04T10:00:00Z"
            }),
            vec![
                column("hostname", "api.example.com"),
                column("service_id", service_id),
                column("namespace_id", namespace_id),
            ],
        )?,
        fixture::<ContainerDocument>(
            CorrosionTable::Containers,
            "id",
            "container-runtime-id",
            json!({
                "v": 1,
                "cluster_id": CLUSTER_ID,
                "machine_id": machine_id,
                "service_id": service_id,
                "namespace_id": namespace_id,
                "ip": "10.210.20.2",
                "deploy": operation_id
            }),
            vec![
                column("machine_id", machine_id),
                column("service_id", service_id),
                column("namespace_id", namespace_id),
            ],
        )?,
        fixture::<MachineStatusDocument>(
            CorrosionTable::MachineStatus,
            "machine_id",
            ROW_ID,
            json!({
                "v": 1,
                "cluster_id": CLUSTER_ID,
                "machine_id": machine_id,
                "ployz_version": "0.1.0-alpha.7",
                "corrosion_version": "0.2.0-beta.0",
                "architecture": "x86_64",
                "free_disk_bytes": 80000000000_u64,
                "free_memory_bytes": 4000000000_u64,
                "load": "idle",
                "observed_at": "2026-08-04T10:06:00Z"
            }),
            vec![],
        )?,
        fixture::<OperationDocument>(
            CorrosionTable::Operations,
            "id",
            operation_id,
            json!({
                "v": 1,
                "cluster_id": CLUSTER_ID,
                "machine_id": machine_id,
                "kind": "deploy",
                "namespace_id": namespace_id,
                "service_id": service_id,
                "initiator": {"kind": "peer", "peer_id": ROW_ID},
                "state": "failed",
                "started_at": "2026-08-04T10:00:00Z",
                "completed_at": "2026-08-04T10:07:00Z",
                "failure": {
                    "kind": "execution",
                    "class": "health_gate_failed",
                    "message": "container did not become healthy"
                }
            }),
            vec![
                column("kind", "deploy"),
                column("state", "failed"),
                column("machine_id", machine_id),
            ],
        )?,
        fixture::<CertHoldingDocument>(
            CorrosionTable::CertHoldings,
            "id",
            "01ARZ3NDEKTSV4RRFFQ69G5FAV:api.example.com",
            json!({
                "v": 1,
                "cluster_id": CLUSTER_ID,
                "machine_id": machine_id,
                "hostname": "api.example.com",
                "fingerprint": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                "issued_at": "2026-08-04T10:08:00Z",
                "expires_at": "2026-11-02T10:08:00Z"
            }),
            vec![
                column("hostname", "api.example.com"),
                column("machine_id", machine_id),
                column("expires_at", "2026-11-02T10:08:00.000000000Z"),
            ],
        )?,
        fixture::<AcmeHttp01Document>(
            CorrosionTable::AcmeHttp01,
            "id",
            "acme-challenge-token",
            json!({
                "v": 1,
                "cluster_id": CLUSTER_ID,
                "machine_id": machine_id,
                "hostname": "api.example.com",
                "key_authorization": "public-acme-key-authorization",
                "created_at": "2026-08-04T10:09:00Z"
            }),
            vec![column("machine_id", machine_id)],
        )?,
    ])
}

#[derive(Debug)]
struct ReleasePin {
    release_tag: String,
    archive_name: String,
    archive_url: String,
    archive_sha256: String,
    embedded_version: String,
}

impl ReleasePin {
    fn load(repo_root: &Path) -> Result<Self, String> {
        let platform = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "x86_64") => "linux-amd64",
            ("linux", "aarch64") => "linux-arm64",
            (os, arch) => {
                return Err(format!(
                    "unsupported Corrosion release platform {os}/{arch}"
                ));
            }
        };
        Self::load_for_platform(
            repo_root,
            &repo_root.join("corrosion-release.json"),
            platform,
        )
    }

    fn load_for_platform(
        repo_root: &Path,
        manifest_path: &Path,
        platform: &str,
    ) -> Result<Self, String> {
        let reader = repo_root.join("scripts/read-corrosion-release.py");
        let output = Command::new("python3")
            .arg(&reader)
            .arg(manifest_path)
            .arg(platform)
            .output()
            .map_err(|error| format!("run {}: {error}", reader.display()))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("Corrosion release reader output is not UTF-8: {error}"))?;
        let fields = stdout.lines().collect::<Vec<_>>();
        let [
            release_tag,
            archive_name,
            archive_url,
            archive_sha256,
            embedded_version,
        ] = fields.as_slice()
        else {
            return Err(
                "Corrosion release reader did not yield exactly five pinned fields".to_owned(),
            );
        };
        if fields.iter().any(|field| field.is_empty()) {
            return Err("Corrosion release reader yielded an empty pinned field".to_owned());
        }
        Ok(Self {
            release_tag: (*release_tag).to_owned(),
            archive_name: (*archive_name).to_owned(),
            archive_url: (*archive_url).to_owned(),
            archive_sha256: (*archive_sha256).to_owned(),
            embedded_version: (*embedded_version).to_owned(),
        })
    }
}

#[test]
fn corrosion_release_pin_parser_selects_both_supported_linux_assets() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest_path = repo_root.join("corrosion-release.json");

    let amd64 = ReleasePin::load_for_platform(&repo_root, &manifest_path, "linux-amd64")
        .expect("amd64 pin is complete");
    assert_eq!(
        amd64.archive_name,
        "corrosion-x86_64-unknown-linux-gnu.tar.gz"
    );
    assert_eq!(
        amd64.archive_sha256,
        "3504d7d1b4b53737457fc40f2353a400cf4df0c1217ec318924d7ee310876194"
    );

    let arm64 = ReleasePin::load_for_platform(&repo_root, &manifest_path, "linux-arm64")
        .expect("arm64 pin is complete");
    assert_eq!(
        arm64.archive_name,
        "corrosion-aarch64-unknown-linux-gnu.tar.gz"
    );
    assert_eq!(
        arm64.archive_sha256,
        "b00b5c6996dee89c36e51e1d4196602bb7e12bbb25abc9b68390e062bdd13be3"
    );
    assert_eq!(arm64.release_tag, "v1.0.0");
    assert_eq!(arm64.embedded_version, "corrosion 0.2.0-beta.0");
}

#[test]
fn corrosion_release_pin_parser_rejects_an_unsupported_platform() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let error = ReleasePin::load_for_platform(
        &repo_root,
        &repo_root.join("corrosion-release.json"),
        "linux-riscv64",
    )
    .expect_err("unsupported platform is rejected before pin selection");
    assert_eq!(
        error,
        "corrosion release manifest has no pin for linux-riscv64"
    );
}

#[test]
fn corrosion_release_pin_parser_rejects_noncanonical_archive_names() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let temp = TempDir::new().expect("temporary directory");
    let manifest_path = temp.path().join("corrosion-release.json");
    let source =
        fs::read_to_string(repo_root.join("corrosion-release.json")).expect("manifest reads");
    fs::write(
        &manifest_path,
        source.replace(
            "corrosion-x86_64-unknown-linux-gnu.tar.gz",
            "corrosion-linux-amd64.tar.gz",
        ),
    )
    .expect("invalid manifest writes");

    let error = ReleasePin::load_for_platform(&repo_root, &manifest_path, "linux-amd64")
        .expect_err("noncanonical archive is rejected");
    assert_eq!(
        error,
        "corrosion release manifest archive name for linux-amd64 is not canonical"
    );
}

#[test]
fn corrosion_release_pin_parser_rejects_schema_extensions() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let temp = TempDir::new().expect("temporary directory");
    let manifest_path = temp.path().join("corrosion-release.json");
    let mut manifest: Value = serde_json::from_slice(
        &fs::read(repo_root.join("corrosion-release.json")).expect("manifest reads"),
    )
    .expect("manifest parses");
    let Some(root) = manifest.as_object_mut() else {
        panic!("release manifest has an object root")
    };
    root.insert("unexpected".to_owned(), json!(true));
    fs::write(
        &manifest_path,
        serde_json::to_vec(&manifest).expect("invalid manifest serializes"),
    )
    .expect("invalid manifest writes");

    let error = ReleasePin::load_for_platform(&repo_root, &manifest_path, "linux-amd64")
        .expect_err("schema extension is rejected");
    assert_eq!(error, "corrosion release manifest root is invalid");
}

struct StockCorrosion {
    _temp: TempDir,
    child: Option<Child>,
    log_path: PathBuf,
    api_addr: SocketAddr,
    client: CorrosionClient,
}

impl StockCorrosion {
    async fn start() -> Result<Self, String> {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let release = ReleasePin::load(&repo_root)?;
        let archive = release_archive(&repo_root, &release)?;
        verify_sha256(&archive, &release.archive_sha256)?;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let extract = Command::new("tar")
            .args(["-xzf"])
            .arg(&archive)
            .arg("-C")
            .arg(temp.path())
            .output()
            .map_err(|error| format!("run tar: {error}"))?;
        if !extract.status.success() {
            return Err(format!(
                "extract {}: {}",
                archive.display(),
                String::from_utf8_lossy(&extract.stderr)
            ));
        }
        let binary = temp.path().join("corrosion");
        let version = Command::new(&binary)
            .arg("--version")
            .output()
            .map_err(|error| format!("run {} --version: {error}", binary.display()))?;
        if !version.status.success() {
            return Err(format!(
                "{} --version failed: {}",
                binary.display(),
                String::from_utf8_lossy(&version.stderr)
            ));
        }
        let actual_version = String::from_utf8_lossy(&version.stdout).trim().to_owned();
        if actual_version != release.embedded_version {
            return Err(format!(
                "{} contains {actual_version:?}, expected {:?} for {}",
                archive.display(),
                release.embedded_version,
                release.release_tag
            ));
        }

        let api_addr = unused_loopback_addr()?;
        let gossip_addr = unused_loopback_addr()?;
        let subscriptions = temp.path().join("subscriptions");
        let run = temp.path().join("run");
        fs::create_dir_all(&subscriptions).map_err(|error| error.to_string())?;
        fs::create_dir_all(&run).map_err(|error| error.to_string())?;
        let config_path = temp.path().join("config.toml");
        let schema_path = repo_root.join("docs/design/corrosion-schema-v1.sql");
        let quote = |path: &Path| {
            serde_json::to_string(&path.to_string_lossy())
                .map_err(|error| format!("quote path {}: {error}", path.display()))
        };
        let config = format!(
            "[db]\npath = {}\nschema_paths = [{}]\nsubscriptions_path = {}\n\n[gossip]\naddr = {:?}\nbootstrap = []\nplaintext = true\nmax_mtu = 1232\n\n[api]\naddr = {:?}\nauthz.bearer-token = {TOKEN:?}\n\n[admin]\npath = {}\n",
            quote(&temp.path().join("corrosion.db"))?,
            quote(&schema_path)?,
            quote(&subscriptions)?,
            gossip_addr.to_string(),
            api_addr.to_string(),
            quote(&run.join("admin.sock"))?,
        );
        fs::write(&config_path, config).map_err(|error| error.to_string())?;

        let bearer = BearerToken::new(TOKEN).map_err(|error| error.to_string())?;
        let config = CorrosionClientConfig::new(
            api_addr,
            bearer,
            CorrosionClientBounds {
                connect_timeout: Duration::from_millis(500),
                request_timeout: Duration::from_secs(2),
                stream_idle_timeout: Duration::from_secs(5),
                max_ndjson_frame_bytes: 1024 * 1024,
                max_error_body_bytes: 64 * 1024,
            },
        )
        .map_err(|error| error.to_string())?;
        let client = CorrosionClient::new(config).map_err(|error| error.to_string())?;

        let log_path = temp.path().join("corrosion.log");
        let log = File::create(&log_path).map_err(|error| error.to_string())?;
        let stderr = log.try_clone().map_err(|error| error.to_string())?;
        let child = Command::new(&binary)
            .args(["--config"])
            .arg(&config_path)
            .arg("agent")
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|error| format!("spawn {}: {error}", binary.display()))?;

        let mut harness = Self {
            _temp: temp,
            child: Some(child),
            log_path,
            api_addr,
            client,
        };

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last_error = "Corrosion was not queried".to_owned();
        while Instant::now() < deadline {
            if let Some(status) = harness
                .child
                .as_mut()
                .expect("running child")
                .try_wait()
                .map_err(|error| error.to_string())?
            {
                return Err(format!(
                    "Corrosion exited during startup with {status}: {}",
                    harness.logs()
                ));
            }
            match query_one(&harness.client, &Statement::simple("SELECT 1")).await {
                Ok(_) => return Ok(harness),
                Err(error) => last_error = error,
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Err(format!(
            "Corrosion did not become ready: {last_error}: {}",
            harness.logs()
        ))
    }

    fn logs(&self) -> String {
        fs::read_to_string(&self.log_path)
            .unwrap_or_else(|error| format!("could not read {}: {error}", self.log_path.display()))
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for StockCorrosion {
    fn drop(&mut self) {
        self.stop();
    }
}

fn unused_loopback_addr() -> Result<SocketAddr, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    listener.local_addr().map_err(|error| error.to_string())
}

fn release_archive(repo_root: &Path, release: &ReleasePin) -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("PLOYZ_CORROSION_ARCHIVE") {
        return Ok(PathBuf::from(path));
    }

    let cache_dir = repo_root
        .join("target/corrosion-cache")
        .join(&release.release_tag);
    fs::create_dir_all(&cache_dir).map_err(|error| error.to_string())?;
    let archive = cache_dir.join(&release.archive_name);
    if archive.is_file() && verify_sha256(&archive, &release.archive_sha256).is_ok() {
        return Ok(archive);
    }

    let partial = cache_dir.join(format!(
        "{}.{}.partial",
        release.archive_name,
        std::process::id()
    ));
    let output = Command::new("curl")
        .args(["--fail", "--location", "--retry", "3", "--output"])
        .arg(&partial)
        .arg(&release.archive_url)
        .output()
        .map_err(|error| {
            format!(
                "download {}: {error}; offline runs must set PLOYZ_CORROSION_ARCHIVE",
                release.archive_url
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "download {} failed: {}; offline runs must set PLOYZ_CORROSION_ARCHIVE",
            release.archive_url,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    verify_sha256(&partial, &release.archive_sha256)?;
    fs::rename(&partial, &archive).map_err(|error| error.to_string())?;
    Ok(archive)
}

fn verify_sha256(path: &Path, expected: &str) -> Result<(), String> {
    let mut file = File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    let mut digest = Sha256::new();
    std::io::copy(&mut file, &mut digest)
        .map_err(|error| format!("hash {}: {error}", path.display()))?;
    let actual = format!("{:x}", digest.finalize());
    if actual != expected {
        return Err(format!(
            "{} has SHA-256 {actual}, expected {expected}",
            path.display()
        ));
    }
    Ok(())
}
