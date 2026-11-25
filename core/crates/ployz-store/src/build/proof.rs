//! What proves a rule build's image to a Server: its run's image token, then what a Server issued once it checked the image online.

use std::collections::BTreeMap;

use ployz_core::authority::{
    AutoDeployRule, BuildClaim, BuildStage, BuiltImageDigest, CommitAuthority, RuleEvidence,
    RuleProof, Signed, VerifiedBuild,
};
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::deployment::{self};
use crate::error;
use crate::id::DeploymentId;
use crate::storage::{Tx, json_text};

use super::github::RunEnd;
use super::{BuildStatus, RowStatus, rows};

/// An image token GitHub's builder reported under a rule, with what its audience names
/// beside the commit: the image, the rule generation and the commit authority it was
/// built under. Cloud's word: each Server rebuilds the claim and checks the token.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageToken {
    pub token: String,
    /// The image's reference, `…@sha256:…`.
    pub image: String,
    pub generation: u64,
    pub authority: CommitAuthority,
}

/// What a Server issued once its online check of a rule build passed: the claim it
/// checked and its verified build. A later Deployment or drain of the image sends both in
/// place of the build's token. Cloud's word, as a token is: each Server checks the MAC
/// against its own Sealing Key, and refuses one for another rule generation or Service.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VerifiedClaim {
    pub claim: BuildClaim,
    pub verified: VerifiedBuild,
}

impl VerifiedClaim {
    fn image(&self) -> Option<BuiltImageDigest> {
        match self.claim.stage {
            BuildStage::Built { image, .. } => Some(image),
            BuildStage::Grant { .. } => None,
        }
    }
}

/// The image token of each Service Deployment `id` built or reused under a rule. A build
/// that ended before the Store kept its token's terms reads its run's token, under
/// `terms`.
pub(crate) fn image_tokens(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    terms: impl Fn(String) -> ImageToken,
) -> Result<BTreeMap<ServiceName, ImageToken>, RpcError> {
    Ok(rows(tx, id)?
        .into_iter()
        .filter(|row| {
            matches!(
                row.stage.status(),
                RowStatus::Reported(BuildStatus::Built | BuildStatus::Reused)
            )
        })
        .filter_map(|row| {
            let token = row
                .image_token
                .or_else(|| match row.stage.github()?.end()? {
                    RunEnd::Built {
                        image_token: Some(token),
                        ..
                    } => Some(terms(token.clone())),
                    RunEnd::Built { .. } | RunEnd::Failed => None,
                })?;
            Some((row.service, token))
        })
        .collect())
}

/// What a Server issued for each Service Deployment `id` built or reused under a rule,
/// once it checked the image online: this Deployment's own, or its source's for a
/// redeploy. Only one issued under the rule and generation its Environment keeps now
/// counts: a Server refuses any other with the current rule.
pub(crate) fn verified(
    tx: &mut dyn Tx,
    id: &DeploymentId,
) -> Result<BTreeMap<ServiceName, VerifiedClaim>, RpcError> {
    let stored = deployment::load(tx, id)?;
    let Some(current) = crate::auto_rule::stored(tx, &stored.summary.environment_id)?
        .as_ref()
        .and_then(crate::auto_rule::opened)
    else {
        return Ok(BTreeMap::new());
    };
    Ok(rows(tx, id)?
        .into_iter()
        .filter(|row| {
            matches!(
                row.stage.status(),
                RowStatus::Reported(BuildStatus::Built | BuildStatus::Reused)
            )
        })
        .filter_map(|row| Some((row.service, row.verified?)))
        .filter(|(_, kept)| {
            (kept.claim.rule, kept.claim.generation) == (current.rule, current.generation)
        })
        .collect())
}

/// Keep what the Servers issued for the images Deployment `id` ran, by runtime Service,
/// for a later Deployment or drain of the same image. Only `runner`, which holds the
/// Deployment, writes it. A Service the Deployment built nothing for is skipped.
///
/// # Errors
/// Returns `conflict` when another runner holds the Deployment, or a storage error.
pub(crate) fn keep_verified(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    runner: &crate::RunnerId,
    kept: &BTreeMap<ServiceName, VerifiedClaim>,
) -> Result<(), RpcError> {
    let stored = deployment::load(tx, id)?;
    if stored.summary.runner.as_ref() != Some(runner) {
        return Err(error::conflict(
            "Another runner holds this Deployment",
            json!({ "deployment": id }),
        ));
    }
    for (service, verified) in kept {
        let verified = json_text(verified);
        tx.execute(
            "UPDATE config_build SET verified = ?3 WHERE deployment_id = ?1 AND service = ?2",
            &[
                id.as_str().into(),
                service.as_str().into(),
                verified.as_str().into(),
            ],
        )?;
    }
    Ok(())
}

/// The newest verified build `organization` keeps for `image` of `service` in
/// `namespace`, under the rule and generation its Environment keeps now, as the proof a
/// Server takes in place of a token: what a drain sends with a copy a rule built, and
/// what a Deploy that reuses the image carries. None when no Server issued one for that
/// image under that generation, which every Server would refuse.
///
/// # Errors
/// Returns a storage error, or `corrupt` for a row that doesn't read.
pub(crate) fn verified_proof(
    tx: &mut dyn Tx,
    organization: &str,
    (namespace, service): (&ployz_core::Namespace, &ServiceName),
    image: BuiltImageDigest,
) -> Result<Option<RuleProof>, RpcError> {
    // The claim names its image in hex, so this narrows the rows; the decode below
    // confirms each.
    let named = format!("%{}%", hex_of(&image));
    let rows = tx.query(
        "SELECT r.rule, b.verified FROM config_build b \
         JOIN config_deployment d ON d.id = b.deployment_id \
         JOIN config_auto_deploy_rule r ON r.environment_id = d.environment_id \
         WHERE b.organization_id = ?1 AND d.namespace = ?2 AND b.service = ?3 \
           AND b.verified LIKE ?4 \
         ORDER BY d.admitted DESC, d.id",
        &[
            organization.into(),
            namespace.as_str().into(),
            service.as_str().into(),
            named.as_str().into(),
        ],
    )?;
    for row in &rows {
        let rule: Signed<AutoDeployRule> = row.json(0, "Automatic deploy rule")?;
        let kept: VerifiedClaim = row.json(1, "verified build")?;
        let Ok(current) = rule.open() else {
            continue;
        };
        if kept.image() == Some(image)
            && kept.claim.service == *service
            && (kept.claim.rule, kept.claim.generation) == (current.rule, current.generation)
        {
            return Ok(Some(RuleProof {
                rule,
                evidence: RuleEvidence::Verified(kept.verified),
                claim: Some(kept.claim),
                reader: None,
            }));
        }
    }
    Ok(None)
}

fn hex_of(image: &BuiltImageDigest) -> String {
    let text = image.to_string();
    text.strip_prefix("sha256:").unwrap_or(&text).to_owned()
}
