use super::*;

fn report(state: ContainerState, exit_code: Option<i64>, stopped: bool) -> ContainerReport {
    ContainerReport {
        server: ServerName::parse("web-1").unwrap(),
        service: ServiceName::parse("web").unwrap(),
        state,
        exit_code,
        stopped,
        health: None,
        restarts: None,
        image: "example.test/web".into(),
        deployment: None,
    }
}

#[test]
fn a_container_its_server_stopped_is_stopped_whatever_its_exit_code() {
    assert_eq!(
        container(&report(ContainerState::Exited, Some(1), true)),
        Container::Stopped
    );
    assert_eq!(
        container(&report(ContainerState::Exited, Some(1), false)),
        Container::Down
    );
    assert_eq!(
        container(&report(ContainerState::Exited, Some(0), false)),
        Container::Idle
    );
}

#[test]
fn an_answer_from_a_cloud_older_than_the_stop_mark_reads_its_exit_code_as_before() {
    let wire = serde_json::to_value(report(ContainerState::Exited, Some(1), false)).unwrap();
    assert_eq!(wire.get("stopped"), None, "an older Cloud never sends it");

    let decoded: ContainerReport = serde_json::from_value(wire).unwrap();

    assert!(!decoded.stopped);
    assert_eq!(container(&decoded), Container::Down);
}
