use super::*;

fn report(state: ContainerState, exit_code: Option<i64>) -> ContainerReport {
    ContainerReport {
        server: ServerName::parse("web-1").unwrap(),
        service: ServiceName::parse("web").unwrap(),
        state,
        exit_code,
        health: None,
        restarts: None,
        image: "example.test/web".into(),
        deployment: None,
    }
}

#[test]
fn a_container_its_server_stopped_is_stopped_whatever_its_exit_code() {
    assert_eq!(
        container(&report(ContainerState::Stopped, Some(1))),
        Container::Stopped
    );
    assert_eq!(
        container(&report(ContainerState::Exited, Some(1))),
        Container::Down
    );
    assert_eq!(
        container(&report(ContainerState::Exited, Some(0))),
        Container::Idle
    );
}
