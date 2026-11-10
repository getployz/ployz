//! An Environment's preview rule: a Device's standing approval of previews of it. Each
//! preview of a pull request runs in a Namespace every Server derives from the rule and
//! the pull request's branch, with settings the rule approves once those derived names
//! are put back. Cloud keeps the signed rule and relays it with each preview's
//! Deployment; every Server checks it, so nothing here admits a deploy.
//!
//! What a Device signs comes from a rehearsal: the Store opens a PR Environment for a
//! sample pull request exactly as a real one opens, lowers it, and undoes every write.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::authority::preview::{PreviewKey, preview_namespace};
use ployz_core::authority::{PreviewRule, RuleId, Signed};
use ployz_core::config::Generator;
use ployz_core::{Namespace, RequestedServiceSpec, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use ployz_core::authority::rule_draft::RuleSourceView;
use crate::storage::Tx;
use crate::{Actor, EnvironmentId, EnvironmentRef, EnvironmentSummary, Trusted, error, scope};

/// Keep the preview rule a Device signed for an Environment. Cloud checks only that it
/// fits the Environment; the Servers check the signature.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ApprovePreviewRule {
    #[serde(default)]
    pub environment: EnvironmentRef,
    #[ts(as = "Signed<PreviewRule>")]
    pub rule: Box<Signed<PreviewRule>>,
}

/// What a Device signs a preview rule of an Environment over, for pull requests of one
/// repository: the Store rehearses opening one and reads back its settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PlanPreviewRule {
    #[serde(default)]
    pub environment: EnvironmentRef,
    #[ts(type = "number")]
    pub repository_id: u64,
    /// The rule's ID: the kept rule's, or a new one the approving client chose.
    pub rule: RuleId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct PreviewRuleView {
    /// The Environment previews start from.
    pub environment: EnvironmentSummary,
    /// Its Namespace.
    pub namespace: Namespace,
    /// The rule a Device last signed, ended or not.
    pub rule: Option<Signed<PreviewRule>>,
    /// Each Saved Service of the Environment that builds from a GitHub repository.
    pub sources: Vec<RuleSourceView>,
    /// The Namespace the rehearsed preview ran in.
    pub sample: Namespace,
    /// The Cluster Domain its generated domains are under.
    pub domain: Option<String>,
    /// The rehearsed preview lowered as a later Deployment of it would be.
    pub target: Vec<RequestedServiceSpec>,
    /// The same, as its first Deployment lowers it, with its Setup Commands: only the
    /// Services that have some.
    pub setup: Vec<RequestedServiceSpec>,
    /// Each Service's Generated Values, by key.
    pub generated: BTreeMap<ServiceName, BTreeMap<String, Generator>>,
    /// Each Service's Sealed values a preview opens from the parent, by key.
    pub inherited: BTreeMap<ServiceName, BTreeSet<String>>,
    /// Volumes a preview mounts from the parent rather than a copy of its own, by the
    /// Services that mount them.
    pub shared: BTreeMap<ServiceName, BTreeSet<String>>,
    /// The GitHub accounts of the Organization's members, as Cloud linked them.
    #[ts(type = "number[]")]
    pub members: Vec<u64>,
}

/// The preview rule an Environment holds, ended or not.
pub(crate) fn stored(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<Signed<PreviewRule>>, RpcError> {
    let rows = tx.query(
        "SELECT rule FROM config_preview_rule WHERE environment_id = ?1",
        &[environment.as_str().into()],
    )?;
    rows.first()
        .map(|row| serde_json::from_str(row.text(0)?).map_err(|_| error::corrupt("Preview rule")))
        .transpose()
}

/// The live preview rule an Environment holds for pull requests of `repository_id`.
pub(crate) fn live(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    repository_id: u64,
) -> Result<Option<PreviewRule>, RpcError> {
    Ok(stored(tx, environment)?
        .and_then(|signed| signed.open().ok())
        .filter(|rule| !rule.ended && rule.source.repository_id == repository_id))
}

/// Keep `c.rule` as its Environment's preview rule. A rule for another Environment, or
/// one that doesn't supersede the one kept, is refused: every Server would refuse it.
pub(crate) fn approve(
    tx: &mut dyn Tx,
    who: &Actor,
    trusted: &Trusted,
    c: &ApprovePreviewRule,
) -> Result<PreviewRuleView, RpcError> {
    let environment = scope::lock(tx, who, &c.environment)?;
    let id = environment.summary.id.clone();
    let rule = c
        .rule
        .open()
        .map_err(|_| error::invalid("The rule isn't a signed preview rule", json!({})))?;
    let namespace = crate::deployment::namespace(tx, who, &environment.summary, true)?;
    if rule.parent != namespace || rule.parent_id.to_string() != id.as_str() {
        return Err(error::invalid(
            "The preview rule is for another environment",
            json!({ "namespace": namespace }),
        ));
    }
    if let Some(kept) = stored(tx, &id)?.and_then(|kept| kept.open().ok())
        && kept.rule == rule.rule
        && kept.generation >= rule.generation
    {
        return Err(error::conflict(
            "A newer approval of this preview rule is already kept. Approve it again.",
            json!({ "generation": kept.generation }),
        ));
    }
    let text = serde_json::to_string(&*c.rule).map_err(|_| error::corrupt("Preview rule"))?;
    let generation = i64::try_from(rule.generation)
        .map_err(|_| error::invalid("The rule's generation is too large", json!({})))?;
    tx.execute(
        "DELETE FROM config_preview_rule WHERE environment_id = ?1",
        &[id.as_str().into()],
    )?;
    tx.execute(
        "INSERT INTO config_preview_rule (environment_id, rule, generation, ended) \
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
        "kept a preview rule"
    );
    plan(
        tx,
        who,
        trusted,
        &PlanPreviewRule {
            environment: c.environment.clone(),
            repository_id: rule.source.repository_id,
            rule: rule.rule,
        },
    )
}

/// The sample pull request a plan rehearses: from the repository itself, on a branch
/// no person names.
fn sample(repository_id: u64) -> Result<crate::PullRequest, RpcError> {
    let repository = crate::RepositoryId::parse(repository_id)?;
    Ok(crate::PullRequest {
        repository_id: repository,
        number: crate::PullRequestNumber::parse(1)?,
        title: "Preview rule sample".to_owned(),
        author: "ployz".to_owned(),
        author_id: None,
        bot: false,
        head_branch: crate::BranchName::parse(SAMPLE_BRANCH)?,
        head_repository_id: Some(repository),
        head: crate::CommitSha::parse("0".repeat(40))?,
        target_branch: crate::BranchName::parse("main")?,
        commits: 1,
        open: true,
        merge_commit: None,
        target_head: None,
        updated: crate::GithubTimestamp::parse("2026-01-01T00:00:00Z")?,
    })
}

/// The sample pull request's branch.
const SAMPLE_BRANCH: &str = "ployz-preview-sample";

/// The sample preview's key, whose Namespace a plan derives.
pub(crate) fn sample_key() -> PreviewKey {
    PreviewKey::Branch(SAMPLE_BRANCH.to_owned())
}

/// What signing `c.rule` next takes: the rehearsed preview's settings, and the rule
/// kept now.
pub(crate) fn plan(
    tx: &mut dyn Tx,
    who: &Actor,
    trusted: &Trusted,
    c: &PlanPreviewRule,
) -> Result<PreviewRuleView, RpcError> {
    let parent = scope::environment(tx, who, &c.environment)?;
    let namespace = crate::deployment::namespace(tx, who, &parent.summary, false)?;
    let rule = stored(tx, &parent.summary.id)?;
    let saved = crate::review::latest_saved(tx, &parent.summary.id)?;
    let sources = saved
        .as_ref()
        .map(|saved| crate::auto_rule::sources(&saved.intent))
        .unwrap_or_default();
    let event = sample(c.repository_id)?;
    let sample = preview_namespace(c.rule, &sample_key());
    let rehearsed = crate::storage::rehearse(tx, |tx| {
        let Some(opened) =
            crate::pull_request::rehearse(tx, who, &parent, (&event, trusted), c.rule)?
        else {
            return Err(error::invalid(
                "No Service of this environment deploys from that repository",
                json!({}),
            ));
        };
        let later = crate::deployment::query::saved_target_with(
            tx,
            who,
            trusted,
            (&opened, &opened.working),
            false,
        )?;
        let first = crate::deployment::query::saved_target_with(
            tx,
            who,
            trusted,
            (&opened, &opened.working),
            true,
        )?;
        Ok((opened.working, later, first))
    })?;
    let (working, target, first) = rehearsed;
    let setup = first
        .into_iter()
        .filter(|spec| !target.contains(spec))
        .collect();
    let generated = working
        .services
        .iter()
        .filter_map(|service| {
            let name = ServiceName::parse(service.slug.as_str()).ok()?;
            let generated: BTreeMap<String, Generator> = service
                .variables
                .iter()
                .filter_map(|variable| Some((variable.key.clone(), variable.generator()?.clone())))
                .collect();
            (!generated.is_empty()).then_some((name, generated))
        })
        .collect();
    let inherited = target
        .iter()
        .filter_map(|spec| {
            let keys: BTreeSet<String> = spec
                .container
                .environment
                .values()
                .chain(
                    spec.pre_deploy
                        .iter()
                        .flat_map(|hook| hook.environment.values()),
                )
                .flat_map(ployz_core::EnvValue::sealed_parts)
                .filter(|value| value.environment.is_some())
                .map(|value| value.key.clone())
                .collect();
            (!keys.is_empty()).then(|| (spec.name.clone(), keys))
        })
        .collect();
    let shared = shared_mounts(&target);
    Ok(PreviewRuleView {
        environment: parent.summary.clone(),
        namespace,
        rule,
        sources,
        sample,
        domain: trusted
            .domains
            .cluster_domain
            .as_ref()
            .map(|cluster| cluster.name.to_string()),
        target,
        setup,
        generated,
        inherited,
        shared,
        members: trusted
            .members
            .iter()
            .flatten()
            .map(|member| member.get())
            .collect(),
    })
}

/// The Docker Volumes each Service of `target` mounts from outside its own preview:
/// data a preview shares with the Environment it starts from.
fn shared_mounts(target: &[RequestedServiceSpec]) -> BTreeMap<ServiceName, BTreeSet<String>> {
    target
        .iter()
        .filter_map(|spec| {
            let shared: BTreeSet<String> = spec
                .mount_graph
                .volume_graph()
                .mounted_volumes()
                .filter_map(|volume| match volume.source.kind() {
                    ployz_core::RawVolumeSource::External { name } => Some(name.to_string()),
                    ployz_core::RawVolumeSource::Bind { .. }
                    | ployz_core::RawVolumeSource::Ordinary { .. }
                    | ployz_core::RawVolumeSource::Provisioned { .. }
                    | ployz_core::RawVolumeSource::Tmpfs { .. } => None,
                })
                .collect();
            (!shared.is_empty()).then(|| (spec.name.clone(), shared))
        })
        .collect()
}
