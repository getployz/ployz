//! `ployz build`: check out one Git Service's commit, refuse unless its build inputs
//! match the expected fingerprint, build it with local Buildx, and push it into one
//! Machine with a Build Grant. It knows nothing of the CI system running it; it uses
//! the GitHub Actions cache when that runtime's environment is exported to it.

use std::{collections::BTreeMap, path::Path, process::Command, sync::Arc};

use clap::{ArgMatches, ValueHint};
use tokio_util::sync::CancellationToken;

use crate::cli::{env, value};
use ployz_core::{
    BuildGrant, OpenedSealed, RETAINED_DIGEST_TAG_PREFIX, SealedRecord, ServiceName,
    config::{ServiceSource, parse_service_config},
};
use serde_json::{Value, json};

use super::{Error, leaf_matches, required, runtime};
use crate::{
    build::{CapturedBuild, LocalImage},
    connect::{ManagementRelay, open_grant_registry},
    failure::Failure,
    sdk::{
        PreparationInput, expected_fingerprints,
        preparation::{capture_opened, frozen_intent},
    },
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
    #[error("{0}")]
    Sealed(String),
    #[error("the build was cancelled")]
    Cancelled,
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
    let fingerprint = expected_fingerprints(
        deployment.clone(),
        BTreeMap::from([(service.clone(), commit.clone())]),
    )?
    .remove(&service);
    if fingerprint.as_ref() != Some(&expected) {
        return Err(BuildCommandError::FingerprintMismatch {
            version: env!("CARGO_PKG_VERSION"),
        }
        .into());
    }
    let events = Arc::new(Events(
        matches
            .get_one::<String>("events")
            .map(std::fs::File::create)
            .transpose()?
            .map(std::sync::Mutex::new),
    ));
    // A CI runner's TERM must unwind the staged values and capture, not kill the process.
    let (cancellation, _sigterm) = crate::cancellation::interrupted_or_terminated()?;
    let runtime = runtime()?;
    let opened = runtime.block_on(until_cancelled(
        &cancellation,
        sealed(&grant, &deployment, &service, &events),
    ))?;
    let captured = capture_opened(
        PreparationInput {
            deployment,
            sources: BTreeMap::from([(service.clone(), source)]),
            source_commits: BTreeMap::from([(service, commit)]),
            ..PreparationInput::default()
        },
        opened,
    )?;
    runtime.block_on(run(captured.build, &grant, events, &cancellation))
}

/// Build `build` locally, push it through `grant` and export its cache, stopping at
/// `cancellation`. The Build's staged values and capture are gone when this returns.
async fn run(
    build: CapturedBuild,
    grant: &BuildGrant,
    events: Arc<Events>,
    cancellation: &CancellationToken,
) -> Result<(), Error> {
    let token = cancellation.clone();
    let building = Arc::clone(&events);
    let (build, built) = tokio::task::spawn_blocking(move || {
        let built = build.execute_local(&token, &|event| building.write(event));
        (build, built)
    })
    .await
    .map_err(std::io::Error::other)?;
    push(
        grant,
        &built.map_err(BuildCommandError::from)?,
        &events,
        cancellation,
    )
    .await?;
    // The pushed image and its printed result are final; uploading cache only speeds up
    // the next Build, so its failure is a warning and it never reaches the events file.
    let export = cancellation.clone();
    let exported = tokio::task::spawn_blocking(move || build.export_local_cache(&export))
        .await
        .map_err(std::io::Error::other)?;
    if let Err(error) = exported {
        crate::ui::warn_cause(
            "the image was pushed, but its build cache was not exported",
            &error,
        );
    }
    Ok(())
}

async fn until_cancelled<T>(
    cancellation: &CancellationToken,
    work: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    tokio::select! {
        done = work => done,
        () = cancellation.cancelled() => Err(BuildCommandError::Cancelled.into()),
    }
}

/// The Sealed values `service` reads, opened by the grant's Machine. A failure reaches
/// the events file only as a fixed line, naming at most a variable.
async fn sealed(
    grant: &BuildGrant,
    deployment: &Value,
    service: &ServiceName,
    events: &Events,
) -> Result<Option<OpenedSealed>, Error> {
    let intent = frozen_intent(deployment.clone())?;
    let record = intent
        .target
        .iter()
        .find(|spec| spec.name == *service)
        .map(SealedRecord::of)
        .transpose()
        .map_err(|error| BuildCommandError::Sealed(error.to_string()))?
        .flatten();
    let Some(record) = record else {
        return Ok(None);
    };
    let fetched = async {
        let registry = open_grant_registry(grant, &ManagementRelay::default()).await?;
        Ok(registry.sealed().await?)
    }
    .await;
    covering(fetched, &record, events).map(Some)
}

fn covering(
    fetched: Result<OpenedSealed, Error>,
    record: &SealedRecord,
    events: &Events,
) -> Result<OpenedSealed, Error> {
    let opened = fetched.inspect_err(|_| events.push_line(SEALED_UNREAD))?;
    opened.covering(record.keys()).map_err(|missing| {
        let missing = missing.to_string();
        events.push_line(&missing);
        BuildCommandError::Sealed(missing).into()
    })
}

const SEALED_UNREAD: &str = "the Server did not open this Build's sealed values";

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
async fn push(
    grant: &BuildGrant,
    built: &LocalImage,
    events: &Events,
    cancellation: &CancellationToken,
) -> Result<(), Error> {
    let digest = &built.image.reference;
    let hex = digest
        .strip_prefix("sha256:")
        .ok_or(BuildCommandError::NotDigestIdentified)?;
    let tag = format!("{}:{RETAINED_DIGEST_TAG_PREFIX}{hex}", built.repository);
    events.write(&ployz_build::Progress::Stage(ployz_build::Stage::Push));
    let registry = until_cancelled(cancellation, async {
        Ok(open_grant_registry(grant, &ManagementRelay::default()).await?)
    })
    .await?;
    let local = format!("{}/{tag}", registry.address());
    docker(&["tag", digest, &local]).await?;
    let pushed = until_cancelled(cancellation, docker_push(&local, events)).await;
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
    // The Cloud runner's fixed result line: stdout in every mode.
    use std::io::Write as _;
    writeln!(
        crate::ui::stdout_bytes(),
        "{}",
        json!({"digest": digest, "tag": tag, "platforms": built.image.platforms})
    )?;
    Ok(())
}

/// `docker push`, its per-layer lines streamed to the events file as they come.
async fn docker_push(reference: &str, events: &Events) -> Result<(), Error> {
    use tokio::io::AsyncBufReadExt as _;
    let mut child = tokio::process::Command::new("docker")
        .args(["push", reference])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
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

pub(super) fn command(command: clap::Command) -> clap::Command {
    command
        .about("Build one Git Service and push it to a Server that granted the build")
        .long_about("Build one Git Service and push it to the Server whose Build Grant allows it.\n\nChecks out --commit and refuses to build unless the build inputs match --fingerprint. When the GitHub Actions cache runtime (ACTIONS_RUNTIME_TOKEN and its cache URLs) is in the environment, Buildx uses the GitHub Actions cache.")
        .arg(
            value("grant", None)
                .env(env::BUILD_GRANT)
                .hide_env_values(true)
                .required(true)
                .help("Build Grant naming the receiving Server; prefer the environment variable"),
        )
        .arg(
            value("deployment", None)
                .required(true)
                .value_hint(ValueHint::FilePath)
                .help("Frozen deployment JSON holding exactly one Git-sourced Service"),
        )
        .arg(value("commit", None).required(true).help("Commit to build"))
        .arg(
            value("fingerprint", None)
                .required(true)
                .help("Expected build-input fingerprint; the build is refused on mismatch"),
        )
        .arg(
            value("source", None)
                .default_value(".")
                .value_hint(ValueHint::DirPath)
                .help("Repository working tree of the Service"),
        )
        .arg(
            value("events", None)
                .value_hint(ValueHint::FilePath)
                .help("Also write build progress to this file, one JSON line per event"),
        )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ployz_core::{EnvValue, SealedIdentity, SealedScope, SealedValue};

    use super::*;

    const MARKER: &str = "marker-sealed-0123456789";

    fn sealed_record() -> SealedRecord {
        let value = EnvValue::Sealed(SealedValue {
            sealed: ployz_core::Ciphertext::from_bytes(vec![7; 48]),
            scope: SealedScope::Shared,
            key: "NPM_TOKEN".into(),
        });
        SealedRecord::new(
            SealedIdentity {
                organization: "org_test".into(),
                environment: uuid::Uuid::from_u128(1),
                service: uuid::Uuid::from_u128(2),
            },
            BTreeMap::from([("NPM_TOKEN".into(), value)]),
        )
    }

    fn events() -> (tempfile::TempDir, Events) {
        let directory = tempfile::tempdir().unwrap();
        let file = fs::File::create(directory.path().join("events")).unwrap();
        (directory, Events(Some(std::sync::Mutex::new(file))))
    }

    /// The build output the events file carries.
    fn written(directory: &tempfile::TempDir) -> String {
        fs::read_to_string(directory.path().join("events"))
            .unwrap()
            .lines()
            .map(|line| {
                let event: Value = serde_json::from_str(line).unwrap();
                let bytes: Vec<u8> =
                    serde_json::from_value(event.pointer("/event/Build/Output").unwrap().clone())
                        .unwrap();
                String::from_utf8(bytes).unwrap()
            })
            .collect()
    }

    #[test]
    fn a_failed_fetch_reaches_the_events_file_as_a_fixed_line() {
        let (directory, events) = events();
        let machine = std::io::Error::other("the Machine did not open them: machine detail");
        let error = covering(Err(machine.into()), &sealed_record(), &events).unwrap_err();
        assert!(error.to_string().contains("machine detail"), "{error}");
        let written = written(&directory);
        assert!(written.contains(SEALED_UNREAD), "{written}");
        assert!(!written.contains("machine detail"), "{written}");
    }

    #[test]
    fn a_fetch_missing_a_value_names_only_the_variable() {
        let (directory, events) = events();
        let opened = OpenedSealed::new(
            BTreeMap::from([("OTHER".into(), MARKER.into())]),
            Vec::new(),
        );
        covering(Ok(opened), &sealed_record(), &events).unwrap_err();
        let written = written(&directory);
        assert!(written.contains("NPM_TOKEN"), "{written}");
        assert!(!written.contains(MARKER), "{written}");
    }

    #[cfg(target_os = "linux")]
    mod terminated {
        use std::time::Duration;

        use ployz_core::ManagementIdentity;

        use super::*;
        use crate::build::{BuildSpec, Recipe};

        const CHILD: &str = "PLOYZ_BUILD_TERM_CHILD";

        /// Every capture under `tmp` and the Sealed files staged in it.
        fn staged(tmp: &Path) -> (usize, usize) {
            let captures = fs::read_dir(tmp)
                .unwrap()
                .flatten()
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("ployz-build-")
                })
                .collect::<Vec<_>>();
            let sealed = captures
                .iter()
                .filter(|capture| capture.path().join("private/sealed/NPM_TOKEN").exists())
                .count();
            (captures.len(), sealed)
        }

        #[test]
        fn a_sigterm_mid_build_unwinds_its_staged_values_and_capture() {
            use std::os::unix::fs::PermissionsExt as _;
            let directory = tempfile::tempdir().unwrap();
            let bin = directory.path().join("bin");
            let tmp = directory.path().join("tmp");
            fs::create_dir_all(&bin).unwrap();
            fs::create_dir_all(&tmp).unwrap();
            // The solve hangs until it is stopped; teardown succeeds, so the
            // builder's state is confirmed and not quarantined.
            fs::write(
                bin.join("docker"),
                r#"#!/bin/sh
    case "$1 $2" in
      'context show') echo default ;;
      'info --format') echo '{"DriverStatus":[["driver-type","io.containerd.snapshotter.v1"]],"Architecture":"amd64","OSType":"linux","CgroupVersion":"2"}' ;;
      'buildx ls') echo "{\"Name\":\"ployz-$(id -u)\",\"Nodes\":[{\"Status\":\"running\",\"Platforms\":[\"linux/amd64\"]}]}" ;;
      'buildx bake') echo $$ >> "$0.pids"; exec sleep 60 ;;
    esac
    exit 0
    "#,
            )
            .unwrap();
            fs::set_permissions(bin.join("docker"), fs::Permissions::from_mode(0o700)).unwrap();
            let path = format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            );
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "handlers::build::tests::terminated::a_build_runs_until_its_process_is_terminated",
                    "--exact",
                    "--ignored",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("PATH", path)
                .env("TMPDIR", &tmp)
                .spawn()
                .unwrap();
            let pids = bin.join("docker.pids");
            let mut waited = Duration::ZERO;
            while fs::read_to_string(&pids).map_or(true, |pids| !pids.ends_with('\n')) {
                assert!(
                    waited < Duration::from_secs(30),
                    "the child never started its build"
                );
                // real-wait: the Docker stand-in is another process; its start emits no event to await
                std::thread::sleep(Duration::from_millis(20));
                waited += Duration::from_millis(20);
            }
            assert_eq!(staged(&tmp), (1, 1), "the build staged its Sealed value");
            Command::new("kill")
                .args(["-TERM", &child.id().to_string()])
                .status()
                .unwrap();
            let mut waited = Duration::ZERO;
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                assert!(
                    waited < Duration::from_secs(30),
                    "the build ignored SIGTERM"
                );
                // real-wait: the child is another process; its exit emits no event to await
                std::thread::sleep(Duration::from_millis(50));
                waited += Duration::from_millis(50);
            };
            assert!(status.success(), "{status}");
            assert_eq!(staged(&tmp), (0, 0));
            let pids = fs::read_to_string(&pids).unwrap();
            for pid in pids.lines() {
                let alive = Command::new("kill").args(["-0", pid]).status().unwrap();
                assert!(!alive.success(), "docker {pid} outlived the build");
            }
        }

        #[test]
        #[ignore = "child of a_sigterm_mid_build_unwinds_its_staged_values_and_capture"]
        fn a_build_runs_until_its_process_is_terminated() {
            assert!(std::env::var_os(CHILD).is_some());
            let (cancellation, _sigterm) =
                crate::cancellation::interrupted_or_terminated().unwrap();
            let mut intent = crate::build::tests::intent(&["api"]);
            let record = sealed_record();
            let container = &mut intent.target.first_mut().unwrap().container;
            container.environment.extend(record.values().clone());
            container.sealed_identity = Some(record.identity().clone());
            let source = tempfile::tempdir().unwrap();
            fs::write(
                source.path().join("Dockerfile"),
                "FROM scratch\nRUN --mount=type=secret,id=NPM_TOKEN true\n",
            )
            .unwrap();
            let spec = BuildSpec {
                context: source.path().to_owned(),
                recipe: Recipe::Dockerfile(source.path().join("Dockerfile")),
            };
            let opened = OpenedSealed::new(
                BTreeMap::from([("NPM_TOKEN".into(), MARKER.into())]),
                Vec::new(),
            );
            let name = intent.target.first().unwrap().name.clone();
            let build =
                crate::build::capture(&intent, BTreeMap::from([(name, spec)]), Some(opened))
                    .unwrap();
            let grant = BuildGrant::new(ManagementIdentity::from_bytes([7; 32]), [3; 32]);
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let built = runtime.block_on(run(build, &grant, Arc::new(Events(None)), &cancellation));
            assert!(built.is_err());
        }
    }
}
