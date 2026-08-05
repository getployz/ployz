use ployz_core::corrosion::{
    CertHoldingDocument, ClusterDocument, ContainerDocument, MachineDocument,
    MachineStatusDocument, MalformedDocument, MeshProvider, NamespaceDocument, RowSkipReason,
    StoredRow, read_named_roster_rows, read_named_rows, read_roster_rows, read_rows,
};
use ployz_core::ids::ClusterId;
use serde_json::json;

const CLUSTER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const LOWER_ROW_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const HIGHER_ROW_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";

fn cluster_id() -> ClusterId {
    ClusterId::try_new(CLUSTER_ID).expect("fixture cluster id is canonical")
}

fn namespace_document(name: &str) -> String {
    format!(
        r#"{{"v":1,"cluster_id":"{CLUSTER_ID}","name":"{name}","written_by":{{"kind":"machine","machine_id":"{LOWER_ROW_ID}"}},"written_at":"2026-08-04T10:00:00Z"}}"#
    )
}

fn cluster_document(provider: MeshProvider) -> ClusterDocument {
    serde_json::from_value(json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "name": "acme-prod",
        "storage_default": "plain",
        "hostname_mode": { "mode": "disabled" },
        "prefix": "10.210.0.0/16",
        "provider": provider,
        "acme_directory_url": "https://acme.example/directory",
        "acme_contact": null,
        "written_by": {
            "kind": "machine",
            "machine_id": LOWER_ROW_ID
        },
        "written_at": "2026-08-04T10:00:00Z"
    }))
    .expect("cluster fixture")
}

fn machine_document(transport: serde_json::Value) -> String {
    json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "name": "edge-a",
        "lifecycle": "active",
        "transport": transport,
        "storage": {
            "mode": "plain",
            "reason": { "kind": "default" }
        },
        "written_by": {
            "kind": "machine",
            "machine_id": LOWER_ROW_ID
        },
        "written_at": "2026-08-04T10:00:00Z"
    })
    .to_string()
}

fn certificate_document(machine_id: &str, hostname: &str, expires_at: &str) -> String {
    json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "machine_id": machine_id,
        "hostname": hostname,
        "fingerprint": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        "issued_at": "2026-08-04T10:08:00Z",
        "expires_at": expires_at
    })
    .to_string()
}

#[test]
fn roster_reader_accepts_a_transport_matching_the_cluster_provider() {
    let cluster = cluster_document(MeshProvider::BuiltinWireguard);
    let report = read_roster_rows::<MachineDocument>(
        &cluster,
        [StoredRow::new(
            LOWER_ROW_ID,
            machine_document(json!({
                "kind": "wireguard",
                "pubkey": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                "addr_v6": "fd00::20",
                "endpoint": "192.0.2.10:51820",
                "subnet_v4": "10.210.20.0/24"
            })),
        )],
    );

    assert!(report.skipped.is_empty());
    assert!(matches!(
        report.accepted.as_slice(),
        [accepted] if accepted.value.name.as_str() == "edge-a"
    ));
}

#[test]
fn roster_reader_skips_and_surfaces_a_transport_for_the_wrong_provider() {
    let cluster = cluster_document(MeshProvider::BuiltinWireguard);
    let report = read_roster_rows::<MachineDocument>(
        &cluster,
        [StoredRow::new(
            LOWER_ROW_ID,
            machine_document(json!({
                "kind": "tailscale",
                "ip": "100.64.0.10",
                "subnet_v4": "10.210.20.0/24"
            })),
        )],
    );

    assert!(report.accepted.is_empty());
    assert!(matches!(
        report.skipped.as_slice(),
        [skipped]
            if skipped.source.key == LOWER_ROW_ID
                && matches!(
                    skipped.reason,
                    RowSkipReason::MeshProviderMismatch {
                        expected: MeshProvider::BuiltinWireguard,
                        found: MeshProvider::Tailscale,
                    }
                )
    ));
}

#[test]
fn wrong_provider_lower_ulid_cannot_win_or_shadow_a_valid_named_roster_row() {
    let cluster = cluster_document(MeshProvider::BuiltinWireguard);
    let report = read_named_roster_rows::<MachineDocument>(
        &cluster,
        [
            StoredRow::new(
                LOWER_ROW_ID,
                machine_document(json!({
                    "kind": "tailscale",
                    "ip": "100.64.0.10",
                    "subnet_v4": "10.210.20.0/24"
                })),
            ),
            StoredRow::new(
                HIGHER_ROW_ID,
                machine_document(json!({
                    "kind": "wireguard",
                    "pubkey": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                    "addr_v6": "fd00::20",
                    "endpoint": "192.0.2.10:51820",
                    "subnet_v4": "10.210.20.0/24"
                })),
            ),
        ],
    );

    assert!(matches!(
        report.accepted.as_slice(),
        [accepted] if accepted.id.as_str() == HIGHER_ROW_ID
    ));
    assert!(report.shadows.is_empty());
    assert!(matches!(
        report.skipped.as_slice(),
        [skipped]
            if skipped.source.key == LOWER_ROW_ID
                && matches!(
                    skipped.reason,
                    RowSkipReason::MeshProviderMismatch { .. }
                )
    ));
}

#[test]
fn empty_and_malformed_rows_stay_visible_as_typed_skips() {
    let report = read_rows::<NamespaceDocument>(
        &cluster_id(),
        [
            StoredRow::new("whitespace", "  \n"),
            StoredRow::new("zero-key-object", "{}"),
            StoredRow::new("invalid-json", "{"),
            StoredRow::new(
                "missing-version",
                format!(r#"{{"cluster_id":"{CLUSTER_ID}","name":"alpha"}}"#),
            ),
            StoredRow::new(
                "unsupported-old-version",
                format!(r#"{{"v":0,"cluster_id":"{CLUSTER_ID}","name":"alpha"}}"#),
            ),
        ],
    );

    assert!(report.accepted.is_empty());
    assert!(matches!(
        report.skipped.as_slice(),
        [whitespace, zero_key, invalid_json, missing_version, unsupported_version]
            if matches!(whitespace.reason, RowSkipReason::Empty)
                && whitespace.source.key == "whitespace"
                && whitespace.source.document == "  \n"
                && matches!(zero_key.reason, RowSkipReason::Empty)
                && matches!(
                    invalid_json.reason,
                    RowSkipReason::Malformed(MalformedDocument::InvalidJson { .. })
                )
                && matches!(
                    missing_version.reason,
                    RowSkipReason::Malformed(MalformedDocument::MissingVersion)
                )
                && matches!(
                    unsupported_version.reason,
                    RowSkipReason::Malformed(MalformedDocument::UnsupportedVersion { found: 0 })
                )
    ));
}

#[test]
fn raw_cluster_fence_wins_before_other_header_validation() {
    let report = read_rows::<NamespaceDocument>(
        &cluster_id(),
        [StoredRow::new(
            LOWER_ROW_ID,
            r#"{"v":"not-an-integer","cluster_id":"not-a-ulid","name":42}"#,
        )],
    );

    assert!(report.accepted.is_empty());
    assert!(matches!(
        report.skipped.as_slice(),
        [skipped]
            if matches!(
                &skipped.reason,
                RowSkipReason::ForeignCluster { expected, found }
                    if expected == CLUSTER_ID && found == "not-a-ulid"
            )
    ));
}

#[test]
fn newer_version_is_skipped_without_interpreting_its_payload() {
    let report = read_rows::<NamespaceDocument>(
        &cluster_id(),
        [StoredRow::new(
            LOWER_ROW_ID,
            format!(r#"{{"v":2,"cluster_id":"{CLUSTER_ID}","name":42}}"#),
        )],
    );

    assert!(report.accepted.is_empty());
    assert!(matches!(
        report.skipped.as_slice(),
        [skipped]
            if matches!(
                skipped.reason,
                RowSkipReason::NewerVersion { found: 2, supported: 1 }
            )
    ));
}

#[test]
fn additive_unknown_fields_do_not_hide_a_current_document() {
    let report = read_rows::<NamespaceDocument>(
        &cluster_id(),
        [StoredRow::new(
            LOWER_ROW_ID,
            format!(
                r#"{{"v":1,"cluster_id":"{CLUSTER_ID}","name":"alpha","written_by":{{"kind":"machine","machine_id":"{LOWER_ROW_ID}"}},"written_at":"2026-08-04T10:00:00Z","future":{{"enabled":true}}}}"#
            ),
        )],
    );

    assert!(report.skipped.is_empty());
    assert!(
        matches!(report.accepted.as_slice(), [accepted] if accepted.value.name.as_str() == "alpha")
    );
}

#[test]
fn missing_or_malformed_operator_provenance_is_skipped_and_surfaced() {
    let report = read_rows::<NamespaceDocument>(
        &cluster_id(),
        [
            StoredRow::new(
                LOWER_ROW_ID,
                format!(r#"{{"v":1,"cluster_id":"{CLUSTER_ID}","name":"alpha"}}"#),
            ),
            StoredRow::new(
                HIGHER_ROW_ID,
                format!(
                    r#"{{"v":1,"cluster_id":"{CLUSTER_ID}","name":"beta","written_by":{{"kind":"machine","machine_id":"not-a-ulid"}},"written_at":"not-a-time"}}"#
                ),
            ),
        ],
    );

    assert!(report.accepted.is_empty());
    assert!(report.skipped.iter().all(|skipped| matches!(
        skipped.reason,
        RowSkipReason::Malformed(MalformedDocument::InvalidPayload { .. })
    )));
}

#[test]
fn noncanonical_corrosion_reference_is_skipped_and_surfaced() {
    let document = json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "machine_id": "machine_edge_a",
        "service_id": HIGHER_ROW_ID,
        "namespace_id": HIGHER_ROW_ID,
        "ip": "10.210.20.2",
        "deploy": HIGHER_ROW_ID
    });
    let report = read_rows::<ContainerDocument>(
        &cluster_id(),
        [StoredRow::new(LOWER_ROW_ID, document.to_string())],
    );

    assert!(report.accepted.is_empty());
    assert!(matches!(
        report.skipped.as_slice(),
        [skipped]
            if matches!(
                skipped.reason,
                RowSkipReason::Malformed(MalformedDocument::InvalidPayload { .. })
            )
    ));
}

#[test]
fn ordinary_reader_rejects_a_noncanonical_ulid_row_key() {
    let document = json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "machine_id": LOWER_ROW_ID,
        "ployz_version": "0.1.0-alpha.7",
        "corrosion_version": "0.2.0-beta.0",
        "architecture": "x86_64",
        "free_disk_bytes": 80_000_000_000_u64,
        "free_memory_bytes": 4_000_000_000_u64,
        "load": "idle",
        "observed_at": "2026-08-04T10:06:00Z"
    });
    let report = read_rows::<MachineStatusDocument>(
        &cluster_id(),
        [StoredRow::new("machine_edge_a", document.to_string())],
    );

    assert!(report.accepted.is_empty());
    assert!(matches!(
        report.skipped.as_slice(),
        [skipped]
            if skipped.source.key == "machine_edge_a"
                && matches!(skipped.reason, RowSkipReason::InvalidRowId { .. })
    ));
}

#[test]
fn ordinary_reader_accepts_a_natural_container_row_key() {
    let document = json!({
        "v": 1,
        "cluster_id": CLUSTER_ID,
        "machine_id": LOWER_ROW_ID,
        "service_id": HIGHER_ROW_ID,
        "namespace_id": HIGHER_ROW_ID,
        "ip": "10.210.20.2",
        "deploy": HIGHER_ROW_ID
    });
    let report = read_rows::<ContainerDocument>(
        &cluster_id(),
        [StoredRow::new("docker-container-id", document.to_string())],
    );

    assert!(report.skipped.is_empty());
    assert!(matches!(
        report.accepted.as_slice(),
        [accepted] if accepted.source.key == "docker-container-id"
    ));
}

#[test]
fn certificate_holding_reader_rejects_keys_that_disagree_with_the_document() {
    let document = certificate_document(LOWER_ROW_ID, "api.example.com", "2026-11-02T10:08:00Z");
    let report = read_rows::<CertHoldingDocument>(
        &cluster_id(),
        [
            StoredRow::new(LOWER_ROW_ID, document.clone()),
            StoredRow::new(format!("{HIGHER_ROW_ID}:api.example.com"), document),
            StoredRow::new(
                format!("{LOWER_ROW_ID}:API.EXAMPLE.COM"),
                certificate_document(LOWER_ROW_ID, "API.EXAMPLE.COM", "2026-11-02T10:08:00Z"),
            ),
        ],
    );

    assert!(report.accepted.is_empty());
    assert_eq!(report.skipped.len(), 3);
    assert!(report.skipped.iter().all(|skipped| matches!(
        &skipped.reason,
        RowSkipReason::InvalidRowKey { expected }
            if expected == &format!("{LOWER_ROW_ID}:api.example.com")
    )));
}

#[test]
fn certificate_times_are_validated_compared_as_instants_and_canonicalized() {
    let malformed = read_rows::<CertHoldingDocument>(
        &cluster_id(),
        [StoredRow::new(
            format!("{LOWER_ROW_ID}:api.example.com"),
            certificate_document(LOWER_ROW_ID, "api.example.com", "not-a-time"),
        )],
    );
    assert!(malformed.accepted.is_empty());
    assert!(matches!(
        malformed.skipped.as_slice(),
        [skipped]
            if matches!(
                skipped.reason,
                RowSkipReason::Malformed(MalformedDocument::InvalidPayload { .. })
            )
    ));

    let accepted = read_rows::<CertHoldingDocument>(
        &cluster_id(),
        [
            StoredRow::new(
                format!("{LOWER_ROW_ID}:api.example.com"),
                certificate_document(LOWER_ROW_ID, "api.example.com", "2026-11-02T11:08:00+01:00"),
            ),
            StoredRow::new(
                format!("{HIGHER_ROW_ID}:api.example.com"),
                certificate_document(
                    HIGHER_ROW_ID,
                    "api.example.com",
                    "2026-11-02T10:08:00.000000001Z",
                ),
            ),
        ],
    );
    assert!(accepted.skipped.is_empty());
    let [offset, later] = accepted.accepted.as_slice() else {
        panic!("two certificates accepted")
    };
    assert!(offset.value.expires_at < later.value.expires_at);
    let encoded = serde_json::to_value(&offset.value).expect("certificate JSON");
    assert_eq!(
        encoded.get("expires_at"),
        Some(&json!("2026-11-02T10:08:00.000000000Z"))
    );
}

#[test]
fn lowest_canonical_ulid_wins_and_the_duplicate_remains_visible() {
    let report = read_named_rows::<NamespaceDocument>(
        &cluster_id(),
        [
            StoredRow::new(HIGHER_ROW_ID, namespace_document("alpha")),
            StoredRow::new(LOWER_ROW_ID, namespace_document("alpha")),
        ],
    );

    assert!(report.skipped.is_empty());
    assert!(matches!(
        report.accepted.as_slice(),
        [winner] if winner.id.as_str() == LOWER_ROW_ID && winner.value.name.as_str() == "alpha"
    ));
    assert!(matches!(
        report.shadows.as_slice(),
        [conflict]
            if conflict.winner.id.as_str() == LOWER_ROW_ID
                && conflict.loser.id.as_str() == HIGHER_ROW_ID
                && conflict.winner.source.document == namespace_document("alpha")
                && conflict.loser.source.document == namespace_document("alpha")
    ));
}

#[test]
fn invalid_row_id_cannot_win_or_become_a_shadow() {
    let report = read_named_rows::<NamespaceDocument>(
        &cluster_id(),
        [
            StoredRow::new("0000000000000000000000000!", namespace_document("alpha")),
            StoredRow::new(HIGHER_ROW_ID, namespace_document("alpha")),
        ],
    );

    assert!(matches!(
        report.accepted.as_slice(),
        [winner] if winner.id.as_str() == HIGHER_ROW_ID
    ));
    assert!(report.shadows.is_empty());
    assert!(matches!(
        report.skipped.as_slice(),
        [skipped] if matches!(skipped.reason, RowSkipReason::InvalidRowId { .. })
    ));
}
