//! Each closed risk of scoped keys, through the verifier a Machine runs.

use serde_json::json;
use uuid::Uuid;

use super::*;
use crate::authority::{
    AuthorityRefusal, Budget, JoinApproval, KeyApproval, KeyChange, KeyFacts, KeyMember, KeyStep,
    KeyVerified, NewKey, PrivateAccessApproval, PrivateAccessFacts, ProposalApproval, ProposalRef,
    ProposalStep, ServiceVersion, Signed, Signer, SignerKey, StepDigest, StepKind, TrustedKeys,
    VersionFacts, deploy::Creation, deploy::PayloadDigest, join, key, private_access, version,
};
use crate::{
    Ciphertext, ContainerKind, DeviceKey, MachineId, Namespace, OperationId, RequestedServiceSpec,
    ResolvedServiceSpec, ResolvedUpdateConfig, SealingPublicKey, ServiceId, ServiceName,
    WireGuardPublicKey,
};

const IMAGE: &str = "nginx@sha256:0000000000000000000000000000000000000000000000000000000000000001";
const NOW: u64 = 1_000_000;

fn at(millis: u64) -> Timestamp {
    Timestamp::from_millis(millis).unwrap()
}

fn cluster() -> SealingPublicKey {
    SealingPublicKey::from_bytes([9; 32])
}

fn device() -> Signer {
    Signer::from_seed(&[1; 32])
}

fn ci() -> Signer {
    Signer::from_seed(&[2; 32])
}

fn namespace(name: &str) -> Namespace {
    Namespace::parse(name).unwrap()
}

fn service(name: &str) -> ServiceName {
    ServiceName::parse(name).unwrap()
}

fn staging() -> Uuid {
    Uuid::from_u128(0x5747)
}

fn place(namespace_name: &str, service_name: Option<&str>) -> KeyPlace {
    KeyPlace {
        environment: staging(),
        namespace: namespace(namespace_name),
        service: service_name.map(|name| PlaceService {
            id: Uuid::from_u128(0xa1),
            name: service(name),
        }),
    }
}

fn spec_scope(places: Vec<KeyPlace>) -> KeyScope {
    KeyScope {
        caps: CapSet::of(&[Cap::Spec]),
        places: KeyPlaces::Only(places),
    }
}

fn grant(kind: GrantKind, scope: KeyScope, issued_at: u64) -> Grant {
    Grant {
        key: ci().public(),
        kind,
        scope,
        volumes: Vec::new(),
        issued_at: at(issued_at),
    }
}

/// A Machine trusting `device()` as a Device and `ci()` under `grant`.
fn trusting(grant: Grant) -> TrustedKeys {
    TrustedKeys::new(
        [DeviceKey::from(device().public())],
        [],
        [TrustedGrant {
            grant,
            approved_by: PasskeyApprover::Server,
        }],
    )
}

fn ci_in_app() -> TrustedKeys {
    trusting(grant(
        GrantKind::Ci,
        spec_scope(vec![place("app", None)]),
        NOW,
    ))
}

fn requested(name: &str) -> RequestedServiceSpec {
    serde_json::from_value(json!({
        "name": name,
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": IMAGE, "pull_policy": "missing"},
    }))
    .unwrap()
}

fn resolved(spec: &RequestedServiceSpec) -> ResolvedServiceSpec {
    spec.to_resolved(
        ServiceId::parse("c".repeat(32)).unwrap(),
        ResolvedUpdateConfig::default(),
    )
    .unwrap()
}

fn version_of(
    by: &Signer,
    namespace_name: &str,
    service_name: &str,
    forget_older: bool,
) -> Signed<ServiceVersion> {
    let body = ServiceVersion {
        cluster: cluster(),
        namespace: namespace(namespace_name),
        service: service(service_name),
        digest: PayloadDigest::of(&requested(service_name), &namespace(namespace_name)).unwrap(),
        signed_at: at(NOW),
        signer: SignerKey::Device(by.public().into()),
        proposal: None,
        forget_older,
    };
    Signed::sign(&body, by).unwrap()
}

/// Admit a creation of `service_name` in `namespace_name` under `versions`.
fn admit(
    versions: &[Signed<ServiceVersion>],
    trusted: &TrustedKeys,
    namespace_name: &str,
    service_name: &str,
) -> Result<version::VersionAdmitted, AuthorityRefusal> {
    let spec = resolved(&requested(service_name));
    version::verify(
        versions,
        &VersionFacts {
            cluster: Some(cluster()),
            trusted,
            namespace: &namespace(namespace_name),
            creation: Creation {
                service: &service(service_name),
                kind: ContainerKind::ServiceContainer,
                spec: &spec,
            },
        },
        &mut Budget::default(),
    )
}

fn out_of_scope(kind: KeyKind, what: &'static str) -> AuthorityRefusal {
    AuthorityRefusal::OutOfScope {
        code: ci().public().code(),
        kind,
        what,
    }
}

#[test]
fn a_ci_key_admits_a_version_only_in_a_namespace_its_scope_names() {
    let trusted = ci_in_app();

    assert!(
        admit(
            &[version_of(&ci(), "app", "web", false)],
            &trusted,
            "app",
            "web"
        )
        .is_ok()
    );
    assert_eq!(
        admit(
            &[version_of(&ci(), "prod", "web", false)],
            &trusted,
            "prod",
            "web"
        ),
        Err(out_of_scope(KeyKind::Ci, "a version of this Service")),
    );
}

#[test]
fn an_agent_key_scoped_to_one_service_admits_no_sibling() {
    let trusted = trusting(grant(
        GrantKind::Agent,
        spec_scope(vec![place("app", Some("api"))]),
        NOW,
    ));

    assert!(
        admit(
            &[version_of(&ci(), "app", "api", false)],
            &trusted,
            "app",
            "api"
        )
        .is_ok()
    );
    assert_eq!(
        admit(
            &[version_of(&ci(), "app", "worker", false)],
            &trusted,
            "app",
            "worker"
        ),
        Err(out_of_scope(KeyKind::Agent, "a version of this Service")),
    );
}

#[test]
fn a_ci_key_forgets_nothing_even_for_its_own_service() {
    let trusted = ci_in_app();
    let forget = version_of(&ci(), "app", "web", true);

    assert_eq!(
        version::verify_forget(std::slice::from_ref(&forget), &trusted, Some(cluster())),
        vec![Err(out_of_scope(KeyKind::Ci, "a forget"))],
    );
    assert_eq!(
        admit(&[forget], &trusted, "app", "web"),
        Err(out_of_scope(KeyKind::Ci, "a forget")),
    );
    assert!(
        version::verify_forget(
            &[version_of(&device(), "app", "web", true)],
            &trusted,
            Some(cluster())
        )
        .iter()
        .all(Result::is_ok),
        "a Device still forgets"
    );
}

fn here() -> KeyMember {
    KeyMember {
        id: MachineId::parse("a".repeat(32)).unwrap(),
        public_key: WireGuardPublicKey([7; 32]),
    }
}

/// `change` as the only step of a bundle `by` signed, issued at `issued_at`.
fn key_bundle(change: KeyChange, by: &Signer, issued_at: u64) -> Signed<KeyStep> {
    let operation = OperationId::parse("0f8fad5bd9cb469fa16570867728950e").unwrap();
    let step = KeyStep {
        cluster: cluster(),
        change,
        signer: SignerKey::Device(by.public().into()),
        proposal: Some(ProposalRef {
            operation,
            index: 0,
        }),
    };
    let plan = ProposalApproval {
        cluster: cluster(),
        environment: namespace("app"),
        operation,
        steps: vec![ProposalStep {
            index: 0,
            kind: StepKind::Key,
            digest: StepDigest::of(&Signed::unsigned(&step).unwrap().body),
        }],
        issued_at: at(issued_at),
        expires_at: at(issued_at + 600_000),
        signer: SignerKey::Device(by.public().into()),
    };
    Signed::proposed(&step, Signed::sign(&plan, by).unwrap()).unwrap()
}

fn verify_key(
    step: &Signed<KeyStep>,
    trusted: &TrustedKeys,
) -> Result<KeyVerified, AuthorityRefusal> {
    key::verify(
        step,
        &KeyFacts {
            trusted,
            cluster: Some(cluster()),
            member: here(),
            now: at(NOW + 1),
        },
    )
    .map(|(verified, _)| verified)
}

fn trust(key: NewKey) -> KeyChange {
    KeyChange::Trust {
        key,
        members: vec![here()],
    }
}

#[test]
fn a_ci_key_signs_no_key_change() {
    let step = key_bundle(
        trust(NewKey::Device {
            key: DeviceKey::from_bytes([3; 32]),
        }),
        &ci(),
        NOW,
    );

    assert_eq!(
        verify_key(&step, &ci_in_app()),
        Err(out_of_scope(KeyKind::Ci, "a key change")),
    );
}

#[test]
fn a_key_step_refuses_a_ci_or_agent_scope_wider_than_its_kind() {
    let other = Signer::from_seed(&[4; 32]).public();
    let wider_caps = NewKey::Ci {
        key: other,
        scope: KeyScope {
            caps: CapSet::of(&[Cap::Spec, Cap::Exec]),
            places: KeyPlaces::Only(vec![place("app", None)]),
        },
        volumes: Vec::new(),
    };
    let cluster_agent = NewKey::Agent {
        key: other,
        scope: KeyScope {
            caps: CapSet::of(&[Cap::Spec]),
            places: KeyPlaces::Cluster,
        },
        volumes: Vec::new(),
    };
    for key in [wider_caps, cluster_agent] {
        assert_eq!(
            verify_key(&key_bundle(trust(key), &device(), NOW), &ci_in_app()),
            Err(AuthorityRefusal::Mismatch {
                field: "a key scope"
            }),
        );
    }
}

#[test]
fn a_ci_trust_carries_the_issue_time_of_its_bundle() {
    let key = NewKey::Ci {
        key: Signer::from_seed(&[4; 32]).public(),
        scope: spec_scope(vec![place("app", None)]),
        volumes: Vec::new(),
    };

    assert_eq!(
        verify_key(
            &key_bundle(trust(key.clone()), &device(), NOW - 5),
            &ci_in_app()
        ),
        Ok(KeyVerified::Trust {
            key,
            issued_at: at(NOW - 5)
        }),
    );
}

fn joiner() -> crate::Machine {
    crate::Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: MachineId::parse("d".repeat(32)).unwrap(),
        name: crate::MachineName::parse("joining").unwrap(),
        subnet: "10.210.1.0/24".parse().unwrap(),
        public_key: WireGuardPublicKey([7; 32]),
        public_ip: None,
        advertised_endpoints: vec![crate::AdvertisedEndpoint(
            "192.0.2.2:51820".parse().unwrap(),
        )],
        runtime: Default::default(),
        join_approval: None,
        build_concurrency: None,
    }
}

fn admitting() -> MachineId {
    MachineId::parse("b".repeat(32)).unwrap()
}

fn join_body(by: &Signer, grants: Vec<Grant>) -> JoinApproval {
    let joiner = joiner();
    JoinApproval {
        cluster: cluster(),
        machine: (&joiner).into(),
        network: "10.210.0.0/16".parse().unwrap(),
        sealing_recipient: joiner.public_key,
        devices: vec![device().public().into()],
        passkeys: Vec::new(),
        grants,
        admitting_machines: vec![admitting()],
        bootstrap: Vec::new(),
        issued_at: at(NOW),
        expires_at: at(NOW + 60_000),
        signer: SignerKey::Device(by.public().into()),
        proposal: None,
    }
}

fn admit_join(
    signed: &Signed<JoinApproval>,
    trusted: &TrustedKeys,
) -> Result<JoinApproval, AuthorityRefusal> {
    join::verify(
        signed,
        cluster(),
        admitting(),
        trusted,
        &(&joiner()).into(),
        "10.210.0.0/16".parse().unwrap(),
        at(NOW + 1),
    )
}

#[test]
fn a_ci_key_approves_no_join() {
    let signed = Signed::sign(&join_body(&ci(), Vec::new()), &ci()).unwrap();

    assert_eq!(
        admit_join(&signed, &ci_in_app()),
        Err(out_of_scope(KeyKind::Ci, "a join"))
    );
}

#[test]
fn a_join_carries_a_grant_only_as_this_machine_holds_it_now() {
    let held = grant(GrantKind::Ci, spec_scope(vec![place("app", None)]), NOW);
    let trusted = trusting(held.clone());
    let wider = Grant {
        scope: spec_scope(vec![place("app", None), place("prod", None)]),
        ..held.clone()
    };
    let edited_since = Grant {
        issued_at: at(NOW - 1),
        ..held.clone()
    };

    let sign = |grants| Signed::sign(&join_body(&device(), grants), &device()).unwrap();
    assert!(admit_join(&sign(vec![held]), &trusted).is_ok());
    for grants in [vec![wider], vec![edited_since]] {
        assert_eq!(
            admit_join(&sign(grants), &trusted),
            Err(AuthorityRefusal::Mismatch {
                field: "approved grants"
            }),
        );
    }
}

#[test]
fn a_joiner_refuses_a_grant_that_holds_a_device_key() {
    let shadowing = Grant {
        key: device().public(),
        ..grant(GrantKind::Ci, spec_scope(vec![place("app", None)]), NOW)
    };
    let signed = Signed::sign(&join_body(&device(), vec![shadowing]), &device()).unwrap();

    assert_eq!(
        join::verify_as_joiner(&signed, None, &joiner(), Some(cluster()), at(NOW + 1)),
        Err(AuthorityRefusal::Mismatch {
            field: "approved grants"
        }),
    );
}

#[test]
fn a_ci_key_turns_on_no_private_access() {
    let sealed = Ciphertext::from_bytes(vec![3; 80]);
    let body = PrivateAccessApproval {
        cluster: cluster(),
        auth_key: private_access::AuthKeyDigest::of(&sealed),
        issued_at: at(NOW),
        expires_at: at(NOW + 60_000),
        signer: SignerKey::Device(ci().public().into()),
    };
    let trusted = ci_in_app();
    let facts = PrivateAccessFacts {
        cluster: Some(cluster()),
        trusted: &trusted,
        sealed: &sealed,
        now: at(NOW + 1),
    };

    assert_eq!(
        private_access::verify(
            &KeyApproval::Signed(Signed::sign(&body, &ci()).unwrap()),
            &facts
        ),
        Err(out_of_scope(KeyKind::Ci, "Private Access")),
    );
}

#[test]
fn a_grant_entry_this_build_cannot_read_trusts_nothing() {
    let held = TrustedGrant {
        grant: grant(GrantKind::Ci, spec_scope(vec![place("app", None)]), NOW),
        approved_by: PasskeyApprover::Server,
    };
    let mut newer = serde_json::to_value(&held).unwrap();
    newer
        .pointer_mut("/grant/scope/places/only/0")
        .and_then(serde_json::Value::as_object_mut)
        .unwrap()
        .insert("volume".to_owned(), json!("db"));

    let read = readable_grants(json!([newer, held])).unwrap();

    assert_eq!(read, vec![held]);
}
