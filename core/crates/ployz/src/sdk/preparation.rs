//! Cloud's frozen configuration and checked-out paths enter the CLI capture seam here.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::compose::{
    BuildOptions, BuildSpec, CapturedBuild, CapturedCompose, ComposeProject, LoadOptions,
};
use ployz_core::{
    RpcError, RpcErrorCode, ServiceName,
    config::{ServiceBuilder, ServiceSource},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Backend-only frozen settings and repository directories, keyed by runtime Service name.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationInput {
    pub deployment: Value,
    pub sources: BTreeMap<ServiceName, PathBuf>,
    /// Git commits pinned by the source owner, keyed by runtime Service name.
    #[serde(default)]
    pub source_commits: BTreeMap<ServiceName, String>,
    /// Previous completed images are hints; preparation verifies their availability.
    #[serde(default)]
    pub build_receipts: BTreeMap<ServiceName, BuildReceipt>,
}

/// Private build evidence, independent of deployment success or current image availability.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildReceipt {
    pub fingerprint: String,
    pub image: ployz_build::BuiltImage,
    pub machine_id: ployz_core::MachineId,
}

pub(super) struct CapturedPreparation {
    pub candidate: CapturedCompose,
    pub build: Option<CapturedBuild>,
    pub fingerprints: BTreeMap<ServiceName, String>,
    pub reusable: Vec<crate::compose::BuiltService>,
}

pub(super) fn receipts(
    fingerprints: &BTreeMap<ServiceName, String>,
    builds: &[crate::compose::BuiltService],
) -> BTreeMap<ServiceName, BuildReceipt> {
    fingerprints
        .iter()
        .filter_map(|(name, fingerprint)| {
            let build = builds.iter().find(|build| build.name == name.as_str())?;
            let crate::compose::BuildLocation::Machine(machine_id) = build.location else {
                return None;
            };
            Some((
                name.clone(),
                BuildReceipt {
                    fingerprint: fingerprint.clone(),
                    image: build.built.clone(),
                    machine_id,
                },
            ))
        })
        .collect()
}

fn invalid(message: impl ToString) -> RpcError {
    RpcError {
        code: RpcErrorCode::InvalidArgument,
        message: message.to_string(),
        details: Value::Null,
    }
}

/// Capture authorized checkouts using exactly the existing Compose build machinery.
pub(super) fn capture(mut input: PreparationInput) -> Result<CapturedPreparation, RpcError> {
    for receipt in input.build_receipts.values() {
        if !lower_hex(&receipt.fingerprint, 64)
            || !receipt
                .image
                .reference
                .strip_prefix("sha256:")
                .is_some_and(|digest| lower_hex(digest, 64))
        {
            return Err(invalid(
                "build receipt must identify immutable image content",
            ));
        }
    }
    let snapshots = input
        .deployment
        .get_mut("snapshots")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| invalid("deployment snapshots must be an array"))?;
    let mut builds = BTreeMap::new();
    let mut identities = BTreeMap::new();
    for snapshot in snapshots {
        let config = ployz_core::config::parse_service_config(snapshot["config"].clone())
            .map_err(invalid)?;
        if let ServiceSource::Git {
            repository_id,
            root_dir,
            ..
        } = &config.settings.source
        {
            let name = &config.settings.private_dns;
            if let Some(commit) = input.source_commits.remove(name) {
                if !lower_hex(&commit, 40) {
                    return Err(invalid("source commit must be a lowercase Git SHA"));
                }
                identities.insert(name.clone(), json!({
                    "version": 1, "sdk": env!("CARGO_PKG_VERSION"), "buildkit": ployz_build::BUILDKIT_IMAGE,
                    "repository": repository_id, "commit": commit, "root": root_dir, "build": config.settings.build,
                }));
            }
            let repository = input
                .sources
                .remove(name)
                .ok_or_else(|| invalid(format!("missing checkout for {name}")))?;
            let repository = repository
                .canonicalize()
                .map_err(|_| invalid("checkout directory is unavailable"))?;
            let context = contained(&repository, &repository, root_dir)?;
            if !context.is_dir() {
                return Err(invalid("source root must be a directory"));
            }
            let mut build = json!({ "context": context, "x-recipe": match config.settings.build.builder {
                ServiceBuilder::Dockerfile => "dockerfile",
                ServiceBuilder::Railpack => "railpack",
            }});
            if config.settings.build.builder == ServiceBuilder::Dockerfile {
                let dockerfile = config
                    .settings
                    .build
                    .dockerfile_path
                    .as_deref()
                    .unwrap_or("Dockerfile");
                let dockerfile = contained(&repository, &context, dockerfile)?;
                if !dockerfile.is_file() {
                    return Err(invalid("Dockerfile must be a file"));
                }
                build
                    .as_object_mut()
                    .expect("build object")
                    .insert("dockerfile".into(), json!(dockerfile));
            }
            if config.settings.build.builder == ServiceBuilder::Railpack
                && let Some(command) = &config.settings.build.command
            {
                build
                    .as_object_mut()
                    .expect("build object")
                    .insert("args".into(), json!({"RAILPACK_BUILD_CMD": command}));
            }
            builds.insert(
                name.to_string(),
                BuildSpec {
                    raw: serde_norway::to_value(build).map_err(invalid)?,
                },
            );
            // This tag never escapes preparation: bind_builds replaces it with verified content.
            snapshot
                .get_mut("config")
                .and_then(Value::as_object_mut)
                .expect("validated config object")
                .insert(
                    "source".into(),
                    json!({"type":"image", "version":1,
                "image":format!("ployz-build/{name}:pending"), "credentials":{"type":"none"}}),
                );
        }
    }
    if !input.sources.is_empty() || !input.source_commits.is_empty() {
        return Err(invalid("checkout supplied for a non-Git service"));
    }
    let intent = ployz_core::config::lower_deployment(
        serde_json::from_value(input.deployment).map_err(invalid)?,
    )
    .map_err(invalid)?;
    let dependencies = intent
        .dependencies()
        .iter()
        .map(|(name, edges)| (name.to_string(), edges.clone()))
        .collect();
    let services = intent
        .target
        .into_iter()
        .map(|s| (s.name.to_string(), s))
        .collect::<BTreeMap<_, _>>();
    // Explicit empty provider environment prevents capture from reading Cloud's HOME,
    // Docker credentials or process variables. Runtime variables are already resolved.
    let project = ComposeProject::from_frozen_services(
        intent.project_name.to_string(),
        services,
        builds,
        dependencies,
    );
    let (candidate, build) = crate::preparation::capture(
        project,
        intent.project_name,
        intent.options,
        &LoadOptions::default(),
        &BuildOptions::default(),
        false,
        None,
    )
    .map_err(invalid)?;
    let fingerprints: BTreeMap<_, _> = candidate
        .intent()
        .target
        .iter()
        .filter_map(|service| {
            let identity = identities.remove(&service.name)?;
            let bytes = serde_json::to_vec(&(identity, &service.container.environment))
                .expect("build identity serializes");
            Some((service.name.clone(), hex::encode(Sha256::digest(bytes))))
        })
        .collect();
    let reusable = candidate
        .intent()
        .target
        .iter()
        .filter_map(|service| {
            let receipt = input.build_receipts.remove(&service.name)?;
            if fingerprints.get(&service.name) != Some(&receipt.fingerprint)
                || receipt.image.platforms.is_empty()
            {
                return None;
            }
            Some(crate::compose::BuiltService {
                name: service.name.to_string(),
                image: service.container.image.clone(),
                placement: service.placement.clone(),
                location: crate::compose::BuildLocation::Machine(receipt.machine_id),
                built: receipt.image,
                _retention: None,
            })
        })
        .collect();
    Ok(CapturedPreparation {
        candidate,
        build,
        fingerprints,
        reusable,
    })
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn contained(root: &Path, base: &Path, setting: &str) -> Result<PathBuf, RpcError> {
    let path = base
        .join(setting.trim_start_matches('/'))
        .canonicalize()
        .map_err(|_| invalid("source path does not exist"))?;
    if !path.starts_with(root) {
        return Err(invalid("source path escapes its repository root"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_preserves_cloud_dependencies() {
        let deployment = json!({
            "projectName": "app",
            "snapshots": (["web", "db"].map(|name| json!({"config": {
                "version": 2, "privateDns": name,
                "healthcheck": {"type":"none"}, "restartPolicy":"on-failure",
                "source": {"type": "image", "version": 1, "image": "nginx:latest", "credentials": {"type": "none"}}
            }}))),
            "dependencies": {"web": [{"service": "db", "condition": "service_started"}]}
        });
        let captured = capture(PreparationInput {
            deployment,
            sources: BTreeMap::new(),
            source_commits: BTreeMap::new(),
            build_receipts: BTreeMap::new(),
        })
        .unwrap();
        assert!(captured.build.is_none());
        let dependencies = captured.candidate.intent().dependencies();
        assert_eq!(
            dependencies
                .get(&ServiceName::parse("web").unwrap())
                .unwrap()
                .first()
                .unwrap()
                .service
                .as_str(),
            "db"
        );
    }

    #[test]
    fn build_identity_tracks_source_recipe_and_variables_but_not_runtime_settings() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        std::fs::create_dir(root.path().join("app")).unwrap();
        std::fs::write(root.path().join("app/Dockerfile"), "FROM scratch\n").unwrap();
        let base = json!({
            "deployment": {"projectName": "app", "snapshots": [{"config": {
                "version": 2, "privateDns": "web", "healthcheck": {"type":"none"}, "restartPolicy":"on-failure",
                "source": {"version":2, "type":"git", "repository":"acme/web", "repositoryId":42,
                    "access":{"type":"public"}, "rootDir":"/", "branch":{"type":"connected", "name":"main"}},
                "build":{"builder":"dockerfile", "dockerfilePath":"Dockerfile", "command":null}
            }}]},
            "sources":{"web":root.path()}, "source_commits":{"web":"a".repeat(40)}
        });
        let fingerprint = |input| {
            capture(serde_json::from_value(input).unwrap())
                .unwrap()
                .fingerprints
        };
        let expected = fingerprint(base.clone());
        for (pointer, value) in [
            ("/deployment/snapshots/0/config/replicas", json!(2)),
            (
                "/deployment/snapshots/0/config/startCommand",
                json!("serve"),
            ),
            (
                "/deployment/snapshots/0/resolvedEnv",
                json!({"PORT":"8080"}),
            ),
        ] {
            let mut changed = base.clone();
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            changed
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(key.into(), value);
            assert_eq!(fingerprint(changed), expected, "{pointer}");
        }
        for (pointer, value) in [
            ("/source_commits/web", json!("b".repeat(40))),
            (
                "/deployment/snapshots/0/config/source/repositoryId",
                json!(43),
            ),
            (
                "/deployment/snapshots/0/config/source/rootDir",
                json!("/app"),
            ),
            (
                "/deployment/snapshots/0/config/build/dockerfilePath",
                json!("app/Dockerfile"),
            ),
            (
                "/deployment/snapshots/0/resolvedEnv",
                json!({"TOKEN":"changed"}),
            ),
        ] {
            let mut changed = base.clone();
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            changed
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(key.into(), value);
            assert_ne!(fingerprint(changed), expected, "{pointer}");
        }
        let mut invalid_commit = base.clone();
        *invalid_commit.pointer_mut("/source_commits/web").unwrap() = json!("main");
        assert!(capture(serde_json::from_value(invalid_commit).unwrap()).is_err());
        let mut invalid_receipt = base;
        invalid_receipt.as_object_mut().unwrap().insert("build_receipts".into(), json!({"web": {
            "fingerprint": "a".repeat(64), "machine_id": "a".repeat(32),
            "image": {"reference":"mutable:latest", "tags":[], "platforms":["linux/amd64"], "location":"unused"}
        }}));
        assert!(capture(serde_json::from_value(invalid_receipt).unwrap()).is_err());
    }

    #[test]
    fn repository_paths_cannot_escape_through_parent_or_symlink() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("app")).unwrap();
        std::os::unix::fs::symlink("/", temp.path().join("outside")).unwrap();
        assert_eq!(
            contained(temp.path(), temp.path(), "/app").unwrap(),
            temp.path().join("app")
        );
        std::fs::create_dir_all(temp.path().join("apps/api")).unwrap();
        std::fs::write(temp.path().join("Dockerfile"), "FROM scratch").unwrap();
        assert_eq!(
            contained(
                temp.path(),
                &temp.path().join("apps/api"),
                "../../Dockerfile"
            )
            .unwrap(),
            temp.path().join("Dockerfile")
        );
        assert!(contained(temp.path(), &temp.path().join("apps/api"), "../../../").is_err());
        assert!(contained(temp.path(), temp.path(), "../").is_err());
        assert!(contained(temp.path(), temp.path(), "outside/etc").is_err());
    }
}
