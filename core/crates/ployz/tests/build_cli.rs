//! `ployz build` refuses a checkout that does not match the expected build inputs
//! before it builds or pushes anything.

use std::{path::Path, process::Command};

fn git(repository: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args([
            "-c",
            "user.name=ployz",
            "-c",
            "user.email=ployz@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
fn build_refuses_a_mismatched_fingerprint_or_an_unclean_checkout() {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("app");
    std::fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "--quiet"]);
    std::fs::write(repository.join("Dockerfile"), "FROM scratch\n").unwrap();
    git(&repository, &["add", "Dockerfile"]);
    git(&repository, &["commit", "--quiet", "-m", "app"]);
    let commit = git(&repository, &["rev-parse", "HEAD"]);
    let deployment = root.path().join("deployment.json");
    std::fs::write(
        &deployment,
        serde_json::json!({"projectName": "build", "snapshots": [{"config": {
            "version": 2, "privateDns": "app", "healthcheck": {"type": "none"},
            "restartPolicy": "on-failure",
            "source": {"version": 2, "type": "git", "repository": "acme/app", "repositoryId": 42,
                "access": {"type": "public"}, "rootDir": "/", "branch": {"type": "connected", "name": "main"}},
            "build": {"buildMethod": "dockerfile", "dockerfilePath": "Dockerfile", "command": null}
        }}]})
        .to_string(),
    )
    .unwrap();
    let grant =
        ployz_core::BuildGrant::new(ployz_core::ManagementIdentity::from_bytes([1; 32]), [2; 32]);
    let build = || {
        let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
            .args(["build", "--deployment"])
            .arg(&deployment)
            .args([
                "--commit",
                &commit,
                "--fingerprint",
                &"0".repeat(64),
                "--source",
            ])
            .arg(&repository)
            .env("PLOYZ_BUILD_GRANT", grant.to_secret_string())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    let refused = build();
    assert!(refused.contains("expected fingerprint"), "{refused}");
    std::fs::write(repository.join("stray"), "not in the commit").unwrap();
    let refused = build();
    assert!(refused.contains("outside its commit"), "{refused}");
}
