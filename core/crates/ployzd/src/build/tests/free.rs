//! A Free Build: it opens no Sealed value, reads no login and takes no other Service's
//! image, and its image loads under no name with this Cluster's receipt.

use super::*;

/// A capture at `dir` whose recipe names a tag, with `private` files beside it.
fn capture(dir: &Path, private: &[(&str, &str)]) {
    fs::create_dir_all(dir.join("source")).unwrap();
    fs::write(
        dir.join("source/Dockerfile"),
        "FROM scratch\nCOPY payload /payload\n",
    )
    .unwrap();
    fs::write(dir.join("source/payload"), "free").unwrap();
    fs::create_dir_all(dir.join("private/docker")).unwrap();
    for (path, contents) in private {
        fs::write(dir.join("private").join(path), contents).unwrap();
    }
    fs::write(
        dir.join("compose.yaml"),
        "services:\n  api:\n    image: example.test/api:built\n    build:\n      context: source\n      dockerfile: Dockerfile\n",
    )
    .unwrap();
}

fn definition() -> Definition {
    Definition {
        retained_tags: Vec::new(),
        image_contexts: Default::default(),
        targets: vec![ployz_build::Target {
            name: "api".into(),
            platforms: Vec::new(),
        }],
        output: Output::Load,
        no_cache: false,
        pull: false,
    }
}

impl Fixture {
    /// Send a Free Build of the capture at `dir` and read its terminal report, holding the
    /// request open until then as a client does.
    async fn build_free(&self, definition: Definition, dir: &Path) -> Outcome {
        let mut frames = vec![remote::encode(&Input::Start(definition)).unwrap()];
        remote::upload(dir, |input| {
            frames.push(remote::encode(&input).unwrap());
            Ok(())
        })
        .unwrap();
        let (sender, receiver) = mpsc::channel(frames.len());
        for frame in frames {
            sender.send(frame).await.unwrap();
        }
        let socket = LocalSocket(PathBuf::from(self.address.trim_start_matches("unix://")));
        let channel = tonic::transport::Endpoint::from_static("http://[::]:50051")
            .connect_with_connector(socket)
            .await
            .unwrap();
        let mut events = MachineRpcClient::new(channel)
            .build_free(ReceiverStream::new(receiver))
            .await
            .unwrap()
            .into_inner();
        loop {
            let event = events.message().await.unwrap().unwrap();
            if let Event::Finished(outcome) = remote::decode(&event).unwrap() {
                drop(sender);
                return outcome;
            }
        }
    }
}

#[tokio::test]
async fn a_free_build_loads_its_image_untagged_with_its_clusters_receipt_and_reads_no_login() {
    let cluster = crate::sealed_env::test_support::Cluster::new();
    let fixture = Fixture::with(|_| {}, Some(cluster.dir.0.clone())).await;
    let dir = fixture.root.join("free-capture");
    capture(&dir, &[]);

    let outcome = fixture.build_free(definition(), &dir).await;

    let Outcome::FreeImages { images, .. } = &outcome else {
        panic!("expected a free Build's images, got {outcome:?}");
    };
    let [free] = images.as_slice() else {
        panic!("expected one image, got {images:?}");
    };
    assert_eq!(free.image.tags, Vec::<String>::new());
    assert_eq!(free.receipt.image.as_str(), free.image.reference);
    assert!(cluster.store().made_free_receipt(&free.receipt).unwrap());
    let args = fs::read_to_string(fixture.root.join("bake-args")).unwrap();
    let args: Vec<_> = args.lines().collect();
    assert!(args.contains(&"api.tags="), "{args:?}");
    let builder = args
        .iter()
        .skip_while(|arg| **arg != "--builder")
        .nth(1)
        .unwrap();
    assert_eq!(
        *builder,
        format!("ployz-free-{}", rustix::process::getuid().as_raw())
    );
    // The Docker client read a configuration this Server made empty, not the capture's.
    let config = fs::read_to_string(fixture.root.join("docker-config")).unwrap();
    assert!(!config.ends_with("private/docker"), "{config}");
    assert_eq!(
        fs::read_to_string(fixture.root.join("docker-config-files")).unwrap(),
        ""
    );
}

/// A Server only Cloud deploys to cleans up as each Free Build ends: a Free Build's image
/// idle past a week goes, a recent one stays for its creation, and an untagged image Ployz
/// didn't build is never touched.
#[tokio::test]
async fn each_free_build_removes_the_free_images_gone_idle_and_only_those() {
    let cluster = crate::sealed_env::test_support::Cluster::new();
    let fixture = Fixture::with(|_| {}, Some(cluster.dir.0.clone())).await;
    let ago = |hours: i64| (chrono::Utc::now() - chrono::Duration::hours(hours)).to_rfc3339();
    let image = |hex: &str, labels: serde_json::Value, tagged: String| {
        serde_json::json!({
            "Id": format!("sha256:{}", hex.repeat(64)), "ParentId": "", "RepoTags": [],
            "RepoDigests": [], "Created": 0, "Size": 1, "SharedSize": 0, "Containers": 0,
            "Labels": labels, "LastTagTime": tagged,
        })
    };
    let free = serde_json::json!({ployz_build::FREE_IMAGE_LABEL: "true"});
    // Listed, and so removed, first: a wrong removal is the one the cleanup makes first.
    fixture.docker.listed_images.lock().unwrap().extend([
        image("c", serde_json::json!({}), ago(30 * 24)),
        image("b", free.clone(), ago(1)),
        image("a", free, ago(30 * 24)),
    ]);
    let dir = fixture.root.join("free-capture");
    capture(&dir, &[]);
    let removed = fixture.docker.image_removed.clone();
    let mut removal = std::pin::pin!(removed.notified());
    removal.as_mut().enable();

    let outcome = fixture.build_free(definition(), &dir).await;

    assert!(matches!(outcome, Outcome::FreeImages { .. }), "{outcome:?}");
    tokio::time::timeout(Duration::from_secs(10), removal)
        .await
        .expect("the Free Build's cleanup removed an image");
    assert_eq!(
        *fixture.docker.removed_images.lock().unwrap(),
        [format!("sha256:{}", "a".repeat(64))]
    );
    let args = fs::read_to_string(fixture.root.join("bake-args")).unwrap();
    assert!(
        args.lines()
            .any(|arg| arg == format!("api.labels.{}=true", ployz_build::FREE_IMAGE_LABEL)),
        "{args}"
    );
}

#[tokio::test]
async fn a_free_build_refuses_private_material_as_it_arrives_and_never_opens_it() {
    let cluster = crate::sealed_env::test_support::Cluster::new();
    for (path, contents) in [
        // Opened, this record would fail as one that isn't valid: refused first, it never is.
        ("sealed.json", "{}"),
        (
            "docker/config.json",
            r#"{"auths":{"registry.example.test":{"auth":"dXNlcjpwYXNz"}}}"#,
        ),
        ("sealed-args.json", "{}"),
    ] {
        let fixture = Fixture::with(|_| {}, Some(cluster.dir.0.clone())).await;
        let dir = fixture.root.join("free-capture");
        capture(&dir, &[(path, contents)]);

        let outcome = fixture.build_free(definition(), &dir).await;

        assert!(
            matches!(&outcome, Outcome::Failed { stage: Stage::Upload, message, .. }
                if message.contains("takes no Sealed values, registry login")),
            "{path}: {outcome:?}"
        );
        assert!(!fixture.root.join("executed").exists(), "{path}");
    }
}

#[tokio::test]
async fn a_free_build_asking_for_another_services_image_or_a_tag_never_queues() {
    let fixture = Fixture::new().await;
    let dir = fixture.root.join("free-capture");
    capture(&dir, &[]);
    let mut contexts = definition();
    contexts.image_contexts.insert(
        "base".into(),
        serde_json::from_value(serde_json::json!({
            "reference": format!("example.test/base@sha256:{}", "2".repeat(64)),
            "platforms": ["linux/amd64"],
            "source": {"management_address": "fdcc::1", "port": 7572},
        }))
        .unwrap(),
    );
    let mut tagged = definition();
    tagged.retained_tags = vec![format!(
        "example.test/api:ployz-build-{}",
        uuid::Uuid::new_v4()
    )];

    for definition in [contexts, tagged] {
        let outcome = fixture.build_free(definition, &dir).await;
        assert!(
            matches!(
                &outcome,
                Outcome::Failed {
                    stage: Stage::Admission,
                    ..
                }
            ),
            "{outcome:?}"
        );
    }
    assert!(!fixture.root.join("executed").exists());
}
