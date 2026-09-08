use std::{fs, os::unix::fs::PermissionsExt, process::Command};

use ployz::compose::{BuildOptions, capture_build, parse_normalized, plan_build};

#[test]
fn build_plan_selects_dependencies_contexts_and_resolved_names() {
    let project = parse_normalized(
        r#"
name: demo
services:
  base:
    build: ./base
  database:
    build: ./database
  api:
    image: example.test/api:version2
    build:
      context: ./api
      additional_contexts:
        base: service:base
    depends_on: [database]
  frontend:
    image: example.test/frontend@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
    build: ./frontend
  runtime:
    image: alpine:3.23
    depends_on: [api]
"#,
        ".",
    )
    .unwrap();

    let direct = plan_build(
        &project,
        &BuildOptions {
            services: vec!["api".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        direct
            .iter()
            .map(|service| service.name.as_str())
            .collect::<Vec<_>>(),
        ["api", "base"]
    );
    assert_eq!(direct.first().unwrap().image, "example.test/api:version2");
    assert_eq!(
        direct.get(1).unwrap().image,
        project.services.get("base").unwrap().container.image
    );

    let with_deps = plan_build(
        &project,
        &BuildOptions {
            services: vec!["runtime".into(), "frontend".into()],
            deps: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        with_deps
            .iter()
            .map(|service| service.name.as_str())
            .collect::<Vec<_>>(),
        ["api", "base", "database", "frontend"]
    );
    assert_eq!(
        with_deps.get(3).unwrap().image,
        "example.test/frontend@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );

    let none = plan_build(
        &project,
        &BuildOptions {
            services: vec!["runtime".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(none.is_empty());

    let listed = parse_normalized(
        r#"
name: listed
services:
  base: {build: .}
  api:
    build:
      context: ./api
      additional_contexts:
        - base=service:base
"#,
        ".",
    )
    .unwrap();
    let listed_plan = plan_build(
        &listed,
        &BuildOptions {
            services: vec!["api".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        listed_plan
            .iter()
            .map(|service| service.name.as_str())
            .collect::<Vec<_>>(),
        ["api", "base"]
    );

    let cycle = parse_normalized(
        "name: cycle\nservices:\n  a: {build: {context: ., additional_contexts: {b: service:b}}}\n  b: {build: {context: ., additional_contexts: {a: service:a}}}\n",
        ".",
    )
    .unwrap();
    assert!(
        plan_build(
            &cycle,
            &BuildOptions {
                services: vec!["a".into()],
                ..Default::default()
            }
        )
        .unwrap_err()
        .to_string()
        .contains("build dependency cycle")
    );
}

#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
fn captured_build_preserves_sources_configuration_and_builder_flags() {
    let root = std::env::temp_dir().join(format!("ployz-build-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let docker = root.join("docker");
    let compose = root.join("compose.yml");
    let calls = root.join("calls");
    let captured = root.join("override.yaml");
    fs::write(&compose, "services: {}\n").unwrap();
    fs::create_dir(root.join("api")).unwrap();
    fs::create_dir(root.join("shared")).unwrap();
    fs::write(root.join("api/source"), "original source").unwrap();
    fs::write(root.join("shared/data"), "original shared").unwrap();
    fs::write(root.join("Dockerfile"), "FROM scratch\nCOPY . /app\n").unwrap();
    fs::write(root.join("Dockerfile.dockerignore"), "hidden\n").unwrap();
    fs::write(root.join("api/.dockerignore"), "source\n").unwrap();
    fs::create_dir(root.join("api/hidden")).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(root.join("api/hidden/socket")).unwrap();
    fs::write(root.join("shared/.dockerignore"), "socket\n").unwrap();
    let _shared_socket =
        std::os::unix::net::UnixListener::bind(root.join("shared/socket")).unwrap();
    fs::write(root.join("key"), "private-key").unwrap();
    fs::write(
        &docker,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > {:?}\nprevious=\nfor arg in \"$@\"; do\n  if [ \"$previous\" = --file ]; then override=$arg; fi\n  previous=$arg\ndone\ncp \"$override\" {:?}\n",
            calls, captured
        ),
    )
    .unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o700)).unwrap();
    let mut project = parse_normalized(
        "name: demo\nservices:\n  api:\n    image: example.test/api:version2\n    build: {context: ./api, dockerfile: ../Dockerfile, additional_contexts: {shared: ./shared}, ssh: [deploy=./key], args: {VALUE: '$CAPTURED'}}\n  runtime:\n    image: alpine\n",
        &root,
    )
    .unwrap();
    let options = BuildOptions {
        build_args: vec!["MODE=release".into()],
        check: true,
        no_cache: true,
        pull: true,
        push_registry: true,
        ..Default::default()
    };
    let plan = plan_build(&project, &options).unwrap();

    let build = capture_build(&plan, &options, &mut project).unwrap();
    fs::write(&compose, "invalid: [edited after capture").unwrap();
    fs::remove_dir_all(root.join("api")).unwrap();
    fs::remove_dir_all(root.join("shared")).unwrap();
    fs::remove_file(root.join("Dockerfile")).unwrap();
    fs::remove_file(root.join("Dockerfile.dockerignore")).unwrap();
    fs::remove_file(root.join("key")).unwrap();
    project.builds.clear();
    build.execute(Some(&docker)).unwrap();
    let call = fs::read_to_string(calls).unwrap();
    assert!(call.starts_with("compose --all-resources --project-name demo --file "));
    assert_eq!(call.matches("--file").count(), 1);
    assert!(!call.contains(compose.to_str().unwrap()));
    assert!(
        call.ends_with(" build --build-arg MODE=release --check --no-cache --pull --push api\n")
    );
    let override_yaml = fs::read_to_string(captured).unwrap();
    assert!(override_yaml.contains("api"));
    assert!(override_yaml.contains("example.test/api:version2"));
    let config: serde_norway::Value = serde_norway::from_str(&override_yaml).unwrap();
    let captured_build = &config["services"]["api"]["build"];
    let context = std::path::Path::new(captured_build["context"].as_str().unwrap());
    assert!(!context.join("hidden").exists());
    assert_eq!(
        fs::read_to_string(context.join("source")).unwrap(),
        "original source"
    );
    assert_eq!(
        fs::read_to_string(captured_build["dockerfile"].as_str().unwrap()).unwrap(),
        "FROM scratch\nCOPY . /app\n"
    );
    let dockerfile = captured_build["dockerfile"].as_str().unwrap();
    assert_eq!(
        fs::read_to_string(format!("{dockerfile}.dockerignore")).unwrap(),
        "hidden\n"
    );
    let ssh = captured_build["ssh"][0]
        .as_str()
        .unwrap()
        .strip_prefix("deploy=")
        .unwrap();
    assert_eq!(fs::read_to_string(ssh).unwrap(), "private-key");
    let shared = std::path::Path::new(
        captured_build["additional_contexts"]["shared"]
            .as_str()
            .unwrap(),
    );
    assert!(!shared.join("socket").exists());
    assert_eq!(
        fs::read_to_string(shared.join("data")).unwrap(),
        "original shared"
    );
    assert_eq!(captured_build["args"]["VALUE"].as_str(), Some("$$CAPTURED"));
    let context = context.to_owned();
    drop(build);
    assert!(!context.exists());
    assert!(!override_yaml.contains("runtime"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn check_with_direct_push_stops_after_validation() {
    let root = std::env::temp_dir().join(format!("ployz-build-check-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("compose.yaml"),
        "services: {api: {image: example.test/api, build: .}}\n",
    )
    .unwrap();
    fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
    let calls = root.join("calls");
    let docker = root.join("docker");
    fs::write(
        &docker,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n  *'config --environment') exit 0 ;;\n  *' config '*) printf 'name: demo\\nservices:\\n  api:\\n    image: example.test/api\\n    build: {{context: .}}\\n'; exit 0 ;;\n  *' build '*) exit 0 ;;\nesac\nexit 1\n",
            calls.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o700)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            "tcp://127.0.0.1:1",
            "build",
            "--check",
            "--push",
        ])
        .current_dir(&root)
        .env("PATH", format!("{}:/usr/bin:/bin", root.display()))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::read_to_string(calls)
            .unwrap()
            .lines()
            .any(|call| call.contains(" build --check api"))
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
fn build_and_runtime_share_one_captured_secret_resolution() {
    let root = std::env::temp_dir().join(format!("ployz-build-secret-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/Dockerfile"), "FROM scratch\n").unwrap();
    let mut project = parse_normalized(
        r#"
name: demo
services:
  api:
    image: api:1
    build: {context: ./src, secrets: [token]}
    environment: {TOKEN: 'secret://token'}
secrets:
  token: {x-command: "sh -c 'echo once >> calls; printf private-token'"}
"#,
        &root,
    )
    .unwrap();
    let options = BuildOptions::default();
    let plan = plan_build(&project, &options).unwrap();
    let build = capture_build(&plan, &options, &mut project).unwrap();
    project.resolve_secrets().unwrap();
    project.resolve_secrets().unwrap();
    assert_eq!(fs::read_to_string(root.join("calls")).unwrap(), "once\n");
    assert_eq!(
        project.services["api"].container.environment["TOKEN"],
        "private-token"
    );
    drop(build);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn mutable_remote_build_context_is_rejected_before_execution() {
    let mut project = parse_normalized(
        "name: demo\nservices: {api: {build: 'https://example.test/repo.git#main'}}",
        ".",
    )
    .unwrap();
    let options = BuildOptions::default();
    let plan = plan_build(&project, &options).unwrap();
    let error = match capture_build(&plan, &options, &mut project) {
        Ok(_) => panic!("mutable remote context was admitted"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("immutable Git commit or image digest")
    );
}
