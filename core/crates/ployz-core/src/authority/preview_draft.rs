//! The preview rule an approver's client drafts from the Store's preview rule view, and
//! what it shows the person before they sign. One signature covers every preview of one
//! repository: each copies the start-from Environment, its Parent, into a preview Namespace
//! of its own and opens the Parent's Sealed values.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use super::{
    canonical::{self, CanonicalError},
    deploy::content_addressed,
    keys::Signed,
    rule::{
        AutoDeployRule, BuildPolicy, GithubSource, PinnedService, PolicyCache, PreviewTerms,
        RuleEvent, RuleService, RuleSource, Runner, SealedDigest, preview_template,
    },
    rule_draft::{
        RuleBasis, RuleDraft, RuleDraftError, RuleImageView, RuleServiceSummary, RuleSourceView,
        RuleTerms, WORKFLOW,
    },
    shown::Shown,
};
use crate::{Namespace, RequestedServiceSpec, SealedRecord};

/// What a preview rule is drafted from, as the Store's preview rule view gives it: the
/// Parent, its Namespace, the rule kept for the repository, where the repository's Git
/// Services build from, the Services that run an image, and every Service lowered as a
/// preview's Deployment would send it in `sample`. Other fields of the view are ignored.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct PreviewBasis {
    /// The start-from Environment, whose Sealed values each preview opens.
    pub parent: Uuid,
    /// Its name, as the person reads it.
    #[serde(default)]
    pub parent_name: Option<String>,
    /// The Parent's Namespace, the rule's own.
    pub namespace: Namespace,
    /// The preview Namespace `target` was lowered in.
    pub sample: Namespace,
    pub rule: Option<Signed<AutoDeployRule>>,
    pub sources: Vec<RuleSourceView>,
    #[serde(default)]
    pub images: Vec<RuleImageView>,
    pub target: Vec<RequestedServiceSpec>,
    #[serde(default)]
    pub volumes: super::PlannedVolumes,
}

impl PreviewBasis {
    /// The preview rule kept for this repository, when it reads.
    #[must_use]
    pub fn kept(&self) -> Option<AutoDeployRule> {
        self.rule.as_ref().and_then(|rule| rule.open().ok())
    }

    /// The same view as a [`RuleBasis`], which a summary and [`AutoDeployRule::ending`]
    /// read. Its Namespace is the sample's, where `target` lives.
    #[must_use]
    pub fn rule_basis(&self) -> RuleBasis {
        RuleBasis {
            namespace: self.sample.clone(),
            rule: self.rule.clone(),
            sources: self.sources.clone(),
            images: self.images.clone(),
            target: self.target.clone(),
            volumes: self.volumes.clone(),
            minted: self.own_volumes(),
        }
    }

    /// The birth of each Volume `target` mounts that the sample preview derives for itself:
    /// each preview's own, which starts empty.
    fn own_volumes(&self) -> super::Births {
        self.target
            .iter()
            .flat_map(|spec| spec.volume_graph().volumes().to_vec())
            .filter_map(|volume| {
                let birth = volume.source.birth().filter(|birth| birth.is_preview())?;
                Some((volume.source.docker_volume_name()?.clone(), birth.clone()))
            })
            .collect()
    }
}

/// What a person reads of a preview rule beyond any rule's summary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct PreviewSummary {
    /// The Environment previews start from, by name when the view gave one.
    pub start_from: String,
    /// The Services each preview runs from an image the rule pins by digest.
    pub pinned: Vec<RuleServiceSummary>,
    /// Each Sealed variable a preview opens, as `service.KEY`.
    pub secrets: Vec<String>,
}

impl AutoDeployRule {
    /// The preview rule that deploys every pull request of `basis`' repository as a copy of
    /// its Parent: each Git Service builds under the rule with its build policy, and each
    /// Service that runs an image runs it pinned by digest. The kept rule's next generation
    /// when it is for the same repository and Parent, else a new rule's first. Nobody's
    /// pushes are singled out: Cloud decides whose pull requests deploy.
    ///
    /// # Errors
    /// Returns a [`RuleDraftError`] naming a Service no preview rule can cover.
    pub fn draft_preview(basis: &PreviewBasis, terms: RuleTerms) -> Result<Self, RuleDraftError> {
        let first = basis.sources.first().ok_or(RuleDraftError::Nothing)?;
        let (repository, repository_id) = (first.repository.clone(), first.repository_id);
        let mut services = Vec::new();
        let mut pinned = Vec::new();
        for spec in &basis.target {
            let name = &spec.name;
            if let Some(source) = basis.sources.iter().find(|source| source.service == *name) {
                if source.repository_id != repository_id {
                    return Err(RuleDraftError::OtherRepository {
                        service: name.clone(),
                        repository: source.repository.clone(),
                    });
                }
                services.push(preview_service(
                    spec,
                    source,
                    &basis.sample,
                    &terms.platform,
                )?);
            } else if basis.images.iter().any(|image| image.service == *name) {
                if !content_addressed(&spec.container.image) {
                    return Err(RuleDraftError::Unpinned {
                        service: name.clone(),
                        image: spec.container.image.clone(),
                    });
                }
                pinned.push(PinnedService {
                    name: name.clone(),
                    template: preview_template(spec, &basis.sample, true)?,
                });
            } else {
                return Err(RuleDraftError::Uncovered(name.clone()));
            }
        }
        services.sort_by(|left, right| left.name.cmp(&right.name));
        pinned.sort_by(|left, right| left.name.cmp(&right.name));
        let mut machines = terms.machines;
        machines.sort_unstable();
        machines.dedup();
        // A Machine refuses a later generation that moves a rule to another repository or
        // Namespace, and the Parent is what each preview opens, so either starts a new rule.
        let (rule, generation) = match basis.kept() {
            Some(kept)
                if kept.namespace == basis.namespace
                    && kept
                        .preview
                        .as_ref()
                        .is_some_and(|preview| preview.parent == basis.parent)
                    && kept
                        .source
                        .github()
                        .is_some_and(|github| github.repository_id == repository_id) =>
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
                // A preview rule builds the branch each token names, never one of its own.
                branch: String::new(),
                workflow: WORKFLOW.to_owned(),
                events: vec![RuleEvent::Push, RuleEvent::WorkflowDispatch],
                builder: terms.builder,
                runner: Runner::GithubHosted,
            }),
            services,
            issued_at: terms.issued_at,
            ended: false,
            signer: terms.signer,
            proposal: None,
            preview: Some(PreviewTerms {
                parent: basis.parent,
                pushers: Vec::new(),
                pinned,
            }),
        })
    }

    /// This preview rule's canonical body and what it shows the person, compared with the
    /// rule kept in `basis`.
    ///
    /// # Errors
    /// Returns a [`CanonicalError`] when the rule has no canonical form.
    pub fn drafted_preview(&self, basis: &PreviewBasis) -> Result<RuleDraft, CanonicalError> {
        let rule_basis = basis.rule_basis();
        let mut summary = self.summary(&rule_basis);
        let kept = basis.kept().filter(|kept| kept.rule == self.rule);
        let pinned = self.preview.iter().flat_map(|preview| &preview.pinned);
        let mounts = super::Mounts {
            namespace: &rule_basis.namespace,
            volumes: &rule_basis.volumes,
            minted: &rule_basis.minted,
            now: self.issued_at,
            against: None,
        };
        summary.preview = Some(PreviewSummary {
            start_from: basis
                .parent_name
                .clone()
                .unwrap_or_else(|| basis.parent.to_string()),
            pinned: pinned
                .map(|pinned| {
                    let spec = basis.target.iter().find(|spec| spec.name == pinned.name);
                    let before = kept
                        .as_ref()
                        .and_then(|kept| kept.preview.as_ref())
                        .map(|preview| preview.pinned.iter().find(|old| old.name == pinned.name));
                    RuleServiceSummary {
                        name: pinned.name.clone(),
                        builds: spec
                            .map(|spec| {
                                Shown::words("runs ").and_text(&spec.container.image).into()
                            })
                            .unwrap_or_default(),
                        changes: match before {
                            None => Vec::new(),
                            Some(None) => vec!["new to the rule".to_owned()],
                            Some(Some(old)) if old.template != pinned.template => {
                                vec!["settings".to_owned()]
                            }
                            Some(Some(_)) => Vec::new(),
                        },
                        settings: spec
                            .map(|spec| super::rule_draft::settings(spec, mounts.asked(spec)))
                            .unwrap_or_default()
                            .into_iter()
                            .map(String::from)
                            .collect(),
                        confirm: spec
                            .map(|spec| mounts.asked(spec).confirm)
                            .unwrap_or_default()
                            .iter()
                            .map(ToString::to_string)
                            .collect(),
                    }
                })
                .collect(),
            secrets: secrets(&basis.target),
        });
        Ok(RuleDraft {
            body: canonical::to_canonical(self)?,
            summary,
            births: super::Births::new(),
        })
    }
}

/// What a preview rule approves for a Git Service: its preview template less the image,
/// the policy its build runs with, with no cache, and its build's Sealed values.
fn preview_service(
    spec: &RequestedServiceSpec,
    source: &RuleSourceView,
    sample: &Namespace,
    platform: &str,
) -> Result<RuleService, RuleDraftError> {
    let policy = BuildPolicy::derive(
        &source.root_dir,
        &source.build,
        spec,
        BTreeSet::from([platform.to_owned()]),
        PolicyCache::Off,
    );
    let secrets = SealedRecord::of(spec)?
        .map(|record| SealedDigest::of(&record))
        .transpose()?;
    Ok(RuleService {
        name: spec.name.clone(),
        template: preview_template(spec, sample, false)?,
        policy: Some(policy),
        secrets,
        seeded: None,
    })
}

/// Each Sealed variable of `target`, as `service.KEY`.
fn secrets(target: &[RequestedServiceSpec]) -> Vec<String> {
    target
        .iter()
        .flat_map(|spec| {
            spec.container
                .environment
                .iter()
                .filter(|(_, value)| value.is_sealed())
                .map(move |(key, _)| format!("{}.{key}", spec.name))
        })
        .collect()
}

#[cfg(test)]
#[path = "preview_draft_tests.rs"]
mod tests;
