use std::{fs, os::unix::fs::PermissionsExt, process::Command};

use ployz::compose::{BuildOptions, LoadOptions, execute_build, parse_normalized, plan_build};

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
fn compose_build_receives_resolved_images_and_builder_flags() {
    let root = std::env::temp_dir().join(format!("ployz-build-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let docker = root.join("docker");
    let compose = root.join("compose.yml");
    let calls = root.join("calls");
    let captured = root.join("override.yaml");
    fs::write(&compose, "services: {}\n").unwrap();
    fs::write(
        &docker,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > {:?}\nprevious=\nfor arg in \"$@\"; do\n  if [ \"$previous\" = --file ]; then override=$arg; fi\n  previous=$arg\ndone\ncp \"$override\" {:?}\n",
            calls, captured
        ),
    )
    .unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o700)).unwrap();
    let project = parse_normalized(
        "name: demo\nservices:\n  api:\n    image: example.test/api:version2\n    build: ./api\n  runtime:\n    image: alpine\n",
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

    execute_build(
        &plan,
        &options,
        &LoadOptions {
            files: Vec::new(),
            profiles: vec!["test".into()],
            working_dir: Some(root.clone()),
            docker: Some(docker),
            ..Default::default()
        },
    )
    .unwrap();
    let call = fs::read_to_string(calls).unwrap();
    assert!(call.starts_with(&format!(
        "compose --all-resources --file {} --file ",
        compose.display()
    )));
    assert!(call.contains(" --profile test build "));
    assert!(
        call.ends_with(" build --build-arg MODE=release --check --no-cache --pull --push api\n")
    );
    let override_yaml = fs::read_to_string(captured).unwrap();
    assert!(override_yaml.contains("api"));
    assert!(override_yaml.contains("example.test/api:version2"));
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
