//! Railpack 0.39.0 needs separate solves and upstream OCI index assembly.
//! Primary evidence (shipped beta binaries, ARM64 under emulation):
//! https://github.com/getployz/ployz2/blob/c3ca5519a4607256ffb28052d77a1c7d89f1bbe1/prototypes/railpack-transfer/FINDINGS.md

use crate::{
    BuildError, BuiltImage, Docker, Planned, Progress, Request, Stage, Streams, TargetEvidence,
    TargetMetadata, bake_arguments, builder::Builder, builder_name,
};
use std::{collections::BTreeMap, path::Path};

// regctl 0.11.6, matching the prototype. Containerized so remote Docker also works.
const REGCTL: &str =
    "regclient/regctl@sha256:5fe7c6a6206e1d9d71af719674c9db03e74a4ae221a32089b691f5b55533d6ea";

/// Solve each requested platform, assemble its exact content, and verify the local image.
///
/// # Errors
/// Reports solve, assembly, import or content failures; cleanup failure remains uncertain.
pub(crate) fn build(
    docker: &Docker<'_>,
    builder: &Builder<'_>,
    request: &Request<'_>,
    target: &Planned<'_>,
    overrides: Option<&Path>,
) -> Result<BuiltImage, BuildError> {
    let name = format!("{}-assemble", builder_name());
    docker.with_container(
        &name,
        &[
            "--network",
            "none",
            "--entrypoint",
            "/bin/sleep",
            REGCTL,
            "infinity",
        ],
        |name| {
            docker
                .run("start image assembly", &["start", name], Streams::Captured)
                .map_err(|error| error.at(Stage::Preparation))?;
            let output = |action, arguments: &[&str], streams| {
                docker
                    .run(action, arguments, streams)
                    .map_err(|error| error.at(Stage::Output))
            };
            let regctl = |action, args: &[&str]| {
                let mut command = vec!["exec", name, "regctl"];
                command.extend_from_slice(args);
                output(action, &command, Streams::Captured)
            };
            let mut refs = Vec::new();
            let mut tags = Vec::new();
            for (number, platform) in target.target.platforms.iter().enumerate() {
                let archive = request.working_dir.join("private/railpack/variant.tar");
                let metadata = request.working_dir.join("private/railpack/variant.json");
                let mut args =
                    bake_arguments(request, std::slice::from_ref(target), &metadata, overrides);
                args.retain(|arg| arg != "--load");
                for setting in [
                    format!("{}.platform={platform}", target.bake),
                    format!("{}.output=type=oci,dest={}", target.bake, archive.display()),
                    format!("{}.attest=type=provenance,disabled=true", target.bake),
                ] {
                    args.extend(["--set".into(), setting]);
                }
                if let Some(progress) = docker.progress {
                    progress(Progress::Stage(Stage::Building));
                }
                builder.run(&args, || {
                    if let Some(progress) = docker.progress {
                        progress(Progress::Target {
                            name: target.target.name.clone(),
                            outcome: TargetEvidence::Unknown,
                        });
                    }
                })?;
                if let Some(progress) = docker.progress {
                    progress(Progress::Stage(Stage::Output));
                }
                let results: BTreeMap<String, TargetMetadata> =
                    serde_json::from_slice(&std::fs::read(&metadata).map_err(result_error)?)
                        .map_err(result_error)?;
                let result = results.get(&target.bake).ok_or_else(|| {
                    BuildError::Result("Railpack solve produced no image result".into())
                })?;
                let current_tags = result.tags();
                if number == 0 {
                    tags = current_tags;
                } else if tags != current_tags {
                    return Err(BuildError::Result(
                        "Railpack solves reported different image tags".into(),
                    ));
                }
                output(
                    "copy Railpack variant for assembly",
                    &[
                        "cp",
                        &archive.to_string_lossy(),
                        &format!("{name}:/tmp/variant.tar"),
                    ],
                    Streams::Captured,
                )?;
                let reference = format!("ocidir:///tmp/layout@{}", result.digest);
                regctl(
                    "import Railpack variant",
                    &["image", "import", &reference, "/tmp/variant.tar"],
                )?;
                refs.push(reference);
            }
            let tag = tags
                .first()
                .ok_or_else(|| BuildError::Result("Railpack solve tagged no image".into()))?;
            let mut args = vec!["index", "create", "ocidir:///tmp/layout:complete"];
            for reference in &refs {
                args.extend(["--ref", reference]);
            }
            regctl("assemble Railpack image index", &args)?;
            let digest = regctl(
                "read assembled image identity",
                &["image", "digest", "ocidir:///tmp/layout:complete"],
            )?;
            let digest = digest.trim();
            if !digest
                .strip_prefix("sha256:")
                .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                return Err(BuildError::Result(
                    "assembly returned an invalid image digest".into(),
                ));
            }
            regctl(
                "export assembled Railpack image",
                &[
                    "image",
                    "export",
                    "ocidir:///tmp/layout:complete",
                    "/tmp/complete.tar",
                    "--name",
                    tag,
                ],
            )?;
            let archive = request.working_dir.join("private/railpack/complete.tar");
            output(
                "copy assembled Railpack image",
                &[
                    "cp",
                    &format!("{name}:/tmp/complete.tar"),
                    &archive.to_string_lossy(),
                ],
                Streams::Captured,
            )?;
            output(
                "load assembled Railpack image",
                &["image", "load", "--input", &archive.to_string_lossy()],
                Streams::Captured,
            )?;
            let reference = digest.to_owned();
            let inspected = output(
                "inspect assembled Railpack image",
                &["image", "inspect", &reference, "--format", "{{json .}}"],
                Streams::Captured,
            )?;
            let image: crate::ImageInspection =
                serde_json::from_str(&inspected).map_err(result_error)?;
            if !image.descriptor.is_some_and(|d| {
                d.digest == digest && d.media_type == "application/vnd.oci.image.index.v1+json"
            }) {
                return Err(BuildError::Result(
                    "the local store does not hold the assembled Railpack index".into(),
                ));
            }
            for platform in &target.target.platforms {
                // Export forces Docker to read the manifest, config and every layer.
                // Inspect alone can list an index variant whose content is absent.
                output(
                    "verify Railpack platform content",
                    &["image", "save", "--platform", platform, &reference],
                    Streams::Discarded,
                )?;
            }
            for tag in &tags {
                output(
                    "tag completed Railpack image",
                    &["image", "tag", &reference, tag],
                    Streams::Captured,
                )?;
            }
            let image = BuiltImage {
                reference,
                tags,
                platforms: target.target.platforms.clone(),
                location: docker.location(),
            };
            if let Some(progress) = docker.progress {
                progress(Progress::Target {
                    name: target.target.name.clone(),
                    outcome: TargetEvidence::Image(image.clone()),
                });
            }
            Ok(image)
        },
    )
}

fn result_error(error: impl std::fmt::Display) -> BuildError {
    BuildError::Result(format!("read Railpack image result: {error}"))
}
