use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use ployz::compose::{LoadOptions, load_project};
use ployz_core::IngressProxyFragment;

#[test]
fn compose_loading_does_not_invoke_docker() {
    let root = test_dir("no-docker");
    let calls = root.join("calls");
    let docker = executable(
        &root,
        "docker",
        &format!("#!/bin/sh\ntouch '{}'\nexit 1\n", calls.display()),
    );
    fs::write(
        root.join("compose.yaml"),
        "services: {app: {image: alpine}}\n",
    )
    .unwrap();
    let options = LoadOptions {
        working_dir: Some(root.clone()),
        docker: Some(docker),
        ..Default::default()
    };
    assert!(load_project(&options).unwrap().services.contains_key("app"));
    fs::write(
        root.join("compose.yaml"),
        "services: {app: {image: alpine, depends_on: [missing]}}\n",
    )
    .unwrap();
    assert!(
        load_project(&options)
            .unwrap_err()
            .to_string()
            .contains("missing")
    );
    assert!(!calls.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn real_compose_normalizes_without_a_daemon_and_project_inputs_stay_relative() {
    let root = test_dir("real");
    fs::write(
        root.join(".env"),
        "PLOYZ_TEST_CONTEXT=production\nSECRET_VALUE=command-value\n",
    )
    .unwrap();
    fs::write(
        root.join("Caddyfile"),
        "api.example.test { reverse_proxy api:80 }\n",
    )
    .unwrap();
    fs::write(root.join("settings.txt"), "setting=true\n").unwrap();
    fs::write(root.join("file-secret.txt"), "file-value").unwrap();
    fs::write(
        root.join("extra.yaml"),
        "services:\n  worker:\n    image: busybox\n    profiles: [tools]\n",
    )
    .unwrap();
    let compose = root.join("compose.yaml");
    fs::write(
        &compose,
        r#"name: demo
include: [extra.yaml]
x-context: ${PLOYZ_TEST_CONTEXT:-fallback}
services:
  api:
    image: busybox
    x-caddy: Caddyfile
    environment:
      FILE_SECRET: secret://file-secret
      COMMAND_SECRET: secret://command-secret
    configs:
      - source: settings
        target: /etc/settings
configs:
  settings:
    file: ./settings.txt
secrets:
  file-secret:
    file: ./file-secret.txt
  command-secret:
    x-command: printenv SECRET_VALUE
"#,
    )
    .unwrap();
    let docker = executable(&root, "real-docker", "#!/bin/sh\nexit 99\n");
    let mut project = load_project(&LoadOptions {
        command: "deploy".into(),
        files: vec![compose],
        all_profiles: true,
        working_dir: Some(root.clone()),
        docker: Some(docker.clone()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(project.context.as_deref(), Some("production"));
    assert_eq!(
        project
            .services
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["api", "worker"]
    );
    let api = project.services.get("api").unwrap();
    assert_eq!(
        api.ingress_proxy_fragment
            .as_ref()
            .map(IngressProxyFragment::as_str),
        Some("api.example.test { reverse_proxy api:80 }")
    );
    assert_eq!(api.configs().first().unwrap().content, b"setting=true\n");
    project.resolve_secrets().unwrap();
    let environment = &project.services.get("api").unwrap().container.environment;
    assert_eq!(environment.get("FILE_SECRET").unwrap(), "file-value");
    assert_eq!(environment.get("COMMAND_SECRET").unwrap(), "command-value");

    let relative = root.join("relative.yaml");
    fs::write(
        &relative,
        "services: {api: {image: busybox, volumes: ['./data:/data']}}\n",
    )
    .unwrap();
    assert!(
        load_project(&LoadOptions {
            command: "deploy".into(),
            files: vec![relative],
            working_dir: Some(root.clone()),
            docker: Some(docker.clone()),
            ..Default::default()
        })
        .unwrap_err()
        .to_string()
        .contains("relative")
    );
    let external = root.join("external.yaml");
    fs::write(
        &external,
        "services: {api: {image: busybox}}\nconfigs: {outside: {external: true}}\n",
    )
    .unwrap();
    assert!(
        load_project(&LoadOptions {
            command: "deploy".into(),
            files: vec![external],
            working_dir: Some(root.clone()),
            docker: Some(docker.clone()),
            ..Default::default()
        })
        .unwrap_err()
        .to_string()
        .contains("external configs")
    );
    fs::write(
        root.join("compose.yaml"),
        "include: [extra.yaml]\nx-context: ${PLOYZ_TEST_CONTEXT:-fallback}\nservices: {api: {image: busybox}}\n",
    )
    .unwrap();
    let nested = root.join("nested");
    fs::create_dir(&nested).unwrap();
    let discovered = load_project(&LoadOptions {
        command: "build".into(),
        all_profiles: true,
        working_dir: Some(nested),
        docker: Some(docker),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(discovered.working_dir, root);
    assert_eq!(discovered.context.as_deref(), Some("production"));
    assert_eq!(
        discovered
            .services
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["api", "worker"]
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn real_compose_loads_mount_declared_only_by_x_volumes() {
    let (root, docker) = docker_fixture("provisioned-volume");
    let compose = root.join("compose.yaml");
    fs::write(
        &compose,
        r#"services:
  app:
    image: busybox
    volumes: [data:/data]
x-volumes:
  data: 10G
"#,
    )
    .unwrap();
    let project = load_project(&LoadOptions {
        command: "deploy".into(),
        files: Vec::new(),
        working_dir: Some(root.clone()),
        docker: Some(docker),
        ..Default::default()
    })
    .unwrap();

    assert!(matches!(
        &project
            .services
            .get("app")
            .unwrap()
            .volumes()
            .first()
            .unwrap()
            .source.kind(),
        ployz_core::RawVolumeSource::Provisioned {
            name,
            maximum_bytes,
            labels,
        } if name.as_str() == "data"
            && maximum_bytes.get() == 10 * 1024_u64.pow(3)
            && labels.is_empty()
    ));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn real_compose_x_volumes_preserve_other_consistency_checks() {
    let (root, docker) = docker_fixture("provisioned-volume-consistency");
    let compose = root.join("compose.yaml");
    fs::write(
        &compose,
        r#"services:
  app:
    image: busybox
    volumes: [data:/data]
    secrets: [missing]
x-volumes:
  data: 10G
"#,
    )
    .unwrap();
    let error = match load_project(&LoadOptions {
        command: "deploy".into(),
        files: vec![compose],
        working_dir: Some(root.clone()),
        docker: Some(docker),
        ..Default::default()
    }) {
        Err(error) => error,
        Ok(_) => panic!("accepted a Service mount of an undeclared secret"),
    };

    assert!(error.to_string().contains("undefined"), "{error}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn real_compose_still_rejects_undeclared_volume_mounts() {
    let (root, docker) = docker_fixture("undeclared-volume");
    let compose = root.join("compose.yaml");
    fs::write(
        &compose,
        "services: {app: {image: busybox, volumes: [missing:/data]}}\n",
    )
    .unwrap();
    let error = match load_project(&LoadOptions {
        command: "deploy".into(),
        files: vec![compose],
        working_dir: Some(root.clone()),
        docker: Some(docker),
        ..Default::default()
    }) {
        Err(error) => error,
        Ok(_) => panic!("accepted an undeclared Volume reference"),
    };

    assert!(error.to_string().contains("undefined"), "{error}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn real_compose_recovers_command_secrets_selected_by_compose_file() {
    let root = test_dir("compose-file-command");
    let project = root.join("project");
    let caller = root.join("caller");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&caller).unwrap();
    fs::write(
        project.join("Caddyfile"),
        "api.example.test { reverse_proxy api:80 }\n",
    )
    .unwrap();
    let compose = project.join("compose.yaml");
    fs::write(
        &compose,
        r#"name: demo
services:
  api:
    image: busybox
    x-caddy: Caddyfile
secrets:
  token:
    x-command: printf resolved
"#,
    )
    .unwrap();
    executable(&root, "docker", "#!/bin/sh\nexit 99\n");
    let output = ployz(&root)
        .arg("deploy")
        .current_dir(caller)
        .env("COMPOSE_FILE", compose)
        .env("PATH", format!("{}:/usr/bin:/bin", root.display()))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("no Ployz config or local daemon socket is available")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn real_compose_unescapes_literal_dollars_into_the_requested_spec() {
    let root = test_dir("literal-dollars");
    let compose = root.join("compose.yaml");
    fs::write(
        &compose,
        r#"name: demo
services:
  a:
    image: alpine:3.20
    command: ["echo", "a$$b"]
    entrypoint: ["/bin/sh", "-c", "echo a$$b"]
    environment:
      LITERAL: "a$$b"
    volumes:
      - type: volume
        source: data
        target: /data
volumes:
  data:
    name: demo_data
    labels:
      tier: "a$$b"
"#,
    )
    .unwrap();
    let docker = executable(&root, "real-docker", "#!/bin/sh\nexit 99\n");
    let project = load_project(&LoadOptions {
        command: "deploy".into(),
        files: vec![compose],
        working_dir: Some(root.clone()),
        docker: Some(docker),
        ..Default::default()
    })
    .unwrap();
    let service = project.services.get("a").unwrap();
    assert_eq!(
        service
            .container
            .environment
            .get("LITERAL")
            .map(String::as_str),
        Some("a$b")
    );
    assert_eq!(service.container.command, ["echo", "a$b"]);
    assert_eq!(service.container.entrypoint, ["/bin/sh", "-c", "echo a$b"]);
    let ployz_core::RawVolumeSource::Ordinary { labels, .. } =
        service.volumes().first().unwrap().source.kind()
    else {
        panic!("expected an ordinary volume");
    };
    assert_eq!(labels.get("tier").map(String::as_str), Some("a$b"));
    fs::remove_dir_all(root).unwrap();
}

fn test_dir(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "ployz-compose-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn ployz(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ployz"));
    command.env("PLOYZ_CONFIG", root.join("config.yaml"));
    command
}

fn docker_fixture(label: &str) -> (PathBuf, PathBuf) {
    let root = test_dir(label);
    let docker = executable(&root, "real-docker", "#!/bin/sh\nexit 99\n");
    (root, docker)
}

fn executable(root: &Path, name: &str, content: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = root.join(name);
    fs::write(&path, content).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn config_mounts_default_the_target_after_compose_normalization() {
    let root = test_dir("config-target");
    for config in ["{source: settings}", "settings"] {
        fs::write(root.join("compose.yaml"), format!("services: {{app: {{image: alpine, configs: [{config}]}}}}\nconfigs: {{settings: {{content: value}}}}\n")).unwrap();
        let project = load_project(&LoadOptions {
            working_dir: Some(root.clone()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            project
                .services
                .get("app")
                .unwrap()
                .config_mounts()
                .first()
                .unwrap()
                .target
                .as_ref()
                .unwrap()
                .as_str(),
            "/settings"
        );
    }
    fs::remove_dir_all(root).unwrap();
}
