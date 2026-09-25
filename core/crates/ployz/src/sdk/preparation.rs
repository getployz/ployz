//! Cloud's frozen configuration and checked-out paths enter the CLI capture seam here.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use super::prepare::BuildPreference;
use crate::build::{BuildSpec, BuiltService, CapturedBuild, Recipe};
use ployz_core::{
    DeployIntent, RpcError, RpcErrorCode, ServiceName,
    config::{BuildMethod, ServiceBuildConfig, ServiceSource},
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
    /// This build's position among its attempt's builds. Builds whose cache
    /// holder cannot build spread across Machines by it.
    #[serde(default)]
    pub build_index: usize,
    /// The Service's Preferred Machine, tried before any other Machine.
    #[serde(default)]
    pub preferred_machine: Option<ployz_core::MachineId>,
}

/// The one Git Service's frozen deployment, the commit a build would build, and
/// its latest receipt, if any: what a Builder outside the Cluster must do.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutsideBuildInput {
    pub deployment: Value,
    pub commit: String,
    #[serde(default)]
    pub receipt: Option<BuildReceipt>,
}

/// Private build evidence, independent of deployment success or current image availability.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildReceipt {
    pub fingerprint: String,
    pub image: ployz_build::BuiltImage,
    pub machine_id: ployz_core::MachineId,
}

pub(crate) struct CapturedPreparation {
    pub intent: DeployIntent,
    pub build: CapturedBuild,
    pub fingerprints: BTreeMap<ServiceName, String>,
    pub reusable: Vec<BuiltService>,
    pub preference: BuildPreference,
}

pub(super) fn receipts(
    fingerprints: &BTreeMap<ServiceName, String>,
    builds: &[BuiltService],
) -> BTreeMap<ServiceName, BuildReceipt> {
    fingerprints
        .iter()
        .filter_map(|(name, fingerprint)| {
            let build = builds.iter().find(|build| &build.name == name)?;
            Some((
                name.clone(),
                BuildReceipt {
                    fingerprint: fingerprint.clone(),
                    image: build.built.clone(),
                    machine_id: build.machine_id,
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

/// Capture authorized checkouts as Builds.
pub(crate) fn capture(mut input: PreparationInput) -> Result<CapturedPreparation, RpcError> {
    for receipt in input.build_receipts.values() {
        if !ployz_core::is_lower_hex(&receipt.fingerprint, 64)
            || !receipt
                .image
                .reference
                .strip_prefix("sha256:")
                .is_some_and(|digest| ployz_core::is_lower_hex(digest, 64))
        {
            return Err(invalid(
                "build receipt must identify immutable image content",
            ));
        }
    }
    let frozen = freeze(input.deployment, &mut input.source_commits)?;
    let mut builds = BTreeMap::new();
    for (name, (root_dir, settings)) in frozen.checkouts {
        let repository = input
            .sources
            .remove(&name)
            .ok_or_else(|| invalid(format!("missing checkout for {name}")))?;
        let repository = repository
            .canonicalize()
            .map_err(|_| invalid("checkout directory is unavailable"))?;
        let context = contained(&repository, &repository, &root_dir)?;
        if !context.is_dir() {
            return Err(invalid("source root must be a directory"));
        }
        let recipe = match settings.build_method {
            BuildMethod::Dockerfile => {
                let dockerfile = settings.dockerfile_path.as_deref().unwrap_or("Dockerfile");
                let dockerfile = contained(&repository, &context, dockerfile)?;
                if !dockerfile.is_file() {
                    return Err(invalid("Dockerfile must be a file"));
                }
                Recipe::Dockerfile(dockerfile)
            }
            BuildMethod::Railpack => Recipe::Railpack {
                command: settings.command,
            },
        };
        builds.insert(name, BuildSpec { context, recipe });
    }
    if !input.sources.is_empty() {
        return Err(invalid("checkout supplied for a non-Git service"));
    }
    let intent = frozen.intent;
    let build = crate::build::capture(&intent, builds).map_err(invalid)?;
    // The latest receipt names the warm Machine even when its image is stale.
    // ponytail: one preference per call; a multi-Service prepare builds on one
    // Machine, so it follows its first target's cache holder.
    let preference = BuildPreference {
        cache_holder: build.targets().find_map(|target| {
            input
                .build_receipts
                .iter()
                .find(|(name, _)| name.as_str() == target.name)
                .map(|(_, receipt)| receipt.machine_id)
        }),
        build_index: input.build_index,
        preferred: input.preferred_machine,
    };
    let fingerprints = fingerprints(&intent, frozen.identities);
    let reusable = intent
        .target
        .iter()
        .filter_map(|service| {
            let receipt = input.build_receipts.remove(&service.name)?;
            if fingerprints.get(&service.name) != Some(&receipt.fingerprint)
                || receipt.image.platforms.is_empty()
            {
                return None;
            }
            Some(BuiltService {
                name: service.name.clone(),
                machine_id: receipt.machine_id,
                image: service.container.image.clone(),
                placement: service.placement.clone(),
                built: receipt.image,
                _retention: None,
            })
        })
        .collect();
    Ok(CapturedPreparation {
        intent,
        build,
        fingerprints,
        reusable,
        preference,
    })
}

/// A frozen deployment lowered with each Git Service's source replaced by a pending
/// image, plus what its checkout and fingerprint need.
struct Frozen {
    intent: DeployIntent,
    /// Root directory and build settings of each Git Service, for its checkout.
    checkouts: BTreeMap<ServiceName, (String, ServiceBuildConfig)>,
    identities: BTreeMap<ServiceName, Value>,
}

fn freeze(
    mut deployment: Value,
    source_commits: &mut BTreeMap<ServiceName, String>,
) -> Result<Frozen, RpcError> {
    let snapshots = deployment
        .get_mut("snapshots")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| invalid("deployment snapshots must be an array"))?;
    let mut checkouts = BTreeMap::new();
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
            if let Some(commit) = source_commits.remove(name) {
                if !ployz_core::is_lower_hex(&commit, 40) {
                    return Err(invalid("source commit must be a lowercase Git SHA"));
                }
                identities.insert(name.clone(), json!({
                    "version": 1, "sdk": VERSION, "buildkit": ployz_build::BUILDKIT_IMAGE,
                    "repository": repository_id, "commit": commit, "root": root_dir, "build": config.settings.build,
                }));
            }
            checkouts.insert(
                name.clone(),
                (root_dir.clone(), config.settings.build.clone()),
            );
            // This tag never escapes preparation: binding replaces it with verified content.
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
    if !source_commits.is_empty() {
        return Err(invalid("checkout supplied for a non-Git service"));
    }
    let intent =
        ployz_core::config::lower_deployment(serde_json::from_value(deployment).map_err(invalid)?)
            .map_err(invalid)?;
    Ok(Frozen {
        intent,
        checkouts,
        identities,
    })
}

/// sha256 of each build's identity (source, recipe, ployz version) and the container
/// environment its build variables come from.
fn fingerprints(
    intent: &DeployIntent,
    mut identities: BTreeMap<ServiceName, Value>,
) -> BTreeMap<ServiceName, String> {
    intent
        .target
        .iter()
        .filter_map(|service| {
            let identity = identities.remove(&service.name)?;
            let bytes = serde_json::to_vec(&(identity, &service.container.environment))
                .expect("build identity serializes");
            Some((service.name.clone(), hex::encode(Sha256::digest(bytes))))
        })
        .collect()
}

/// The ployz version every fingerprint covers; a runner must install exactly this one.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What `capture` would fingerprint for these pinned commits, without any checkout:
/// the fingerprint Cloud hands a runner to build against.
/// # Errors
/// Rejects an invalid deployment or a commit for a non-Git Service.
pub fn expected_fingerprints(
    deployment: Value,
    mut source_commits: BTreeMap<ServiceName, String>,
) -> Result<BTreeMap<ServiceName, String>, RpcError> {
    let frozen = freeze(deployment, &mut source_commits)?;
    Ok(fingerprints(&frozen.intent, frozen.identities))
}

/// The Deploy Intent `capture` would build for, without any checkout.
/// # Errors
/// Rejects an invalid deployment.
pub(crate) fn frozen_intent(deployment: Value) -> Result<DeployIntent, RpcError> {
    Ok(freeze(deployment, &mut BTreeMap::new())?.intent)
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
#[path = "preparation_tests.rs"]
mod flow_tests;

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
            build_index: 0,
            preferred_machine: None,
        })
        .unwrap();
        assert!(captured.build.targets().next().is_none());
        let dependencies = captured.intent.dependencies();
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
    fn expected_fingerprints_refuse_an_invalid_deployment_or_commit() {
        let web = ServiceName::parse("web").unwrap();
        let commit = |value: &str| BTreeMap::from([(web.clone(), value.to_owned())]);
        let error = expected_fingerprints(json!({"projectName": "app"}), commit(&"a".repeat(40)))
            .unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{error:?}");
        let deployment = json!({"projectName": "app", "snapshots": [{"config": {
            "version": 2, "privateDns": "web", "healthcheck": {"type":"none"}, "restartPolicy":"on-failure",
            "source": {"version":2, "type":"git", "repository":"acme/web", "repositoryId":42,
                "access":{"type":"public"}, "rootDir":"/", "branch":{"type":"connected", "name":"main"}},
            "build":{"buildMethod":"dockerfile", "dockerfilePath":"Dockerfile", "command":null}
        }}]});
        for refused in ["A".repeat(40), "abc".into()] {
            let error = expected_fingerprints(deployment.clone(), commit(&refused)).unwrap_err();
            assert_eq!(
                error.code,
                RpcErrorCode::InvalidArgument,
                "{refused}: {error:?}"
            );
        }
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
                "build":{"buildMethod":"dockerfile", "dockerfilePath":"Dockerfile", "command":null}
            }}]},
            "sources":{"web":root.path()}, "source_commits":{"web":"a".repeat(40)}
        });
        let fingerprint = |input| {
            capture(serde_json::from_value(input).unwrap())
                .unwrap()
                .fingerprints
        };
        let expected = fingerprint(base.clone());
        // Cloud computes the same fingerprint without a checkout.
        let web = ServiceName::parse("web").unwrap();
        assert_eq!(
            expected_fingerprints(
                base.get("deployment").unwrap().clone(),
                BTreeMap::from([(web.clone(), "a".repeat(40))])
            )
            .unwrap()
            .get(&web),
            expected.get(&web)
        );
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
