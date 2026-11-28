//! The automatic deploy rule an approver's client drafts from an Environment's Saved
//! settings, and what it shows the person before they sign. The CLI and the dashboard both
//! draft here, so a rule a passkey answers in the browser has the bytes a Device would
//! sign for the same settings.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ts_rs::TS;

use super::{
    birth::{self, BirthError, Births, MountLines, Mounts, PlannedVolumes},
    canonical::{self, CanonicalError, Timestamp, drop_nulls},
    deploy::run_fields_cleared,
    keys::{Signed, SignerKey},
    rule::{
        AutoDeployRule, BuildPolicy, Builder, GithubSource, PolicyCache, PolicyMethod,
        RegistrySource, RuleEvent, RuleId, RuleService, RuleSource, Runner, SealedDigest, template,
    },
    shown::Shown,
};
use crate::{
    EnvPart, EnvValue, MachineId, MissingSealedIdentity, Namespace, RequestedServiceSpec,
    SealedRecord, SealedScope, SealedValue, SealingPublicKey, ServiceName,
    config::ServiceBuildConfig,
};

/// The caller workflow every rule builds with, as `ployz github` sets it up.
pub const WORKFLOW: &str = ".github/workflows/ployz-build.yml";

/// Where a Saved Service builds from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct RuleSourceView {
    pub service: ServiceName,
    /// `owner/name`.
    pub repository: String,
    #[ts(type = "number")]
    pub repository_id: u64,
    /// None when its branch is disconnected.
    pub branch: Option<String>,
    pub root_dir: String,
    pub build: ServiceBuildConfig,
    /// Whether a push to its branch deploys it: its Auto-deploy is on.
    #[serde(default = "pushes")]
    pub on_push: bool,
}

const fn pushes() -> bool {
    true
}

/// A Saved Service that runs an image from a registry rather than building one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct RuleImageView {
    pub service: ServiceName,
    /// As Saved, such as `ghcr.io/acme/web@sha256:…`.
    pub image: String,
    /// The repository a registry rule would name for `image`, or none when no rule can.
    /// For display: a draft reads `image` again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub registry: Option<RegistrySource>,
}

impl RuleImageView {
    /// `service`'s Saved `image`, with the repository a registry rule would name for it.
    #[must_use]
    pub fn new(service: ServiceName, image: String) -> Self {
        let registry = crate::registry_image(&image)
            .ok()
            .map(|named| RegistrySource {
                host: named.host,
                repository: named.repository,
            });
        Self {
            service,
            image,
            registry,
        }
    }
}

/// What a rule is drafted from, as the Store's rule view gives it: the Environment's
/// Namespace, the rule kept for it, where its Saved Services build from, the latest
/// Saved revision lowered as a push would deploy it, and the Volumes it mounts. Other
/// fields of the view are ignored.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuleBasis {
    pub namespace: Namespace,
    pub rule: Option<Signed<AutoDeployRule>>,
    pub sources: Vec<RuleSourceView>,
    /// The Saved Services that run a registry's image, which a registry rule deploys.
    #[serde(default)]
    pub images: Vec<RuleImageView>,
    pub target: Vec<RequestedServiceSpec>,
    #[serde(default)]
    pub volumes: PlannedVolumes,
    /// The births [`AutoDeployRule::draft`] minted into `target`.
    #[serde(skip)]
    pub minted: Births,
}

/// What the approver's client sets around a rule: the Cluster and its Servers as the
/// Servers report them, the repository owner's GitHub ID, the builder and platform, when,
/// and who signs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuleTerms {
    pub cluster: SealingPublicKey,
    pub machines: Vec<MachineId>,
    #[ts(type = "number")]
    pub owner_id: u64,
    /// Ployz's own release, [`Builder::pinned`], unless the client names another.
    #[serde(default = "Builder::pinned")]
    #[ts(optional, as = "Option<Builder>")]
    pub builder: Builder,
    /// Such as `linux/amd64`, the default.
    #[serde(default = "default_platform")]
    #[ts(optional, as = "Option<String>")]
    pub platform: String,
    /// The ID a new rule takes: 128 random bits from the approver's client. A rule kept
    /// for the same repository, branch and Environment keeps its own.
    pub new_rule: RuleId,
    pub issued_at: Timestamp,
    pub signer: SignerKey,
}

/// What GitHub builds a rule's images for unless it names otherwise.
#[must_use]
pub fn default_platform() -> String {
    "linux/amd64".to_owned()
}

/// What a rule renewed from `kept` builds its images for: what `kept` builds for, so a
/// Deploy or Save that renews it keeps a platform someone approved, else the default.
#[must_use]
pub fn renewed_platform(kept: Option<&AutoDeployRule>) -> String {
    kept.and_then(|kept| {
        (kept.services.iter())
            .find_map(|service| service.policy.as_ref()?.platforms.first().cloned())
    })
    .unwrap_or_else(default_platform)
}

/// Why a rule can't be drafted. Each says what the person can change.
#[derive(Debug, thiserror::Error)]
pub enum RuleDraftError {
    #[error("{0} doesn't build from GitHub, so a rule can't deploy it.")]
    NotGit(ServiceName),
    #[error("{0}'s branch is disconnected. Connect it, then approve again.")]
    Disconnected(ServiceName),
    #[error(
        "A rule deploys one branch, and {service} builds from {repository} at {branch}, not \
         {first_repository} at {first_branch}. Pick one branch for automatic deploys."
    )]
    TwoBranches {
        service: ServiceName,
        repository: String,
        branch: String,
        first_repository: String,
        first_branch: String,
    },
    #[error("{0} isn't saved in this environment. Publish it, then approve again.")]
    NotSaved(ServiceName),
    #[error(
        "Nothing in this environment builds from GitHub, so there's nothing to deploy on push."
    )]
    Nothing,
    #[error("There's no automatic deploy rule to turn off.")]
    NoRule,
    #[error("No service in this environment runs an image from {0}.")]
    NoImages(String),
    #[error("{service} builds from {repository}, so previews can't build it.")]
    OtherRepository {
        service: ServiceName,
        repository: String,
    },
    #[error(
        "{service} runs {image}. Pin it to a digest, like image@sha256:…, then set up previews again."
    )]
    Unpinned { service: ServiceName, image: String },
    #[error("{0} doesn't build from this repository or run an image, so previews can't run it.")]
    Uncovered(ServiceName),
    #[error("{service} runs {image}, which an automatic deploy rule can't name: {reason}")]
    Unnamed {
        service: ServiceName,
        image: String,
        reason: crate::RegistryName,
    },
    #[error(transparent)]
    Birth(#[from] BirthError),
    #[error(transparent)]
    Sealed(#[from] MissingSealedIdentity),
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
}

/// A rule's canonical body, to sign as it is, what it shows the person, and the births it
/// minted, which the Store records beside the signed rule.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct RuleDraft {
    pub body: String,
    pub summary: RuleSummary,
    #[serde(skip_serializing_if = "Births::is_empty")]
    #[ts(optional, as = "Option<Births>")]
    pub births: Births,
}

/// What a person reads before they sign a rule, taken from the body itself.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct RuleSummary {
    /// The Namespace the rule deploys into, which every Server checks.
    pub namespace: String,
    pub source: SourceSummary,
    #[ts(type = "number")]
    pub servers: usize,
    #[ts(type = "number")]
    pub generation: u64,
    pub ended: bool,
    pub services: Vec<RuleServiceSummary>,
    /// What a preview rule adds: where previews start from, the Services they run pinned,
    /// and the Sealed values they open.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub preview: Option<super::preview_draft::PreviewSummary>,
}

/// What a rule deploys, as a person reads it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceSummary {
    /// Every push to `branch`, built by the pinned builder.
    Github {
        repository: String,
        branch: String,
        workflow: String,
        /// The builder's `job_workflow_ref`, followed by `at <sha>` unless the ref ends in it.
        builder: String,
        /// Whether the builder is Ployz's own release.
        pinned: bool,
    },
    /// Every image pushed to `repository`, `host/path`.
    Registry { repository: String },
}

/// One Service of a rule as a person reads it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct RuleServiceSummary {
    pub name: ServiceName,
    /// `Dockerfile in ., for linux/amd64, with the Actions cache`, empty for a registry
    /// rule, which builds nothing, and `runs <image>` for a Service a preview rule pins.
    pub builds: String,
    /// What differs from the rule this one replaces, such as `settings` or `build args`.
    /// Empty when nothing does; `new to the rule` for a Service it didn't cover.
    pub changes: Vec<String>,
    /// The settings it pins: variables (Sealed ones by name only) and what reaches the
    /// host.
    pub settings: Vec<String>,
    /// The mounts the person must say yes to: each one that holds existing data, since a
    /// rule has no verified version to compare with, and each external Docker volume or
    /// host path, which `settings` also lists among the spec's volumes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub confirm: Vec<String>,
}

impl RuleBasis {
    /// The rule kept for this Environment, when it reads.
    #[must_use]
    pub fn kept(&self) -> Option<AutoDeployRule> {
        self.rule.as_ref().and_then(|rule| rule.open().ok())
    }

    /// The Services a rule approved in the dashboard deploys: every one that builds from
    /// GitHub with its Auto-deploy on, when they all build from one branch. When they
    /// build from several, those on the kept rule's branch, if any still do. Otherwise
    /// every one, so drafting names the branches that keep them from one rule.
    #[must_use]
    pub fn branch_services(&self) -> Vec<ServiceName> {
        let pushed = || self.sources.iter().filter(|source| source.on_push);
        let all = || pushed().map(|source| source.service.clone());
        let mut branches = pushed().map(|source| (source.repository_id, source.branch.as_deref()));
        let first = branches.next();
        if branches.all(|branch| Some(branch) == first) {
            return all().collect();
        }
        let Some(kept) = self.kept().and_then(|kept| kept.source.github().cloned()) else {
            return all().collect();
        };
        let on_kept: Vec<ServiceName> = pushed()
            .filter(|source| {
                source.repository_id == kept.repository_id
                    && source.branch.as_deref() == Some(kept.branch.as_str())
            })
            .map(|source| source.service.clone())
            .collect();
        if on_kept.is_empty() {
            all().collect()
        } else {
            on_kept
        }
    }

    fn source(&self, name: &ServiceName) -> Result<&RuleSourceView, RuleDraftError> {
        self.sources
            .iter()
            .find(|source| source.service == *name)
            .ok_or_else(|| RuleDraftError::NotGit(name.clone()))
    }

    fn spec(&self, name: &ServiceName) -> Result<&RequestedServiceSpec, RuleDraftError> {
        self.target
            .iter()
            .find(|spec| spec.name == *name)
            .ok_or_else(|| RuleDraftError::NotSaved(name.clone()))
    }

    /// The one repository and branch every one of `names` builds from.
    fn one_branch(&self, names: &[ServiceName]) -> Result<(String, String, u64), RuleDraftError> {
        let mut found: Option<(String, String, u64)> = None;
        for name in names {
            let source = self.source(name)?;
            let branch = source
                .branch
                .clone()
                .ok_or_else(|| RuleDraftError::Disconnected(name.clone()))?;
            let this = (source.repository.clone(), branch, source.repository_id);
            match &found {
                None => found = Some(this),
                Some(first) if *first == this => {}
                Some((first_repository, first_branch, _)) => {
                    return Err(RuleDraftError::TwoBranches {
                        service: name.clone(),
                        repository: this.0,
                        branch: this.1,
                        first_repository: first_repository.clone(),
                        first_branch: first_branch.clone(),
                    });
                }
            }
        }
        found.ok_or(RuleDraftError::Nothing)
    }

    /// What a rule approves for `name`: its Saved settings less the image, the build
    /// policy the runner must build with, and its build's Sealed values.
    fn rule_service(
        &self,
        name: &ServiceName,
        platform: &str,
    ) -> Result<RuleService, RuleDraftError> {
        let spec = self.spec(name)?;
        let source = self.source(name)?;
        let policy = BuildPolicy::derive(
            &source.root_dir,
            &source.build,
            spec,
            BTreeSet::from([platform.to_owned()]),
            PolicyCache::Actions,
        );
        let secrets = SealedRecord::of(spec)?
            .map(|record| SealedDigest::of(&record))
            .transpose()?;
        Ok(RuleService {
            name: name.clone(),
            template: template(spec, &self.namespace)?,
            policy: Some(policy),
            secrets,
            seeded: None,
        })
    }

    /// The registry repository a registry rule covering the image Services would name: the
    /// kept rule's while any of them still runs an image from it, else the one repository
    /// they all run images from. `None` when there is no image Service, or they run images
    /// from several repositories with none of them the kept one.
    #[must_use]
    pub fn image_source(&self) -> Option<RegistrySource> {
        let named = |view: &RuleImageView| {
            crate::registry_image(&view.image)
                .ok()
                .map(|named| RegistrySource {
                    host: named.host,
                    repository: named.repository,
                })
        };
        let kept = self.kept().and_then(|kept| kept.source.registry().cloned());
        if let Some(kept) = kept
            && self
                .images
                .iter()
                .any(|view| named(view).as_ref() == Some(&kept))
        {
            return Some(kept);
        }
        let mut repositories = self.images.iter().filter_map(named);
        let first = repositories.next()?;
        repositories.all(|other| other == first).then_some(first)
    }

    /// The image Services whose Saved image is in `source`.
    #[must_use]
    pub fn image_services(&self, source: &RegistrySource) -> Vec<ServiceName> {
        self.images
            .iter()
            .filter(|view| {
                crate::registry_image(&view.image).is_ok_and(|named| {
                    named.host == source.host && named.repository == source.repository
                })
            })
            .map(|view| view.service.clone())
            .collect()
    }
}

impl AutoDeployRule {
    /// The rule that deploys every push of `names`' one branch with their Saved settings
    /// in `basis`: the kept rule's next generation when it is for the same repository,
    /// branch and Environment, else a new rule's first. Each Volume `basis` lists as unborn
    /// gets a birth first, written into `basis.target` and kept in `basis.minted`, so the
    /// rule approves the specs a push sends once the Store records them.
    ///
    /// # Errors
    /// Returns a [`RuleDraftError`] naming what keeps `names` from one rule.
    pub fn draft(
        basis: &mut RuleBasis,
        names: &[ServiceName],
        terms: RuleTerms,
    ) -> Result<Self, RuleDraftError> {
        let (repository, branch, repository_id) = basis.one_branch(names)?;
        // The client's random ID for a new rule, which no rule keeps when it extends one,
        // keys each nonce, so every approval mints births no other one can.
        // Only the specs this rule approves get births, so the Store records none for a
        // Volume no Service of the rule mounts.
        basis.minted = birth::mint(
            basis
                .target
                .iter_mut()
                .filter(|spec| names.contains(&spec.name)),
            (&basis.namespace, &basis.volumes),
            terms.issued_at,
            |name| birth::nonce(terms.new_rule.as_bytes(), name),
        )?;
        let mut services = names
            .iter()
            .map(|name| basis.rule_service(name, &terms.platform))
            .collect::<Result<Vec<_>, _>>()?;
        services.sort_by(|left, right| left.name.cmp(&right.name));
        services.dedup_by(|left, right| left.name == right.name);
        let mut machines = terms.machines;
        machines.sort_unstable();
        machines.dedup();
        // A Machine refuses a later generation that moves a rule to another repository,
        // branch or Environment, so such a change starts a new rule.
        let (rule, generation) = match basis.kept() {
            Some(kept)
                if kept.namespace == basis.namespace
                    && kept.source.github().is_some_and(|kept| {
                        kept.repository_id == repository_id && kept.branch == branch
                    }) =>
            {
                (kept.rule, kept.generation + 1)
            }
            _ => (terms.new_rule, 1),
        };
        Ok(Self {
            cluster: terms.cluster,
            rule,
            generation,
            namespace: basis.namespace.clone(),
            machines,
            source: RuleSource::Github(GithubSource {
                repository_id,
                owner_id: terms.owner_id,
                repository,
                branch,
                workflow: WORKFLOW.to_owned(),
                // A push builds its head; a rebuild of an older commit a person approved
                // runs on a dispatch.
                events: vec![RuleEvent::Push, RuleEvent::WorkflowDispatch],
                builder: terms.builder,
                runner: Runner::GithubHosted,
            }),
            services,
            issued_at: terms.issued_at,
            ended: false,
            signer: terms.signer,
            proposal: None,
            preview: None,
        })
    }

    /// The rule that deploys every image pushed to `source` for `names`, with their Saved
    /// settings in `basis`: the kept rule's next generation when it is a registry rule in
    /// the same Environment, for this repository or another, so a Server that takes it
    /// refuses the generation before; else a new rule's first. Births are minted as
    /// [`Self::draft`] mints them. The rule builds nothing, so `terms`' builder, platform
    /// and owner are not read.
    ///
    /// # Errors
    /// Returns a [`RuleDraftError`] naming a Service that doesn't run an image of
    /// `source`, or [`RuleDraftError::NoImages`] for none.
    pub fn draft_images(
        basis: &mut RuleBasis,
        source: &RegistrySource,
        names: &[ServiceName],
        terms: RuleTerms,
    ) -> Result<Self, RuleDraftError> {
        if names.is_empty() {
            return Err(RuleDraftError::NoImages(source.name()));
        }
        let covered = basis.image_services(source);
        if let Some(name) = names.iter().find(|name| !covered.contains(name)) {
            let image = (basis.images.iter())
                .find(|view| view.service == *name)
                .map(|view| view.image.clone());
            return Err(match image {
                Some(image) => match crate::registry_image(&image) {
                    Err(reason) => RuleDraftError::Unnamed {
                        service: name.clone(),
                        image,
                        reason,
                    },
                    Ok(_) => RuleDraftError::NoImages(source.name()),
                },
                None => RuleDraftError::NotSaved(name.clone()),
            });
        }
        basis.minted = birth::mint(
            basis
                .target
                .iter_mut()
                .filter(|spec| names.contains(&spec.name)),
            (&basis.namespace, &basis.volumes),
            terms.issued_at,
            |name| birth::nonce(terms.new_rule.as_bytes(), name),
        )?;
        let mut services = names
            .iter()
            .map(|name| {
                Ok(RuleService {
                    name: name.clone(),
                    template: template(basis.spec(name)?, &basis.namespace)?,
                    policy: None,
                    secrets: None,
                    seeded: None,
                })
            })
            .collect::<Result<Vec<_>, RuleDraftError>>()?;
        services.sort_by(|left, right| left.name.cmp(&right.name));
        services.dedup_by(|left, right| left.name == right.name);
        let mut machines = terms.machines;
        machines.sort_unstable();
        machines.dedup();
        let (rule, generation) = match basis.kept() {
            Some(kept) if kept.namespace == basis.namespace && kept.source.registry().is_some() => {
                (kept.rule, kept.generation + 1)
            }
            _ => (terms.new_rule, 1),
        };
        Ok(Self {
            cluster: terms.cluster,
            rule,
            generation,
            namespace: basis.namespace.clone(),
            machines,
            source: RuleSource::Registry(source.clone()),
            services,
            issued_at: terms.issued_at,
            ended: false,
            signer: terms.signer,
            proposal: None,
            preview: None,
        })
    }

    /// The generation that switches the kept rule off.
    ///
    /// # Errors
    /// Returns [`RuleDraftError::NoRule`] when no rule is kept, or it is already off.
    pub fn ending(
        basis: &RuleBasis,
        issued_at: Timestamp,
        signer: SignerKey,
    ) -> Result<Self, RuleDraftError> {
        let kept = basis
            .kept()
            .filter(|kept| !kept.ended)
            .ok_or(RuleDraftError::NoRule)?;
        Ok(Self {
            generation: kept.generation + 1,
            issued_at,
            ended: true,
            signer,
            proposal: None,
            ..kept
        })
    }

    /// This rule's canonical body and what it shows the person, compared with the rule
    /// kept in `basis`.
    ///
    /// # Errors
    /// Returns a [`CanonicalError`] when the rule has no canonical form.
    pub fn drafted(&self, basis: &RuleBasis) -> Result<RuleDraft, CanonicalError> {
        Ok(RuleDraft {
            body: canonical::to_canonical(self)?,
            summary: self.summary(basis),
            births: basis.minted.clone(),
        })
    }

    /// What a person reads before signing this rule. Each Service's changes are against
    /// the rule kept in `basis`, read from its body, so nothing here takes Cloud's word
    /// for what changed.
    #[must_use]
    pub fn summary(&self, basis: &RuleBasis) -> RuleSummary {
        let kept = basis.kept().filter(|kept| kept.rule == self.rule);
        let mounts = Mounts {
            namespace: &self.namespace,
            volumes: &basis.volumes,
            minted: &basis.minted,
            now: self.issued_at,
            against: None,
        };
        let source = match &self.source {
            RuleSource::Github(github) => {
                let builder = &github.builder;
                let sha = builder.sha.to_string();
                SourceSummary::Github {
                    repository: Shown::text(&github.repository).into(),
                    branch: Shown::text(&github.branch).into(),
                    workflow: Shown::text(&github.workflow).into(),
                    builder: match builder.r#ref.ends_with(&sha) {
                        true => Shown::text(&builder.r#ref),
                        false => Shown::text(&builder.r#ref).and_words(" at ").and_text(&sha),
                    }
                    .into(),
                    pinned: *builder == Builder::pinned(),
                }
            }
            RuleSource::Registry(registry) => SourceSummary::Registry {
                repository: Shown::text(&registry.name()).into(),
            },
        };
        RuleSummary {
            namespace: self.namespace.to_string(),
            source,
            servers: self.machines.len(),
            generation: self.generation,
            ended: self.ended,
            preview: None,
            services: self
                .services
                .iter()
                .map(|service| {
                    let spec = basis.target.iter().find(|spec| spec.name == service.name);
                    let mounted = spec.map(|spec| mounts.asked(spec)).unwrap_or_default();
                    RuleServiceSummary {
                        name: service.name.clone(),
                        builds: service
                            .policy
                            .as_ref()
                            .map(|policy| builds(policy).into())
                            .unwrap_or_default(),
                        changes: match &kept {
                            Some(kept) => changes(kept.service(&service.name), service),
                            None => Vec::new(),
                        },
                        confirm: mounted.confirm.iter().map(ToString::to_string).collect(),
                        settings: spec
                            .map(|spec| settings(spec, mounted))
                            .unwrap_or_default()
                            .into_iter()
                            .chain(spec.and_then(|spec| mounts.region(spec)))
                            .map(String::from)
                            .collect(),
                    }
                })
                .collect(),
        }
    }
}

/// How a Service builds, as a person reads it: every field of `policy`, which the builder
/// must run exactly. The destructuring is exhaustive, so a field added to the policy
/// fails to compile here until it shows.
#[must_use]
pub fn builds(policy: &BuildPolicy) -> Shown {
    let BuildPolicy {
        method,
        context,
        target,
        args,
        secrets,
        platforms,
        cache,
    } = policy;
    let mut line = match method {
        PolicyMethod::Dockerfile { path } => Shown::words("Dockerfile ").and_text(path),
        PolicyMethod::Railpack { command: None } => {
            Shown::words("Railpack, with the build command it detects")
        }
        PolicyMethod::Railpack {
            command: Some(command),
        } => Shown::words("Railpack, with build command ").and_text(command),
    }
    .and_words(", context ")
    .and_text(context)
    .and_words(", platforms ")
    .and(Shown::list(
        platforms.iter().map(|platform| Shown::text(platform)),
    ));
    if let Some(target) = target {
        line = line.and_words(", target ").and_text(target);
    }
    line = match cache {
        PolicyCache::Actions => line.and_words(", with the Actions cache"),
        PolicyCache::Off => line.and_words(", no cache"),
    };
    if !args.is_empty() {
        line = line.and_words(", build args ").and(pairs(
            args.iter().map(|(name, value)| (name, Shown::text(value))),
        ));
    }
    if !secrets.is_empty() {
        line = line
            .and_words(", build secrets ")
            .and(Shown::list(secrets.iter().map(|name| Shown::text(name))));
    }
    line
}

/// `name=value` for each pair, separated by `", "`.
fn pairs<'a>(pairs: impl IntoIterator<Item = (&'a String, Shown)>) -> Shown {
    Shown::list(
        pairs
            .into_iter()
            .map(|(name, value)| Shown::text(name).and_words("=").and(value)),
    )
}

/// `value` as JSON, to compare one policy field across two rules.
fn json(value: &impl Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// What differs between the kept rule's approval of a Service and `next`.
fn changes(kept: Option<&RuleService>, next: &RuleService) -> Vec<String> {
    let Some(kept) = kept else {
        return vec!["new to the rule".to_owned()];
    };
    let policy = |pick: fn(&BuildPolicy) -> Value| {
        let read = |service: &RuleService| service.policy.as_ref().map(pick);
        read(kept) != read(next)
    };
    [
        ("settings", kept.template != next.template),
        (
            "build secrets",
            kept.secrets != next.secrets || policy(|policy| json(&policy.secrets)),
        ),
        ("build method", policy(|policy| json(&policy.method))),
        ("build context", policy(|policy| json(&policy.context))),
        ("build target", policy(|policy| json(&policy.target))),
        ("build args", policy(|policy| json(&policy.args))),
        ("platforms", policy(|policy| json(&policy.platforms))),
        ("build cache", policy(|policy| json(&policy.cache))),
    ]
    .into_iter()
    .filter(|(_, changed)| *changed)
    .map(|(what, _)| what.to_owned())
    .collect()
}

/// The settings a person reads before approving: each variable, Sealed parts by name only,
/// the pre-deploy command, a line for each mount of a managed or Docker volume naming the
/// data it holds with the settings it signs, then every other field a statement signs whose value is not the spec's
/// default, by its wire name. The run fields Cloud picks are not shown, since they are not
/// signed. A field added to the spec later shows without a change here, and a value shows
/// whenever it signs differently from the default, so no two specs that sign differently
/// read the same, but for a Sealed ciphertext and the name a born Volume is declared under,
/// which its birth stands for.
#[must_use]
pub fn settings(spec: &RequestedServiceSpec, mounted: MountLines) -> Vec<Shown> {
    let mut value =
        serde_json::to_value(run_fields_cleared(spec)).expect("a spec serializes to JSON");
    drop_nulls(&mut value);
    hide_cleared_values(&mut value);
    let Value::Object(mut fields) = value else {
        return Vec::new();
    };
    let mut container = object(fields.remove("container"));
    let mut hook = fields.remove("pre_deploy").map(|hook| object(Some(hook)));
    let mut lines = Vec::new();
    variables(&mut lines, "variables ", container.remove("environment"));
    if let Some(hook) = &mut hook {
        if let Some(command) = hook.remove("command") {
            lines.push(Shown::words("pre-deploy ").and_value(&command));
        }
        variables(
            &mut lines,
            "pre-deploy variables ",
            hook.remove("environment"),
        );
    }
    lines.extend(mounted.lines);
    leave_out_mounts(spec, &mut fields, &mounted.targets);
    if let Some(Value::Array(configs)) = fields.get_mut("configs") {
        configs.iter_mut().for_each(config_text);
    }
    let defaults = defaults();
    // The name shows on the Service's own line.
    fields.remove("name");
    changed(&mut lines, "", fields, &defaults.service);
    // The image shows on its own line or is a template's placeholder, the Sealed
    // identity is bound by each value's ciphertext, and the pull policy is cleared.
    container.remove("image");
    container.remove("sealed_identity");
    container.remove("pull_policy");
    changed(&mut lines, "", container, &defaults.container);
    if let Some(hook) = hook {
        changed(&mut lines, "pre-deploy ", hook, &defaults.hook);
    }
    lines
}

/// Hide the fixed values clearing leaves that no person set: a Provisioned Volume's
/// maximum, and the counts of the regions a host-data spec keeps, which show as names.
fn hide_cleared_values(spec: &mut Value) {
    let volumes = spec.get_mut("volumes").and_then(Value::as_array_mut);
    for volume in volumes.into_iter().flatten() {
        if let Some(source) = volume.get_mut("source").and_then(Value::as_object_mut) {
            source.remove("maximum_bytes");
        }
    }
    if let Some(regions) = spec.get_mut("regions") {
        let names = regions
            .as_object()
            .map(|counts| counts.keys().cloned().map(Value::String).collect());
        *regions = Value::Array(names.unwrap_or_default());
    }
}

/// Leave out of `fields` the mounts at `shown`, which have lines of their own, and each
/// Volume only those mounts use.
fn leave_out_mounts(
    spec: &RequestedServiceSpec,
    fields: &mut Map<String, Value>,
    shown: &[String],
) {
    let graph = spec.volume_graph();
    let lined = |target: &str| shown.iter().any(|shown| shown == target);
    let only_lined = |reference: &str| {
        let mut mounts = graph
            .mounts()
            .iter()
            .filter(|mount| mount.volume.as_str() == reference)
            .peekable();
        mounts.peek().is_some() && mounts.all(|mount| lined(mount.target.as_str()))
    };
    let field =
        |item: &Value, name: &str| item.get(name).and_then(Value::as_str).map(str::to_owned);
    if let Some(Value::Array(mounts)) = fields.get_mut("mounts") {
        mounts.retain(|mount| field(mount, "target").is_none_or(|target| !lined(&target)));
    }
    if let Some(Value::Array(volumes)) = fields.get_mut("volumes") {
        volumes.retain(|volume| {
            field(volume, "reference").is_none_or(|reference| !only_lined(&reference))
        });
    }
    for name in ["mounts", "volumes"] {
        if fields
            .get(name)
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            fields.remove(name);
        }
    }
}

/// The JSON a spec writes for each field it leaves at its default, per part. A field
/// absent here, such as an `Option` or one every spec names, shows whenever it is set.
struct Defaults {
    service: Map<String, Value>,
    container: Map<String, Value>,
    hook: Map<String, Value>,
}

fn defaults() -> &'static Defaults {
    static DEFAULTS: std::sync::OnceLock<Defaults> = std::sync::OnceLock::new();
    DEFAULTS.get_or_init(|| {
        let spec: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
            "name": "defaults",
            "mode": {"mode": "global"},
            "container": {"image": "", "pull_policy": "never"},
            "pre_deploy": {"command": ["defaults"]},
        }))
        .expect("the defaults spec parses");
        let mut value = serde_json::to_value(spec).expect("a spec serializes to JSON");
        drop_nulls(&mut value);
        let Value::Object(mut service) = value else {
            unreachable!("a spec serializes to an object")
        };
        let mut container = object(service.remove("container"));
        let mut hook = object(service.remove("pre_deploy"));
        for named in ["image", "pull_policy"] {
            container.remove(named);
        }
        hook.remove("command");
        Defaults {
            service,
            container,
            hook,
        }
    })
}

fn object(value: Option<Value>) -> Map<String, Value> {
    match value {
        Some(Value::Object(fields)) => fields,
        _ => Map::new(),
    }
}

/// One line per field of `fields` whose value is not its default.
fn changed(
    lines: &mut Vec<Shown>,
    prefix: &'static str,
    fields: Map<String, Value>,
    defaults: &Map<String, Value>,
) {
    for (name, value) in fields {
        if defaults.get(&name) != Some(&value) {
            lines.push(
                Shown::words(prefix)
                    .and_text(&name)
                    .and_words(" ")
                    .and_value(&value),
            );
        }
    }
}

/// `label` and each variable of `environment`: its text, a Sealed value by the name it
/// was sealed as, and a template's parts joined by `+`.
fn variables(lines: &mut Vec<Shown>, label: &'static str, environment: Option<Value>) {
    let environment = object(environment);
    if environment.is_empty() {
        return;
    }
    let variables = environment.into_iter().map(|(name, value)| {
        let shown = Shown::text(&name);
        match serde_json::from_value::<EnvValue>(value.clone()) {
            Ok(EnvValue::Text(text)) => shown.and_words("=").and_text(&text),
            Ok(EnvValue::Sealed(sealed)) if sealed.key == name && plain_seal(&sealed) => {
                shown.and_words(" (sealed)")
            }
            Ok(EnvValue::Sealed(sealed)) => shown.and_words("=").and(sealed_part(&sealed)),
            Ok(EnvValue::Template(template)) => {
                let mut shown = shown.and_words("=");
                for (index, part) in template.template.iter().enumerate() {
                    if index > 0 {
                        shown = shown.and_words(" + ");
                    }
                    shown = match part {
                        EnvPart::Text(text) => shown.and_value(text),
                        EnvPart::Sealed(sealed) => shown.and(sealed_part(sealed)),
                    };
                }
                shown
            }
            Err(_) => shown.and_words("=").and_value(&value),
        }
    });
    lines.push(Shown::words(label).and(Shown::list(variables)));
}

fn plain_seal(sealed: &SealedValue) -> bool {
    sealed.scope == SealedScope::Shared
}

/// `(sealed KEY)`, with the Service it was authored on when that is not the Container's own.
fn sealed_part(sealed: &SealedValue) -> Shown {
    let mut shown = Shown::words("(sealed ").and_value(&sealed.key);
    if let SealedScope::Service(service) = sealed.scope {
        shown = shown
            .and_words(" of service ")
            .and_text(&service.to_string());
    }
    shown.and_words(")")
}

/// A Config's bytes as text when they are UTF-8, which a person can read; else as they are.
fn config_text(config: &mut Value) {
    let Some(content) = config.get_mut("content") else {
        return;
    };
    let text = serde_json::from_value::<Vec<u8>>(content.clone())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok());
    if let Some(text) = text {
        *content = Value::String(text);
    }
}

#[cfg(test)]
#[path = "rule_draft_tests.rs"]
pub(super) mod tests;
