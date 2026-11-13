//! The record of what this Machine has consumed, kept in `restart-approvals.json`: restart
//! steps, deploy hooks, starting sets, joins, GitHub tokens, rule marks and Volume records.
//! Every verb goes through [`ConsumptionLog::transact`] or [`ConsumptionLog::read`], so the
//! lock, the lazy load, the write and the cache reset after a failed write are one piece
//! of code.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use ployz_core::{
    ContainerId, Namespace, OperationId, SealingPublicKey, ServiceName,
    authority::{
        AuthorityRefusal, AutoDeployRule, BuildClaim, BuildStage, ClaimRule, DeployApproval,
        PreviewKey, RestartApproval, RestartStep, RuleId, RuleIdentity, Signed, StartingSet,
        Timestamp, UpgradeChannel, restart, upgrade,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::super::{Error, volume_approval::VolumeRecords};

const FILE_NAME: &str = "restart-approvals.json";

/// Device approval consumption in the existing `restart-approvals.json` record.
/// Restart operations expire, while admitted Machine IDs remain consumed across daemon
/// restarts and clock rollback.
pub(crate) struct ConsumptionLog {
    path: PathBuf,
    operations: Mutex<Option<Consumption>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Consumption {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    joins: BTreeMap<ployz_core::MachineId, JoinResult>,
    #[serde(default)]
    expiry_time_floor: Option<Timestamp>,
    /// What this Machine saw first and highest of each automatic deploy rule.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    rules: BTreeMap<RuleId, RuleMark>,
    /// GitHub tokens that minted a Build Grant, by SHA-256 of their `jti`, until they
    /// expire.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    proofs: BTreeMap<String, Timestamp>,
    /// The build each rule's Service last created a Container from here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    accepted: BTreeMap<String, AcceptedBuild>,
    /// The hook each rule's Service last ran here, one per push.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    rule_hooks: BTreeMap<String, RuleHook>,
    /// The newest Release Channel approval this Machine has accepted, per Cluster Sealing
    /// key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    upgrade_channels: BTreeMap<SealingPublicKey, upgrade::Accepted>,
    /// Volume operations, grants and withdrawals, none ever pruned.
    #[serde(default, skip_serializing_if = "VolumeRecords::is_empty")]
    volumes: VolumeRecords,
    #[serde(flatten)]
    operations: BTreeMap<OperationId, Operation>,
}

impl Consumption {
    /// Refuse `rule` when it is below this Machine's mark for it, or moves it to another
    /// repository, branch or Namespace.
    fn check_mark(&self, rule: RuleIdentity<'_>) -> Result<(), AuthorityRefusal> {
        let refused = |field| Err(AuthorityRefusal::Mismatch { field });
        let Some(mark) = self.rules.get(&rule.rule) else {
            return Ok(());
        };
        if rule.generation < mark.generation {
            return refused("a generation");
        }
        if rule.repository_id != mark.repository_id {
            return refused("a repository");
        }
        if rule.branch != mark.branch {
            return refused("a branch");
        }
        if rule.namespace != &mark.namespace {
            return refused("a Namespace");
        }
        Ok(())
    }

    fn raise_mark(&mut self, rule: RuleIdentity<'_>) {
        let mark = self.rules.entry(rule.rule).or_insert_with(|| RuleMark {
            generation: rule.generation,
            repository_id: rule.repository_id,
            branch: rule.branch.to_owned(),
            namespace: rule.namespace.clone(),
        });
        mark.generation = mark.generation.max(rule.generation);
    }

    /// `now`, raised to the highest time this log has taken, so a clock turned back
    /// cannot revive what already expired.
    fn floored(&self, now: Timestamp) -> Timestamp {
        self.expiry_time_floor
            .map_or(now, |highest| now.max(highest))
    }

    /// Record `step` of `approval` on `container`, refusing a step the operation already
    /// reached or passed there. A replay still writes the clock floor and the pruning that
    /// came with it.
    fn take_step(
        &mut self,
        approval: &RestartApproval,
        container: &ContainerId,
        step: RestartStep,
        now: Timestamp,
    ) -> Result<Edit<(), Error>, Error> {
        let now = self.floored(now);
        restart::verify_time(approval, now)?;
        self.expiry_time_floor = Some(now);
        self.operations
            .retain(|_, operation| operation.expires_at.plus(restart::CLOCK_SKEW_MILLIS) >= now);
        let operation = self
            .operations
            .entry(consumption_key(approval))
            .or_insert_with(|| Operation::new(approval.expires_at));
        operation.expires_at = operation.expires_at.max(approval.expires_at);
        let replayed = operation
            .containers
            .get(container)
            .is_some_and(|reached| *reached >= step);
        if replayed {
            return Ok(Edit::WriteThenFail(
                AuthorityRefusal::Replayed {
                    operation: approval.operation,
                }
                .into(),
            ));
        }
        operation.containers.insert(*container, step);
        Ok(Edit::Write(()))
    }
}

/// Where a rule's record for one Service is kept: one entry per rule and Service, and for
/// a preview rule per preview Namespace too, so the record grows with rules and previews,
/// not with pushes.
fn rule_service_key(rule: RuleId, preview: Option<&Namespace>, service: &ServiceName) -> String {
    match preview {
        None => format!("{rule}/{service}"),
        Some(namespace) => format!("{rule}/{namespace}/{service}"),
    }
}

/// The build this Machine last admitted a creation for under a rule's Service. A later
/// creation of the same build may rely on it in place of an unexpired token; it still
/// needs the commit to be the head.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct AcceptedBuild {
    generation: u64,
    commit: ployz_core::authority::CommitSha,
    image: ployz_core::authority::ImageDigest,
    /// The preview the build's token named, for a preview rule's build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preview: Option<PreviewKey>,
    /// The review it was built under, for a reviewed preview's build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    review: Option<ployz_core::authority::ReviewDigest>,
    /// The commit approval it was built under, for a build of a commit a person approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    approved: Option<ployz_core::authority::ApprovalDigest>,
}

impl AcceptedBuild {
    fn of(claim: &BuildClaim, preview: Option<&PreviewKey>) -> Option<Self> {
        use ployz_core::authority::CommitAuthority;
        let (review, approved) = match claim.authority {
            CommitAuthority::Review(digest) => (Some(digest), None),
            CommitAuthority::Approved(digest) => (None, Some(digest)),
            CommitAuthority::Head => (None, None),
        };
        match claim.stage {
            BuildStage::Built { image, .. } => Some(Self {
                generation: claim.generation,
                commit: claim.commit,
                image,
                preview: preview.cloned(),
                review,
                approved,
            }),
            BuildStage::Grant { .. } => None,
        }
    }
}

/// The hook one push ran for a rule's Service: its build and the slot it was made in.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct RuleHook {
    generation: u64,
    commit: ployz_core::authority::CommitSha,
    slot: ployz_core::CreationKey,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    made: bool,
    /// The person's approval it ran under, for a redeploy of the commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    approval: Option<HookApproval>,
}

/// The commit approval a hook ran under, and when the person signed it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct HookApproval {
    digest: ployz_core::authority::ApprovalDigest,
    issued_at: Timestamp,
}

impl HookApproval {
    fn of(approval: &ployz_core::authority::CheckedCommitApproval) -> Self {
        Self {
            digest: approval.digest(),
            issued_at: approval.body().issued_at,
        }
    }
}

/// A rule's identity as this Machine first saw it, which no later generation may change,
/// and the highest generation it has seen.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct RuleMark {
    generation: u64,
    repository_id: u64,
    branch: String,
    namespace: ployz_core::Namespace,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct JoinResult {
    pub approval: Signed<ployz_core::authority::JoinApproval>,
    pub reply: JoinReply,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", content = "result", rename_all = "snake_case")]
pub(crate) enum JoinReply {
    Register(Box<ployz_core::Registered>),
    Share(ployz_core::SealedSealingKey),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Operation {
    expires_at: Timestamp,
    containers: BTreeMap<ContainerId, RestartStep>,
    /// The slot a deploy's hook was created in, for a hook entry: a request resent for
    /// that slot adopts the hook that ran instead of running another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hook: Option<ployz_core::CreationKey>,
    /// The hook's Container was made in that slot, just before it first started. A
    /// resend then adopts it while it exists and makes no other once it's gone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    hook_made: bool,
}

impl Operation {
    fn new(expires_at: Timestamp) -> Self {
        Self {
            expires_at,
            containers: BTreeMap::new(),
            hook: None,
            hook_made: false,
        }
    }
}

/// The entry a hash of `parts` names: 32 hex digits, which no real operation ID collides
/// with in practice.
fn hashed_key(parts: &[&[u8]]) -> OperationId {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    let digest = hasher.finalize();
    let (head, _) = digest.split_at(16);
    OperationId::parse(hex::encode(head)).expect("the hash is lowercase hex")
}

/// What an edit of the log decided, besides the answer it carries. Nothing is persisted
/// for an `Err` the edit itself returns.
enum Edit<R, E> {
    /// Persist the edit, then answer.
    Write(R),
    /// Answer without touching the file.
    Keep(R),
    /// Persist the edit, then refuse: the refusal itself is what must not be forgotten.
    WriteThenFail(E),
}

/// Where `approval`'s consumption is kept. A proposal step gets an entry of its own, so a plan
/// that restarts one Service at step 0 and again at step 2 takes each once, while a resent
/// envelope for the same step still replays. The entry's name is 32 hex digits of the hash of
/// the operation and the step's index, which no real operation ID collides with in practice.
fn consumption_key(approval: &RestartApproval) -> OperationId {
    match &approval.proposal {
        None => approval.operation,
        Some(step) => hashed_key(&[
            step.operation.as_str().as_bytes(),
            &step.index.to_be_bytes(),
        ]),
    }
}

impl ConsumptionLog {
    pub(crate) fn new(data_dir: &Path) -> Arc<Self> {
        Arc::new(Self {
            path: data_dir.join(FILE_NAME),
            operations: Mutex::new(None),
        })
    }

    /// Run `edit` on a copy of the record and persist what it decided, under the log's
    /// lock. A record that cannot be read or written is `map_io`'s error. The copy in
    /// memory is replaced only once the file is, and forgotten on any failure to write, so
    /// the next call reads the file again.
    fn transact<R, E>(
        &self,
        map_io: fn(io::Error) -> E,
        edit: impl FnOnce(&mut Consumption) -> Result<Edit<R, E>, E>,
    ) -> Result<R, E> {
        self.transact_with(
            map_io,
            |path, contents| crate::filesystem::atomic_write(path, contents, 0o600),
            edit,
        )
    }

    fn transact_with<R, E>(
        &self,
        map_io: fn(io::Error) -> E,
        write: impl FnOnce(&Path, &[u8]) -> io::Result<()>,
        edit: impl FnOnce(&mut Consumption) -> Result<Edit<R, E>, E>,
    ) -> Result<R, E> {
        let mut guard = self.loaded(map_io)?;
        let mut updated = guard.as_ref().expect("loaded above").clone();
        let outcome = match edit(&mut updated)? {
            Edit::Keep(answer) => return Ok(answer),
            Edit::Write(answer) => Ok(answer),
            Edit::WriteThenFail(refusal) => Err(refusal),
        };
        let written = serde_json::to_vec(&updated)
            .map_err(io::Error::other)
            .and_then(|contents| write(&self.path, &contents))
            .map_err(map_io);
        if let Err(error) = written {
            *guard = None;
            return Err(error);
        }
        *guard = Some(updated);
        outcome
    }

    /// Read the record as it is, loading it first when this is the first use.
    fn read<R, E>(
        &self,
        map_io: fn(io::Error) -> E,
        read: impl FnOnce(&Consumption) -> R,
    ) -> Result<R, E> {
        let guard = self.loaded(map_io)?;
        Ok(read(guard.as_ref().expect("loaded above")))
    }

    fn loaded<E>(
        &self,
        map_io: fn(io::Error) -> E,
    ) -> Result<std::sync::MutexGuard<'_, Option<Consumption>>, E> {
        let mut guard = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(self.load().map_err(map_io)?);
        }
        Ok(guard)
    }

    fn load(&self) -> io::Result<Consumption> {
        match std::fs::read(&self.path) {
            Ok(contents) => serde_json::from_slice(&contents).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Consumption::default()),
            Err(error) => Err(error),
        }
    }

    /// Record `step` of `approval` on `container`, refusing a step the operation already
    /// reached or passed there. The record is written before the caller acts, so a crash
    /// after it costs a new approval, never a second effect.
    pub(super) fn consume(
        &self,
        approval: &RestartApproval,
        container: &ContainerId,
        step: RestartStep,
        now: Timestamp,
    ) -> Result<(), Error> {
        self.transact(Error::ApprovalRecord, |log| {
            log.take_step(approval, container, step, now)
        })
    }

    /// Take `service`'s hook place under deploy approval `approval` for the slot named
    /// `slot`, refusing one taken for another slot. A request resent for the same slot,
    /// after a lost reply, passes: the Machine adopts the hook that ran, never runs it
    /// again. The entry's name is 32 hex digits of the hash of the operation and the
    /// Service, so it shares the operations map and its expiry.
    pub(in crate::machine::local_machine) fn consume_hook(
        &self,
        approval: &DeployApproval,
        (service, slot): (&ServiceName, &ployz_core::CreationKey),
        now: Timestamp,
    ) -> Result<(), Error> {
        let key = hashed_key(&[
            b"pre-deploy hook\n",
            approval.operation.as_str().as_bytes(),
            b"\n",
            service.as_str().as_bytes(),
        ]);
        self.transact(Error::ApprovalRecord, |log| {
            let now = log.floored(now);
            ployz_core::authority::validity::verify(approval.issued_at, approval.expires_at, now)
                .map_err(Error::DeployRefused)?;
            if let Some(taken) = log.operations.get(&key) {
                if taken.hook.as_ref() == Some(slot) {
                    return Ok(Edit::Keep(()));
                }
                return Err(Error::DeployRefused(AuthorityRefusal::Replayed {
                    operation: approval.operation,
                }));
            }
            log.expiry_time_floor = Some(now);
            log.operations.insert(
                key,
                Operation {
                    hook: Some(slot.clone()),
                    ..Operation::new(approval.expires_at)
                },
            );
            Ok(Edit::Write(()))
        })
    }

    /// Whether a deploy's hook was already made in the slot `key` names.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read.
    pub(in crate::machine::local_machine) fn hook_made(&self, key: &str) -> Result<bool, Error> {
        self.read(Error::ApprovalRecord, |log| {
            log.rule_hooks
                .values()
                .any(|taken| taken.made && taken.slot.as_str() == key)
                || log.operations.values().any(|taken| {
                    taken.hook_made && taken.hook.as_ref().is_some_and(|hook| hook.as_str() == key)
                })
        })
    }

    /// Record that a deploy's hook is about to start in the slot `key` names, so no
    /// resend of that slot makes another once this one is gone. A slot no deploy approval
    /// took is left alone.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read or written.
    pub(in crate::machine::local_machine) fn mark_hook_made(&self, key: &str) -> Result<(), Error> {
        self.transact(Error::ApprovalRecord, |log| {
            let mut marked = false;
            for taken in log.operations.values_mut() {
                if !taken.hook_made && taken.hook.as_ref().is_some_and(|hook| hook.as_str() == key)
                {
                    taken.hook_made = true;
                    marked = true;
                }
            }
            for taken in log.rule_hooks.values_mut() {
                if !taken.made && taken.slot.as_str() == key {
                    taken.made = true;
                    marked = true;
                }
            }
            Ok(if marked {
                Edit::Write(())
            } else {
                Edit::Keep(())
            })
        })
    }

    pub(crate) fn consume_starting_set(
        &self,
        set: &StartingSet,
        now: Timestamp,
    ) -> Result<(), Error> {
        self.transact(Error::ApprovalRecord, |log| {
            let now = log.floored(now);
            ployz_core::authority::validity::verify(set.issued_at, set.expires_at, now)
                .map_err(Error::EnforcementRefused)?;
            if log.operations.contains_key(&set.operation) {
                return Err(Error::EnforcementRefused(AuthorityRefusal::Replayed {
                    operation: set.operation,
                }));
            }
            log.expiry_time_floor = Some(now);
            log.operations
                .insert(set.operation, Operation::new(set.expires_at));
            Ok(Edit::Write(()))
        })
    }

    pub(crate) fn join_result(
        &self,
        machine: ployz_core::MachineId,
    ) -> Result<Option<JoinResult>, Error> {
        self.read(Error::JoinRecord, |log| log.joins.get(&machine).cloned())
    }

    pub(crate) fn require_unused_join(&self, machine: ployz_core::MachineId) -> Result<(), Error> {
        match self.read(Error::JoinRecord, |log| log.joins.contains_key(&machine))? {
            true => Err(Error::JoinConsumed(machine)),
            false => Ok(()),
        }
    }

    pub(crate) fn consume_join(&self, result: JoinResult, now: Timestamp) -> Result<(), Error> {
        self.transact(Error::JoinRecord, |log| {
            let now = log.floored(now);
            let approval = result.approval.open().map_err(Error::JoinRefused)?;
            ployz_core::authority::validity::verify(approval.issued_at, approval.expires_at, now)
                .map_err(Error::JoinRefused)?;
            if log.joins.contains_key(&approval.machine.id) {
                return Err(Error::JoinConsumed(approval.machine.id));
            }
            log.expiry_time_floor = Some(now);
            log.joins.insert(approval.machine.id, result);
            Ok(Edit::Write(()))
        })
    }

    /// Spend the GitHub token `jti`, valid until `expires_at`, on a grant under `rule`.
    /// Refuses a token already spent, a generation lower than the highest this Machine has
    /// seen of the rule, and a generation that moves the rule to another repository,
    /// branch or Namespace. Raises the rule's mark. Written before the grant is minted, so
    /// a crash after it costs the builder a new token, never a second grant.
    pub(crate) fn consume_rule_proof(
        &self,
        rule: &impl ClaimRule,
        refused: impl FnOnce(AuthorityRefusal) -> Error,
        jti: &str,
        expires_at: Timestamp,
        now: Timestamp,
    ) -> Result<(), Error> {
        self.transact(Error::ApprovalRecord, |log| {
            let now = log.floored(now);
            if let Err(refusal) = log.check_mark(rule.identity()) {
                return Err(refused(refusal));
            }
            let spent = hex::encode(Sha256::digest(jti.as_bytes()));
            if log.proofs.contains_key(&spent) {
                return Err(refused(AuthorityRefusal::ProofReplayed));
            }
            log.expiry_time_floor = Some(now);
            log.proofs.retain(|_, expires| {
                expires.plus(ployz_core::authority::validity::CLOCK_SKEW_MILLIS) >= now
            });
            log.proofs.insert(spent, expires_at);
            log.raise_mark(rule.identity());
            Ok(Edit::Write(()))
        })
    }

    /// Record that a validly signed `rule` exists, ended or not, before anything else
    /// decides on it: no generation below it is admitted again. Refuses, through
    /// `refused`, a generation lower than the mark, or one that moves the rule to another
    /// repository, branch or Namespace. Writes only when the mark rises.
    pub(crate) fn mark_rule(
        &self,
        rule: &impl ClaimRule,
        refused: impl FnOnce(AuthorityRefusal) -> Error,
    ) -> Result<(), Error> {
        let rule = rule.identity();
        self.transact(Error::ApprovalRecord, |log| {
            log.check_mark(rule).map_err(refused)?;
            if log
                .rules
                .get(&rule.rule)
                .is_some_and(|mark| mark.generation >= rule.generation)
            {
                return Ok(Edit::Keep(()));
            }
            log.raise_mark(rule);
            Ok(Edit::Write(()))
        })
    }

    /// Record that a verified Release Channel approval of `channel.generation` exists for
    /// its Cluster, synced before the upgrade it admits writes its receipt. Writes only
    /// when the mark rises, so a resend after a crash between this write and the receipt
    /// is admitted again.
    ///
    /// # Errors
    /// Returns [`Error::UpgradeChannelRefused`] for a generation below the mark, or equal to
    /// it for another channel, and
    /// [`Error::ApprovalRecord`] when the record cannot be read or written.
    pub(crate) fn mark_upgrade_channel(&self, channel: &UpgradeChannel) -> Result<(), Error> {
        self.transact(Error::ApprovalRecord, |log| {
            let newest = log.upgrade_channels.get(&channel.cluster).copied();
            upgrade::admit_generation(channel, newest).map_err(Error::UpgradeChannelRefused)?;
            let accepted = upgrade::Accepted {
                generation: channel.generation,
                channel: channel.channel,
            };
            if newest == Some(accepted) {
                return Ok(Edit::Keep(()));
            }
            log.upgrade_channels.insert(channel.cluster, accepted);
            Ok(Edit::Write(()))
        })
    }

    /// Whether this Machine already admitted a creation of `claim`'s build under `rule`.
    pub(crate) fn accepted(
        &self,
        rule: &AutoDeployRule,
        claim: &BuildClaim,
    ) -> Result<bool, Error> {
        self.read(Error::ApprovalRecord, |log| {
            let accepted = log
                .accepted
                .get(&rule_service_key(rule.rule, None, &claim.service));
            AcceptedBuild::of(claim, None).is_some_and(|build| accepted == Some(&build))
        })
    }

    /// The preview this Machine already admitted a creation of `claim`'s build for under
    /// `rule` in `namespace`, which only that preview's Namespace derives from.
    pub(crate) fn accepted_preview(
        &self,
        rule: RuleId,
        namespace: &Namespace,
        claim: &BuildClaim,
    ) -> Result<Option<PreviewKey>, Error> {
        self.read(Error::ApprovalRecord, |log| {
            let accepted =
                log.accepted
                    .get(&rule_service_key(rule, Some(namespace), &claim.service));
            accepted.and_then(|build| {
                let preview = build.preview.as_ref()?;
                (AcceptedBuild::of(claim, Some(preview)).as_ref() == Some(build))
                    .then(|| preview.clone())
            })
        })
    }

    /// Record `claim`'s build as the one `rule`'s Service last created a Container from
    /// here, in the preview `preview` names for a preview rule. Written before the
    /// Container is made. Refuses, through `refused`, a `rule` a newer generation replaced
    /// while its admission waited on GitHub.
    pub(crate) fn accept(
        &self,
        rule: &impl ClaimRule,
        preview: Option<(&Namespace, &PreviewKey)>,
        claim: &BuildClaim,
        refused: impl FnOnce(AuthorityRefusal) -> Error,
    ) -> Result<(), Error> {
        self.transact(Error::ApprovalRecord, |log| {
            let rule = rule.identity();
            log.check_mark(rule).map_err(refused)?;
            let Some(build) = AcceptedBuild::of(claim, preview.map(|(_, key)| key)) else {
                return Ok(Edit::Keep(()));
            };
            let key = rule_service_key(
                rule.rule,
                preview.map(|(namespace, _)| namespace),
                &claim.service,
            );
            if log.accepted.get(&key) == Some(&build) {
                return Ok(Edit::Keep(()));
            }
            log.accepted.insert(key, build);
            Ok(Edit::Write(()))
        })
    }

    /// Take the hook place of `claim`'s push under `rule` for the slot named `slot`,
    /// refusing one taken for another slot. A request resent for the same slot passes:
    /// the Machine adopts the hook that ran. A later push of the Service replaces the
    /// entry, so a push runs its hook once; a force push back to an earlier commit can
    /// run that commit's hook again. A person's `approval` of the commit runs it once
    /// more, and only one signed after the approval the hook last ran under does. A
    /// preview rule keeps one entry per preview, named by its Namespace in `preview`.
    pub(crate) fn consume_rule_hook(
        &self,
        (rule, preview): (RuleId, Option<&Namespace>),
        claim: &BuildClaim,
        approval: Option<&ployz_core::authority::CheckedCommitApproval>,
        slot: &ployz_core::CreationKey,
        refused: impl FnOnce(AuthorityRefusal) -> Error,
    ) -> Result<(), Error> {
        self.transact(Error::ApprovalRecord, |log| {
            let key = rule_service_key(rule, preview, &claim.service);
            let approval = approval.map(HookApproval::of);
            if let Some(taken) = log.rule_hooks.get(&key)
                && taken.generation == claim.generation
                && taken.commit == claim.commit
            {
                if &taken.slot == slot {
                    return Ok(Edit::Keep(()));
                }
                let newer = match (approval, taken.approval) {
                    (Some(approval), Some(ran)) => approval.issued_at > ran.issued_at,
                    (Some(_), None) => true,
                    (None, _) => false,
                };
                if !newer {
                    return Err(refused(AuthorityRefusal::Mismatch {
                        field: "a hook run",
                    }));
                }
            }
            log.rule_hooks.insert(
                key,
                RuleHook {
                    generation: claim.generation,
                    commit: claim.commit,
                    slot: slot.clone(),
                    made: false,
                    approval,
                },
            );
            Ok(Edit::Write(()))
        })
    }

    /// Apply `change` to this Machine's Volume records at `now`, raised to the log's time
    /// floor, and keep the result, synced, before returning its answer. A refusal keeps the
    /// records as they were, and an unchanged record is not written.
    pub(in crate::machine::local_machine) fn change_volumes<R>(
        &self,
        now: Timestamp,
        change: impl FnOnce(&mut VolumeRecords, Timestamp) -> Result<R, Error>,
    ) -> Result<R, Error> {
        self.transact(Error::ApprovalRecord, |log| {
            let now = log.floored(now);
            let before = log.volumes.clone();
            let answer = change(&mut log.volumes, now)?;
            if log.volumes == before {
                return Ok(Edit::Keep(answer));
            }
            log.expiry_time_floor = Some(now);
            Ok(Edit::Write(answer))
        })
    }

    pub(in crate::machine::local_machine) fn read_volumes<R>(
        &self,
        read: impl FnOnce(&VolumeRecords) -> R,
    ) -> Result<R, Error> {
        self.read(Error::ApprovalRecord, |log| read(&log.volumes))
    }
}

#[cfg(test)]
mod golden_tests;

#[cfg(test)]
mod tests {
    use ployz_core::DeviceKey;

    use super::*;

    fn approval(operation: &str, expires_at: u64) -> RestartApproval {
        RestartApproval {
            cluster: ployz_core::SealingPublicKey::from_bytes([1; 32]),
            operation: OperationId::parse(operation).unwrap(),
            kind: ployz_core::authority::WorkflowKind::Restart,
            namespace: ployz_core::Namespace::parse("app").unwrap(),
            service: ployz_core::ServiceName::parse("api").unwrap(),
            machines: vec![],
            container: container('a'),
            issued_at: at(0),
            expires_at: Timestamp::from_millis(expires_at).unwrap(),
            signer: ployz_core::authority::SignerKey::Device(DeviceKey::from_bytes([2; 32])),
            proposal: None,
        }
    }

    fn container(fill: char) -> ContainerId {
        ContainerId::parse(fill.to_string().repeat(64)).unwrap()
    }

    fn at(millis: u64) -> Timestamp {
        Timestamp::from_millis(millis).unwrap()
    }

    fn replayed(result: Result<(), Error>) -> bool {
        matches!(
            result,
            Err(Error::RestartRefused(AuthorityRefusal::Replayed { .. }))
        )
    }

    #[test]
    fn each_step_runs_once_per_container_in_order_and_survives_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let log = ConsumptionLog::new(dir.path());
        let first = approval(&"1".repeat(32), 10_000);

        log.consume(&first, &container('a'), RestartStep::Hold, at(1))
            .unwrap();
        log.consume(&first, &container('a'), RestartStep::Stop, at(2))
            .unwrap();
        assert!(replayed(log.consume(
            &first,
            &container('a'),
            RestartStep::Stop,
            at(3)
        )));
        assert!(replayed(log.consume(
            &first,
            &container('a'),
            RestartStep::Hold,
            at(3)
        )));
        log.consume(&first, &container('b'), RestartStep::Stop, at(3))
            .unwrap();

        let reopened = ConsumptionLog::new(dir.path());
        assert!(replayed(reopened.consume(
            &first,
            &container('a'),
            RestartStep::Stop,
            at(4)
        )));
        reopened
            .consume(&first, &container('a'), RestartStep::Start, at(4))
            .unwrap();
        assert!(replayed(reopened.consume(
            &first,
            &container('a'),
            RestartStep::Start,
            at(5)
        )));

        let second = approval(&"2".repeat(32), 10_000);
        reopened
            .consume(&second, &container('a'), RestartStep::Hold, at(5))
            .unwrap();
    }

    #[test]
    fn a_proposal_that_restarts_one_container_at_two_steps_takes_each_once() {
        let dir = tempfile::tempdir().unwrap();
        let log = ConsumptionLog::new(dir.path());
        let operation = "1".repeat(32);
        let step = |index: u32| RestartApproval {
            proposal: Some(ployz_core::authority::ProposalRef {
                operation: OperationId::parse(&operation).unwrap(),
                index,
            }),
            ..approval(&operation, 10_000)
        };

        log.consume(&step(0), &container('a'), RestartStep::Hold, at(1))
            .unwrap();
        log.consume(&step(0), &container('a'), RestartStep::Start, at(2))
            .unwrap();
        log.consume(&step(1), &container('b'), RestartStep::Stop, at(3))
            .unwrap();
        log.consume(&step(2), &container('a'), RestartStep::Hold, at(4))
            .unwrap();
        log.consume(&step(3), &container('b'), RestartStep::Start, at(5))
            .unwrap();

        assert!(replayed(log.consume(
            &step(0),
            &container('a'),
            RestartStep::Hold,
            at(6)
        )));
        assert!(replayed(log.consume(
            &step(2),
            &container('a'),
            RestartStep::Hold,
            at(6)
        )));
        log.consume(
            &approval(&operation, 10_000),
            &container('a'),
            RestartStep::Hold,
            at(7),
        )
        .unwrap();
        let reopened = ConsumptionLog::new(dir.path());
        assert!(replayed(reopened.consume(
            &step(2),
            &container('a'),
            RestartStep::Hold,
            at(8)
        )));
    }

    #[test]
    fn pruning_then_clock_rollback_cannot_replay_even_after_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let log = ConsumptionLog::new(dir.path());
        let short = approval(&"1".repeat(32), 100);
        log.consume(&short, &container('a'), RestartStep::Start, at(50))
            .unwrap();
        assert!(replayed(log.consume(
            &short,
            &container('a'),
            RestartStep::Start,
            at(100)
        )));
        let later = approval(&"2".repeat(32), 200_000);
        log.consume(&later, &container('a'), RestartStep::Start, at(120_101))
            .unwrap();
        let stored = std::fs::read_to_string(dir.path().join(FILE_NAME)).unwrap();
        assert!(!stored.contains(&"1".repeat(32)), "{stored}");
        let reopened = ConsumptionLog::new(dir.path());
        assert!(matches!(
            reopened.consume(&short, &container('a'), RestartStep::Start, at(50)),
            Err(Error::RestartRefused(AuthorityRefusal::Expired))
        ));
        assert!(matches!(
            reopened.consume(&short, &container('b'), RestartStep::Hold, at(50)),
            Err(Error::RestartRefused(AuthorityRefusal::Expired))
        ));
    }

    #[test]
    fn the_previous_log_shape_still_refuses_consumed_steps() {
        let dir = tempfile::tempdir().unwrap();
        let first = approval(&"1".repeat(32), 100);
        let previous = BTreeMap::from([(
            first.operation,
            Operation {
                expires_at: first.expires_at,
                containers: BTreeMap::from([(container('a'), RestartStep::Stop)]),
                hook: None,
                hook_made: false,
            },
        )]);
        std::fs::write(
            dir.path().join(FILE_NAME),
            serde_json::to_vec(&previous).unwrap(),
        )
        .unwrap();
        let log = ConsumptionLog::new(dir.path());
        assert!(replayed(log.consume(
            &first,
            &container('a'),
            RestartStep::Stop,
            at(50)
        )));
        log.consume(&first, &container('a'), RestartStep::Start, at(51))
            .unwrap();
        let reopened = ConsumptionLog::new(dir.path());
        assert!(replayed(reopened.consume(
            &first,
            &container('a'),
            RestartStep::Start,
            at(52)
        )));
    }

    #[test]
    fn a_reused_operation_keeps_consumption_until_its_latest_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let log = ConsumptionLog::new(dir.path());
        let short = approval(&"1".repeat(32), 100);
        log.consume(&short, &container('a'), RestartStep::Start, at(50))
            .unwrap();
        let extended = RestartApproval {
            expires_at: at(300_000),
            ..short
        };
        assert!(replayed(log.consume(
            &extended,
            &container('a'),
            RestartStep::Start,
            at(60)
        )));
        let later = approval(&"2".repeat(32), 400_000);
        log.consume(&later, &container('b'), RestartStep::Start, at(200_000))
            .unwrap();
        let reopened = ConsumptionLog::new(dir.path());
        assert!(replayed(reopened.consume(
            &extended,
            &container('a'),
            RestartStep::Start,
            at(200_001)
        )));
    }

    #[test]
    fn a_write_that_lands_then_errors_is_reloaded_before_the_next_consumption() {
        let dir = tempfile::tempdir().unwrap();
        let log = ConsumptionLog::new(dir.path());
        let first = approval(&"1".repeat(32), 100);
        assert!(matches!(
            log.transact_with(
                Error::ApprovalRecord,
                |path, contents| {
                    crate::filesystem::atomic_write(path, contents, 0o600)?;
                    Err(io::Error::other("directory sync failed after rename"))
                },
                |log| log.take_step(&first, &container('a'), RestartStep::Stop, at(1)),
            ),
            Err(Error::ApprovalRecord(_))
        ));
        let later = approval(&"2".repeat(32), 100);
        log.consume(&later, &container('a'), RestartStep::Stop, at(2))
            .unwrap();
        assert!(replayed(log.consume(
            &first,
            &container('a'),
            RestartStep::Stop,
            at(3)
        )));
        let reopened = ConsumptionLog::new(dir.path());
        assert!(replayed(reopened.consume(
            &first,
            &container('a'),
            RestartStep::Stop,
            at(3)
        )));
    }

    #[test]
    fn an_unreadable_record_refuses_rather_than_forgetting() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE_NAME), b"{").unwrap();
        let log = ConsumptionLog::new(dir.path());
        assert!(matches!(
            log.consume(
                &approval(&"1".repeat(32), 100),
                &container('a'),
                RestartStep::Start,
                at(1)
            ),
            Err(Error::ApprovalRecord(_))
        ));
    }
}
