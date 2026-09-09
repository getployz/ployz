//! Portable source and private-material checks through capture/execute.
use super::*;

#[test]
fn capture_rejects_links_outside_the_captured_source() {
    if !isolated_build_test() {
        return;
    }
    let root = std::env::temp_dir().join(format!("ployz-build-links-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/Dockerfile"), "FROM scratch\nCOPY . /app\n").unwrap();
    fs::write(root.join("host-only"), "must never be staged").unwrap();
    let options = BuildOptions {
        output: Output::Validate,
        ..Default::default()
    };
    for target in [
        root.join("host-only"),
        "../host-only".into(),
        "../../private/secret-0".into(),
    ] {
        let link = root.join("src/link");
        std::os::unix::fs::symlink(target, &link).unwrap();
        let mut project = parse_normalized("services: {api: {build: ./src}}", &root).unwrap();
        let plan = plan_build(&project, &options).unwrap();
        let error = match capture_build(&plan, &options, &mut project) {
            Ok(_) => panic!("a link escaping captured source was admitted"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("symlink") && error.contains("captured"),
            "{error}"
        );
        fs::remove_file(link).unwrap();
    }
    // Docker retains contained links even when their targets are excluded.
    fs::write(root.join("src/ignored-file"), "excluded bytes").unwrap();
    fs::write(root.join("src/.dockerignore"), "ignored-file\n").unwrap();
    std::os::unix::fs::symlink("ignored-file", root.join("src/dangling")).unwrap();
    // A safe source link still works after capture and execution.
    fs::write(root.join("src/value"), "included").unwrap();
    std::os::unix::fs::symlink("value", root.join("src/link")).unwrap();
    let docker = root.join("docker");
    write_docker(&docker, &root);
    let mut project = parse_normalized("services: {api: {build: ./src}}", &root).unwrap();
    let plan = plan_build(&project, &options).unwrap();
    let build = capture_build(&plan, &options, &mut project).unwrap();
    fs::remove_dir_all(root.join("src")).unwrap();
    build.execute(Some(&docker)).unwrap();
    let config: serde_norway::Value =
        serde_norway::from_str(&fs::read_to_string(root.join("override.yaml")).unwrap()).unwrap();
    let context = config
        .get("services")
        .unwrap()
        .get("api")
        .unwrap()
        .get("build")
        .unwrap()
        .get("context")
        .unwrap()
        .as_str()
        .unwrap();
    assert_eq!(
        fs::read_to_string(root.join("relocated").join(context).join("link")).unwrap(),
        "included"
    );
    let dangling = root.join("relocated").join(context).join("dangling");
    assert_eq!(fs::read_link(&dangling).unwrap(), Path::new("ignored-file"));
    assert!(!dangling.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn remote_contexts_require_valid_immutable_references() {
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let digest = format!("sha256:{}", "a".repeat(64));
    for (source, accepted) in [
        (format!("https://example.test/repo.git#{commit}:src"), true),
        (format!("ssh://git@example.test/repo.git#{commit}"), true),
        (format!("docker-image://example.test/base@{digest}"), true),
        ("docker-image://base@sha256:short".into(), false),
        ("docker-image://base:latest".into(), false),
        (format!("file:///tmp/repo#{commit}"), false),
        (
            format!("https://example.test/source.tar.gz#{commit}"),
            false,
        ),
        (
            format!("https://user:private@example.test/repo.git#{commit}"),
            false,
        ),
        (
            format!("https://example.test/repo.git#{commit}:../outside"),
            false,
        ),
    ] {
        let mut project = parse_normalized(
            &format!("services: {{api: {{build: {{context: '{source}'}}}}}}"),
            ".",
        )
        .unwrap();
        let options = BuildOptions::default();
        let plan = plan_build(&project, &options).unwrap();
        assert_eq!(
            capture_build(&plan, &options, &mut project).is_ok(),
            accepted,
            "{source}"
        );
    }
}

#[test]
#[expect(clippy::indexing_slicing, reason = "Fixed test fixture")]
fn resolved_file_credentials_stay_private_across_captures() {
    if !isolated_build_test() {
        return;
    }
    let root = std::env::temp_dir().join(format!("ployz-build-private-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/Dockerfile"), "FROM scratch\n").unwrap();
    fs::write(root.join("src/token"), "private-file-token").unwrap();
    fs::write(root.join("src/.dockerignore"), "token\n").unwrap();
    let mut project = parse_normalized("services: {api: {build: {context: ./src, secrets: [token]}, environment: {TOKEN: 'secret://token'}}}\nsecrets: {token: {file: ./src/token}}", &root).unwrap();
    project.resolve_secrets().unwrap();
    let options = BuildOptions {
        output: Output::Validate,
        ..Default::default()
    };
    let plan = plan_build(&project, &options).unwrap();
    let docker = root.join("docker");
    write_docker(&docker, &root);
    let build = capture_build(&plan, &options, &mut project).unwrap();
    build.execute(Some(&docker)).unwrap();
    let config: serde_norway::Value =
        serde_norway::from_str(&fs::read_to_string(root.join("override.yaml")).unwrap()).unwrap();
    let staged = root.join("relocated");
    let context = staged.join(
        config["services"]["api"]["build"]["context"]
            .as_str()
            .unwrap(),
    );
    assert!(
        !context.join("token").exists(),
        "resolved credential entered reusable source"
    );
    let private = staged.join(config["secrets"]["token"]["file"].as_str().unwrap());
    assert_eq!(fs::read_to_string(&private).unwrap(), "private-file-token");
    assert_eq!(
        fs::metadata(&private).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::write(root.join("src/token"), "later-provider-value").unwrap();
    let second = capture_build(&plan, &options, &mut project).unwrap();
    second.execute(Some(&docker)).unwrap();
    assert_eq!(
        project.services["api"].container.environment["TOKEN"],
        "private-file-token"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn builds_use_captured_explicit_registry_credentials_and_proxies() {
    if !isolated_build_test() {
        return;
    }
    let root = std::env::temp_dir().join(format!("ployz-build-auth-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("auth")).unwrap();
    fs::create_dir(root.join("src")).unwrap();
    fs::write(
        root.join("compose.yaml"),
        "services: {api: {build: ./src}}\n",
    )
    .unwrap();
    fs::write(root.join("src/Dockerfile"), "FROM scratch\n").unwrap();
    fs::write(
        root.join(".env"),
        format!("DOCKER_CONFIG={}\n", root.join("auth").display()),
    )
    .unwrap();
    let credentials = r#"{"auths":{"example.test":{"auth":"cHJpdmF0ZTp0b2tlbg=="}},"proxies":{"default":{"httpProxy":"http://proxy.test:3128","noProxy":"localhost"},"ssh://builder@host":{"httpsProxy":"http://private:token@proxy.test:3128"}}}"#;
    fs::write(root.join("auth/config.json"), credentials).unwrap();
    let mut project = load_project(&LoadOptions {
        working_dir: Some(root.clone()),
        ..Default::default()
    })
    .unwrap();
    let options = BuildOptions {
        output: Output::Validate,
        ..Default::default()
    };
    let plan = plan_build(&project, &options).unwrap();
    let build = capture_build(&plan, &options, &mut project).unwrap();
    fs::write(root.join("auth/config.json"), "changed after capture").unwrap();
    let docker = root.join("docker");
    write_docker(&docker, &root);
    build.execute(Some(&docker)).unwrap();
    let captured: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join("docker-config.json")).unwrap())
            .unwrap();
    let expected: serde_json::Value = serde_json::from_str(credentials).unwrap();
    for key in ["auths", "proxies"] {
        assert_eq!(captured.get(key), expected.get(key));
    }
    let environment = fs::read_to_string(root.join("docker-environment")).unwrap();
    assert!(!environment.contains(root.join("auth").to_str().unwrap()));
    assert!(
        environment.lines().nth(2).unwrap_or("").is_empty(),
        "an ambient SSH agent was supplied"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[expect(clippy::indexing_slicing, reason = "Fixed capture fixture")]
fn capture_keeps_recipe_exclusions_negations_and_inline_contexts_separate() {
    if !isolated_build_test() {
        return;
    }
    let root = std::env::temp_dir().join(format!("ployz-build-rules-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src/cache")).unwrap();
    fs::write(root.join("src/cache/keep"), [0, 255, 128, 1]).unwrap();
    fs::set_permissions(
        root.join("src/cache/keep"),
        fs::Permissions::from_mode(0o751),
    )
    .unwrap();
    fs::write(root.join("src/cache/drop"), "excluded before capture").unwrap();
    fs::write(root.join("src/.dockerignore"), "cache\n").unwrap();
    for recipe in ["a.Dockerfile", "b.Dockerfile"] {
        fs::write(root.join("src").join(recipe), "FROM scratch\n").unwrap();
    }
    fs::write(
        root.join("src/a.Dockerfile.dockerignore"),
        "cache\n!**/keep\n",
    )
    .unwrap();
    fs::write(root.join("src/b.Dockerfile.dockerignore"), "").unwrap();
    let mut project = parse_normalized(
        r#"
services:
  a: {build: {context: ./src, dockerfile: a.Dockerfile}}
  b:
    build: {context: ./src, dockerfile: b.Dockerfile}
    environment: {TRIGGER: 'secret://churn'}
  c: {build: {context: ./src, dockerfile_inline: 'FROM scratch'}}
secrets:
  churn: {x-command: "sh -c 'printf changed > src/cache/drop; printf token'"}
"#,
        &root,
    )
    .unwrap();
    let options = BuildOptions {
        output: Output::Validate,
        ..Default::default()
    };
    let plan = plan_build(&project, &options).unwrap();
    let build = capture_build(&plan, &options, &mut project).unwrap();
    fs::remove_dir_all(root.join("src")).unwrap();
    let docker = root.join("docker");
    write_docker(&docker, &root);
    build.execute(Some(&docker)).unwrap();
    let config: serde_norway::Value =
        serde_norway::from_str(&fs::read_to_string(root.join("override.yaml")).unwrap()).unwrap();
    let context = |service: &str| {
        root.join("relocated").join(
            config["services"][service]["build"]["context"]
                .as_str()
                .unwrap(),
        )
    };
    assert_ne!(context("a"), context("b"));
    assert_eq!(
        fs::read(context("a").join("cache/keep")).unwrap(),
        [0, 255, 128, 1]
    );
    assert_eq!(
        fs::metadata(context("a").join("cache/keep"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o751
    );
    assert!(!context("a").join("cache/drop").exists());
    assert_eq!(
        fs::read_to_string(context("b").join("cache/drop")).unwrap(),
        "changed"
    );
    assert!(!context("c").join("cache").exists());
    assert_eq!(
        config["services"]["c"]["build"]["dockerfile_inline"].as_str(),
        Some("FROM scratch")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn provider_failure_diagnostics_do_not_print_private_output() {
    let mut project = parse_normalized(
        r#"
services:
  api:
    build: {context: .}
    environment: {TOKEN: 'secret://token'}
secrets:
  token: {x-command: "sh -c 'printf private-output >&2; exit 1'"}
"#,
        ".",
    )
    .unwrap();
    let options = BuildOptions::default();
    let plan = plan_build(&project, &options).unwrap();
    let error = match capture_build(&plan, &options, &mut project) {
        Ok(_) => panic!("failing provider was admitted"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("token"));
    assert!(!error.contains("private-output"), "{error}");
}

#[test]
#[expect(clippy::indexing_slicing, reason = "Fixed capture fixture")]
fn authored_configuration_follows_dockerignore() {
    if !isolated_build_test() {
        return;
    }
    let root = std::env::temp_dir().join(format!("ployz-build-envfiles-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("compose.yaml"), "services: {api: {build: {context: ., secrets: [token]}, env_file: ./runtime.env, environment: {TOKEN: 'secret://token', INLINE: private-inline-value}}}\nsecrets: {token: {environment: PLOYZ_CAPTURE_TOKEN}}\n").unwrap();
    fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
    fs::write(root.join(".dockerignore"), ".env\nruntime.env\n").unwrap();
    fs::write(
        root.join(".env"),
        "PLOYZ_CAPTURE_TOKEN=private-provider-value\n",
    )
    .unwrap();
    fs::write(root.join("runtime.env"), "RUNTIME=private-runtime-value\n").unwrap();
    fs::write(
        root.join("override.yaml"),
        "services: {api: {build: {args: {OVERRIDE: private-override-value}}}}\n",
    )
    .unwrap();
    let mut project = load_project(&LoadOptions {
        working_dir: Some(root.clone()),
        files: vec!["compose.yaml".into(), "override.yaml".into()],
        ..Default::default()
    })
    .unwrap();
    let options = BuildOptions {
        output: Output::Validate,
        ..Default::default()
    };
    let plan = plan_build(&project, &options).unwrap();
    let build = capture_build(&plan, &options, &mut project).unwrap();
    let docker = root.join("docker");
    write_docker(&docker, &root);
    build.execute(Some(&docker)).unwrap();
    let config: serde_norway::Value =
        serde_norway::from_str(&fs::read_to_string(root.join("override.yaml")).unwrap()).unwrap();
    let context = root.join("relocated").join(
        config["services"]["api"]["build"]["context"]
            .as_str()
            .unwrap(),
    );
    assert!(
        context.join("compose.yaml").exists(),
        "authored Compose source was filtered without a Docker exclusion"
    );
    assert!(
        context.join("override.yaml").exists(),
        "authored override source was filtered without a Docker exclusion"
    );
    assert!(
        !context.join(".env").exists(),
        "environment provider file entered source"
    );
    assert!(
        !context.join("runtime.env").exists(),
        "runtime environment file entered source"
    );
    assert_eq!(
        config["services"]["api"]["build"]["args"]["TOKEN"].as_str(),
        Some("private-provider-value")
    );
    assert_eq!(
        config["services"]["api"]["build"]["args"]["RUNTIME"].as_str(),
        Some("private-runtime-value")
    );
    assert_eq!(
        config["services"]["api"]["build"]["args"]["INLINE"].as_str(),
        Some("private-inline-value")
    );
    assert_eq!(
        config["services"]["api"]["build"]["args"]["OVERRIDE"].as_str(),
        Some("private-override-value")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn included_source_edits_during_capture_are_refused() {
    let root = std::env::temp_dir().join(format!("ployz-build-unstable-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/value"), "before").unwrap();
    let mut project = parse_normalized(
        r#"
services:
  a: {build: {context: ./src, dockerfile_inline: 'FROM scratch'}}
  b:
    build: {context: ./src, dockerfile_inline: 'FROM scratch'}
    environment: {TRIGGER: 'secret://churn'}
secrets:
  churn: {x-command: "sh -c 'printf after > src/value; printf token'"}
"#,
        &root,
    )
    .unwrap();
    let options = BuildOptions::default();
    let plan = plan_build(&project, &options).unwrap();
    let error = match capture_build(&plan, &options, &mut project) {
        Ok(_) => panic!("unstable included source was admitted"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("build inputs changed during capture"),
        "{error}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[expect(clippy::indexing_slicing, reason = "Fixed capture fixture")]
fn default_platform_is_captured_and_verified_unless_compose_overrides_it() {
    if !isolated_build_test() {
        return;
    }
    let root = std::env::temp_dir().join(format!("ployz-build-platform-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/Dockerfile"), "FROM scratch\n").unwrap();
    let docker = root.join("docker");
    write_docker(&docker, &root);
    fs::write(root.join("digest"), FIRST_CONTENT).unwrap();
    fs::write(root.join("image"), "example.test/api:latest").unwrap();
    for declared in ["", ", platforms: [linux/amd64]"] {
        fs::write(root.join(".env"), "DOCKER_DEFAULT_PLATFORM=linux/arm64\n").unwrap();
        fs::write(root.join("compose.yaml"), format!("services:\n  api:\n    image: example.test/api:latest\n    build: {{context: ./src{declared}}}\n")).unwrap();
        let mut project = load_project(&LoadOptions {
            working_dir: Some(root.clone()),
            ..Default::default()
        })
        .unwrap();
        let options = BuildOptions::default();
        let plan = plan_build(&project, &options).unwrap();
        let build = capture_build(&plan, &options, &mut project).unwrap();
        fs::write(root.join(".env"), "DOCKER_DEFAULT_PLATFORM=linux/amd64\n").unwrap();
        // The recording executor reports AMD64: an ARM64 request must reject it.
        let result = build.execute(Some(&docker));
        let expected = if declared.is_empty() {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("not the requested linux/arm64")
            );
            "linux/arm64"
        } else {
            assert_eq!(one_built(result.unwrap()).built.platform, "linux/amd64");
            "linux/amd64"
        };
        let config: serde_norway::Value =
            serde_norway::from_str(&fs::read_to_string(root.join("override.yaml")).unwrap())
                .unwrap();
        assert_eq!(
            config["services"]["api"]["build"]["platforms"][0].as_str(),
            Some(expected)
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ssh_docker_hosts_are_refused_but_git_contexts_keep_the_captured_agent_socket() {
    if !isolated_build_test() {
        return;
    }
    let root = std::env::temp_dir().join(format!("ployz-build-agent-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/Dockerfile"), "FROM scratch\n").unwrap();
    let docker = root.join("docker");
    write_docker(&docker, &root);
    let options = BuildOptions {
        output: Output::Validate,
        ..Default::default()
    };
    // Process environment takes precedence over the project's .env file.
    let socket =
        std::env::var("SSH_AUTH_SOCK").unwrap_or_else(|_| "/tmp/captured-agent.sock".into());
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let remote = format!("ssh://git@example.test/repo.git#{commit}");
    let scp = format!("git@example.test:repo.git#{commit}");
    for (host, context, additional, expected) in [
        (
            "ssh://builder@host",
            "./src",
            String::new(),
            socket.as_str(),
        ),
        ("unix:///var/run/docker.sock", "./src", String::new(), ""),
        (
            "unix:///var/run/docker.sock",
            remote.as_str(),
            String::new(),
            socket.as_str(),
        ),
        (
            "unix:///var/run/docker.sock",
            scp.as_str(),
            String::new(),
            socket.as_str(),
        ),
        (
            "unix:///var/run/docker.sock",
            "./src",
            format!(", additional_contexts: {{repo: '{remote}'}}"),
            socket.as_str(),
        ),
    ] {
        fs::write(
            root.join("compose.yaml"),
            format!("services:\n  api:\n    build: {{context: '{context}'{additional}}}\n"),
        )
        .unwrap();
        fs::write(
            root.join(".env"),
            format!("DOCKER_HOST={host}\nSSH_AUTH_SOCK=/tmp/captured-agent.sock\n"),
        )
        .unwrap();
        let mut project = load_project(&LoadOptions {
            working_dir: Some(root.clone()),
            ..Default::default()
        })
        .unwrap();
        let plan = plan_build(&project, &options).unwrap();
        let build = capture_build(&plan, &options, &mut project).unwrap();
        fs::write(root.join(".env"), "SSH_AUTH_SOCK=/tmp/changed-agent.sock\n").unwrap();
        let result = build.execute(Some(&docker));
        if host.starts_with("ssh://") {
            assert!(result.unwrap_err().to_string().contains("local Docker"));
            assert!(!root.join("docker-environment").exists());
            continue;
        }
        result.unwrap();
        let environment = fs::read_to_string(root.join("docker-environment")).unwrap();
        assert_eq!(environment.lines().nth(2), Some(expected), "{host}");
    }
    fs::remove_dir_all(root).unwrap();
}
