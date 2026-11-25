use super::*;
use crate::storage::test_support::FakePlugin;
use crate::volume_copy::{CUT_ROUTE, DESTROY_ROUTE, INSPECT_ROUTE, WRITER_ROUTE};
use ployz_core::{Gen, SetVolumeCopyReply};

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
        .unwrap()
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

/// A `ployz volume sync --full` forgets the mirror before it copies the Volume again, with
/// the same bare Forget an older Cloud's `mirror rm` sends. The slot here is the copy a
/// departure parked, with writes no other Machine has, so a bare Forget keeps it, and only
/// `mirror rm`, which says to discard it, deletes it.
#[tokio::test]
async fn a_bare_forget_keeps_a_parked_copy_that_mirror_rm_deletes() {
    let fleet = Fleet::start().await;
    let parked = json!({"Ok": {"kind":"slot", "slot":"1", "readonly":true,
        "newest":snapshot(), "resume_token":null, "pull":null, "role":"old_copy",
        "written_here":true}});
    let plugin = plugin(parked, DESTROY_ROUTE, json!({"Ok": null}));
    let machine = fleet.with_plugin(0, &plugin);
    let forget = |want: Value| json!({"name":VOLUME, "expect":{"kind":"mirror", "newest":"7", "slot":"1"}, "want":want});

    let refused = try_set_copy(&machine, forget(json!({"kind":"forget"})))
        .await
        .unwrap_err();
    assert!(refused.contains("written_here"), "{refused}");
    assert!(
        !plugin
            .routes_called()
            .iter()
            .any(|route| route == DESTROY_ROUTE),
        "{:?}",
        plugin.routes_called()
    );

    set_copy(&machine, forget(json!({"kind":"forget", "discard":true}))).await;
    assert!(
        plugin
            .routes_called()
            .iter()
            .any(|route| route == DESTROY_ROUTE),
        "{:?}",
        plugin.routes_called()
    );
}

/// Someone confirmed deleting this slot while it was written nowhere else. Written here
/// since, it is not the copy they confirmed, so their Forget deletes nothing.
#[tokio::test]
async fn a_confirmed_forget_keeps_a_copy_written_here_since() {
    let fleet = Fleet::start().await;
    let parked = json!({"Ok": {"kind":"slot", "slot":"1", "readonly":true,
        "newest":snapshot(), "resume_token":null, "pull":null, "role":"old_copy",
        "written_here":true}});
    let plugin = plugin(parked, DESTROY_ROUTE, json!({"Ok": null}));
    let machine = fleet.with_plugin(0, &plugin);
    let confirmed = |written: bool| {
        json!({"name":VOLUME,
            "expect":{"kind":"mirror", "newest":"7", "slot":"1", "written_here":written},
            "want":{"kind":"forget", "discard":true}})
    };
    let destroyed = || {
        plugin
            .routes_called()
            .iter()
            .any(|route| route == DESTROY_ROUTE)
    };

    let refused = try_set_copy(&machine, confirmed(false)).await.unwrap_err();
    assert!(refused.contains("mismatch"), "{refused}");
    assert!(!destroyed(), "{:?}", plugin.routes_called());

    set_copy(&machine, confirmed(true)).await;
    assert!(destroyed(), "{:?}", plugin.routes_called());
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
