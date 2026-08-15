use ployz::compose::{ComposeProject, parse_normalized};
use ployz_core::{RequestedServiceSpec, VolumeSource};

fn app(project: &ComposeProject) -> &RequestedServiceSpec {
    project.services.get("app").unwrap()
}

#[test]
fn compose_maps_stop_grace_period_to_whole_docker_seconds() {
    let project = parse_normalized(
        "services: {app: {image: app, stop_grace_period: 1500ms}}",
        ".",
    )
    .unwrap();
    assert_eq!(app(&project).container.stop_timeout_secs, Some(1));
}

#[test]
fn compose_rejects_invalid_restart_pid_and_bind_recursive() {
    for (yaml, expected) in [
        (
            "services: {app: {image: app, restart: maybe}}",
            "restart policy",
        ),
        ("services: {app: {image: app, pid: private}}", "PID mode"),
        (
            "services: {app: {image: app, volumes: [{type: bind, source: /srv, target: /host, bind: {recursive: enabled}}]}}",
            "bind recursive",
        ),
    ] {
        let error = parse_normalized(yaml, ".").unwrap_err().to_string();
        assert!(
            error.contains(expected),
            "{error:?} did not contain {expected:?}"
        );
    }
}

#[test]
fn compose_maps_bind_recursive_disabled() {
    let project = parse_normalized(
        "services: {app: {image: app, volumes: [{type: bind, source: /srv, target: /host, bind: {recursive: disabled}}]}}",
        ".",
    )
    .unwrap();
    assert!(app(&project).volumes.iter().any(|volume| matches!(
        &volume.source,
        VolumeSource::Bind {
            recursive: Some(ployz_core::BindRecursive::Disabled),
            ..
        }
    )));
}
