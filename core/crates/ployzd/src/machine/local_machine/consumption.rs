//! The record of what this Machine has consumed, kept in `restart-approvals.json`: joins,
//! GitHub tokens, rule marks, the build each rule's Service last ran and its hook, and the
//! specs it admitted signed. Every
//! verb goes through [`ConsumptionLog::transact`] or [`ConsumptionLog::read`], so the
//! lock, the lazy load, the write and the cache reset after a failed write are one piece
//! of code.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use ployz_core::{
    DeviceCode, Namespace, ServiceName,
    authority::{
        Approver, AuthorityRefusal, AutoDeployRule, BuildClaim, BuildStage, ClaimRule,
        PayloadDigest, RuleId, RuleIdentity, Signed, SigningPublicKey, Timestamp, TrustedKeys,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::Error;

const FILE_NAME: &str = "restart-approvals.json";

/// What this Machine has consumed, in `restart-approvals.json`. Admitted Machine IDs stay
/// consumed across daemon restarts and clock rollback.
pub(crate) struct ConsumptionLog {
    path: PathBuf,
    record: Mutex<Option<Consumption>>,
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
    /// The specs this Machine admitted signed, by Namespace and Service, oldest first.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    specs: BTreeMap<String, Vec<AdmittedDigest>>,
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
}

/// Where a rule's record for one Service is kept: one entry per rule and Service, so the
/// record grows with rules, not with pushes.
fn rule_service_key(rule: RuleId, service: &ServiceName) -> String {
    format!("{rule}/{service}")
}

/// How many signed specs this Machine remembers for one Service. A rollback further back
/// than this needs its signature again.
const SPECS_KEPT: usize = 32;

fn spec_key(namespace: &Namespace, service: &ServiceName) -> String {
    format!("{namespace}/{service}")
}

/// Who stood behind a spec this Machine admitted.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(in crate::machine::local_machine) enum SpecSigner {
    /// A call at Full access: this Server's own socket, or a Device's own connection.
    FullAccess,
    Device(SigningPublicKey),
    Passkey(DeviceCode),
}

impl SpecSigner {
    /// Whether this Machine still trusts the key. Full access names no key to remove.
    fn trusted(self, trusted: &TrustedKeys) -> bool {
        match self {
            Self::FullAccess => true,
            Self::Device(key) => trusted.signing_keys().any(|trusted| trusted == key),
            Self::Passkey(code) => trusted
                .passkeys
                .iter()
                .any(|trusted| trusted.passkey.code() == code),
        }
    }
}

impl From<Approver> for SpecSigner {
    fn from(approver: Approver) -> Self {
        match approver {
            Approver::Device(key) => Self::Device(key),
            Approver::Passkey(code) => Self::Passkey(code),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct AdmittedDigest {
    digest: PayloadDigest,
    by: SpecSigner,
}

/// The build this Machine last admitted a creation for under a rule's Service. A later
/// creation of the same build may rely on it in place of an unexpired token; it still
/// needs the commit to be the head.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct AcceptedBuild {
    generation: u64,
    commit: ployz_core::authority::CommitSha,
    image: ployz_core::authority::ImageDigest,
}

impl AcceptedBuild {
    fn of(claim: &BuildClaim) -> Option<Self> {
        match claim.stage {
            BuildStage::Built { image, .. } => Some(Self {
                generation: claim.generation,
                commit: claim.commit,
                image,
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

/// What an edit of the log decided, besides the answer it carries. Nothing is persisted
/// for an `Err` the edit itself returns.
enum Edit<R> {
    /// Persist the edit, then answer.
    Write(R),
    /// Answer without touching the file.
    Keep(R),
}

impl ConsumptionLog {
    pub(crate) fn new(data_dir: &Path) -> Arc<Self> {
        Arc::new(Self {
            path: data_dir.join(FILE_NAME),
            record: Mutex::new(None),
        })
    }

    /// Run `edit` on a copy of the record and persist what it decided, under the log's
    /// lock. A record that cannot be read or written is `map_io`'s error. The copy in
    /// memory is replaced only once the file is, and forgotten on any failure to write, so
    /// the next call reads the file again.
    fn transact<R, E>(
        &self,
        map_io: fn(io::Error) -> E,
        edit: impl FnOnce(&mut Consumption) -> Result<Edit<R>, E>,
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
        edit: impl FnOnce(&mut Consumption) -> Result<Edit<R>, E>,
    ) -> Result<R, E> {
        let mut guard = self.loaded(map_io)?;
        let mut updated = guard.as_ref().expect("loaded above").clone();
        let answer = match edit(&mut updated)? {
            Edit::Keep(answer) => return Ok(answer),
            Edit::Write(answer) => answer,
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
        Ok(answer)
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
            .record
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

    /// Who admitted `digest` signed for `service` here before, among the keys this
    /// Machine still trusts and Full access. Empty when none did.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read.
    pub(in crate::machine::local_machine) fn spec_admitted(
        &self,
        namespace: &Namespace,
        service: &ServiceName,
        digest: PayloadDigest,
        trusted: &TrustedKeys,
    ) -> Result<Vec<SpecSigner>, Error> {
        self.read(Error::ApprovalRecord, |log| {
            log.specs
                .get(&spec_key(namespace, service))
                .into_iter()
                .flatten()
                .filter(|entry| entry.digest == digest && entry.by.trusted(trusted))
                .map(|entry| entry.by)
                .collect()
        })
    }

    /// Record that `by` stood behind `digest` for `service`, as the newest of the
    /// [`SPECS_KEPT`] this Machine remembers for it, each with the `trusted` keys that
    /// signed it. `trusted` is called under the log's lock, so it is never older than the
    /// keys an earlier write was checked against. Written before the Container is made.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read or written.
    pub(in crate::machine::local_machine) fn admit_spec(
        &self,
        namespace: &Namespace,
        service: &ServiceName,
        digest: PayloadDigest,
        by: SpecSigner,
        trusted: impl FnOnce() -> TrustedKeys,
    ) -> Result<(), Error> {
        self.transact(Error::ApprovalRecord, |log| {
            let admitted = log.specs.entry(spec_key(namespace, service)).or_default();
            let entry = AdmittedDigest { digest, by };
            if admitted.last() == Some(&entry) {
                return Ok(Edit::Keep(()));
            }
            admitted.retain(|kept| kept != &entry);
            admitted.push(entry);
            // What a removed key signed counts for nothing, so it neither takes a place nor
            // stays. A version several keys signed is one version, as new as its newest
            // signature a trusted key made.
            let trusted = trusted();
            let mut newest = Vec::new();
            for kept in admitted.iter().rev() {
                if kept.by.trusted(&trusted) && !newest.contains(&kept.digest) {
                    newest.push(kept.digest);
                }
            }
            newest.truncate(SPECS_KEPT);
            *admitted = newest
                .iter()
                .rev()
                .flat_map(|version| admitted.iter().filter(move |kept| kept.digest == *version))
                .filter(|kept| kept.by.trusted(&trusted))
                .copied()
                .collect();
            Ok(Edit::Write(()))
        })
    }

    /// Whether a rule deploy's hook was already made in the slot `key` names.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read.
    pub(in crate::machine::local_machine) fn hook_made(&self, key: &str) -> Result<bool, Error> {
        self.read(Error::ApprovalRecord, |log| {
            log.rule_hooks
                .values()
                .any(|taken| taken.made && taken.slot.as_str() == key)
        })
    }

    /// Record that a rule deploy's hook is about to start in the slot `key` names, so no
    /// resend of that slot makes another once this one is gone. A slot no rule took is
    /// left alone.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read or written.
    pub(in crate::machine::local_machine) fn mark_hook_made(&self, key: &str) -> Result<(), Error> {
        self.transact(Error::ApprovalRecord, |log| {
            let mut marked = false;
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

    /// Whether this Machine already admitted a creation of `claim`'s build under `rule`.
    pub(crate) fn accepted(
        &self,
        rule: &AutoDeployRule,
        claim: &BuildClaim,
    ) -> Result<bool, Error> {
        self.read(Error::ApprovalRecord, |log| {
            let accepted = log
                .accepted
                .get(&rule_service_key(rule.rule, &claim.service));
            AcceptedBuild::of(claim).is_some_and(|build| accepted == Some(&build))
        })
    }

    /// Record `claim`'s build as the one `rule`'s Service last created a Container from
    /// here. Written before the Container is made. Refuses, through `refused`, a `rule` a
    /// newer generation replaced while its admission waited on GitHub.
    pub(crate) fn accept(
        &self,
        rule: &impl ClaimRule,
        claim: &BuildClaim,
        refused: impl FnOnce(AuthorityRefusal) -> Error,
    ) -> Result<(), Error> {
        self.transact(Error::ApprovalRecord, |log| {
            let rule = rule.identity();
            log.check_mark(rule).map_err(refused)?;
            let Some(build) = AcceptedBuild::of(claim) else {
                return Ok(Edit::Keep(()));
            };
            let key = rule_service_key(rule.rule, &claim.service);
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
    /// run that commit's hook again.
    pub(crate) fn consume_rule_hook(
        &self,
        rule: RuleId,
        claim: &BuildClaim,
        slot: &ployz_core::CreationKey,
        refused: impl FnOnce(AuthorityRefusal) -> Error,
    ) -> Result<(), Error> {
        self.transact(Error::ApprovalRecord, |log| {
            let key = rule_service_key(rule, &claim.service);
            if let Some(taken) = log.rule_hooks.get(&key)
                && taken.generation == claim.generation
                && taken.commit == claim.commit
            {
                if &taken.slot == slot {
                    return Ok(Edit::Keep(()));
                }
                return Err(refused(AuthorityRefusal::Mismatch {
                    field: "a hook run",
                }));
            }
            log.rule_hooks.insert(
                key,
                RuleHook {
                    generation: claim.generation,
                    commit: claim.commit,
                    slot: slot.clone(),
                    made: false,
                },
            );
            Ok(Edit::Write(()))
        })
    }
}

#[cfg(test)]
mod tests;
