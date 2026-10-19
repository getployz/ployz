//! A Volume Move across the fleet: the cut on the source Machine and the Writer
//! start on the destination, with the source copy routed before the Move.

use super::*;
use crate::storage::test_support::FakePlugin;
use crate::volume_copy::{CUT_ROUTE, INSPECT_ROUTE, WRITER_ROUTE};
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

/// A plugin whose copy reads `before` until `route` runs, and `after` from then on.
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
    /// `index`'s Machine API with its own fake plugin.
    fn with_plugin(&self, index: usize, plugin: &FakePlugin) -> MachineService {
        let directory = self.dir.join(format!("plugin-{}", MachineId::random()));
        std::fs::create_dir_all(&directory).unwrap();
        let (socket, _server) = plugin.serve(&directory);
        self.node(index).service.clone().with_volume_plugin(socket)
    }
}

async fn set_copy(service: &MachineService, request: Value) -> SetVolumeCopyReply {
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
        .unwrap_or_else(|error| panic!("{error}: {response:?}"))
}

/// The cut: the source stops its holder and parks the root in a slot.
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

/// A Release before the commit starts the cut's holder again. The cut left its
/// rotation alone, so it routes again; an Out written by the cut would be final.
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
