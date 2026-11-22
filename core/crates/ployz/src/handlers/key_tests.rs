use ployz_core::authority::{
    Cap, CapSet, Grant, GrantKind, KeyPlace, KeyPlaces, KeyScope, PasskeyApprover, PayloadDigest,
    PlaceService, ServiceVersion, Signed, Signer, SignerKey, Timestamp, TrustedGrant,
};
use ployz_core::{
    DeviceKey, Namespace, RequestedServiceSpec, RpcErrorCode, SealingPublicKey, ServiceName,
};
use ployz_store::Running;
use serde_json::json;
use uuid::Uuid;

use super::rotate::verified;
use super::wanted::Wanted;
use super::{Consent, consent};

const PRODUCTION: Uuid = Uuid::from_u128(1);
const STAGING: Uuid = Uuid::from_u128(2);
const WEB: Uuid = Uuid::from_u128(3);

fn ci() -> Signer {
    Signer::from_seed(&[2; 32])
}

fn namespace(name: &str) -> Namespace {
    Namespace::parse(name).unwrap()
}

fn web() -> ServiceName {
    ServiceName::parse("web").unwrap()
}

fn place(environment: Uuid, name: &str, service: bool) -> KeyPlace {
    KeyPlace {
        environment,
        namespace: namespace(name),
        service: service.then(|| PlaceService {
            id: WEB,
            name: web(),
        }),
    }
}

fn scope(places: Vec<KeyPlace>) -> KeyScope {
    KeyScope {
        caps: CapSet::of(&[Cap::Spec]),
        places: KeyPlaces::Only(places),
    }
}

/// A CI key that may deploy anywhere in production.
fn production_grant() -> Grant {
    Grant {
        key: ci().public(),
        kind: GrantKind::Ci,
        scope: scope(vec![place(PRODUCTION, "app-prod", false)]),
        volumes: Vec::new(),
        name: None,
        issued_at: Timestamp::from_millis(100_000).unwrap(),
    }
}

fn wanting(places: Vec<KeyPlace>) -> Wanted {
    Wanted {
        scope: scope(places),
        volumes: Vec::new(),
        name: None,
        shown: Vec::new(),
    }
}

#[test]
fn a_scope_change_that_widens_the_key_is_refused_under_yes_and_one_that_narrows_goes_through() {
    let held = production_grant();
    let code = held.key.code();
    let widened = wanting(vec![
        place(PRODUCTION, "app-prod", false),
        place(STAGING, "app-staging", false),
    ]);
    let refused = consent(code, &[Some(&held)], true, &widened, true).unwrap_err();
    assert_eq!(refused.report().code, RpcErrorCode::ConfirmationRequired);
    assert_eq!(
        consent(code, &[Some(&held)], true, &widened, false).unwrap(),
        Consent::Ask
    );

    let narrowed = wanting(vec![place(PRODUCTION, "app-prod", true)]);
    assert_eq!(
        consent(code, &[Some(&held)], true, &narrowed, true).unwrap(),
        Consent::Silent
    );
    // A Server that didn't answer, or one that never held the key, might be widened.
    for (held, complete) in [(vec![Some(&held)], false), (vec![Some(&held), None], true)] {
        let refused = consent(code, &held, complete, &narrowed, true).unwrap_err();
        assert_eq!(refused.report().code, RpcErrorCode::ConfirmationRequired);
    }
}

fn spec() -> RequestedServiceSpec {
    serde_json::from_value(json!({
        "name": "web",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {
            "image": "nginx@sha256:1111111111111111111111111111111111111111111111111111111111111111",
            "pull_policy": "missing",
        },
    }))
    .unwrap()
}

/// A version of `web` naming the CI key as its signer, signed by `by`.
fn version_by(by: &Signer, spec: &RequestedServiceSpec) -> Signed<ServiceVersion> {
    let namespace = namespace("app-prod");
    Signed::sign(
        &ServiceVersion {
            cluster: SealingPublicKey::from_bytes([9; 32]),
            digest: PayloadDigest::of(spec, &namespace).unwrap(),
            namespace,
            service: web(),
            signed_at: Timestamp::from_millis(200_000).unwrap(),
            signer: SignerKey::Device(DeviceKey::from(ci().public())),
            proposal: None,
            forget_older: false,
            parent: Some(PayloadDigest::from_bytes([7; 32])),
        },
        by,
    )
    .unwrap()
}

#[test]
fn a_rotation_carries_only_versions_that_check_under_the_old_key() {
    let old = TrustedGrant {
        grant: production_grant(),
        approved_by: PasskeyApprover::Unknown,
    };
    let running = |statement| Running {
        spec: Box::new(spec()),
        pin: None,
        versions: vec![statement],
    };
    let carried = verified(
        &old,
        &namespace("app-prod"),
        &web(),
        &running(version_by(&ci(), &spec())),
    )
    .unwrap()
    .expect("the old key signed what runs");
    assert_eq!(carried.parent, Some(PayloadDigest::from_bytes([7; 32])));

    // Cloud hands back a version naming the old key that another key signed, or one for
    // a spec other than the one it runs: the rotation carries neither.
    let forger = Signer::from_seed(&[3; 32]);
    let mut other = spec();
    other.container.image =
        "nginx@sha256:2222222222222222222222222222222222222222222222222222222222222222".to_owned();
    for statement in [version_by(&forger, &spec()), version_by(&ci(), &other)] {
        let refused =
            verified(&old, &namespace("app-prod"), &web(), &running(statement)).unwrap_err();
        assert_eq!(refused.report().code, RpcErrorCode::Conflict);
    }
}
