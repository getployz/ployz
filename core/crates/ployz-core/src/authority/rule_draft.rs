//! The automatic deploy rule an approver's client drafts from an Environment's Saved
//! settings, and what it shows the person before they sign. The CLI and the dashboard both
//! draft here, so a rule a passkey answers in the browser has the bytes a Device would
//! sign for the same settings.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ts_rs::TS;

use super::{
    canonical::{self, CanonicalError, Timestamp},
    deploy::drop_nulls,
    keys::Signed,
    restart::SignerKey,
    rule::{
        AutoDeployRule, BuildPolicy, Builder, PolicyCache, PolicyMethod, RuleEvent, RuleId,
        RuleService, RuleSource, Runner, SealedDigest, template,
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
                // A push builds its head; a rebuild of an older commit a person approved
                // runs on a dispatch.
                events: vec![RuleEvent::Push, RuleEvent::WorkflowDispatch],
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
            repository: Shown::text(&self.source.repository).into(),
            branch: Shown::text(&self.source.branch).into(),
            workflow: Shown::text(&self.source.workflow).into(),
            builder: match builder.r#ref.ends_with(&sha) {
                true => Shown::text(&builder.r#ref),
                false => Shown::text(&builder.r#ref).and_words(" at ").and_text(&sha),
            }
            .into(),
            pinned: *builder == Builder::pinned(),
            servers: self.machines.len(),
            generation: self.generation,
            ended: self.ended,
            services: self
                .services
                .iter()
                .map(|service| RuleServiceSummary {
                    name: service.name.clone(),
                    builds: builds(&service.policy).into(),
                    changes: match &kept {
                        Some(kept) => changes(kept.service(&service.name), service),
                        None => Vec::new(),
                    },
                    settings: basis
                        .target
                        .iter()
                        .find(|spec| spec.name == service.name)
                        .map(|spec| settings(spec, Signs::Template))
                        .unwrap_or_default()
                        .into_iter()
                        .map(String::from)
                        .collect(),
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

/// What a statement signs of a spec beyond its name, which decides what its summary leaves
/// out as shown elsewhere or replaced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Signs {
    /// A rule's or a review's template, whose image and pull policy are placeholders.
    Template,
    /// A deploy approval, which shows its image on the Service's own line.
    Deploy,
}

/// The settings a person reads before approving: each variable, Sealed parts by name only,
/// the pre-deploy command, then every other field `signs` covers whose value is not the
/// spec's default, by its wire name. A field added to the spec later shows without a
/// change here, and a value shows whenever it signs differently from the default, so no
/// two specs that sign differently read the same, but for a Sealed ciphertext.
#[must_use]
pub fn settings(spec: &RequestedServiceSpec, signs: Signs) -> Vec<Shown> {
    settings_edited(spec, signs, |_| {})
}

/// [`settings`] of `spec`'s JSON after `edit` changed it, as a digest that edits the JSON
/// it hashes, such as a preview template's placeholders, reads it.
pub(super) fn settings_edited(
    spec: &RequestedServiceSpec,
    signs: Signs,
    edit: impl FnOnce(&mut Value),
) -> Vec<Shown> {
    let mut value = serde_json::to_value(spec).expect("a spec serializes to JSON");
    drop_nulls(&mut value);
    edit(&mut value);
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
    if let Some(Value::Array(configs)) = fields.get_mut("configs") {
        configs.iter_mut().for_each(config_text);
    }
    let defaults = defaults();
    // A payload digest leaves out how many copies run and in what order.
    for unsigned in ["name", "mode", "update"] {
        fields.remove(unsigned);
    }
    changed(&mut lines, "", fields, &defaults.service);
    // The image shows on its own line or is a template's placeholder, and the Sealed
    // identity is bound by each value's ciphertext.
    container.remove("image");
    container.remove("sealed_identity");
    if signs == Signs::Template {
        container.remove("pull_policy");
    }
    changed(&mut lines, "", container, &defaults.container);
    if let Some(hook) = hook {
        changed(&mut lines, "pre-deploy ", hook, &defaults.hook);
    }
    lines
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
    sealed.scope == SealedScope::Shared && sealed.environment.is_none()
}

/// `(sealed KEY)`, with the Service it was authored on and the Environment it was sealed
/// for when they are not the Container's own.
fn sealed_part(sealed: &SealedValue) -> Shown {
    let mut shown = Shown::words("(sealed ").and_value(&sealed.key);
    if let SealedScope::Service(service) = sealed.scope {
        shown = shown
            .and_words(" of service ")
            .and_text(&service.to_string());
    }
    if let Some(environment) = sealed.environment {
        shown = shown
            .and_words(" from environment ")
            .and_text(&environment.to_string());
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
mod tests;
