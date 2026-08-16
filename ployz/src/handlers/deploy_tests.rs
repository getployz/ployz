use ployz_core::{HttpProtocol, IngressHostname, PortPublication, RestartPolicy, ServiceMode};

use super::*;

#[test]
fn run_normalizes_supported_inputs_and_rejects_l4_ingress() {
    let command = crate::cli::command();
    let matches = command
        .try_get_matches_from([
            "ployz",
            "run",
            "--name",
            "api",
            "--env",
            "A=b",
            "--publish",
            "8080:80@host",
            "--volume",
            "data:/data:ro",
            "--volume",
            "cache:/cache:volume-nocopy",
            "alpine",
            "echo",
            "hello",
        ])
        .unwrap();
    let spec = run_spec(super::leaf_matches(&matches)).unwrap();
    assert_eq!(spec.name.as_str(), "api");
    assert_eq!(spec.container.command, ["echo", "hello"]);
    assert_eq!(spec.container.restart, RestartPolicy::No);
    assert_eq!(
        spec.container.environment.get("A").map(String::as_str),
        Some("b")
    );
    assert!(matches!(
        spec.ports.first(),
        Some(PortPublication::Host { .. })
    ));
    assert!(spec.mounts.first().is_some_and(|mount| mount.read_only));
    assert!(matches!(
        spec.volumes.get(1).map(|volume| &volume.source),
        Some(ployz_core::VolumeSource::Named { no_copy: true, .. })
    ));

    let matches = crate::cli::command()
        .try_get_matches_from([
            "ployz",
            "run",
            "--volume",
            "/tmp:/data:volume-nocopy",
            "alpine",
        ])
        .unwrap();
    assert!(run_spec(super::leaf_matches(&matches)).is_err());

    let matches = crate::cli::command()
        .try_get_matches_from(["ployz", "run", "--publish", "8080:80", "alpine"])
        .unwrap();
    assert!(
        run_spec(super::leaf_matches(&matches))
            .unwrap_err()
            .to_string()
            .contains("host publication")
    );

    let assigned = crate::cli::command()
        .try_get_matches_from([
            "ployz",
            "run",
            "--name",
            "api",
            "--publish",
            "8080/https",
            "alpine",
        ])
        .unwrap();
    assert!(matches!(
        run_spec(super::leaf_matches(&assigned))
            .unwrap()
            .ports
            .first(),
        Some(PortPublication::Ingress {
            hostname: IngressHostname::AssignFromClusterDomain,
            http_protocol: HttpProtocol::Https,
            ..
        })
    ));
    let explicit = crate::cli::command()
        .try_get_matches_from([
            "ployz",
            "run",
            "--name",
            "api",
            "--publish",
            "app.example.com:8080/https",
            "alpine",
        ])
        .unwrap();
    assert_eq!(
        run_spec(super::leaf_matches(&explicit))
            .unwrap()
            .ports
            .first(),
        Some(&PortPublication::Ingress {
            hostname: IngressHostname::explicit("app.example.com").unwrap(),
            load_balancer_port: 443.try_into().unwrap(),
            container_port: 8080.try_into().unwrap(),
            http_protocol: HttpProtocol::Https,
        })
    );

    let global = crate::cli::command()
        .try_get_matches_from(["ployz", "run", "--mode", "global", "alpine"])
        .unwrap();
    assert_eq!(
        run_spec(super::leaf_matches(&global)).unwrap().mode,
        ServiceMode::Global
    );
    let global_replicas = crate::cli::command()
        .try_get_matches_from([
            "ployz",
            "run",
            "--mode",
            "global",
            "--replicas",
            "2",
            "alpine",
        ])
        .unwrap();
    assert!(run_spec(super::leaf_matches(&global_replicas)).is_err());
}

#[test]
fn run_pulls_untagged_images_as_latest() {
    let image = |image: &str| {
        let matches = crate::cli::command()
            .try_get_matches_from(["ployz", "run", "--name", "api", image, "sleep", "30"])
            .unwrap();
        run_spec(super::leaf_matches(&matches))
            .unwrap()
            .container
            .image
    };
    assert_eq!(image("alpine"), "alpine:latest");
    assert_eq!(image("alpine:3.20"), "alpine:3.20");
    let digest = format!("alpine@sha256:{}", "0".repeat(64));
    assert_eq!(image(&digest), digest);
    assert_eq!(image("localhost:5000/foo"), "localhost:5000/foo:latest");
}

#[test]
fn run_forms_share_normalization() {
    let root = crate::cli::command()
        .try_get_matches_from(["ployz", "run", "--name", "api", "alpine"])
        .unwrap();
    let nested = crate::cli::command()
        .try_get_matches_from(["ployz", "service", "run", "--name", "api", "alpine"])
        .unwrap();
    assert_eq!(
        run_spec(super::leaf_matches(&root)).unwrap(),
        run_spec(super::leaf_matches(&nested)).unwrap()
    );
}
