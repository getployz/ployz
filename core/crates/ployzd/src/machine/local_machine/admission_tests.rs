//! A Container a CI key admitted, started again after a person changed its grant.

use ployz_core::{
    Namespace, SealedScope, ServiceName, VolumeOrigin,
    authority::{
        Cap, CapSet, Grant, GrantKind, GrantVolume, KeyKind, KeyPlace, KeyPlaces, KeyScope,
        PasskeyApprover, PayloadDigest, PlaceService, SignerKey, TrustedGrant,
    },
};
use serde_json::{Value, json};

use super::super::rule_grant::tests::{Fixture, fixture};
use super::*;

const IMAGE: &str =
    "ployz-build/api@sha256:4444444444444444444444444444444444444444444444444444444444444444";

fn ci() -> ployz_core::authority::Signer {
    ployz_core::authority::Signer::from_seed(&[8; 32])
}

fn app() -> Namespace {
    Namespace::parse("app").unwrap()
}

fn api() -> QualifiedService {
    QualifiedService::parse("app/api").unwrap()
}

/// `app/api`, opening a Shared value sealed for the fixture's Environment.
fn opening_shared(fixture: &Fixture) -> Value {
    json!({
        "name": "api",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {
            "image": IMAGE,
            "pull_policy": "missing",
            "environment": {
                "DB_PASSWORD": fixture.sealing.seal("DB_PASSWORD", SealedScope::Shared, "pa55"),
            },
            "sealed_identity": fixture.sealing.identity,
        },
    })
}

fn birth() -> Value {
    json!({"namespace": "app", "name": "data", "made_at": 1_762_178_640_000_u64,
        "nonce": "00112233445566778899aabbccddeeff"})
}

/// `app/api`, mounting the Provisioned Volume `data`.
fn mounting_data() -> Value {
    json!({
        "name": "api",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": IMAGE, "pull_policy": "missing"},
        "volumes": [{"reference": "data", "source": {"kind": "provisioned", "name": "data",
            "maximum_bytes": 1_073_741_824_u64, "birth": birth()}}],
        "mounts": [{"volume": "data", "target": "/data"}],
    })
}

fn data_entry() -> GrantVolume {
    GrantVolume {
        origin: VolumeOrigin::of(&serde_json::from_value(birth()).unwrap()).unwrap(),
        name: app().volume_name(&ployz_core::DockerVolumeName::parse("data").unwrap()),
    }
}

fn requested(spec: &Value) -> ployz_core::RequestedServiceSpec {
    serde_json::from_value(spec.clone()).unwrap()
}

/// `spec` as a Machine is sent it, its Volumes scoped to `namespace`.
fn resolved(spec: &Value, namespace: &Namespace) -> ResolvedServiceSpec {
    let mut scoped = requested(spec);
    scoped.mount_graph = scoped.mount_graph.scope_to_namespace(namespace).unwrap();
    scoped
        .to_resolved(
            ployz_core::ServiceId::parse("c".repeat(32)).unwrap(),
            ployz_core::ResolvedUpdateConfig::default(),
        )
        .unwrap()
}

fn want(spec: &Value) -> ContainerWant {
    let resolved = resolved(spec, &app());
    serde_json::from_value(
        json!({"state":"running", "rotation":"pending", "container":{"new":{
        "slot":{"namespace":"app", "service":"api", "kind":"service_container", "creation_key":"1"},
        "resolved_spec":resolved, "gen":1}}}),
    )
    .unwrap()
}

/// A CI grant over Namespace `app` of the fixture's Environment, or one Service of it,
/// set by a bundle issued at `issued`.
fn grant(
    fixture: &Fixture,
    service: Option<(uuid::Uuid, &str)>,
    volumes: Vec<GrantVolume>,
    issued: u64,
) -> TrustedGrant {
    TrustedGrant {
        grant: Grant {
            key: ci().public(),
            kind: GrantKind::Ci,
            scope: KeyScope {
                caps: CapSet::of(&[Cap::Spec]),
                places: KeyPlaces::Only(vec![KeyPlace {
                    environment: fixture.sealing.identity.environment,
                    namespace: app(),
                    service: service.map(|(id, name)| PlaceService {
                        id,
                        name: ServiceName::parse(name).unwrap(),
                    }),
                }]),
            },
            volumes,
            name: None,
            issued_at: Timestamp::from_millis(issued).unwrap(),
        },
        approved_by: PasskeyApprover::Server,
    }
}

/// Trust `grant`, over the grant its key holds when it holds one.
async fn trust(fixture: &Fixture, grant: TrustedGrant) {
    fixture
        .machine
        .owner
        .mutate(move |store| {
            let prior = store
                .record()
                .grants()
                .iter()
                .find(|held| held.grant.key == grant.grant.key)
                .map(|held| ployz_core::authority::Prior::Edit {
                    issued_at: held.grant.issued_at,
                });
            store
                .approve_grant(&crate::machine::KeyChangeHeld::for_test(), grant, prior)
                .unwrap()
        })
        .await
        .unwrap();
}

/// `spec` run from another image: what the person last approved before a CI edit.
fn approved(spec: &Value) -> Value {
    let mut approved = spec.clone();
    *approved.pointer_mut("/container/image").unwrap() = json!(
        "ployz-build/api@sha256:5555555555555555555555555555555555555555555555555555555555555555"
    );
    approved
}

/// A version of [`approved`] `spec` the fixture's Device signed at `at`, which a CI edit
/// of `spec` names as its parent.
fn root(
    fixture: &Fixture,
    spec: &Value,
    at: Timestamp,
    forget_older: bool,
) -> Signed<ServiceVersion> {
    version(fixture, &approved(spec), &fixture.signer, at, forget_older)
}

/// A version of `spec` in Namespace `app` the Device `signer` signed at `at`.
fn version(
    fixture: &Fixture,
    spec: &Value,
    signer: &ployz_core::authority::Signer,
    at: Timestamp,
    forget_older: bool,
) -> Signed<ServiceVersion> {
    let body = ServiceVersion {
        cluster: fixture.cluster,
        namespace: app(),
        service: ServiceName::parse("api").unwrap(),
        digest: PayloadDigest::of(&requested(spec), &app()).unwrap(),
        signed_at: at,
        signer: SignerKey::Device(signer.public().into()),
        proposal: None,
        forget_older,
        parent: None,
    };
    Signed::sign(&body, signer).unwrap()
}

/// Admit `spec` under the CI key's edit of `root`, signed at `at`, as an Operator's
/// deploy does, with `root` presented beside it.
async fn admit_ci(
    fixture: &Fixture,
    spec: &Value,
    root: &Signed<ServiceVersion>,
    at: Timestamp,
) -> Result<AdmittedBy, Error> {
    let body = ServiceVersion {
        cluster: fixture.cluster,
        namespace: app(),
        service: ServiceName::parse("api").unwrap(),
        digest: PayloadDigest::of(&requested(spec), &app()).unwrap(),
        signed_at: at,
        signer: SignerKey::Device(ci().public().into()),
        proposal: None,
        forget_older: false,
        parent: Some(root.open().unwrap().digest),
    };
    let versions = [Signed::sign(&body, &ci()).unwrap(), root.clone()];
    let admitted = fixture
        .machine
        .clone()
        .at_operator_scope()
        .admit_spec(&want(spec), None, &versions, Admit::Record)
        .await?;
    Ok(admitted.expect("a creation").by().clone())
}

/// Admit `spec` under a CI edit of a version the person signed just now.
async fn admitted_by_ci(fixture: &Fixture, spec: &Value) -> AdmittedBy {
    let now = Timestamp::now();
    admit_ci(fixture, spec, &root(fixture, spec, now, false), now)
        .await
        .unwrap()
}

/// What `start_checked` decides for an Operator's start of a Container kept under `by`.
fn start(fixture: &Fixture, by: &AdmittedBy) -> Result<(), AuthorityRefusal> {
    fixture
        .machine
        .standing()
        .admits(by, Error::ApprovalRecord)
        .unwrap()
}

fn out_of_scope(what: &'static str) -> AuthorityRefusal {
    AuthorityRefusal::OutOfScope {
        code: ci().public().code(),
        kind: KeyKind::Ci,
        what,
    }
}

#[tokio::test]
async fn a_kept_start_stops_once_the_grant_narrows_to_its_service_or_moves_to_another() {
    let fixture = fixture().await;
    let service = fixture.sealing.identity.service;
    trust(&fixture, grant(&fixture, None, Vec::new(), 1)).await;
    let kept = admitted_by_ci(&fixture, &opening_shared(&fixture)).await;
    assert_eq!(start(&fixture, &kept), Ok(()));

    trust(
        &fixture,
        grant(&fixture, Some((service, "api")), Vec::new(), 2),
    )
    .await;
    assert_eq!(
        start(&fixture, &kept),
        Err(out_of_scope("Sealed values outside its places")),
        "a Service place holds no Shared value it opened"
    );

    trust(&fixture, grant(&fixture, None, Vec::new(), 3)).await;
    assert_eq!(start(&fixture, &kept), Ok(()), "widened again by a person");

    trust(
        &fixture,
        grant(&fixture, Some((service, "web")), Vec::new(), 4),
    )
    .await;
    assert_eq!(
        start(&fixture, &kept),
        Err(out_of_scope("a version of this Service"))
    );
}

/// A rotation removes the old key once the new one is in place, and `key rm` removes a
/// leaked one. A Container only that key admitted no longer starts, and its refusal says
/// why and what heals it.
#[tokio::test]
async fn a_kept_start_after_its_key_is_removed_says_the_key_went_and_to_deploy_again() {
    let fixture = fixture().await;
    trust(&fixture, grant(&fixture, None, Vec::new(), 1)).await;
    let kept = admitted_by_ci(&fixture, &opening_shared(&fixture)).await;
    assert_eq!(start(&fixture, &kept), Ok(()));

    remove_ci(&fixture).await;
    let refused = start(&fixture, &kept).unwrap_err();
    assert_eq!(
        super::refusal_text(&refused),
        "The key that approved this was rotated or removed; deploy again."
    );
}

/// A CI edit that runs exactly the spec a person signed still starts once the CI key is
/// removed: the person signed this very spec.
#[tokio::test]
async fn a_kept_start_of_the_spec_a_person_signed_survives_its_key_removal() {
    let fixture = fixture().await;
    trust(&fixture, grant(&fixture, None, Vec::new(), 1)).await;
    let kept = admitted_by_ci(&fixture, &approved(&opening_shared(&fixture))).await;

    remove_ci(&fixture).await;
    assert_eq!(start(&fixture, &kept), Ok(()));
}

async fn remove_ci(fixture: &Fixture) {
    fixture
        .machine
        .owner
        .mutate(|store| {
            store
                .remove_key(
                    &crate::machine::KeyChangeHeld::for_test(),
                    ployz_core::authority::KeyRef::Signing {
                        key: ci().public().into(),
                    },
                )
                .unwrap()
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_kept_start_stops_once_the_grant_drops_its_volume() {
    let fixture = fixture().await;
    trust(&fixture, grant(&fixture, None, vec![data_entry()], 1)).await;
    let kept = admitted_by_ci(&fixture, &mounting_data()).await;
    assert_eq!(start(&fixture, &kept), Ok(()));

    trust(&fixture, grant(&fixture, None, Vec::new(), 2)).await;
    assert_eq!(
        start(&fixture, &kept),
        Err(AuthorityRefusal::VolumeNotGranted {
            code: ci().public().code(),
            kind: KeyKind::Ci,
            volume: data_entry().name,
        })
    );
}

#[tokio::test]
async fn a_container_kept_before_its_reach_starts_for_a_device_but_not_a_grant() {
    let fixture = fixture().await;
    trust(&fixture, grant(&fixture, None, Vec::new(), 1)).await;
    let kept_by = |key: ployz_core::authority::SigningPublicKey| AdmittedBy::Keys {
        service: api(),
        keys: vec![(KeptKey::Device(key), Timestamp::now())],
        reach: None,
    };

    assert_eq!(start(&fixture, &kept_by(fixture.signer.public())), Ok(()));
    assert_eq!(
        start(&fixture, &kept_by(ci().public())),
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::KeptReach
        })
    );
}

#[tokio::test]
async fn a_ci_edit_is_forgotten_with_the_version_it_edits_even_when_signed_after_the_forget() {
    let fixture = fixture().await;
    trust(&fixture, grant(&fixture, None, Vec::new(), 1)).await;
    let spec = opening_shared(&fixture);
    let now = Timestamp::now().as_millis();
    let at = |ago: u64| Timestamp::from_millis(now - ago).unwrap();
    let old_root = root(&fixture, &spec, at(60_000), false);
    let forget = root(&fixture, &spec, at(30_000), true);
    fixture
        .machine
        .clone()
        .at_operator_scope()
        .admit_spec(
            &want(&approved(&spec)),
            None,
            std::slice::from_ref(&forget),
            Admit::Record,
        )
        .await
        .unwrap();

    let refused = admit_ci(&fixture, &spec, &old_root, at(0)).await;
    assert!(
        matches!(
            refused,
            Err(Error::DeployRefused(AuthorityRefusal::Forgotten { floor })) if floor == at(30_000)
        ),
        "{refused:?}"
    );

    let AdmittedBy::Keys { keys, .. } = admit_ci(&fixture, &spec, &forget, at(0)).await.unwrap()
    else {
        panic!("kept under keys");
    };
    assert_eq!(keys, vec![(KeptKey::Device(ci().public()), at(30_000))]);
}

/// A Move's Create in Namespace `app` of `resolved_spec`, carrying `versions`.
fn create(resolved_spec: &ResolvedServiceSpec, versions: &[Signed<ServiceVersion>]) -> Start {
    serde_json::from_value(json!({"kind": "create", "namespace": "app",
        "resolved_spec": resolved_spec, "creation_key": "move-1", "gen": 2,
        "rotation": "pending", "versions": versions}))
    .unwrap()
}

/// `start` asked only as a check.
fn checking(mut start: Start) -> Start {
    if let Start::Create { check, .. } = &mut start {
        *check = true;
    }
    start
}

/// What an Operator's start of `start` on Volume `volume` is admitted under.
async fn start_on(
    machine: &LocalMachine,
    volume: &str,
    start: &Start,
) -> Result<AdmittedBy, AuthorityRefusal> {
    match machine
        .clone()
        .at_operator_scope()
        .admit_volume_start(&volume.parse().unwrap(), start)
        .await
    {
        Ok(admitted) => Ok(admitted.expect("a Create's admission").by().clone()),
        Err(Error::DeployRefused(refusal)) => Err(refusal),
        Err(error) => panic!("not a refusal: {error:?}"),
    }
}

fn mismatch(field: MismatchField) -> Result<AdmittedBy, AuthorityRefusal> {
    Err(AuthorityRefusal::Mismatch { field })
}

#[tokio::test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture")]
async fn a_moved_volume_starts_only_the_signed_spec_that_mounts_it() {
    let fixture = fixture().await;
    let spec = mounting_data();
    let signed = [version(
        &fixture,
        &spec,
        &fixture.signer,
        Timestamp::now(),
        false,
    )];
    let start = create(&resolved(&spec, &app()), &signed);

    assert!(matches!(
        start_on(&fixture.machine, "app_data", &start).await,
        Ok(AdmittedBy::Keys { .. })
    ));
    assert_eq!(
        start_on(&fixture.machine, "app_logs", &start).await,
        mismatch(MismatchField::StartedVolume),
        "a signature of a spec opens no other Volume"
    );
    assert_eq!(
        start_on(
            &fixture.machine,
            "app_data",
            &create(&resolved(&approved(&spec), &app()), &signed)
        )
        .await,
        mismatch(MismatchField::Payload),
        "a changed field"
    );
    assert_eq!(
        start_on(
            &fixture.machine,
            "app_data",
            &create(&resolved(&spec, &app()), &[])
        )
        .await,
        mismatch(MismatchField::VolumeStart),
        "no versions"
    );

    let mut declared = spec.clone();
    declared["mounts"] = json!([]);
    let signed = [version(
        &fixture,
        &declared,
        &fixture.signer,
        Timestamp::now(),
        false,
    )];
    assert_eq!(
        start_on(
            &fixture.machine,
            "app_data",
            &create(&resolved(&declared, &app()), &signed)
        )
        .await,
        mismatch(MismatchField::StartedVolume),
        "a Volume the spec declares and doesn't mount"
    );
}

/// Environments share a Volume's name and differ by Namespace. A Legacy Volume has no birth
/// to tell them apart, only its physical name.
#[tokio::test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture")]
async fn a_signature_in_one_environment_opens_no_volume_of_the_same_name_in_another() {
    let fixture = fixture().await;
    let other = Namespace::parse("shop").unwrap();
    let mut legacy = mounting_data();
    legacy["volumes"][0]["source"]
        .as_object_mut()
        .unwrap()
        .remove("birth");
    for spec in [mounting_data(), legacy] {
        let signed = [version(
            &fixture,
            &spec,
            &fixture.signer,
            Timestamp::now(),
            false,
        )];
        assert_eq!(
            start_on(
                &fixture.machine,
                "shop_data",
                &create(&resolved(&spec, &app()), &signed)
            )
            .await,
            mismatch(MismatchField::StartedVolume)
        );
        assert_eq!(
            start_on(
                &fixture.machine,
                "shop_data",
                &create(&resolved(&spec, &other), &signed)
            )
            .await,
            mismatch(MismatchField::Payload),
            "the spec scoped to the other Environment's Volume is not the one signed"
        );
    }
}

#[tokio::test]
async fn a_moved_volume_starts_no_more_once_its_signer_is_removed() {
    let fixture = fixture().await;
    let laptop = ployz_core::authority::Signer::from_seed(&[9; 32]);
    let key = laptop.public();
    fixture
        .machine
        .owner
        .mutate(move |store| store.approve_device(key.into()).unwrap())
        .await
        .unwrap();
    let spec = mounting_data();
    let start = create(
        &resolved(&spec, &app()),
        &[version(&fixture, &spec, &laptop, Timestamp::now(), false)],
    );
    assert!(start_on(&fixture.machine, "app_data", &start).await.is_ok());

    fixture
        .machine
        .owner
        .mutate(move |store| {
            store
                .remove_key(
                    &crate::machine::KeyChangeHeld::for_test(),
                    ployz_core::authority::KeyRef::Signing { key: key.into() },
                )
                .unwrap()
        })
        .await
        .unwrap();
    assert_eq!(
        start_on(&fixture.machine, "app_data", &start).await,
        Err(AuthorityRefusal::UnknownKey { key })
    );
}

/// Cloud asks before it cuts whether the destination would start the Move. The answer
/// must not raise a floor a forget carries, or the asking alone would stop older kept
/// Containers of the Service.
#[tokio::test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture")]
async fn a_checked_start_answers_as_a_start_and_keeps_nothing() {
    let fixture = fixture().await;
    let machine = fixture.machine.clone().with_zpool("true");
    let spec = mounting_data();
    let start = create(
        &resolved(&spec, &app()),
        &[version(
            &fixture,
            &spec,
            &fixture.signer,
            Timestamp::now(),
            true,
        )],
    );
    let floor = || {
        fixture
            .machine
            .approvals
            .floor(
                fixture.cluster,
                &api(),
                Timestamp::now(),
                Error::ApprovalRecord,
            )
            .unwrap()
            .0
    };

    assert!(
        start_on(&machine, "app_data", &checking(start.clone()))
            .await
            .is_ok()
    );
    assert_eq!(floor(), None);

    let mut placed = spec.clone();
    placed["placement"] = json!({"constraints": ["node.labels.region==eu"]});
    let elsewhere = checking(create(
        &resolved(&placed, &app()),
        &[version(
            &fixture,
            &placed,
            &fixture.signer,
            Timestamp::now(),
            false,
        )],
    ));
    let refused = machine
        .clone()
        .at_operator_scope()
        .admit_volume_start(&"app_data".parse().unwrap(), &elsewhere)
        .await;
    assert!(
        matches!(
            refused,
            Err(Error::Docker(
                crate::docker::Error::ServicePlacementMismatch
            ))
        ),
        "{refused:?}"
    );

    assert!(start_on(&machine, "app_data", &start).await.is_ok());
    assert!(floor().is_some(), "the start itself keeps the forget");
}

/// An Operator's unsigned create of a Service that reaches no data runs, and so does a
/// create through this Server's own socket, until its name has run signed: then both are
/// refused, with no signed copy left running. The socket's copy would answer for nothing.
#[tokio::test]
async fn an_unsigned_create_under_a_name_that_ran_signed_is_refused() {
    let fixture = fixture().await;
    let (replicated, _server) = crate::corrosion::fake_cluster::store().await;
    let machine = fixture.machine.clone().with_cluster(Some((
        replicated.clone(),
        crate::corrosion::AdminClient::new("/no/such/admin.sock"),
    )));
    let plain = json!({
        "name": "api",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": IMAGE, "pull_policy": "missing"},
    });
    let operator = machine.clone().at_operator_scope();
    let socket = machine.clone().on_local_socket();
    let want = want(&plain);
    assert!(matches!(
        operator
            .admit_spec(&want, None, &[], Admit::Check)
            .await
            .unwrap()
            .map(|admitted| admitted.by().clone()),
        Some(AdmittedBy::Free)
    ));
    assert!(matches!(
        socket
            .admit_spec(&want, None, &[], Admit::Check)
            .await
            .unwrap()
            .map(|admitted| admitted.by().clone()),
        Some(AdmittedBy::Local { .. })
    ));

    replicated.keep_signed_service(&api()).await.unwrap();
    for (caller, machine) in [("an Operator", &operator), ("the socket", &socket)] {
        assert!(
            matches!(
                machine.admit_spec(&want, None, &[], Admit::Check).await,
                Err(Error::DeployRefused(AuthorityRefusal::SignedName { ref service })) if *service == api()
            ),
            "a name kept signed takes no Container {caller} admits unsigned"
        );
    }
}

/// Cloud asks the Server a Move would start a Writer on before it cuts. A preview's
/// Writer is refused there on any Server but its home, so no cut or promote runs for a
/// Container its create would refuse.
#[tokio::test]
async fn a_checked_start_of_another_servers_preview_is_refused_before_any_cut() {
    let fixture = fixture().await;
    let elsewhere = Namespace::parse("pv-0123456789abcdef-00000000-00c0ffee").unwrap();
    let start: Start = serde_json::from_value(json!({"kind": "create",
        "namespace": elsewhere, "resolved_spec": resolved(&mounting_data(), &elsewhere),
        "creation_key": "move-1", "gen": 2, "rotation": "pending", "versions": []}))
    .unwrap();
    let volume = format!("{elsewhere}_data");
    for start in [checking(start.clone()), start] {
        let refused = fixture
            .machine
            .admit_volume_start(&volume.parse().unwrap(), &start)
            .await;
        assert!(
            matches!(
                refused,
                Err(Error::PreviewRefused(
                    ployz_core::PreviewRefusal::Elsewhere { .. }
                ))
            ),
            "{refused:?}"
        );
    }
}

/// A Trash entry holding another Server's preview's data is never put back here: the
/// plugin is asked for no restore at all.
#[tokio::test]
async fn a_trash_restore_of_another_servers_preview_changes_nothing() {
    let fixture = fixture().await;
    let dir = crate::test_dir::TestDir::new("preview-trash");
    std::fs::create_dir_all(&dir.0).unwrap();
    let plugin = crate::storage::test_support::FakePlugin::default();
    let id = "a".repeat(26);
    plugin.reply(
        crate::volume_copy::TRASH_LIST_ROUTE,
        json!({"Ok": {"entries": [{"id": id, "place": "root",
            "name": "pv-0123456789abcdef-00000000-00c0ffee_data", "trashed_at": 0,
            "expires_at": 0, "used_bytes": 0}]}}),
    );
    plugin.reply(
        crate::volume_copy::TRASH_RESTORE_ROUTE,
        json!({"Ok": {"id": id, "name": "pv-0123456789abcdef-00000000-00c0ffee_data",
            "place": "root"}}),
    );
    let (served, _server) = plugin.serve(&dir.0);
    let machine = fixture.machine.clone().with_plugin(served);
    let request = ployz_core::RestoreTrashRequest {
        id: ployz_core::TrashId::parse(&id).unwrap(),
        promote: false,
        approval: None,
    };
    let refused = machine.restore_trash(&request).await;
    assert!(
        matches!(refused, Err(Error::PreviewRefused(_))),
        "{refused:?}"
    );
    let calls = plugin.calls.lock().unwrap().clone();
    assert!(
        calls
            .iter()
            .all(|(route, _)| route != crate::volume_copy::TRASH_RESTORE_ROUTE),
        "{calls:?}"
    );
}

/// An entry the Trash lists as unreadable, or not at all, could hold any Server's preview,
/// so its restore never reaches the plugin.
#[tokio::test]
async fn a_trash_restore_of_an_entry_not_listed_readable_changes_nothing() {
    let fixture = fixture().await;
    let dir = crate::test_dir::TestDir::new("preview-trash-unread");
    std::fs::create_dir_all(&dir.0).unwrap();
    let plugin = crate::storage::test_support::FakePlugin::default();
    let id = "b".repeat(26);
    plugin.reply(
        crate::volume_copy::TRASH_LIST_ROUTE,
        json!({"Ok": {"entries": []}}),
    );
    plugin.reply(
        crate::volume_copy::TRASH_RESTORE_ROUTE,
        json!({"Ok": {"id": id, "name": "pv-0123456789abcdef-00000000-00c0ffee_data",
            "place": "root"}}),
    );
    let (served, _server) = plugin.serve(&dir.0);
    let machine = fixture.machine.clone().with_plugin(served);
    let request = ployz_core::RestoreTrashRequest {
        id: ployz_core::TrashId::parse(&id).unwrap(),
        promote: false,
        approval: None,
    };
    let refused = machine.restore_trash(&request).await;
    assert!(
        matches!(
            refused,
            Err(Error::PreviewRefused(
                ployz_core::PreviewRefusal::UnreadTrash { .. }
            ))
        ),
        "{refused:?}"
    );
    let calls = plugin.calls.lock().unwrap().clone();
    assert!(
        calls
            .iter()
            .all(|(route, _)| route != crate::volume_copy::TRASH_RESTORE_ROUTE),
        "{calls:?}"
    );
}

/// A Device's approval to empty `ids` in the Trash of `machine`, signed just now.
fn trash_approval(
    fixture: &Fixture,
    machine: ployz_core::MachineId,
    ids: &[ployz_core::TrashId],
) -> Signed<ployz_core::authority::TrashApproval> {
    let body = ployz_core::authority::TrashApproval::drafted(&ployz_core::authority::TrashTerms {
        cluster: fixture.cluster,
        machine,
        action: ployz_core::authority::TrashAction::Empty,
        ids: ids.to_vec(),
        issued_at: Timestamp::now(),
        signer: SignerKey::Device(fixture.signer.public().into()),
    });
    Signed::sign(&body, &fixture.signer).unwrap()
}

/// An Operator empties only the entries a person approved on this Server: no approval, an
/// approval of other entries or of another Server reaches no plugin, and the approved one does.
#[tokio::test]
async fn an_operators_trash_empty_takes_only_an_approval_of_those_entries_here() {
    let fixture = fixture().await;
    let dir = crate::test_dir::TestDir::new("trash-approval");
    std::fs::create_dir_all(&dir.0).unwrap();
    let plugin = crate::storage::test_support::FakePlugin::default();
    let picked = ployz_core::TrashId::parse("a".repeat(26)).unwrap();
    let other = ployz_core::TrashId::parse("b".repeat(26)).unwrap();
    plugin.reply(
        crate::volume_copy::TRASH_EMPTY_ROUTE,
        json!({"Ok": {"emptied": [picked.to_string()]}}),
    );
    let (served, _server) = plugin.serve(&dir.0);
    let machine = fixture
        .machine
        .clone()
        .with_plugin(served)
        .at_operator_scope();
    let here = fixture.machine.record().id();
    let elsewhere = ployz_core::MachineId::parse("5".repeat(32)).unwrap();
    let empty = |approval| ployz_core::EmptyTrashRequest {
        ids: vec![picked],
        bulk: false,
        approval,
    };
    for (approval, field) in [
        (None, None),
        (
            Some(trash_approval(&fixture, here, &[picked, other])),
            Some(ployz_core::authority::MismatchField::TrashEntries),
        ),
        (
            Some(trash_approval(&fixture, elsewhere, &[picked])),
            Some(ployz_core::authority::MismatchField::ThisMachine),
        ),
    ] {
        let refused = machine.empty_trash(&empty(approval)).await;
        match field {
            None => assert!(
                matches!(
                    refused,
                    Err(Error::TrashRefused(AuthorityRefusal::Unsigned { .. }))
                ),
                "{refused:?}"
            ),
            Some(field) => assert!(
                matches!(refused, Err(Error::TrashRefused(AuthorityRefusal::Mismatch { field: got })) if got == field),
                "{refused:?}"
            ),
        }
    }
    assert!(plugin.calls.lock().unwrap().is_empty());
    let emptied = machine
        .empty_trash(&empty(Some(trash_approval(&fixture, here, &[picked]))))
        .await
        .unwrap();
    assert_eq!(emptied.emptied, vec![picked]);
}
