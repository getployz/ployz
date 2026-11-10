//! An Environment's automatic deploy rule: a person's standing approval of every push to
//! one branch, signed by a Device or a passkey's own prompt. Cloud keeps the signed rule and relays it with each push's
//! Deployment; every Machine checks it, so nothing here admits a deploy. The Store only
//! holds a push whose Saved settings the rule no longer approves, so a settings change
//! waits for a person instead of failing on every Machine.

use ployz_core::authority::{
    AutoDeployRule, BuildPolicy, RuleBasis, Signed, rule::template, rule_draft::RuleSourceView,
};
use ployz_core::config::{SavedEnvironmentIntent, ServiceSource};
use ployz_core::{Namespace, RequestedServiceSpec, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

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
}

/// An Environment's rule, and what a Device needs to sign its next generation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuleQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
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
        .map(|row| {
            serde_json::from_str(row.text(0)?).map_err(|_| error::corrupt("Automatic deploy rule"))
        })
        .transpose()
}

/// Keep `c.rule` as its Environment's rule. A rule for another Namespace, or one that
/// doesn't supersede the one kept, is refused: every Machine would refuse it.
pub(crate) fn approve(tx: &mut dyn Tx, who: &Actor, c: &ApproveRule) -> Result<RuleView, RpcError> {
    let environment = scope::lock(tx, who, &c.environment)?;
    let id = environment.summary.id.clone();
    let rule = c
        .rule
        .open()
        .map_err(|_| error::invalid("The rule isn't a signed automatic deploy rule", json!({})))?;
    let namespace = crate::deployment::namespace(tx, who, &environment.summary, true)?;
    if rule.namespace != namespace {
        return Err(error::invalid(
            "The rule is for another environment's namespace",
            json!({ "namespace": namespace }),
        ));
    }
    if let Some(kept) = stored(tx, &id)?.and_then(|kept| kept.open().ok())
        && kept.rule == rule.rule
        && kept.generation >= rule.generation
    {
        return Err(error::conflict(
            "A newer approval of this rule is already kept. Approve it again.",
            json!({ "generation": kept.generation }),
        ));
    }
    let text =
        serde_json::to_string(&*c.rule).map_err(|_| error::corrupt("Automatic deploy rule"))?;
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
    view_of(tx, who, &environment, None)
}

/// `q.environment`'s rule and what signing it next takes.
pub(crate) fn view(
    tx: &mut dyn Tx,
    who: &Actor,
    trusted: &Trusted,
    q: &RuleQuery,
) -> Result<RuleView, RpcError> {
    let environment = scope::environment(tx, who, &q.environment)?;
    view_of(tx, who, &environment, Some(trusted))
}

fn view_of(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &scope::Environment,
    trusted: Option<&Trusted>,
) -> Result<RuleView, RpcError> {
    let id = &environment.summary.id;
    let rule = stored(tx, id)?;
    let namespace = crate::deployment::namespace(tx, who, &environment.summary, false)?;
    let saved = crate::review::latest_saved(tx, id)?;
    let sources = saved
        .as_ref()
        .map(|saved| sources(&saved.intent))
        .unwrap_or_default();
    let target = match (trusted, &saved) {
        (Some(trusted), Some(saved)) => crate::deployment::query::saved_target(
            tx,
            who,
            trusted,
            (environment, &saved.intent),
            &[],
        )?,
        _ => Vec::new(),
    };
    let changed = match rule.as_ref().and_then(|rule| rule.open().ok()) {
        Some(rule) if !target.is_empty() => changed(&rule, &namespace, &target, &sources, &[]),
        _ => Vec::new(),
    };
    Ok(RuleView {
        environment: environment.summary.clone(),
        namespace,
        rule,
        sources,
        target,
        changed,
    })
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
                branch: match branch {
                    ployz_core::config::ServiceGitBranch::Connected { name } => Some(name.clone()),
                    ployz_core::config::ServiceGitBranch::Disconnected { .. } => None,
                },
                root_dir: root_dir.clone(),
                build: service.config.build.clone(),
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
