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
