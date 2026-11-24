//! An Environment's automatic deploy rule: a person's standing approval of every push to
//! one branch, signed by a Device or a passkey's own prompt. Cloud keeps the signed rule and relays it with each push's
//! Deployment; every Machine checks it, so nothing here admits a deploy. The Store only
//! holds a push whose Saved settings the rule no longer approves, so a settings change
//! waits for a person instead of failing on every Machine.

use ployz_core::authority::{
    AutoDeployRule, Births, BuildPolicy, PlannedVolumes, RuleBasis, RuleId, Signed, rule::template,
    rule_draft::RuleSourceView,
};
use ployz_core::config::{SavedEnvironmentIntent, ServiceSource, canonicalize_environment_intent};
use ployz_core::{Namespace, RequestedServiceSpec, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::id::{BranchName, RepositoryId};
use crate::storage::Tx;
use crate::{Actor, EnvironmentId, EnvironmentRef, EnvironmentSummary, Trusted, error, scope};

/// Keep the rule a Device or passkey signed for an Environment. Cloud checks only that it fits
/// the Environment; the Machines check the signature.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ApproveRule {
    #[serde(default)]
    pub environment: EnvironmentRef,
    #[ts(as = "Signed<AutoDeployRule>")]
    pub rule: Box<Signed<AutoDeployRule>>,
    /// The births the approval minted for Volumes its Services mount first, by the name
    /// their specs declare each under. The Store records them, so every push's specs
    /// carry them.
    #[serde(default, skip_serializing_if = "Births::is_empty")]
    #[ts(optional, as = "Option<Births>")]
    pub births: Births,
}

/// The rule a Save's tap signed, kept in the Save's own transaction once it saved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SavedRule {
    #[ts(as = "Signed<AutoDeployRule>")]
    pub rule: Box<Signed<AutoDeployRule>>,
    /// As [`ApproveRule::births`].
    #[serde(default, skip_serializing_if = "Births::is_empty")]
    #[ts(optional, as = "Option<Births>")]
    pub births: Births,
    /// The rule kept when the tap read the Save, none when there was none. A Save that
    /// finds another kept refuses, as a stale review does, and saves nothing.
    pub prior: Option<RulePrior>,
}

/// One generation of a rule.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RulePrior {
    pub rule: RuleId,
    #[ts(type = "number")]
    pub generation: u64,
}

/// An Environment's rule, and what a Device needs to sign its next generation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuleQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Read what a Save would save, staged changes included, rather than the latest Saved
    /// revision, with the review version that Save names.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub staged: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct RuleView {
    pub environment: EnvironmentSummary,
    /// The Namespace its Deployments run in.
    pub namespace: Namespace,
    /// The rule a Device or a passkey last signed, ended or not.
    pub rule: Option<Signed<AutoDeployRule>>,
    /// Each Saved Service that builds from a GitHub repository.
    pub sources: Vec<RuleSourceView>,
    /// The latest Saved revision lowered as a push's Deployment would: the settings a
    /// rule approves, less each built image.
    pub target: Vec<RequestedServiceSpec>,
    /// Services the rule covers whose Saved settings it no longer approves.
    pub changed: Vec<ServiceName>,
    /// The latest Saved revision's Volumes, by the name `target` declares each under.
    #[serde(default)]
    pub volumes: PlannedVolumes,
    /// For a staged read, the review version a Save of what it read names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version: Option<String>,
    /// For a staged read, whether the Environment is Protected, read with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub protected: Option<bool>,
}

impl RuleView {
    /// What a rule is drafted from.
    #[must_use]
    pub fn basis(&self) -> RuleBasis {
        RuleBasis {
            namespace: self.namespace.clone(),
            rule: self.rule.clone(),
            sources: self.sources.clone(),
            target: self.target.clone(),
            volumes: self.volumes.clone(),
            minted: Births::new(),
        }
    }
}

/// The rule an Environment holds, ended or not.
pub(crate) fn stored(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<Signed<AutoDeployRule>>, RpcError> {
    let rows = tx.query(
        "SELECT rule FROM config_auto_deploy_rule WHERE environment_id = ?1",
        &[environment.as_str().into()],
    )?;
    rows.first()
        .map(|row| row.json(0, "Automatic deploy rule"))
        .transpose()
}

/// `rule`'s terms. A kept rule that won't open counts as no rule: nothing it would
/// allow or hold back applies.
pub(crate) fn opened(rule: &Signed<AutoDeployRule>) -> Option<AutoDeployRule> {
    rule.open().ok()
}

/// Keep `c.rule` as its Environment's rule. A rule for another Namespace, or one that
/// doesn't supersede the one kept, is refused: every Machine would refuse it.
pub(crate) fn approve(tx: &mut dyn Tx, who: &Actor, c: &ApproveRule) -> Result<RuleView, RpcError> {
    let environment = scope::lock(tx, who, &c.environment)?;
    keep(tx, who, &environment, (&c.rule, &c.births))?;
    view_of(tx, who, &environment, None, false)
}

/// Keep the rule a Save's tap signed, once the Save saved under `environment`'s lock,
/// unless the rule kept moved since the tap read it.
pub(crate) fn keep_saved(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &scope::Environment,
    saved: &SavedRule,
) -> Result<(), RpcError> {
    let kept = stored(tx, &environment.summary.id)?
        .as_ref()
        .and_then(opened)
        .map(|kept| RulePrior {
            rule: kept.rule,
            generation: kept.generation,
        });
    if kept != saved.prior {
        return Err(error::conflict(
            "Automatic deploys changed since you looked. Save again.",
            json!({}),
        ));
    }
    keep(tx, who, environment, (&saved.rule, &saved.births))
}

fn keep(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &scope::Environment,
    (signed, births): (&Signed<AutoDeployRule>, &Births),
) -> Result<(), RpcError> {
    let id = environment.summary.id.clone();
    let rule = signed
        .open()
        .map_err(|_| error::invalid("The rule isn't a signed automatic deploy rule", json!({})))?;
    let namespace = crate::deployment::reserve_namespace(tx, who, &environment.summary)?;
    if rule.namespace != namespace {
        return Err(error::invalid(
            "The rule is for another environment's namespace",
            json!({ "namespace": namespace }),
        ));
    }
    if !births.is_empty() {
        let saved = crate::review::latest_saved(tx, &id)?.ok_or_else(|| {
            error::invalid(
                "This environment has no saved settings for the rule's new volumes. Save, then approve again.",
                json!({}),
            )
        })?;
        crate::volume::birth::record(tx, (&id, &namespace), &saved.intent, births)?;
    }
    if let Some(kept) = stored(tx, &id)?.as_ref().and_then(opened)
        && kept.rule == rule.rule
        && kept.generation >= rule.generation
    {
        return Err(error::conflict(
            "A newer approval of this rule is already kept. Approve it again.",
            json!({ "generation": kept.generation }),
        ));
    }
    let text =
        serde_json::to_string(signed).map_err(|_| error::corrupt("Automatic deploy rule"))?;
    let generation = i64::try_from(rule.generation)
        .map_err(|_| error::invalid("The rule's generation is too large", json!({})))?;
    tx.execute(
        "DELETE FROM config_auto_deploy_rule WHERE environment_id = ?1",
        &[id.as_str().into()],
    )?;
    tx.execute(
        "INSERT INTO config_auto_deploy_rule (environment_id, rule, generation, ended) \
         VALUES (?1, ?2, ?3, ?4)",
        &[
            id.as_str().into(),
            text.as_str().into(),
            generation.into(),
            i64::from(rule.ended).into(),
        ],
    )?;
    tracing::info!(
        environment = %id,
        rule = %rule.rule,
        generation = rule.generation,
        ended = rule.ended,
        "kept an automatic deploy rule"
    );
    Ok(())
}

/// `q.environment`'s rule and what signing it next takes.
pub(crate) fn view(
    tx: &mut dyn Tx,
    who: &Actor,
    trusted: &Trusted,
    q: &RuleQuery,
) -> Result<RuleView, RpcError> {
    let environment = scope::environment(tx, who, &q.environment)?;
    view_of(tx, who, &environment, Some(trusted), q.staged)
}

fn view_of(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &scope::Environment,
    trusted: Option<&Trusted>,
    staged: bool,
) -> Result<RuleView, RpcError> {
    let id = &environment.summary.id;
    let rule = stored(tx, id)?;
    let namespace = crate::deployment::namespace(tx, who, &environment.summary)?;
    let (saved, version, protected) = match staged {
        true => {
            let version = crate::review::review(tx, environment)?.view.version;
            let working = canonicalize_environment_intent(environment.working.clone());
            let protected = scope::protected(tx, id)?;
            (Some(working), Some(version), Some(protected))
        }
        false => (
            crate::review::latest_saved(tx, id)?.map(|saved| saved.intent),
            None,
            None,
        ),
    };
    let mut sources = saved.as_ref().map(sources).unwrap_or_default();
    if let Some(saved) = &saved {
        for source in &mut sources {
            let service = (saved.services.iter())
                .find(|service| service.config.private_dns == source.service);
            if let Some(service) = service {
                source.on_push = crate::policy::load(tx, id, &service.id)?.auto_deploy;
            }
        }
    }
    let target = match (trusted, &saved) {
        (Some(trusted), Some(saved)) => {
            crate::deployment::query::saved_target(tx, who, trusted, (environment, saved), &[])?
        }
        _ => Vec::new(),
    };
    let changed = match (rule.as_ref().and_then(opened), &saved) {
        (Some(rule), Some(saved)) if !target.is_empty() => {
            let ships = following(tx, id, saved, &rule)?;
            changed(&rule, &namespace, &target, &sources, &ships)
        }
        _ => Vec::new(),
    };
    let volumes = match &saved {
        Some(saved) => crate::volume::birth::planned(tx, id, saved)?,
        None => PlannedVolumes::new(),
    };
    Ok(RuleView {
        environment: environment.summary.clone(),
        namespace,
        rule,
        sources,
        target,
        changed,
        volumes,
        version,
        protected,
    })
}

/// The repository and branch whose pushes `rule` approves; none when it names one GitHub
/// couldn't have, so no push is its.
pub(crate) fn branch(rule: &AutoDeployRule) -> Option<(RepositoryId, BranchName)> {
    Some((
        RepositoryId::parse(rule.source.repository_id).ok()?,
        BranchName::parse(rule.source.branch.as_str()).ok()?,
    ))
}

/// The Services a push of `rule`'s branch ships as following it: each Saved Service
/// that tracks the branch and deploys on push, as a push's hold counts them. A
/// Service among them the rule doesn't cover holds the push, so the view shows it too.
fn following(
    tx: &mut dyn Tx,
    id: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
    rule: &AutoDeployRule,
) -> Result<Vec<ServiceName>, RpcError> {
    let Some((repository, branch)) = branch(rule) else {
        return Ok(Vec::new());
    };
    let mut ships = Vec::new();
    for service in &saved.services {
        if crate::git::tracks(service, repository, &branch)
            && crate::policy::load(tx, id, &service.id)?.auto_deploy
        {
            ships.push(service.config.private_dns.clone());
        }
    }
    Ok(ships)
}

pub(crate) fn sources(saved: &SavedEnvironmentIntent) -> Vec<RuleSourceView> {
    saved
        .services
        .iter()
        .filter_map(|service| match &service.config.source {
            ServiceSource::Git {
                repository,
                repository_id,
                root_dir,
                branch,
                ..
            } => Some(RuleSourceView {
                service: service.config.private_dns.clone(),
                repository: repository.clone(),
                repository_id: *repository_id,
                branch: crate::git::branch_name(branch).map(str::to_owned),
                root_dir: root_dir.clone(),
                build: service.config.build.clone(),
                on_push: true,
            }),
            ServiceSource::Image { .. } | ServiceSource::Empty { .. } => None,
        })
        .collect()
}

/// The Services a push of `rule`'s branch would deploy that the rule doesn't approve:
/// each it covers whose template in `target`, or whose build policy from `sources`,
/// changed, and each of `ships` it doesn't cover. All of them when the Environment
/// moved to another Namespace.
pub(crate) fn changed(
    rule: &AutoDeployRule,
    namespace: &Namespace,
    target: &[RequestedServiceSpec],
    sources: &[RuleSourceView],
    ships: &[ServiceName],
) -> Vec<ServiceName> {
    let approves = |approved: &ployz_core::authority::RuleService| {
        let Some(spec) = target.iter().find(|spec| spec.name == approved.name) else {
            return false;
        };
        let Some(source) = sources
            .iter()
            .find(|source| source.service == approved.name)
        else {
            return false;
        };
        let policy = BuildPolicy::derive(
            &source.root_dir,
            &source.build,
            spec,
            approved.policy.platforms.clone(),
            approved.policy.cache,
        );
        template(spec, namespace).ok().as_ref() == Some(&approved.template)
            && policy == approved.policy
    };
    let mut changed: Vec<ServiceName> = rule
        .services
        .iter()
        .filter(|approved| rule.namespace != *namespace || !approves(approved))
        .map(|approved| approved.name.clone())
        .collect();
    for service in ships {
        if rule.service(service).is_none() && !changed.contains(service) {
            changed.push(service.clone());
        }
    }
    changed.sort();
    changed
}

/// Why a push waits for the rule's next approval.
pub(crate) fn reapprove(changed: &[ServiceName]) -> String {
    let names = changed
        .iter()
        .map(ServiceName::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{} changed since the automatic deploy rule was approved. Approve it again in the \
         environment's settings, or run ployz github rule approve {names}.",
        changed
            .iter()
            .map(ServiceName::as_str)
            .collect::<Vec<_>>()
            .join(", "),
    )
}
