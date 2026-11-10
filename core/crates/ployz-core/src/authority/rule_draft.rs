//! The automatic deploy rule an approver's client drafts from an Environment's Saved
//! settings, and what it shows the person before they sign. The CLI and the dashboard both
//! draft here, so a rule a passkey answers in the browser has the bytes a Device would
//! sign for the same settings.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    canonical::{self, CanonicalError, Timestamp},
    keys::Signed,
    restart::SignerKey,
    rule::{
        AutoDeployRule, BuildPolicy, Builder, PolicyCache, PolicyMethod, RuleEvent, RuleId,
        RuleService, RuleSource, Runner, SealedDigest, template,
    },
};
use crate::{
    MachineId, MissingSealedIdentity, Namespace, RequestedServiceSpec, SealedRecord,
    SealingPublicKey, ServiceName, config::ServiceBuildConfig,
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
}

/// What a rule is drafted from, as the Store's rule view gives it: the Environment's
/// Namespace, the rule kept for it, where its Saved Services build from, and the latest
/// Saved revision lowered as a push would deploy it. Other fields of the view are ignored.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuleBasis {
    pub namespace: Namespace,
    pub rule: Option<Signed<AutoDeployRule>>,
    pub sources: Vec<RuleSourceView>,
    pub target: Vec<RequestedServiceSpec>,
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

fn default_platform() -> String {
    "linux/amd64".to_owned()
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
         {first_repository} at {first_branch}. Approve a rule for each."
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
    #[error(transparent)]
    Sealed(#[from] MissingSealedIdentity),
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
}

/// A rule's canonical body, to sign as it is, and what it shows the person.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct RuleDraft {
    pub body: String,
    pub summary: RuleSummary,
}

/// What a person reads before they sign a rule, taken from the body itself.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct RuleSummary {
    pub repository: String,
    pub branch: String,
    pub workflow: String,
    /// The builder's `job_workflow_ref`, followed by `at <sha>` unless the ref ends in it.
    pub builder: String,
    /// Whether the builder is Ployz's own release.
    pub pinned: bool,
    #[ts(type = "number")]
    pub servers: usize,
    #[ts(type = "number")]
    pub generation: u64,
    pub ended: bool,
    pub services: Vec<RuleServiceSummary>,
}

/// One Service of a rule as a person reads it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct RuleServiceSummary {
    pub name: ServiceName,
    /// `Dockerfile in ., for linux/amd64, with the Actions cache`.
    pub builds: String,
    /// What differs from the rule this one replaces, such as `settings` or `build args`.
    /// Empty when nothing does; `new to the rule` for a Service it didn't cover.
    pub changes: Vec<String>,
    /// The settings it pins: variables (Sealed ones by name only) and what reaches the
    /// host.
    pub settings: Vec<String>,
}

impl RuleBasis {
    /// The rule kept for this Environment, when it reads.
    #[must_use]
    pub fn kept(&self) -> Option<AutoDeployRule> {
        self.rule.as_ref().and_then(|rule| rule.open().ok())
    }

    /// The Services a rule approved in the dashboard deploys: every one that builds from
    /// GitHub, when they all build from one branch. When they build from several, those
    /// on the kept rule's branch, if any still do. Otherwise every one, so drafting names
    /// the branches that keep them from one rule.
    #[must_use]
    pub fn branch_services(&self) -> Vec<ServiceName> {
        let all = || self.sources.iter().map(|source| source.service.clone());
        let mut branches = self
            .sources
            .iter()
            .map(|source| (source.repository_id, source.branch.as_deref()));
        let first = branches.next();
        if branches.all(|branch| Some(branch) == first) {
            return all().collect();
        }
        let Some(kept) = self.kept().map(|kept| kept.source) else {
            return all().collect();
        };
        let on_kept: Vec<ServiceName> = self
            .sources
            .iter()
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
            policy,
            secrets,
        })
    }
}

impl AutoDeployRule {
    /// The rule that deploys every push of `names`' one branch with their Saved settings
    /// in `basis`: the kept rule's next generation when it is for the same repository,
    /// branch and Environment, else a new rule's first.
    ///
    /// # Errors
    /// Returns a [`RuleDraftError`] naming what keeps `names` from one rule.
    pub fn draft(
        basis: &RuleBasis,
        names: &[ServiceName],
        terms: RuleTerms,
    ) -> Result<Self, RuleDraftError> {
        let (repository, branch, repository_id) = basis.one_branch(names)?;
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
                    && kept.source.repository_id == repository_id
                    && kept.source.branch == branch =>
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
            source: RuleSource {
                repository_id,
                owner_id: terms.owner_id,
                repository,
                branch,
                workflow: WORKFLOW.to_owned(),
                events: vec![RuleEvent::Push],
                builder: terms.builder,
                runner: Runner::GithubHosted,
            },
            services,
            issued_at: terms.issued_at,
            ended: false,
            signer: terms.signer,
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
        })
    }

    /// What a person reads before signing this rule. Each Service's changes are against
    /// the rule kept in `basis`, read from its body, so nothing here takes Cloud's word
    /// for what changed.
    #[must_use]
    pub fn summary(&self, basis: &RuleBasis) -> RuleSummary {
        let kept = basis.kept().filter(|kept| kept.rule == self.rule);
        let builder = &self.source.builder;
        let sha = builder.sha.to_string();
        RuleSummary {
            repository: self.source.repository.clone(),
            branch: self.source.branch.clone(),
            workflow: self.source.workflow.clone(),
            builder: match builder.r#ref.ends_with(&sha) {
                true => builder.r#ref.clone(),
                false => format!("{} at {sha}", builder.r#ref),
            },
            pinned: *builder == Builder::pinned(),
            servers: self.machines.len(),
            generation: self.generation,
            ended: self.ended,
            services: self
                .services
                .iter()
                .map(|service| RuleServiceSummary {
                    name: service.name.clone(),
                    builds: builds(&service.policy),
                    changes: match &kept {
                        Some(kept) => changes(kept.service(&service.name), service),
                        None => Vec::new(),
                    },
                    settings: basis
                        .target
                        .iter()
                        .find(|spec| spec.name == service.name)
                        .map(settings)
                        .unwrap_or_default(),
                })
                .collect(),
        }
    }
}

fn builds(policy: &BuildPolicy) -> String {
    let method = match &policy.method {
        PolicyMethod::Dockerfile { path } => path.clone(),
        PolicyMethod::Railpack { .. } => "Railpack".to_owned(),
    };
    format!(
        "{method} in {}, for {}{}",
        policy.context,
        policy
            .platforms
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
        match policy.cache {
            PolicyCache::Actions => ", with the Actions cache",
            PolicyCache::Off => "",
        }
    )
}

/// What differs between the kept rule's approval of a Service and `next`.
fn changes(kept: Option<&RuleService>, next: &RuleService) -> Vec<String> {
    let Some(kept) = kept else {
        return vec!["new to the rule".to_owned()];
    };
    let (old, new) = (&kept.policy, &next.policy);
    [
        ("settings", kept.template != next.template),
        (
            "build secrets",
            kept.secrets != next.secrets || old.secrets != new.secrets,
        ),
        ("build method", old.method != new.method),
        ("build context", old.context != new.context),
        ("build target", old.target != new.target),
        ("build args", old.args != new.args),
        ("platforms", old.platforms != new.platforms),
        ("build cache", old.cache != new.cache),
    ]
    .into_iter()
    .filter(|(_, changed)| *changed)
    .map(|(what, _)| what.to_owned())
    .collect()
}

/// The settings a person reads before approving: each variable, Sealed ones by name only,
/// and what reaches the host.
fn settings(spec: &RequestedServiceSpec) -> Vec<String> {
    let container = &spec.container;
    let mut lines = Vec::new();
    let variables: Vec<String> = container
        .environment
        .iter()
        .map(|(name, value)| match value.text() {
            Some(text) => format!("{name}={text}"),
            None => format!("{name} (sealed)"),
        })
        .collect();
    if !variables.is_empty() {
        lines.push(format!("variables {}", variables.join(", ")));
    }
    if let Some(hook) = &spec.pre_deploy {
        lines.push(format!(
            "pre-deploy {}",
            serde_json::to_string(&hook.command).unwrap_or_default()
        ));
    }
    if !spec.ports.is_empty() {
        lines.push(format!(
            "host ports {}",
            serde_json::to_string(&spec.ports).unwrap_or_default()
        ));
    }
    if container.privileged {
        lines.push("privileged".to_owned());
    }
    lines
}

#[cfg(test)]
#[path = "rule_draft_tests.rs"]
mod tests;
