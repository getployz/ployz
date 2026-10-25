//! Backfill one Sealing Key across a Cluster founded before Sealing Keys existed.
//!
//! The operator drives it (DESIGN bet 6): no Machine elects itself. One pass observes
//! every listed Machine, settles which key the Cluster has or who generates it, then
//! carries the key from a holder to each Participating Machine that needs access. Every
//! Machine RPC is bounded and safe to repeat, so a caller reruns the whole verb after any
//! failure. The report carries no ciphertext.

use std::collections::BTreeMap;

use futures_util::future::join_all;
use ployz_core::{
    DescribeContractRequest, GENERATE_SEALING_KEY_CAPABILITY, GenerateSealingKeyRequest,
    INSTALL_SEALING_KEY_CAPABILITY, InspectRequest, InstallSealingKeyRequest, LocalMachinePhase,
    MachineId, MachineTarget, RpcError, RpcErrorCode, SHARE_SEALING_KEY_CAPABILITY,
    SealedSealingKey, Sealing, SealingPublicKey, SealingRefusal, ShareSealingKeyRequest,
    WireGuardPublicKey, op,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::connect::{Client, rpc_error};

/// What the operator knows going in.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct BackfillSealingKeyRequest {
    /// The key the Cluster is known to hold. Set, only its holders are sources and a
    /// Machine holding another key is a `conflict`. Unset, the holders seen must agree.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub expected: Option<SealingPublicKey>,
    /// The Machine the operator chose to generate the key when no Machine holds one.
    /// Unset, no key is generated: the report names a `candidate` instead.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub generator: Option<MachineId>,
}

/// What one backfill pass found and did, flat on `result`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum BackfillSealingKeyReport {
    /// The Cluster's key is settled and every listed Machine was handled; see `machines`.
    Distributed {
        public_key: SealingPublicKey,
        machines: Vec<MachineBackfill>,
    },
    /// No Machine seen holds `expected`.
    NoHolder { expected: SealingPublicKey },
    /// The Machines seen hold different keys; the operator names one as `expected`.
    Conflicting { keys: Vec<SealingPublicKey> },
    /// No Machine seen holds a key. The operator names `candidate`, or another Machine,
    /// as `generator` to go on.
    NeedsGenerator { candidate: MachineId },
    /// No Machine seen holds a key, and none seen can generate one.
    NoCandidate,
    /// The chosen `generator` is not listed in the Cluster.
    GeneratorGone { generator: MachineId },
    /// The chosen `generator` was not seen Participating with a daemon that can generate.
    Waiting { generator: MachineId },
    /// The Cluster lost its key, as `machine` reported or refused to generate, so no new
    /// one is made: it awaits recovery of the key it published.
    AwaitsRecovery { machine: MachineId },
    /// The `generator` did not make a key. A refusal names itself in `error.details`.
    GenerationFailed {
        generator: MachineId,
        error: RpcError,
    },
}

/// One listed Machine and what the backfill made of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineBackfill {
    pub machine: MachineId,
    #[serde(flatten)]
    #[ts(flatten)]
    pub outcome: MachineOutcome,
}

/// One Machine's outcome, flat on `result`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum MachineOutcome {
    /// It already held the key.
    Held,
    /// It generated the key in this pass.
    Generated,
    /// It installed the key this pass carried from `from`.
    Installed { from: MachineId },
    /// It holds another key. The backfill never replaces one.
    Conflict { public_key: SealingPublicKey },
    /// No holder could carry the key to it; `error` is the last refusal.
    Failed { error: RpcError },
    /// Whether it holds the key could not be seen.
    Unknown { reason: String },
    /// It needs the key, and this pass could not give it one.
    Skipped { reason: SkipReason },
}

/// Why a Machine that needs the key was left alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Only a Participating Machine installs the key.
    NotParticipating,
    /// Its daemon predates the install route.
    CannotInstall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Listed {
    pub id: MachineId,
    pub invites_rpc: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Sight {
    Unknown { reason: String },
    Seen(Seen),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Seen {
    pub phase: LocalMachinePhase,
    pub wireguard_key: WireGuardPublicKey,
    pub sealing: Sealing,
    pub can: Can,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Can {
    pub generate: bool,
    pub share: bool,
    pub install: bool,
}

impl Seen {
    fn held(&self) -> Option<SealingPublicKey> {
        match self.sealing {
            Sealing::Held { public_key } => Some(public_key),
            Sealing::NeedsAccess | Sealing::AwaitsRecovery { .. } => None,
        }
    }

    fn participating(&self) -> bool {
        self.phase == LocalMachinePhase::Participating
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum Plan {
    Stop(BackfillSealingKeyReport),
    Generate {
        generator: MachineId,
    },
    Distribute {
        public_key: SealingPublicKey,
        sources: Vec<MachineId>,
    },
}

pub(crate) fn plan(
    request: &BackfillSealingKeyRequest,
    sights: &BTreeMap<MachineId, Sight>,
) -> Plan {
    let holders: Vec<(MachineId, SealingPublicKey, &Seen)> = seen(sights)
        .filter_map(|(id, seen)| seen.held().map(|key| (id, key, seen)))
        .collect();
    let public_key = match request.expected {
        Some(expected) => {
            if !holders.iter().any(|(_, key, _)| *key == expected) {
                return Plan::Stop(BackfillSealingKeyReport::NoHolder { expected });
            }
            expected
        }
        None => {
            let mut keys: Vec<SealingPublicKey> = Vec::new();
            for (_, key, _) in &holders {
                if !keys.contains(key) {
                    keys.push(*key);
                }
            }
            match keys.as_slice() {
                [] => {
                    return awaiting_recovery(sights)
                        .unwrap_or_else(|| generation(request.generator, sights));
                }
                [key] => *key,
                _ => return Plan::Stop(BackfillSealingKeyReport::Conflicting { keys }),
            }
        }
    };
    let sources = holders
        .iter()
        .filter(|(_, key, seen)| *key == public_key && seen.can.share)
        .map(|(id, _, _)| *id)
        .collect();
    Plan::Distribute {
        public_key,
        sources,
    }
}

fn awaiting_recovery(sights: &BTreeMap<MachineId, Sight>) -> Option<Plan> {
    seen(sights)
        .find(|(_, seen)| matches!(seen.sealing, Sealing::AwaitsRecovery { .. }))
        .map(|(machine, _)| Plan::Stop(BackfillSealingKeyReport::AwaitsRecovery { machine }))
}

fn generation(generator: Option<MachineId>, sights: &BTreeMap<MachineId, Sight>) -> Plan {
    let Some(generator) = generator else {
        let candidate = seen(sights)
            .find(|(_, seen)| can_generate(seen))
            .map(|(id, _)| id);
        return Plan::Stop(
            candidate.map_or(BackfillSealingKeyReport::NoCandidate, |candidate| {
                BackfillSealingKeyReport::NeedsGenerator { candidate }
            }),
        );
    };
    match sights.get(&generator) {
        None => Plan::Stop(BackfillSealingKeyReport::GeneratorGone { generator }),
        Some(Sight::Seen(seen)) if can_generate(seen) => Plan::Generate { generator },
        Some(_) => Plan::Stop(BackfillSealingKeyReport::Waiting { generator }),
    }
}

fn can_generate(seen: &Seen) -> bool {
    seen.participating() && seen.can.generate && seen.held().is_none()
}

fn seen(sights: &BTreeMap<MachineId, Sight>) -> impl Iterator<Item = (MachineId, &Seen)> {
    sights.iter().filter_map(|(id, sight)| match sight {
        Sight::Seen(seen) => Some((*id, seen)),
        Sight::Unknown { .. } => None,
    })
}

#[derive(Debug, PartialEq)]
pub(crate) struct Distribution {
    pub recipients: Vec<(MachineId, WireGuardPublicKey)>,
    pub settled: Vec<MachineBackfill>,
}

pub(crate) fn distribution(
    public_key: SealingPublicKey,
    sights: &BTreeMap<MachineId, Sight>,
    generated: Option<MachineId>,
) -> Distribution {
    let mut distribution = Distribution {
        recipients: Vec::new(),
        settled: Vec::new(),
    };
    for (&machine, sight) in sights {
        let outcome = if generated == Some(machine) {
            MachineOutcome::Generated
        } else {
            match sight {
                Sight::Unknown { reason } => MachineOutcome::Unknown {
                    reason: reason.clone(),
                },
                Sight::Seen(seen) => match seen.held() {
                    Some(key) if key == public_key => MachineOutcome::Held,
                    Some(key) => MachineOutcome::Conflict { public_key: key },
                    None if !seen.participating() => MachineOutcome::Skipped {
                        reason: SkipReason::NotParticipating,
                    },
                    None if !seen.can.install => MachineOutcome::Skipped {
                        reason: SkipReason::CannotInstall,
                    },
                    None => {
                        distribution.recipients.push((machine, seen.wireguard_key));
                        continue;
                    }
                },
            }
        };
        distribution
            .settled
            .push(MachineBackfill { machine, outcome });
    }
    distribution
}

pub(crate) trait BackfillClient {
    async fn list(&mut self) -> Result<Vec<Listed>, RpcError>;
    async fn observe(&mut self, listed: &[Listed]) -> BTreeMap<MachineId, Sight>;
    async fn generate(&mut self, generator: MachineId) -> Result<SealingPublicKey, RpcError>;
    async fn share(
        &mut self,
        holder: MachineId,
        recipient: MachineId,
        public_key: WireGuardPublicKey,
    ) -> Result<SealedSealingKey, RpcError>;
    async fn install(
        &mut self,
        recipient: MachineId,
        from: MachineId,
        sealed: SealedSealingKey,
        public_key: SealingPublicKey,
    ) -> Result<(), RpcError>;
}

/// The Cluster's Sealing Key, settled as Cloud settles it: the one its Machines
/// hold, carried to each that needs it, or one generated on the candidate a pass
/// names when none holds a key. Any other outcome is the report that stopped it.
///
/// # Errors
///
/// As [`execute`].
pub(crate) async fn settle<C: BackfillClient>(
    client: &mut C,
) -> Result<Result<SealingPublicKey, Box<BackfillSealingKeyReport>>, RpcError> {
    let mut request = BackfillSealingKeyRequest::default();
    loop {
        let report = execute(client, &request).await?;
        match &report {
            BackfillSealingKeyReport::Distributed { public_key, .. } => return Ok(Ok(*public_key)),
            BackfillSealingKeyReport::NeedsGenerator { candidate }
                if request.generator.is_none() =>
            {
                request.generator = Some(*candidate);
            }
            BackfillSealingKeyReport::NeedsGenerator { .. }
            | BackfillSealingKeyReport::NoHolder { .. }
            | BackfillSealingKeyReport::Conflicting { .. }
            | BackfillSealingKeyReport::NoCandidate
            | BackfillSealingKeyReport::GeneratorGone { .. }
            | BackfillSealingKeyReport::Waiting { .. }
            | BackfillSealingKeyReport::AwaitsRecovery { .. }
            | BackfillSealingKeyReport::GenerationFailed { .. } => {
                return Ok(Err(Box::new(report)));
            }
        }
    }
}

pub(crate) async fn execute<C: BackfillClient>(
    client: &mut C,
    request: &BackfillSealingKeyRequest,
) -> Result<BackfillSealingKeyReport, RpcError> {
    let listed = client.list().await?;
    let sights = client.observe(&listed).await;
    let (public_key, sources, generated) = match plan(request, &sights) {
        Plan::Stop(report) => return Ok(report),
        Plan::Distribute {
            public_key,
            sources,
        } => (public_key, sources, None),
        Plan::Generate { generator } => match client.generate(generator).await {
            Ok(public_key) => {
                let shares = matches!(
                    sights.get(&generator),
                    Some(Sight::Seen(seen)) if seen.can.share
                );
                let sources = if shares { vec![generator] } else { Vec::new() };
                (public_key, sources, Some(generator))
            }
            Err(error) => {
                return Ok(match error.sealing_refusal() {
                    Some(SealingRefusal::AwaitsRecovery) => {
                        BackfillSealingKeyReport::AwaitsRecovery { machine: generator }
                    }
                    _ => BackfillSealingKeyReport::GenerationFailed { generator, error },
                });
            }
        },
    };
    let Distribution {
        recipients,
        settled: mut machines,
    } = distribution(public_key, &sights, generated);
    for (recipient, wireguard_key) in recipients {
        let outcome = carry(client, &sources, recipient, wireguard_key, public_key).await;
        machines.push(MachineBackfill {
            machine: recipient,
            outcome,
        });
    }
    machines.sort_by_key(|machine| machine.machine);
    Ok(BackfillSealingKeyReport::Distributed {
        public_key,
        machines,
    })
}

async fn carry<C: BackfillClient>(
    client: &mut C,
    sources: &[MachineId],
    recipient: MachineId,
    wireguard_key: WireGuardPublicKey,
    public_key: SealingPublicKey,
) -> MachineOutcome {
    let mut last = RpcError {
        code: RpcErrorCode::Unsupported,
        message: "no Machine that holds the Sealing Key can share it".into(),
        details: Value::Null,
        cause: Vec::new(),
    };
    for &source in sources {
        let sealed = match client.share(source, recipient, wireguard_key).await {
            Ok(sealed) => sealed,
            Err(error) => {
                last = error;
                continue;
            }
        };
        match client.install(recipient, source, sealed, public_key).await {
            Ok(()) => return MachineOutcome::Installed { from: source },
            Err(error) => last = error,
        }
    }
    MachineOutcome::Failed { error: last }
}

impl BackfillClient for Client {
    async fn list(&mut self) -> Result<Vec<Listed>, RpcError> {
        let machines = self.machines().await.map_err(rpc_error)?;
        Ok(machines
            .iter()
            .map(|observation| Listed {
                id: observation.machine.id,
                invites_rpc: observation.invites_rpc(),
            })
            .collect())
    }

    async fn observe(&mut self, listed: &[Listed]) -> BTreeMap<MachineId, Sight> {
        let mut sights = BTreeMap::new();
        let mut asked = Vec::new();
        for machine in listed {
            if machine.invites_rpc {
                asked.push(observe(self.clone(), machine.id));
            } else {
                sights.insert(
                    machine.id,
                    Sight::Unknown {
                        reason: "its peers do not see it up".into(),
                    },
                );
            }
        }
        sights.extend(join_all(asked).await);
        sights
    }

    async fn generate(&mut self, generator: MachineId) -> Result<SealingPublicKey, RpcError> {
        self.read::<op::GenerateSealingKey>(
            GenerateSealingKeyRequest {},
            &MachineTarget::from(&generator),
        )
        .await
        .map(|held| held.public_key)
    }

    async fn share(
        &mut self,
        holder: MachineId,
        recipient: MachineId,
        public_key: WireGuardPublicKey,
    ) -> Result<SealedSealingKey, RpcError> {
        self.read::<op::ShareSealingKey>(
            ShareSealingKeyRequest {
                machine: recipient,
                public_key,
            },
            &MachineTarget::from(&holder),
        )
        .await
        .map(|shared| shared.sealed)
    }

    async fn install(
        &mut self,
        recipient: MachineId,
        from: MachineId,
        sealed: SealedSealingKey,
        public_key: SealingPublicKey,
    ) -> Result<(), RpcError> {
        self.read::<op::InstallSealingKey>(
            InstallSealingKeyRequest {
                from,
                sealed,
                public_key,
            },
            &MachineTarget::from(&recipient),
        )
        .await
        .map(|_| ())
    }
}

async fn observe(mut client: Client, id: MachineId) -> (MachineId, Sight) {
    let target = MachineTarget::from(&id);
    let unknown = |reason: String| (id, Sight::Unknown { reason });
    let description = match client
        .read::<op::DescribeContract>(DescribeContractRequest {}, &target)
        .await
    {
        Ok(description) => description,
        Err(error) => return unknown(error.to_string()),
    };
    let details = match client
        .read::<op::Inspect>(InspectRequest::default(), &target)
        .await
    {
        Ok(details) => details,
        Err(error) => return unknown(error.to_string()),
    };
    if details.id != id {
        return unknown(format!("it answered as Machine {}", details.id));
    }
    let Some(sealing) = details.sealing else {
        return unknown("its daemon predates the Sealing Key".into());
    };
    (
        id,
        Sight::Seen(Seen {
            phase: details.phase,
            wireguard_key: details.public_key,
            sealing,
            can: Can {
                generate: description.supports(GENERATE_SEALING_KEY_CAPABILITY),
                share: description.supports(SHARE_SEALING_KEY_CAPABILITY),
                install: description.supports(INSTALL_SEALING_KEY_CAPABILITY),
            },
        }),
    )
}

#[cfg(test)]
#[path = "sealing_backfill_tests.rs"]
mod tests;
