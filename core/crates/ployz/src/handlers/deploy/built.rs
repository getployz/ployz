//! A Device's approval of a redeploy, such as a rollback, on servers that enforce Device
//! approval. A redeploy has no branch head behind it, so the person approves it twice:
//! once to build each commit GitHub has no image of that this client can check, and once,
//! after the builds, for the images themselves.
//!
//! This client checks each image token itself: GitHub's signature against the keys it
//! fetches from GitHub, the claims against the rule a Device the Servers trust signed, and
//! the audience against a claim it rebuilds from its own Cluster's keys. It ignores the
//! token's expiry: the person, not freshness, is the authority now. Then it signs an
//! ordinary deploy approval naming each image by digest, which the Servers check as any
//! other. The Deployment waits, queued, until that approval lands.

use std::collections::BTreeMap;
use std::time::Duration;

use clap::ArgMatches;
use ployz_core::authority::github_oidc::{self, KeySet, TokenRefusal};
use ployz_core::authority::rule::{RuleEvent, verify_signature};
use ployz_core::authority::{
    ApprovedChange, AttestKey, AuthorityRefusal, AutoDeployRule, BuildApproval, BuildClaim,
    BuildStage, CommitAuthority, CommitSha, DeployApproval, DeployTerms, ImageDigest, PolicyDigest,
    Signed, Signer, SignerKey, Timestamp, TrustedKeys, validity,
};
use ployz_core::{
    DescribeContractRequest, InspectRequest, MachineTarget, OperationId, RpcErrorCode, Sealing,
    SealingPublicKey, ServiceName, op,
};
use ployz_store::{
    ApproveDeployment, BuildStatus, BuildView, Cancel, DeploymentId, DeploymentQuery,
    DeploymentSummary, DeploymentTargetQuery, DeploymentView, ImageToken, RuleQuery,
};
use serde_json::json;

use super::super::store::Store;
use super::super::{Error, server};
use super::approve::{self, Enforcing, Probe};

/// How long fetching GitHub's keys may take.
const KEYS_TIMEOUT: Duration = Duration::from_secs(15);
/// How often the redeploy is read while GitHub builds.
const BUILD_LOOK: Duration = Duration::from_secs(3);

/// A redeploy this Device approves once it is admitted.
pub(in crate::handlers) struct Redeploy {
    signer: Signer,
    enforcing: Enforcing,
    /// The facts each image token is checked against, when the redeploy runs an image
    /// GitHub built.
    checking: Option<Facts>,
    /// The person's approval to build each commit no checkable image of is left.
    pub(in crate::handlers) builds: Vec<Signed<BuildApproval>>,
}

/// What a token is checked against: the Cluster's keys, read from a Server over this
/// Device's own connection, the rule a Device those Servers trust signed, and GitHub's keys.
struct Facts {
    attest: AttestKey,
    rule: AutoDeployRule,
    keys: KeySet,
}

/// Prepare to approve a redeploy of `source`, when this CLI holds a Device key and a Server
/// enforces Device approval. None otherwise: the redeploy runs as before, and a Server that
/// enforces refuses what nobody approved. Signs a build approval for each Service GitHub
/// built whose image token can't be checked any more, such as one whose key GitHub rotated
/// out.
///
/// # Errors
/// Refuses before admission when a Service built from an upload or on the Servers, when no
/// rule a trusted Device signed covers a built Service, when a stored image token doesn't
/// hold up, and when the Servers or GitHub can't be read.
pub(in crate::handlers) fn prepare(
    matches: &ArgMatches,
    store: &Store,
    source: &DeploymentId,
) -> Result<Option<Redeploy>, Error> {
    let key = super::super::device::key::path(&super::super::config_path(matches)?);
    let Ok(Some(secret)) = super::super::device::key::load(&key) else {
        return Ok(None);
    };
    // The Servers are asked while this reads the source and, for a built Service, what its
    // token is checked against, so the probe adds no wait of its own.
    let (probe, view, reported) = std::thread::scope(|scope| {
        let probe = scope.spawn(|| approve::probe(matches));
        let view = store.read(&DeploymentQuery { id: source.clone() });
        let reported = view
            .as_ref()
            .is_ok_and(|view| git_builds(view).is_ok_and(|built| !built.is_empty()))
            .then(|| Reported::read(matches));
        (probe.join().unwrap_or(Probe::Unknown), view, reported)
    });
    let enforcing = match probe {
        Probe::Off => return Ok(None),
        Probe::Unknown => {
            crate::ui::warn(
                "Couldn't ask your servers whether they enforce device approval, so this \
                 redeploy isn't signed. Servers that enforce will refuse it.",
            );
            return Ok(None);
        }
        Probe::NoClusterKey => {
            return Err(Error::coded(
                RpcErrorCode::Unavailable,
                "Your servers enforce device approval, but none reported the cluster's key, \
                 so this device can't sign the redeploy. Run ployz status, then redeploy again.",
            ));
        }
        Probe::Enforcing(enforcing) => enforcing,
    };
    let signer = Signer::from_seed(&secret);
    let view = view?;
    let built = git_builds(&view)?;
    if built.is_empty() {
        return Ok(Some(Redeploy {
            signer,
            enforcing,
            checking: None,
            builds: Vec::new(),
        }));
    }
    let reported = reported.unwrap_or_else(|| Reported::read(matches));
    let facts = facts(store, &view, &enforcing, reported)?;
    let mut rebuild = Vec::new();
    for (service, commit, token) in built {
        covered(&facts.rule, service)?;
        let checked = match token {
            Some(token) => check(token, service, commit, &facts, enforcing.cluster)
                .map_err(|refused| refused.error(service, commit))?,
            None => Checked::Rebuild,
        };
        if checked == Checked::Rebuild {
            rebuild.push((service.clone(), commit));
        }
    }
    if !rebuild.is_empty()
        && !facts
            .rule
            .source
            .events
            .contains(&RuleEvent::WorkflowDispatch)
    {
        return Err(Error::coded(
            RpcErrorCode::Conflict,
            format!(
                "Rebuilding {} needs the automatic deploy rule approved again. Run ployz \
                 github rule approve, then redeploy.",
                joined(rebuild.iter().map(|(service, _)| service))
            ),
        ));
    }
    let builds = rebuild
        .into_iter()
        .map(|(service, commit)| build_approval(&signer, &enforcing, &facts.rule, service, commit))
        .collect::<Result<_, _>>()?;
    Ok(Some(Redeploy {
        signer,
        enforcing,
        checking: Some(facts),
        builds,
    }))
}

impl Redeploy {
    /// Wait for `admitted`'s builds, check each image token, then sign the deploy approval
    /// with each built image by digest and hand it to the Store, which lets the redeploy
    /// run. Cancels the redeploy on any refusal, so nothing waits for an approval that
    /// won't come.
    ///
    /// # Errors
    /// Returns a build that failed, a token that doesn't hold up, a tag that can't be
    /// pinned, and the Store's refusal.
    pub(in crate::handlers) fn approve(
        self,
        matches: &ArgMatches,
        store: &Store,
        admitted: &DeploymentSummary,
    ) -> Result<Vec<ApprovedChange>, Error> {
        let approved = self.approve_or_fail(matches, store, admitted);
        if approved.is_err() {
            // A failed cancel leaves Cloud to fail it once its wait for the approval ends.
            let _ = store.try_write(&Cancel {
                deployment: admitted.id.clone(),
            });
        }
        approved
    }

    fn approve_or_fail(
        self,
        matches: &ArgMatches,
        store: &Store,
        admitted: &DeploymentSummary,
    ) -> Result<Vec<ApprovedChange>, Error> {
        let images = match &self.checking {
            Some(facts) => {
                let view = ended_builds(store, &admitted.id)?;
                built_images(&view, facts, self.enforcing.cluster)?
            }
            None => BTreeMap::new(),
        };
        let planned = store.read(&DeploymentTargetQuery {
            id: admitted.id.clone(),
        })?;
        let mut target = planned.target;
        for spec in &mut target {
            if let Some(digest) = images.get(&spec.name) {
                spec.container.image =
                    ployz_build::digest_reference(&spec.container.image, &digest.to_string())
                        .map_err(Error::command)?;
                spec.container.pull_policy = ployz_core::PullPolicy::Never;
            }
        }
        let target = approve::signable(Some(target))?;
        let pins = approve::pin_tags(matches, &target)?;
        let issued_at = Timestamp::now();
        let terms = DeployTerms {
            cluster: self.enforcing.cluster,
            operation: OperationId::from_uuid(admitted.id.as_str()).map_err(Error::command)?,
            machines: self.enforcing.machines,
            issued_at,
            expires_at: issued_at.plus(validity::MAX_LIFETIME_MILLIS),
            signer: SignerKey::Device(self.signer.public().into()),
            proposal: None,
        };
        let (body, approved) = DeployApproval::draft(terms, planned.namespace, &target, &pins)
            .map_err(Error::command)?;
        let signed = Signed::sign(&body, &self.signer).map_err(Error::command)?;
        store.write(&ApproveDeployment {
            deployment: admitted.id.clone(),
            approval: Box::new(signed),
        })?;
        for approved in &approved {
            crate::ui::stream(format_args!("{}", approved.line()));
        }
        Ok(approved)
    }
}

/// A Service built from a commit, with the image token kept for it.
type GitBuild<'a> = (&'a ServiceName, CommitSha, Option<&'a ImageToken>);

/// Each Service `view` built from a commit. One built from an upload has no commit a
/// Device can name.
fn git_builds(view: &DeploymentView) -> Result<Vec<GitBuild<'_>>, Error> {
    view.builds
        .iter()
        .map(|build| match &build.commit {
            Some(commit) => Ok((
                &build.service,
                git_commit(commit)?,
                build.image_token.as_ref(),
            )),
            None => Err(Error::detailed(
                RpcErrorCode::InvalidArgument,
                format!(
                    "{} builds from an upload, and servers that enforce device approval can't \
                     run a build. Deploy it from an image pinned by digest.",
                    build.service
                ),
                json!({ "service": build.service }),
            )),
        })
        .collect()
}

/// What the Servers and GitHub report that a token is checked against.
struct Reported {
    cluster: Result<(SealingPublicKey, AttestKey, TrustedKeys), Error>,
    github: Result<KeySet, Error>,
}

impl Reported {
    /// Ask the Servers and GitHub at once.
    fn read(matches: &ArgMatches) -> Self {
        std::thread::scope(|scope| {
            let github = scope.spawn(github_keys);
            let cluster = cluster_keys(matches);
            Self {
                cluster,
                github: github.join().unwrap_or_else(|_| Err(unread_keys(None))),
            }
        })
    }
}

/// The facts a token is checked against: what the Servers and GitHub `reported`, and the
/// rule a trusted key signed.
fn facts(
    store: &Store,
    view: &DeploymentView,
    enforcing: &Enforcing,
    reported: Reported,
) -> Result<Facts, Error> {
    let environment = ployz_store::EnvironmentRef {
        project: Some(view.environment.project.clone()),
        environment: Some(view.environment.name.clone()),
    };
    let (sealing, attest, trusted) = reported.cluster?;
    if sealing != enforcing.cluster {
        return Err(Error::coded(
            RpcErrorCode::Conflict,
            "Your servers report two cluster keys. Run ployz status, then redeploy again.",
        ));
    }
    let rules = store.read(&RuleQuery { environment })?;
    let Some(signed) = rules.rule else {
        return Err(no_rule(view.environment.name.as_str()));
    };
    let (rule, _) = verify_signature(&signed, &trusted, Timestamp::now()).map_err(|_| {
        Error::coded(
            RpcErrorCode::Conflict,
            format!(
                "No device your servers trust signed {}'s automatic deploy rule. Approve it \
                 again with ployz github rule approve, then redeploy.",
                view.environment.name
            ),
        )
    })?;
    if rule.cluster != enforcing.cluster || rule.namespace != view.namespace {
        return Err(no_rule(view.environment.name.as_str()));
    }
    Ok(Facts {
        attest,
        rule,
        keys: reported.github?,
    })
}

fn no_rule(environment: &str) -> Error {
    Error::coded(
        RpcErrorCode::Conflict,
        format!(
            "{environment} has no automatic deploy rule. Approve one with ployz github rule \
             approve, then redeploy."
        ),
    )
}

/// The Cluster's Sealing key, its build attest key and the keys its Servers trust, from
/// the Server this CLI enters by.
fn cluster_keys(matches: &ArgMatches) -> Result<(SealingPublicKey, AttestKey, TrustedKeys), Error> {
    let context = matches
        .try_get_one::<String>("context")
        .ok()
        .flatten()
        .map(String::as_str);
    super::super::runtime()?.block_on(async {
        let client = server::connect(matches, context).await?;
        let Some(entry) = client.entry_machine() else {
            return Err(unread_cluster());
        };
        let target = MachineTarget::from(&entry);
        let about = client
            .invoke::<op::DescribeContract>(DescribeContractRequest {}, &target, None)
            .await?;
        let details = client
            .invoke::<op::Inspect>(InspectRequest::default(), &target, None)
            .await?;
        match (about.sealing, about.attest) {
            (Some(Sealing::Held { public_key }), Some(attest)) => Ok((
                public_key,
                attest,
                TrustedKeys {
                    devices: details.devices,
                    passkeys: details.passkeys,
                },
            )),
            _ => Err(unread_cluster()),
        }
    })
}

fn unread_cluster() -> Error {
    Error::coded(
        RpcErrorCode::Conflict,
        "Your server doesn't report the cluster's build key. Upgrade Ployz on your servers, \
         then redeploy again.",
    )
}

/// GitHub's token keys, fetched by this client itself.
fn github_keys() -> Result<KeySet, Error> {
    super::super::runtime()?.block_on(async {
        let http = reqwest::Client::builder()
            .user_agent(concat!("ployz-cli/", env!("CARGO_PKG_VERSION")))
            .timeout(KEYS_TIMEOUT)
            .build()
            .map_err(|error| unread_keys(Some(error)))?;
        let body = http
            .get(github_oidc::JWKS_URL)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|error| unread_keys(Some(error)))?
            .bytes()
            .await
            .map_err(|error| unread_keys(Some(error)))?;
        KeySet::parse(&body).map_err(|_| unread_keys(None))
    })
}

fn unread_keys(error: Option<reqwest::Error>) -> Error {
    Error::detailed_caused(
        RpcErrorCode::Unavailable,
        "Couldn't read GitHub's signing keys. Try again.",
        json!({}),
        error,
    )
}

/// Refuse a Service the rule doesn't cover: no Server would run its image.
fn covered(rule: &AutoDeployRule, service: &ServiceName) -> Result<(), Error> {
    match rule.service(service) {
        Some(_) => Ok(()),
        None => Err(Error::detailed(
            RpcErrorCode::Conflict,
            format!(
                "The automatic deploy rule doesn't cover {service}. Approve it for {service} \
                 with ployz github rule approve, then redeploy."
            ),
            json!({ "service": service }),
        )),
    }
}

fn build_approval(
    signer: &Signer,
    enforcing: &Enforcing,
    rule: &AutoDeployRule,
    service: ServiceName,
    commit: CommitSha,
) -> Result<Signed<BuildApproval>, Error> {
    let issued_at = Timestamp::now();
    let body = BuildApproval {
        cluster: enforcing.cluster,
        rule: rule.rule,
        generation: rule.generation,
        service,
        commit,
        issued_at,
        expires_at: issued_at.plus(validity::MAX_LIFETIME_MILLIS),
        signer: SignerKey::Device(signer.public().into()),
    };
    Signed::sign(&body, signer).map_err(Error::command)
}

/// `id` once GitHub ended every build it runs. It reports each build as it starts.
fn ended_builds(store: &Store, id: &DeploymentId) -> Result<DeploymentView, Error> {
    let interrupted = crate::cancellation::interrupted().map_err(Error::command)?;
    let mut said = BTreeMap::new();
    loop {
        let view = store.read(&DeploymentQuery { id: id.clone() })?;
        if !view.deployment.status.in_flight() {
            return Err(Error::coded(
                RpcErrorCode::Conflict,
                "The redeploy ended before this device approved its images. Nothing deployed.",
            ));
        }
        for build in &view.builds {
            if let Some(passed) = &build.github_skipped {
                return Err(Error::detailed(
                    RpcErrorCode::Conflict,
                    format!(
                        "GitHub didn't build {}: {passed}. Nothing deployed.",
                        build.service
                    ),
                    json!({ "service": build.service }),
                ));
            }
            if said.insert(build.service.clone(), build.status) != Some(build.status)
                && build.status == BuildStatus::Building
            {
                crate::ui::stream(format_args!("Building {} on GitHub", building(build)));
            }
        }
        if view.builds.iter().all(|build| ended(build.status)) {
            return Ok(view);
        }
        let cancelled = super::super::runtime()?.block_on(async {
            tokio::select! {
                () = interrupted.cancelled() => true,
                () = tokio::time::sleep(BUILD_LOOK) => false,
            }
        });
        if cancelled {
            return Err(Error::cancelled());
        }
    }
}

fn building(build: &BuildView) -> String {
    match &build.commit {
        Some(commit) => format!("{} at {}", build.service, short_str(&commit.to_string())),
        None => build.service.to_string(),
    }
}

/// The Store's commit as the one a Device signs.
fn git_commit(commit: &ployz_store::CommitSha) -> Result<CommitSha, Error> {
    CommitSha::parse(&commit.to_string()).map_err(Error::command)
}

const fn ended(status: BuildStatus) -> bool {
    matches!(
        status,
        BuildStatus::Built | BuildStatus::Reused | BuildStatus::Failed
    )
}

/// Each built Service's image, once its token holds up.
fn built_images(
    view: &DeploymentView,
    facts: &Facts,
    cluster: SealingPublicKey,
) -> Result<BTreeMap<ServiceName, ImageDigest>, Error> {
    let mut images = BTreeMap::new();
    for (service, commit, token) in git_builds(view)? {
        let build = view
            .builds
            .iter()
            .find(|build| &build.service == service)
            .ok_or_else(|| Error::conflict(format!("{service} has no build")))?;
        if build.status == BuildStatus::Failed {
            return Err(Error::detailed(
                RpcErrorCode::Conflict,
                format!(
                    "{} failed to build{}. Nothing deployed.",
                    building(build),
                    build
                        .message
                        .as_deref()
                        .map(|message| format!(": {message}"))
                        .unwrap_or_default()
                ),
                json!({ "service": service }),
            ));
        }
        let Some(token) = token else {
            return Err(Error::detailed(
                RpcErrorCode::Conflict,
                format!(
                    "GitHub reported no image token for {}. Nothing deployed.",
                    building(build)
                ),
                json!({ "service": service }),
            ));
        };
        match check(token, service, commit, facts, cluster) {
            Ok(Checked::Verified(image)) => {
                images.insert(service.clone(), image);
            }
            // A build that just ended carries a token GitHub's current keys signed.
            Ok(Checked::Rebuild) => {
                return Err(Refused::Stale.error(service, commit));
            }
            Err(refused) => return Err(refused.error(service, commit)),
        }
    }
    Ok(images)
}

/// What a client concludes of one image token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Checked {
    /// GitHub signed it for this image, built from this commit under this rule.
    Verified(ImageDigest),
    /// It can't be checked any more: GitHub rotated its key out, or an older generation
    /// of the rule built it under settings the rule no longer names. Building the commit
    /// again under the person's approval gives one that can.
    Rebuild,
}

/// Why a token doesn't hold up. Anything but [`Checked::Rebuild`] is Cloud's word against
/// GitHub's, so the client refuses before it signs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Refused {
    Signature(TokenRefusal),
    Commit,
    Claim(&'static str),
    Audience,
    Stale,
}

impl Refused {
    fn error(self, service: &ServiceName, commit: CommitSha) -> Error {
        let why = match self {
            Self::Signature(_) => "GitHub didn't sign it".to_owned(),
            Self::Commit => "it names another commit".to_owned(),
            Self::Claim(field) => format!("its {field} isn't what the rule approves"),
            Self::Audience => {
                "it names another image, another cluster or other build settings".to_owned()
            }
            Self::Stale => "GitHub's current keys didn't sign it".to_owned(),
        };
        Error::detailed(
            RpcErrorCode::Conflict,
            format!(
                "Cloud's image token for {service} at {} doesn't check out: {why}. Nothing \
                 deployed.",
                short(commit)
            ),
            json!({ "service": service, "commit": commit }),
        )
    }
}

/// Check `token` for `service` built from `commit` against `facts`, with no regard for its
/// expiry.
fn check(
    token: &ImageToken,
    service: &ServiceName,
    commit: CommitSha,
    facts: &Facts,
    cluster: SealingPublicKey,
) -> Result<Checked, Refused> {
    let kid = github_oidc::key_id(&token.token).map_err(Refused::Signature)?;
    if !facts.keys.contains(&kid) {
        return Ok(Checked::Rebuild);
    }
    let claims =
        github_oidc::signed_claims(&token.token, &facts.keys).map_err(Refused::Signature)?;
    // GitHub signs the commit a push ran at. A rebuild a person approved runs at the
    // branch's head and builds the commit they named, which the audience binds.
    if token.authority == CommitAuthority::Head && claims.sha != commit {
        return Err(Refused::Commit);
    }
    let rule = &facts.rule;
    // An older generation may have named other settings: building again is safe, since
    // the person approves it and the new token must hold up.
    let older = |refused| match token.generation < rule.generation {
        true => Ok(Checked::Rebuild),
        false => Err(refused),
    };
    if let Err(refusal) = rule.verify_claims(&claims) {
        // The rule names each claim it refuses.
        let AuthorityRefusal::Mismatch { field } = refusal else {
            return older(Refused::Claim("claims"));
        };
        return older(Refused::Claim(field));
    }
    let policy = rule
        .service(service)
        .ok_or(Refused::Claim("Service"))
        .and_then(|approved| PolicyDigest::of(&approved.policy).map_err(|_| Refused::Audience))?;
    let image = ImageDigest::parse(&token.image).map_err(|_| Refused::Audience)?;
    let claim = BuildClaim {
        cluster,
        rule: rule.rule,
        generation: token.generation,
        service: service.clone(),
        policy,
        commit,
        authority: token.authority,
        stage: BuildStage::Built {
            image,
            attest: facts.attest,
        },
    };
    match claim.audience() {
        Ok(audience) if audience == claims.aud => Ok(Checked::Verified(image)),
        _ => older(Refused::Audience),
    }
}

fn short(commit: CommitSha) -> String {
    short_str(&commit.to_string())
}

fn short_str(commit: &str) -> String {
    commit.chars().take(7).collect()
}

fn joined<'a>(names: impl Iterator<Item = &'a ServiceName>) -> String {
    names
        .map(ServiceName::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
#[path = "built_tests.rs"]
mod tests;
