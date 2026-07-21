use std::fs;
use std::path::{Path, PathBuf};

use ployz_core::install::MachineJoinMaterial;
use sha2::{Digest as _, Sha256};

use crate::shell::shell_quote;
use crate::ssh::{SshClient, SshCommandError, SshPhase, SshTarget};

const ARTIFACTS: [Artifact; 4] = [
    Artifact::new("ployz", "PLOYZ"),
    Artifact::new("ployzd", "PLOYZD"),
    Artifact::new("ployz-ebpf-ctl", "PLOYZ_EBPF_CTL"),
    Artifact::new("ployz-ebpf-tc", "PLOYZ_EBPF_TC"),
];
const SUPPORT_FILES: [&str; 3] = ["ployz.sh", "install.sh", "release.env"];

#[derive(Clone, Copy)]
struct Artifact {
    name: &'static str,
    manifest_key: &'static str,
}

impl Artifact {
    const fn new(name: &'static str, manifest_key: &'static str) -> Self {
        Self { name, manifest_key }
    }

    fn url_key(self) -> String {
        format!("{}_URL", self.manifest_key)
    }

    fn digest_key(self) -> String {
        format!("{}_SHA256", self.manifest_key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalReleaseBundle {
    directory: PathBuf,
    version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalArtifact {
    sha256: String,
}

impl LocalReleaseBundle {
    pub fn open(directory: PathBuf) -> Result<Self, String> {
        let manifest_path = directory.join("release.env");
        let manifest = fs::read_to_string(&manifest_path)
            .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?;
        let version = manifest_value(&manifest, "PLOYZ_VERSION")?;
        validate_version(&version)?;
        if directory.file_name().and_then(|name| name.to_str()) != Some(version.as_str()) {
            return Err(format!(
                "bundle directory must be named {version} to match PLOYZ_VERSION"
            ));
        }
        if manifest_value(&manifest, "PLOYZ_RELEASE_TAG")? != version {
            return Err("PLOYZ_RELEASE_TAG must match PLOYZ_VERSION".into());
        }

        for name in SUPPORT_FILES {
            require_regular_file(&directory.join(name))?;
        }
        let remote_directory = remote_directory_for(&version);
        let [ployz, ployzd, ebpf_ctl, ebpf_tc] = ARTIFACTS
            .map(|artifact| validate_artifact(&directory, &manifest, &remote_directory, artifact));
        let artifacts = [ployz?, ployzd?, ebpf_ctl?, ebpf_tc?];

        let platform = manifest_value(&manifest, "PLOYZ_RELEASE_PLATFORM")?;
        let nats_version = manifest_value(&manifest, "PLOYZ_NATS_SERVER_VERSION")?;
        let nats_url = manifest_value(&manifest, "PLOYZ_NATS_SERVER_URL")?;
        let nats_sha256 = manifest_value(&manifest, "PLOYZ_NATS_SERVER_SHA256")?;
        let ployz_script_sha256 = sha256_file(&directory.join("ployz.sh"))?;
        let mut identity = Sha256::new();
        for (descriptor, artifact) in ARTIFACTS.into_iter().zip(&artifacts) {
            identity.update(format!("{} {}\n", descriptor.name, artifact.sha256));
        }
        identity.update(format!("ployz.sh {ployz_script_sha256}\n"));
        identity.update(format!("platform {platform}\n"));
        identity.update(format!("nats-version {nats_version}\n"));
        identity.update(format!("nats-url {nats_url}\n"));
        identity.update(format!("nats-sha256 {nats_sha256}\n"));
        let actual_version = format!("dev-{}", &format!("{:x}", identity.finalize())[..16]);
        if version != actual_version {
            return Err(format!(
                "PLOYZ_VERSION is {version}, expected content-derived {actual_version}"
            ));
        }

        let expected_installer = format!(
            "#!/bin/sh\nset -eu\nPLOYZ_RELEASE_MANIFEST_URL=file://{remote_directory}/release.env exec {remote_directory}/ployz.sh \"$@\"\n"
        );
        let installer_path = directory.join("install.sh");
        if fs::read_to_string(&installer_path)
            .map_err(|error| format!("cannot read {}: {error}", installer_path.display()))?
            != expected_installer
        {
            return Err("install.sh does not match the canonical local-release wrapper".into());
        }

        Ok(Self { directory, version })
    }

    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn validate_join_material(&self, material: &MachineJoinMaterial) -> Result<(), String> {
        if self.version != material.substrate_release.version.as_str() {
            return Err("local release does not match the cluster machine-join release".into());
        }
        Ok(())
    }

    pub fn stage(
        &self,
        client: &SshClient,
        target: &SshTarget,
    ) -> Result<StagedLocalRelease, LocalReleaseStageError> {
        let mut archive = tar::Builder::new(Vec::new());
        for name in ARTIFACTS
            .into_iter()
            .map(|artifact| artifact.name)
            .chain(SUPPORT_FILES)
        {
            archive
                .append_path_with_name(self.directory.join(name), name)
                .map_err(LocalReleaseStageError::Archive)?;
        }
        let archive = archive
            .into_inner()
            .map_err(LocalReleaseStageError::Archive)?;
        let remote_directory = remote_directory_for(&self.version);
        let temporary_directory = format!("{remote_directory}.tmp.{}", nuid::next());
        let command = format!(
            "{REMOTE_ROOT_HELPER} tmp={}; dest={}; cleanup() {{ if [ -d \"$tmp\" ]; then run rm -rf -- \"$tmp\"; fi; }}; trap cleanup EXIT; run install -d -o root -g root -m 0755 -- \"$tmp\"; run tar -xf - -C \"$tmp\"; run chmod -R u=rwX,go=rX \"$tmp\"; run chown -R root:root \"$tmp\"; run mv -T -n -- \"$tmp\" \"$dest\"",
            shell_quote(&temporary_directory),
            shell_quote(&remote_directory),
        );
        client
            .run_with_stdin(target, SshPhase::StageLocalRelease, &command, &archive)
            .map_err(LocalReleaseStageError::Ssh)?;
        Ok(StagedLocalRelease {
            installer_script: format!("{remote_directory}/install.sh"),
            manifest_url: format!("file://{remote_directory}/release.env"),
        })
    }
}

const REMOTE_ROOT_HELPER: &str =
    "if [ \"$(id -u)\" -eq 0 ]; then run() { \"$@\"; }; else run() { sudo \"$@\"; }; fi;";

pub struct StagedLocalRelease {
    pub installer_script: String,
    pub manifest_url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum LocalReleaseStageError {
    #[error("could not build local release archive: {0}")]
    Archive(std::io::Error),
    #[error(transparent)]
    Ssh(Box<SshCommandError>),
}

fn validate_artifact(
    directory: &Path,
    manifest: &str,
    remote_directory: &str,
    artifact: Artifact,
) -> Result<LocalArtifact, String> {
    let path = directory.join(artifact.name);
    require_regular_file(&path)?;
    let source = manifest_value(manifest, &artifact.url_key())?;
    let expected_source = format!("{remote_directory}/{}", artifact.name);
    if source != expected_source {
        return Err(format!("{} must be {expected_source}", artifact.url_key()));
    }
    let sha256 = manifest_value(manifest, &artifact.digest_key())?;
    if sha256_file(&path)? != sha256 {
        return Err(format!(
            "{} does not match {}",
            artifact.digest_key(),
            artifact.name
        ));
    }
    Ok(LocalArtifact { sha256 })
}

fn validate_version(version: &str) -> Result<(), String> {
    let Some(digest) = version.strip_prefix("dev-") else {
        return Err("PLOYZ_VERSION must be dev- followed by 16 lowercase hex characters".into());
    };
    if digest.len() == 16
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err("PLOYZ_VERSION must be dev- followed by 16 lowercase hex characters".into())
    }
}

fn require_regular_file(path: &Path) -> Result<(), String> {
    if fs::metadata(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?
        .is_file()
    {
        Ok(())
    } else {
        Err(format!("{} is not a regular file", path.display()))
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?
        )
    ))
}

fn manifest_value(contents: &str, key: &str) -> Result<String, String> {
    contents
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("release.env is missing {key}"))
}

fn remote_directory_for(version: &str) -> String {
    format!("/var/lib/ployz/dev-releases/{version}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_the_complete_content_identity_and_canonical_wrapper() {
        let root = tempfile::tempdir().expect("temp dir");
        let staging = root.path().join("staging");
        fs::create_dir(&staging).expect("staging dir");
        let artifacts = ARTIFACTS.map(|artifact| {
            let contents = format!("local {}", artifact.name);
            fs::write(staging.join(artifact.name), &contents).expect("artifact");
            (artifact, format!("{:x}", Sha256::digest(contents)))
        });
        let ployz_script = "#!/bin/sh\necho local\n";
        fs::write(staging.join("ployz.sh"), ployz_script).expect("ployz.sh");
        let platform = "linux-amd64";
        let nats_version = "2.14.2";
        let nats_url = "https://example.invalid/nats.tar.gz";
        let nats_sha256 = "a".repeat(64);
        let mut identity = Sha256::new();
        for (artifact, sha256) in &artifacts {
            identity.update(format!("{} {sha256}\n", artifact.name));
        }
        identity.update(format!(
            "ployz.sh {:x}\nplatform {platform}\nnats-version {nats_version}\nnats-url {nats_url}\nnats-sha256 {nats_sha256}\n",
            Sha256::digest(ployz_script)
        ));
        let version = format!("dev-{}", &format!("{:x}", identity.finalize())[..16]);
        let directory = root.path().join(&version);
        fs::rename(&staging, &directory).expect("name bundle");
        let remote = remote_directory_for(&version);
        fs::write(
            directory.join("install.sh"),
            format!(
                "#!/bin/sh\nset -eu\nPLOYZ_RELEASE_MANIFEST_URL=file://{remote}/release.env exec {remote}/ployz.sh \"$@\"\n"
            ),
        )
        .expect("install wrapper");
        let mut manifest = format!(
            "PLOYZ_VERSION={version}\nPLOYZ_RELEASE_TAG={version}\nPLOYZ_RELEASE_PLATFORM={platform}\n"
        );
        for (artifact, sha256) in artifacts {
            manifest.push_str(&format!(
                "{}_URL={remote}/{}\n{}_SHA256={sha256}\n",
                artifact.manifest_key, artifact.name, artifact.manifest_key
            ));
        }
        manifest.push_str(&format!(
            "PLOYZ_NATS_SERVER_VERSION={nats_version}\nPLOYZ_NATS_SERVER_URL={nats_url}\nPLOYZ_NATS_SERVER_SHA256={nats_sha256}\n"
        ));
        fs::write(directory.join("release.env"), manifest).expect("manifest");

        LocalReleaseBundle::open(directory.clone()).expect("valid bundle");
        fs::write(directory.join("ployzd"), "tampered").expect("tamper artifact");
        assert!(LocalReleaseBundle::open(directory).is_err());
    }

    #[test]
    fn join_validation_compares_only_the_exact_release_identity() {
        let bundle = LocalReleaseBundle {
            directory: PathBuf::from("/unused"),
            version: "dev-0123456789abcdef".to_owned(),
        };
        let mut material = ployz_test_support::fixtures::machine_join_material(
            "nats://127.0.0.1:7422",
            ployz_test_support::fixtures::TEST_CA_PEM,
        );
        material.substrate_release.version =
            ployz_core::install::ExactPloyzVersion::try_new("dev-0123456789abcdef")
                .expect("exact local release version");

        bundle
            .validate_join_material(&material)
            .expect("same exact release is accepted");

        material.substrate_release.version =
            ployz_core::install::ExactPloyzVersion::try_new("dev-fedcba9876543210")
                .expect("different exact local release version");
        assert!(bundle.validate_join_material(&material).is_err());
    }
}
