//! Cloud's frozen configuration and checked-out paths enter the CLI capture seam here.
use std::{
    collections::{BTreeMap, BTreeSet},
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

/// An Uploaded Source's content digest: lowercase hex sha256 of its paths, bytes,
/// modes and links.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct UploadDigest(String);

impl UploadDigest {
    /// # Errors
    /// Returns `invalid_argument` unless `value` is a lowercase sha256.
    pub fn parse(value: impl Into<String>) -> Result<Self, RpcError> {
        let value = value.into();
        if !ployz_core::is_lower_hex(&value, 64) {
            return Err(invalid("upload digest must be a lowercase sha256"));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for UploadDigest {
    type Error = RpcError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<UploadDigest> for String {
    fn from(digest: UploadDigest) -> Self {
        digest.0
    }
}

impl std::fmt::Display for UploadDigest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Backend-only frozen settings and repository directories, keyed by runtime Service name.
#[derive(Default, Deserialize)]
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
    pub uploads: BTreeMap<ServiceName, UploadDigest>,
    /// Previous completed images to try, in order: the Service's own first, then
    /// other Environments' images of the same build inputs. Preparation verifies
    /// each one's availability.
    #[serde(default)]
    pub build_receipts: BTreeMap<ServiceName, Vec<BuildReceipt>>,
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
    /// sha256 of the build's identity and the values of its `variables`.
    pub fingerprint: String,
    /// Which of the Service's variables the build read.
    pub variables: BuildVariables,
    pub image: ployz_build::BuiltImage,
    pub machine_id: ployz_core::MachineId,
}

/// Which variables are a build's inputs: a variable it never reads can change
/// without a rebuild.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BuildVariables {
    /// Every variable: Railpack reads them all, and a Git Service's fingerprint is
    /// fixed before its checkout exists (a GitHub build's grant carries it).
    All,
    /// An uploaded Dockerfile build's declared `ARG`s; Docker's predefined proxy and
    /// `BUILDKIT_` arguments count too.
    Declared(BTreeSet<String>),
}

impl BuildVariables {
    /// The `ARG` names `dockerfile` declares, in every stage.
    fn declared(dockerfile: &str) -> Self {
        let mut names = BTreeSet::new();
        for line in dockerfile.lines() {
            let mut words = line.split_whitespace();
            if words
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case("ARG"))
            {
                names.extend(words.filter_map(|word| {
                    let name = word.split('=').next()?;
                    (!name.is_empty()).then(|| name.to_owned())
                }));
            }
        }
        Self::Declared(names)
    }

    fn reads(&self, name: &str) -> bool {
        const PREDEFINED: [&str; 5] = [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "FTP_PROXY",
            "NO_PROXY",
            "ALL_PROXY",
        ];
        match self {
            Self::All => true,
            Self::Declared(names) => {
                names.contains(name)
                    || PREDEFINED.contains(&name.to_ascii_uppercase().as_str())
                    || name.starts_with("BUILDKIT_")
            }
        }
    }
}

pub(crate) struct CapturedPreparation {
    pub intent: DeployIntent,
    pub build: CapturedBuild,
    /// Each Service to build: its fingerprint and the variables it reads.
    pub fingerprints: BTreeMap<ServiceName, (String, BuildVariables)>,
    /// Every image that may serve a Service, in the order to try them.
    pub reusable: Vec<BuiltService>,
    /// The receipts behind `reusable`: a reused image keeps its own fingerprint.
    pub reused: BTreeMap<ServiceName, Vec<BuildReceipt>>,
    pub preference: BuildPreference,
}

/// A receipt for each of `builds`: a reused image keeps the receipt it came with.
pub(crate) fn receipts(
    fingerprints: &BTreeMap<ServiceName, (String, BuildVariables)>,
    reused: &BTreeMap<ServiceName, Vec<BuildReceipt>>,
    builds: &[BuiltService],
) -> BTreeMap<ServiceName, BuildReceipt> {
    builds
        .iter()
        .filter_map(|build| {
            // Preparation may still rebuild a reused image that misses a platform.
            let reused = reused.get(&build.name).and_then(|candidates| {
                candidates
                    .iter()
                    .find(|reused| reused.image == build.built)
            });
            let receipt = match reused {
                Some(reused) => BuildReceipt {
                    machine_id: build.machine_id,
                    ..reused.clone()
                },
                None => {
                    let (fingerprint, variables) = fingerprints.get(&build.name)?;
                    BuildReceipt {
                        fingerprint: fingerprint.clone(),
                        variables: variables.clone(),
                        image: build.built.clone(),
                        machine_id: build.machine_id,
                    }
                }
            };
            Some((build.name.clone(), receipt))
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
    for receipt in input.build_receipts.values().flatten() {
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
    let mut variables = BTreeMap::new();
    let mut sourceless = Vec::new();
    for (name, (root_dir, settings)) in frozen.checkouts {
        let Some(repository) = input.sources.remove(&name) else {
            // An upload, or a commit built elsewhere (GitHub): only its receipt can
            // serve it now; checked below.
            if frozen.identities.contains_key(&name) {
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
            if crate::build::content_digest(&repository).map_err(invalid)? != digest.as_str() {
                return Err(invalid(format!(
                    "the source for {name} is not the uploaded content {digest}"
                )));
            }
        }
        let context = contained(&repository, &repository, &root_dir)?;
        if !context.is_dir() {
            return Err(invalid("source root must be a directory"));
        }
        let mut reads = BuildVariables::All;
        let recipe = match settings.build_method {
            BuildMethod::Dockerfile => {
                let dockerfile = settings.dockerfile_path.as_deref().unwrap_or("Dockerfile");
                let dockerfile = contained(&repository, &context, dockerfile)?;
                if !dockerfile.is_file() {
                    return Err(invalid("Dockerfile must be a file"));
                }
                if frozen.uploaded.contains_key(&name) {
                    let text = std::fs::read_to_string(&dockerfile)
                        .map_err(|_| invalid("Dockerfile is unreadable"))?;
                    reads = BuildVariables::declared(&text);
                }
                Recipe::Dockerfile(dockerfile)
            }
            BuildMethod::Railpack => Recipe::Railpack {
                command: settings.command,
            },
        };
        variables.insert(name.clone(), reads);
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
                .and_then(|(_, receipts)| receipts.first())
                .map(|receipt| receipt.machine_id)
        }),
        build_index: input.build_index,
        preferred: input.preferred_machine,
    };
    // A receipt may serve a Service when the build inputs it read are unchanged;
    // preparation then takes the first whose image is still usable.
    let mut reused = BTreeMap::new();
    for service in &intent.target {
        let Some(identity) = frozen.identities.get(&service.name) else {
            continue;
        };
        let matching: Vec<_> = input
            .build_receipts
            .remove(&service.name)
            .unwrap_or_default()
            .into_iter()
            .filter(|receipt| {
                !receipt.image.platforms.is_empty()
                    && receipt.fingerprint == fingerprint(identity, service, &receipt.variables)
            })
            .collect();
        if !matching.is_empty() {
            reused.insert(service.name.clone(), matching);
        }
    }
    let reusable = intent
        .target
        .iter()
        .flat_map(|service| {
            reused
                .get(&service.name)
                .into_iter()
                .flatten()
                .map(|receipt| BuiltService {
                    name: service.name.clone(),
                    machine_id: receipt.machine_id,
                    image: service.container.image.clone(),
                    placement: service.placement.clone(),
                    built: receipt.image.clone(),
                    _retention: None,
                })
        })
        .collect::<Vec<BuiltService>>();
    let fingerprints = intent
        .target
        .iter()
        .filter_map(|service| {
            let reads = variables.remove(&service.name)?;
            let identity = frozen.identities.get(&service.name)?;
            Some((
                service.name.clone(),
                (fingerprint(identity, service, &reads), reads),
            ))
        })
        .collect();
    let (uploads, commits): (Vec<ServiceName>, Vec<ServiceName>) = sourceless
        .into_iter()
        .filter(|name| !reusable.iter().any(|built| &built.name == name))
        .partition(|name| frozen.uploaded.contains_key(name));
    if let Some(name) = commits.first() {
        return Err(invalid(format!("missing checkout for {name}")));
    }
    if !uploads.is_empty() {
        return Err(upload_needed(&uploads));
    }
    Ok(CapturedPreparation {
        intent,
        build,
        fingerprints,
        reusable,
        reused,
        preference,
    })
}

/// Why preparation needs something only the user can give, as its error details say.
#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Needed {
    UploadNeeded { services: Vec<ServiceName> },
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
        details: json!({"preparation": Needed::UploadNeeded { services: services.to_vec() }}),
    }
}

/// The uploaded Services `error` says need a new upload, if that's what it says.
pub(crate) fn needs_upload(error: &RpcError) -> Option<Vec<ServiceName>> {
    match Needed::deserialize(error.details.get("preparation")?).ok()? {
        Needed::UploadNeeded { services } => Some(services),
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
    uploaded: BTreeMap<ServiceName, UploadDigest>,
}

fn freeze(
    mut deployment: Value,
    source_commits: &mut BTreeMap<ServiceName, String>,
    uploads: &mut BTreeMap<ServiceName, UploadDigest>,
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

/// sha256 of a build's identity (source, recipe, ployz version) and the values of
/// the variables it reads: its build inputs.
fn fingerprint(
    identity: &Value,
    service: &ployz_core::RequestedServiceSpec,
    variables: &BuildVariables,
) -> String {
    let read: BTreeMap<_, _> = service
        .container
        .environment
        .iter()
        .filter(|(name, _)| variables.reads(name))
        .collect();
    digest(&(identity, read))
}

fn digest(value: &impl Serialize) -> String {
    let bytes = serde_json::to_vec(value).expect("build identity serializes");
    hex::encode(Sha256::digest(bytes))
}

/// The ployz version every fingerprint covers; a runner must install exactly this one.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What `capture` fingerprints for these pinned commits, without any checkout: the
/// fingerprint Cloud hands a runner to build against.
/// # Errors
/// Rejects an invalid deployment, or a commit for a non-Git Service.
pub fn expected_fingerprints(
    deployment: Value,
    mut source_commits: BTreeMap<ServiceName, String>,
) -> Result<BTreeMap<ServiceName, String>, RpcError> {
    let frozen = freeze(deployment, &mut source_commits, &mut BTreeMap::new())?;
    Ok(frozen
        .intent
        .target
        .iter()
        .filter_map(|service| {
            let identity = frozen.identities.get(&service.name)?;
            Some((
                service.name.clone(),
                fingerprint(identity, service, &BuildVariables::All),
            ))
        })
        .collect())
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
        let error = expected_fingerprints(json!({"namespace": "app"}), commit(&"a".repeat(40)))
            .unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{error:?}");
        let deployment = json!({"namespace": "app", "snapshots": [{"config": {
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
                .into_iter()
                .map(|(name, (fingerprint, _))| (name, fingerprint))
                .collect::<BTreeMap<_, _>>()
        };
        let expected = fingerprint(base.clone());
        // Cloud computes the same fingerprint without a checkout.
        let web = ServiceName::parse("web").unwrap();
        assert_eq!(
            expected_fingerprints(
                base.get("deployment").unwrap().clone(),
                BTreeMap::from([(web.clone(), "a".repeat(40))]),
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
        invalid_receipt.as_object_mut().unwrap().insert("build_receipts".into(), json!({"web": [{
            "fingerprint": "a".repeat(64), "variables": "all", "machine_id": "a".repeat(32),
            "image": {"reference":"mutable:latest", "tags":[], "platforms":["linux/amd64"], "location":"unused"}
        }]}));
        assert!(capture(serde_json::from_value(invalid_receipt).unwrap()).is_err());
    }

    #[test]
    #[expect(
        clippy::indexing_slicing,
        reason = "Fixed fixtures; a missing entry must fail the test."
    )]
    fn an_uploaded_source_has_its_own_identity_and_needs_its_content_or_a_receipt() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Dockerfile"),
            "FROM scratch\nARG API_URL\n",
        )
        .unwrap();
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
        let with_env = |env: Value| {
            let mut changed = empty.clone();
            changed["snapshots"][0]["resolvedEnv"] = env;
            changed
        };
        let uploaded = capture(input(&empty, true, None, Some(&digest))).unwrap();
        assert_eq!(uploaded.build.targets().count(), 1);
        let (fingerprint, variables) = uploaded.fingerprints[&web].clone();
        assert_eq!(
            variables,
            BuildVariables::Declared(BTreeSet::from(["API_URL".to_owned()]))
        );
        // Git receipts stay separate: the same bytes as a commit never match.
        let commit = "a".repeat(40);
        let clean = capture(input(&git, true, Some(&commit), None)).unwrap();
        assert_ne!(clean.fingerprints[&web].0, fingerprint);
        let dirty = capture(input(&git, true, None, Some(&digest))).unwrap();
        assert_eq!(
            dirty.fingerprints[&web].0, fingerprint,
            "the base commit is provenance only"
        );
        // A variable the Dockerfile declares is a build input; any other isn't.
        let captured = |deployment: &Value| {
            capture(input(deployment, true, None, Some(&digest)))
                .unwrap()
                .fingerprints[&web]
                .0
                .clone()
        };
        assert_eq!(
            captured(&with_env(json!({"TOKEN": "runtime"}))),
            fingerprint
        );
        assert_ne!(
            captured(&with_env(json!({"API_URL": "changed"}))),
            fingerprint
        );
        // The source must hold exactly the uploaded content.
        std::fs::write(root.path().join("extra"), "edit").unwrap();
        let error = capture(input(&empty, true, None, Some(&digest)))
            .err()
            .unwrap();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{error:?}");
        // Without its content, only a matching receipt serves it: its own first, then
        // a borrowed one.
        let error = capture(input(&empty, false, None, Some(&digest)))
            .err()
            .unwrap();
        assert_eq!(error.code, RpcErrorCode::NotFound, "{error:?}");
        assert_eq!(error.details["preparation"]["kind"], "upload_needed");
        let receipt = |fingerprint: &str| -> BuildReceipt {
            serde_json::from_value(json!({"fingerprint": fingerprint,
                "variables": {"declared": ["API_URL"]}, "machine_id": "a".repeat(32),
                "image": {"reference": format!("sha256:{}", "1".repeat(64)), "tags": [],
                    "platforms": ["linux/amd64"], "location": "unused"}}))
            .unwrap()
        };
        let sourceless = |deployment: &Value, receipts: &[&str]| {
            let mut sourceless = input(deployment, false, None, Some(&digest));
            sourceless.build_receipts = BTreeMap::from([(
                web.clone(),
                receipts
                    .iter()
                    .map(|fingerprint| receipt(fingerprint))
                    .collect(),
            )]);
            capture(sourceless)
        };
        let stale = "f".repeat(64);
        assert_eq!(
            sourceless(&empty, &[&stale]).err().unwrap().code,
            RpcErrorCode::NotFound
        );
        let reused = sourceless(&empty, &[&stale, &stale, &fingerprint]).unwrap();
        assert_eq!(reused.build.targets().count(), 0);
        assert_eq!(reused.reusable.len(), 1);
        assert_eq!(
            reused.reused[&web][0].fingerprint, fingerprint,
            "a reused image keeps the receipt it came with"
        );
        // A runtime variable change still reuses it; a changed build input needs the upload.
        let runtime = with_env(json!({"TOKEN": "changed"}));
        assert_eq!(
            sourceless(&runtime, &[&fingerprint])
                .unwrap()
                .reusable
                .len(),
            1
        );
        let build_input = with_env(json!({"API_URL": "changed"}));
        assert_eq!(
            sourceless(&build_input, &[&fingerprint])
                .err()
                .unwrap()
                .code,
            RpcErrorCode::NotFound
        );
        // A commit built elsewhere (GitHub) needs no checkout while its receipt matches.
        let mut built = input(&git, false, Some(&commit), None);
        built.build_receipts = BTreeMap::from([(
            web.clone(),
            vec![BuildReceipt {
                variables: BuildVariables::All,
                ..receipt(&clean.fingerprints[&web].0)
            }],
        )]);
        assert_eq!(capture(built).unwrap().reusable.len(), 1);
        assert_eq!(
            expected_fingerprints(git.clone(), BTreeMap::from([(web.clone(), commit.clone())]))
                .unwrap()[&web],
            clean.fingerprints[&web].0,
            "Cloud computes a Git Service's fingerprint without its checkout"
        );
        let unbuilt = capture(input(&git, false, Some(&commit), None))
            .err()
            .unwrap();
        assert_eq!(unbuilt.code, RpcErrorCode::InvalidArgument, "{unbuilt:?}");
        // Refused: a bad digest, a commit and an upload together, an upload for an image.
        assert!(
            serde_json::from_value::<PreparationInput>(json!({
                "deployment": empty, "sources": {}, "uploads": {"web": "abc"},
            }))
            .is_err()
        );
        let image = service(json!({"type":"image", "version":1, "image":"nginx",
            "credentials":{"type":"none"}}));
        for refused in [
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
