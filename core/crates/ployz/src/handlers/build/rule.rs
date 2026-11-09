//! `ployz build --cloud`: a build under an automatic deploy rule, on the GitHub run the
//! rule's workflow started.
//!
//! The runner checks out the commit GitHub ran, never one Cloud names. Everything Cloud
//! relays at check-in (the rule, the Cluster key, the attest key, the deployment) goes
//! into the grant token's audience or opens the secrets, so a lie fails a check on the
//! Machine or here. The runner's GitHub token credentials stay in this process: Buildx
//! gets a fresh environment.
//!
//!   check-in ─▶ grant token (holder K, recipient R) ─▶ grant ─▶ secrets opened with A
//!     ─▶ build with the rule's policy ─▶ push ─▶ image token (image, A)

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    BuildGrantId, GithubToken, ServiceName, X25519Secret,
    authority::{
        AttestKey, AutoDeployRule, BuildClaim, BuildPolicy, BuildStage, CommitAuthority, CommitSha,
        ImageDigest, PolicyDigest, RecipientKey, Signed, rule::PolicyCache, rule::PolicyMethod,
    },
    config::{BuildMethod, ServiceSource, parse_service_config},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::BuildCommandError;
use crate::handlers::Error;

/// What Cloud relays to a rule build at check-in. None of it is trusted here: the
/// Machine checks the rule and the audience, and the secrets open only with the real
/// attest key.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CheckedIn {
    /// Cloud's handle for this build, which its reports name.
    pub(super) build: String,
    pub(super) rule: Signed<AutoDeployRule>,
    pub(super) cluster: ployz_core::SealingPublicKey,
    pub(super) attest: AttestKey,
    pub(super) service: ServiceName,
    /// The Deployment's lowering input narrowed to the Service, its Sealed values sealed.
    pub(super) deployment: Value,
}

/// What the runner asks Cloud to relay to the grant Machine. Its own word, which the
/// grant token's audience binds.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GrantAsk<'a> {
    pub(super) proof: &'a GithubToken,
    pub(super) service: &'a ServiceName,
    pub(super) holder: BuildGrantId,
    pub(super) recipient: RecipientKey,
    pub(super) commit: CommitSha,
    pub(super) authority: CommitAuthority,
}

/// The grant Machine Cloud relayed the ask to.
#[derive(Deserialize)]
pub(super) struct Granted {
    pub(super) machine: String,
}

/// The rule's policy for `service`, once the deployment Cloud sent builds exactly it.
///
/// # Errors
/// Refuses a rule without the Service, and a deployment whose build inputs differ from
/// the policy in any field, before any token is asked for.
pub(super) fn policy<'rule>(
    rule: &'rule AutoDeployRule,
    service: &ServiceName,
    deployment: &Value,
) -> Result<&'rule BuildPolicy, Error> {
    let policy = &rule
        .service(service)
        .ok_or(BuildCommandError::Rule(
            "the rule does not build this Service",
        ))?
        .policy;
    if derived(deployment, service, policy)? != *policy {
        return Err(BuildCommandError::Rule(
            "the deployment Cloud sent does not build with the rule's policy; refusing to build",
        )
        .into());
    }
    Ok(policy)
}

/// The policy `deployment` builds `service` with. Platforms and cache are the rule's
/// own: the build takes them from the policy, not from Cloud.
fn derived(
    deployment: &Value,
    service: &ServiceName,
    rule: &BuildPolicy,
) -> Result<BuildPolicy, Error> {
    let config = deployment
        .get("snapshots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|snapshot| parse_service_config(snapshot.get("config")?.clone()).ok())
        .find(|config| config.settings.private_dns == *service)
        .ok_or(BuildCommandError::Rule(
            "the deployment does not hold the Service",
        ))?;
    let ServiceSource::Git { root_dir, .. } = &config.settings.source else {
        return Err(BuildCommandError::Rule("the Service does not build from Git").into());
    };
    let build = &config.settings.build;
    let method = match build.build_method {
        BuildMethod::Dockerfile => PolicyMethod::Dockerfile {
            path: build
                .dockerfile_path
                .clone()
                .unwrap_or_else(|| "Dockerfile".to_owned()),
        },
        BuildMethod::Railpack => PolicyMethod::Railpack {
            command: build.command.clone(),
        },
    };
    let intent = crate::sdk::preparation::frozen_intent(deployment.clone())?;
    let spec = intent
        .target
        .iter()
        .find(|spec| spec.name == *service)
        .ok_or(BuildCommandError::Rule(
            "the deployment does not hold the Service",
        ))?;
    let mut args = BTreeMap::new();
    let mut secrets = BTreeSet::new();
    for (name, value) in &spec.container.environment {
        match value.text() {
            Some(text) => {
                args.insert(name.clone(), text.to_owned());
            }
            None => {
                secrets.insert(name.clone());
            }
        }
    }
    Ok(BuildPolicy {
        method,
        context: root_dir.clone(),
        target: None,
        args,
        secrets,
        platforms: rule.platforms.clone(),
        cache: rule.cache,
    })
}

/// The grant token's claim: this build's keys, at `commit`, under the relayed rule.
///
/// # Errors
/// Only when the policy cannot be hashed.
pub(super) fn grant_claim(
    checked_in: &CheckedIn,
    rule: &AutoDeployRule,
    policy: &BuildPolicy,
    commit: CommitSha,
    holder: BuildGrantId,
    recipient: RecipientKey,
) -> Result<BuildClaim, Error> {
    Ok(BuildClaim {
        cluster: checked_in.cluster,
        rule: rule.rule,
        generation: rule.generation,
        service: checked_in.service.clone(),
        policy: PolicyDigest::of(policy).map_err(Error::command)?,
        commit,
        authority: CommitAuthority::Head,
        stage: BuildStage::Grant { holder, recipient },
    })
}

/// The image token's claim: the grant's, naming the pushed `image` and the attest key
/// the secrets opened with.
pub(super) fn built_claim(grant: &BuildClaim, image: ImageDigest, attest: AttestKey) -> BuildClaim {
    BuildClaim {
        stage: BuildStage::Built { image, attest },
        ..grant.clone()
    }
}

/// The Actions cache scope of a rule build: its policy's digest, so a run of another
/// policy or branch never writes what this one reads. `None` keeps the cache off.
pub(super) fn cache_scope(policy: &BuildPolicy) -> Result<Option<String>, Error> {
    match policy.cache {
        PolicyCache::Off => Ok(None),
        PolicyCache::Actions => PolicyDigest::of(policy)
            .map(|digest| Some(digest.to_string()))
            .map_err(Error::command),
    }
}

/// The runner's GitHub OIDC credentials, read once and kept in this process.
pub(super) struct Oidc {
    url: String,
    token: String,
    client: reqwest::Client,
}

impl Oidc {
    /// # Errors
    /// Without `permissions: id-token: write` the run has no credentials.
    pub(super) fn from_env() -> Result<Self, Error> {
        let read = |name| {
            std::env::var(name)
                .ok()
                .filter(|value: &String| !value.is_empty())
        };
        let (Some(url), Some(token)) = (
            read("ACTIONS_ID_TOKEN_REQUEST_URL"),
            read("ACTIONS_ID_TOKEN_REQUEST_TOKEN"),
        ) else {
            return Err(BuildCommandError::Rule(
                "the workflow needs 'permissions: id-token: write'",
            )
            .into());
        };
        Ok(Self {
            url,
            token,
            client: client()?,
        })
    }

    /// A GitHub-signed token for this run whose audience is `audience`.
    ///
    /// # Errors
    /// Reports GitHub's refusal without the token.
    pub(super) async fn token(&self, audience: &str) -> Result<GithubToken, Error> {
        #[derive(Deserialize)]
        struct Issued {
            value: String,
        }
        let response = self
            .client
            .get(&self.url)
            .query(&[("audience", audience)])
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| BuildCommandError::Rule("GitHub did not issue a token for this run"))?;
        if !response.status().is_success() {
            return Err(
                BuildCommandError::Rule("GitHub did not issue a token for this run").into(),
            );
        }
        let issued: Issued = response
            .json()
            .await
            .map_err(|_| BuildCommandError::Rule("GitHub did not issue a token for this run"))?;
        Ok(GithubToken::new(issued.value))
    }
}

/// Cloud's runner API, each call proven by a fresh token whose audience is its origin.
pub(super) struct Cloud {
    origin: String,
    client: reqwest::Client,
}

impl Cloud {
    /// # Errors
    /// Refuses anything but an origin.
    pub(super) fn new(origin: &str) -> Result<Self, Error> {
        let origin = origin.trim_end_matches('/');
        let url = reqwest::Url::parse(origin)
            .map_err(|_| Error::usage("--cloud must be an origin like https://ployz.dev"))?;
        if url.origin().ascii_serialization() != origin {
            return Err(Error::usage(
                "--cloud must be an origin like https://ployz.dev",
            ));
        }
        Ok(Self {
            origin: origin.to_owned(),
            client: client()?,
        })
    }

    /// POST `body` to `path` as this run, reading Cloud's answer.
    ///
    /// # Errors
    /// Names Cloud's status and message: never a token.
    pub(super) async fn post<T: serde::de::DeserializeOwned>(
        &self,
        oidc: &Oidc,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T, Error> {
        let token = oidc.token(&self.origin).await?;
        let response = self
            .client
            .post(format!("{}{path}", self.origin))
            .bearer_auth(token.as_str())
            .json(body)
            .send()
            .await
            .map_err(|_| {
                Error::unavailable(format!("could not reach Ployz Cloud at {}", self.origin))
            })?;
        let status = response.status();
        if status.is_success() {
            return response
                .json()
                .await
                .map_err(|_| Error::unavailable("Ployz Cloud's answer is unreadable".to_owned()));
        }
        let body: Value = response.json().await.unwrap_or_default();
        let message = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_else(|| status.canonical_reason().unwrap_or("refused"));
        Err(Error::unavailable(format!(
            "Ployz Cloud refused the build (HTTP {}): {message}",
            status.as_u16()
        )))
    }
}

fn client() -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(Error::command)
}

/// This build's keys: the holder K its push authenticates with, and the recipient R its
/// secrets are sealed to. Neither leaves the process.
pub(super) struct Keys {
    pub(super) holder_seed: [u8; 32],
    pub(super) recipient: X25519Secret,
}

impl Keys {
    pub(super) fn generate() -> Self {
        Self {
            holder_seed: iroh::SecretKey::generate().to_bytes(),
            recipient: X25519Secret::generate(),
        }
    }

    pub(super) fn holder(&self) -> BuildGrantId {
        let public = iroh::SecretKey::from_bytes(&self.holder_seed).public();
        BuildGrantId::parse(hex::encode(public.as_bytes())).expect("an ed25519 key is 32 bytes")
    }
}

#[cfg(test)]
#[path = "rule_tests.rs"]
mod tests;
