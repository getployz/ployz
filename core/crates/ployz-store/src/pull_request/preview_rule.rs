//! What a person signs to turn previews on for a repository: the preview rule an approver's
//! client drafts from [`PreviewRuleView`] with `AutoDeployRule::draft_preview`.

use std::num::NonZeroU8;

use ployz_core::authority::{
    PlannedVolumes, RuleId,
    rule_draft::{RuleImageView, RuleSourceView},
};
use ployz_core::{MachineId, Namespace, RequestedServiceSpec};
use uuid::Uuid;

use super::*;
use crate::Trusted;

/// What signing `repository`'s preview rule takes, with previews starting from
/// `start_from`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PreviewRuleQuery {
    /// The Project; omitted means the Organization's only Project.
    #[serde(default)]
    pub project: Option<ProjectName>,
    pub repository: RepositoryName,
    /// The Environment each preview copies, its Parent.
    pub start_from: EnvironmentName,
}

/// What a preview rule is drafted from: the Parent's latest Saved revision lowered as a
/// preview's Deployment would send it, in the sample preview Namespace `sample`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct PreviewRuleView {
    /// The Parent's ID, the only Environment whose Sealed values a preview opens.
    #[ts(type = "string")]
    pub parent: Uuid,
    pub parent_name: EnvironmentName,
    /// The Parent's Namespace, which the rule names.
    pub namespace: Namespace,
    /// The preview Namespace `target` was lowered in.
    pub sample: Namespace,
    /// The repository's preview rule as kept, ended or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rule: Option<Signed<AutoDeployRule>>,
    /// Each of the Parent's Services that builds from a GitHub repository, this
    /// repository's first: a preview can build no other's.
    pub sources: Vec<RuleSourceView>,
    /// Each of the Parent's Services that runs a registry's image, which the rule pins.
    pub images: Vec<RuleImageView>,
    pub target: Vec<RequestedServiceSpec>,
    /// The Parent's Volumes, by the name `target` declares each under.
    pub volumes: PlannedVolumes,
    /// Each Sealed variable a preview opens, as `service.KEY`.
    pub secrets: Vec<String>,
    /// The Servers a preview may run on, then those the kept rule names that can't now.
    pub servers: Vec<PreviewServer>,
    /// The most previews open at once.
    #[ts(type = "number")]
    pub limit: NonZeroU8,
}

/// A Server previews may run on. Cloud adds its name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PreviewServer {
    #[ts(as = "String")]
    pub machine_id: MachineId,
    /// Whether it can hold a preview now.
    pub holds: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub why: Option<String>,
}

/// Why a Server the kept rule names gets no new preview.
const CANT_HOLD: &str = "Can't hold a preview: upgrade it, or let it take Services";

pub(crate) fn view(
    tx: &mut dyn Tx,
    who: &Actor,
    trusted: &Trusted,
    q: &PreviewRuleQuery,
) -> Result<PreviewRuleView, RpcError> {
    let project = scope::project(tx, who, q.project.as_ref())?;
    let repository_id = repository(tx, &project, &q.repository)?;
    let parent = scope::environment(
        tx,
        who,
        &EnvironmentRef {
            project: Some(project.name.clone()),
            environment: Some(q.start_from.clone()),
        },
    )?;
    let id = &parent.summary.id;
    if scope::protected(tx, id)? {
        return Err(error::invalid(
            "Previews never start from a Protected Environment",
            json!({ "start_from": q.start_from }),
        ));
    }
    if branch::rows::parent_of(tx, id.as_str())?.is_some() {
        return Err(error::invalid(
            "Previews start from an Environment that isn't a Branch",
            json!({ "start_from": q.start_from }),
        ));
    }
    let saved = review::latest_saved(tx, id)?
        .ok_or_else(|| {
            error::invalid(
                format!(
                    "Save {} first. Previews copy what it last saved.",
                    q.start_from
                ),
                json!({ "start_from": q.start_from }),
            )
        })?
        .intent;
    let kept = load(tx, &project.id, repository_id)?.and_then(|plan| plan.preview);
    let terms = kept.as_ref().and_then(|kept| kept.rule.open().ok());
    let homes = trusted.preview_homes.clone().unwrap_or_default();
    let home = match homes.first() {
        Some(home) => *home,
        None => MachineId::parse("0".repeat(32)).map_err(|_| error::corrupt("Machine ID"))?,
    };
    let rule = terms
        .as_ref()
        .map_or(RuleId::from_bytes([0; 16]), |terms| terms.rule);
    let sample = PreviewNamespace::mint(&rule, &home, [0; 4]);
    let target = deployment::query::preview_target(tx, who, (id, &saved), &sample)?;
    let secrets = (target.iter())
        .flat_map(|spec| {
            (spec.container.environment.iter())
                .filter(|(_, value)| value.is_sealed())
                .map(move |(key, _)| format!("{}.{key}", spec.name))
        })
        .collect();
    let named = (terms.iter())
        .flat_map(|terms| &terms.machines)
        .filter(|machine| !homes.contains(machine))
        .map(|machine| PreviewServer {
            machine_id: *machine,
            holds: false,
            why: Some(CANT_HOLD.to_owned()),
        });
    let servers = (homes.iter())
        .map(|machine| PreviewServer {
            machine_id: *machine,
            holds: true,
            why: None,
        })
        .chain(named)
        .collect();
    let repository = repository_id.get();
    let mut sources = crate::auto_rule::sources(&saved);
    sources.sort_by_key(|source| source.repository_id != repository);
    Ok(PreviewRuleView {
        parent: id.uuid(),
        parent_name: parent.summary.name.clone(),
        namespace: deployment::namespace(tx, who, &parent.summary)?,
        sample: sample.namespace().clone(),
        rule: kept.as_ref().map(|kept| kept.rule.clone()),
        sources,
        images: crate::auto_rule::images(&saved),
        target,
        volumes: crate::volume::birth::planned(tx, id, &saved)?,
        secrets,
        servers,
        limit: kept.map_or(preview::DEFAULT_CAP, |kept| kept.cap),
    })
}
