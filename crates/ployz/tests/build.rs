use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use ployz::compose::{
    BuildOptions, BuiltService, LoadOptions, capture_build, load_project, parse_normalized,
    plan_build,
};
use ployz_build::Output;

/// Digests the Docker stand-in reports for one attempt's completed image.
const FIRST_CONTENT: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const SECOND_CONTENT: &str =
    "sha256:2222222222222222222222222222222222222222222222222222222222222222";

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
    if !isolated_build_test() {
        return;
    }
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
    fs::create_dir(root.join("api/readonly")).unwrap();
    fs::write(root.join("api/readonly/data"), "read-only source").unwrap();
    fs::set_permissions(root.join("api/readonly"), fs::Permissions::from_mode(0o555)).unwrap();
    fs::write(root.join("shared/data"), "original shared").unwrap();
    fs::write(root.join("Dockerfile"), "FROM scratch\nCOPY . /app\n").unwrap();
    fs::write(root.join("Dockerfile.dockerignore"), "hidden\nkey\n").unwrap();
    fs::write(root.join("api/.dockerignore"), "source\n").unwrap();
    fs::create_dir(root.join("api/hidden")).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(root.join("api/hidden/socket")).unwrap();
    fs::write(root.join("shared/.dockerignore"), "socket\n").unwrap();
    let _shared_socket =
        std::os::unix::net::UnixListener::bind(root.join("shared/socket")).unwrap();
    fs::write(root.join("api/key"), "private-key").unwrap();
    write_docker(&docker, &root);
    fs::write(root.join("digest"), FIRST_CONTENT).unwrap();
    fs::write(root.join("image"), "example.test/api:version2").unwrap();
    let mut project = parse_normalized(
        "name: demo\nservices:\n  api:\n    image: example.test/api:version2\n    build: {context: ./api, dockerfile: ../Dockerfile, additional_contexts: {shared: ./shared}, ssh: [deploy=./api/key], args: {VALUE: '$CAPTURED'}}\n  runtime:\n    image: alpine\n",
        &root,
    )
    .unwrap();
    let options = BuildOptions {
        build_args: vec!["MODE=release".into()],
        no_cache: true,
        pull: true,
        ..Default::default()
    };
    assert_eq!(options.output, Output::Load);
    let plan = plan_build(&project, &options).unwrap();

    let build = capture_build(&plan, &options, &mut project).unwrap();
    fs::write(&compose, "invalid: [edited after capture").unwrap();
    fs::set_permissions(root.join("api/readonly"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_dir_all(root.join("api")).unwrap();
    fs::remove_dir_all(root.join("shared")).unwrap();
    fs::remove_file(root.join("Dockerfile")).unwrap();
    fs::remove_file(root.join("Dockerfile.dockerignore")).unwrap();
    project.builds.clear();
    let outcome = build.execute(Some(&docker)).unwrap();
    let calls = fs::read_to_string(calls).unwrap();
    let bake = calls
        .lines()
        .find(|call| call.starts_with("buildx bake "))
        .expect("the capture was built once");
    // The build reads the capture, never the edited source Compose file.
    assert!(!bake.contains(compose.to_str().unwrap()), "{bake}");
    assert_eq!(bake.matches("--file").count(), 1, "{bake}");
    assert!(bake.contains("--load"), "{bake}");
    assert!(!bake.contains("--push"), "{bake}");
    assert!(bake.contains("--no-cache"), "{bake}");
    assert!(bake.contains("--pull"), "{bake}");
    assert!(
        !bake.contains("MODE=release"),
        "private arguments reached argv: {bake}"
    );
    // The builder runs the pinned BuildKit release and is removed afterwards
    // with its cache kept. The version is spelled out so an unpin fails here.
    assert!(
        calls
            .lines()
            .any(|call| call.starts_with("buildx create --name")
                && call.contains("--driver-opt image=moby/buildkit:v0.26.2")),
        "{calls}"
    );
    assert!(
        calls
            .lines()
            .any(|call| call == format!("buildx rm --keep-state {}", ployz_build::builder_name())),
        "{calls}"
    );
    let service = one_built(outcome);
    assert_eq!(service.image, "example.test/api:version2");
    assert_eq!(
        service.built.reference,
        format!("example.test/api@{FIRST_CONTENT}")
    );
    assert_eq!(service.built.tags, ["example.test/api:version2"]);
    assert_eq!(service.built.platform, "linux/amd64");
    let override_yaml = fs::read_to_string(captured).unwrap();
    assert!(override_yaml.contains("api"));
    assert!(override_yaml.contains("example.test/api:version2"));
    let config: serde_norway::Value = serde_norway::from_str(&override_yaml).unwrap();
    let captured_build = &config["services"]["api"]["build"];
    let staged = root.join("relocated");
    assert!(
        !override_yaml.contains("/ployz-build-"),
        "capture paths must be relocatable"
    );
    let context = staged.join(captured_build["context"].as_str().unwrap());
    assert!(
        !context.join("key").exists(),
        "SSH material entered reusable source"
    );
    assert!(!context.join("hidden").exists());
    assert_eq!(
        fs::read_to_string(context.join("source")).unwrap(),
        "original source"
    );
    assert_eq!(
        fs::read_to_string(context.join(captured_build["dockerfile"].as_str().unwrap())).unwrap(),
        "FROM scratch\nCOPY . /app\n"
    );
    let dockerfile = context.join(captured_build["dockerfile"].as_str().unwrap());
    let dockerfile = dockerfile.display();
    assert_eq!(
        fs::read_to_string(format!("{dockerfile}.dockerignore")).unwrap(),
        "hidden\nkey\n"
    );
    let ssh = captured_build["ssh"][0]
        .as_str()
        .unwrap()
        .strip_prefix("deploy=")
        .unwrap();
    assert_eq!(fs::read_to_string(staged.join(ssh)).unwrap(), "private-key");
    let shared = staged.join(
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
    assert_eq!(captured_build["args"]["MODE"].as_str(), Some("release"));
    let original = fs::read_to_string(root.join("capture-root")).unwrap();
    drop(build);
    assert!(!Path::new(original.trim()).exists());
    assert!(!override_yaml.contains("runtime"));
    fs::set_permissions(context.join("readonly"), fs::Permissions::from_mode(0o755)).unwrap();
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
    write_docker(&docker, &root);

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
    let calls = fs::read_to_string(calls).unwrap();
    let bake = calls
        .lines()
        .find(|call| call.starts_with("buildx bake "))
        .expect("validation still runs the recipe");
    assert!(bake.contains("--check"), "{bake}");
    // Validation produces no image, so it can neither load nor report one.
    assert!(!bake.contains("--load"), "{bake}");
    assert!(!bake.contains("--metadata-file"), "{bake}");
    assert!(
        !calls.lines().any(|call| call.starts_with("tag ")),
        "validation transferred an image: {calls}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn built_images_bind_to_exact_content_after_tag_reuse() {
    if !isolated_build_test() {
        return;
    }
    let root = std::env::temp_dir().join(format!("ployz-build-binding-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/Dockerfile"), "FROM scratch\n").unwrap();
    let docker = root.join("docker");
    write_docker(&docker, &root);
    fs::write(root.join("image"), "example.test/api:latest").unwrap();
    fs::write(root.join("digest"), FIRST_CONTENT).unwrap();
    let mut project = parse_normalized(
        "name: demo\nservices: {api: {image: 'example.test/api:latest', build: ./src}}\n",
        &root,
    )
    .unwrap();
    let options = BuildOptions::default();
    let plan = plan_build(&project, &options).unwrap();

    let first = capture_build(&plan, &options, &mut project)
        .unwrap()
        .execute(Some(&docker))
        .unwrap();
    // A later Build moves the same requested tag onto different content.
    fs::write(root.join("digest"), SECOND_CONTENT).unwrap();
    let second = capture_build(&plan, &options, &mut project)
        .unwrap()
        .execute(Some(&docker))
        .unwrap();

    let (first, second) = (one_built(first), one_built(second));
    assert_eq!(first.built.tags, second.built.tags);
    assert_eq!(
        first.built.reference,
        format!("example.test/api@{FIRST_CONTENT}")
    );
    assert_eq!(
        second.built.reference,
        format!("example.test/api@{SECOND_CONTENT}")
    );
    // Each attempt verified its own content instead of the shared tag.
    let calls = fs::read_to_string(root.join("calls")).unwrap();
    for content in [FIRST_CONTENT, SECOND_CONTENT] {
        assert!(
            calls.lines().any(|call| call
                == format!("image inspect example.test/api@{content} --format {{{{json .}}}}")),
            "{calls}"
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_image_the_store_does_not_hold_is_refused_as_a_result() {
    if !isolated_build_test() {
        return;
    }
    let root = std::env::temp_dir().join(format!("ployz-build-content-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/Dockerfile"), "FROM scratch\n").unwrap();
    let docker = root.join("docker");
    write_docker(&docker, &root);
    fs::write(root.join("image"), "example.test/api:latest").unwrap();
    fs::write(root.join("digest"), FIRST_CONTENT).unwrap();
    // The build claims one image while the store holds different content.
    fs::write(root.join("store"), SECOND_CONTENT).unwrap();
    let mut project = parse_normalized(
        "name: demo\nservices: {api: {image: 'example.test/api:latest', build: ./src}}\n",
        &root,
    )
    .unwrap();
    let options = BuildOptions::default();
    let plan = plan_build(&project, &options).unwrap();

    let build = capture_build(&plan, &options, &mut project).unwrap();
    let error = match build.execute(Some(&docker)) {
        Ok(outcome) => panic!("substituted content was reported as built: {outcome:?}"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains(SECOND_CONTENT), "{error}");
    assert!(
        error.contains("rather than the completed content"),
        "{error}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn several_requested_build_platforms_are_refused_with_the_service_named() {
    let mut project = parse_normalized(
        "name: demo\nservices: {api: {build: {context: ., platforms: [linux/amd64, linux/arm64]}}}\n",
        ".",
    )
    .unwrap();
    let options = BuildOptions::default();
    let plan = plan_build(&project, &options).unwrap();
    let error = match capture_build(&plan, &options, &mut project) {
        Ok(_) => panic!("a multi-platform Dockerfile Build was admitted"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("api"), "{error}");
    assert!(error.contains("one platform"), "{error}");
}

#[test]
fn content_holding_several_platforms_is_refused_however_it_was_requested() {
    if !isolated_build_test() {
        return;
    }
    let root = std::env::temp_dir().join(format!("ployz-build-index-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/Dockerfile"), "FROM scratch\n").unwrap();
    let docker = root.join("docker");
    write_docker(&docker, &root);
    fs::write(root.join("image"), "example.test/api:latest").unwrap();
    fs::write(root.join("digest"), FIRST_CONTENT).unwrap();
    // The store holds an index, whatever the Compose file asked for.
    fs::write(
        root.join("media"),
        "application/vnd.oci.image.index.v1+json",
    )
    .unwrap();
    let mut project = parse_normalized(
        "name: demo\nservices: {api: {image: 'example.test/api:latest', build: ./src}}\n",
        &root,
    )
    .unwrap();
    let options = BuildOptions::default();
    let plan = plan_build(&project, &options).unwrap();

    let build = capture_build(&plan, &options, &mut project).unwrap();
    let error = match build.execute(Some(&docker)) {
        Ok(built) => panic!("a multi-platform image was reported as built: {built:?}"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("several platforms"), "{error}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn registry_and_inline_caches_still_reach_buildkit() {
    let root = std::env::temp_dir().join(format!("ployz-build-cache-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/Dockerfile"), "FROM scratch\n").unwrap();
    let mut project = parse_normalized(
        "name: demo\nservices: {api: {build: {context: ./src, cache_from: ['type=registry\\,ref=example.test/cache'], cache_to: ['type=inline']}}}\n",
        &root,
    )
    .unwrap();
    let options = BuildOptions::default();
    let plan = plan_build(&project, &options).unwrap();
    // BuildKit owns registry caching; only a local path cannot be relocated.
    capture_build(&plan, &options, &mut project).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn settings_upstream_would_drop_are_named_before_execution() {
    for (setting, value) in [
        ("isolation", "default"),
        ("entitlements", "[network.host]"),
        ("cache_from", "['type=local\\,src=./cache']"),
        ("cache_to", "['type=local\\,dest=./cache']"),
        ("network", "host"),
        ("x-bake", "{output: ['type=local,dest=./out']}"),
        ("cache_to", "['type=gha']"),
    ] {
        let mut project = parse_normalized(
            &format!(
                "name: demo\nservices: {{api: {{build: {{context: ., {setting}: {value}}}}}}}\n"
            ),
            ".",
        )
        .unwrap();
        let options = BuildOptions::default();
        let plan = plan_build(&project, &options).unwrap();
        let error = match capture_build(&plan, &options, &mut project) {
            Ok(_) => panic!("an unsupported build setting was admitted"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains(&format!("build.{setting}")), "{error}");
        assert!(error.contains("api"), "{error}");
    }
}

/// Fake Docker has no shared builder; give its production lock a private HOME too.
/// Re-exec avoids changing process environment while other tests are running.
fn isolated_build_test() -> bool {
    const CHILD: &str = "PLOYZ_ISOLATED_BUILD_TEST";
    let thread = std::thread::current();
    let name = thread.name().expect("libtest names its test threads");
    if std::env::var(CHILD).as_deref() == Ok(name) {
        return true;
    }
    let home = std::env::temp_dir().join(format!("ployz-build-home-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&home).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, name)
        .env("HOME", &home)
        .output()
        .unwrap();
    fs::remove_dir_all(home).unwrap();
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "{name}:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    false
}

fn one_built(mut built: Vec<BuiltService>) -> BuiltService {
    assert_eq!(built.len(), 1, "expected one built Service, got {built:?}");
    built.remove(0)
}

/// A Docker stand-in answering the evidence the runner reads, recording every
/// call, and retaining the captured Compose file it was given.
fn write_docker(path: &Path, root: &Path) {
    let root = root.display();
    let script = format!(
        r#"#!/bin/sh
root='{root}'
case "$1" in --ready) exit 0 ;; esac
printf '%s\n' "$*" >> "$root/calls"
case "$1 $2" in
  'info --format') printf '%s\n' '{{"OSType":"linux","Architecture":"x86_64","DriverStatus":[["driver-type","io.containerd.snapshotter.v1"]]}}'; exit 0 ;;
  'context show') echo default; exit 0 ;;
  'buildx version') exit 0 ;;
  'version --format') printf 'linux/amd64\n'; exit 0 ;;
  'create --name'|'rm --force') exit 0 ;;
  'start --attach') test ! -f "$root/preparation-fails"; exit $? ;;
  cp\ *)
    case "$3" in
      *:/app) rm -rf "$root/received"; cp -a "$2" "$root/received" ;;
      *:/prepare.sh) cp "$2" "$root/prepare.sh" ;;
      *) printf '{{}}' > "$3" ;;
    esac
    exit 0 ;;
  'buildx create') : > "$root/builder"; exit 0 ;;
  'buildx inspect') exit 0 ;;
  'buildx rm') rm -f "$root/builder"; exit 0 ;;
  'buildx ls')
    if [ -f "$root/builder" ]; then
      printf '%s\n' '{{"Name":"{builder}","Nodes":[{{"Status":"running","Platforms":["linux/amd64","linux/arm64"],"DriverOpts":{{"image":"{image}"}}}}]}}'
    fi
    exit 0 ;;
  'image inspect')
    identity=$(cat "$root/store" 2>/dev/null || cat "$root/digest")
    media=$(cat "$root/media" 2>/dev/null || printf 'application/vnd.oci.image.manifest.v1+json')
    printf '{{"Os":"linux","Architecture":"amd64","Variant":null,"Descriptor":{{"mediaType":"%s","digest":"%s"}}}}' "$media" "$identity"
    exit 0 ;;
  'buildx bake')
    pwd > "$root/capture-root"
    printf '%s\n' "$HOME" "$DOCKER_CONFIG" "$SSH_AUTH_SOCK" > "$root/docker-environment"
    if [ -f "$DOCKER_CONFIG/config.json" ]; then cp "$DOCKER_CONFIG/config.json" "$root/docker-config.json"; fi
    rm -rf "$root/relocated"
    cp -a "$PWD" "$root/relocated"
    previous=
    for argument in "$@"; do
      if [ "$previous" = --file ]; then cp "$argument" "$root/override.yaml"; fi
      if [ "$previous" = --metadata-file ]; then
        printf '{{"api": {{"containerimage.digest": "%s", "image.name": "%s"}}}}'           "$(cat "$root/digest")" "$(cat "$root/image")" > "$argument"
      fi
      previous=$argument
    done
    exit 0 ;;
  *'config --environment') exit 0 ;;
esac
case "$*" in
  *' config '*)
    printf 'name: demo\nservices:\n  api:\n    image: example.test/api\n    build: {{context: .}}\n'
    exit 0 ;;
esac
exit 1
"#,
        builder = ployz_build::builder_name(),
        image = ployz_build::BUILDKIT_IMAGE,
    );
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    // A concurrently forked process can briefly hold a just-written program
    // open, so wait until this one can actually be executed.
    for _ in 0..100 {
        match Command::new(path).arg("--ready").status() {
            Ok(_) => return,
            Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => panic!("Docker stand-in {}: {error}", path.display()),
        }
    }
    panic!("Docker stand-in {} never became executable", path.display());
}

#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
fn build_and_runtime_share_one_captured_secret_resolution() {
    if !isolated_build_test() {
        return;
    }
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
    build: {context: ./src, secrets: [token], args: {MODE: compose}}
    environment: {TOKEN: 'secret://token', MODE: runtime, DEFAULT: service}
secrets:
  token: {x-command: "sh -c 'echo once >> provider-calls; printf private-token'"}
"#,
        &root,
    )
    .unwrap();
    let docker = root.join("docker");
    write_docker(&docker, &root);
    let options = BuildOptions {
        output: Output::Validate,
        build_args: vec!["MODE=cli".into()],
        ..Default::default()
    };
    let plan = plan_build(&project, &options).unwrap();
    let build = capture_build(&plan, &options, &mut project).unwrap();
    build.execute(Some(&docker)).unwrap();
    let config: serde_norway::Value =
        serde_norway::from_str(&fs::read_to_string(root.join("override.yaml")).unwrap()).unwrap();
    assert_eq!(
        config["services"]["api"]["build"]["args"]["TOKEN"].as_str(),
        Some("private-token")
    );
    assert_eq!(
        config["services"]["api"]["build"]["args"]["DEFAULT"].as_str(),
        Some("service")
    );
    assert_eq!(
        config["services"]["api"]["build"]["args"]["MODE"].as_str(),
        Some("cli")
    );
    assert_eq!(
        project.services["api"].container.environment["MODE"],
        "runtime"
    );
    project.resolve_secrets().unwrap();
    project.resolve_secrets().unwrap();
    assert_eq!(
        fs::read_to_string(root.join("provider-calls")).unwrap(),
        "once\n"
    );
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

#[path = "build/capture.rs"]
mod capture;

#[path = "build/railpack.rs"]
mod railpack;
