use std::collections::BTreeMap;

use ployz_core::authority::{
    Budget, Cap, CapSet, Grant, GrantKind, KeyPlaces, KeyScope, PasskeyApprover, PayloadDigest,
    ServiceVersion, Signed, Signer, SignerKey, Timestamp, TrustedGrant, TrustedKeys, VersionFacts,
    deploy::Creation, version,
};
use ployz_core::{
    ContainerKind, DeviceKey, Hint, Namespace, RequestedServiceSpec, ResolvedUpdateConfig,
    RpcErrorCode, SealingPublicKey, ServiceId, ServiceName,
};
use ployz_store::{Parent, TagPin};
use serde_json::{Value, json};

use super::{Child, ImageEdit, Refused, children, posted, refuse_forget};
use crate::handlers::device::DeviceSigning;

const P1: &str = "nginx@sha256:1111111111111111111111111111111111111111111111111111111111111111";
const P2: &str = "nginx@sha256:2222222222222222222222222222222222222222222222222222222222222222";
const CLUSTER: [u8; 32] = [9; 32];

fn person() -> Signer {
    Signer::from_seed(&[1; 32])
}

const CI_SEED: [u8; 32] = [2; 32];

fn namespace() -> Namespace {
    Namespace::parse("app").unwrap()
}

fn web() -> ServiceName {
    ServiceName::parse("web").unwrap()
}

fn spec_of(value: Value) -> RequestedServiceSpec {
    serde_json::from_value(value).unwrap()
}

/// `web` running `image` with `MODE` set to `mode`.
fn spec(image: &str, mode: &str) -> RequestedServiceSpec {
    spec_of(json!({
        "name": "web",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": image, "pull_policy": "missing", "environment": {"MODE": mode}},
    }))
}

/// `web` running `image`, mounting the Provisioned Volume `data` born with `nonce`.
fn mounting(image: &str, nonce: &str) -> RequestedServiceSpec {
    spec_of(json!({
        "name": "web",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": image, "pull_policy": "missing"},
        "volumes": [{"reference": "data", "source": {"kind": "provisioned", "name": "data",
            "maximum_bytes": 1_073_741_824_u64,
            "birth": {"namespace": "app", "name": "data", "made_at": 1_762_178_640_000_u64,
                "nonce": nonce}}}],
        "mounts": [{"volume": "data", "target": "/data"}],
    }))
}

/// The version a person approved of `approved`, which ran its tag `nginx:1` pinned to
/// its image.
fn root(approved: &RequestedServiceSpec) -> Signed<ServiceVersion> {
    let person = person();
    Signed::sign(
        &ServiceVersion {
            cluster: SealingPublicKey::from_bytes(CLUSTER),
            namespace: namespace(),
            service: web(),
            digest: PayloadDigest::of(approved, &namespace()).unwrap(),
            signed_at: Timestamp::from_millis(100_000).unwrap(),
            signer: SignerKey::Device(DeviceKey::from(person.public())),
            proposal: None,
            forget_older: false,
            parent: None,
        },
        &person,
    )
    .unwrap()
}

/// What Cloud says `web`'s parent is: `statement`, with `spec` as the spec it ran.
fn parents(
    statement: Signed<ServiceVersion>,
    spec: RequestedServiceSpec,
) -> BTreeMap<ServiceName, Parent> {
    let pin = TagPin {
        tag: "nginx:1".to_owned(),
        image: spec.container.image.clone(),
    };
    BTreeMap::from([(
        web(),
        Parent::Signed {
            statement,
            spec: Box::new(spec),
            pin: Some(pin),
        },
    )])
}

fn image(service: ServiceName, image: &str, pinned: &str) -> ImageEdit {
    ImageEdit {
        service,
        image: image.to_owned(),
        pinned: pinned.to_owned(),
    }
}

fn edit_to_nginx_2() -> Vec<ImageEdit> {
    vec![image(web(), "nginx:2", P2)]
}

/// Sign as the silent deploy does: `planned` is Cloud's plan, `parents` what Cloud says
/// each Service's parent is.
fn sign(
    planned: &[RequestedServiceSpec],
    parents: &BTreeMap<ServiceName, Parent>,
    edits: &[ImageEdit],
) -> Result<Vec<ployz_store::PostedVersion>, Refused> {
    let children = children((&namespace(), planned), parents, edits)?;
    let signing = DeviceSigning::from_seed(&CI_SEED);
    Ok(posted(children, &signing, Timestamp::from_millis(300_000).unwrap()).unwrap())
}

/// A Server trusting the person's Device, and the CI key for every Service.
fn server_trust() -> TrustedKeys {
    TrustedKeys::new(
        [DeviceKey::from(person().public())],
        [],
        [TrustedGrant {
            grant: Grant {
                key: Signer::from_seed(&CI_SEED).public(),
                kind: GrantKind::Ci,
                scope: KeyScope {
                    caps: CapSet::of(&[Cap::Spec]),
                    places: KeyPlaces::Cluster,
                },
                volumes: Vec::new(),
                issued_at: Timestamp::from_millis(1).unwrap(),
            },
            approved_by: PasskeyApprover::Server,
        }],
    )
}

/// What a Server says to creating `runs` under `versions`.
fn admit(
    versions: &[Signed<ServiceVersion>],
    runs: &RequestedServiceSpec,
) -> Result<version::VersionAdmitted, ployz_core::authority::AuthorityRefusal> {
    let resolved = runs
        .to_resolved(
            ServiceId::parse("c".repeat(32)).unwrap(),
            ResolvedUpdateConfig::default(),
        )
        .unwrap();
    version::verify(
        versions,
        &VersionFacts {
            cluster: Some(SealingPublicKey::from_bytes(CLUSTER)),
            trusted: &server_trust(),
            namespace: &namespace(),
            creation: Creation {
                service: &web(),
                kind: ContainerKind::ServiceContainer,
                spec: &resolved,
            },
        },
        &mut Budget::default(),
    )
}

fn deploy_matches(args: &[&str]) -> clap::ArgMatches {
    let typed = crate::cli::command()
        .try_get_matches_from(["ployz", "deploy"].iter().chain(args))
        .unwrap();
    crate::handlers::leaf_matches(&typed).clone()
}

#[test]
fn an_image_edit_is_signed_as_an_edit_of_the_version_a_person_approved_and_a_server_admits_it() {
    let approved = spec(P1, "a");
    let root = root(&approved);

    let [posted] = sign(
        &[spec("nginx:2", "a")],
        &parents(root.clone(), approved),
        &edit_to_nginx_2(),
    )
    .unwrap()
    .try_into()
    .unwrap();

    let body = posted.statement.open().unwrap();
    assert_eq!(body.parent, Some(root.open().unwrap().digest));
    assert_eq!(body.cluster, SealingPublicKey::from_bytes(CLUSTER));
    assert_eq!(
        posted.pin,
        Some(TagPin {
            tag: "nginx:2".to_owned(),
            image: P2.to_owned()
        })
    );
    let admitted = admit(&[posted.statement.clone(), root.clone()], &spec(P2, "a")).unwrap();
    assert_eq!(
        admitted.by.first().map(|by| by.floor_at),
        Some(root.open().unwrap().signed_at)
    );
    assert!(
        admit(&[posted.statement], &spec(P2, "a")).is_err(),
        "a Server refuses the edit without the version it edits"
    );
}

#[test]
fn a_service_with_no_image_edit_runs_what_its_parent_ran() {
    let approved = spec(P1, "a");

    let [child]: [Child; 1] = children(
        (&namespace(), &[spec("nginx:1", "a")]),
        &parents(root(&approved), approved.clone()),
        &[],
    )
    .unwrap()
    .try_into()
    .unwrap();

    assert_eq!(
        child.digest,
        PayloadDigest::of(&approved, &namespace()).unwrap()
    );
    assert_eq!(child.pin.map(|pin| pin.image), Some(P1.to_owned()));
}

#[test]
fn a_first_version_is_never_signed_without_a_person() {
    let missing = BTreeMap::from([(web(), Parent::Missing { deployed: false })]);

    let refused = sign(&[spec("nginx:2", "a")], &missing, &edit_to_nginx_2()).unwrap_err();

    assert_eq!(
        refused,
        Refused::NoParent {
            service: web(),
            deployed: false
        }
    );
    let error = refused.error(&deploy_matches(&["--yes"]));
    assert_eq!(error.report().code, RpcErrorCode::ConfirmationRequired);
}

#[test]
fn a_parent_spec_that_doesnt_match_what_the_person_signed_signs_nothing() {
    let root = root(&spec(P1, "a"));

    assert_eq!(
        sign(
            &[spec("nginx:2", "b")],
            &parents(root, spec(P1, "b")),
            &edit_to_nginx_2()
        ),
        Err(Refused::Forged(web()))
    );
}

#[test]
fn a_plan_that_swaps_a_volume_is_refused() {
    let approved = mounting(P1, "00112233445566778899aabbccddeeff");

    assert_eq!(
        sign(
            &[mounting("nginx:2", "ffeeddccbbaa99887766554433221100")],
            &parents(root(&approved), approved),
            &edit_to_nginx_2()
        ),
        Err(Refused::Mounts(web()))
    );
}

#[test]
fn a_plan_with_a_change_beyond_the_image_names_the_field_and_signs_nothing() {
    let approved = spec(P1, "a");

    assert_eq!(
        sign(
            &[spec("nginx:2", "b")],
            &parents(root(&approved), approved),
            &edit_to_nginx_2()
        ),
        Err(Refused::Differs {
            service: web(),
            field: "container.environment.MODE".to_owned(),
            image: "nginx:2".to_owned(),
        })
    );
}

/// `ployz set web.image=nginx:2` then `ployz deploy --yes`: the new image is a change
/// nobody approved, so the refusal shows the deploy that names it.
#[test]
fn a_saved_image_with_no_image_edit_refuses_and_names_the_image_edit() {
    let approved = spec(P1, "a");

    let refused = sign(
        &[spec("nginx:2", "a")],
        &parents(root(&approved), approved),
        &[],
    )
    .unwrap_err();

    let error = refused.error(&deploy_matches(&["--yes"]));
    assert_eq!(error.report().code, RpcErrorCode::ConfirmationRequired);
    assert!(
        error.hints().iter().any(|hint| matches!(
            hint,
            Hint::Retry(retry) if retry.ends_with("--image 'web=nginx:2'")
        )),
        "{:?}",
        error.hints()
    );
}

#[test]
fn a_silent_deploy_never_forgets() {
    let error = refuse_forget(&deploy_matches(&["--yes", "--forget-older"])).unwrap_err();

    assert_eq!(error.report().code, RpcErrorCode::ConfirmationRequired);
    assert!(refuse_forget(&deploy_matches(&["--yes"])).is_ok());
}
