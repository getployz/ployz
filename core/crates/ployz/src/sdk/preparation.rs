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
    /// Uploaded Source content digests, keyed by runtime Service name: these Services
    /// build from an upload instead of a commit. With a `sources` directory it must hold
    /// exactly that content; without one only a matching, still-usable receipt serves it.
    #[serde(default)]
    pub uploads: BTreeMap<ServiceName, String>,
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
    /// An uploaded Service's fingerprint without its variables. Without the upload,
    /// a receipt of the same content still serves it after a variable change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    pub image: ployz_build::BuiltImage,
    pub machine_id: ployz_core::MachineId,
}

pub(crate) struct CapturedPreparation {
    pub intent: DeployIntent,
    pub build: CapturedBuild,
    pub fingerprints: BTreeMap<ServiceName, String>,
    /// Each uploaded Service's fingerprint without its variables.
    pub contents: BTreeMap<ServiceName, String>,
    pub reusable: Vec<BuiltService>,
    pub preference: BuildPreference,
}

pub(crate) fn receipts(
    fingerprints: &BTreeMap<ServiceName, String>,
    contents: &BTreeMap<ServiceName, String>,
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
                    content: contents.get(name).cloned(),
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
    let frozen = freeze(
        input.deployment,
        &mut input.source_commits,
        &mut input.uploads,
    )?;
    let mut builds = BTreeMap::new();
    let mut sourceless = Vec::new();
    for (name, (root_dir, settings)) in frozen.checkouts {
        let Some(repository) = input.sources.remove(&name) else {
            if frozen.uploaded.contains_key(&name) {
                // Only its receipt can serve it now; checked below.
                sourceless.push(name);
                continue;
            }
            return Err(invalid(format!("missing checkout for {name}")));
        };
        let repository = repository
            .canonicalize()
            .map_err(|_| invalid("checkout directory is unavailable"))?;
        if let Some(digest) = frozen.uploaded.get(&name) {
            // ponytail: an edit between this check and capture goes unnoticed; Cloud's
            // extracted uploads are private, so only a local runner's directory can race.
            if crate::build::content_digest(&repository).map_err(invalid)? != *digest {
                return Err(invalid(format!(
                    "the source for {name} is not the uploaded content {digest}"
                )));
            }
        }
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
    let contents = frozen
        .identities
        .iter()
        .filter(|(name, _)| frozen.uploaded.contains_key(*name))
        .map(|(name, identity)| (name.clone(), digest(identity)))
        .collect::<BTreeMap<_, _>>();
    let fingerprints = fingerprints(&intent, frozen.identities);
    let reusable = intent
        .target
        .iter()
        .filter_map(|service| {
            let receipt = input.build_receipts.remove(&service.name)?;
            // Without its upload, a Service's variables are runtime-only: its image
            // keeps the build variables it was built with.
            let same_content = sourceless.contains(&service.name)
                && receipt.content.is_some()
                && receipt.content.as_ref() == contents.get(&service.name);
            if (fingerprints.get(&service.name) != Some(&receipt.fingerprint) && !same_content)
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
        .collect::<Vec<BuiltService>>();
    let missing: Vec<ServiceName> = sourceless
        .into_iter()
        .filter(|name| !reusable.iter().any(|built| &built.name == name))
        .collect();
    if !missing.is_empty() {
        return Err(upload_needed(&missing));
    }
    Ok(CapturedPreparation {
        intent,
        build,
        fingerprints,
        contents,
        reusable,
        preference,
    })
}

/// No source and no usable image for these uploaded Services: only a new upload builds them.
pub(crate) fn upload_needed(services: &[ServiceName]) -> RpcError {
    let names = services
        .iter()
        .map(ServiceName::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    RpcError {
        code: RpcErrorCode::NotFound,
        message: format!(
            "No upload or usable image for {names}: its image is gone, or its build inputs \
             changed. Upload its source again"
        ),
        details: json!({"preparation": {"kind": "upload_needed", "services": services}}),
    }
}

/// A frozen deployment lowered with each built Service's source replaced by a pending
/// image, plus what its checkout and fingerprint need.
struct Frozen {
    intent: DeployIntent,
    /// Root directory and build settings of each Git or uploaded Service, for its source.
    checkouts: BTreeMap<ServiceName, (String, ServiceBuildConfig)>,
    identities: BTreeMap<ServiceName, Value>,
    /// Content digest of each Service built from an Uploaded Source.
    uploaded: BTreeMap<ServiceName, String>,
}

fn freeze(
    mut deployment: Value,
    source_commits: &mut BTreeMap<ServiceName, String>,
    uploads: &mut BTreeMap<ServiceName, String>,
) -> Result<Frozen, RpcError> {
    let snapshots = deployment
        .get_mut("snapshots")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| invalid("deployment snapshots must be an array"))?;
    let mut checkouts = BTreeMap::new();
    let mut identities = BTreeMap::new();
    let mut uploaded = BTreeMap::new();
    for snapshot in snapshots {
        let config = ployz_core::config::parse_service_config(snapshot["config"].clone())
            .map_err(invalid)?;
        let name = &config.settings.private_dns;
        let (root_dir, repository_id) = match &config.settings.source {
            ServiceSource::Git {
                repository_id,
                root_dir,
                ..
            } => (root_dir, Some(repository_id)),
            ServiceSource::Empty { root_dir, .. } => (root_dir, None),
            ServiceSource::Image { .. } => continue,
        };
        // Tagged, so an upload's identity never equals a commit's.
        let source = match (uploads.remove(name), repository_id) {
            (Some(digest), _) => {
                if !ployz_core::is_lower_hex(&digest, 64) {
                    return Err(invalid("upload digest must be a lowercase sha256"));
                }
                if source_commits.contains_key(name) {
                    return Err(invalid(format!(
                        "{name} builds from a commit or an upload, not both"
                    )));
                }
                uploaded.insert(name.clone(), digest.clone());
                Some(json!({"type": "uploaded", "digest": digest}))
            }
            (None, Some(repository_id)) => source_commits
                .remove(name)
                .map(|commit| {
                    if !ployz_core::is_lower_hex(&commit, 40) {
                        return Err(invalid("source commit must be a lowercase Git SHA"));
                    }
                    Ok(json!({"type": "git", "repository": repository_id, "commit": commit}))
                })
                .transpose()?,
            // An Empty Service without an upload has nothing to deploy.
            (None, None) => continue,
        };
        if let Some(source) = source {
            identities.insert(
                name.clone(),
                json!({
                    "version": 1, "sdk": VERSION, "buildkit": ployz_build::BUILDKIT_IMAGE,
                    "source": source, "root": root_dir, "build": config.settings.build,
                }),
            );
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
    if !source_commits.is_empty() {
        return Err(invalid("checkout supplied for a non-Git service"));
    }
    if !uploads.is_empty() {
        return Err(invalid("upload supplied for a Service that pulls an image"));
    }
    let intent =
        ployz_core::config::lower_deployment(serde_json::from_value(deployment).map_err(invalid)?)
            .map_err(invalid)?;
    Ok(Frozen {
        intent,
        checkouts,
        identities,
        uploaded,
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
            Some((
                service.name.clone(),
                digest(&(identity, &service.container.environment)),
            ))
        })
        .collect()
}

fn digest(value: &impl Serialize) -> String {
    let bytes = serde_json::to_vec(value).expect("build identity serializes");
    hex::encode(Sha256::digest(bytes))
}

/// The ployz version every fingerprint covers; a runner must install exactly this one.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What `capture` would fingerprint for these pinned commits and upload digests,
/// without any source: the fingerprint Cloud hands a runner to build against.
/// # Errors
/// Rejects an invalid deployment, a commit for a non-Git Service, or an upload for
/// a Service that pulls an image.
pub fn expected_fingerprints(
    deployment: Value,
    mut source_commits: BTreeMap<ServiceName, String>,
    mut uploads: BTreeMap<ServiceName, String>,
) -> Result<BTreeMap<ServiceName, String>, RpcError> {
    let frozen = freeze(deployment, &mut source_commits, &mut uploads)?;
    Ok(fingerprints(&frozen.intent, frozen.identities))
}

/// The Deploy Intent `capture` would build for, without any checkout.
/// # Errors
/// Rejects an invalid deployment.
pub(crate) fn frozen_intent(deployment: Value) -> Result<DeployIntent, RpcError> {
    Ok(freeze(deployment, &mut BTreeMap::new(), &mut BTreeMap::new())?.intent)
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
    fn capture_orders_by_cloud_references() {
        let deployment = json!({
            "namespace": "app",
            "snapshots": (["web", "db"].map(|name| json!({"serviceId": name, "config": {
                "version": 2, "privateDns": name,
                "env": if name == "web" { json!({"DB": {"kind": "literal", "value": "", "parts": [
                    {"kind": "ref", "owner": {"scope": "service", "lineageId": "db-lineage"}, "key": "PORT"}]}}) } else { json!({}) },
                "healthcheck": {"type":"none"}, "restartPolicy":"on-failure",
                "source": {"type": "image", "version": 1, "image": "nginx:latest", "credentials": {"type": "none"}}
            }}))),
            "lineages": {"db-lineage": "db"}
        });
        let captured = capture(PreparationInput {
            deployment,
            sources: BTreeMap::new(),
            source_commits: BTreeMap::new(),
            uploads: BTreeMap::new(),
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
        let error = expected_fingerprints(
            json!({"namespace": "app"}),
            commit(&"a".repeat(40)),
            BTreeMap::new(),
        )
        .unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{error:?}");
        let deployment = json!({"namespace": "app", "snapshots": [{"config": {
            "version": 2, "privateDns": "web", "healthcheck": {"type":"none"}, "restartPolicy":"on-failure",
            "source": {"version":2, "type":"git", "repository":"acme/web", "repositoryId":42,
                "access":{"type":"public"}, "rootDir":"/", "branch":{"type":"connected", "name":"main"}},
            "build":{"buildMethod":"dockerfile", "dockerfilePath":"Dockerfile", "command":null}
        }}]});
        for refused in ["A".repeat(40), "abc".into()] {
            let error =
                expected_fingerprints(deployment.clone(), commit(&refused), BTreeMap::new())
                    .unwrap_err();
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
            "deployment": {"namespace": "app", "snapshots": [{"config": {
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
                BTreeMap::from([(web.clone(), "a".repeat(40))]),
                BTreeMap::new(),
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
            (
                "/deployment/snapshots/0/setupCommands",
                json!(["seed --demo"]),
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
            assert_eq!(fingerprint(changed.clone()), expected, "{pointer}");
            assert_eq!(
                expected_fingerprints(
                    changed.get("deployment").unwrap().clone(),
                    BTreeMap::from([(web.clone(), "a".repeat(40))]),
                    BTreeMap::new(),
                )
                .unwrap(),
                expected,
                "{pointer}"
            );
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
    #[expect(
        clippy::indexing_slicing,
        reason = "Fixed fixtures; a missing entry must fail the test."
    )]
    fn an_uploaded_source_has_its_own_identity_and_needs_its_content_or_a_receipt() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        let digest = crate::build::content_digest(root.path()).unwrap();
        let web = ServiceName::parse("web").unwrap();
        let service = |source: Value| {
            json!({"namespace": "app", "snapshots": [{"config": {
                "version": 2, "privateDns": "web", "healthcheck": {"type":"none"},
                "restartPolicy":"on-failure", "source": source,
                "build":{"buildMethod":"dockerfile", "dockerfilePath":"Dockerfile", "command":null}
            }}]})
        };
        // A directory without Git metadata deploys through a Service with no source.
        let empty = service(json!({"type":"empty", "version":1, "rootDir":"/"}));
        let git = service(json!({"version":2, "type":"git", "repository":"acme/web",
            "repositoryId":42, "access":{"type":"public"}, "rootDir":"/",
            "branch":{"type":"connected", "name":"main"}}));
        let input =
            |deployment: &Value, sources: bool, commit: Option<&str>, upload: Option<&str>| {
                serde_json::from_value::<PreparationInput>(json!({
                    "deployment": deployment,
                    "sources": if sources { json!({"web": root.path()}) } else { json!({}) },
                    "source_commits": commit.map_or(json!({}), |commit| json!({"web": commit})),
                    "uploads": upload.map_or(json!({}), |digest| json!({"web": digest})),
                }))
                .unwrap()
            };
        let uploads = || BTreeMap::from([(web.clone(), digest.clone())]);
        let uploaded = capture(input(&empty, true, None, Some(&digest))).unwrap();
        assert_eq!(uploaded.build.targets().count(), 1);
        let fingerprint = uploaded.fingerprints.get(&web).unwrap().clone();
        assert_eq!(
            expected_fingerprints(empty.clone(), BTreeMap::new(), uploads()).unwrap()[&web],
            fingerprint,
            "Cloud computes the same fingerprint without the upload"
        );
        // Git receipts stay separate: the same bytes as a commit never match.
        let commit = "a".repeat(40);
        let clean = capture(input(&git, true, Some(&commit), None)).unwrap();
        assert_ne!(clean.fingerprints[&web], fingerprint);
        let dirty = capture(input(&git, true, None, Some(&digest))).unwrap();
        assert_eq!(
            dirty.fingerprints[&web], fingerprint,
            "the base commit is provenance only"
        );
        // Changed build inputs change the fingerprint.
        let mut railpack = empty.clone();
        *railpack
            .pointer_mut("/snapshots/0/config/build/buildMethod")
            .unwrap() = json!("railpack");
        let mut variable = empty.clone();
        variable["snapshots"][0]["resolvedEnv"] = json!({"TOKEN": "changed"});
        for changed in [railpack, variable] {
            assert_ne!(
                expected_fingerprints(changed, BTreeMap::new(), uploads()).unwrap()[&web],
                fingerprint
            );
        }
        // The source must hold exactly the uploaded content.
        std::fs::write(root.path().join("extra"), "edit").unwrap();
        let error = capture(input(&empty, true, None, Some(&digest)))
            .err()
            .unwrap();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{error:?}");
        // Without its content, only a matching receipt serves it.
        let error = capture(input(&empty, false, None, Some(&digest)))
            .err()
            .unwrap();
        assert_eq!(error.code, RpcErrorCode::NotFound, "{error:?}");
        assert_eq!(error.details["preparation"]["kind"], "upload_needed");
        let receipt = |fingerprint: &str| {
            json!({"web": {"fingerprint": fingerprint, "machine_id": "a".repeat(32),
                "image": {"reference": format!("sha256:{}", "1".repeat(64)), "tags": [],
                    "platforms": ["linux/amd64"], "location": "unused"}}})
        };
        let mut sourceless = input(&empty, false, None, Some(&digest));
        sourceless.build_receipts = serde_json::from_value(receipt(&"f".repeat(64))).unwrap();
        assert_eq!(
            capture(sourceless).err().unwrap().code,
            RpcErrorCode::NotFound
        );
        let mut sourceless = input(&empty, false, None, Some(&digest));
        sourceless.build_receipts = serde_json::from_value(receipt(&fingerprint)).unwrap();
        let reused = capture(sourceless).unwrap();
        assert_eq!(reused.build.targets().count(), 0);
        assert_eq!(reused.reusable.len(), 1);
        // A variable change: without the upload, a receipt of the same content serves it.
        let content = uploaded.contents[&web].clone();
        let mut variable = empty.clone();
        variable["snapshots"][0]["resolvedEnv"] = json!({"TOKEN": "changed"});
        let with_content = |content: &str| {
            let mut receipts = receipt(&fingerprint);
            receipts["web"]["content"] = json!(content);
            let mut sourceless = input(&variable, false, None, Some(&digest));
            sourceless.build_receipts = serde_json::from_value(receipts).unwrap();
            sourceless
        };
        assert_eq!(capture(with_content(&content)).unwrap().reusable.len(), 1);
        assert_eq!(
            capture(with_content(&"f".repeat(64))).err().unwrap().code,
            RpcErrorCode::NotFound
        );
        // With the upload, the variable change rebuilds.
        std::fs::remove_file(root.path().join("extra")).unwrap();
        let mut sourced = with_content(&content);
        sourced.sources = BTreeMap::from([(web.clone(), root.path().to_path_buf())]);
        let rebuilt = capture(sourced).unwrap();
        assert!(rebuilt.reusable.is_empty());
        assert_eq!(rebuilt.build.targets().count(), 1);
        // Refused: a bad digest, a commit and an upload together, an upload for an image.
        let image = service(json!({"type":"image", "version":1, "image":"nginx",
            "credentials":{"type":"none"}}));
        for refused in [
            input(&empty, false, None, Some("abc")),
            input(&git, true, Some(&commit), Some(&digest)),
            input(&image, false, None, Some(&digest)),
        ] {
            assert_eq!(
                capture(refused).err().unwrap().code,
                RpcErrorCode::InvalidArgument
            );
        }
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
