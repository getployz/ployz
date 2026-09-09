use std::{
    fs,
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use ployz::compose::{
    BuildOptions, BuiltService, LoadOptions, execute_build, load_project, plan_build,
};

#[test]
#[ignore = "informing: requires Docker, Compose, and registry:2"]
fn compose_build_basic_pushes_only_buildable_resolved_images() {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let name = format!("ployz-l3-004-{}", std::process::id());
    command([
        "run",
        "--detach",
        "--name",
        &name,
        "--publish",
        &format!("127.0.0.1:{port}:5000"),
        "registry:2",
    ]);
    let mut cleanup = Cleanup {
        name,
        images: Vec::new(),
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "registry did not become ready");
        std::thread::sleep(Duration::from_millis(50));
    }

    let root = std::env::temp_dir().join(format!("ployz-l3-004-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    // Interpolation happens while loading, so the fixture is copied with this
    // run's registry port already resolved.
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/compose-build-basic");
    let project_directory = root.join("project");
    for service in ["service-first-dir", "service-second-dir"] {
        fs::create_dir_all(project_directory.join(service)).unwrap();
    }
    fs::write(
        project_directory.join("compose.yaml"),
        fs::read_to_string(fixture.join("compose.yaml"))
            .unwrap()
            .replace("${TEST_REGISTRY_PORT}", &port.to_string()),
    )
    .unwrap();
    for recipe in [
        "service-first-dir/Dockerfile",
        "service-second-dir/Dockerfile.alt",
    ] {
        fs::copy(fixture.join(recipe), project_directory.join(recipe)).unwrap();
    }
    let load = LoadOptions {
        command: "build".into(),
        working_dir: Some(project_directory),
        ..Default::default()
    };
    let mut project = load_project(&load).unwrap();
    let options = BuildOptions {
        output: ployz_build::Output::Registry,
        ..Default::default()
    };
    let plan = plan_build(&project, &options).unwrap();
    assert_eq!(
        plan.iter()
            .map(|service| service.name.as_str())
            .collect::<Vec<_>>(),
        ["service-first", "service-second"]
    );
    assert_eq!(
        plan.get(1).unwrap().image,
        format!("127.0.0.1:{port}/service-second:version2")
    );
    assert!(
        plan.first()
            .unwrap()
            .image
            .starts_with(&format!("127.0.0.1:{port}/service-first:"))
    );
    cleanup.images = plan.iter().map(|service| service.image.clone()).collect();

    execute_build(&plan, &options, &load, &mut project).unwrap();
    for image in &cleanup.images {
        // Publication retains no local copy, so the registry is the only source.
        let _ = Command::new("docker").args(["image", "rm", image]).status();
        command(["pull", image]);
    }
    assert!(
        Command::new("docker")
            .args([
                "image",
                "inspect",
                &format!("127.0.0.1:{port}/service-no-build")
            ])
            .status()
            .is_ok_and(|status| !status.success())
    );
    fs::remove_dir_all(root).unwrap();
    drop(cleanup);
}

fn command<const N: usize>(args: [&str; N]) {
    let status = Command::new("docker").args(args).status().unwrap();
    assert!(status.success(), "Docker command failed with {status}");
}

struct Cleanup {
    name: String,
    images: Vec<String>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "--force", "--volumes", &self.name])
            .status();
        for image in &self.images {
            let _ = Command::new("docker").args(["image", "rm", image]).status();
        }
    }
}

#[test]
#[ignore = "informing: requires Docker with the containerd image store"]
fn local_dockerfile_build_loads_a_runnable_image_and_reuses_its_retained_cache() {
    let root = std::env::temp_dir().join(format!("ployz-l3-799-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let image = format!("ployz-local-build-{}:check", std::process::id());
    fs::write(
        root.join("compose.yaml"),
        format!("services:\n  app:\n    image: {image}\n    build: .\n"),
    )
    .unwrap();
    // The stamp differs on every uncached execution, so identical stamps prove
    // the retained cache was reused rather than the layer rebuilt.
    fs::write(
        root.join("Dockerfile"),
        "FROM busybox:1.37.0\nRUN date +%s%N > /built-at\n",
    )
    .unwrap();
    let cleanup = LocalBuild {
        images: vec![image.clone()],
    };
    let load = LoadOptions {
        command: "build".into(),
        working_dir: Some(root.clone()),
        ..Default::default()
    };
    let mut project = load_project(&load).unwrap();
    let options = BuildOptions::default();
    let plan = plan_build(&project, &options).unwrap();

    let first = one_built(execute_build(&plan, &options, &load, &mut project).unwrap());
    assert_eq!(first.image, image);
    assert!(first.built.tags.iter().any(|tag| tag.ends_with(&image)));
    assert_eq!(first.built.platform, host_platform());
    assert!(first.built.reference.contains("@sha256:"));
    // The image is in the local store under exactly the content just built.
    command(["image", "inspect", &first.built.reference]);
    let stamp = output(["run", "--rm", &first.built.reference, "cat", "/built-at"]);
    assert!(!stamp.trim().is_empty());

    // The builder container is gone; its dedicated cache volume is retained.
    assert!(
        !Command::new("docker")
            .args(["buildx", "inspect", ployz_build::builder_name().as_str()])
            .status()
            .unwrap()
            .success()
    );
    command(["volume", "inspect", &cache_volume()]);

    // A second Build recreates the builder and reuses that cache.
    let rebuilt = one_built(execute_build(&plan, &options, &load, &mut project).unwrap());
    assert_eq!(
        output(["run", "--rm", &rebuilt.built.reference, "cat", "/built-at"]),
        stamp,
        "the recreated builder rebuilt the layer instead of reusing its cache"
    );

    fs::remove_dir_all(root).unwrap();
    drop(cleanup);
}

fn one_built(mut built: Vec<BuiltService>) -> BuiltService {
    assert_eq!(built.len(), 1, "expected one built Service, got {built:?}");
    built.remove(0)
}

fn host_platform() -> String {
    output(["version", "--format", "{{.Server.Os}}/{{.Server.Arch}}"])
        .trim()
        .to_owned()
}

fn cache_volume() -> String {
    format!("buildx_buildkit_{}0_state", ployz_build::builder_name())
}

fn output<const N: usize>(args: [&str; N]) -> String {
    let output = Command::new("docker").args(args).output().unwrap();
    assert!(
        output.status.success(),
        "docker {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// Leaves no image, builder, or retained cache behind for the next run.
struct LocalBuild {
    images: Vec<String>,
}

impl Drop for LocalBuild {
    fn drop(&mut self) {
        for image in &self.images {
            let _ = Command::new("docker").args(["image", "rm", image]).status();
        }
        let _ = Command::new("docker")
            .args(["buildx", "rm", ployz_build::builder_name().as_str()])
            .status();
        let _ = Command::new("docker")
            .args(["volume", "rm", &cache_volume()])
            .status();
    }
}

#[test]
#[ignore = "informing: requires Docker with the containerd image store"]
fn captured_variables_and_secret_mounts_are_consumed_by_dockerfile() {
    let root = std::env::temp_dir().join(format!("ployz-l3-800-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir(root.join("shared")).unwrap();
    let image = format!("ployz-private-build-{}:check", std::process::id());
    let cleanup = LocalBuild {
        images: vec![image.clone()],
    };
    fs::write(
        root.join("compose.yaml"),
        format!(
            r#"
services:
  app:
    image: {image}
    environment:
      MODE: runtime
      DEFAULT: service
      TOKEN: secret://token
      LITERAL: '$$LATER'
    build:
      context: ./src
      dockerfile: ../Dockerfile
      args: {{MODE: compose}}
      secrets: [token]
      additional_contexts: {{shared: ./shared}}
secrets:
  token: {{file: ./src/token}}
"#
        ),
    )
    .unwrap();
    fs::write(root.join("src/token"), "private-$VALUE").unwrap();
    fs::write(root.join("src/included"), "captured source").unwrap();
    fs::write(root.join("src/ignored"), "excluded source").unwrap();
    fs::write(root.join("src/.dockerignore"), "ignored\ntoken\n").unwrap();
    fs::write(root.join("shared/data"), "named context").unwrap();
    fs::write(root.join("Dockerfile"), r#"FROM busybox:1.37.0
ARG MODE
ARG DEFAULT
ARG TOKEN
ARG LITERAL
COPY . /source
COPY --from=shared /data /named
RUN --mount=type=secret,id=token test ! -e /source/token && test ! -e /source/ignored && printf '%s\n' "$MODE" "$DEFAULT" "$TOKEN" "$LITERAL" "$(cat /run/secrets/token)" > /values
"#).unwrap();
    let load = LoadOptions {
        command: "build".into(),
        working_dir: Some(root.clone()),
        ..Default::default()
    };
    let mut project = load_project(&load).unwrap();
    let options = BuildOptions {
        build_args: vec!["MODE=cli".into()],
        ..Default::default()
    };
    let plan = plan_build(&project, &options).unwrap();
    let captured = ployz::compose::capture_build(&plan, &options, &mut project).unwrap();
    project.resolve_secrets().unwrap();
    assert_eq!(
        project
            .services
            .get("app")
            .unwrap()
            .container
            .environment
            .get("MODE")
            .unwrap(),
        "runtime"
    );
    fs::remove_dir_all(&root).unwrap();
    let built = one_built(captured.execute(None).unwrap());
    assert_eq!(
        output(["run", "--rm", &built.built.reference, "cat", "/values"]),
        "cli\nservice\nprivate-$VALUE\n$LATER\nprivate-$VALUE\n"
    );
    assert_eq!(
        output([
            "run",
            "--rm",
            &built.built.reference,
            "cat",
            "/source/included"
        ]),
        "captured source"
    );
    assert_eq!(
        output(["run", "--rm", &built.built.reference, "cat", "/named"]),
        "named context"
    );
    // Mount confidentiality is separate from ordinary arguments: this recipe
    // deliberately persisted argument values, but the mount itself is gone.
    command([
        "run",
        "--rm",
        &built.built.reference,
        "test",
        "!",
        "-e",
        "/run/secrets/token",
    ]);
    drop(cleanup);
}

#[path = "build_layer3/remote.rs"]
mod remote;
#[tokio::test]
#[ignore = "informing: requires Docker with the containerd image store and the privileged Ployz testkit"]
async fn railpack_build_and_deploy_preserve_variables_cache_and_failure_boundaries() {
    let root = std::env::temp_dir().join(format!("ployz-l3-801-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let image = format!("example.test/ployz-railpack-{}:check", std::process::id());
    let _cleanup = LocalBuild {
        images: vec![image.clone()],
    };
    fs::write(root.join("compose.yaml"), format!("services:\n  app:\n    image: {image}\n    environment: {{MESSAGE: runtime}}\n    build: .\n")).unwrap();
    fs::write(root.join("package.json"), r#"{"name":"railpack-check","version":"1.0.0","engines":{"node":"22.14.0"},"scripts":{"build":"node build.js","start":"node index.js"}}"#).unwrap();
    fs::write(root.join("build.js"), "require('fs').writeFileSync('built.json', JSON.stringify({message:process.env.MESSAGE,stamp:Date.now()}));").unwrap();
    fs::write(
        root.join("index.js"),
        "const message = require('./built.json').message; if (process.env.ONESHOT) console.log(message); else require('http').createServer((req,res) => res.end(message)).listen(3000, '0.0.0.0');",
    )
    .unwrap();
    let load = LoadOptions {
        command: "build".into(),
        working_dir: Some(root.clone()),
        ..Default::default()
    };
    let cluster = ployz_testkit::Cluster::create(
        ployz_testkit::ClusterPlan::new(&format!("l3-railpack-{}", std::process::id()), 1).unwrap(),
    )
    .unwrap();
    cluster.wait_ready(Duration::from_secs(120)).await.unwrap();
    cluster.initialize_first().await.unwrap();
    // Direct Image Transfer needs the normal SSH transport, not the testkit's
    // read-only TCP forwarding. Scope its test key and known-hosts file outside source.
    use std::os::unix::fs::PermissionsExt as _;
    let ssh = root.with_extension("ssh");
    fs::create_dir_all(&ssh).unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700)).unwrap();
    let key = ssh.join("key");
    fs::write(
        &key,
        cluster
            .shell(0, "cat /root/.ssh/id_ed25519")
            .unwrap()
            .stdout,
    )
    .unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    let wrapper = ssh.join("ssh");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexec /usr/bin/ssh -i '{}' -o UserKnownHostsFile='{}' \"$@\"\n",
            key.display(),
            ssh.join("known_hosts").display()
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(ssh.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let destination = format!("root@{}", cluster.endpoint(0).unwrap().0.ip());
    let mut previous = None;
    for (value, no_cache) in [
        ("first", false),
        ("first", false),
        ("changed", false),
        ("changed", true),
    ] {
        let mut project = load_project(&load).unwrap();
        let options = BuildOptions {
            build_args: vec![format!("MESSAGE={value}")],
            no_cache,
            ..Default::default()
        };
        let plan = plan_build(&project, &options).unwrap();
        let built = one_built(execute_build(&plan, &options, &load, &mut project).unwrap());
        let result = output([
            "run",
            "--rm",
            "--pull",
            "never",
            "--env",
            "ONESHOT=1",
            &built.built.reference,
        ]);
        assert_eq!(result.lines().last(), Some(value));
        let content = output([
            "run",
            "--rm",
            "--entrypoint",
            "cat",
            &built.built.reference,
            "/app/built.json",
        ]);
        if value == "first"
            && let Some(previous) = &previous
        {
            assert_eq!(
                &content, previous,
                "unchanged inputs should reuse the build layer"
            );
        }
        if no_cache {
            assert_ne!(
                Some(&content),
                previous.as_ref(),
                "--no-cache must execute the build step again"
            );
        }
        previous = Some(content);
        assert!(built.built.platform.starts_with("linux/"));
        assert!(built.built.tags.contains(&image));
        assert!(
            output([
                "ps",
                "-aq",
                "--filter",
                &format!("name=buildx_buildkit_{}0", ployz_build::builder_name())
            ])
            .trim()
            .is_empty()
        );
        assert!(
            output([
                "ps",
                "-aq",
                "--filter",
                &format!("name={}-prepare", ployz_build::builder_name())
            ])
            .trim()
            .is_empty()
        );
        command(["volume", "inspect", &cache_volume()]);
    }

    let deploy = || {
        Command::new(env!("CARGO_BIN_EXE_ployz"))
            .current_dir(&root)
            .args([
                "--connect",
                &destination,
                "deploy",
                "--yes",
                "--recreate",
                "--skip-health",
                "--build-arg",
                "MESSAGE=changed",
            ])
            .env("PLOYZ_HEALTH_MONITOR_PERIOD", "0s")
            .env("PATH", &path)
            .env("XDG_RUNTIME_DIR", &ssh)
            .env("PLOYZ_SSH_CONTROL_PERSIST", "0")
            .output()
            .unwrap()
    };
    let deployed = deploy();
    assert!(
        deployed.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&deployed.stdout),
        String::from_utf8_lossy(&deployed.stderr)
    );
    let containers = || {
        String::from_utf8(
            cluster
                .shell(
                    0,
                    "docker ps -aq --no-trunc --filter label=ployz.service.name=app",
                )
                .unwrap()
                .stdout,
        )
        .unwrap()
    };
    let before = containers();
    assert_eq!(before.lines().count(), 1);
    let id = before.trim();
    let expected_image = output(["image", "inspect", &image, "--format", "{{.Id}}"]);
    let deployed_image = cluster
        .shell(0, &format!("docker inspect --format '{{{{.Image}}}}' {id}"))
        .unwrap();
    assert_eq!(
        String::from_utf8(deployed_image.stdout).unwrap(),
        expected_image
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if cluster.docker(0, &["exec", id, "node", "-e", "fetch('http://127.0.0.1:3000').then(r=>r.text()).then(t=>{if(t!=='changed')process.exit(1)})"]).is_ok() { break; }
        assert!(
            Instant::now() < deadline,
            "deployed application did not serve its built variable"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Neither a compiler failure nor a detection failure may touch the live
    // Service, even when the subsequent Deploy explicitly requests recreation.
    fs::write(
        root.join("build.js"),
        "throw new Error('expected compilation failure');",
    )
    .unwrap();
    let failed = deploy();
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("build"));
    assert_eq!(containers(), before);
    for file in ["package.json", "index.js", "build.js"] {
        fs::remove_file(root.join(file)).unwrap();
    }
    fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
    fs::write(
        root.join("compose.yaml"),
        format!(
            "services:\n  app:\n    image: {image}\n    build: {{context: ., x-recipe: railpack}}\n"
        ),
    )
    .unwrap();
    let failed = deploy();
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("Railpack"));
    assert_eq!(containers(), before);
    assert!(
        output([
            "ps",
            "-aq",
            "--filter",
            &format!("name={}-prepare", ployz_build::builder_name())
        ])
        .trim()
        .is_empty()
    );
    fs::remove_dir_all(ssh).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[path = "build_layer3/policy.rs"]
mod policy;
