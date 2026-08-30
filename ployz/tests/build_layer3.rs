use std::{
    fs,
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use ployz::compose::{BuildOptions, LoadOptions, execute_build, load_project, plan_build};

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
        name: name.clone(),
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
    let docker = root.join("docker");
    fs::write(
        &docker,
        format!(
            "#!/bin/sh\nexport TEST_REGISTRY_PORT={port}\nexport BUILDX_BUILDER=default\nexec docker \"$@\"\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o700)).unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/compose-build-basic");
    let load = LoadOptions {
        command: "build".into(),
        working_dir: Some(fixture),
        docker: Some(docker),
        ..Default::default()
    };
    let project = load_project(&load).unwrap();
    let options = BuildOptions {
        push_registry: true,
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

    execute_build(&plan, &options, &load).unwrap();
    for image in &cleanup.images {
        command(["image", "rm", image]);
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
