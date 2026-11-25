use super::*;
use crate::storage::test_support::FakePlugin;
use crate::volume_copy::{
    CUT_ROUTE, INSPECT_ROUTE, PREVIEW_CLOSE_ROUTE, VOLUME_SEED_ROUTE, WRITER_ROUTE,
};
use ployz_core::{DockerVolumeName, Gen, Namespace, SetVolumeCopyReply};

const VOLUME: &str = "app_data";

fn writer_root(epoch: u32, newest: Option<Value>) -> Value {
    json!({"Ok": {"kind":"root", "readonly":false,
        "newest":newest, "epoch":epoch, "seed":false}})
}

fn slot(newest: Option<Value>) -> Value {
    json!({"Ok": {"kind":"slot", "slot":"1", "readonly":true,
        "newest":newest, "resume_token":null, "pull":null}})
}

fn snapshot() -> Value {
    json!({"name":"f-1", "guid":"7", "created_unix_seconds":1_700_000_000})
}

fn plugin(before: Value, route: &str, after: Value) -> FakePlugin {
    let plugin = FakePlugin::default();
    plugin.reply(INSPECT_ROUTE, before);
    plugin.reply(route, after.clone());
    let replies = plugin.replies.clone();
    plugin.on(route, move || {
        replies
            .lock()
            .unwrap()
            .insert(INSPECT_ROUTE.into(), after.clone());
    });
    plugin
}

impl Fleet {
    fn with_plugin(&self, index: usize, plugin: &FakePlugin) -> MachineService {
        let directory = self.dir.join(format!("plugin-{}", MachineId::random()));
        std::fs::create_dir_all(&directory).unwrap();
        let (socket, _server) = plugin.serve(&directory);
        self.node(index).service.clone().with_volume_plugin(socket)
    }
}

async fn set_copy(service: &MachineService, request: Value) -> SetVolumeCopyReply {
    try_set_copy(service, request)
        .await
        .unwrap_or_else(|error| panic!("{error}"))
}

async fn try_set_copy(
    service: &MachineService,
    request: Value,
) -> Result<SetVolumeCopyReply, String> {
    let response = service
        .set_volume_copy(Request::new(
            op::SetVolumeCopy::into_request(serde_json::from_value(request).unwrap())
                .encode()
                .unwrap(),
        ))
        .await
        .map_err(|status| status.message().to_owned())?
        .into_inner()
        .decode_response()
        .unwrap();
    response
        .clone()
        .decode::<op::SetVolumeCopy>()
        .map_err(|error| format!("{error}: {response:?}"))
}

async fn cut(fleet: &Fleet, index: usize) {
    let source = fleet.with_plugin(
        index,
        &plugin(writer_root(0, None), CUT_ROUTE, slot(Some(snapshot()))),
    );
    set_copy(
        &source,
        json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":0},
            "want":{"kind":"mirror", "from":null, "target":"f-1", "drain":0}}),
    )
    .await;
}

fn only_container(node: &Node) -> ContainerId {
    let named = node.fake.named_containers.as_ref().unwrap().lock().unwrap();
    let [container] = named.values().collect::<Vec<_>>()[..] else {
        panic!("expected one Container, got {named:?}");
    };
    container["Id"].as_str().unwrap().parse().unwrap()
}

#[tokio::test]
async fn moved_service_stays_routed() {
    let fleet = Fleet::start().await;
    let source = fleet.routed(0, "a", "api", 2).await;

    cut(&fleet, 0).await;
    assert!(!fleet.node(0).running(source), "the cut stops the holder");
    assert!(
        !fleet
            .node(0)
            .runtime
            .observation()
            .inspect_managed(&source, &fleet.node(0).row.id)
            .await
            .unwrap()
            .stopped,
        "a move's drain is no stop on request"
    );

    // As the dashboard's Move does: the source copy's gen, and In because it was In.
    let destination = fleet.with_plugin(
        1,
        &plugin(
            slot(Some(snapshot())),
            WRITER_ROUTE,
            writer_root(1, Some(snapshot())),
        ),
    );
    set_copy(
        &destination,
        json!({"name":VOLUME, "expect":{"kind":"mirror", "newest":"7", "slot":"1"},
            "want":{"kind":"writer", "at":"7", "epoch":1, "start":{
                "kind":"create", "namespace":"app", "resolved_spec":spec("api"),
                "creation_key":"move-1", "gen":2,
                "rotation":{"in":{"run":null, "expect":"pending"}}}}}),
    )
    .await;

    let moved = only_container(fleet.node(1));
    assert!(fleet.node(1).running(moved));
    fleet.published(moved, Some(CopyRotation::In)).await;
    let row = fleet.store.container(&moved).await.unwrap().unwrap();
    assert_eq!(row.copy_gen(), Gen::new(2));

    // The Move's removal of the old holder, as a user remove of an In copy.
    ensured(&ensure(fleet.node(0), out(source, true, Some(CopyRotation::In))).await);
    assert_eq!(fleet.node(0).rotation(source).await, CopyRotation::Out);
    let removed = ensure(fleet.node(0), gone(source)).await;
    assert!(
        matches!(
            removed,
            EnsuredContainer::Ensured {
                now: ContainerNow::Absent { .. },
                ..
            }
        ),
        "{removed:?}"
    );
}

#[tokio::test]
async fn unhealthy_destination_keeps_source_and_fails_run() {
    let fleet = Fleet::start().await;
    let source = fleet.routed(0, "a", "api", 2).await;
    cut(&fleet, 0).await;

    let mut checked = checked_spec("api");
    if let Some(HealthcheckSpec::Configured(check)) = &mut checked.container.healthcheck {
        check.deadline_millis = Some(100);
    }
    let destination = fleet.with_plugin(
        1,
        &plugin(
            slot(Some(snapshot())),
            WRITER_ROUTE,
            writer_root(1, Some(snapshot())),
        ),
    );
    let create = json!({"name":VOLUME, "expect":{"kind":"mirror", "newest":"7", "slot":"1"},
        "want":{"kind":"writer", "at":"7", "epoch":1, "start":{
            "kind":"create", "namespace":"app", "resolved_spec":checked,
            "creation_key":"move-1", "gen":2,
            "rotation":{"in":{"run":null, "expect":"pending"}}}}});

    let failed = try_set_copy(&destination, create.clone())
        .await
        .expect_err("an unrouted copy fails the start");
    assert!(failed.contains("not routed"), "{failed}");
    let moved = only_container(fleet.node(1));
    assert!(fleet.node(1).running(moved));
    assert_eq!(fleet.node(1).rotation(moved).await, CopyRotation::Pending);
    assert!(
        fleet.node(0).container(source).is_some(),
        "the source is kept"
    );
    assert_eq!(fleet.node(0).rotation(source).await, CopyRotation::In);

    fleet.node(1).set_state(
        moved,
        json!({"Status": "running", "Health": {"Status": "healthy"}}),
    );
    set_copy(&destination, create).await;
    fleet.published(moved, Some(CopyRotation::In)).await;
}

/// A destination that exits right after Docker starts it is not In, so the Move keeps
/// its source: success here would let the run remove the only startable copy.
#[tokio::test]
async fn a_destination_that_exits_after_start_fails_and_keeps_the_source() {
    let fleet = Fleet::start().await;
    let source = fleet.routed(0, "a", "api", 2).await;
    cut(&fleet, 0).await;

    fleet
        .node(1)
        .fake
        .exit_on_start
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let destination = fleet.with_plugin(
        1,
        &plugin(
            slot(Some(snapshot())),
            WRITER_ROUTE,
            writer_root(1, Some(snapshot())),
        ),
    );
    let failed = try_set_copy(
        &destination,
        json!({"name":VOLUME, "expect":{"kind":"mirror", "newest":"7", "slot":"1"},
            "want":{"kind":"writer", "at":"7", "epoch":1, "start":{
                "kind":"create", "namespace":"app", "resolved_spec":spec("api"),
                "creation_key":"move-1", "gen":2,
                "rotation":{"in":{"run":null, "expect":"pending"}}}}}),
    )
    .await
    .expect_err("a copy that exited is not In");
    assert!(failed.contains("not routed"), "{failed}");

    let moved = only_container(fleet.node(1));
    assert!(!fleet.node(1).running(moved));
    assert_eq!(fleet.node(1).rotation(moved).await, CopyRotation::Pending);
    assert!(
        fleet.node(0).container(source).is_some(),
        "the source is kept"
    );
    assert_eq!(fleet.node(0).rotation(source).await, CopyRotation::In);
}

/// The cut left its rotation alone, so it routes again; an Out written by the cut
/// would be final.
#[tokio::test]
async fn a_release_after_the_cut_routes_the_kept_copy_again() {
    let fleet = Fleet::start().await;
    let source = fleet.routed(0, "a", "api", 1).await;
    cut(&fleet, 0).await;

    let released = fleet.with_plugin(
        0,
        &plugin(
            slot(Some(snapshot())),
            WRITER_ROUTE,
            writer_root(1, Some(snapshot())),
        ),
    );
    set_copy(
        &released,
        json!({"name":VOLUME, "expect":{"kind":"mirror", "newest":"7", "slot":"1"},
            "want":{"kind":"writer", "at":"7", "epoch":1,
                "start":{"kind":"kept", "container_id":source}}}),
    )
    .await;

    assert!(fleet.node(0).running(source));
    assert_eq!(fleet.node(0).rotation(source).await, CopyRotation::In);
}

/// A Container `docker run` made on the Volume, with no Ployz labels, on Machine
/// `index`, shaped like the Service on Machine 0.
fn add_stray(fleet: &Fleet, index: usize, status: &str) {
    let named = |node: usize| {
        fleet
            .node(node)
            .fake
            .named_containers
            .as_ref()
            .unwrap()
            .clone()
    };
    let mut stray = named(0).lock().unwrap().values().next().unwrap().clone();
    stray["Id"] = json!(format!("{}{}", MachineId::random(), MachineId::random()));
    stray["Name"] = json!("/stray");
    stray["Config"]["Labels"] = json!({});
    stray["State"] = json!({"Status":status});
    named(index).lock().unwrap().insert("stray".into(), stray);
}

/// A Release resent after A became the Writer again, while a helper Ployz didn't make
/// runs on the Volume: the helper is not the kept Service, so the resend starts it.
#[tokio::test]
async fn a_release_resent_beside_a_running_stray_starts_the_kept_copy() {
    let fleet = Fleet::start().await;
    let source = fleet.routed(0, "a", "api", 1).await;
    cut(&fleet, 0).await;
    add_stray(&fleet, 0, "running");
    let released = fleet.with_plugin(
        0,
        &plugin(
            writer_root(1, Some(snapshot())),
            WRITER_ROUTE,
            writer_root(1, Some(snapshot())),
        ),
    );

    set_copy(
        &released,
        json!({"name":VOLUME, "expect":{"kind":"mirror", "newest":"7", "slot":"1"},
            "want":{"kind":"writer", "at":"7", "epoch":1,
                "start":{"kind":"kept", "container_id":source}}}),
    )
    .await;

    assert!(fleet.node(0).running(source));
}

/// `name` mounting the Provisioned Volume the Move copies.
fn on_volume(name: &str) -> ResolvedServiceSpec {
    let mut on_volume = spec_with_sources(vec![crate::docker::test_support::provisioned_source(
        "data",
        1_073_741_824,
    )]);
    on_volume.name = spec(name).name;
    on_volume.update.monitor_millis = Some(0);
    on_volume
}

fn cut_naming(holder: ContainerId) -> Value {
    json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":0},
        "want":{"kind":"mirror", "from":null, "target":"f-1", "drain":0, "holder":holder}})
}

/// A stopped Container Ployz didn't make, left on the Volume, neither blocks the cut
/// nor is stopped by it.
#[tokio::test]
async fn a_cut_naming_its_holder_passes_a_stopped_stray() {
    let fleet = Fleet::start().await;
    let source = fleet.routed(0, "a", "api", 2).await;
    add_stray(&fleet, 0, "exited");
    let plugin = plugin(writer_root(0, None), CUT_ROUTE, slot(Some(snapshot())));
    let machine = fleet.with_plugin(0, &plugin);

    let cut = set_copy(&machine, cut_naming(source)).await;

    assert!(
        matches!(cut.copy, Some(ployz_core::VolumeCopy::Slot { .. })),
        "{cut:?}"
    );
    assert!(!fleet.node(0).running(source));
}

/// A running Container Ployz didn't make refuses the cut before the Service stops.
#[tokio::test]
async fn a_cut_naming_its_holder_refuses_a_running_stray() {
    let fleet = Fleet::start().await;
    let source = fleet.routed(0, "a", "api", 2).await;
    add_stray(&fleet, 0, "running");
    let plugin = plugin(writer_root(0, None), CUT_ROUTE, slot(Some(snapshot())));
    let machine = fleet.with_plugin(0, &plugin);

    let refused = try_set_copy(&machine, cut_naming(source))
        .await
        .expect_err("a running stray refuses the cut");

    assert!(refused.contains("stray"), "{refused}");
    assert!(refused.contains("\"held\""), "{refused}");
    assert!(fleet.node(0).running(source), "the cut stopped nothing");
}

/// A start resent after B became the Writer, its Container not yet made, while a
/// helper Ployz didn't make runs on the Volume: the helper is not the Service, so the
/// start is no success, and Cloud keeps A's Container for the retry.
#[tokio::test]
async fn a_start_resent_beside_a_running_stray_is_no_success() {
    let fleet = Fleet::start().await;
    let source = fleet.routed(0, "a", "api", 2).await;
    cut(&fleet, 0).await;
    add_stray(&fleet, 1, "running");
    let destination = fleet.with_plugin(
        1,
        &plugin(
            writer_root(1, Some(snapshot())),
            WRITER_ROUTE,
            writer_root(1, Some(snapshot())),
        ),
    );

    let refused = try_set_copy(
        &destination,
        json!({"name":VOLUME, "expect":{"kind":"mirror", "newest":"7", "slot":"1"},
            "want":{"kind":"writer", "at":"7", "epoch":1, "start":{
                "kind":"create", "namespace":"app", "resolved_spec":on_volume("api"),
                "creation_key":"move-1", "gen":2,
                "rotation":{"in":{"run":null, "expect":"pending"}}}}}),
    )
    .await
    .expect_err("a running stray is not the moved Service");

    assert!(refused.contains("stray"), "{refused}");
    assert!(
        fleet.node(0).container(source).is_some(),
        "the source is kept"
    );
}

/// A start resent after B became the Writer, its Container not yet made, while a
/// paused replacement of the same Service sits on the Volume: a paused Service serves
/// nothing, so the start is no success, and Cloud keeps A's Container for the retry.
#[tokio::test]
async fn a_start_resent_beside_a_paused_replacement_is_no_success() {
    let fleet = Fleet::start().await;
    let source = fleet.routed(0, "a", "api", 2).await;
    cut(&fleet, 0).await;
    let mut replacement = fleet.node(0).container(source).unwrap();
    replacement["Id"] = json!(format!("{}{}", MachineId::random(), MachineId::random()));
    replacement["Name"] = json!("/api-paused");
    replacement["State"] = json!({"Status":"paused"});
    fleet
        .node(1)
        .fake
        .named_containers
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .insert("api-paused".into(), replacement);
    let destination = fleet.with_plugin(
        1,
        &plugin(
            writer_root(1, Some(snapshot())),
            WRITER_ROUTE,
            writer_root(1, Some(snapshot())),
        ),
    );

    try_set_copy(
        &destination,
        json!({"name":VOLUME, "expect":{"kind":"mirror", "newest":"7", "slot":"1"},
            "want":{"kind":"writer", "at":"7", "epoch":1, "start":{
                "kind":"create", "namespace":"app", "resolved_spec":on_volume("api"),
                "creation_key":"move-1", "gen":2,
                "rotation":{"in":{"run":null, "expect":"pending"}}}}}),
    )
    .await
    .expect_err("a paused replacement is not the Service running");

    assert!(
        fleet.node(0).container(source).is_some(),
        "the source is kept"
    );
}

/// A Move's cut names the Service it counted. Another Container running on the Volume,
/// one Cloud's frame never showed, refuses the cut before anything stops.
#[tokio::test]
async fn a_cut_naming_its_holder_stops_nothing_beside_another() {
    let fleet = Fleet::start().await;
    let source = fleet.routed(0, "a", "api", 2).await;
    {
        let mut named = fleet
            .node(0)
            .fake
            .named_containers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap();
        let mut hook = named.values().next().unwrap().clone();
        hook["Id"] = json!(format!("{}{}", MachineId::random(), MachineId::random()));
        hook["Name"] = json!("/api-hook");
        hook["State"] = json!({"Status":"running"});
        named.insert("api-hook".into(), hook);
    }
    let plugin = plugin(writer_root(0, None), CUT_ROUTE, slot(Some(snapshot())));
    let machine = fleet.with_plugin(0, &plugin);

    let refused = try_set_copy(
        &machine,
        json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":0},
            "want":{"kind":"mirror", "from":null, "target":"f-1", "drain":0, "holder":source}}),
    )
    .await
    .expect_err("a second running holder refuses the cut");

    assert!(refused.contains("api-hook"), "{refused}");
    assert!(refused.contains("\"held\""), "{refused}");
    assert!(fleet.node(0).running(source), "the cut stopped nothing");
}

fn cut_running(holder: ContainerId) -> Value {
    json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":0}, "gen":3,
        "want":{"kind":"mirror", "from":null, "target":"f-1", "drain":0, "holder":holder, "running":true}})
}

/// A drain's Move names the Service it saw running. Stopped since, by its operator,
/// it refuses the cut, so no other Server starts a Service someone stopped.
#[tokio::test]
async fn a_cut_whose_holder_must_run_refuses_a_stopped_one() {
    let fleet = Fleet::start().await;
    let source = fleet.routed(0, "a", "api", 2).await;
    fleet.node(0).set_state(source, json!({"Status":"exited"}));
    let plugin = plugin(writer_root(0, None), CUT_ROUTE, slot(Some(snapshot())));
    let machine = fleet.with_plugin(0, &plugin);

    let refused = try_set_copy(&machine, cut_running(source))
        .await
        .expect_err("a stopped holder refuses the cut");

    assert!(refused.contains("\"stopped\""), "{refused}");
    assert!(
        !plugin
            .routes_called()
            .iter()
            .any(|route| route == CUT_ROUTE),
        "the root stays a writer"
    );
}

/// A cut that stopped its holder and then failed is resent at the same gen: the stop
/// was its own, so the resend goes on to the snapshot rather than refusing the holder
/// it stopped, and a Container started beside it since reads busy, not held.
#[tokio::test]
async fn a_resent_cut_knows_the_stop_it_made() {
    let fleet = Fleet::start().await;
    let source = fleet.routed(0, "a", "api", 2).await;
    // No answer to the snapshot yet, so the first cut fails past its stop.
    let plugin = FakePlugin::default();
    plugin.reply(INSPECT_ROUTE, writer_root(0, None));
    let machine = fleet.with_plugin(0, &plugin);

    try_set_copy(&machine, cut_running(source))
        .await
        .expect_err("the snapshot fails after the stop");
    assert!(!fleet.node(0).running(source), "the first cut stopped it");

    add_stray(&fleet, 0, "running");
    let busy = try_set_copy(&machine, cut_running(source))
        .await
        .expect_err("a Container beside the resend refuses it");
    assert!(busy.contains("\"busy\""), "{busy}");
    fleet
        .node(0)
        .fake
        .named_containers
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .remove("stray");

    plugin.reply(CUT_ROUTE, slot(Some(snapshot())));
    plugin.reply(INSPECT_ROUTE, slot(Some(snapshot())));
    let cut = set_copy(&machine, cut_running(source)).await;
    assert!(
        matches!(cut.copy, Some(ployz_core::VolumeCopy::Slot { .. })),
        "{cut:?}"
    );
}

/// A cut that names its holder without `running` is one a caller sent before the
/// capability, or one that isn't a drain's: it keeps no record of its stop, so a
/// Container beside its resend reads held, as it always has.
#[tokio::test]
async fn a_cut_naming_only_its_holder_keeps_no_record_of_its_stop() {
    let fleet = Fleet::start().await;
    let source = fleet.routed(0, "a", "api", 2).await;
    let plugin = FakePlugin::default();
    plugin.reply(INSPECT_ROUTE, writer_root(0, None));
    let machine = fleet.with_plugin(0, &plugin);
    let cut = json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":0}, "gen":3,
        "want":{"kind":"mirror", "from":null, "target":"f-1", "drain":0, "holder":source}});

    try_set_copy(&machine, cut.clone())
        .await
        .expect_err("the snapshot fails after the stop");
    assert!(!fleet.node(0).running(source), "the first cut stopped it");

    add_stray(&fleet, 0, "running");
    let held = try_set_copy(&machine, cut)
        .await
        .expect_err("a Container beside the resend refuses it");
    assert!(held.contains("\"held\""), "{held}");
}

/// A preview of `node`'s Server, whose Service `api` mounts the preview's own Volume `data`
/// and pulls its image on every create.
fn preview_on(node: &Node) -> (Namespace, ResolvedServiceSpec, DockerVolumeName) {
    let home = ployz_core::PreviewHome::of(&node.row.id);
    let namespace = Namespace::parse(format!("pv-0123456789abcdef-{home}-00c0ffee")).unwrap();
    let birth = ployz_core::PreviewNamespace::of(&namespace)
        .unwrap()
        .birth("data");
    let mut source = ployz_core::RawVolumeSource::Provisioned {
        name: DockerVolumeName::parse("data").unwrap(),
        maximum_bytes: ployz_core::ProvisionedVolumeMaximumBytes::new(
            std::num::NonZeroU64::new(1_073_741_824).unwrap(),
        ),
        labels: BTreeMap::new(),
        birth: Some(birth),
    }
    .admit()
    .unwrap();
    source.scope_to_namespace(&namespace);
    let mut preview = spec_with_sources(vec![source]);
    preview.name = spec("api").name;
    preview.pre_deploy = None;
    preview.update.monitor_millis = Some(0);
    preview.container.pull_policy = ployz_core::PullPolicy::Always;
    preview.container.image = "example.test/api:1".to_owned();
    let volume = namespace.volume_name(&DockerVolumeName::parse("data").unwrap());
    (namespace, preview, volume)
}

/// The preview Volume's root at `epoch`, carrying the preview's origin.
fn preview_root(spec: &ResolvedServiceSpec, volume: &DockerVolumeName, epoch: u32) -> Value {
    let birth = spec.volume_graph().volumes()[0].source.birth().cloned();
    let origin = ployz_core::VolumeOrigin::expected(birth.as_ref(), volume).unwrap();
    json!({"Ok": {"kind":"root", "readonly":false, "newest":snapshot(), "epoch":epoch,
        "seed":false, "origin":origin}})
}

fn preview_writer(
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
    volume: &DockerVolumeName,
) -> Value {
    preview_writer_at(namespace, spec, volume, 1)
}

/// A Writer start of the preview's Service on its Volume, promoting the root to `epoch`.
fn preview_writer_at(
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
    volume: &DockerVolumeName,
    epoch: u32,
) -> Value {
    json!({"name":volume, "expect":{"kind":"writer", "epoch":epoch - 1},
        "want":{"kind":"writer", "at":"7", "epoch":epoch, "start":{
            "kind":"create", "namespace":namespace, "resolved_spec":spec,
            "creation_key":"move-1", "gen":2,
            "rotation":{"in":{"run":null, "expect":"pending"}}}}})
}

fn holds_a_container(node: &Node) -> bool {
    !node
        .fake
        .named_containers
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .is_empty()
}

fn promotes(plugin: &FakePlugin) -> usize {
    plugin
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(route, _)| route == WRITER_ROUTE)
        .count()
}

/// A preview's Writer start pulls its image once, before the promote: a registry that
/// refuses every pull after that one never leaves a promoted copy with no holder.
#[tokio::test]
async fn a_preview_start_pulls_once_so_no_refused_pull_follows_its_promote() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let (namespace, spec, volume) = preview_on(node);
    *node.fake.pulls_left.lock().unwrap() = Some(1);
    let plugin = plugin(
        preview_root(&spec, &volume, 0),
        WRITER_ROUTE,
        preview_root(&spec, &volume, 1),
    );
    let service = fleet.with_plugin(0, &plugin);

    let reply = try_set_copy(&service, preview_writer(&namespace, &spec, &volume)).await;

    assert!(
        !reply
            .as_ref()
            .is_err_and(|error| error.contains("toomanyrequests")),
        "the start pulled again: {reply:?}"
    );
    assert!(
        promotes(&plugin) == 0 || holds_a_container(node),
        "the copy was promoted with no holder made: {reply:?}"
    );
}

/// A preview start never runs a Container its slot kept from an image its tag no longer
/// names, and refuses it before the promote: its checks read the image the tag names now.
#[tokio::test]
async fn a_preview_start_refuses_a_kept_container_of_the_image_its_tag_left() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let (namespace, spec, volume) = preview_on(node);
    let (first, moved) = (
        format!("sha256:{}", "a".repeat(64)),
        format!("sha256:{}", "b".repeat(64)),
    );
    let tag = spec.container.image.clone();
    node.fake
        .registry
        .lock()
        .unwrap()
        .insert(tag.clone(), first.clone());
    let plugin = plugin(
        preview_root(&spec, &volume, 0),
        WRITER_ROUTE,
        preview_root(&spec, &volume, 1),
    );
    let service = fleet.with_plugin(0, &plugin);
    // The first start makes its Container from the first image, which exits at once.
    node.fake.exit_on_start.store(true, Ordering::SeqCst);
    let _ = try_set_copy(&service, preview_writer(&namespace, &spec, &volume)).await;
    node.fake.exit_on_start.store(false, Ordering::SeqCst);
    let kept = only_container(node);
    assert_eq!(node.container(kept).unwrap()["Image"], first.as_str());
    assert_eq!(promotes(&plugin), 1);

    node.fake.registry.lock().unwrap().insert(tag, moved);
    let reply = try_set_copy(&service, preview_writer_at(&namespace, &spec, &volume, 2)).await;

    assert!(
        !node.running(kept),
        "the start ran the Container of the image its tag left: {reply:?}"
    );
    assert!(
        reply
            .as_ref()
            .is_err_and(|error| error.contains("runs another image")),
        "{reply:?}"
    );
    assert_eq!(promotes(&plugin), 1, "a refused start promoted the copy");
}

async fn close(
    service: &MachineService,
    namespace: &Namespace,
) -> Result<ployz_core::PreviewClosed, String> {
    let request = ployz_core::ClosePreviewRequest {
        namespace: namespace.clone(),
    };
    let response = service
        .close_preview(Request::new(
            op::ClosePreview::into_request(request).encode().unwrap(),
        ))
        .await
        .map_err(|status| status.message().to_owned())?
        .into_inner()
        .decode_response()
        .unwrap();
    response
        .clone()
        .decode::<op::ClosePreview>()
        .map_err(|error| format!("{error}: {response:?}"))
}

fn calls_to(plugin: &FakePlugin, route: &str) -> usize {
    plugin
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(called, _)| called == route)
        .count()
}

/// A preview whose Service `api` this Server created, which opens the preview, and a Docker
/// Volume of another driver in its Namespace.
async fn created_preview(
    fleet: &Fleet,
    plugin: &FakePlugin,
) -> (
    MachineService,
    Namespace,
    ResolvedServiceSpec,
    DockerVolumeName,
) {
    let node = fleet.node(0);
    let (namespace, spec, volume) = preview_on(node);
    let service = fleet.with_plugin(0, plugin);
    create_in(&service, &namespace, &spec).await.unwrap();
    assert!(holds_a_container(node));
    node.fake.listed_volumes.lock().unwrap().push(json!({
        "Name": format!("{namespace}_cache"), "Driver": "local", "Mountpoint": "/cache"}));
    (service, namespace, spec, volume)
}

async fn create_in(
    service: &MachineService,
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
) -> Result<(), String> {
    service
        .local
        .create_container(
            ployz_core::ContainerKind::ServiceContainer,
            namespace,
            spec,
            None,
            None,
            None,
        )
        .await
        .map(drop)
        .map_err(|error| ployz_core::error_chain::inline(&error))
}

fn preview_plugin(spec: &ResolvedServiceSpec, volume: &DockerVolumeName) -> FakePlugin {
    plugin(
        preview_root(spec, volume, 0),
        WRITER_ROUTE,
        preview_root(spec, volume, 1),
    )
}

/// A Close removes the preview's Containers, has the plugin sweep its Volumes and reports
/// a Volume of another driver kept. From then on no start creates a Container and no copy
/// or Seed reaches its data, and a repeat Close sweeps again.
#[tokio::test]
async fn a_closed_preview_runs_and_receives_nothing_more() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let (_, spec, volume) = preview_on(node);
    let plugin = preview_plugin(&spec, &volume);
    plugin.reply(PREVIEW_CLOSE_ROUTE, json!({"Ok": {"destroyed": [volume]}}));
    let (service, namespace, spec, volume) = created_preview(&fleet, &plugin).await;
    let promoted = promotes(&plugin);

    let closed = close(&service, &namespace).await.unwrap();

    assert_eq!(closed.destroyed, std::slice::from_ref(&volume));
    assert_eq!(
        closed.kept,
        [DockerVolumeName::parse(format!("{namespace}_cache")).unwrap()]
    );
    assert!(!holds_a_container(node), "the Close left its Container");
    let start = try_set_copy(&service, preview_writer_at(&namespace, &spec, &volume, 2)).await;
    assert!(
        start
            .as_ref()
            .is_err_and(|error| error.contains("is closed")),
        "{start:?}"
    );
    assert!(!holds_a_container(node), "a start after the Close made one");
    let create = create_in(&service, &namespace, &spec).await;
    assert!(
        create
            .as_ref()
            .is_err_and(|error| error.contains("is closed")),
        "{create:?}"
    );
    assert!(
        !holds_a_container(node),
        "a create after the Close made one"
    );
    assert_eq!(
        promotes(&plugin),
        promoted,
        "a start after the Close promoted"
    );
    let pull = json!({"name":volume, "expect":{"kind":"writer", "epoch":0},
        "want":{"kind":"mirror", "from":fleet.node(1).row.management_address(), "target":"f-2", "drain":0}});
    let pull = try_set_copy(&service, pull).await;
    assert!(
        pull.as_ref()
            .is_err_and(|error| error.contains("is closed")),
        "{pull:?}"
    );
    let seed = ployz_core::SeedVolumeRequest {
        name: DockerVolumeName::parse("app_data").unwrap(),
        into: volume.clone(),
        guid: ployz_core::SnapshotGuid::new(7),
        r#gen: None,
    };
    let seed = service
        .seed_volume(Request::new(
            op::SeedVolume::into_request(seed).encode().unwrap(),
        ))
        .await;
    assert!(
        seed.as_ref()
            .is_err_and(|status| status.message().contains("is closed")),
        "{seed:?}"
    );
    assert_eq!(calls_to(&plugin, VOLUME_SEED_ROUTE), 0);

    close(&service, &namespace).await.unwrap();
    assert_eq!(
        calls_to(&plugin, PREVIEW_CLOSE_ROUTE),
        2,
        "a repeat sweeps again"
    );
}

/// A Close of a Namespace this Server never ran as a preview changes nothing: not an
/// ordinary Namespace, not another Server's preview, not its own preview it never opened.
#[tokio::test]
async fn a_close_of_a_preview_this_server_never_opened_touches_nothing() {
    let fleet = Fleet::start().await;
    let (unopened, _, _) = preview_on(fleet.node(0));
    let (elsewhere, _, _) = preview_on(fleet.node(1));
    let plugin = FakePlugin::default();
    plugin.reply(PREVIEW_CLOSE_ROUTE, json!({"Ok": {}}));
    let service = fleet.with_plugin(0, &plugin);
    for namespace in [unopened, elsewhere, Namespace::parse("app").unwrap()] {
        let refused = close(&service, &namespace).await;
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.contains("never ran")),
            "{namespace}: {refused:?}"
        );
    }
    assert_eq!(calls_to(&plugin, PREVIEW_CLOSE_ROUTE), 0);
}

/// A Close whose sweep fails leaves the preview Closing: its Container is gone and no start
/// makes another. A daemon start runs the sweep again and marks it Closed.
#[tokio::test]
async fn a_close_stopped_mid_sweep_resumes_at_the_next_daemon_start() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let (_, spec, volume) = preview_on(node);
    let plugin = preview_plugin(&spec, &volume);
    plugin.reply(
        PREVIEW_CLOSE_ROUTE,
        json!({"Err": {"code": "internal", "message": "zfs destroy failed", "details": null}}),
    );
    let (service, namespace, spec, volume) = created_preview(&fleet, &plugin).await;

    let stopped = close(&service, &namespace).await;
    assert!(stopped.is_err(), "{stopped:?}");
    assert!(!holds_a_container(node));
    let start = try_set_copy(&service, preview_writer_at(&namespace, &spec, &volume, 2)).await;
    assert!(
        start
            .as_ref()
            .is_err_and(|error| error.contains("is closed")),
        "{start:?}"
    );

    plugin.reply(PREVIEW_CLOSE_ROUTE, json!({"Ok": {"destroyed": [volume]}}));
    service.local.resume_preview_closes().await;
    assert_eq!(
        calls_to(&plugin, PREVIEW_CLOSE_ROUTE),
        2,
        "the start resumed the sweep"
    );
    service.local.resume_preview_closes().await;
    assert_eq!(
        calls_to(&plugin, PREVIEW_CLOSE_ROUTE),
        2,
        "a finished Close is not resumed again"
    );
}

/// A Close that can't remove the preview's Container leaves it Closing, and the Container
/// it left starts no more.
#[tokio::test]
async fn a_container_a_close_left_behind_starts_no_more() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let (_, spec, volume) = preview_on(node);
    let plugin = preview_plugin(&spec, &volume);
    plugin.reply(PREVIEW_CLOSE_ROUTE, json!({"Ok": {}}));
    let (service, namespace, spec, _) = created_preview(&fleet, &plugin).await;
    node.fake
        .refuse_remove_of
        .lock()
        .unwrap()
        .insert(spec.name.to_string());

    let stopped = close(&service, &namespace).await;
    assert!(stopped.is_err(), "{stopped:?}");
    assert_eq!(calls_to(&plugin, PREVIEW_CLOSE_ROUTE), 0);
    let start = service
        .local
        .start_container(only_container(node), None)
        .await
        .map_err(|error| ployz_core::error_chain::inline(&error));
    assert!(
        start
            .as_ref()
            .is_err_and(|error| error.contains("is closed")),
        "{start:?}"
    );
}
