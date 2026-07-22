use super::*;
use sha2::{Digest, Sha256};

const TAG: &str = "v0.0.2-alpha.1";
const VERSION: &str = "0.0.2-alpha.1";
const RAILPACK_VERSION: &str = "v0.31.0";

struct BuildExecutorFixture {
    root: PathBuf,
    host_runner_source: PathBuf,
    railpack_source: PathBuf,
    manifest: PathBuf,
    install_dir: PathBuf,
    railpack_bin: PathBuf,
    release_env: PathBuf,
    script_path: PathBuf,
    fake_bin: PathBuf,
}

impl BuildExecutorFixture {
    fn new(name: &str, railpack_contents: &str) -> Self {
        let root = temp_dir(name);
        let host_runner_source = write_fake_host_runner(&root);
        let railpack_source = root.join("railpack-source");
        fs::write(&railpack_source, railpack_contents).expect("Railpack source can be written");
        let manifest = root.join("ployz-release-linux-amd64.env");
        let install_dir = root.join("bin");
        let railpack_bin = root.join("lib/ployz/railpack/v0.31.0/railpack");
        let release_env = root.join("release.env");
        let script_path = test_bootstrap_script_path(&root, &install_dir);
        let fake_bin = root.join("fake-bin");
        fs::create_dir_all(&fake_bin).expect("fake bin can be created");
        write_fake_tools(&fake_bin);
        Self {
            root,
            host_runner_source,
            railpack_source,
            manifest,
            install_dir,
            railpack_bin,
            release_env,
            script_path,
            fake_bin,
        }
    }

    fn write_manifest(&self, spec: &ManifestSpec) {
        let railpack_url = if spec.include_railpack_url {
            format!(
                "PLOYZ_RAILPACK_URL=file://{}\n",
                self.railpack_source.display()
            )
        } else {
            String::new()
        };
        fs::write(
            &self.manifest,
            format!(
                "PLOYZ_VERSION={}\nPLOYZ_RELEASE_TAG={}\nPLOYZ_RELEASE_PLATFORM={}\nPLOYZ_URL=file://{}\nPLOYZ_SHA256={}\nPLOYZ_RAILPACK_VERSION={}\n{railpack_url}PLOYZ_RAILPACK_SHA256={}\n",
                spec.version,
                spec.tag,
                spec.platform,
                self.host_runner_source.display(),
                spec.ployz_sha,
                spec.railpack_version,
                spec.railpack_sha,
            ),
        )
        .expect("Build Executor manifest can be written");
    }

    fn command(&self) -> Command {
        let mut command = Command::new("sh");
        command
            .arg(&self.script_path)
            .arg("--build-executor")
            .env("PATH", test_path(&self.fake_bin))
            .env(
                "PLOYZ_RELEASE_MANIFEST_URL",
                format!("file://{}", self.manifest.display()),
            )
            .env_remove("PLOYZ_VERSION")
            .env_remove("PLOYZ_CHANNEL")
            .env_remove("PLOYZ_URL")
            .env_remove("PLOYZ_SHA256")
            .env_remove("PLOYZ_RAILPACK_VERSION")
            .env_remove("PLOYZ_RAILPACK_URL")
            .env_remove("PLOYZ_RAILPACK_SHA256");
        command
    }

    fn seed_existing_installation(&self) -> [Vec<u8>; 3] {
        fs::create_dir_all(&self.install_dir).expect("install directory can be created");
        fs::create_dir_all(self.railpack_bin.parent().expect("Railpack parent"))
            .expect("Railpack directory can be created");
        fs::write(self.install_dir.join("ployz"), "existing Ployz\n")
            .expect("existing Ployz can be written");
        fs::write(&self.railpack_bin, "existing Railpack\n")
            .expect("existing Railpack can be written");
        fs::write(&self.release_env, "existing release evidence\n")
            .expect("existing release evidence can be written");
        self.installed_bytes()
    }

    fn installed_bytes(&self) -> [Vec<u8>; 3] {
        [
            fs::read(self.install_dir.join("ployz")).expect("installed Ployz is readable"),
            fs::read(&self.railpack_bin).expect("installed Railpack is readable"),
            fs::read(&self.release_env).expect("release evidence is readable"),
        ]
    }
}

struct ManifestSpec {
    platform: &'static str,
    tag: &'static str,
    version: &'static str,
    railpack_version: &'static str,
    include_railpack_url: bool,
    ployz_sha: String,
    railpack_sha: String,
}

impl ManifestSpec {
    fn valid(fixture: &BuildExecutorFixture) -> Self {
        Self {
            platform: "linux-amd64",
            tag: TAG,
            version: VERSION,
            railpack_version: RAILPACK_VERSION,
            include_railpack_url: true,
            ployz_sha: sha256_file(&fixture.host_runner_source),
            railpack_sha: sha256_file(&fixture.railpack_source),
        }
    }
}

#[test]
fn installs_verified_artifacts_at_exact_paths_and_is_idempotent() {
    let fixture = BuildExecutorFixture::new("ployz-build-executor-install", "railpack helper\n");
    fixture.write_manifest(&ManifestSpec::valid(&fixture));
    overwrite_with_real_sha256(&fixture.fake_bin);

    let output = fixture.command().output().expect("installer can run");
    assert_success(&output);
    assert_eq!(
        fs::read_to_string(fixture.install_dir.join("ployz")).expect("Ployz is installed"),
        "#!/bin/sh\nexit 0\n"
    );
    assert_eq!(
        fs::read_to_string(&fixture.railpack_bin).expect("Railpack is installed"),
        "railpack helper\n"
    );
    for (path, mode) in [
        (fixture.install_dir.join("ployz"), 0o755),
        (fixture.railpack_bin.clone(), 0o755),
        (fixture.release_env.clone(), 0o644),
    ] {
        assert_eq!(
            fs::metadata(path)
                .expect("installed metadata is readable")
                .permissions()
                .mode()
                & 0o777,
            mode
        );
    }

    let before = fixture.installed_bytes();
    assert_success(&fixture.command().output().expect("installer can rerun"));
    assert_eq!(fixture.installed_bytes(), before);
}

#[derive(Clone, Copy, Debug)]
enum PreflightFailure {
    MissingRailpackUrl,
    MalformedRailpackVersion,
    UnpinnedRailpackVersion,
    PloyzChecksum,
    RailpackChecksum,
    WrongPlatform,
    IncoherentIdentity,
    SelectedVersionMismatch,
}

#[test]
fn preflight_failures_preserve_existing_installation() {
    for failure in [
        PreflightFailure::MissingRailpackUrl,
        PreflightFailure::MalformedRailpackVersion,
        PreflightFailure::UnpinnedRailpackVersion,
        PreflightFailure::PloyzChecksum,
        PreflightFailure::RailpackChecksum,
        PreflightFailure::WrongPlatform,
        PreflightFailure::IncoherentIdentity,
        PreflightFailure::SelectedVersionMismatch,
    ] {
        let fixture = BuildExecutorFixture::new(
            &format!("ployz-build-executor-{failure:?}"),
            "new Railpack\n",
        );
        let before = fixture.seed_existing_installation();
        let mut spec = ManifestSpec::valid(&fixture);
        match failure {
            PreflightFailure::MissingRailpackUrl => spec.include_railpack_url = false,
            PreflightFailure::MalformedRailpackVersion => spec.railpack_version = "../v0.31.0",
            PreflightFailure::UnpinnedRailpackVersion => spec.railpack_version = "v0.32.0",
            PreflightFailure::PloyzChecksum => spec.ployz_sha = HOST_RUNNER_DIGEST.to_owned(),
            PreflightFailure::RailpackChecksum => {
                spec.railpack_sha = HOST_RUNNER_DIGEST.to_owned();
            }
            PreflightFailure::WrongPlatform => spec.platform = "linux-arm64",
            PreflightFailure::IncoherentIdentity => spec.tag = "v0.0.3-alpha.1",
            PreflightFailure::SelectedVersionMismatch => {}
        }
        fixture.write_manifest(&spec);
        overwrite_with_real_sha256(&fixture.fake_bin);
        let mut command = fixture.command();
        if matches!(failure, PreflightFailure::SelectedVersionMismatch) {
            command.args(["--version", "v0.0.3-alpha.1"]);
        }

        let output = command.output().expect("installer can run");
        assert!(!output.status.success(), "{failure:?} must fail");
        assert_eq!(
            fixture.installed_bytes(),
            before,
            "{failure:?} mutated files"
        );
    }
}

#[test]
fn direct_artifact_overrides_fail_before_mutation() {
    let fixture = BuildExecutorFixture::new("ployz-build-executor-overrides", "new Railpack\n");
    let before = fixture.seed_existing_installation();
    fixture.write_manifest(&ManifestSpec::valid(&fixture));
    let output = fixture
        .command()
        .env("PLOYZ_RAILPACK_URL", "https://example.invalid/railpack")
        .output()
        .expect("installer can run");
    assert!(!output.status.success());
    assert_eq!(fixture.installed_bytes(), before);
}

#[test]
fn railpack_install_failure_preserves_existing_files() {
    let fixture =
        BuildExecutorFixture::new("ployz-build-executor-install-failure", "new Railpack\n");
    let before = fixture.seed_existing_installation();
    fixture.write_manifest(&ManifestSpec::valid(&fixture));
    overwrite_with_real_sha256(&fixture.fake_bin);
    write_executable(
        &fixture.fake_bin.join("install"),
        "#!/bin/sh\nfor arg in \"$@\"; do\n  case \"$arg\" in */railpack) exit 73 ;; esac\ndone\nexec /usr/bin/install \"$@\"\n",
    );

    let output = fixture.command().output().expect("installer can run");
    assert!(!output.status.success());
    assert_eq!(fixture.installed_bytes(), before);
}

#[derive(Clone, Copy)]
enum ReleaseSelector {
    Channel,
    Version,
    Manifest,
}

#[test]
fn selectors_keep_one_release_identity_for_both_artifacts() {
    for selector in [
        ReleaseSelector::Channel,
        ReleaseSelector::Version,
        ReleaseSelector::Manifest,
    ] {
        let fixture = BuildExecutorFixture::new("ployz-build-executor-selector", "Railpack\n");
        let (tag, version) = match selector {
            ReleaseSelector::Channel => ("v0.0.3-beta.1", "0.0.3-beta.1"),
            ReleaseSelector::Version | ReleaseSelector::Manifest => (TAG, VERSION),
        };
        let mut spec = ManifestSpec::valid(&fixture);
        spec.tag = tag;
        spec.version = version;
        fixture.write_manifest(&spec);
        let channel = fixture.root.join("beta.env");
        write_channel(&channel, "beta", tag, version);
        let mut command = fixture.command();
        match selector {
            ReleaseSelector::Channel => {
                command
                    .env_remove("PLOYZ_RELEASE_MANIFEST_URL")
                    .args(["--channel", "beta"])
                    .env("PLOYZ_TEST_BETA_CHANNEL", &channel)
                    .env("PLOYZ_TEST_RELEASE_MANIFEST", &fixture.manifest);
            }
            ReleaseSelector::Version => {
                command
                    .env_remove("PLOYZ_RELEASE_MANIFEST_URL")
                    .args(["--version", TAG])
                    .env("PLOYZ_TEST_RELEASE_MANIFEST", &fixture.manifest);
            }
            ReleaseSelector::Manifest => {}
        }

        assert_success(&command.output().expect("installer can run"));
        assert_eq!(
            fs::read_to_string(&fixture.railpack_bin).expect("Railpack is installed"),
            "Railpack\n"
        );
        let evidence = fs::read_to_string(&fixture.release_env).expect("release evidence exists");
        assert!(evidence.contains(&format!("PLOYZ_RELEASE_TAG={tag}\n")));
        assert!(evidence.contains(&format!("PLOYZ_VERSION={version}\n")));
        assert!(evidence.contains("PLOYZ_RELEASE_PLATFORM=linux-amd64\n"));
    }
}

fn overwrite_with_real_sha256(fake_bin: &std::path::Path) {
    write_executable(
        &fake_bin.join("sha256sum"),
        "#!/bin/sh\nexec /usr/bin/sha256sum \"$@\"\n",
    );
}

fn sha256_file(path: &std::path::Path) -> String {
    let mut digest = Sha256::new();
    digest.update(fs::read(path).expect("artifact is readable"));
    format!("{:x}", digest.finalize())
}
