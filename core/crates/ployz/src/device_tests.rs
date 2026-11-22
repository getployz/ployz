use ployz_core::authority::PasskeyAccount;
use ployz_core::authority::trusted_key::PasskeyAlgorithm;
use ployz_core::{MachineId, MachineName, OpaquePayload, RpcRequestBody, RpcResponse};
use serde_json::json;
use tonic::{Request, Response};

use super::*;
use crate::global_fixtures::machine;

fn key(first: u8) -> DeviceKey {
    DeviceKey::from_bytes(std::array::from_fn(|index| first + index as u8))
}

fn id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

fn passkey_key(seed: u8) -> ployz_core::authority::PasskeyPublicKey {
    let key = iroh::SecretKey::from_bytes(&[seed; 32]).public();
    ployz_core::authority::PasskeyPublicKey::parse(PasskeyAlgorithm::Eddsa, key.as_bytes()).unwrap()
}

fn waiting(public_key: ployz_core::authority::PasskeyPublicKey) -> ployz_core::authority::Passkey {
    ployz_core::authority::Passkey {
        credential_id: serde_json::from_value(json!("AQID")).unwrap(),
        public_key,
        account: PasskeyAccount::parse("ada@example.com").unwrap(),
        relying_party: ployz_core::authority::trusted_key::RelyingParty::of(
            &"https://ployz.cloud".parse().unwrap(),
        )
        .unwrap(),
    }
}

#[tokio::test]
async fn automatic_join_approval_refuses_clouds_substituted_recipient() {
    use ployz_core::{
        PendingJoins, RegisterRequest, SealingPublicKey,
        authority::{JoinApproval, Signer, Timestamp, join::JoinAllocation},
    };
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.yaml");
    super::key::store(&super::key::path(&config), &[1; 32]).unwrap();
    let expected_machine = machine('a', "joiner");
    let expected = RegisterRequest {
        join_approval: None,
        join_confirmation: None,
        machine_id: expected_machine.id,
        assigned_subnet: Some(expected_machine.subnet),
        initial_policy: Default::default(),
        name: expected_machine.name.clone(),
        storage: ployz_core::StorageChoice::None,
        public_key: expected_machine.public_key,
        public_ip: expected_machine.public_ip,
        advertised_endpoints: expected_machine.advertised_endpoints.clone(),
        runtime: Default::default(),
        capabilities: Vec::new(),
        first_key: None,
    };
    let mut allocation = JoinAllocation::from(&expected_machine);
    allocation.public_key = ployz_core::WireGuardPublicKey([9; 32]);
    let signer = Signer::from_seed(&[1; 32]);
    let issued_at = Timestamp::now();
    let body = JoinApproval {
        cluster: SealingPublicKey::from_bytes([2; 32]),
        machine: allocation,
        network: "10.210.0.0/16".parse().unwrap(),
        sealing_recipient: ployz_core::WireGuardPublicKey([9; 32]),
        devices: vec![signer.public().into()],
        bootstrap: Vec::new(),
        passkeys: Vec::new(),
        admitting_machines: vec![id('b')],
        issued_at,
        expires_at: issued_at.plus(900_000),
        signer: signer.public().into(),
        proposal: None,
        grants: Vec::new(),
    };
    let (mut client, server) =
        crate::connect::test_support::rpc_client(move |request: Request<OpaquePayload>| {
            let body = body.clone();
            async move {
                assert!(
                    matches!(
                        request.into_inner().decode_request().unwrap().body,
                        RpcRequestBody::PendingJoins(_)
                    ),
                    "a substituted key must never reach ApproveJoin"
                );
                Ok(Response::new(
                    RpcResponse::from(PendingJoins {
                        joins: vec![body],
                        first_keys: Default::default(),
                    })
                    .encode()
                    .unwrap(),
                ))
            }
        })
        .await;
    let error = sign_waiting_join(
        &mut client,
        &config,
        &expected,
        JoinEntry::Relayed { pin: None },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("does not match"));
    server.abort();
}

fn relayed_join(
    joiner: &ployz_core::Machine,
    devices: Vec<DeviceKey>,
    cluster: ployz_core::SealingPublicKey,
) -> (
    ployz_core::RegisterRequest,
    ployz_core::authority::JoinApproval,
) {
    use ployz_core::authority::{JoinApproval, Timestamp, join::JoinAllocation};
    let expected = ployz_core::RegisterRequest {
        join_approval: None,
        join_confirmation: None,
        machine_id: joiner.id,
        assigned_subnet: Some(joiner.subnet),
        initial_policy: Default::default(),
        name: joiner.name.clone(),
        storage: ployz_core::StorageChoice::None,
        public_key: joiner.public_key,
        public_ip: joiner.public_ip,
        advertised_endpoints: joiner.advertised_endpoints.clone(),
        runtime: Default::default(),
        capabilities: Vec::new(),
        first_key: None,
    };
    let issued_at = Timestamp::now();
    let body = JoinApproval {
        cluster,
        machine: JoinAllocation::from(joiner),
        network: "10.210.0.0/16".parse().unwrap(),
        sealing_recipient: joiner.public_key,
        devices,
        bootstrap: Vec::new(),
        passkeys: vec![waiting(passkey_key(8))],
        admitting_machines: vec![id('b')],
        issued_at,
        expires_at: issued_at.plus(900_000),
        signer: DeviceKey::from_bytes([0; 32]).into(),
        proposal: None,
        grants: Vec::new(),
    };
    (expected, body)
}

fn pin_cluster(config: &std::path::Path, cluster: ployz_core::SealingPublicKey) {
    pin_cluster_for(config, "org", cluster);
}

fn pin_cluster_for(
    config: &std::path::Path,
    organization: &str,
    cluster: ployz_core::SealingPublicKey,
) {
    crate::sealing_pin::Pins::beside(config)
        .set(
            organization,
            crate::sealing_pin::Pin {
                public_key: cluster,
                server: MachineName::parse("entry").unwrap(),
            },
        )
        .unwrap();
}

/// A join relayed through Cloud whose body names `devices`, while this computer is signed
/// in to Organization "org", answering each approval the CLI sent.
async fn sign_relayed(
    config: &std::path::Path,
    devices: Vec<DeviceKey>,
    cluster: ployz_core::SealingPublicKey,
) -> (
    Result<bool, Error>,
    Vec<ployz_core::authority::Signed<ployz_core::authority::JoinApproval>>,
) {
    use std::sync::{Arc, Mutex};
    let joiner = machine('a', "joiner");
    let (expected, body) = relayed_join(&joiner, devices, cluster);
    let sent = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&sent);
    let (mut client, server) =
        crate::connect::test_support::rpc_client(move |request: Request<OpaquePayload>| {
            let body = body.clone();
            let seen = Arc::clone(&seen);
            async move {
                #[expect(
                    clippy::wildcard_enum_match_arm,
                    reason = "the fixture answers only what signing a join sends"
                )]
                let response = match request.into_inner().decode_request().unwrap().body {
                    RpcRequestBody::PendingJoins(_) => {
                        RpcResponse::from(ployz_core::PendingJoins {
                            joins: vec![body],
                            first_keys: Default::default(),
                        })
                    }
                    RpcRequestBody::ApproveJoin(request) => {
                        seen.lock().unwrap().push(request.approval);
                        RpcResponse::from(ployz_core::JoinApproved {})
                    }
                    other => panic!("unexpected request {other:?}"),
                };
                Ok(Response::new(response.encode().unwrap()))
            }
        })
        .await;
    let pin = crate::sealing_pin::Pins::beside(config).get("org").unwrap();
    let signed =
        sign_waiting_join(&mut client, config, &expected, JoinEntry::Relayed { pin }).await;
    server.abort();
    let sent = sent.lock().unwrap().clone();
    (signed, sent)
}

#[tokio::test]
async fn a_relayed_join_to_a_cluster_this_computer_never_pinned_is_refused_unsigned() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.yaml");
    super::key::store(&super::key::path(&config), &[1; 32]).unwrap();
    let own = super::key::public(&[1; 32]);
    pin_cluster(&config, ployz_core::SealingPublicKey::from_bytes([3; 32]));

    let (signed, sent) = sign_relayed(
        &config,
        vec![own],
        ployz_core::SealingPublicKey::from_bytes([2; 32]),
    )
    .await;

    let refused = signed.unwrap_err();
    assert_eq!(
        refused.hints(),
        vec![Hint::Next(crate::sealing_pin::PIN_COMMAND.to_owned())]
    );
    assert!(
        sent.is_empty(),
        "nothing may be signed for an unknown Cluster"
    );
}

/// One Device can be trusted in two Organizations. A join relayed while it is signed in to
/// one is vouched for only by that Organization's pin, so Cloud can't steer it into signing
/// a join to the other's Cluster.
#[tokio::test]
async fn a_relayed_join_to_another_organizations_cluster_is_refused_unsigned() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.yaml");
    super::key::store(&super::key::path(&config), &[1; 32]).unwrap();
    let own = super::key::public(&[1; 32]);
    let other = ployz_core::SealingPublicKey::from_bytes([2; 32]);
    pin_cluster(&config, ployz_core::SealingPublicKey::from_bytes([3; 32]));
    pin_cluster_for(&config, "other-org", other);

    let (signed, sent) = sign_relayed(&config, vec![own], other).await;

    assert!(signed.is_err());
    assert!(
        sent.is_empty(),
        "nothing may be signed for another Organization"
    );
}

#[tokio::test]
async fn a_relayed_join_that_leaves_out_this_device_is_refused_unsigned() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.yaml");
    super::key::store(&super::key::path(&config), &[1; 32]).unwrap();
    let cluster = ployz_core::SealingPublicKey::from_bytes([2; 32]);
    pin_cluster(&config, cluster);

    let (signed, sent) = sign_relayed(&config, vec![key(40)], cluster).await;

    assert!(signed.is_err());
    assert!(sent.is_empty());
}

#[test]
fn join_summary_names_the_server_subnet_and_endpoints_before_signing() {
    use ployz_core::{
        AdvertisedEndpoint, SealingPublicKey,
        authority::{JoinApproval, Timestamp, join::JoinAllocation},
    };
    let mut joiner = machine('a', "joiner");
    joiner.subnet = "10.210.7.0/24".parse().unwrap();
    joiner.advertised_endpoints = vec![AdvertisedEndpoint("[2001:db8::7]:51820".parse().unwrap())];
    let issued_at = Timestamp::now();
    let body = JoinApproval {
        cluster: SealingPublicKey::from_bytes([2; 32]),
        machine: JoinAllocation::from(&joiner),
        network: "10.210.0.0/16".parse().unwrap(),
        sealing_recipient: joiner.public_key,
        devices: vec![key(1)],
        bootstrap: Vec::new(),
        passkeys: Vec::new(),
        admitting_machines: vec![id('b')],
        issued_at,
        expires_at: issued_at.plus(900_000),
        signer: key(1).into(),
        proposal: None,
        grants: Vec::new(),
    };
    assert_eq!(
        join_summary(&body),
        format!(
            "Approving Server joiner on 10.210.7.0/24 with endpoints [2001:db8::7]:51820 (join code {}). It will trust the same devices, passkeys and CI or agent keys as the Server that admits it, and none that Server removed.",
            body.code()
        )
    );
}

/// A Server whose install command named a key takes only that key's approval, so this
/// computer refuses to spend the join under its own device, and says which key to use.
#[test]
fn a_join_waiting_for_another_key_is_not_signed_with_this_device() {
    use ployz_core::first_key::FirstKeyHint;
    let own: DeviceKey = ployz_core::authority::Signer::from_seed(&[1; 32])
        .public()
        .into();
    let other: DeviceKey = ployz_core::authority::Signer::from_seed(&[2; 32])
        .public()
        .into();
    let (_, body) = relayed_join(
        &crate::global_fixtures::machine('a', "web-3"),
        vec![own],
        ployz_core::SealingPublicKey::from_bytes([2; 32]),
    );
    let passkey = FirstKeyHint::Passkey { code: other.code() };
    let refused = super::refuse_another_key(&body, passkey, own).unwrap_err();
    assert!(
        refused
            .to_string()
            .contains(&format!("passkey {}", other.code())),
        "{refused}"
    );
    assert!(
        super::refuse_another_key(&body, FirstKeyHint::Device { code: other.code() }, own).is_err()
    );
    super::refuse_another_key(&body, FirstKeyHint::Device { code: own.code() }, own).unwrap();
}
