//! A person's review of an untrusted pull request's preview: what their client signs it
//! over, and the reviews an approval carries.

use std::collections::BTreeMap;

use ployz_core::authority::preview::{PreviewKey, preview_namespace};
use ployz_core::authority::{PreviewRule, Review, Signed};
use ployz_core::{Namespace, RequestedServiceSpec, RpcError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use super::{ApprovePullRequest, preview_key};
use crate::id::{CommitSha, EnvironmentId};
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::storage::Tx;
use crate::{Actor, RuleQuery, Trusted, branch, error};

/// A person's review of a pull request's head in one of its PR Environments.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentReview {
    pub environment: EnvironmentId,
    pub review: Review,
}

/// What a person's client signs a review of a PR Environment's preview over.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PreviewReviewQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct PreviewReviewView {
    pub environment: EnvironmentSummary,
    /// The Namespace the preview runs in, which the rule and its key derive.
    pub namespace: Namespace,
    /// The live preview rule of the Environment it starts from.
    #[ts(as = "Signed<PreviewRule>")]
    pub rule: Box<Signed<PreviewRule>>,
    /// Which preview of the rule it is.
    pub key: PreviewKey,
    /// The pull request's head, which a review approves.
    pub commit: CommitSha,
    /// The preview's latest Saved revision lowered as its next Deployment would be.
    pub target: Vec<RequestedServiceSpec>,
}

/// `q.environment`'s preview and what reviewing its head takes. Refused for an
/// Environment that is no pull request's, or whose previews no live rule covers.
pub(crate) fn preview_review(
    tx: &mut dyn Tx,
    who: &Actor,
    trusted: &Trusted,
    q: &PreviewReviewQuery,
) -> Result<PreviewReviewView, RpcError> {
    let invalid = |message: &str| error::invalid(message.to_owned(), json!({}));
    let environment = scope::environment(tx, who, &q.environment)?;
    let id = environment.summary.id.clone();
    let pr = crate::pr_environment::of(tx, &id)?
        .ok_or_else(|| invalid("This environment isn't a pull request's preview"))?;
    let facts = super::facts(tx, who, pr.repository_id, pr.number)?
        .ok_or_else(|| invalid("This environment isn't a pull request's preview"))?;
    let (signed, rule) = rule(tx, &id)?
        .filter(|(_, rule)| rule.source.repository_id == facts.repository_id.get())
        .ok_or_else(|| invalid("This environment's previews run under no preview rule"))?;
    let key = preview_key(&facts);
    let view = crate::auto_rule::view(
        tx,
        who,
        trusted,
        &RuleQuery {
            environment: q.environment.clone(),
        },
    )?;
    if view.namespace != preview_namespace(rule.rule, &key) {
        return Err(invalid(
            "This preview opened before its preview rule. Close and reopen the pull request.",
        ));
    }
    Ok(PreviewReviewView {
        environment: view.environment,
        namespace: view.namespace,
        rule: Box::new(signed),
        key,
        commit: facts.head,
        target: view.target,
    })
}

/// Whether PR Environment `id` runs under a live preview rule of its repository that
/// doesn't trust `facts`' author, or whose code comes from a fork. Each Server then
/// starts a head only once a person reviews it, whatever Cloud's members say. A Server
/// checks the account that pushed; Cloud knows only the author.
pub(super) fn needs_review(
    tx: &mut dyn Tx,
    id: &EnvironmentId,
    facts: &super::PullRequest,
) -> Result<bool, RpcError> {
    let Some((_, rule)) = rule(tx, id)? else {
        return Ok(false);
    };
    if rule.source.repository_id != facts.repository_id.get() {
        return Ok(false);
    }
    let own = facts.head_repository_id == Some(facts.repository_id);
    let trusted = facts
        .author_id
        .is_some_and(|author| rule.trusted.contains(&author.get()));
    Ok(!(own && trusted))
}

/// The live preview rule of the Environment PR Environment `id` starts from.
fn rule(
    tx: &mut dyn Tx,
    id: &EnvironmentId,
) -> Result<Option<(Signed<PreviewRule>, PreviewRule)>, RpcError> {
    Ok(branch::row(tx, id)?
        .map(|branch| crate::preview_rule::stored(tx, &branch.parent))
        .transpose()?
        .flatten()
        .and_then(|signed| signed.open().ok().map(|rule| (signed, rule)))
        .filter(|(_, rule)| !rule.ended))
}

/// Each review in `approval`, by the PR Environment it names: one of the pull request's
/// own, under its parent's live preview rule, of the approved head and that Environment.
/// Cloud checks no signature, since every Server does; this only keeps a review from
/// riding on a Deployment it can't be for.
pub(super) fn reviews(
    tx: &mut dyn Tx,
    approval: &ApprovePullRequest,
    current: &[(EnvironmentId, crate::id::ProjectId)],
) -> Result<BTreeMap<EnvironmentId, ployz_core::authority::Review>, RpcError> {
    let invalid = |message: &str| Err(error::invalid(message.to_owned(), json!({})));
    let mut reviews = BTreeMap::new();
    for EnvironmentReview {
        environment,
        review,
    } in &approval.reviews
    {
        if !current.iter().any(|(id, _)| id == environment) {
            return invalid("The review is for another pull request's environment");
        }
        let Some(body) = review.opened() else {
            return invalid("The review isn't a readable preview review");
        };
        let Some((_, rule)) = rule(tx, environment)? else {
            return invalid("This environment's previews run under no preview rule");
        };
        if body.rule != rule.rule || body.generation != rule.generation {
            return invalid(
                "The review is for another approval of the preview rule. Review again.",
            );
        }
        if body.commit.to_string() != approval.head.as_str()
            || body.environment.to_string() != environment.as_str()
        {
            return invalid("The review is for another commit or environment. Review again.");
        }
        if reviews
            .insert(environment.clone(), review.clone())
            .is_some()
        {
            return invalid("Each environment takes one review");
        }
    }
    Ok(reviews)
}
