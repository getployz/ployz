use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use ployz::compose::{ComposeError, LoadOptions, load_project};
use ployz_core::{IngressProxyFragment, VolumeSource};

#[test]
fn compose_failures_probe_the_plugin_once_and_preserve_project_diagnostics() {
    let root = test_dir("failures");
    let calls = root.join("calls");
    let docker = executable(
        &root,
        "docker",
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
if [ "$*" = "compose version" ]; then
  exit 0
fi
printf 'compose diagnostic\n' >&2
exit 1
"#,
            calls.display()
        ),
    );
    let options = LoadOptions {
        command: "deploy".into(),
        docker: Some(docker.clone()),
        working_dir: Some(root.clone()),
        ..Default::default()
    };

    let error = load_project(&options).unwrap_err();
    assert_eq!(error, ComposeError::Compose("compose diagnostic\n".into()));
    assert_eq!(
        fs::read_to_string(&calls)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "compose --all-resources config --no-consistency --no-normalize --no-path-resolution --format yaml",
            "compose version"
        ]
    );

    fs::remove_file(&calls).unwrap();
    let failing_docker = executable(
        &root,
        "docker-without-compose",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$*\" = \"compose version\" ]; then exit 1; fi\nprintf 'compose diagnostic\\n' >&2\nexit 1\n",
            calls.display()
        ),
    );
    let error = load_project(&LoadOptions {
        docker: Some(failing_docker),
        ..options.clone()
    })
    .unwrap_err();
    assert!(matches!(error, ComposeError::Prerequisite(message) if
        message.contains("ployz deploy")
            && message.contains("Docker Compose plugin")
            && message.contains("https://docs.docker.com/compose/install/")));
    assert_eq!(
        fs::read_to_string(&calls)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "compose --all-resources config --no-consistency --no-normalize --no-path-resolution --format yaml",
            "compose version"
        ]
    );

    let missing = LoadOptions {
        docker: Some(root.join("missing-docker")),
        ..options
    };
    assert!(
        matches!(load_project(&missing), Err(ComposeError::Prerequisite(message)) if
        message.contains("Docker CLI") && message.contains("ployz deploy"))
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn only_compose_commands_load_a_project() {
    let root = test_dir("scope");
    let calls = root.join("calls");
    let compose = root.join("compose.yaml");
    fs::write(&compose, "services: {api: {image: api}}\n").unwrap();
    let alternate = root.join("alternate.yaml");
    fs::write(&alternate, "services: {api: {image: alternate}}\n").unwrap();
    executable(
        &root,
        "docker",
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$PLOYZ_DOCKER_CALLS"
case "$*" in
  *"config --environment") exit 0 ;;
  *"config --no-consistency --no-normalize --no-path-resolution --format yaml")
    printf 'name: demo\nservices:\n  api:\n    image: api\n'
    exit 0
    ;;
  *"config --format yaml")
    printf 'name: demo\nservices:\n  api:\n    image: api\n'
    exit 0
    ;;
  "compose version") exit 0 ;;
esac
exit 1
"#,
    );
    let path = format!("{}:/usr/bin:/bin", root.display());
    for args in [
        vec!["deploy", "--file", compose.to_str().unwrap()],
        vec!["build", "--file", compose.to_str().unwrap()],
        vec!["logs", "--file", compose.to_str().unwrap()],
        vec!["logs", "api", "--file", compose.to_str().unwrap()],
        vec!["version"],
    ] {
        let _ = Command::new(env!("CARGO_BIN_EXE_ployz"))
            .args(args)
            .env("PATH", &path)
            .env("PLOYZ_DOCKER_CALLS", &calls)
            .output()
            .unwrap();
    }
    let _ = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("deploy")
        .current_dir(&root)
        .env("PATH", &path)
        .env("PLOYZ_DOCKER_CALLS", &calls)
        .env("COMPOSE_FILE", &alternate)
        .output()
        .unwrap();
    let calls = fs::read_to_string(calls).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|line| {
                line.ends_with("config --format yaml") && !line.contains("--no-normalize")
            })
            .count(),
        4
    );
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.ends_with("config --environment"))
            .count(),
        4
    );
    assert_eq!(
        calls
            .lines()
            .filter(|line| {
                line.contains("--profile * config --format yaml")
                    && !line.contains("--no-normalize")
            })
            .count(),
        3,
        "deploy and bare logs must request disabled-profile services"
    );
    assert!(
        calls
            .lines()
            .filter(|line| {
                line.ends_with("config --format yaml") && !line.contains("--no-normalize")
            })
            .all(|line| !line.contains("alternate.yaml")),
        "COMPOSE_FILE selection is delegated to the plugin"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn real_compose_normalizes_without_a_daemon_and_project_inputs_stay_relative() {
    if !Path::new("/usr/bin/docker").is_file() {
        eprintln!("skipping: /usr/bin/docker is unavailable");
        return;
    }
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
    let docker = executable(
        &root,
        "real-docker",
        "#!/bin/sh\nexport DOCKER_HOST=tcp://127.0.0.1:1\nexec /usr/bin/docker \"$@\"\n",
    );
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
            .and_then(IngressProxyFragment::as_caddy),
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
    let Some((root, docker)) = real_docker_fixture("provisioned-volume") else {
        return;
    };
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
            .source,
        VolumeSource::Provisioned {
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
    let Some((root, docker)) = real_docker_fixture("provisioned-volume-consistency") else {
        return;
    };
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

    assert!(matches!(error, ComposeError::Compose(_)));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn real_compose_still_rejects_undeclared_volume_mounts() {
    let Some((root, docker)) = real_docker_fixture("undeclared-volume") else {
        return;
    };
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

    assert!(matches!(error, ComposeError::Compose(_)));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn real_compose_recovers_command_secrets_selected_by_compose_file() {
    if !Path::new("/usr/bin/docker").is_file() {
        eprintln!("skipping: /usr/bin/docker is unavailable");
        return;
    }
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
    executable(
        &root,
        "docker",
        "#!/bin/sh\nexport DOCKER_HOST=tcp://127.0.0.1:1\nexec /usr/bin/docker \"$@\"\n",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
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
    if !Path::new("/usr/bin/docker").is_file() {
        eprintln!("skipping: /usr/bin/docker is unavailable");
        return;
    }
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
    let docker = executable(
        &root,
        "real-docker",
        "#!/bin/sh\nexport DOCKER_HOST=tcp://127.0.0.1:1\nexec /usr/bin/docker \"$@\"\n",
    );
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
    let VolumeSource::Named { labels, .. } = &service.volumes().first().unwrap().source else {
        panic!("expected a named volume");
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

fn real_docker_fixture(label: &str) -> Option<(PathBuf, PathBuf)> {
    if !Path::new("/usr/bin/docker").is_file() {
        eprintln!("skipping: /usr/bin/docker is unavailable");
        return None;
    }
    let root = test_dir(label);
    let docker = executable(
        &root,
        "real-docker",
        "#!/bin/sh\nexport DOCKER_HOST=tcp://127.0.0.1:1\nexec /usr/bin/docker \"$@\"\n",
    );
    Some((root, docker))
}

fn executable(root: &Path, name: &str, content: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = root.join(name);
    fs::write(&path, content).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}
