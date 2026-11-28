use ployz_core::authority::{
    Cap, CapSet, Grant, GrantKind, KeyName, KeyPlace, KeyPlaces, KeyScope, PasskeyApprover,
    PayloadDigest, PlaceService, ServiceVersion, Signed, Signer, SignerKey, Timestamp,
    TrustedGrant,
};
use ployz_core::{
    DeviceKey, Namespace, RequestedServiceSpec, RpcErrorCode, SealingPublicKey, ServiceName,
};
use std::cell::RefCell;
use std::collections::BTreeMap;

use ployz_store::{EnvironmentRef, Running};
use serde_json::json;
use uuid::Uuid;

use super::rotate::{Applied, Carried, CarriedVersion, carry, verified};
use super::wanted::Wanted;
use super::{Ask, Consent, Contents, consent};

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

fn wanting(places: Vec<KeyPlace>) -> Contents {
    Contents::wanted(&Wanted {
        scope: scope(places),
        volumes: Vec::new(),
        name: None,
        shown: Vec::new(),
    })
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
        Consent::Ask(Ask::Widens)
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

/// `ployz key scope K --env production --service web --yes` on a key that also holds a
/// name: the flags leave the name out, so the change would drop it. It narrows the
/// places, yet it never goes out unread.
#[test]
fn a_scope_change_that_drops_the_name_the_key_holds_needs_a_person() {
    let held = Grant {
        name: Some(KeyName::parse("github-actions").unwrap()),
        ..production_grant()
    };
    let code = held.key.code();
    let narrowed = wanting(vec![place(PRODUCTION, "app-prod", true)]);

    let refused = consent(code, &[Some(&held)], true, &narrowed, true).unwrap_err();
    assert_eq!(refused.report().code, RpcErrorCode::ConfirmationRequired);
    assert_eq!(
        consent(code, &[Some(&held)], true, &narrowed, false).unwrap(),
        Consent::Ask(Ask::Drops)
    );
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
    version_in(by, spec, namespace("app-prod"))
}

fn version_in(
    by: &Signer,
    spec: &RequestedServiceSpec,
    namespace: Namespace,
) -> Signed<ServiceVersion> {
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
        (&old, &Applied::default()),
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
        let refused = verified(
            (&old, &Applied::default()),
            &namespace("app-prod"),
            &web(),
            &running(statement),
        )
        .unwrap_err();
        assert_eq!(refused.report().code, RpcErrorCode::Conflict);
    }
}

/// A rotation's redeploy failed after it posted the new key's signature, so the
/// Containers still run admitted under the old key alone. Run again, it redeploys the
/// Service, and skips it only once its own redeploy applied that spec.
#[test]
fn a_rotation_run_again_redeploys_until_its_own_redeploy_applied() {
    let old = TrustedGrant {
        grant: production_grant(),
        approved_by: PasskeyApprover::Unknown,
    };
    let fresh = Signer::from_seed(&[4; 32]);
    let carried = version_by(&ci(), &spec()).open().unwrap();
    let resigned = Signed::sign(
        &ServiceVersion {
            signer: SignerKey::Device(DeviceKey::from(fresh.public())),
            ..carried.clone()
        },
        &fresh,
    )
    .unwrap();
    let running = Running {
        spec: Box::new(spec()),
        pin: None,
        versions: vec![resigned, version_by(&ci(), &spec())],
    };
    let carries = |applied: &Applied| {
        verified((&old, applied), &namespace("app-prod"), &web(), &running).unwrap()
    };

    let mut applied = Applied::default();
    assert_eq!(carries(&applied), Some(carried.clone()), "failed redeploy");

    let mut elsewhere = carried.clone();
    elsewhere.digest = PayloadDigest::from_bytes([8; 32]);
    applied.add(&elsewhere);
    assert_eq!(
        carries(&applied),
        Some(carried.clone()),
        "another spec applied"
    );

    applied.add(&carried);
    assert_eq!(carries(&applied), None, "its own redeploy applied");
}

/// `web` at image `n`, signed by the CI key alone.
fn ci_deploy(n: u64, namespace: &Namespace) -> Running {
    let mut spec = spec();
    spec.container.image = format!("nginx@sha256:{n:064x}");
    Running {
        versions: vec![version_in(&ci(), &spec, namespace.clone())],
        spec: Box::new(spec),
        pin: None,
    }
}

/// Production and staging each run `web` the CI key deployed. The rotation redeploys
/// production, then staging. CI deploys to production with the old key during any
/// redeploy `deploying` names, given the Environment it carries and how many CI deploys
/// landed so far.
/// Returns the Environments each redeploy carried, in order, and what the old key alone
/// still signed at the end.
fn rotate_while(
    mut deploying: impl FnMut(&Namespace, u64) -> bool,
) -> (Vec<Namespace>, Vec<Namespace>) {
    let old = TrustedGrant {
        grant: Grant {
            scope: scope(vec![
                place(PRODUCTION, "app-prod", false),
                place(STAGING, "app-staging", false),
            ]),
            ..production_grant()
        },
        approved_by: PasskeyApprover::Unknown,
    };
    let (production, staging) = (namespace("app-prod"), namespace("app-staging"));
    let world = RefCell::new(BTreeMap::from([
        (production.clone(), ci_deploy(1, &production)),
        (staging.clone(), ci_deploy(1, &staging)),
    ]));
    let read = |applied: &Applied| {
        let mut carried = Vec::new();
        for (namespace, running) in world.borrow().iter() {
            if let Some(version) = verified((&old, applied), namespace, &web(), running)? {
                carried.push(Carried {
                    environment: EnvironmentRef::default(),
                    namespace: namespace.clone(),
                    services: vec![CarriedVersion {
                        authored: web(),
                        version,
                        image: running.spec.container.image.clone(),
                        pin: None,
                    }],
                });
            }
        }
        Ok(carried)
    };
    let mut redeployed = Vec::new();
    let mut deploys = 1;
    let dir = tempfile::tempdir().unwrap();
    let mut applied = Applied::default();
    let first = read(&applied).unwrap();
    let carried = carry(
        first,
        &mut applied,
        &dir.path().join("applied"),
        read,
        |environment| {
            redeployed.push(environment.namespace.clone());
            if deploying(&environment.namespace, deploys) {
                deploys += 1;
                world
                    .borrow_mut()
                    .insert(production.clone(), ci_deploy(deploys, &production));
            }
            Ok(Some(redeployed.len() as u64))
        },
    )
    .unwrap();
    let left = carried
        .left
        .into_iter()
        .map(|left| left.namespace)
        .collect();
    (redeployed, left)
}

/// CI ships a fresh image to production with the old key after the rotation redeployed
/// production but before staging's redeploy finished. Production runs a version only
/// the old key signed, so the rotation redeploys it again before removing the old key,
/// and keeps the old key while a deployer with it keeps landing.
#[test]
fn a_rotation_redeploys_what_the_old_key_shipped_while_it_ran_before_retiring_it() {
    let (production, staging) = (namespace("app-prod"), namespace("app-staging"));

    let (redeployed, left) = rotate_while(|carried, deploys| *carried == staging && deploys == 1);
    assert_eq!(
        redeployed,
        [production.clone(), staging, production.clone()]
    );
    assert!(left.is_empty(), "nothing needs the old key: {left:?}");

    let (_, left) = rotate_while(|_, _| true);
    assert_eq!(left, [production], "the old key stays trusted");
}
