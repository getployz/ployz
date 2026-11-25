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

fn promotes(plugin: &FakePlugin) -> usize {
    plugin
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(route, _)| route == WRITER_ROUTE)
        .count()
}

/// A Move's Writer start pulls its image before the promote, and only then: a registry
/// that refuses the pull leaves the Mirror as it was, and one that serves a single pull
/// leaves the promoted copy with its holder running.
#[tokio::test]
async fn a_move_start_pulls_before_its_promote_so_no_refused_pull_follows_it() {
    let fleet = Fleet::start().await;
    let node = fleet.node(1);
    let mut moved = spec("api");
    moved.container.pull_policy = ployz_core::PullPolicy::Always;
    let plugin = plugin(
        slot(Some(snapshot())),
        WRITER_ROUTE,
        writer_root(1, Some(snapshot())),
    );
    let destination = fleet.with_plugin(1, &plugin);
    let create = json!({"name":VOLUME, "expect":{"kind":"mirror", "newest":"7", "slot":"1"},
        "want":{"kind":"writer", "at":"7", "epoch":1, "start":{
            "kind":"create", "namespace":"app", "resolved_spec":moved,
            "creation_key":"move-1", "gen":2,
            "rotation":{"in":{"run":null, "expect":"pending"}}}}});

    *node.fake.pulls_left.lock().unwrap() = Some(0);
    let refused = try_set_copy(&destination, create.clone()).await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.contains("toomanyrequests")),
        "{refused:?}"
    );
    assert_eq!(
        promotes(&plugin),
        0,
        "the copy was promoted before a pull the registry refused: {refused:?}"
    );

    *node.fake.pulls_left.lock().unwrap() = Some(1);
    let started = try_set_copy(&destination, create).await;
    assert!(
        started.is_ok(),
        "the start pulled again after the promote: {started:?}"
    );
    assert_eq!(promotes(&plugin), 1);
    assert!(node.running(only_container(node)));
}

/// A start that sees its Service's replacement running before the promote, and finds it
/// exited after, creates a holder all the same: it pulled before the promote, so no
/// refused pull follows it.
#[tokio::test]
async fn a_replacement_that_exits_during_the_promote_leaves_no_pull_after_it() {
    let fleet = Fleet::start().await;
    let source = fleet.routed(0, "a", "api", 2).await;
    cut(&fleet, 0).await;
    let node = fleet.node(1);
    let mut replacement = fleet.node(0).container(source).unwrap();
    replacement["Id"] = json!(format!("{}{}", MachineId::random(), MachineId::random()));
    replacement["Name"] = json!("/api-replacement");
    replacement["State"] = json!({"Status":"running"});
    let named = node.fake.named_containers.as_ref().unwrap().clone();
    named
        .lock()
        .unwrap()
        .insert("api-replacement".into(), replacement);
    let plugin = plugin(
        json!({"Ok": {"kind":"root", "readonly":true, "newest":snapshot(), "epoch":0,
            "seed":false}}),
        WRITER_ROUTE,
        writer_root(1, Some(snapshot())),
    );
    // The replacement exits while the plugin promotes.
    let exits = named.clone();
    plugin.on(WRITER_ROUTE, move || {
        exits.lock().unwrap().get_mut("api-replacement").unwrap()["State"] =
            json!({"Status":"exited"});
    });
    let destination = fleet.with_plugin(1, &plugin);
    let mut moved = on_volume("api");
    moved.container.pull_policy = ployz_core::PullPolicy::Always;
    *node.fake.pulls_left.lock().unwrap() = Some(0);

    let reply = try_set_copy(
        &destination,
        json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":0},
            "want":{"kind":"writer", "at":"7", "epoch":1, "start":{
                "kind":"create", "namespace":"app", "resolved_spec":moved,
                "creation_key":"move-1", "gen":2,
                "rotation":{"in":{"run":null, "expect":"pending"}}}}}),
    )
    .await;

    assert!(
        promotes(&plugin) == 0 || named.lock().unwrap().len() > 2,
        "the copy was promoted, then a refused pull left it with no holder: {reply:?}"
    );
}

/// A signed start creates its holder from the image it pulled before the promote, by ID,
/// though the local tag moved to another image while the promote ran.
#[tokio::test]
async fn a_signed_start_runs_the_image_it_pulled_though_its_tag_moved() {
    let fleet = Fleet::start().await;
    let node = fleet.node(1);
    let mut moved = spec("api");
    moved.container.pull_policy = ployz_core::PullPolicy::Always;
    moved.container.image = "example.test/api:1".to_owned();
    let (pulled, moved_to) = (
        format!("sha256:{}", "a".repeat(64)),
        format!("sha256:{}", "b".repeat(64)),
    );
    let tag = moved.container.image.clone();
    node.fake
        .registry
        .lock()
        .unwrap()
        .insert(tag.clone(), pulled.clone());
    let plugin = plugin(
        slot(Some(snapshot())),
        WRITER_ROUTE,
        writer_root(1, Some(snapshot())),
    );
    let images = node.fake.images.clone();
    plugin.on(WRITER_ROUTE, move || {
        images
            .lock()
            .unwrap()
            .insert(tag.clone(), Some(moved_to.clone()));
    });
    let destination = fleet.with_plugin(1, &plugin);

    set_copy(
        &destination,
        json!({"name":VOLUME, "expect":{"kind":"mirror", "newest":"7", "slot":"1"},
            "want":{"kind":"writer", "at":"7", "epoch":1, "start":{
                "kind":"create", "namespace":"app", "resolved_spec":moved,
                "creation_key":"move-1", "gen":2,
                "rotation":{"in":{"run":null, "expect":"pending"}}}}}),
    )
    .await;

    let holder = only_container(node);
    assert_eq!(node.container(holder).unwrap()["Image"], pulled.as_str());
}

/// A start stopped between its promote and its Create, then resent, creates its holder
/// from the image the promote recorded: the registry refuses every pull and the tag has
/// moved, so a start that placed its image again would refuse or run another image. With
/// that image gone, the resend refuses rather than pulls.
#[tokio::test]
async fn a_start_resent_after_its_promote_runs_the_image_the_promote_recorded() {
    let fleet = Fleet::start().await;
    let node = fleet.node(1);
    let mut moved = spec("api");
    moved.container.pull_policy = ployz_core::PullPolicy::Always;
    moved.container.image = "example.test/api:1".to_owned();
    let (pulled, moved_to) = (
        format!("sha256:{}", "a".repeat(64)),
        format!("sha256:{}", "b".repeat(64)),
    );
    let tag = moved.container.image.clone();
    node.fake
        .registry
        .lock()
        .unwrap()
        .insert(tag.clone(), pulled.clone());
    let plugin = plugin(
        slot(Some(snapshot())),
        WRITER_ROUTE,
        writer_root(1, Some(snapshot())),
    );
    let destination = fleet.with_plugin(1, &plugin);
    let start = json!({"name":VOLUME, "expect":{"kind":"mirror", "newest":"7", "slot":"1"},
        "want":{"kind":"writer", "at":"7", "epoch":1, "start":{
            "kind":"create", "namespace":"app", "resolved_spec":moved,
            "creation_key":"move-1", "gen":2,
            "rotation":{"in":{"run":null, "expect":"pending"}}}}});

    // The daemon stops after the promote, before Docker creates the holder.
    node.fake
        .refuse_create_of
        .lock()
        .unwrap()
        .insert("api".into());
    try_set_copy(&destination, start.clone())
        .await
        .expect_err("the Create fails after the promote");
    assert_eq!(promotes(&plugin), 1);
    node.fake.refuse_create_of.lock().unwrap().clear();
    *node.fake.pulls_left.lock().unwrap() = Some(0);
    node.fake
        .registry
        .lock()
        .unwrap()
        .insert(tag.clone(), moved_to.clone());
    node.fake.images.lock().unwrap().insert(tag, Some(moved_to));

    node.fake
        .images
        .lock()
        .unwrap()
        .insert(pulled.clone(), None);
    let gone = try_set_copy(&destination, start.clone())
        .await
        .expect_err("the recorded image is gone");
    assert!(gone.contains("no longer on this Server"), "{gone}");

    node.fake.images.lock().unwrap().remove(&pulled);
    let resent = try_set_copy(&destination, start).await;
    assert!(
        resent.is_ok(),
        "the resend placed its image again: {resent:?}"
    );
    assert_eq!(promotes(&plugin), 1);
    let holder = only_container(node);
    assert_eq!(node.container(holder).unwrap()["Image"], pulled.as_str());
}

/// A Move that made its holder under `never` and failed to start it leaves that holder
/// stopped under the start's key. Cloud's forward Release resends the key under
/// `missing`, so the spec differs only in how its image is placed: the Release adopts
/// the holder the Move made, starts it, and routes it.
#[tokio::test]
async fn a_release_starts_the_holder_its_move_made_under_another_pull_policy() {
    let fleet = Fleet::start().await;
    let node = fleet.node(1);
    let plugin = plugin(
        slot(Some(snapshot())),
        WRITER_ROUTE,
        writer_root(1, Some(snapshot())),
    );
    let destination = fleet.with_plugin(1, &plugin);
    let service = spec("api");
    let start = |policy: &str| {
        let mut moved = service.clone();
        moved.container.pull_policy = serde_json::from_value(json!(policy)).unwrap();
        json!({"name":VOLUME, "expect":{"kind":"mirror", "newest":"7", "slot":"1"},
            "want":{"kind":"writer", "at":"7", "epoch":1, "start":{
                "kind":"create", "namespace":"app", "resolved_spec":moved,
                "creation_key":"move-1", "gen":2,
                "rotation":{"in":{"run":null, "expect":"pending"}}}}})
    };

    node.fake
        .exit_on_start
        .store(true, std::sync::atomic::Ordering::SeqCst);
    try_set_copy(&destination, start("never"))
        .await
        .expect_err("the Move's holder exited");
    let made = only_container(node);
    assert!(!node.running(made));

    node.fake
        .exit_on_start
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let released = try_set_copy(&destination, start("missing")).await;
    assert!(released.is_ok(), "{released:?}");
    assert_eq!(only_container(node), made);
    assert!(node.running(made));
    assert_eq!(node.rotation(made).await, CopyRotation::In);
}

/// The record binds an image to the admitted spec, not to the start's creation key. A
/// start whose spec changed under the same key and epoch after the promote, such as a
/// redeploy to another tag, places its own image rather than run the one the promote
/// recorded for the spec before it.
#[tokio::test]
async fn a_start_resent_with_another_spec_places_its_own_image() {
    let fleet = Fleet::start().await;
    let node = fleet.node(1);
    let mut service = spec("api");
    service.container.pull_policy = ployz_core::PullPolicy::Always;
    let (first, second) = (
        format!("sha256:{}", "a".repeat(64)),
        format!("sha256:{}", "b".repeat(64)),
    );
    for (tag, id) in [
        ("example.test/api:1", &first),
        ("example.test/api:2", &second),
    ] {
        node.fake
            .registry
            .lock()
            .unwrap()
            .insert(tag.to_owned(), id.clone());
    }
    let plugin = plugin(
        slot(Some(snapshot())),
        WRITER_ROUTE,
        writer_root(1, Some(snapshot())),
    );
    let destination = fleet.with_plugin(1, &plugin);
    let start = |image: &str| {
        let mut moved = service.clone();
        moved.container.image = image.to_owned();
        json!({"name":VOLUME, "expect":{"kind":"mirror", "newest":"7", "slot":"1"},
            "want":{"kind":"writer", "at":"7", "epoch":1, "start":{
                "kind":"create", "namespace":"app", "resolved_spec":moved,
                "creation_key":"move-1", "gen":2,
                "rotation":{"in":{"run":null, "expect":"pending"}}}}})
    };

    node.fake
        .refuse_create_of
        .lock()
        .unwrap()
        .insert("api".into());
    try_set_copy(&destination, start("example.test/api:1"))
        .await
        .expect_err("the Create fails after the promote");
    node.fake.refuse_create_of.lock().unwrap().clear();

    let resent = try_set_copy(&destination, start("example.test/api:2")).await;
    assert!(resent.is_ok(), "{resent:?}");
    assert_eq!(promotes(&plugin), 1);
    let holder = only_container(node);
    assert_eq!(node.container(holder).unwrap()["Image"], second.as_str());
}

/// A volume plugin that promotes without recording the start's image runs an older
/// build than its daemon. The start is refused, naming the plugin to restart, rather
/// than run with no record a resend after a stop could take.
#[tokio::test]
async fn a_start_refuses_a_promote_that_recorded_no_image() {
    let fleet = Fleet::start().await;
    let node = fleet.node(1);
    let plugin = plugin(
        slot(Some(snapshot())),
        WRITER_ROUTE,
        writer_root(1, Some(snapshot())),
    );
    plugin.records_no_image();
    let destination = fleet.with_plugin(1, &plugin);
    let start = json!({"name":VOLUME, "expect":{"kind":"mirror", "newest":"7", "slot":"1"},
        "want":{"kind":"writer", "at":"7", "epoch":1, "start":{
            "kind":"create", "namespace":"app", "resolved_spec":spec("api"),
            "creation_key":"move-1", "gen":2,
            "rotation":{"in":{"run":null, "expect":"pending"}}}}});

    let refused = try_set_copy(&destination, start)
        .await
        .expect_err("the promote recorded no image");
    assert!(refused.contains("Restart ployz-volume-plugin"), "{refused}");
    assert!(
        node.fake
            .named_containers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .is_empty()
    );
}
