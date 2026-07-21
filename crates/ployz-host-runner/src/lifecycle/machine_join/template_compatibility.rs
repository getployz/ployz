//! Private compatibility for promoting stored machine-join templates.

use std::path::Path;

use ployz_core::install::{
    ExactPloyzVersion, InstallArtifactSpec, MachineJoinBundle, MachineJoinClusterName,
    MachineJoinMaterial, MachineJoinRuntimeNatsUrl, MachineJoinSubstrateRelease,
    MachineJoinTemplate, MachineJoinTrustedNats, WrappedCaKey, WrappedCoreSeeds,
};
use ployz_core::network::MachineEndpointSupernet;
use ployz_core::operation::FailureMessage;
use serde::Deserialize;

use crate::execution::{FileMode, write_durable_file};

const PLOYZD_INSTALL_PATH: &str = "/usr/local/bin/ployzd";
const EBPF_BYTECODE_INSTALL_PATH: &str = "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc";
const EBPF_CTL_INSTALL_PATH: &str = "/usr/local/bin/ployz-ebpf-ctl";
const RAILPACK_INSTALL_PATH: &str = "/usr/local/lib/ployz/railpack/v0.31.0/railpack";

pub(in crate::lifecycle) fn promote_machine_join_template_release(
    path: &Path,
    target_version: &ExactPloyzVersion,
) -> Result<(), FailureMessage> {
    let contents = match std::fs::read(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(failure_message(format!(
                "failed to read machine join template {}: {error}",
                path.display()
            )));
        }
    };
    let mut template = match serde_json::from_slice::<MachineJoinTemplate>(&contents) {
        Ok(template) => template,
        Err(current_error) => {
            let legacy = serde_json::from_slice::<LegacyMachineJoinTemplate>(&contents).map_err(
                |legacy_error| {
                    failure_message(format!(
                        "machine join template {} is neither the current shape ({current_error}) nor the supported legacy shape ({legacy_error})",
                        path.display()
                    ))
                },
            )?;
            legacy.into_current(target_version.clone())?
        }
    };
    template.join_bundle.material.substrate_release.version = target_version.clone();
    write_template(path, &template)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyMachineJoinTemplate {
    join_bundle: LegacyMachineJoinBundle,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyMachineJoinBundle {
    material: LegacyMachineJoinMaterial,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyMachineJoinMaterial {
    cluster_name: MachineJoinClusterName,
    dataplane_endpoint_supernet: MachineEndpointSupernet,
    runtime_nats_url: MachineJoinRuntimeNatsUrl,
    trusted_nats: MachineJoinTrustedNats,
    recovery_key_wrapped: WrappedCaKey,
    core_seeds_wrapped: WrappedCoreSeeds,
    ployzd: InstallArtifactSpec,
    ebpf_bytecode: InstallArtifactSpec,
    ebpf_ctl: InstallArtifactSpec,
    railpack: InstallArtifactSpec,
}

impl LegacyMachineJoinTemplate {
    fn into_current(
        self,
        target_version: ExactPloyzVersion,
    ) -> Result<MachineJoinTemplate, FailureMessage> {
        let LegacyMachineJoinMaterial {
            cluster_name,
            dataplane_endpoint_supernet,
            runtime_nats_url,
            trusted_nats,
            recovery_key_wrapped,
            core_seeds_wrapped,
            ployzd,
            ebpf_bytecode,
            ebpf_ctl,
            railpack,
        } = self.join_bundle.material;
        let ployz_version = ployzd.version.as_str();
        if ebpf_bytecode.version.as_str() != ployz_version
            || ebpf_ctl.version.as_str() != ployz_version
        {
            return Err(failure_message(
                "legacy machine join template Ployz artifact versions disagree",
            ));
        }
        for (name, artifact, expected_path) in [
            ("ployzd", &ployzd, PLOYZD_INSTALL_PATH),
            ("eBPF bytecode", &ebpf_bytecode, EBPF_BYTECODE_INSTALL_PATH),
            ("eBPF controller", &ebpf_ctl, EBPF_CTL_INSTALL_PATH),
            ("Railpack", &railpack, RAILPACK_INSTALL_PATH),
        ] {
            if artifact.install_path.as_str() != expected_path {
                return Err(failure_message(format!(
                    "legacy machine join template {name} install path must be {expected_path}"
                )));
            }
        }
        Ok(MachineJoinTemplate {
            join_bundle: MachineJoinBundle {
                material: MachineJoinMaterial {
                    cluster_name,
                    dataplane_endpoint_supernet,
                    runtime_nats_url,
                    trusted_nats,
                    recovery_key_wrapped,
                    core_seeds_wrapped,
                    substrate_release: MachineJoinSubstrateRelease {
                        version: target_version,
                    },
                },
            },
        })
    }
}

fn write_template(path: &Path, template: &MachineJoinTemplate) -> Result<(), FailureMessage> {
    let mut rendered = serde_json::to_vec_pretty(template).map_err(|error| {
        failure_message(format!(
            "failed to encode promoted machine join template {}: {error}",
            path.display()
        ))
    })?;
    rendered.push(b'\n');
    let Some(directory) = path.parent() else {
        return Err(failure_message(format!(
            "machine join template path {} has no parent directory",
            path.display()
        )));
    };
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err(failure_message(format!(
            "machine join template path {} has no UTF-8 file name",
            path.display()
        )));
    };
    write_durable_file(directory, file_name, FileMode::Plain, &rendered)
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message).expect("machine join template failure message is non-empty")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ployz_core::install::{ExactPloyzVersion, MachineJoinTemplate};

    use super::promote_machine_join_template_release;

    #[test]
    fn absent_machine_join_template_is_not_a_core_and_is_ignored() {
        let root = tempfile::tempdir().expect("tempdir");
        let version = ExactPloyzVersion::try_new("v0.0.2-alpha.88").expect("exact version");

        promote_machine_join_template_release(&root.path().join("missing.json"), &version)
            .expect("missing template is a no-op");
    }

    #[test]
    fn current_machine_join_template_promotes_only_its_release_identity() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("machine-join-template.json");
        let original = ployz_test_support::fixtures::machine_join_template();
        fs::write(
            &path,
            serde_json::to_vec_pretty(&original).expect("template serializes"),
        )
        .expect("write template");
        let version = ExactPloyzVersion::try_new("v0.0.2-alpha.88").expect("exact version");

        promote_machine_join_template_release(&path, &version).expect("template promotes");

        let promoted: MachineJoinTemplate =
            serde_json::from_slice(&fs::read(&path).expect("read promoted template"))
                .expect("promoted template is strict current shape");
        let mut expected = original;
        expected.join_bundle.material.substrate_release.version = version;
        assert_eq!(promoted, expected);
    }

    #[test]
    fn deployed_legacy_template_promotes_to_release_identity_without_artifact_urls() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("machine-join-template.json");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&legacy_template()).expect("legacy template serializes"),
        )
        .expect("write legacy template");
        let version = ExactPloyzVersion::try_new("v0.0.2-alpha.88").expect("exact version");

        promote_machine_join_template_release(&path, &version).expect("legacy template promotes");

        let rendered = fs::read_to_string(&path).expect("read promoted template");
        let promoted: MachineJoinTemplate =
            serde_json::from_str(&rendered).expect("promoted template is strict current shape");
        let material = promoted.join_bundle.material;
        assert_eq!(material.cluster_name.as_str(), "prod");
        assert_eq!(
            material.dataplane_endpoint_supernet.as_string(),
            "10.198.0.0/16"
        );
        assert_eq!(
            material.runtime_nats_url.as_str(),
            "tls://203.0.113.10:4222"
        );
        assert_eq!(
            material.trusted_nats.ca_pem.as_str(),
            "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n"
        );
        assert_eq!(material.recovery_key_wrapped.as_bytes(), &[1, 2, 3]);
        assert_eq!(material.core_seeds_wrapped.as_bytes(), &[4, 5, 6]);
        assert_eq!(material.substrate_release.version, version);
        assert!(!rendered.contains("linux-amd64"));
        assert!(!rendered.contains("sha256"));
    }

    #[test]
    fn incoherent_legacy_template_fails_without_replacing_the_file() {
        let mut legacy = legacy_template();
        *legacy
            .pointer_mut("/join_bundle/material/ebpf_ctl/version")
            .expect("fixture carries eBPF controller version") =
            serde_json::json!("v0.0.2-alpha.85");

        assert_invalid_template_is_unchanged(legacy, "artifact versions disagree");
    }

    #[test]
    fn unknown_legacy_fields_fail_without_replacing_the_file() {
        let mut legacy = legacy_template();
        legacy
            .pointer_mut("/join_bundle/material")
            .and_then(serde_json::Value::as_object_mut)
            .expect("fixture carries join material")
            .insert("unexpected".to_owned(), serde_json::json!(true));

        assert_invalid_template_is_unchanged(legacy, "neither the current shape");
    }

    #[test]
    fn noncanonical_legacy_railpack_path_fails_without_replacing_the_file() {
        let mut legacy = legacy_template();
        *legacy
            .pointer_mut("/join_bundle/material/railpack/install_path")
            .expect("fixture carries Railpack install path") = serde_json::json!("/tmp/railpack");

        assert_invalid_template_is_unchanged(legacy, "Railpack install path");
    }

    fn assert_invalid_template_is_unchanged(template: serde_json::Value, message: &str) {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("machine-join-template.json");
        let original = serde_json::to_vec_pretty(&template).expect("legacy template serializes");
        fs::write(&path, &original).expect("write legacy template");
        let version = ExactPloyzVersion::try_new("v0.0.2-alpha.88").expect("exact version");

        let error = promote_machine_join_template_release(&path, &version)
            .expect_err("invalid legacy template fails");

        assert!(error.as_str().contains(message));
        assert_eq!(fs::read(path).expect("read unchanged template"), original);
    }

    fn legacy_template() -> serde_json::Value {
        let artifact = |version: &str, name: &str, install_path: &str| {
            serde_json::json!({
                "version": version,
                "source": format!("https://example.invalid/linux-amd64/{name}"),
                "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "install_path": install_path,
            })
        };
        serde_json::json!({
            "join_bundle": {
                "material": {
                    "cluster_name": "prod",
                    "dataplane_endpoint_supernet": "10.198.0.0/16",
                    "runtime_nats_url": "tls://203.0.113.10:4222",
                    "trusted_nats": {
                        "ca_pem": "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n"
                    },
                    "recovery_key_wrapped": [1, 2, 3],
                    "core_seeds_wrapped": [4, 5, 6],
                    "ployzd": artifact(
                        "v0.0.2-alpha.86",
                        "ployzd",
                        "/usr/local/bin/ployzd"
                    ),
                    "ebpf_bytecode": artifact(
                        "v0.0.2-alpha.86",
                        "ployz-ebpf-tc",
                        "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"
                    ),
                    "ebpf_ctl": artifact(
                        "v0.0.2-alpha.86",
                        "ployz-ebpf-ctl",
                        "/usr/local/bin/ployz-ebpf-ctl"
                    ),
                    "railpack": artifact(
                        "0.31.0",
                        "railpack",
                        "/usr/local/lib/ployz/railpack/v0.31.0/railpack"
                    )
                }
            }
        })
    }
}
