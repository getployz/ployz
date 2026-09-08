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
    assert!(!spec.volume_graph().mounts().is_empty());
    assert!(spec.config_graph().mounts().is_empty());
    assert!(spec.mounts().first().is_some_and(|mount| mount.read_only));
    assert!(spec.mounts().get(1).is_some_and(|mount| mount.no_copy));

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

#[test]
fn run_rejects_cpu_overflow_and_preserves_fractional_quantities() {
    for invalid in ["1e20", "NaN", "inf", "9223372036.854776"] {
        let matches = crate::cli::command()
            .try_get_matches_from(["ployz", "run", "--cpu", invalid, "alpine"])
            .unwrap();
        assert!(
            run_spec(super::leaf_matches(&matches)).is_err(),
            "{invalid}"
        );
    }
    let matches = crate::cli::command()
        .try_get_matches_from([
            "ployz",
            "run",
            "--cpu",
            "0.125",
            "--memory",
            "9223372036854775807",
            "alpine",
        ])
        .unwrap();
    let spec = run_spec(super::leaf_matches(&matches)).unwrap();
    let resources = serde_json::to_value(spec.container.resources).unwrap();
    assert_eq!(
        resources.get("cpu_nanos"),
        Some(&serde_json::json!(125_000_000))
    );
    assert_eq!(
        resources.get("memory_bytes"),
        Some(&serde_json::json!(9_223_372_036_854_775_807_i64))
    );
}

#[test]
fn deploy_preparation_captures_resolved_input_and_selection_before_planning() {
    let directory = std::env::temp_dir().join(format!("ployz-prepare-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(
        directory.join("provider.sh"),
        "printf x >> calls; printf resolved",
    )
    .unwrap();
    let source = directory.join("chosen.yaml");
    let yaml = "services: {web: {image: nginx, command: [captured], depends_on: [db], environment: {A: 'secret://token', B: 'secret://token'}}, db: {image: postgres, profiles: [data]}}\nsecrets: {token: {x-command: '/bin/sh provider.sh'}}";
    std::fs::write(&source, yaml).unwrap();
    let project = crate::compose::parse_normalized(yaml, &directory).unwrap();
    let root = crate::cli::command()
        .try_get_matches_from(["ployz", "deploy", "--no-build", "web"])
        .unwrap();
    let matches = leaf_matches(&root);
    let load = LoadOptions {
        files: vec![source.clone()],
        working_dir: Some(directory.clone()),
        profiles: vec!["data".into()],
        all_profiles: true,
        ..Default::default()
    };
    let resolved = ResolvedProject {
        name: ployz_core::ProjectName::parse("shop").unwrap(),
        source: ProjectNameSource::CommandLine,
    };
    let options = ployz_core::PlanOptions {
        selected: selected_attempts(&project, &["web".into()]).unwrap(),
        ..Default::default()
    };
    std::fs::write(&source, yaml.replace("captured", "later-edit")).unwrap();
    let (candidate, builds) = prepare_deploy(matches, &load, project, &resolved, options).unwrap();
    assert!(builds.is_empty());
    assert_eq!(
        std::fs::read_to_string(directory.join("calls")).unwrap(),
        "x"
    );
    assert_eq!(candidate.source().requested_files, [source]);
    assert_eq!(candidate.intent().project_name, resolved.name);
    assert_eq!(
        candidate.intent().prune_refusal(true),
        Some(ployz_core::PruneRefusal::SelectedServices)
    );
    assert_eq!(
        candidate
            .intent()
            .applied_names()
            .into_iter()
            .map(|name| name.as_str())
            .collect::<Vec<_>>(),
        ["db", "web"]
    );
    let web = candidate
        .intent()
        .target
        .iter()
        .find(|spec| spec.name.as_str() == "web")
        .unwrap();
    assert_eq!(web.container.command, ["captured"]);
    assert_eq!(web.container.environment.get("A").unwrap(), "resolved");
    assert_eq!(web.container.environment.get("B").unwrap(), "resolved");
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_failed_build_leaves_the_deployment_unattempted() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = std::env::temp_dir().join(format!("ployz-build-fail-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("Dockerfile"), "FROM scratch\n").unwrap();
    let yaml = "services: {web: {image: 'example.test/web:1', build: .}}";
    let source = directory.join("compose.yaml");
    std::fs::write(&source, yaml).unwrap();
    let docker = directory.join("docker");
    // Every builder step succeeds; only the build itself fails.
    std::fs::write(
        &docker,
        "#!/bin/sh\ncase \"$1 $2\" in\n  'version --format') echo linux/amd64; exit 0 ;;\n  'buildx ls'|'buildx create'|'buildx inspect'|'buildx rm') exit 0 ;;\nesac\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o700)).unwrap();
    let project = crate::compose::parse_normalized(yaml, &directory).unwrap();
    let root = crate::cli::command()
        .try_get_matches_from(["ployz", "deploy"])
        .unwrap();
    let load = LoadOptions {
        files: vec![source],
        working_dir: Some(directory.clone()),
        docker: Some(docker),
        all_profiles: true,
        ..Default::default()
    };
    let resolved = ResolvedProject {
        name: ployz_core::ProjectName::parse("shop").unwrap(),
        source: ProjectNameSource::CommandLine,
    };

    let error = match prepare_deploy(
        leaf_matches(&root),
        &load,
        project,
        &resolved,
        ployz_core::PlanOptions::default(),
    ) {
        Ok(_) => panic!("a failed build was admitted for deployment"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("No Service, hook, or volume change was attempted"),
        "{error}"
    );
    std::fs::remove_dir_all(directory).unwrap();
}
