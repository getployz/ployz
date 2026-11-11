//! Device-signed restart approvals: the switch that requires them, the gate an Operator
//! caller's restart step passes under it, and the record of steps already taken. A new
//! creation, or a step carrying only a deploy approval, goes to `deploy_approval`.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use ployz_core::{
    ContainerId, ContainerLocator, ContainerNow, ContainerWant, DeviceEnforcementSet, DeviceKey,
    LocalMachinePhase, ManagementScope, Namespace, OperationId, RunTarget, Sealing,
    SealingPublicKey, ServiceName,
    SetDeviceEnforcementRequest,
    authority::{
        AuthorityRefusal, AutoDeployRule, BuildClaim, BuildStage, ClaimRule, DeployApproval,
        PreviewKey, Purpose, RestartApproval, RestartFacts, RestartStep, RuleId, RuleIdentity,
        Signed, StartingSet, Timestamp, UpgradeChannel, restart, starting_set, upgrade,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Error, LocalMachine, volume_approval::VolumeRecords};

const FILE_NAME: &str = "restart-approvals.json";

impl LocalMachine {
    /// Turn Device enforcement on or off under mutation admission, at Full scope only.
    /// Turning it on trusts only the Members of a Device-signed starting set that names
    /// this Machine, used once.
    ///
    /// # Errors
    /// Returns [`Error::NeedsFullScope`] for an Operator caller, [`Error::NotParticipating`]
    /// when this Machine is not participating, [`Error::EnforcementRefused`] for a missing,
    /// untrusted, expired or reused starting set, admission failures,
    /// [`Error::RecordOwner`] when the record owner has stopped, and [`Error::Store`] when
    /// the record cannot be written.
    pub async fn set_device_enforcement(
        &self,
        request: SetDeviceEnforcementRequest,
    ) -> Result<DeviceEnforcementSet, Error> {
        let client = self.management_client;
        let approvals = self.approvals.clone();
        let owner = self.owner.clone();
        self.finish_mutation(async move {
            owner
                .mutate(move |store| {
                    let record = store.record();
                    if record.phase() != LocalMachinePhase::Participating {
                        return Err(Error::NotParticipating);
                    }
                    if client.is_some_and(|remote| {
                        record.management_scope(&remote) != Some(ManagementScope::Full)
                    }) {
                        return Err(Error::NeedsFullScope);
                    }
                    let starting = match (request.enforced, &request.starting_set) {
                        (false, _) => Vec::new(),
                        (true, None) => {
                            return Err(Error::EnforcementRefused(AuthorityRefusal::Unsigned {
                                purpose: Purpose::Enforce,
                            }));
                        }
                        (true, Some(signed)) => {
                            let own = record.machine().ok_or(Error::NotParticipating)?.into();
                            let devices: Vec<DeviceKey> = record.devices().collect();
                            let now = Timestamp::now();
                            let body = starting_set::verify(signed, &devices, &own, now)
                                .map_err(Error::EnforcementRefused)?;
                            approvals.consume_starting_set(&body, now)?;
                            body.members
                        }
                    };
                    store.set_device_enforcement(request.enforced, starting)?;
                    Ok(DeviceEnforcementSet {
                        enforced: request.enforced,
                    })
                })
                .await?
        })
        .await
    }

    /// Under Device enforcement an Operator caller's `SetIngressDoors` or
    /// `SetPrivateAccess` is refused before anything changes: no statement admits either
    /// yet. `capability` is the RPC refused.
    ///
    /// # Errors
    /// Returns [`Error::IngressRefused`] naming `capability` under enforcement.
    pub(crate) fn admit_operator_ingress(&self, capability: &'static str) -> Result<(), Error> {
        if self.record().device_enforcement() {
            return Err(Error::IngressRefused { capability });
        }
        Ok(())
    }

    /// Under Device enforcement an Operator caller's `Leave` or `Forget` is refused before
    /// any Container stops. Either one resets a Server: `Leave` this one, and `Forget`
    /// another, which fences itself once its row goes. Neither is Cloud's to start. A Full
    /// caller and a Machine not enforcing pass untouched.
    ///
    /// # Errors
    /// Returns [`Error::DepartureRefused`] naming `capability` under enforcement.
    pub(crate) fn admit_operator_departure(&self, capability: &'static str) -> Result<(), Error> {
        if self.record().device_enforcement() {
            return Err(Error::DepartureRefused { capability });
        }
        Ok(())
    }

    /// Under Device enforcement an Operator caller's `Build` or `MintBuildGrant` is
    /// refused before anything opens. A Build runs recipe steps of the caller's choosing
    /// with Sealed values opened, and a Build Grant hands them to a key the caller names,
    /// so neither is Cloud's to start. `capability` is the RPC refused. A Full caller and a
    /// Machine not enforcing pass untouched.
    ///
    /// # Errors
    /// Returns [`Error::BuildRefused`] naming `capability` under enforcement.
    pub(crate) fn admit_operator_build(&self, capability: &'static str) -> Result<(), Error> {
        if self.record().device_enforcement() {
            return Err(Error::BuildRefused { capability });
        }
        Ok(())
    }

    /// Under Device enforcement, let an Operator caller's restart step through only with
    /// an approved Device's approval of exactly this restart, or a browser key's under a
    /// trusted passkey's session grant that covers it, and record the step so the
    /// same approval never runs it again. A new creation, adoption included, needs a deploy
    /// approval of its spec, and a step that carries only a deploy approval is checked
    /// against it. A change carrying only an automatic deploy rule's proof is checked
    /// against the rule. Returns a want pinned to the verified Container ID. An absent
    /// target is refused. A Machine not enforcing passes untouched.
    ///
    /// # Errors
    /// Returns [`Error::RestartRefused`], [`Error::DeployRefused`] or
    /// [`Error::RuleDeployRefused`] naming what the approval lacks, [`Error::ApprovalRecord`] when the record of taken steps cannot be
    /// read or written, and [`Error::DockerUnavailable`] or [`Error::Docker`] when the
    /// Container cannot be read.
    pub async fn admit_operator_restart(
        &self,
        want: ContainerWant,
        approval: Option<&Signed<RestartApproval>>,
        deploy: Option<&Signed<DeployApproval>>,
        rule: Option<&ployz_core::authority::RuleProof>,
        preview: Option<&ployz_core::authority::PreviewProof>,
    ) -> Result<ContainerWant, Error> {
        let record = self.record();
        if !record.device_enforcement() {
            return Ok(want);
        }
        match (approval, deploy, rule, preview) {
            (None, None, Some(rule), None) => {
                return self.admit_rule_deploy_online(want, rule).await;
            }
            (None, None, None, Some(preview)) => {
                return self.admit_preview_deploy_online(want, preview).await;
            }
            _ => {}
        }
        let Some((step, locator)) = RestartStep::of(&want) else {
            self.admit_operator_creation(&want, deploy)?;
            return Ok(want);
        };
        if let (None, Some(deploy)) = (approval, deploy) {
            return self.admit_deploy_step(want, deploy).await;
        }
        let approval = approval.ok_or(AuthorityRefusal::Unsigned {
            purpose: Purpose::Workflow,
        })?;
        let ContainerNow::Present(observed) = self.container_now(locator).await? else {
            return Err(AuthorityRefusal::Mismatch {
                field: "a Container",
            }
            .into());
        };
        let trusted = record.trusted_keys();
        let identity = observed.identity();
        let machine = record.id();
        let now = Timestamp::now();
        let cluster = match self.held_sealing(record.phase()) {
            Some(Sealing::Held { public_key }) => Some(public_key),
            _ => None,
        };
        let approved = restart::verify(
            approval,
            &RestartFacts {
                cluster,
                machine: &machine,
                trusted: &trusted,
                namespace: &identity.namespace,
                service: &identity.name,
                container: &observed.container_id,
                kind: observed.kind,
                step,
                now,
            },
        )?;
        self.approvals
            .consume(&approved.body, &observed.container_id, step, now)?;
        tracing::info!(
            operation = %approved.body.operation,
            proposal = ?approved.body.proposal,
            ?step,
            service = %identity,
            container = observed.container_id.as_str().get(..12).unwrap_or_default(),
            "{} approved by {}",
            approved.body.kind,
            approved.by
        );
        Ok(pinned(want, observed.container_id))
    }
}

/// `want` aimed at `container` by its ID, so the step acts on the Container verified.
pub(super) fn pinned(mut want: ContainerWant, container: ContainerId) -> ContainerWant {
    let locator = match &mut want {
        ContainerWant::Held { container, .. }
        | ContainerWant::Out { container, .. }
        | ContainerWant::Stopped { container, .. }
        | ContainerWant::Gone { container, .. }
        | ContainerWant::Running {
            container: RunTarget::Existing(container),
            ..
        } => container,
        ContainerWant::Running {
            container: RunTarget::New(_),
            ..
        } => unreachable!("a new creation has no Container to pin"),
    };
    *locator = ContainerLocator::Id(container);
    want
}

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
    /// The highest generation of the Release Channel approval this Machine has accepted,
    /// per Cluster Sealing key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    upgrade_channels: BTreeMap<SealingPublicKey, u64>,
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
}

impl AcceptedBuild {
    fn of(claim: &BuildClaim, preview: Option<&PreviewKey>) -> Option<Self> {
        let review = match claim.authority {
            ployz_core::authority::CommitAuthority::Review(digest) => Some(digest),
            ployz_core::authority::CommitAuthority::Head => None,
        };
        match claim.stage {
            BuildStage::Built { image, .. } => Some(Self {
                generation: claim.generation,
                commit: claim.commit,
                image,
                preview: preview.cloned(),
                review,
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

/// Where `approval`'s consumption is kept. A proposal step gets an entry of its own, so a plan
/// that restarts one Service at step 0 and again at step 2 takes each once, while a resent
/// envelope for the same step still replays. The entry's name is 32 hex digits of the hash of
/// the operation and the step's index, which no real operation ID collides with in practice.
fn consumption_key(approval: &RestartApproval) -> OperationId {
    let Some(step) = &approval.proposal else {
        return approval.operation;
    };
    let mut hasher = Sha256::new();
    hasher.update(step.operation.as_str().as_bytes());
    hasher.update(step.index.to_be_bytes());
    let digest = hasher.finalize();
    let (head, _) = digest.split_at(16);
    OperationId::parse(hex::encode(head)).expect("the hash is lowercase hex")
}

impl ConsumptionLog {
    pub(crate) fn new(data_dir: &Path) -> Arc<Self> {
        Arc::new(Self {
            path: data_dir.join(FILE_NAME),
            operations: Mutex::new(None),
        })
    }

    /// Record `step` of `approval` on `container`, refusing a step the operation already
    /// reached or passed there. The record is written before the caller acts, so a crash
    /// after it costs a new approval, never a second effect.
    fn consume(
        &self,
        approval: &RestartApproval,
        container: &ContainerId,
        step: RestartStep,
        now: Timestamp,
    ) -> Result<(), Error> {
        self.consume_with(approval, container, step, now, |path, contents| {
            crate::filesystem::atomic_write(path, contents, 0o600)
        })
    }

    fn consume_with(
        &self,
        approval: &RestartApproval,
        container: &ContainerId,
        step: RestartStep,
        now: Timestamp,
        write: impl FnOnce(&Path, &[u8]) -> io::Result<()>,
    ) -> Result<(), Error> {
        let mut guard = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(self.load().map_err(Error::ApprovalRecord)?);
        }
        let operations = guard.as_mut().expect("loaded above");
        let now = operations
            .expiry_time_floor
            .map_or(now, |highest| now.max(highest));
        restart::verify_time(approval, now)?;
        let mut updated = operations.clone();
        updated.expiry_time_floor = Some(now);
        updated
            .operations
            .retain(|_, operation| operation.expires_at.plus(restart::CLOCK_SKEW_MILLIS) >= now);
        let operation = updated
            .operations
            .entry(consumption_key(approval))
            .or_insert_with(|| Operation {
                expires_at: approval.expires_at,
                containers: BTreeMap::new(),
                hook: None,
                hook_made: false,
            });
        operation.expires_at = operation.expires_at.max(approval.expires_at);
        let replayed = operation
            .containers
            .get(container)
            .is_some_and(|reached| *reached >= step);
        if !replayed {
            operation.containers.insert(*container, step);
        }
        let contents = serde_json::to_vec(&updated).map_err(io::Error::other);
        let written = contents
            .and_then(|contents| write(&self.path, &contents))
            .map_err(Error::ApprovalRecord);
        if let Err(error) = written {
            *guard = None;
            return Err(error);
        }
        *operations = updated;
        if replayed {
            return Err(AuthorityRefusal::Replayed {
                operation: approval.operation,
            }
            .into());
        }
        Ok(())
    }

    /// Take `service`'s hook place under deploy approval `approval` for the slot named
    /// `slot`, refusing one taken for another slot. A request resent for the same slot,
    /// after a lost reply, passes: the Machine adopts the hook that ran, never runs it
    /// again. The entry's name is 32 hex digits of the hash of the operation and the
    /// Service, so it shares the operations map and its expiry.
    pub(super) fn consume_hook(
        &self,
        approval: &DeployApproval,
        (service, slot): (&ServiceName, &ployz_core::CreationKey),
        now: Timestamp,
    ) -> Result<(), Error> {
        let mut hasher = Sha256::new();
        hasher.update(b"pre-deploy hook\n");
        hasher.update(approval.operation.as_str().as_bytes());
        hasher.update(b"\n");
        hasher.update(service.as_str().as_bytes());
        let digest = hasher.finalize();
        let (head, _) = digest.split_at(16);
        let key = OperationId::parse(hex::encode(head)).expect("the hash is lowercase hex");
        let mut guard = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(self.load().map_err(Error::ApprovalRecord)?);
        }
        let consumption = guard.as_ref().expect("loaded above");
        let now = consumption
            .expiry_time_floor
            .map_or(now, |floor| now.max(floor));
        ployz_core::authority::validity::verify(approval.issued_at, approval.expires_at, now)
            .map_err(Error::DeployRefused)?;
        if let Some(taken) = consumption.operations.get(&key) {
            if taken.hook.as_ref() == Some(slot) {
                return Ok(());
            }
            return Err(Error::DeployRefused(AuthorityRefusal::Replayed {
                operation: approval.operation,
            }));
        }
        let mut updated = consumption.clone();
        updated.expiry_time_floor = Some(now);
        updated.operations.insert(
            key,
            Operation {
                expires_at: approval.expires_at,
                containers: BTreeMap::new(),
                hook: Some(slot.clone()),
                hook_made: false,
            },
        );
        let written = serde_json::to_vec(&updated)
            .map_err(io::Error::other)
            .and_then(|contents| crate::filesystem::atomic_write(&self.path, &contents, 0o600))
            .map_err(Error::ApprovalRecord);
        if let Err(error) = written {
            *guard = None;
            return Err(error);
        }
        *guard = Some(updated);
        Ok(())
    }

    /// Whether a deploy's hook was already made in the slot `key` names.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read.
    pub(super) fn hook_made(&self, key: &str) -> Result<bool, Error> {
        let mut guard = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(self.load().map_err(Error::ApprovalRecord)?);
        }
        let consumption = guard.as_ref().expect("loaded above");
        let rule_hook = consumption
            .rule_hooks
            .values()
            .any(|taken| taken.made && taken.slot.as_str() == key);
        Ok(rule_hook
            || consumption.operations.values().any(|taken| {
                taken.hook_made && taken.hook.as_ref().is_some_and(|hook| hook.as_str() == key)
            }))
    }

    /// Record that a deploy's hook is about to start in the slot `key` names, so no
    /// resend of that slot makes another once this one is gone. A slot no deploy approval
    /// took is left alone.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read or written.
    pub(super) fn mark_hook_made(&self, key: &str) -> Result<(), Error> {
        let mut guard = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(self.load().map_err(Error::ApprovalRecord)?);
        }
        let consumption = guard.as_ref().expect("loaded above");
        let mut updated = consumption.clone();
        let mut marked = false;
        for taken in updated.operations.values_mut() {
            if !taken.hook_made && taken.hook.as_ref().is_some_and(|hook| hook.as_str() == key) {
                taken.hook_made = true;
                marked = true;
            }
        }
        for taken in updated.rule_hooks.values_mut() {
            if !taken.made && taken.slot.as_str() == key {
                taken.made = true;
                marked = true;
            }
        }
        if !marked {
            return Ok(());
        }
        let written = serde_json::to_vec(&updated)
            .map_err(io::Error::other)
            .and_then(|contents| crate::filesystem::atomic_write(&self.path, &contents, 0o600))
            .map_err(Error::ApprovalRecord);
        if let Err(error) = written {
            *guard = None;
            return Err(error);
        }
        *guard = Some(updated);
        Ok(())
    }

    pub(crate) fn consume_starting_set(
        &self,
        set: &StartingSet,
        now: Timestamp,
    ) -> Result<(), Error> {
        let mut guard = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(self.load().map_err(Error::ApprovalRecord)?);
        }
        let consumption = guard.as_ref().expect("loaded above");
        let now = consumption
            .expiry_time_floor
            .map_or(now, |floor| now.max(floor));
        ployz_core::authority::validity::verify(set.issued_at, set.expires_at, now)
            .map_err(Error::EnforcementRefused)?;
        if consumption.operations.contains_key(&set.operation) {
            return Err(Error::EnforcementRefused(AuthorityRefusal::Replayed {
                operation: set.operation,
            }));
        }
        let mut updated = consumption.clone();
        updated.expiry_time_floor = Some(now);
        updated.operations.insert(
            set.operation,
            Operation {
                expires_at: set.expires_at,
                containers: BTreeMap::new(),
                hook: None,
                hook_made: false,
            },
        );
        let written = serde_json::to_vec(&updated)
            .map_err(io::Error::other)
            .and_then(|contents| crate::filesystem::atomic_write(&self.path, &contents, 0o600))
            .map_err(Error::ApprovalRecord);
        if let Err(error) = written {
            *guard = None;
            return Err(error);
        }
        *guard = Some(updated);
        Ok(())
    }

    pub(crate) fn join_result(
        &self,
        machine: ployz_core::MachineId,
    ) -> Result<Option<JoinResult>, Error> {
        let mut guard = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(self.load().map_err(Error::JoinRecord)?);
        }
        Ok(guard
            .as_ref()
            .expect("loaded above")
            .joins
            .get(&machine)
            .cloned())
    }

    pub(crate) fn require_unused_join(&self, machine: ployz_core::MachineId) -> Result<(), Error> {
        let mut guard = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(self.load().map_err(Error::JoinRecord)?);
        }
        if guard
            .as_ref()
            .expect("loaded above")
            .joins
            .contains_key(&machine)
        {
            return Err(Error::JoinConsumed(machine));
        }
        Ok(())
    }

    pub(crate) fn consume_join(&self, result: JoinResult, now: Timestamp) -> Result<(), Error> {
        let mut guard = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(self.load().map_err(Error::JoinRecord)?);
        }
        let consumption = guard.as_ref().expect("loaded above");
        let now = consumption
            .expiry_time_floor
            .map_or(now, |floor| now.max(floor));
        let approval = result.approval.open().map_err(Error::JoinRefused)?;
        ployz_core::authority::validity::verify(approval.issued_at, approval.expires_at, now)
            .map_err(Error::JoinRefused)?;
        let mut updated = consumption.clone();
        updated.expiry_time_floor = Some(now);
        if updated.joins.contains_key(&approval.machine.id) {
            return Err(Error::JoinConsumed(approval.machine.id));
        }
        updated.joins.insert(approval.machine.id, result);
        let written = serde_json::to_vec(&updated)
            .map_err(io::Error::other)
            .and_then(|contents| crate::filesystem::atomic_write(&self.path, &contents, 0o600))
            .map_err(Error::JoinRecord);
        if let Err(error) = written {
            *guard = None;
            return Err(error);
        }
        *guard = Some(updated);
        Ok(())
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
        let mut guard = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(self.load().map_err(Error::ApprovalRecord)?);
        }
        let consumption = guard.as_ref().expect("loaded above");
        let now = consumption
            .expiry_time_floor
            .map_or(now, |floor| now.max(floor));
        if let Err(refusal) = consumption.check_mark(rule.identity()) {
            return Err(refused(refusal));
        }
        let spent = hex::encode(Sha256::digest(jti.as_bytes()));
        if consumption.proofs.contains_key(&spent) {
            return Err(refused(AuthorityRefusal::ProofReplayed));
        }
        let mut updated = consumption.clone();
        updated.expiry_time_floor = Some(now);
        updated.proofs.retain(|_, expires| {
            expires.plus(ployz_core::authority::validity::CLOCK_SKEW_MILLIS) >= now
        });
        updated.proofs.insert(spent, expires_at);
        updated.raise_mark(rule.identity());
        self.write(&mut guard, updated)
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
        let mut guard = self.loaded()?;
        let consumption = guard.as_ref().expect("loaded above");
        consumption.check_mark(rule).map_err(refused)?;
        if consumption
            .rules
            .get(&rule.rule)
            .is_some_and(|mark| mark.generation >= rule.generation)
        {
            return Ok(());
        }
        let mut updated = consumption.clone();
        updated.raise_mark(rule);
        self.write(&mut guard, updated)
    }

    /// Record that a verified Release Channel approval of `channel.generation` exists for
    /// its Cluster, synced before the upgrade it admits writes its receipt. Writes only
    /// when the mark rises, so a resend after a crash between this write and the receipt
    /// is admitted again.
    ///
    /// # Errors
    /// Returns [`Error::UpgradeChannelRefused`] for a generation below the mark, and
    /// [`Error::ApprovalRecord`] when the record cannot be read or written.
    pub(crate) fn mark_upgrade_channel(&self, channel: &UpgradeChannel) -> Result<(), Error> {
        let mut guard = self.loaded()?;
        let consumption = guard.as_ref().expect("loaded above");
        let newest = consumption.upgrade_channels.get(&channel.cluster).copied();
        upgrade::admit_generation(channel.generation, newest)
            .map_err(Error::UpgradeChannelRefused)?;
        if newest == Some(channel.generation) {
            return Ok(());
        }
        let mut updated = consumption.clone();
        updated
            .upgrade_channels
            .insert(channel.cluster, channel.generation);
        self.write(&mut guard, updated)
    }

    /// Whether this Machine already admitted a creation of `claim`'s build under `rule`.
    pub(crate) fn accepted(
        &self,
        rule: &AutoDeployRule,
        claim: &BuildClaim,
    ) -> Result<bool, Error> {
        let guard = self.loaded()?;
        let accepted = guard
            .as_ref()
            .expect("loaded above")
            .accepted
            .get(&rule_service_key(rule.rule, None, &claim.service));
        Ok(AcceptedBuild::of(claim, None).is_some_and(|build| accepted == Some(&build)))
    }

    /// The preview this Machine already admitted a creation of `claim`'s build for under
    /// `rule` in `namespace`, which only that preview's Namespace derives from.
    pub(crate) fn accepted_preview(
        &self,
        rule: RuleId,
        namespace: &Namespace,
        claim: &BuildClaim,
    ) -> Result<Option<PreviewKey>, Error> {
        let guard = self.loaded()?;
        let accepted = guard
            .as_ref()
            .expect("loaded above")
            .accepted
            .get(&rule_service_key(rule, Some(namespace), &claim.service));
        Ok(accepted.and_then(|build| {
            let preview = build.preview.as_ref()?;
            (AcceptedBuild::of(claim, Some(preview)).as_ref() == Some(build))
                .then(|| preview.clone())
        }))
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
        let mut guard = self.loaded()?;
        let consumption = guard.as_ref().expect("loaded above");
        let rule = rule.identity();
        consumption.check_mark(rule).map_err(refused)?;
        let Some(build) = AcceptedBuild::of(claim, preview.map(|(_, key)| key)) else {
            return Ok(());
        };
        let key = rule_service_key(
            rule.rule,
            preview.map(|(namespace, _)| namespace),
            &claim.service,
        );
        if consumption.accepted.get(&key) == Some(&build) {
            return Ok(());
        }
        let mut updated = consumption.clone();
        updated.accepted.insert(key, build);
        self.write(&mut guard, updated)
    }

    /// Take the hook place of `claim`'s push under `rule` for the slot named `slot`,
    /// refusing one taken for another slot. A request resent for the same slot passes:
    /// the Machine adopts the hook that ran. A later push of the Service replaces the
    /// entry, so a push runs its hook once; a force push back to an earlier commit can
    /// run that commit's hook again. A preview rule keeps one entry per preview, named by
    /// its Namespace in `preview`.
    pub(crate) fn consume_rule_hook(
        &self,
        (rule, preview): (RuleId, Option<&Namespace>),
        claim: &BuildClaim,
        slot: &ployz_core::CreationKey,
        refused: impl FnOnce(AuthorityRefusal) -> Error,
    ) -> Result<(), Error> {
        let mut guard = self.loaded()?;
        let consumption = guard.as_ref().expect("loaded above");
        let key = rule_service_key(rule, preview, &claim.service);
        if let Some(taken) = consumption.rule_hooks.get(&key)
            && taken.generation == claim.generation
            && taken.commit == claim.commit
        {
            if &taken.slot == slot {
                return Ok(());
            }
            return Err(refused(AuthorityRefusal::Mismatch {
                field: "a hook run",
            }));
        }
        let mut updated = consumption.clone();
        updated.rule_hooks.insert(
            key,
            RuleHook {
                generation: claim.generation,
                commit: claim.commit,
                slot: slot.clone(),
                made: false,
            },
        );
        self.write(&mut guard, updated)
    }

    fn loaded(&self) -> Result<std::sync::MutexGuard<'_, Option<Consumption>>, Error> {
        let mut guard = self
            .operations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(self.load().map_err(Error::ApprovalRecord)?);
        }
        Ok(guard)
    }

    /// Apply `change` to this Machine's Volume records at `now`, raised to the log's time
    /// floor, and keep the result, synced, before returning its answer. A refusal keeps the
    /// records as they were, and an unchanged record is not written.
    pub(super) fn change_volumes<R>(
        &self,
        now: Timestamp,
        change: impl FnOnce(&mut VolumeRecords, Timestamp) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let mut guard = self.loaded()?;
        let consumption = guard.as_ref().expect("loaded above");
        let now = consumption
            .expiry_time_floor
            .map_or(now, |floor| now.max(floor));
        let mut updated = consumption.clone();
        let answer = change(&mut updated.volumes, now)?;
        if updated.volumes == consumption.volumes {
            return Ok(answer);
        }
        updated.expiry_time_floor = Some(now);
        self.write(&mut guard, updated)?;
        Ok(answer)
    }

    pub(super) fn read_volumes<R>(
        &self,
        read: impl FnOnce(&VolumeRecords) -> R,
    ) -> Result<R, Error> {
        let guard = self.loaded()?;
        Ok(read(&guard.as_ref().expect("loaded above").volumes))
    }

    fn write(
        &self,
        guard: &mut std::sync::MutexGuard<'_, Option<Consumption>>,
        updated: Consumption,
    ) -> Result<(), Error> {
        let written = serde_json::to_vec(&updated)
            .map_err(io::Error::other)
            .and_then(|contents| crate::filesystem::atomic_write(&self.path, &contents, 0o600))
            .map_err(Error::ApprovalRecord);
        if let Err(error) = written {
            **guard = None;
            return Err(error);
        }
        **guard = Some(updated);
        Ok(())
    }

    fn load(&self) -> io::Result<Consumption> {
        match std::fs::read(&self.path) {
            Ok(contents) => serde_json::from_slice(&contents).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Consumption::default()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
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
            ..short.clone()
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
            log.consume_with(
                &first,
                &container('a'),
                RestartStep::Stop,
                at(1),
                |path, contents| {
                    crate::filesystem::atomic_write(path, contents, 0o600)?;
                    Err(io::Error::other("directory sync failed after rename"))
                }
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
