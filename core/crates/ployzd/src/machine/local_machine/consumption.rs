//! The record of what this Machine has consumed, kept in `restart-approvals.json`: joins,
//! GitHub tokens, rule marks, the hook each rule's Service last ran, and each Service's
//! floor.
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
    QualifiedService, SealingPublicKey, ServiceName,
    authority::{
        AuthorityRefusal, AutoDeployRule, BuildClaim, ClaimRule, RuleId, RuleIdentity, Signed,
        Timestamp, version::Forget,
    },
};
use ployz_decide::service_floor::{self, Raise, Refused, Signature};

use crate::sealing_key::Floors;
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
    /// The hook each rule's Service last ran here, one per push.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    rule_hooks: BTreeMap<String, RuleHook>,
    /// Each Service's floor, by [`floor_key`]: the newest forget that reached this
    /// Machine. It only rises.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    floors: BTreeMap<String, Timestamp>,
}

/// Where a Service's floor is kept: its Cluster's Sealing public key and the Service, so a
/// Machine that moves to another Cluster keeps none of the old one's.
pub(crate) fn floor_key(cluster: SealingPublicKey, service: &QualifiedService) -> String {
    format!("{cluster}/{service}")
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

    /// The floor of `service` in `cluster`, and this Machine's floored clock at `now`.
    ///
    /// # Errors
    /// Returns `unread`'s error when the record cannot be read.
    pub(crate) fn floor<E>(
        &self,
        cluster: SealingPublicKey,
        service: &QualifiedService,
        now: Timestamp,
        unread: fn(io::Error) -> E,
    ) -> Result<(Option<Timestamp>, Timestamp), E> {
        self.read(unread, |log| {
            let floor = log.floors.get(&floor_key(cluster, service)).copied();
            (floor, log.floored(now))
        })
    }

    /// Admit `signatures` for a new Container of `service` in `cluster` at `now`, as
    /// [`service_floor::admit`] decides, and keep the floor a forget among them raised. The
    /// floor is read and raised in one transaction, so no forget lands between them.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read or written.
    pub(crate) fn admit_versions<K: Copy + PartialEq>(
        &self,
        cluster: SealingPublicKey,
        service: &QualifiedService,
        signatures: &[Signature<K>],
        now: Timestamp,
    ) -> Result<Result<Vec<(K, Timestamp)>, Refused>, Error> {
        self.transact(Error::ApprovalRecord, |log| {
            let key = floor_key(cluster, service);
            let floor = log.floors.get(&key).copied();
            Ok(
                match service_floor::admit(signatures, floor, log.floored(now)) {
                    Ok(admitted) => match admitted.floor {
                        Some(raised) if admitted.floor != floor => {
                            log.floors.insert(key, raised);
                            Edit::Write(Ok(admitted.keys))
                        }
                        _ => Edit::Keep(Ok(admitted.keys)),
                    },
                    Err(refused) => Edit::Keep(Err(refused)),
                },
            )
        })
    }

    /// Every floor this Machine keeps in `cluster`, to seal beside its Sealing Key.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read.
    pub(crate) fn floors_in(&self, cluster: SealingPublicKey) -> Result<Floors, Error> {
        let prefix = format!("{cluster}/");
        self.read(Error::ApprovalRecord, |log| {
            log.floors
                .iter()
                .filter_map(|(key, floor)| {
                    let service = key.strip_prefix(&prefix)?.parse().ok()?;
                    Some((service, *floor))
                })
                .collect()
        })
    }

    /// Take `floors`, sealed beside `cluster`'s Sealing Key, keeping the higher of each and
    /// this Machine's own. They are written before the key, so a Machine that holds the
    /// key holds them.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read or written.
    pub(crate) fn install_floors(
        &self,
        cluster: SealingPublicKey,
        floors: &Floors,
    ) -> Result<(), Error> {
        self.transact(Error::ApprovalRecord, |log| {
            let mut raised = false;
            for (service, &floor) in floors {
                let key = floor_key(cluster, service);
                if log.floors.get(&key).is_none_or(|kept| *kept < floor) {
                    log.floors.insert(key, floor);
                    raised = true;
                }
            }
            Ok(if raised {
                Edit::Write(())
            } else {
                Edit::Keep(())
            })
        })
    }

    /// Whether this Machine keeps a floor for any Service of any Cluster.
    ///
    /// # Errors
    /// Returns `unread`'s error when the record cannot be read.
    pub(crate) fn keeps_floors<E>(&self, unread: fn(io::Error) -> E) -> Result<bool, E> {
        self.read(unread, |log| !log.floors.is_empty())
    }

    /// Raise the floor of each forget's Service in `cluster` to its time, at `now`, and
    /// answer each one. A forget signed past the guard leaves its floor.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read or written.
    pub(crate) fn raise_floors(
        &self,
        cluster: SealingPublicKey,
        forgets: &[Forget],
        now: Timestamp,
    ) -> Result<Vec<Result<Raise, Refused>>, Error> {
        self.transact(Error::ApprovalRecord, |log| {
            let now = log.floored(now);
            let mut raised = false;
            let answers = forgets
                .iter()
                .map(|forget| {
                    let key = floor_key(cluster, &forget.service);
                    let answer =
                        service_floor::raise(log.floors.get(&key).copied(), forget.signed_at, now);
                    if let Ok(Raise::Raised(floor)) = answer {
                        log.floors.insert(key, floor);
                        raised = true;
                    }
                    answer
                })
                .collect();
            Ok(if raised {
                Edit::Write(answers)
            } else {
                Edit::Keep(answers)
            })
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

    /// Record `result` as the one answer to its join, if its approval and the bundle it
    /// rides in are both still open on the floored clock. The time is kept even when the
    /// window has closed, so a clock turned back cannot reopen it.
    pub(crate) fn consume_join(&self, result: JoinResult, now: Timestamp) -> Result<(), Error> {
        self.transact(Error::JoinRecord, |log| {
            let now = log.floored(now);
            let approval = result.approval.open().map_err(Error::JoinRefused)?;
            if log.joins.contains_key(&approval.machine.id) {
                return Err(Error::JoinConsumed(approval.machine.id));
            }
            log.expiry_time_floor = Some(now);
            if let Err(refusal) = ployz_core::authority::join::windows(&result.approval, now) {
                return Ok(Edit::Write(Err(Error::JoinRefused(refusal))));
            }
            log.joins.insert(approval.machine.id, result);
            Ok(Edit::Write(Ok(())))
        })?
    }

    /// This Machine's clock for a one-shot window: `now`, raised to the highest time this
    /// log has taken, which it keeps.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read or written.
    pub(crate) fn observed_now(&self, now: Timestamp) -> Result<Timestamp, Error> {
        self.transact(Error::ApprovalRecord, |log| {
            let floored = log.floored(now);
            if log.expiry_time_floor == Some(floored) {
                return Ok(Edit::Keep(floored));
            }
            log.expiry_time_floor = Some(floored);
            Ok(Edit::Write(floored))
        })
    }

    /// Spend the GitHub token `jti`, valid until `expires_at`, on a grant under `rule` for
    /// the Service whose floor [`floor_key`] names `floor`. Refuses a token already spent,
    /// a generation lower than the highest this Machine has seen of the rule, a generation
    /// that moves the rule to another repository, branch or Namespace, and a rule the
    /// floor refuses. Raises the rule's mark. Written before the grant is minted, so a
    /// crash after it costs the builder a new token, never a second grant.
    pub(crate) fn consume_rule_proof(
        &self,
        rule: &AutoDeployRule,
        floor: &str,
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
            // Read after GitHub answered, so a forget acknowledged during that wait
            // refuses the grant.
            if let Err(below) =
                service_floor::admits(rule.issued_at, log.floors.get(floor).copied(), now)
            {
                return Err(refused(super::admission::floor_refusal(below)));
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

    /// The newest generation of `rule` this Machine has seen, ended or not, and `None`
    /// when it has seen none.
    pub(crate) fn rule_generation<E>(
        &self,
        rule: RuleId,
        map_io: fn(io::Error) -> E,
    ) -> Result<Option<u64>, E> {
        self.read(map_io, |log| {
            log.rules.get(&rule).map(|mark| mark.generation)
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
