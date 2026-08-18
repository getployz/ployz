use crate::project::{ProjectNameSource, resolve_run_command};
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
    assert!(!spec.volume_graph.mounts().is_empty());
    assert!(spec.config_graph.mounts().is_empty());
    assert!(spec.mounts().first().is_some_and(|mount| mount.read_only));
    assert!(matches!(
        spec.volumes().get(1).map(|volume| &volume.source),
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
    assert_eq!(
        run_spec(super::leaf_matches(&assigned))
            .unwrap()
            .ports
            .first(),
        Some(&PortPublication::Ingress {
            hostname: IngressHostname::cluster_domain(),
            load_balancer_port: 443.try_into().unwrap(),
            container_port: 8080.try_into().unwrap(),
            http_protocol: HttpProtocol::Https,
        })
    );
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
    let chosen = crate::cli::command()
        .try_get_matches_from([
            "ployz",
            "run",
            "--name",
            "api",
            "--publish",
            "api:8080/https",
            "alpine",
        ])
        .unwrap();
    assert_eq!(
        run_spec(super::leaf_matches(&chosen))
            .unwrap()
            .ports
            .first(),
        Some(&PortPublication::Ingress {
            hostname: IngressHostname::cluster_domain_label("api").unwrap(),
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
fn run_places_the_service_in_default_or_an_explicit_project() {
    let resolve = |args: &[&str]| {
        let matches = crate::cli::command().try_get_matches_from(args).unwrap();
        resolve_run_command(super::leaf_matches(&matches)).unwrap()
    };
    let bare = resolve(&["ployz", "run", "alpine"]);
    assert_eq!(bare.name.as_str(), "default");
    assert_eq!(bare.source, ProjectNameSource::Default);
    let named = resolve(&[
        "ployz",
        "run",
        "--project-name",
        "shop",
        "--name",
        "web",
        "alpine",
    ]);
    assert_eq!(named.name.as_str(), "shop");
    assert_eq!(named.source, ProjectNameSource::CommandLine);
    let nested = resolve(&[
        "ployz",
        "service",
        "run",
        "--project-name",
        "shop",
        "alpine",
    ]);
    assert_eq!(nested.name, named.name);
    assert_eq!(nested.source, named.source);
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

#[test]
fn run_placement_rejects_star_and_keeps_all_as_identity() {
    let command = crate::cli::command();
    let rejected = command
        .clone()
        .try_get_matches_from(["ployz", "run", "--machine", "*", "alpine"])
        .unwrap();
    assert!(run_spec(super::leaf_matches(&rejected)).is_err());

    let named_all = command
        .try_get_matches_from(["ployz", "run", "--machine", "all", "alpine"])
        .unwrap();
    assert_eq!(
        run_spec(super::leaf_matches(&named_all))
            .unwrap()
            .placement
            .machines
            .first()
            .unwrap()
            .as_str(),
        "all"
    );
}

#[test]
fn deploy_loads_every_profile_and_leaves_named_services_for_the_planner() {
    let named = crate::cli::command()
        .try_get_matches_from(["ployz", "deploy", "web"])
        .unwrap();
    let load = deploy_load(super::leaf_matches(&named));
    assert!(load.all_profiles);
    assert_eq!(
        string_values(super::leaf_matches(&named), "service"),
        ["web"]
    );

    let full = crate::cli::command()
        .try_get_matches_from(["ployz", "deploy"])
        .unwrap();
    let load = deploy_load(super::leaf_matches(&full));
    assert!(load.all_profiles);
    assert!(string_values(super::leaf_matches(&full), "service").is_empty());

    let explicit = crate::cli::command()
        .try_get_matches_from(["ployz", "deploy", "--file", "prod.yaml"])
        .unwrap();
    assert!(has_explicit_nondefault_compose_file(&deploy_load(
        super::leaf_matches(&explicit)
    )));
    let default_name = crate::cli::command()
        .try_get_matches_from(["ployz", "deploy", "--file", "compose.yaml"])
        .unwrap();
    assert!(!has_explicit_nondefault_compose_file(&deploy_load(
        super::leaf_matches(&default_name)
    )));
}
