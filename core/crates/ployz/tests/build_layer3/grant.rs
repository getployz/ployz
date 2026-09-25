//! Informing evidence for a CI Builder: `ployz build` pushes into a Machine with a
//! Build Grant, and the Machine holds the image under the digest it reported.

use std::{collections::BTreeMap, fs, time::Duration};

use ployz::sdk::PreparationInput;
use ployz_core::{EndBuildGrantRequest, MintBuildGrantRequest, ServiceName};
use ployz_testkit::{Cluster, ClusterPlan};
use serde_json::Value;

#[tokio::test]
#[ignore = "informing: requires the privileged Ployz testkit image with Buildx and the Ployz Relay"]
async fn ployz_build_pushes_into_a_machine_with_a_build_grant() {
    let cluster =
        Cluster::create(ClusterPlan::new(&format!("l3-grant-{}", std::process::id()), 1).unwrap())
            .unwrap();
    cluster.wait_ready(Duration::from_secs(120)).await.unwrap();
    cluster.initialize_first().await.unwrap();
    let session = super::session(&cluster).await;
    let dockerfile = "FROM alpine:3.23.3\nRUN echo pushed-with-a-grant > /grant\n";

    // The runner's checkout lives on the Machine that runs `ployz build`.
    let commit = cluster
        .machine_shell(
            0,
            &format!(
                "set -e; mkdir /tmp/app; cd /tmp/app; git init -q; printf %s {} > Dockerfile; \
                 git add Dockerfile; git -c user.name=ployz -c user.email=ployz@example.invalid commit -qm app; \
                 git rev-parse HEAD",
                shell_words::quote(dockerfile)
            ),
        )
        .unwrap()
        .trim()
        .to_owned();

    // Cloud's expected fingerprint is the one its own preparation records.
    let deployment = super::git_deployment("dockerfile", "grant");
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("Dockerfile"), dockerfile).unwrap();
    let app: ServiceName = "app".parse().unwrap();
    let prepared = session
        .prepare(PreparationInput {
            deployment: deployment.clone(),
            sources: BTreeMap::from([(app.clone(), root.path().to_owned())]),
            source_commits: BTreeMap::from([(app.clone(), commit.clone())]),
            build_receipts: BTreeMap::new(),
            build_index: 0,
            preferred_machine: None,
        })
        .unwrap()
        .finished()
        .await
        .unwrap();
    let fingerprint = prepared.build_receipts()[&app].fingerprint.clone();
    prepared.close();

    let minted = session
        .mint_build_grant(MintBuildGrantRequest {
            repository: ployz_core::BuildGrantRepository::parse("ployz-build/app").unwrap(),
        })
        .await
        .unwrap();
    let output = cluster
        .machine_shell(
            0,
            &format!(
                "set -e; printf %s {} > /tmp/deployment.json; \
                 PLOYZ_BUILD_GRANT={} ployz build --deployment /tmp/deployment.json \
                 --commit {commit} --fingerprint {fingerprint} --source /tmp/app",
                shell_words::quote(&deployment.to_string()),
                shell_words::quote(&minted.grant.to_secret_string()),
            ),
        )
        .unwrap();
    let pushed: Value = serde_json::from_str(output.trim()).unwrap();
    let digest = pushed.get("digest").and_then(Value::as_str).unwrap();
    let tag = pushed.get("tag").and_then(Value::as_str).unwrap();
    assert_eq!(
        tag,
        format!(
            "ployz-build/app:ployz-sha256-{}",
            digest.strip_prefix("sha256:").unwrap()
        )
    );
    // The Machine holds the pushed tag as exactly the reported content.
    let held = cluster
        .machine_shell(
            0,
            &format!("docker image inspect {tag} --format '{{{{.Id}}}}'"),
        )
        .unwrap();
    assert_eq!(held.trim(), digest);
    let ended = session
        .end_build_grant(EndBuildGrantRequest { id: minted.id })
        .await
        .unwrap();
    assert_eq!(
        ended.pushed.as_ref().map(ployz_core::ImageDigest::as_str),
        Some(digest)
    );
    session.close().await;
}
