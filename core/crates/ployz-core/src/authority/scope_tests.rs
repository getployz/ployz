//! Each closed risk of scoped keys, through the verifier a Machine runs.

use crate::authority::MismatchField;
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
        name: None,
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

fn privileged(name: &str) -> RequestedServiceSpec {
    serde_json::from_value(json!({
        "name": name,
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": IMAGE, "pull_policy": "missing", "privileged": true},
    }))
    .unwrap()
}

fn version_of(
    by: &Signer,
    namespace_name: &str,
    service_name: &str,
    forget_older: bool,
) -> Signed<ServiceVersion> {
    version_for(by, namespace_name, &requested(service_name), forget_older)
}

/// A version of `spec` signed by `by`. One the CI key signs edits [`root`], as every CI
/// version must, and [`admit_kind`] presents that root beside it.
fn version_for(
    by: &Signer,
    namespace_name: &str,
    spec: &RequestedServiceSpec,
    forget_older: bool,
) -> Signed<ServiceVersion> {
    let parent = (by.public() == ci().public())
        .then(|| PayloadDigest::of(&root(&spec.name), &namespace(namespace_name)).unwrap());
    let body = ServiceVersion {
        cluster: cluster(),
        namespace: namespace(namespace_name),
        service: spec.name.clone(),
        digest: PayloadDigest::of(spec, &namespace(namespace_name)).unwrap(),
        signed_at: at(NOW),
        signer: SignerKey::Device(by.public().into()),
        proposal: None,
        forget_older,
        parent,
    };
    Signed::sign(&body, by).unwrap()
}

/// The spec of `name` a person last approved, which every CI version here edits. Its
/// digest is never one a test creates.
fn root(name: &ServiceName) -> RequestedServiceSpec {
    serde_json::from_value(json!({
        "name": name,
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": IMAGE, "pull_policy": "missing", "environment": {"ROOT": "1"}},
    }))
    .unwrap()
}

/// Admit a creation of `service_name` in `namespace_name` under `versions`.
fn admit(
    versions: &[Signed<ServiceVersion>],
    trusted: &TrustedKeys,
    namespace_name: &str,
    service_name: &str,
) -> Result<version::VersionAdmitted, AuthorityRefusal> {
    admit_spec(versions, trusted, namespace_name, &requested(service_name))
}

fn admit_spec(
    versions: &[Signed<ServiceVersion>],
    trusted: &TrustedKeys,
    namespace_name: &str,
    requested: &RequestedServiceSpec,
) -> Result<version::VersionAdmitted, AuthorityRefusal> {
    admit_kind(
        versions,
        trusted,
        namespace_name,
        requested,
        ContainerKind::ServiceContainer,
    )
}

/// Admit a creation of `kind` from `requested`, its Volumes scoped to `namespace_name` as
/// a Machine receives them.
fn admit_kind(
    versions: &[Signed<ServiceVersion>],
    trusted: &TrustedKeys,
    namespace_name: &str,
    requested: &RequestedServiceSpec,
    kind: ContainerKind,
) -> Result<version::VersionAdmitted, AuthorityRefusal> {
    let mut scoped = requested.clone();
    scoped.mount_graph = scoped
        .mount_graph
        .scope_to_namespace(&namespace(namespace_name))
        .unwrap();
    let spec = resolved(&scoped);
    let mut versions = versions.to_vec();
    if versions
        .iter()
        .any(|version| version.open().is_ok_and(|body| body.parent.is_some()))
    {
        versions.push(version_for(
            &device(),
            namespace_name,
            &root(&requested.name),
            false,
        ));
    }
    version::verify(
        &versions,
        &VersionFacts {
            cluster: Some(cluster()),
            trusted,
            namespace: &namespace(namespace_name),
            creation: Creation {
                service: &requested.name,
                kind,
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
fn a_ci_key_made_before_keys_named_environments_admits_nothing() {
    let held = TrustedGrant {
        grant: grant(GrantKind::Ci, spec_scope(vec![place("app", None)]), NOW),
        approved_by: PasskeyApprover::Server,
    };
    let mut legacy = serde_json::to_value(&held).unwrap();
    *legacy.pointer_mut("/grant/scope/places").unwrap() = json!("cluster");
    let [read] = <[TrustedGrant; 1]>::try_from(readable_grants(json!([legacy])).unwrap()).unwrap();
    let trusted = trusting(read.grant);

    assert_eq!(
        admit(
            &[version_of(&ci(), "app", "web", false)],
            &trusted,
            "app",
            "web"
        ),
        Err(AuthorityRefusal::Unscoped {
            code: ci().public().code()
        }),
    );
}

#[test]
fn a_ci_key_admits_no_version_that_reaches_its_servers_host() {
    let trusted = ci_in_app();
    let spec = privileged("web");

    assert_eq!(
        admit_spec(
            &[version_for(&ci(), "app", &spec, false)],
            &trusted,
            "app",
            &spec
        ),
        Err(out_of_scope(
            KeyKind::Ci,
            "a version that reaches its Server's host"
        )),
    );
    assert!(
        admit_spec(
            &[version_for(&device(), "app", &spec, false)],
            &trusted,
            "app",
            &spec
        )
        .is_ok(),
        "a Device still approves it"
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
        key: Box::new(key),
        members: vec![here()],
        prior: None,
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
fn a_key_step_refuses_a_ci_key_wider_than_its_kind_or_naming_no_environment() {
    let other = Signer::from_seed(&[4; 32]).public();
    let wider_caps = NewKey::Ci {
        key: other,
        scope: KeyScope {
            caps: CapSet::of(&[Cap::Spec, Cap::Exec]),
            places: KeyPlaces::Only(vec![place("app", None)]),
        },
        volumes: Vec::new(),
        name: None,
    };
    let unscoped_ci = NewKey::Ci {
        key: other,
        scope: KeyScope {
            caps: CapSet::of(&[Cap::Spec]),
            places: KeyPlaces::Unscoped,
        },
        volumes: Vec::new(),
        name: None,
    };
    for key in [wider_caps, unscoped_ci] {
        assert_eq!(
            verify_key(&key_bundle(trust(key), &device(), NOW), &ci_in_app()),
            Err(AuthorityRefusal::Mismatch {
                field: MismatchField::KeyScope
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
        name: None,
    };

    assert_eq!(
        verify_key(
            &key_bundle(trust(key.clone()), &device(), NOW - 5),
            &ci_in_app()
        ),
        Ok(KeyVerified::Trust {
            key: Box::new(key),
            issued_at: at(NOW - 5),
            prior: None,
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
                field: MismatchField::ApprovedGrants
            }),
        );
    }
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

fn prod() -> Uuid {
    Uuid::from_u128(0x960d)
}

/// The authored ID of `app/api`, which [`place`] names.
fn api() -> Uuid {
    Uuid::from_u128(0xa1)
}

fn sibling() -> Uuid {
    Uuid::from_u128(0xb2)
}

/// A Sealed value authored as `scope`, which admission never opens.
fn sealed(key: &str, scope: SealedScope) -> serde_json::Value {
    serde_json::to_value(crate::SealedValue {
        sealed: Ciphertext::from_bytes(vec![7; 48]),
        scope,
        key: key.to_owned(),
    })
    .unwrap()
}

/// `app/api` sealed in `environment`, with `environment` as its variables.
fn sealed_api(environment: Uuid, variables: serde_json::Value) -> RequestedServiceSpec {
    serde_json::from_value(json!({
        "name": "api",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {
            "image": IMAGE,
            "pull_policy": "missing",
            "environment": variables,
            "sealed_identity": {"organization": "org_1", "environment": environment, "service": api()},
        },
    }))
    .unwrap()
}

/// A Service place on `app/api` alone.
fn api_only() -> TrustedKeys {
    trusting(grant(
        GrantKind::Agent,
        spec_scope(vec![place("app", Some("api"))]),
        NOW,
    ))
}

fn values_out_of_place(kind: KeyKind) -> AuthorityRefusal {
    out_of_scope(kind, "Sealed values outside its places")
}

#[test]
fn a_service_place_opens_no_template_part_of_a_sibling_service() {
    let own = sealed_api(
        staging(),
        json!({"TOKEN": sealed("TOKEN", SealedScope::Service(api()))}),
    );
    let borrowed = sealed_api(
        staging(),
        json!({
            "TOKEN": sealed("TOKEN", SealedScope::Service(api())),
            "URL": {"template": ["postgres://app:", sealed("DB_PASSWORD", SealedScope::Service(sibling())), "@db"]},
        }),
    );

    assert!(
        admit_spec(
            &[version_for(&ci(), "app", &own, false)],
            &api_only(),
            "app",
            &own
        )
        .is_ok()
    );
    assert_eq!(
        admit_spec(
            &[version_for(&ci(), "app", &borrowed, false)],
            &api_only(),
            "app",
            &borrowed
        ),
        Err(values_out_of_place(KeyKind::Agent)),
    );
}

#[test]
fn a_service_place_opens_no_shared_part_where_its_environment_place_would() {
    let shared = sealed_api(
        staging(),
        json!({"URL": {"template": ["postgres://", sealed("DB_PASSWORD", SealedScope::Shared)]}}),
    );
    let version = [version_for(&ci(), "app", &shared, false)];

    assert_eq!(
        admit_spec(&version, &api_only(), "app", &shared),
        Err(values_out_of_place(KeyKind::Agent)),
    );
    assert!(admit_spec(&version, &ci_in_app(), "app", &shared).is_ok());
}

#[test]
fn a_hooks_shared_part_counts_for_the_hook_it_runs_in() {
    let mut spec = sealed_api(staging(), json!({}));
    spec.pre_deploy = Some(
        serde_json::from_value(json!({
            "command": ["migrate"],
            "environment": {"DB_PASSWORD": sealed("DB_PASSWORD", SealedScope::Shared)},
        }))
        .unwrap(),
    );
    let version = [version_for(&ci(), "app", &spec, false)];

    assert_eq!(
        admit_kind(
            &version,
            &api_only(),
            "app",
            &spec,
            ContainerKind::PreDeployHook
        ),
        Err(values_out_of_place(KeyKind::Agent)),
    );
    assert!(
        admit_kind(
            &version,
            &api_only(),
            "app",
            &spec,
            ContainerKind::ServiceContainer
        )
        .is_ok(),
        "the Service Container opens none of its hook's values"
    );
}

#[test]
fn a_ci_key_opens_no_value_sealed_in_another_environment_of_its_namespace() {
    let spec = sealed_api(
        prod(),
        json!({"TOKEN": sealed("TOKEN", SealedScope::Shared)}),
    );

    assert_eq!(
        admit_spec(
            &[version_for(&ci(), "app", &spec, false)],
            &ci_in_app(),
            "app",
            &spec
        ),
        Err(values_out_of_place(KeyKind::Ci)),
    );
}

fn birth(name: &str) -> crate::VolumeBirth {
    serde_json::from_value(json!({
        "namespace": "app", "name": name, "made_at": 1_762_178_640_000_u64,
        "nonce": "00112233445566778899aabbccddeeff",
    }))
    .unwrap()
}

/// `app/api` mounting each of `volumes`, by declared name and the birth it carries.
fn mounting(volumes: &[(&str, Option<crate::VolumeBirth>)]) -> RequestedServiceSpec {
    let declared: Vec<_> = volumes
        .iter()
        .map(|(name, born)| {
            let mut source =
                json!({"kind": "provisioned", "name": name, "maximum_bytes": 1_073_741_824_u64});
            if let (Some(born), Some(fields)) = (born, source.as_object_mut()) {
                fields.insert("birth".to_owned(), serde_json::to_value(born).unwrap());
            }
            json!({"reference": name, "source": source})
        })
        .collect();
    let mounts: Vec<_> = volumes
        .iter()
        .map(|(name, _)| json!({"volume": name, "target": format!("/{name}")}))
        .collect();
    serde_json::from_value(json!({
        "name": "api",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": IMAGE, "pull_policy": "missing"},
        "volumes": declared,
        "mounts": mounts,
    }))
    .unwrap()
}

/// The entry for the Volume declared as `name` in Namespace `app`, expecting `origin`.
fn entry(name: &str, origin: crate::VolumeOrigin) -> GrantVolume {
    GrantVolume {
        origin,
        name: namespace("app").volume_name(&crate::DockerVolumeName::parse(name).unwrap()),
    }
}

fn born(name: &str) -> crate::VolumeOrigin {
    crate::VolumeOrigin::of(&birth(name)).unwrap()
}

/// A CI key over Namespace `app` that may mount `volumes`.
fn ci_with(volumes: Vec<GrantVolume>) -> TrustedKeys {
    trusting(Grant {
        volumes,
        ..grant(GrantKind::Ci, spec_scope(vec![place("app", None)]), NOW)
    })
}

fn not_granted(name: &str) -> AuthorityRefusal {
    AuthorityRefusal::VolumeNotGranted {
        code: ci().public().code(),
        kind: KeyKind::Ci,
        volume: namespace("app").volume_name(&crate::DockerVolumeName::parse(name).unwrap()),
    }
}

#[test]
fn a_ci_key_mounts_no_sibling_volume_its_grant_leaves_out() {
    let trusted = ci_with(vec![entry("data", born("data"))]);
    let own = mounting(&[("data", Some(birth("data")))]);
    let both = mounting(&[
        ("data", Some(birth("data"))),
        ("cache", Some(birth("cache"))),
    ]);

    assert!(
        admit_spec(
            &[version_for(&ci(), "app", &own, false)],
            &trusted,
            "app",
            &own
        )
        .is_ok()
    );
    assert_eq!(
        admit_spec(
            &[version_for(&ci(), "app", &both, false)],
            &trusted,
            "app",
            &both
        ),
        Err(not_granted("cache")),
    );
    assert!(
        admit_spec(
            &[version_for(&device(), "app", &both, false)],
            &trusted,
            "app",
            &both
        )
        .is_ok(),
        "a Device mounts any Volume"
    );
}

#[test]
fn a_copy_of_a_granted_volume_under_another_name_needs_its_own_entry() {
    let trusted = ci_with(vec![entry("data", born("data"))]);
    let copy = mounting(&[("copy", Some(birth("data")))]);

    assert_eq!(
        admit_spec(
            &[version_for(&ci(), "app", &copy, false)],
            &trusted,
            "app",
            &copy
        ),
        Err(not_granted("copy")),
    );
}

#[test]
fn a_volume_from_before_births_needs_its_legacy_origin_on_the_list() {
    let legacy = mounting(&[("data", None)]);
    let version = [version_for(&ci(), "app", &legacy, false)];
    let physical = namespace("app").volume_name(&crate::DockerVolumeName::parse("data").unwrap());

    assert_eq!(
        admit_spec(
            &version,
            &ci_with(vec![entry("data", born("data"))]),
            "app",
            &legacy
        ),
        Err(not_granted("data")),
    );
    assert!(
        admit_spec(
            &version,
            &ci_with(vec![entry("data", crate::VolumeOrigin::legacy(&physical))]),
            "app",
            &legacy
        )
        .is_ok()
    );
}
