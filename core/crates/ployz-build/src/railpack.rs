//! Pinned Railpack preparation feeding the same Buildx solve and image result.

use crate::{BuildError, Docker, Output, Request, Streams};
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

/// Preparation runs the frontend's CLI here: mise resolves Python versions with
/// a bash script, and the Alpine frontend image has no bash.
const PREPARATION_IMAGE: &str = "buildpack-deps:trixie-curl@sha256:3dd78c060b4d7c97b426d0160f7d8876c84fbf2fc36e651e9c75620ac9a7da24";

/// Preparation's own bridge. The `ployz-build` prefix puts it behind the same
/// host firewall as build steps: internet only.
const NETWORK: &str = "ployz-build-p";

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
    /// Sealed values by name, each as a fingerprint of its ciphertext. The
    /// Builder opens them into files; only the fingerprint reaches BuildKit.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sealed: BTreeMap<String, String>,
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
    builder: &str,
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
            .chain(target.is_native().then_some(native))
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
            &["buildx", "prune", "--builder", builder, "--all", "--force"],
            Streams::Inherited,
        )?;
    }
    let cli = preparation.directory.join("railpack");
    docker.with_container(&format!("{builder}-railpack"), &[IMAGE], |name| {
        docker.run(
            "copy the Railpack CLI",
            &["cp", &format!("{name}:/railpack"), &cli.to_string_lossy()],
            Streams::Captured,
        )
    })?;
    for (index, recipe) in request.railpack.iter().enumerate() {
        let directory = preparation.directory.join(index.to_string());
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(input_error)?;
        let script = directory.join("prepare.sh");
        let inputs = frontend_inputs(recipe, &directory, request.working_dir)?;
        private_file(&script, inputs.script.as_bytes())?;
        let plan = directory.join("plan.json");
        prepare_container(
            docker,
            &request.working_dir.join(&recipe.context),
            &cli,
            &script,
            &plan,
            builder,
            resources,
        )?;
        targets.insert(
            recipe.name.replace('.', "_"),
            json!({
                "dockerfile": plan,
                "platforms": [request.targets.iter().find(|t| t.name == recipe.name).and_then(|t| t.platforms.first()).map_or(native, String::as_str)],
                "args": {"BUILDKIT_SYNTAX": IMAGE, "secrets-hash": inputs.hash},
                "secret": inputs.secrets,
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

/// What one recipe hands the frontend: the prepare script, the secret
/// mounts, and the digest BuildKit sees in place of the values.
struct FrontendInputs {
    script: String,
    secrets: Vec<String>,
    hash: String,
}

/// Text values ride in the script's environment and as secret files written
/// here. Sealed names ride only as mounts of the files the host opened, with
/// an empty placeholder so the plan still lists them.
fn frontend_inputs(
    recipe: &crate::Railpack,
    directory: &Path,
    working_dir: &Path,
) -> Result<FrontendInputs, BuildError> {
    let mut command = String::from("#!/bin/sh\nexec env --");
    let mut secrets = Vec::new();
    for (index, (name, value)) in recipe.variables.iter().enumerate() {
        if !crate::valid_name(name) || value.contains('\0') {
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
    for name in recipe.sealed.keys() {
        if !crate::valid_name(name) || recipe.variables.contains_key(name) {
            return Err(BuildError::Request(
                "Railpack sealed variables require distinct valid names".into(),
            ));
        }
        command.push(' ');
        command.push_str(&quote(&format!("{name}=")));
        let file = working_dir.join(crate::sealed_file(name));
        secrets.push(format!("id={name},src={}", file.display()));
    }
    command.push_str(" /railpack prepare /app --plan-out /plan.json");
    for name in recipe.variables.keys().chain(recipe.sealed.keys()) {
        command.push_str(" --env=");
        command.push_str(&quote(name));
    }
    command.push('\n');
    // Length-delimited, sorted JSON prevents ambiguous concatenations. Only
    // the digest reaches BuildKit metadata; values travel as secret mounts.
    // A recipe without Sealed values hashes as before, keeping its cache.
    let encoded = if recipe.sealed.is_empty() {
        serde_json::to_vec(&recipe.variables)
    } else {
        serde_json::to_vec(&json!({"sealed": recipe.sealed, "variables": recipe.variables}))
    }
    .map_err(|_| BuildError::Request("invalid Railpack build variables".into()))?;
    let hash = Sha256::digest(encoded)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(FrontendInputs {
        script: command,
        secrets,
        hash,
    })
}

fn prepare_container(
    docker: &Docker<'_>,
    context: &Path,
    cli: &Path,
    script: &Path,
    plan: &Path,
    builder: &str,
    resources: &crate::policy::Resources,
) -> Result<(), BuildError> {
    let name = format!("{builder}-prepare");
    network(docker)?;
    let mut arguments = vec![
        "--entrypoint".into(),
        "/bin/sh".into(),
        "--network".into(),
        NETWORK.into(),
    ];
    arguments.extend(resources.preparation_arguments());
    arguments.extend([PREPARATION_IMAGE.into(), "/prepare.sh".into()]);
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
                "copy the Railpack CLI into preparation",
                &["cp", &cli.to_string_lossy(), &format!("{name}:/railpack")],
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

/// Create the preparation bridge once; a concurrent Build may create it first.
fn network(docker: &Docker<'_>) -> Result<(), BuildError> {
    let inspect = || {
        docker.run(
            "inspect the Railpack preparation network",
            &["network", "inspect", "--format", "{{.Name}}", NETWORK],
            Streams::Captured,
        )
    };
    if inspect().is_ok() {
        return Ok(());
    }
    let bridge = format!("com.docker.network.bridge.name={NETWORK}");
    docker
        .run(
            "create the Railpack preparation network",
            &[
                "network", "create", "--driver", "bridge", "--opt", &bridge, NETWORK,
            ],
            Streams::Captured,
        )
        .or_else(|error| inspect().map_err(|_| error))
        .map(|_| ())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe(variables: &[(&str, &str)], sealed: &[(&str, &str)]) -> crate::Railpack {
        let pairs = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect()
        };
        crate::Railpack {
            name: "api".into(),
            context: "source/app".into(),
            variables: pairs(variables),
            sealed: pairs(sealed),
            refresh_cache: false,
        }
    }

    fn inputs(recipe: &crate::Railpack) -> FrontendInputs {
        let directory =
            std::env::temp_dir().join(format!("ployz-railpack-inputs-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let inputs = frontend_inputs(recipe, &directory, Path::new("/capture")).unwrap();
        fs::remove_dir_all(&directory).unwrap();
        inputs
    }

    #[test]
    fn sealed_names_ride_only_as_mounts_of_the_opened_files() {
        let inputs = inputs(&recipe(
            &[("PLAIN", "plain-value")],
            &[("NPM_TOKEN", "fp-ciphertext")],
        ));
        let prefix = inputs.script.split(" /railpack ").next().unwrap();
        assert!(prefix.contains("'PLAIN=plain-value'"), "{prefix}");
        assert!(prefix.contains("'NPM_TOKEN='"), "{prefix}");
        assert!(
            !inputs.script.contains("fp-ciphertext"),
            "{}",
            inputs.script
        );
        assert!(
            inputs.script.ends_with("--env='PLAIN' --env='NPM_TOKEN'\n"),
            "{}",
            inputs.script
        );
        let [plain, sealed] = inputs.secrets.as_slice() else {
            panic!("{:?}", inputs.secrets);
        };
        assert!(plain.starts_with("id=PLAIN,src="), "{plain}");
        assert_eq!(
            *sealed,
            format!(
                "id=NPM_TOKEN,src=/capture/{}",
                crate::sealed_file("NPM_TOKEN").display()
            )
        );
    }

    #[test]
    fn the_secrets_hash_follows_the_fingerprint_and_never_the_values() {
        let plain = inputs(&recipe(&[("PLAIN", "plain-value")], &[]));
        let sealed = inputs(&recipe(
            &[("PLAIN", "plain-value")],
            &[("NPM_TOKEN", "fp-1")],
        ));
        let changed = inputs(&recipe(
            &[("PLAIN", "plain-value")],
            &[("NPM_TOKEN", "fp-2")],
        ));
        assert_ne!(plain.hash, sealed.hash);
        assert_ne!(sealed.hash, changed.hash);
        let expected: String = Sha256::digest(
            serde_json::to_vec(&recipe(&[("PLAIN", "plain-value")], &[]).variables).unwrap(),
        )
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
        assert_eq!(
            plain.hash, expected,
            "a recipe without Sealed values keeps trunk's hash"
        );
        for hash in [&plain.hash, &sealed.hash] {
            assert!(!hash.contains("plain-value") && hash.len() == 64);
        }
    }

    #[test]
    fn a_sealed_name_cannot_shadow_a_text_variable_or_escape_the_directory() {
        let directory =
            std::env::temp_dir().join(format!("ployz-railpack-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        for recipe in [
            recipe(&[("TOKEN", "x")], &[("TOKEN", "fp")]),
            recipe(&[], &[("../escape", "fp")]),
        ] {
            assert!(frontend_inputs(&recipe, &directory, Path::new("/capture")).is_err());
        }
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn preparation_sits_behind_the_build_bridge_firewall() {
        assert!(super::NETWORK.starts_with(crate::BUILD_BRIDGE_PREFIX));
        assert!(
            super::NETWORK.len() <= 15,
            "Linux interface names are 15 bytes"
        );
        assert!(
            super::NETWORK[crate::BUILD_BRIDGE_PREFIX.len()..]
                .parse::<u8>()
                .is_err(),
            "a slot's bridge never takes the preparation bridge's name"
        );
    }
}
