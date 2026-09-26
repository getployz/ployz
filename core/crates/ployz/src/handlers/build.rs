//! `ployz build`: check out one Git Service's commit, refuse unless its build inputs
//! match the expected fingerprint, build it with local Buildx, and push it into one
//! Machine with a Build Grant. It knows nothing of the CI system running it; it uses
//! the GitHub Actions cache when that runtime's environment is exported to it.

use std::{collections::BTreeMap, path::Path, process::Command};

use clap::ArgMatches;
use ployz_core::{
    BuildGrant, RETAINED_DIGEST_TAG_PREFIX, ServiceName,
    config::{ServiceSource, parse_service_config},
};
use serde_json::{Value, json};

use super::{Error, leaf_matches, required, runtime};
use crate::{
    build::LocalImage,
    connect::{ManagementRelay, open_grant_registry},
    failure::Failure,
    sdk::{PreparationInput, preparation::capture},
};

/// Why `ployz build` refused or failed after its arguments were accepted.
#[derive(Debug, thiserror::Error)]
enum BuildCommandError {
    #[error(
        "the checkout's build inputs do not match the expected fingerprint; refusing to build. \
         The fingerprint covers this ployz version ({version}), so install the version that computed it"
    )]
    FingerprintMismatch { version: &'static str },
    #[error("the checkout is not at {0}")]
    NotAtCommit(String),
    #[error("the checkout has changes outside its commit; refusing to build")]
    UncleanCheckout,
    #[error("git {command} failed: {stderr}")]
    Git { command: String, stderr: String },
    #[error("docker {command} failed: {stderr}")]
    Docker { command: String, stderr: String },
    #[error(transparent)]
    Build(#[from] crate::build::Error),
    #[error("the built image is not identified by a SHA-256 digest")]
    NotDigestIdentified,
    #[error("{0}")]
    GrantRefused(&'static str),
}

impl From<BuildCommandError> for Failure {
    fn from(error: BuildCommandError) -> Self {
        Self::command(error)
    }
}

pub(super) fn build(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let grant = BuildGrant::parse(required(matches, "grant")?)?;
    let expected = required(matches, "fingerprint")?;
    let commit = required(matches, "commit")?;
    let source = Path::new(
        matches
            .get_one::<String>("source")
            .expect("source has a default"),
    )
    .canonicalize()?;
    let deployment: Value =
        serde_json::from_slice(&std::fs::read(required(matches, "deployment")?)?)?;
    let service = git_service(&deployment)?;
    if !ployz_core::is_lower_hex(&commit, 40) {
        return Err(Error::usage("--commit must be a full lowercase Git SHA"));
    }
    check_out(&source, &commit)?;
    let captured = capture(PreparationInput {
        deployment,
        sources: BTreeMap::from([(service.clone(), source)]),
        source_commits: BTreeMap::from([(service.clone(), commit)]),
        build_receipts: BTreeMap::new(),
        build_index: 0,
        preferred_machine: None,
    })?;
    if captured.fingerprints.get(&service) != Some(&expected) {
        return Err(BuildCommandError::FingerprintMismatch {
            version: env!("CARGO_PKG_VERSION"),
        }
        .into());
    }
    let events = std::sync::Arc::new(Events(
        matches
            .get_one::<String>("events")
            .map(std::fs::File::create)
            .transpose()?
            .map(std::sync::Mutex::new),
    ));
    runtime()?.block_on(async {
        let cancellation = crate::cancellation::on_ctrl_c();
        let build = captured.build;
        let export = cancellation.clone();
        let building = events.clone();
        let (build, built) = tokio::task::spawn_blocking(move || {
            let built = build.execute_local(&cancellation, &|event| building.write(event));
            (build, built)
        })
        .await
        .map_err(std::io::Error::other)?;
        push(&grant, &built.map_err(BuildCommandError::from)?, &events).await?;
        // The pushed image and its printed result are final; uploading cache only speeds up
        // the next Build, so its failure is a warning and it never reaches the events file.
        let exported = tokio::task::spawn_blocking(move || build.export_local_cache(&export))
            .await
            .map_err(std::io::Error::other)?;
        if let Err(error) = exported {
            eprintln!(
                "warning: the image was pushed, but its build cache was not exported: {error}"
            );
        }
        Ok(())
    })
}

/// The `--events` file: one `{"at": <unix ms>, "event": {"Build": <progress>}}` line per
/// event, the SDK preparation event shape Cloud's build log reads. Progress is best effort:
/// a failed write never fails the build.
struct Events(Option<std::sync::Mutex<std::fs::File>>);

impl Events {
    fn write(&self, event: &ployz_build::Progress) {
        use std::io::Write as _;
        let Some(file) = &self.0 else { return };
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        if let Ok(mut file) = file.lock() {
            let _ = writeln!(file, "{}", json!({"at": at, "event": {"Build": event}}));
        }
    }

    /// One line of the push: output of the open `Push` stage.
    fn push_line(&self, line: &str) {
        self.write(&ployz_build::Progress::Output(
            format!("{line}\n").into_bytes(),
        ));
    }
}

/// The deployment's single Git-sourced Service: the one this command builds.
fn git_service(deployment: &Value) -> Result<ServiceName, Error> {
    let mut services = deployment
        .get("snapshots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|snapshot| parse_service_config(snapshot.get("config")?.clone()).ok())
        .filter(|config| matches!(config.settings.source, ServiceSource::Git { .. }))
        .map(|config| config.settings.private_dns);
    match (services.next(), services.next()) {
        (Some(service), None) => Ok(service),
        _ => Err(Error::usage(
            "--deployment must hold exactly one Git-sourced Service",
        )),
    }
}

/// Check out `commit`, fetching it from `origin` when absent, and require a clean tree.
fn check_out(source: &Path, commit: &str) -> Result<(), Error> {
    if git(source, &["rev-parse", "HEAD"])? != commit {
        git(source, &["fetch", "--quiet", "--depth=1", "origin", commit])?;
        git(source, &["checkout", "--quiet", "--detach", commit])?;
    }
    if git(source, &["rev-parse", "HEAD"])? != commit {
        return Err(BuildCommandError::NotAtCommit(commit.to_owned()).into());
    }
    if !git(source, &["status", "--porcelain"])?.is_empty() {
        return Err(BuildCommandError::UncleanCheckout.into());
    }
    Ok(())
}

fn git(source: &Path, args: &[&str]) -> Result<String, Error> {
    let output = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(BuildCommandError::Git {
            command: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        }
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Push through the grant with `docker push`, which sends only the layers the Machine
/// lacks, and print what the Machine verified it received. Each layer's progress line
/// goes to the events file as the push's output.
async fn push(grant: &BuildGrant, built: &LocalImage, events: &Events) -> Result<(), Error> {
    let digest = &built.image.reference;
    let hex = digest
        .strip_prefix("sha256:")
        .ok_or(BuildCommandError::NotDigestIdentified)?;
    let tag = format!("{}:{RETAINED_DIGEST_TAG_PREFIX}{hex}", built.repository);
    events.write(&ployz_build::Progress::Stage(ployz_build::Stage::Push));
    let registry = open_grant_registry(grant, &ManagementRelay::default()).await?;
    let local = format!("{}/{tag}", registry.address());
    docker(&["tag", digest, &local]).await?;
    let pushed = docker_push(&local, events).await;
    // The loopback name is meaningless once this command exits.
    let _ = docker(&["image", "rm", &local]).await;
    if let Err(error) = pushed {
        return Err(registry.refusal().map_or(error, |refusal| {
            BuildCommandError::GrantRefused(refusal).into()
        }));
    }
    events.push_line(&format!("Pushed {digest}"));
    // The Machine stores a tagged manifest only when its bytes hash to the tag's
    // digest, so a completed push is the Machine's confirmation of `digest`.
    println!(
        "{}",
        json!({"digest": digest, "tag": tag, "platforms": built.image.platforms})
    );
    Ok(())
}

/// `docker push`, its per-layer lines streamed to the events file as they come.
async fn docker_push(reference: &str, events: &Events) -> Result<(), Error> {
    use tokio::io::AsyncBufReadExt as _;
    let mut child = tokio::process::Command::new("docker")
        .args(["push", reference])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let stderr = child.stderr.take().map(|stderr| {
        tokio::spawn(async move {
            let mut text = String::new();
            let _ = tokio::io::AsyncReadExt::read_to_string(
                &mut tokio::io::BufReader::new(stderr),
                &mut text,
            )
            .await;
            text
        })
    });
    if let Some(stdout) = child.stdout.take() {
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            events.push_line(&line);
        }
    }
    let status = child.wait().await?;
    let stderr = match stderr {
        Some(task) => task.await.unwrap_or_default(),
        None => String::new(),
    };
    if status.success() {
        return Ok(());
    }
    Err(BuildCommandError::Docker {
        command: "push".to_owned(),
        stderr: stderr.trim().to_owned(),
    }
    .into())
}

async fn docker(args: &[&str]) -> Result<(), Error> {
    let output = tokio::process::Command::new("docker")
        .args(args)
        .output()
        .await?;
    if output.status.success() {
        return Ok(());
    }
    Err(BuildCommandError::Docker {
        command: args.first().copied().unwrap_or_default().to_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    }
    .into())
}
