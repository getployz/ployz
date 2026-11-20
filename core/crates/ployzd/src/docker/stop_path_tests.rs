//! The order of Docker calls on the paths that stop, retire, create and repair the Ingress
//! doors, recorded against the fake Docker. The Ingress Proxy is the only Container the
//! daemon starts unasked, so these sequences are what a restructure must not change.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use ployz_core::{ContainerId, ContainerKind, Namespace};

use super::{
    ContainerRuntime, Error,
    lifecycle::ContainerRequest,
    recovery::Repair,
    test_support::{FakeDocker, container_request, fake_runtime_with, machine},
};
use crate::images::DoorSpecs;

type Containers = Arc<Mutex<BTreeMap<String, serde_json::Value>>>;

const PORT_TAKEN: &str = "failed to bind host port 0.0.0.0:80/tcp: address already in use";

struct Doors {
    runtime: ContainerRuntime,
    fake: FakeDocker,
    containers: Containers,
    holder: ContainerId,
    caddy: ContainerId,
}

impl Doors {
    async fn new() -> Self {
        let containers = Containers::default();
        let (runtime, fake) = fake_runtime_with(FakeDocker {
            named_containers: Some(containers.clone()),
            list_named: true,
            ..Default::default()
        })
        .await;
        let specs = DoorSpecs::public(ployz_core::ServiceId::random());
        let holder = create(&runtime, &specs.holder, "holder").await;
        let mut doors = Self {
            runtime,
            fake,
            containers,
            holder,
            caddy: holder,
        };
        doors.set_state(
            &holder,
            serde_json::json!({"Status": "running", "Running": true}),
        );
        doors.caddy = create(&doors.runtime, &specs.proxy, "caddy").await;
        doors.set_state(
            &doors.caddy,
            serde_json::json!({"Status": "running", "Running": true}),
        );
        doors
    }

    fn restart_policy(&self, id: &ContainerId) -> String {
        let containers = self.containers.lock().unwrap();
        let container = containers
            .values()
            .find(|container| container["Id"] == id.as_str())
            .unwrap();
        container
            .pointer("/HostConfig/RestartPolicy/Name")
            .and_then(serde_json::Value::as_str)
            .unwrap()
            .to_owned()
    }

    fn set_restart_policy(&self, id: &ContainerId, name: &str) {
        let mut containers = self.containers.lock().unwrap();
        let container = containers
            .values_mut()
            .find(|container| container["Id"] == id.as_str())
            .unwrap();
        *container.pointer_mut("/HostConfig/RestartPolicy").unwrap() =
            serde_json::json!({"Name": name});
    }

    fn set_state(&self, id: &ContainerId, state: serde_json::Value) {
        let mut containers = self.containers.lock().unwrap();
        let container = containers
            .values_mut()
            .find(|container| container["Id"] == id.as_str())
            .unwrap();
        container["State"] = state;
    }

    fn fail_start(&self, id: &ContainerId) {
        self.set_state(
            id,
            serde_json::json!({"Status": "created", "ExitCode": 128, "Error": PORT_TAKEN}),
        );
    }

    /// Every Docker request since the last call, naming each Container.
    fn calls(&self) -> Vec<String> {
        let names = [(self.holder, "holder"), (self.caddy, "caddy")];
        self.fake
            .requests
            .lock()
            .unwrap()
            .drain(..)
            .map(|(method, path)| {
                let mut path = path;
                for (id, name) in names {
                    path = path.replace(id.as_str(), name);
                }
                if path.starts_with("/images/") {
                    path = "/images/<pinned image>/json".to_owned();
                }
                path = path
                    .split('/')
                    .map(|part| {
                        if part.starts_with("ingress-") {
                            "<new caddy>"
                        } else {
                            part
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("/");
                format!("{method} {path}")
            })
            .collect()
    }
}

async fn create(
    runtime: &ContainerRuntime,
    spec: &ployz_core::ResolvedServiceSpec,
    key: &str,
) -> ContainerId {
    let namespace = Namespace::system();
    runtime
        .lifecycle()
        .create_with_admission::<_, _, Error>(
            &machine(),
            ContainerRequest {
                creation_key: key,
                ..container_request(
                    ContainerKind::ServiceContainer,
                    &namespace,
                    spec,
                    std::future::ready(None),
                )
            },
        )
        .await
        .unwrap()
        .container_id
}

fn lines(calls: &[&str]) -> Vec<String> {
    calls.iter().map(|call| (*call).to_owned()).collect()
}

#[tokio::test]
async fn a_stop_inspects_the_container_then_stops_it() {
    let doors = Doors::new().await;
    doors.calls();

    doors
        .runtime
        .lifecycle()
        .stop(&doors.caddy, None, None)
        .await
        .unwrap();

    assert_eq!(
        doors.calls(),
        lines(&[
            "GET /containers/caddy/json",
            "POST /containers/caddy/update",
            "POST /containers/caddy/stop",
        ]),
    );
}

// A stopped Container that kept `always` would run again when dockerd restarts,
// beside the Container that replaced it on the same Volume.
#[tokio::test]
async fn a_stopped_container_stays_stopped_through_a_dockerd_restart_until_it_starts() {
    let doors = Doors::new().await;
    let declared = doors.restart_policy(&doors.caddy);
    assert_ne!(
        declared, "no",
        "the door declares a policy that restarts it"
    );

    doors
        .runtime
        .lifecycle()
        .stop(&doors.caddy, None, None)
        .await
        .unwrap();
    assert_eq!(doors.restart_policy(&doors.caddy), "no");

    doors.runtime.lifecycle().start(&doors.caddy).await.unwrap();
    assert_eq!(doors.restart_policy(&doors.caddy), declared);
}

#[tokio::test]
async fn a_stop_of_a_container_docker_lost_asks_docker_once_and_never_stops() {
    let doors = Doors::new().await;
    doors.containers.lock().unwrap().clear();
    doors.calls();

    let stopped = doors
        .runtime
        .lifecycle()
        .stop(&doors.caddy, None, None)
        .await;

    assert!(matches!(stopped, Err(Error::ContainerNotFound(id)) if id == doors.caddy));
    assert_eq!(doors.calls(), lines(&["GET /containers/caddy/json"]));
}

#[tokio::test]
async fn a_stop_that_a_refusal_wins_sends_no_stop_and_holds_nothing() {
    let doors = Doors::new().await;
    doors.fail_start(&doors.caddy);
    doors.calls();

    let stopped = doors
        .runtime
        .lifecycle()
        .stop_if(&doors.caddy, None, None, async { Ok::<_, Error>(false) })
        .await;

    assert!(matches!(stopped, Ok(false)));
    assert_eq!(doors.calls(), lines(&["GET /containers/caddy/json"]));
    assert!(
        !doors
            .runtime
            .lifecycle()
            .recovery
            .is_held(&doors.caddy)
            .await
    );
}

#[tokio::test]
async fn a_requested_stop_holds_the_door_and_a_requested_start_lifts_the_hold() {
    let doors = Doors::new().await;
    doors.fail_start(&doors.caddy);
    doors.calls();

    doors
        .runtime
        .lifecycle()
        .stop(&doors.caddy, None, None)
        .await
        .unwrap();
    assert!(
        doors
            .runtime
            .lifecycle()
            .recovery
            .is_held(&doors.caddy)
            .await
    );
    doors.calls();
    let creates = doors.runtime.docker.creates();
    doors
        .runtime
        .lifecycle()
        .recover(
            Repair::StartCaddy {
                holder: Some(doors.holder),
                caddy: doors.caddy,
            },
            creates,
        )
        .await;
    assert!(
        doors.calls().is_empty(),
        "a held door is not even inspected"
    );

    doors.runtime.lifecycle().start(&doors.caddy).await.unwrap();
    assert!(
        !doors
            .runtime
            .lifecycle()
            .recovery
            .is_held(&doors.caddy)
            .await
    );
    assert_eq!(
        doors.calls(),
        lines(&[
            "GET /containers/caddy/json",
            "POST /containers/caddy/update",
            "POST /containers/caddy/start",
        ]),
    );
}

// A copy Docker would not stop serves on. Under restart policy no, its next exit or a
// reboot would leave it down.
#[tokio::test]
async fn a_stop_docker_refuses_leaves_the_running_container_its_restart_policy() {
    let doors = Doors::new().await;
    let declared = doors.restart_policy(&doors.caddy);
    doors
        .fake
        .refuse_stop
        .store(true, std::sync::atomic::Ordering::SeqCst);

    doors
        .runtime
        .lifecycle()
        .stop(&doors.caddy, None, None)
        .await
        .unwrap_err();

    assert_eq!(doors.restart_policy(&doors.caddy), declared);
}

// A failed start takes the door's restart policy. The repair that starts it gives the
// policy back, or a later crash of the serving Caddy would leave Ingress down.
#[tokio::test]
async fn a_repaired_caddy_gets_its_restart_policy_back() {
    let doors = Doors::new().await;
    let declared = doors.restart_policy(&doors.caddy);
    doors.fail_start(&doors.caddy);
    doors.set_restart_policy(&doors.caddy, "no");

    let creates = doors.runtime.docker.creates();
    doors
        .runtime
        .lifecycle()
        .recover(
            Repair::StartCaddy {
                holder: Some(doors.holder),
                caddy: doors.caddy,
            },
            creates,
        )
        .await;

    assert_eq!(doors.restart_policy(&doors.caddy), declared);
}

// Docker started Caddy but its reply was lost. Under restart policy no, the serving
// Caddy's next exit would leave Ingress down, and no scan would start it again.
#[tokio::test]
async fn a_caddy_started_under_a_lost_reply_keeps_its_restart_policy() {
    let doors = Doors::new().await;
    let declared = doors.restart_policy(&doors.caddy);
    doors.fail_start(&doors.caddy);
    doors
        .fake
        .lose_start_reply
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let creates = doors.runtime.docker.creates();
    doors
        .runtime
        .lifecycle()
        .recover(
            Repair::StartCaddy {
                holder: Some(doors.holder),
                caddy: doors.caddy,
            },
            creates,
        )
        .await;

    assert_eq!(doors.restart_policy(&doors.caddy), declared);
}

#[tokio::test]
async fn a_caddy_whose_start_failed_is_started_once_its_holder_runs() {
    let doors = Doors::new().await;
    doors.fail_start(&doors.caddy);
    doors.calls();

    let creates = doors.runtime.docker.creates();
    doors
        .runtime
        .lifecycle()
        .recover(
            Repair::StartCaddy {
                holder: Some(doors.holder),
                caddy: doors.caddy,
            },
            creates,
        )
        .await;

    assert_eq!(
        doors.calls(),
        lines(&[
            "GET /containers/caddy/json",
            "GET /containers/holder/json",
            "POST /containers/caddy/start",
        ]),
    );
}

#[tokio::test]
async fn a_caddy_is_left_alone_while_its_holder_is_stopped() {
    let doors = Doors::new().await;
    doors.fail_start(&doors.caddy);
    doors.set_state(&doors.holder, serde_json::json!({"Status": "exited"}));
    doors.calls();

    let creates = doors.runtime.docker.creates();
    doors
        .runtime
        .lifecycle()
        .recover(
            Repair::StartCaddy {
                holder: Some(doors.holder),
                caddy: doors.caddy,
            },
            creates,
        )
        .await;

    assert_eq!(
        doors.calls(),
        lines(&["GET /containers/caddy/json", "GET /containers/holder/json",]),
    );
}

#[tokio::test]
async fn a_holder_whose_start_failed_is_started() {
    let doors = Doors::new().await;
    doors.fail_start(&doors.holder);
    doors.calls();

    let creates = doors.runtime.docker.creates();
    doors
        .runtime
        .lifecycle()
        .recover(Repair::StartHolder(doors.holder), creates)
        .await;

    assert_eq!(
        doors.calls(),
        lines(&[
            "GET /containers/holder/json",
            "GET /networks/ployz",
            "GET /containers/json",
            "POST /networks/ployz/connect",
            "POST /containers/holder/start",
        ]),
    );
}

#[tokio::test]
async fn a_rejoin_that_finds_both_doors_in_one_network_does_nothing() {
    let doors = Doors::new().await;
    doors.calls();

    let creates = doors.runtime.docker.creates();
    doors
        .runtime
        .lifecycle()
        .recover(
            Repair::Rejoin {
                owner: doors.holder,
                joined: doors.caddy,
            },
            creates,
        )
        .await;

    assert_eq!(
        doors.calls(),
        lines(&["GET /containers/holder/json", "GET /containers/caddy/json",]),
    );
}

#[tokio::test]
async fn leaving_retires_the_caddy_before_the_holder_it_joins() {
    let doors = Doors::new().await;
    doors.calls();

    doors
        .runtime
        .lifecycle()
        .remove_all_managed()
        .await
        .unwrap();

    let calls = doors.calls();
    let position = |call: &str| {
        calls
            .iter()
            .position(|seen| seen == call)
            .unwrap_or_else(|| panic!("{call} not in {calls:?}"))
    };
    let retired = [
        "POST /containers/caddy/stop",
        "DELETE /containers/caddy",
        "POST /containers/holder/stop",
        "DELETE /containers/holder",
    ]
    .map(position);
    assert!(retired.is_sorted(), "{calls:?}");
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("POST") || call.starts_with("DELETE"))
            .cloned()
            .collect::<Vec<_>>(),
        lines(&[
            "POST /containers/caddy/stop",
            "DELETE /containers/caddy",
            "POST /containers/holder/stop",
            "DELETE /containers/holder",
        ]),
    );
    assert!(doors.containers.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_leave_marks_the_machine_departing_until_it_fails() {
    let doors = Doors::new().await;
    assert!(!doors.runtime.lifecycle().departing());

    doors
        .runtime
        .lifecycle()
        .remove_all_managed()
        .await
        .unwrap();
    assert!(
        doors.runtime.lifecycle().departing(),
        "a committed Leave keeps the mark"
    );

    doors.runtime.lifecycle().departure_failed();
    assert!(
        !doors.runtime.lifecycle().departing(),
        "a failed Leave clears it"
    );
}

#[tokio::test]
async fn a_leave_that_cannot_retire_a_container_still_marks_the_machine_departing() {
    let doors = Doors::new().await;
    doors
        .fake
        .refuse_remove_of
        .lock()
        .unwrap()
        .insert("ingress".to_owned());

    assert!(
        doors
            .runtime
            .lifecycle()
            .remove_all_managed()
            .await
            .is_err()
    );

    assert!(
        doors.runtime.lifecycle().departing(),
        "the caller clears it when the Leave fails"
    );
}

#[tokio::test]
async fn creating_a_door_asks_docker_for_nothing_it_does_not_need() {
    let doors = Doors::new().await;
    doors.calls();
    let specs = DoorSpecs::public(ployz_core::ServiceId::random());

    create(&doors.runtime, &specs.proxy, "caddy-again").await;

    assert_eq!(
        doors.calls(),
        lines(&[
            "GET /containers/<new caddy>/json",
            "GET /containers/json",
            "GET /containers/holder/json",
            "GET /images/<pinned image>/json",
            "GET /containers/<new caddy>/json",
            "GET /networks/ployz",
            "GET /containers/json",
            "POST /containers/create",
        ])
    );
}
