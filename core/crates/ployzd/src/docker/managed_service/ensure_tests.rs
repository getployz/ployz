//! The Docker calls one `ensure` makes, for a service on the host's Docker and for one on
//! the local endpoint.

use std::sync::{Arc, Mutex};

use super::*;
use crate::docker::test_support::*;

const PINNED: PinnedImage = PinnedImage::new(
    "example.test/service@sha256:1111111111111111111111111111111111111111111111111111111111111111",
);
const IMAGE: &str = "GET /images/example.test/service@sha256:1111111111111111111111111111111111111111111111111111111111111111/json";
const CREATE: &str = "POST /containers/create";
const START: &str = "POST /containers/svc/start";
const INSPECT: &str = "GET /containers/svc/json";

fn desired(command: &str) -> DesiredContainer {
    DesiredContainer::new(
        PINNED,
        ContainerCreateBody {
            cmd: Some(vec![command.to_owned()]),
            ..Default::default()
        },
        &[],
    )
}

/// The requests Docker received since the last call.
fn calls(fake: &FakeDocker) -> Vec<String> {
    fake.requests
        .lock()
        .unwrap()
        .drain(..)
        .map(|(method, path)| format!("{method} {path}"))
        .collect()
}

async fn local_docker() -> (LocalDocker, FakeDocker) {
    let containers = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
    let (runtime, fake) = fake_runtime_with(FakeDocker {
        named_containers: Some(containers),
        ..Default::default()
    })
    .await;
    (runtime.docker, fake)
}

#[tokio::test]
async fn a_service_on_the_host_docker_creates_starts_and_replaces() {
    let (docker, fake) = local_docker().await;
    let service = ManagedService::new(docker.client.clone(), "svc");

    assert_eq!(
        service.ensure(desired("one")).await.unwrap(),
        Ensure::Create
    );
    assert_eq!(calls(&fake), [INSPECT, IMAGE, CREATE, START]);

    assert_eq!(service.ensure(desired("one")).await.unwrap(), Ensure::Start);
    assert_eq!(calls(&fake), [INSPECT, START]);

    assert_eq!(
        service.ensure(desired("two")).await.unwrap(),
        Ensure::Replace
    );
    assert_eq!(
        calls(&fake),
        [
            INSPECT,
            IMAGE,
            "POST /containers/svc/stop",
            "DELETE /containers/svc",
            IMAGE,
            CREATE,
            START,
        ]
    );
}

#[tokio::test]
async fn a_service_on_the_endpoint_creates_through_the_local_docker() {
    let (docker, fake) = local_docker().await;
    let service = ManagedService::new(docker.clone(), "svc");
    let network = ["GET /networks/ployz", "GET /containers/json"];

    service.ensure(desired("one")).await.unwrap();
    assert_eq!(
        calls(&fake),
        [&[INSPECT, IMAGE][..], &network, &[CREATE, START]].concat()
    );

    service.ensure(desired("one")).await.unwrap();
    assert_eq!(calls(&fake), [INSPECT, START]);

    service.ensure(desired("two")).await.unwrap();
    assert_eq!(
        calls(&fake),
        [
            &[
                INSPECT,
                IMAGE,
                "POST /containers/svc/stop",
                "DELETE /containers/svc",
                IMAGE,
            ][..],
            &network,
            &[CREATE, START],
        ]
        .concat()
    );
    assert_eq!(docker.creates(), 2);
}
