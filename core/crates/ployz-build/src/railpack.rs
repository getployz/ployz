//! Pinned Railpack preparation feeding the same Buildx solve and image result.

use crate::{BuildError, Docker, Output, Request, Streams, builder_name};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write as _,
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _},
    path::{Path, PathBuf},
};

// Railpack 0.39.0: the frontend image also ships the matching /railpack CLI.
const IMAGE: &str = "ghcr.io/railwayapp/railpack-frontend@sha256:db24dc37640b6887c3d455b40876ea30f75182964479670cba6e4cde7ffef103";

/// Private captured inputs for a Railpack target. Never a display payload.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Railpack {
    /// Buildx target name before Compose's dot normalization.
    pub name: String,
    /// Source directory relative to the capture root.
    pub context: PathBuf,
    /// Effective Service values, with build-only overrides already applied.
    pub variables: BTreeMap<String, String>,
    /// Request fresh compilation/base resolution on the pinned frontend.
    pub refresh_cache: bool,
}

/// Generated frontend inputs live for one solve, including failed preparation.
pub(crate) struct Preparation {
    directory: PathBuf,
}

impl Preparation {
    /// The Bake override consumed while this preparation remains alive.
    pub(crate) fn override_file(&self) -> PathBuf {
        self.directory.join("bake.json")
    }
}

impl Drop for Preparation {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// Prepare captured Railpack targets and retain their private inputs for the solve.
///
/// # Errors
/// Rejects unsupported options or variables and reports staging or Docker failures.
pub(crate) fn prepare(
    docker: &Docker<'_>,
    request: &Request<'_>,
    native: &str,
    resources: &crate::policy::Resources,
) -> Result<Option<Preparation>, BuildError> {
    if request.railpack.is_empty() {
        return Ok(None);
    }
    if request.output == Output::Validate {
        return Err(BuildError::Request(
            "Railpack does not support --check".into(),
        ));
    }
    for recipe in request.railpack {
        let target = request
            .targets
            .iter()
            .find(|t| t.name == recipe.name)
            .ok_or_else(|| BuildError::Request("Railpack recipe has no build target".into()))?;
        for platform in target
            .platforms
            .iter()
            .map(String::as_str)
            .chain(target.platforms.is_empty().then_some(native))
        {
            if !matches!(platform, "linux/amd64" | "linux/arm64") {
                return Err(BuildError::Request(
                    "Railpack supports only linux/amd64 and linux/arm64".into(),
                ));
            }
        }
    }
    let directory = request.working_dir.join("private/railpack");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .map_err(input_error)?;
    let preparation = Preparation { directory };
    let mut targets = serde_json::Map::new();
    if request.railpack.iter().any(|recipe| recipe.refresh_cache) {
        // ponytail: this frontend ignores no-cache/pull. Prune only the locked,
        // Ployz-owned builder; replace this cold rebuild when upstream supports them.
        docker.run(
            "refresh Railpack build cache",
            &[
                "buildx",
                "prune",
                "--builder",
                &builder_name(),
                "--all",
                "--force",
            ],
            Streams::Inherited,
        )?;
    }
    for (index, recipe) in request.railpack.iter().enumerate() {
        let directory = preparation.directory.join(index.to_string());
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(input_error)?;
        let script = directory.join("prepare.sh");
        let mut command = String::from("#!/bin/sh\nexec env --");
        let mut secrets = Vec::new();
        for (index, (name, value)) in recipe.variables.iter().enumerate() {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_+-".contains(&b))
                || value.contains('\0')
            {
                return Err(BuildError::Request(
                    "Railpack build variables require valid names and NUL-free values".into(),
                ));
            }
            command.push(' ');
            command.push_str(&quote(&format!("{name}={value}")));
            let file = directory.join(format!("secret-{index}"));
            private_file(&file, value.as_bytes())?;
            secrets.push(format!("id={name},src={}", file.display()));
        }
        command.push_str(" /railpack prepare /app --plan-out /plan.json");
        for name in recipe.variables.keys() {
            command.push_str(" --env=");
            command.push_str(&quote(name));
        }
        command.push('\n');
        private_file(&script, command.as_bytes())?;
        let plan = directory.join("plan.json");
        prepare_container(
            docker,
            &request.working_dir.join(&recipe.context),
            &script,
            &plan,
            resources,
        )?;
        // Length-delimited, sorted JSON prevents ambiguous concatenations. Only
        // the digest reaches BuildKit metadata; values travel as secret mounts.
        let encoded = serde_json::to_vec(&recipe.variables)
            .map_err(|_| BuildError::Request("invalid Railpack build variables".into()))?;
        let hash: String = Sha256::digest(encoded)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        targets.insert(
            recipe.name.replace('.', "_"),
            json!({
                "dockerfile": plan,
                "platforms": [request.targets.iter().find(|t| t.name == recipe.name).and_then(|t| t.platforms.first()).map_or(native, String::as_str)],
                "args": {"BUILDKIT_SYNTAX": IMAGE, "secrets-hash": hash},
                "secret": secrets,
            }),
        );
    }
    let override_file = preparation.override_file();
    private_file(
        &override_file,
        &serde_json::to_vec(&json!({"target": targets}))
            .map_err(|_| BuildError::Request("invalid Railpack build targets".into()))?,
    )?;
    Ok(Some(preparation))
}

fn prepare_container(
    docker: &Docker<'_>,
    context: &Path,
    script: &Path,
    plan: &Path,
    resources: &crate::policy::Resources,
) -> Result<(), BuildError> {
    let name = format!("{}-prepare", builder_name());
    let mut arguments = vec![
        "--network".into(),
        "host".into(),
        "--entrypoint".into(),
        "/bin/sh".into(),
    ];
    arguments.extend(resources.preparation_arguments());
    arguments.extend([IMAGE.into(), "/prepare.sh".into()]);
    docker.with_container(
        &name,
        &arguments.iter().map(String::as_str).collect::<Vec<_>>(),
        |name| {
            docker.run(
                "copy captured Railpack source",
                &["cp", &context.to_string_lossy(), &format!("{name}:/app")],
                Streams::Captured,
            )?;
            docker.run(
                "copy private Railpack variables",
                &[
                    "cp",
                    &script.to_string_lossy(),
                    &format!("{name}:/prepare.sh"),
                ],
                Streams::Captured,
            )?;
            docker.run(
                "Railpack preparation",
                &["start", "--attach", name],
                Streams::Inherited,
            )?;
            docker.run(
                "read the Railpack plan",
                &["cp", &format!("{name}:/plan.json"), &plan.to_string_lossy()],
                Streams::Captured,
            )?;
            Ok(())
        },
    )
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn private_file(path: &Path, content: &[u8]) -> Result<(), BuildError> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| file.write_all(content))
        .map_err(input_error)
}

fn input_error(error: std::io::Error) -> BuildError {
    BuildError::Request(format!("stage private Railpack inputs: {error}"))
}
