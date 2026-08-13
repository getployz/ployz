use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;

use ployz_core::corrosion::{
    AutomaticHostnameMode, ClusterDocument, CorrosionDocumentVersion, CorrosionTimestamp,
    MachineDocument, MachineStorageSelection, MachineStorageSelectionReason, MachineTransport,
    MeshProvider, OperationInitiator, OperatorWriteProvenance, PeerTransport, Sha256Hex,
    StorageMode, TokenDocument, derive_builtin_wireguard_member,
};
use ployz_core::ids::{ClusterName, MachineName, PeerName, TokenName};
use ployz_core::install::{
    AbsoluteInstallPath, ExactPloyzVersion, InstallArtifactSource, InstallArtifactSpec,
    InstallArtifactVersion, InstallSha256Digest,
};
use ployz_core::join::{
    AdvertisedJoinDoorEndpoints, JOIN_DOOR_PORT, JOIN_RESET_COMMAND, JoinAcceptanceValidationError,
    JoinAdmissionValidationError, JoinArrival, JoinArrivalDisposition, JoinBlob,
    JoinDoorCertFingerprint, JoinDoorPrivateKeyPem, JoinDoorRefusal, JoinMachineSubstrate,
    JoinMemberRequest, JoinRepairCommand, JoinStorageChoice, JoinStorageFacts, JoinTokenProof,
    JoinTokenSecret, JoinTokenTtlSeconds, MACHINE_ENDPOINT_SET_COMMAND, MAX_JOIN_DOOR_ENDPOINTS,
    MachineEndpointSetRefusal, MachineEndpointSetRequest, MachineJoinRequest, PeerJoinRequest,
    TokenCreateRefusal, TokenListRequest, TokenListScope, advertise_join_door_endpoints,
    classify_join_arrival, prepare_machine_admission, prepare_peer_admission, select_join_storage,
    set_machine_endpoint, token_list_reply, validate_join_token,
};
use ployz_core::machine::MachineLifecycle;
use ployz_core::network::{MachineEndpointSubnet, MachineEndpointSupernet, WireGuardPublicKey};
use ployz_core::{
    JOIN_ROUTE, KnownApiFeature, MACHINE_ENDPOINT_ROUTE_PREFIX, TOKEN_CREATE_ROUTE,
    TOKEN_LIST_ROUTE, TOKEN_REVOKE_ROUTE_PREFIX, V2Method, V2Route, token_revoke_route,
};

const CLUSTER: &str = "main";
const MACHINE: &str = "edge-a";
const TOKEN: &str = "bootstrap";

fn timestamp(value: &str) -> CorrosionTimestamp {
    CorrosionTimestamp::try_new(value).expect("fixture timestamp")
}

fn provenance() -> OperatorWriteProvenance {
    OperatorWriteProvenance {
        written_by: OperationInitiator::Machine {
            machine_id: MachineName::try_new(MACHINE).expect("fixture id"),
        },
        written_at: timestamp("2026-08-05T12:00:00Z"),
    }
}

fn token_provenance() -> OperatorWriteProvenance {
    OperatorWriteProvenance {
        written_by: OperationInitiator::ApiToken {
            token_id: TokenName::try_new(TOKEN).expect("fixture token id"),
        },
        written_at: timestamp("2026-08-05T12:00:00Z"),
    }
}

fn token_document(secret: &JoinTokenSecret, expires_at: &str) -> TokenDocument {
    TokenDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: ClusterName::try_new(CLUSTER).expect("fixture cluster id"),
        provenance: provenance(),
        secret_sha256: Sha256Hex::try_new(secret.sha256().as_str()).expect("fixture digest"),
        created_at: timestamp("2026-08-05T10:00:00Z"),
        expires_at: timestamp(expires_at),
    }
}

fn cluster() -> ClusterDocument {
    ClusterDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: ClusterName::try_new(CLUSTER).expect("fixture id"),
        provenance: provenance(),
        name: "acme".to_owned(),
        storage_default: StorageMode::Plain,
        hostname_mode: AutomaticHostnameMode::Disabled,
        prefix: MachineEndpointSupernet::try_new("10.210.0.0/16").expect("fixture prefix"),
        provider: MeshProvider::BuiltinWireguard,
        acme_directory_url: "https://acme.invalid/directory".to_owned(),
        acme_contact: None,
    }
}

fn machine(endpoint: Option<SocketAddr>) -> MachineDocument {
    MachineDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: ClusterName::try_new(CLUSTER).expect("fixture id"),
        provenance: provenance(),
        name: MachineName::try_new("door-one").expect("fixture name"),
        lifecycle: MachineLifecycle::Active,
        transport: MachineTransport::Wireguard {
            pubkey: WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                .expect("fixture key"),
            addr_v6: Ipv6Addr::from_str("fd00::1").expect("fixture address"),
            endpoint,
            subnet_v4: MachineEndpointSubnet::try_new("10.210.20.0/24").expect("fixture subnet"),
        },
        storage: MachineStorageSelection {
            mode: StorageMode::Plain,
            reason: MachineStorageSelectionReason::Default,
        },
    }
}

fn install_artifact(name: &str, install_path: &str) -> InstallArtifactSpec {
    InstallArtifactSpec {
        version: InstallArtifactVersion::try_new("0.1.0").expect("artifact version"),
        source: InstallArtifactSource::try_new(format!("/tmp/{name}")).expect("artifact source"),
        sha256: InstallSha256Digest::try_new("ab".repeat(32)).expect("artifact digest"),
        install_path: AbsoluteInstallPath::try_new(install_path).expect("install path"),
    }
}

fn required_install_artifacts() -> Vec<InstallArtifactSpec> {
    [
        "/usr/local/bin/ployzd",
        "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
        "/usr/local/bin/ployz-ebpf-ctl",
        "/usr/local/bin/corrosion",
        "/usr/local/lib/ployz/corrosion-schema-v1.sql",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, install_path)| install_artifact(&format!("artifact-{index}"), install_path))
    .collect()
}

#[test]
fn opaque_join_blob_round_trips_canonically_and_redacts_secrets() {
    let secret = JoinTokenSecret::try_from_bytes(std::array::from_fn(|index| index as u8));
    let fingerprint = JoinDoorCertFingerprint::try_new("ab".repeat(32)).expect("fingerprint");
    let blob = JoinBlob::try_new(
        TokenName::try_new(TOKEN).expect("token id"),
        secret.clone(),
        fingerprint,
        vec![
            SocketAddr::from_str("[2001:db8::10]:2021").expect("v6 door"),
            SocketAddr::from_str("198.51.100.10:2021").expect("v4 door"),
        ],
    )
    .expect("join blob");

    let encoded = blob.expose().to_owned();
    assert!(encoded.starts_with("pzjoin_"));
    assert!(!encoded.contains('='), "URL-safe base64 is unpadded");
    assert_eq!(JoinBlob::try_parse(&encoded).expect("canonical blob"), blob);
    assert_eq!(
        secret.sha256().as_str(),
        "630dcd2966c4336691125448bbb25b4ff412a49c732db2c8abc1b8581bd710dd"
    );
    assert!(!format!("{secret:?}").contains(&secret.expose_base64()));
    assert!(!format!("{blob:?}").contains(&encoded));
    assert_eq!(encoded.parse::<JoinBlob>().expect("FromStr blob"), blob);

    assert!(JoinBlob::try_parse("pzjoin_not+url/base64").is_err());
    assert!(JoinBlob::try_parse(&(encoded + "=")).is_err());
}

#[test]
fn advertised_doors_reuse_endpoint_ips_but_never_wireguard_udp_ports() {
    let endpoints = advertise_join_door_endpoints([
        &machine(Some(
            SocketAddr::from_str("198.51.100.10:51820").expect("v4 endpoint"),
        )),
        &machine(Some(
            SocketAddr::from_str("[2001:db8::10]:51821").expect("v6 endpoint"),
        )),
        &machine(None),
    ])
    .expect("at least one public endpoint");

    assert_eq!(JOIN_DOOR_PORT, 2021);
    assert_eq!(
        endpoints.as_slice(),
        &[
            SocketAddr::new(Ipv4Addr::new(198, 51, 100, 10).into(), JOIN_DOOR_PORT),
            SocketAddr::new(
                Ipv6Addr::from_str("2001:db8::10")
                    .expect("v6 address")
                    .into(),
                JOIN_DOOR_PORT,
            ),
        ]
    );
}

#[test]
fn token_creation_without_an_advertised_door_names_the_exact_repair() {
    assert_eq!(
        advertise_join_door_endpoints([&machine(None)]),
        Err(TokenCreateRefusal::NoAdvertisedDoorEndpoint {
            repair_command: MACHINE_ENDPOINT_SET_COMMAND.to_owned(),
        })
    );
    assert_eq!(
        MACHINE_ENDPOINT_SET_COMMAND,
        "ployz machine endpoint set <machine> <ip:wireguard-port>"
    );
}

#[test]
fn token_name_conflict_is_a_typed_client_refusal() {
    let name = TokenName::try_new("bootstrap").expect("token name");
    assert_eq!(
        serde_json::to_value(TokenCreateRefusal::NameConflict { name })
            .expect("refusal serializes"),
        serde_json::json!({"kind": "name_conflict", "name": "bootstrap"})
    );
}

#[test]
fn token_ttl_is_bounded() {
    assert_eq!(JoinTokenTtlSeconds::default_v1().get(), 24 * 60 * 60);
    assert!(JoinTokenTtlSeconds::try_new(JoinTokenTtlSeconds::MIN - 1).is_err());
    assert!(JoinTokenTtlSeconds::try_new(JoinTokenTtlSeconds::MIN).is_ok());
    assert!(JoinTokenTtlSeconds::try_new(JoinTokenTtlSeconds::MAX).is_ok());
    assert!(JoinTokenTtlSeconds::try_new(JoinTokenTtlSeconds::MAX + 1).is_err());
}

#[test]
fn token_validation_is_o1_by_embedded_id_and_refuses_expiry_or_wrong_secret() {
    let token_id = TokenName::try_new(TOKEN).expect("token id");
    let secret = JoinTokenSecret::try_from_bytes([7; 32]);
    let proof = JoinTokenProof {
        token_id: token_id.clone(),
        secret: secret.clone(),
    };
    let document = token_document(&secret, "2026-08-06T10:00:00Z");

    assert_eq!(
        validate_join_token(
            &proof,
            Some((&token_id, &document)),
            timestamp("2026-08-05T12:00:00Z"),
        )
        .expect("live token"),
        OperationInitiator::ApiToken {
            token_id: token_id.clone(),
        }
    );
    assert!(matches!(
        validate_join_token(
            &proof,
            Some((&token_id, &document)),
            timestamp("2026-08-06T10:00:00Z"),
        ),
        Err(ployz_core::join::JoinDoorRefusal::TokenExpired { .. })
    ));
    let wrong = JoinTokenProof {
        token_id: token_id.clone(),
        secret: JoinTokenSecret::try_from_bytes([8; 32]),
    };
    assert!(matches!(
        validate_join_token(
            &wrong,
            Some((&token_id, &document)),
            timestamp("2026-08-05T12:00:00Z"),
        ),
        Err(ployz_core::join::JoinDoorRefusal::TokenSecretMismatch { .. })
    ));
    assert!(matches!(
        validate_join_token(&proof, None, timestamp("2026-08-05T12:00:00Z")),
        Err(ployz_core::join::JoinDoorRefusal::TokenNotFound { .. })
    ));
}

#[test]
fn token_list_hides_expired_rows_unless_all_is_requested() {
    let secret = JoinTokenSecret::try_from_bytes([9; 32]);
    let expired = TokenName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAY").expect("expired id");
    let live = TokenName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAZ").expect("live id");
    let rows = || {
        vec![
            (
                expired.clone(),
                token_document(&secret, "2026-08-05T11:00:00Z"),
            ),
            (
                live.clone(),
                token_document(&secret, "2026-08-06T11:00:00Z"),
            ),
        ]
    };

    let visible = token_list_reply(
        TokenListRequest {
            scope: TokenListScope::Live,
        },
        timestamp("2026-08-05T12:00:00Z"),
        rows(),
    );
    let [visible_token] = visible.tokens.as_slice() else {
        panic!("the live-token view must contain exactly one token")
    };
    assert_eq!(visible_token.token_id, live);
    assert_eq!(
        token_list_reply(
            TokenListRequest {
                scope: TokenListScope::All,
            },
            timestamp("2026-08-05T12:00:00Z"),
            rows(),
        )
        .tokens
        .len(),
        2
    );
}

#[test]
fn join_storage_and_arrival_policy_are_explicit_and_idempotent() {
    assert_eq!(
        select_join_storage(
            StorageMode::Zfs,
            JoinStorageChoice::Automatic,
            JoinStorageFacts {
                imported_zfs_pool: true,
                total_memory_bytes: 1024,
            },
        ),
        MachineStorageSelection {
            mode: StorageMode::Plain,
            reason: MachineStorageSelectionReason::Ineligible {
                reason: ployz_core::corrosion::MachineStorageIneligibleReason::LowRam,
            },
        }
    );

    let requested = JoinDoorCertFingerprint::try_new("ab".repeat(32)).expect("fingerprint");
    assert_eq!(
        classify_join_arrival(&requested, JoinArrival::Clean),
        Ok(JoinArrivalDisposition::Join)
    );
    assert_eq!(
        classify_join_arrival(
            &requested,
            JoinArrival::Partial {
                persisted_door_fingerprint: requested.clone(),
            }
        ),
        Ok(JoinArrivalDisposition::Resume)
    );
    assert_eq!(
        classify_join_arrival(
            &requested,
            JoinArrival::Complete {
                persisted_door_fingerprint: requested.clone(),
            }
        ),
        Ok(JoinArrivalDisposition::NoOp)
    );
    let foreign = JoinDoorCertFingerprint::try_new("cd".repeat(32)).expect("foreign fingerprint");
    let refusal = classify_join_arrival(
        &requested,
        JoinArrival::Partial {
            persisted_door_fingerprint: foreign,
        },
    )
    .expect_err("foreign state refuses");
    assert_eq!(refusal.repair_command(), JoinRepairCommand::ResetMachine);
    assert_eq!(refusal.repair_command().as_str(), JOIN_RESET_COMMAND);
    assert!(matches!(
        classify_join_arrival(&requested, JoinArrival::Founding),
        Err(ployz_core::join::JoinArrivalRefusal::FoundingState { .. })
    ));
}

#[test]
fn machine_and_peer_admission_are_structurally_separate() {
    let machine_request = JoinMemberRequest::Machine {
        request: MachineJoinRequest {
            name: MachineName::try_new(MACHINE).expect("machine name"),
            public_key: WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                .expect("public key"),
            endpoint: None,
            storage_choice: JoinStorageChoice::Automatic,
            storage_facts: JoinStorageFacts {
                imported_zfs_pool: false,
                total_memory_bytes: 1024,
            },
        },
    };
    let peer_request = JoinMemberRequest::Peer {
        request: PeerJoinRequest {
            name: PeerName::try_new("operator-laptop").expect("peer name"),
            public_key: WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                .expect("public key"),
            endpoint: None,
        },
    };

    let machine_json = serde_json::to_value(machine_request).expect("machine wire");
    let peer_json = serde_json::to_value(peer_request).expect("peer wire");
    assert!(machine_json.to_string().contains("storage_facts"));
    assert!(!peer_json.to_string().contains("storage"));
    assert!(!peer_json.to_string().contains("subnet_v4"));
}

#[test]
fn public_join_requests_tolerate_additive_fields() {
    let mut machine = serde_json::to_value(MachineJoinRequest {
        name: MachineName::try_new(MACHINE).expect("machine name"),
        public_key: WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .expect("public key"),
        endpoint: None,
        storage_choice: JoinStorageChoice::Automatic,
        storage_facts: JoinStorageFacts {
            imported_zfs_pool: false,
            total_memory_bytes: 1024,
        },
    })
    .expect("machine request serializes");
    machine
        .as_object_mut()
        .expect("machine request is an object")
        .insert("future_machine_field".to_owned(), serde_json::json!(true));
    serde_json::from_value::<MachineJoinRequest>(machine)
        .expect("machine request ignores additive fields");

    let mut peer = serde_json::to_value(PeerJoinRequest {
        name: PeerName::try_new("operator-laptop").expect("peer name"),
        public_key: WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .expect("public key"),
        endpoint: None,
    })
    .expect("peer request serializes");
    peer.as_object_mut()
        .expect("peer request is an object")
        .insert("future_peer_field".to_owned(), serde_json::json!(true));
    serde_json::from_value::<PeerJoinRequest>(peer).expect("peer request ignores additive fields");
}

#[test]
fn every_join_door_refusal_names_a_repair_command() {
    let token_id = TokenName::try_new(TOKEN).expect("token id");
    let refusals = [
        JoinDoorRefusal::TokenNotFound {
            token_id: token_id.clone(),
        },
        JoinDoorRefusal::TokenExpired {
            token_id: token_id.clone(),
            expires_at: timestamp("2026-08-05T12:00:00Z"),
        },
        JoinDoorRefusal::TokenSecretMismatch { token_id },
        JoinDoorRefusal::InvalidAdmission {
            reason: JoinAdmissionValidationError::EndpointPortZero,
        },
        JoinDoorRefusal::MachineNameConflict {
            name: "edge-a".to_owned(),
        },
        JoinDoorRefusal::PeerNameConflict {
            name: "operator-laptop".to_owned(),
        },
        JoinDoorRefusal::IdentityConflict,
        JoinDoorRefusal::NoReachableSeed,
        JoinDoorRefusal::EndpointSubnetExhausted,
    ];

    for refusal in refusals {
        assert!(
            refusal.to_string().contains("ployz "),
            "refusal omitted its repair command: {refusal}"
        );
    }

    assert_eq!(
        JoinDoorRefusal::MachineNameConflict {
            name: "edge-a".to_owned(),
        }
        .to_string(),
        "machine name \"edge-a\" is already claimed; run `ployz machine rm edge-a` before joining again"
    );
    assert_eq!(
        serde_json::to_value(JoinDoorRefusal::MachineNameConflict {
            name: "edge-a".to_owned(),
        })
        .expect("machine name conflict serializes"),
        serde_json::json!({"kind":"name_conflict","name":"edge-a"}),
        "the existing machine-conflict wire tag remains compatible"
    );
    assert_eq!(
        JoinDoorRefusal::PeerNameConflict {
            name: "operator-laptop".to_owned(),
        }
        .to_string(),
        "peer name \"operator-laptop\" is already claimed; run `ployz peer rm operator-laptop` before joining again"
    );
}

#[test]
fn admission_derives_addresses_and_keeps_peer_rows_subnet_free() {
    let cluster = cluster();
    let key = WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
        .expect("public key");
    let machine_request = MachineJoinRequest {
        name: MachineName::try_new(MACHINE).expect("machine name"),
        public_key: key.clone(),
        endpoint: None,
        storage_choice: JoinStorageChoice::Automatic,
        storage_facts: JoinStorageFacts {
            imported_zfs_pool: false,
            total_memory_bytes: 1024,
        },
    };
    let accepted_machine = prepare_machine_admission(
        &cluster,
        machine_request,
        MachineEndpointSubnet::try_new("10.210.44.0/24").expect("subnet"),
        token_provenance(),
    )
    .expect("valid machine admission");
    let MachineTransport::Wireguard {
        pubkey,
        addr_v6,
        subnet_v4,
        ..
    } = &accepted_machine.transport
    else {
        panic!("builtin admission creates WireGuard transport")
    };
    assert_eq!(
        *addr_v6,
        derive_builtin_wireguard_member(&cluster.cluster_id, pubkey)
            .bind_address()
            .get()
    );
    assert_eq!(subnet_v4.as_string(), "10.210.44.0/24");
    assert!(matches!(
        accepted_machine.provenance.written_by,
        OperationInitiator::ApiToken { .. }
    ));

    let accepted_peer = prepare_peer_admission(
        &cluster,
        PeerJoinRequest {
            name: PeerName::try_new("operator-laptop").expect("peer name"),
            public_key: key,
            endpoint: None,
        },
        token_provenance(),
    )
    .expect("valid peer admission");
    assert!(matches!(
        accepted_peer.transport,
        PeerTransport::Wireguard { .. }
    ));
    assert!(
        !serde_json::to_string(&accepted_peer)
            .expect("peer row serializes")
            .contains("subnet_v4")
    );
}

#[test]
fn endpoint_set_refuses_port_zero_without_mutating_the_machine() {
    let machine_id = MachineName::try_new(MACHINE).expect("machine id");
    let machine_name = MachineName::try_new("door-one").expect("machine name");
    let request = MachineEndpointSetRequest {
        machine_name: machine_name.clone(),
        endpoint: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
    };

    assert_eq!(
        set_machine_endpoint(machine_id, &request, machine(None)),
        Err(MachineEndpointSetRefusal::EndpointPortZero { machine_name })
    );
}

#[test]
fn machine_substrate_requires_exact_destinations_and_ignores_extras() {
    let version = || ExactPloyzVersion::try_new("0.1.0").expect("exact version");
    let valid = JoinMachineSubstrate::try_new(version(), "0.3.1", required_install_artifacts())
        .expect("complete substrate");
    assert_eq!(valid.ployz_version().as_str(), "0.1.0");
    assert_eq!(valid.corrosion_version(), "0.3.1");
    assert_eq!(valid.artifacts().len(), 5);
    let mut incomplete_wire = serde_json::to_value(&valid).expect("substrate serializes");
    let serde_json::Value::Object(ref mut fields) = incomplete_wire else {
        panic!("substrate serializes as an object")
    };
    let Some(serde_json::Value::Array(artifacts)) = fields.get_mut("artifacts") else {
        panic!("substrate carries artifacts")
    };
    artifacts.pop();
    assert!(
        serde_json::from_value::<JoinMachineSubstrate>(incomplete_wire).is_err(),
        "deserialization must preserve constructor validation"
    );

    let mut missing = required_install_artifacts();
    missing.remove(0);
    assert_eq!(
        JoinMachineSubstrate::try_new(version(), "0.3.1", missing),
        Err(JoinAcceptanceValidationError::MissingInstallArtifact {
            install_path: "/usr/local/bin/ployzd".to_owned(),
        })
    );

    let mut duplicate = required_install_artifacts();
    duplicate.push(install_artifact("duplicate", "/usr/local/bin/ployzd"));
    assert_eq!(
        JoinMachineSubstrate::try_new(version(), "0.3.1", duplicate),
        Err(JoinAcceptanceValidationError::DuplicateInstallArtifact {
            install_path: "/usr/local/bin/ployzd".to_owned(),
        })
    );

    let mut legacy = required_install_artifacts();
    legacy.push(install_artifact(
        "legacy-railpack",
        "/usr/local/lib/ployz/railpack/v0.31.0/railpack",
    ));
    let legacy = serde_json::from_value::<JoinMachineSubstrate>(serde_json::json!({
        "ployz_version": version(),
        "corrosion_version": "0.3.1",
        "artifacts": legacy,
    }))
    .expect("extra artifacts in legacy JSON are inert");
    assert_eq!(legacy.artifacts().len(), 6);
    let executable = legacy.artifacts_by_kind().collect::<Vec<_>>();
    assert_eq!(executable.len(), 5);
    assert!(executable.into_iter().all(|(_, artifact)| {
        artifact.install_path.as_str() != "/usr/local/lib/ployz/railpack/v0.31.0/railpack"
    }));
}

#[test]
fn secret_door_material_is_redacted() {
    let key = JoinDoorPrivateKeyPem::try_new(
        "-----BEGIN PRIVATE KEY-----\nsecret\n-----END PRIVATE KEY-----\n",
    )
    .expect("private key");
    assert!(!format!("{key:?}").contains("secret"));
}

#[test]
fn join_routes_are_centralized_and_token_principals_are_join_only() {
    assert_eq!(TOKEN_CREATE_ROUTE, "/tokens/create");
    assert_eq!(TOKEN_LIST_ROUTE, "/tokens/list");
    assert_eq!(TOKEN_REVOKE_ROUTE_PREFIX, "/tokens/revoke");
    assert_eq!(MACHINE_ENDPOINT_ROUTE_PREFIX, "/machines/endpoint");
    assert_eq!(JOIN_ROUTE, "/join");
    assert_eq!(V2Route::parse(JOIN_ROUTE), Some(V2Route::Join));
    assert_eq!(V2Route::Join.method(), V2Method::Post);
    assert_eq!(V2Route::Join.feature(), KnownApiFeature::JoinDoor);

    let token_principal = OperationInitiator::ApiToken {
        token_id: TokenName::try_new(TOKEN).expect("token id"),
    };
    assert!(V2Route::Join.accepts_principal(&token_principal));
    assert!(!V2Route::TokenCreate.accepts_principal(&token_principal));
}

#[test]
fn token_revoke_route_round_trips_the_typed_token_id() {
    let token_id = TokenName::try_new(TOKEN).expect("token id");
    let path = token_revoke_route(&token_id);

    assert_eq!(path, format!("{TOKEN_REVOKE_ROUTE_PREFIX}/{TOKEN}"));
    assert_eq!(
        V2Route::parse(&path),
        Some(V2Route::TokenRevoke(token_id.clone()))
    );
    assert_eq!(V2Route::TokenRevoke(token_id).path(), path);
    assert_eq!(V2Route::parse(TOKEN_REVOKE_ROUTE_PREFIX), None);
}

#[test]
fn operator_authority_routes_accept_peers_and_refuse_machines() {
    let machine_principal = OperationInitiator::Machine {
        machine_id: MachineName::try_new(MACHINE).expect("machine id"),
    };
    let peer_principal = OperationInitiator::Peer {
        peer_id: PeerName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAY").expect("peer id"),
    };

    for route in [
        V2Route::TokenCreate,
        V2Route::TokenList,
        V2Route::TokenRevoke(TokenName::try_new(TOKEN).expect("token id")),
        V2Route::MachineEndpointSet,
    ] {
        assert!(route.accepts_principal(&peer_principal));
        assert!(!route.accepts_principal(&machine_principal));
    }
}

// Keep the validated endpoint wrapper in this public-boundary test so callers
// cannot accidentally construct an empty blob after token creation succeeds.
#[test]
fn advertised_endpoint_set_is_nonempty() {
    assert!(AdvertisedJoinDoorEndpoints::try_new(Vec::new()).is_err());
    let too_many = (0..=MAX_JOIN_DOOR_ENDPOINTS)
        .map(|index| SocketAddr::new(Ipv6Addr::from(index as u128).into(), JOIN_DOOR_PORT))
        .collect();
    assert!(AdvertisedJoinDoorEndpoints::try_new(too_many).is_err());
}

#[test]
fn accepted_contract_fixtures_can_carry_the_cluster_document() {
    assert_eq!(cluster().cluster_id.as_str(), CLUSTER);
}

#[cfg(feature = "ts")]
#[test]
fn public_join_contracts_are_exported_to_typescript() {
    let generated = ployz_core::api_typescript();
    for contract in [
        "type TokenCreateRequest",
        "type TokenCreateReply",
        "type TokenCreateRefusal",
        "type TokenListReply",
        "type TokenRevokeRequest",
        "type TokenRevokeReply",
        "type TokenRevokeRefusal",
        "type MachineEndpointSetRequest",
        "type MachineEndpointSetReply",
        "type MachineEndpointSetRefusal",
        "type JoinAdmissionRequest",
        "type JoinAdmissionReply",
        "type JoinMachineSubstrate",
    ] {
        assert!(generated.contains(contract), "missing {contract}");
    }
    for unused_wrapper in [
        "type TokenCreateResponse",
        "type TokenRevokeResponse",
        "type MachineEndpointSetResponse",
    ] {
        assert!(
            !generated.contains(unused_wrapper),
            "unused response wrapper leaked into the public contract: {unused_wrapper}"
        );
    }
}
