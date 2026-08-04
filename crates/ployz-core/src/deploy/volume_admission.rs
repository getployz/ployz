//! Shared admission policy for mounted Plain and Provisioned Volumes.

use std::collections::{BTreeMap, BTreeSet};

use crate::deploy::{
    DatasetName, DatasetNameError, VolumeMaxSizeBytes, VolumeName, VolumeSpec, ZfsPoolName,
};
use crate::ids::{MachineId, NamespaceId};
use crate::intent::{VolumeKind, VolumePinState};
use crate::machine::{
    PoolCapacityAdmissionFailure, PoolCapacityFacts, StorageCapability, StorageTestimony,
    StorageUnavailableReason, admit_pool_quota_total,
};

#[derive(Debug, Clone, Copy)]
pub struct VolumeAdmissionInput<'a> {
    pub namespace_id: &'a NamespaceId,
    pub mounted_volume_names: &'a [VolumeName],
    pub declarations: &'a BTreeMap<VolumeName, VolumeSpec>,
    pub volume_pins: &'a [VolumePinState],
    pub selected_machine_id: &'a MachineId,
    pub storage_testimony: StorageTestimony<'a>,
}

#[derive(Debug, Clone, Copy)]
pub struct SingleVolumeAdmissionInput<'a> {
    pub namespace_id: &'a NamespaceId,
    pub volume_name: &'a VolumeName,
    pub declaration: &'a VolumeSpec,
    pub volume_pins: &'a [VolumePinState],
    pub selected_machine_id: &'a MachineId,
    pub storage_testimony: StorageTestimony<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeAdmissionDecision {
    Existing {
        pin: VolumePinState,
    },
    NeedsCreation {
        pin: VolumePinState,
    },
    NeedsQuotaGrowth {
        current: VolumePinState,
        replacement: VolumePinState,
    },
}

impl VolumeAdmissionDecision {
    #[must_use]
    pub const fn desired_pin(&self) -> &VolumePinState {
        match self {
            Self::Existing { pin } | Self::NeedsCreation { pin } => pin,
            Self::NeedsQuotaGrowth { replacement, .. } => replacement,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VolumeAdmissionFailure {
    #[error("mounted volume {volume_name:?} has no declaration")]
    MissingDeclaration { volume_name: VolumeName },
    #[error("volume {volume_name:?} has {pin_count} durable pins")]
    AmbiguousPins {
        volume_name: VolumeName,
        pin_count: usize,
    },
    #[error(
        "volume {volume_name:?} is pinned to machine {pinned_machine_id:?}, not selected machine {selected_machine_id:?}"
    )]
    PinnedToDifferentMachine {
        volume_name: VolumeName,
        pinned_machine_id: MachineId,
        selected_machine_id: MachineId,
    },
    #[error("volume {volume_name:?} cannot change kind after birth")]
    KindConversion {
        volume_name: VolumeName,
        declaration: VolumeSpec,
        pin_kind: VolumeKind,
    },
    #[error(
        "provisioned volume {volume_name:?} cannot shrink from {pinned_max_size_bytes:?} to {declared_max_size_bytes:?}"
    )]
    QuotaShrink {
        volume_name: VolumeName,
        declared_max_size_bytes: VolumeMaxSizeBytes,
        pinned_max_size_bytes: VolumeMaxSizeBytes,
    },
    #[error("machine {machine_id:?} did not answer storage testimony")]
    MachineSilent { machine_id: MachineId },
    #[error("machine {machine_id:?} did not report storage capability")]
    StorageTestimonyNotReported { machine_id: MachineId },
    #[error("machine {machine_id:?} has not prepared storage")]
    StorageUnprepared { machine_id: MachineId },
    #[error("machine {machine_id:?} storage is unavailable: {reason:?}")]
    StorageUnavailable {
        machine_id: MachineId,
        reason: StorageUnavailableReason,
    },
    #[error(
        "provisioned volume {volume_name:?} belongs to pool {pinned_pool:?}, not ready pool {reported_pool:?}"
    )]
    PoolMismatch {
        volume_name: VolumeName,
        pinned_pool: ZfsPoolName,
        reported_pool: ZfsPoolName,
    },
    #[error("cannot construct the dataset for volume {volume_name:?}: {source}")]
    DatasetIdentity {
        volume_name: VolumeName,
        source: DatasetNameError,
    },
    #[error("storage testimony contains duplicate quota rows for {dataset:?}")]
    DuplicateDatasetQuota { dataset: DatasetName },
    #[error("storage testimony has no quota row for existing dataset {dataset:?}")]
    DatasetQuotaNotReported { dataset: DatasetName },
    #[error("dataset {dataset:?} exists without a durable volume pin")]
    UnpinnedDatasetExists { dataset: DatasetName },
    #[error("quota admission arithmetic overflowed")]
    CapacityOverflow,
    #[error("quota admission exceeds available capacity")]
    CapacityExceeded {
        total_bytes: u64,
        provisioned_used_bytes: u64,
        free_bytes: u64,
        required_headroom_bytes: u64,
        requested_total_bytes: u64,
    },
    #[error(
        "storage capacity facts are inconsistent: total={total_bytes} provisioned_used={provisioned_used_bytes} free={free_bytes}"
    )]
    InconsistentCapacityFacts {
        total_bytes: u64,
        provisioned_used_bytes: u64,
        free_bytes: u64,
    },
}

#[must_use = "volume admission decisions must be executed or rejected"]
pub fn admit_mounted_volumes(
    input: VolumeAdmissionInput<'_>,
) -> Result<Vec<VolumeAdmissionDecision>, VolumeAdmissionFailure> {
    let storage_testimony = input.storage_testimony;
    let classified = classify_mounted_volumes(input)?;
    complete_volume_admissions(classified, storage_testimony)
}

#[must_use = "volume admission decisions must be executed or rejected"]
pub fn admit_volume(
    input: SingleVolumeAdmissionInput<'_>,
) -> Result<VolumeAdmissionDecision, VolumeAdmissionFailure> {
    let namespace_id = input.namespace_id;
    let selected_machine_id = input.selected_machine_id;
    let storage_testimony = input.storage_testimony;
    complete_volume_admission(
        classify_volume(input)?,
        namespace_id,
        selected_machine_id,
        storage_testimony,
    )
}

/// Validates durable birth-kind and quota monotonicity without consulting
/// live machine testimony. Namespace planning uses this before eligibility so
/// a structural intent error cannot be hidden by a liveness failure.
pub fn validate_mounted_volume_structure(
    input: VolumeAdmissionInput<'_>,
) -> Result<(), VolumeAdmissionFailure> {
    classify_mounted_volumes(input).map(|_| ())
}

#[derive(Debug)]
struct ClassifiedMountedVolumeAdmissions {
    namespace_id: NamespaceId,
    selected_machine_id: MachineId,
    decisions: Vec<VolumeAdmissionDecision>,
    pending_provisioned: Vec<PendingProvisionedAdmission>,
}

#[derive(Debug)]
enum ClassifiedVolumeAdmission {
    Decided(VolumeAdmissionDecision),
    PendingProvisioned(PendingProvisionedAdmission),
}

fn classify_mounted_volumes(
    input: VolumeAdmissionInput<'_>,
) -> Result<ClassifiedMountedVolumeAdmissions, VolumeAdmissionFailure> {
    let mounted_volume_names = input
        .mounted_volume_names
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut decisions = Vec::with_capacity(mounted_volume_names.len());
    let mut pending_provisioned = Vec::new();

    for volume_name in mounted_volume_names {
        let Some(declaration) = input.declarations.get(&volume_name) else {
            return Err(VolumeAdmissionFailure::MissingDeclaration { volume_name });
        };
        match classify_volume(SingleVolumeAdmissionInput {
            namespace_id: input.namespace_id,
            volume_name: &volume_name,
            declaration,
            volume_pins: input.volume_pins,
            selected_machine_id: input.selected_machine_id,
            storage_testimony: input.storage_testimony,
        })? {
            ClassifiedVolumeAdmission::Decided(decision) => decisions.push(decision),
            ClassifiedVolumeAdmission::PendingProvisioned(pending) => {
                pending_provisioned.push(pending);
            }
        }
    }

    Ok(ClassifiedMountedVolumeAdmissions {
        namespace_id: input.namespace_id.clone(),
        selected_machine_id: input.selected_machine_id.clone(),
        decisions,
        pending_provisioned,
    })
}

fn classify_volume(
    input: SingleVolumeAdmissionInput<'_>,
) -> Result<ClassifiedVolumeAdmission, VolumeAdmissionFailure> {
    let matching_pins = input
        .volume_pins
        .iter()
        .filter(|pin| {
            pin.namespace_id() == input.namespace_id && pin.volume_name() == input.volume_name
        })
        .collect::<Vec<_>>();
    let pin = match matching_pins.as_slice() {
        [] => None,
        [pin] => Some(*pin),
        pins => {
            return Err(VolumeAdmissionFailure::AmbiguousPins {
                volume_name: input.volume_name.clone(),
                pin_count: pins.len(),
            });
        }
    };

    let decision = if let Some(pin) = pin {
        if pin.machine_id() != input.selected_machine_id {
            return Err(VolumeAdmissionFailure::PinnedToDifferentMachine {
                volume_name: input.volume_name.clone(),
                pinned_machine_id: pin.machine_id().clone(),
                selected_machine_id: input.selected_machine_id.clone(),
            });
        }
        match (input.declaration, pin.kind()) {
            (VolumeSpec::Plain, VolumeKind::Plain) => {
                ClassifiedVolumeAdmission::Decided(VolumeAdmissionDecision::Existing {
                    pin: pin.clone(),
                })
            }
            (VolumeSpec::Plain, pin_kind @ VolumeKind::Provisioned { .. })
            | (VolumeSpec::Provisioned { .. }, pin_kind @ VolumeKind::Plain) => {
                return Err(VolumeAdmissionFailure::KindConversion {
                    volume_name: input.volume_name.clone(),
                    declaration: input.declaration.clone(),
                    pin_kind: pin_kind.clone(),
                });
            }
            (
                VolumeSpec::Provisioned {
                    max_size_bytes: declared,
                },
                VolumeKind::Provisioned {
                    max_size_bytes: pinned,
                    ..
                },
            ) if declared.get() < pinned.get() => {
                return Err(VolumeAdmissionFailure::QuotaShrink {
                    volume_name: input.volume_name.clone(),
                    declared_max_size_bytes: *declared,
                    pinned_max_size_bytes: *pinned,
                });
            }
            (
                VolumeSpec::Provisioned {
                    max_size_bytes: declared,
                },
                VolumeKind::Provisioned {
                    max_size_bytes: pinned,
                    ..
                },
            ) if declared == pinned => {
                ClassifiedVolumeAdmission::Decided(VolumeAdmissionDecision::Existing {
                    pin: pin.clone(),
                })
            }
            (
                VolumeSpec::Provisioned { max_size_bytes },
                VolumeKind::Provisioned { dataset, .. },
            ) => {
                ClassifiedVolumeAdmission::PendingProvisioned(PendingProvisionedAdmission::Growth {
                    volume_name: input.volume_name.clone(),
                    requested: *max_size_bytes,
                    current: pin.clone(),
                    dataset: dataset.clone(),
                })
            }
        }
    } else {
        match input.declaration {
            VolumeSpec::Plain => {
                ClassifiedVolumeAdmission::Decided(VolumeAdmissionDecision::NeedsCreation {
                    pin: VolumePinState::plain(
                        input.namespace_id.clone(),
                        input.volume_name.clone(),
                        input.selected_machine_id.clone(),
                    ),
                })
            }
            VolumeSpec::Provisioned { max_size_bytes } => {
                ClassifiedVolumeAdmission::PendingProvisioned(
                    PendingProvisionedAdmission::Creation {
                        volume_name: input.volume_name.clone(),
                        requested: *max_size_bytes,
                    },
                )
            }
        }
    };
    Ok(decision)
}

#[derive(Debug)]
enum PendingProvisionedAdmission {
    Creation {
        volume_name: VolumeName,
        requested: VolumeMaxSizeBytes,
    },
    Growth {
        volume_name: VolumeName,
        requested: VolumeMaxSizeBytes,
        current: VolumePinState,
        dataset: DatasetName,
    },
}

fn ready_capacity<'a>(
    machine_id: &MachineId,
    testimony: StorageTestimony<'a>,
) -> Result<(&'a ZfsPoolName, &'a PoolCapacityFacts), VolumeAdmissionFailure> {
    match testimony {
        StorageTestimony::NoAnswer => Err(VolumeAdmissionFailure::MachineSilent {
            machine_id: machine_id.clone(),
        }),
        StorageTestimony::Answered(None) => {
            Err(VolumeAdmissionFailure::StorageTestimonyNotReported {
                machine_id: machine_id.clone(),
            })
        }
        StorageTestimony::Answered(Some(StorageCapability::Unprepared)) => {
            Err(VolumeAdmissionFailure::StorageUnprepared {
                machine_id: machine_id.clone(),
            })
        }
        StorageTestimony::Answered(Some(StorageCapability::Unavailable { reason })) => {
            Err(VolumeAdmissionFailure::StorageUnavailable {
                machine_id: machine_id.clone(),
                reason: reason.clone(),
            })
        }
        StorageTestimony::Answered(Some(StorageCapability::Ready { pool, capacity })) => {
            Ok((pool, capacity))
        }
    }
}

fn complete_volume_admission(
    classified: ClassifiedVolumeAdmission,
    namespace_id: &NamespaceId,
    selected_machine_id: &MachineId,
    storage_testimony: StorageTestimony<'_>,
) -> Result<VolumeAdmissionDecision, VolumeAdmissionFailure> {
    match classified {
        ClassifiedVolumeAdmission::Decided(decision) => Ok(decision),
        ClassifiedVolumeAdmission::PendingProvisioned(pending) => {
            let mut capacity = ProvisionedCapacityAdmission::new(
                namespace_id,
                selected_machine_id,
                storage_testimony,
            )?;
            let decision = capacity.admit(pending)?;
            capacity.finish()?;
            Ok(decision)
        }
    }
}

fn complete_volume_admissions(
    mut classified: ClassifiedMountedVolumeAdmissions,
    storage_testimony: StorageTestimony<'_>,
) -> Result<Vec<VolumeAdmissionDecision>, VolumeAdmissionFailure> {
    if !classified.pending_provisioned.is_empty() {
        let mut capacity = ProvisionedCapacityAdmission::new(
            &classified.namespace_id,
            &classified.selected_machine_id,
            storage_testimony,
        )?;
        for pending in classified.pending_provisioned {
            classified.decisions.push(capacity.admit(pending)?);
        }
        capacity.finish()?;
    }
    classified.decisions.sort_by(|left, right| {
        left.desired_pin()
            .volume_name()
            .cmp(right.desired_pin().volume_name())
    });
    Ok(classified.decisions)
}

struct ProvisionedCapacityAdmission<'a> {
    namespace_id: &'a NamespaceId,
    machine_id: &'a MachineId,
    pool: &'a ZfsPoolName,
    capacity: &'a PoolCapacityFacts,
    quotas: BTreeMap<DatasetName, u64>,
    requested_total: u64,
}

impl<'a> ProvisionedCapacityAdmission<'a> {
    fn new(
        namespace_id: &'a NamespaceId,
        machine_id: &'a MachineId,
        storage_testimony: StorageTestimony<'a>,
    ) -> Result<Self, VolumeAdmissionFailure> {
        let (pool, capacity) = ready_capacity(machine_id, storage_testimony)?;
        let mut quotas = BTreeMap::new();
        let mut requested_total = 0_u64;
        for fact in &capacity.child_quotas {
            if quotas
                .insert(fact.dataset.clone(), fact.quota_bytes)
                .is_some()
            {
                return Err(VolumeAdmissionFailure::DuplicateDatasetQuota {
                    dataset: fact.dataset.clone(),
                });
            }
            requested_total = requested_total
                .checked_add(fact.quota_bytes)
                .ok_or(VolumeAdmissionFailure::CapacityOverflow)?;
        }

        Ok(Self {
            namespace_id,
            machine_id,
            pool,
            capacity,
            quotas,
            requested_total,
        })
    }

    fn admit(
        &mut self,
        admission: PendingProvisionedAdmission,
    ) -> Result<VolumeAdmissionDecision, VolumeAdmissionFailure> {
        match admission {
            PendingProvisionedAdmission::Creation {
                volume_name,
                requested,
            } => {
                let dataset = DatasetName::for_volume(self.pool, self.namespace_id, &volume_name)
                    .map_err(|source| VolumeAdmissionFailure::DatasetIdentity {
                    volume_name: volume_name.clone(),
                    source,
                })?;
                if self.quotas.contains_key(&dataset) {
                    return Err(VolumeAdmissionFailure::UnpinnedDatasetExists { dataset });
                }
                self.requested_total = self
                    .requested_total
                    .checked_add(requested.get())
                    .ok_or(VolumeAdmissionFailure::CapacityOverflow)?;
                let pin = VolumePinState::try_new(
                    self.namespace_id.clone(),
                    volume_name,
                    self.machine_id.clone(),
                    VolumeKind::Provisioned {
                        dataset,
                        max_size_bytes: requested,
                    },
                )
                .expect("canonical dataset identity matches its volume");
                Ok(VolumeAdmissionDecision::NeedsCreation { pin })
            }
            PendingProvisionedAdmission::Growth {
                volume_name,
                requested,
                current,
                dataset,
            } => {
                let pinned_pool = dataset.pool();
                if &pinned_pool != self.pool {
                    return Err(VolumeAdmissionFailure::PoolMismatch {
                        volume_name,
                        pinned_pool,
                        reported_pool: self.pool.clone(),
                    });
                }
                let Some(observed_quota) = self.quotas.get(&dataset) else {
                    return Err(VolumeAdmissionFailure::DatasetQuotaNotReported { dataset });
                };
                self.requested_total = self
                    .requested_total
                    .checked_sub(*observed_quota)
                    .and_then(|total| total.checked_add(requested.get()))
                    .ok_or(VolumeAdmissionFailure::CapacityOverflow)?;
                let replacement = VolumePinState::try_new(
                    self.namespace_id.clone(),
                    volume_name,
                    self.machine_id.clone(),
                    VolumeKind::Provisioned {
                        dataset,
                        max_size_bytes: requested,
                    },
                )
                .expect("existing canonical dataset identity remains unchanged");
                Ok(VolumeAdmissionDecision::NeedsQuotaGrowth {
                    current,
                    replacement,
                })
            }
        }
    }

    fn finish(self) -> Result<(), VolumeAdmissionFailure> {
        admit_pool_quota_total(self.capacity, self.requested_total).map_err(|failure| match failure
        {
            PoolCapacityAdmissionFailure::InconsistentFacts {
                total_bytes,
                provisioned_used_bytes,
                free_bytes,
            } => VolumeAdmissionFailure::InconsistentCapacityFacts {
                total_bytes,
                provisioned_used_bytes,
                free_bytes,
            },
            PoolCapacityAdmissionFailure::Overflow => VolumeAdmissionFailure::CapacityOverflow,
            PoolCapacityAdmissionFailure::Exceeded {
                free_bytes,
                required_headroom_bytes,
                requested_total_bytes,
            } => VolumeAdmissionFailure::CapacityExceeded {
                total_bytes: self.capacity.total_bytes,
                provisioned_used_bytes: self.capacity.provisioned_used_bytes,
                free_bytes,
                required_headroom_bytes,
                requested_total_bytes,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::DatasetQuotaFact;

    fn namespace_id() -> NamespaceId {
        NamespaceId::try_new("team-a").expect("valid namespace")
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine")
    }

    fn volume_name(value: &str) -> VolumeName {
        VolumeName::try_new(value).expect("valid volume")
    }

    fn size(value: u64) -> VolumeMaxSizeBytes {
        VolumeMaxSizeBytes::try_new(value).expect("positive size")
    }

    fn ready(
        pool: &ZfsPoolName,
        free_bytes: u64,
        child_quotas: Vec<DatasetQuotaFact>,
    ) -> StorageCapability {
        StorageCapability::Ready {
            pool: pool.clone(),
            capacity: PoolCapacityFacts {
                total_bytes: free_bytes,
                provisioned_used_bytes: 0,
                free_bytes,
                child_quotas,
            },
        }
    }

    fn provisioned_pin(
        namespace: &NamespaceId,
        volume: &VolumeName,
        machine: &MachineId,
        pool: &ZfsPoolName,
        quota: u64,
    ) -> VolumePinState {
        VolumePinState::try_new(
            namespace.clone(),
            volume.clone(),
            machine.clone(),
            VolumeKind::Provisioned {
                dataset: DatasetName::for_volume(pool, namespace, volume).expect("dataset"),
                max_size_bytes: size(quota),
            },
        )
        .expect("pin")
    }

    #[test]
    fn single_volume_admission_returns_one_canonical_decision() {
        let namespace = namespace_id();
        let machine = machine_id("machine-a");
        let volume = volume_name("data");

        let decision = admit_volume(SingleVolumeAdmissionInput {
            namespace_id: &namespace,
            volume_name: &volume,
            declaration: &VolumeSpec::Plain,
            volume_pins: &[],
            selected_machine_id: &machine,
            storage_testimony: StorageTestimony::NoAnswer,
        })
        .expect("plain volume needs no storage testimony");

        assert_eq!(
            decision,
            VolumeAdmissionDecision::NeedsCreation {
                pin: VolumePinState::plain(namespace, volume, machine)
            }
        );
    }

    #[test]
    fn mounted_volume_requires_a_declaration_but_unmounted_declarations_are_skipped() {
        let namespace = namespace_id();
        let machine = machine_id("machine-a");
        let mounted = volume_name("mounted");
        let unmounted = volume_name("unmounted");
        let declarations = BTreeMap::from([(
            unmounted,
            VolumeSpec::Provisioned {
                max_size_bytes: size(99),
            },
        )]);

        let error = admit_mounted_volumes(VolumeAdmissionInput {
            namespace_id: &namespace,
            mounted_volume_names: std::slice::from_ref(&mounted),
            declarations: &declarations,
            volume_pins: &[],
            selected_machine_id: &machine,
            storage_testimony: StorageTestimony::NoAnswer,
        })
        .expect_err("missing declaration");

        assert_eq!(
            error,
            VolumeAdmissionFailure::MissingDeclaration {
                volume_name: mounted
            }
        );

        let skipped = admit_mounted_volumes(VolumeAdmissionInput {
            namespace_id: &namespace,
            mounted_volume_names: &[],
            declarations: &declarations,
            volume_pins: &[],
            selected_machine_id: &machine,
            storage_testimony: StorageTestimony::NoAnswer,
        })
        .expect("unmounted provisioned declaration needs no testimony");
        assert!(skipped.is_empty());
    }

    #[test]
    fn equal_existing_quota_needs_no_live_testimony() {
        let namespace = namespace_id();
        let machine = machine_id("machine-a");
        let volume = volume_name("data");
        let pool = ZfsPoolName::try_new("tank").expect("pool");
        let pin = provisioned_pin(&namespace, &volume, &machine, &pool, 100);
        let declarations = BTreeMap::from([(
            volume.clone(),
            VolumeSpec::Provisioned {
                max_size_bytes: size(100),
            },
        )]);

        let decisions = admit_mounted_volumes(VolumeAdmissionInput {
            namespace_id: &namespace,
            mounted_volume_names: std::slice::from_ref(&volume),
            declarations: &declarations,
            volume_pins: std::slice::from_ref(&pin),
            selected_machine_id: &machine,
            storage_testimony: StorageTestimony::NoAnswer,
        })
        .expect("stored quota is sufficient");

        assert_eq!(decisions, vec![VolumeAdmissionDecision::Existing { pin }]);
    }

    #[test]
    fn rejects_both_kind_conversions_and_quota_shrink() {
        let namespace = namespace_id();
        let machine = machine_id("machine-a");
        let plain = volume_name("plain");
        let provisioned = volume_name("provisioned");
        let pool = ZfsPoolName::try_new("tank").expect("pool");
        let plain_pin = VolumePinState::plain(namespace.clone(), plain.clone(), machine.clone());
        let provisioned_pin = provisioned_pin(&namespace, &provisioned, &machine, &pool, 100);

        for (volume, declaration, pin) in [
            (
                plain.clone(),
                VolumeSpec::Provisioned {
                    max_size_bytes: size(100),
                },
                plain_pin,
            ),
            (
                provisioned.clone(),
                VolumeSpec::Plain,
                provisioned_pin.clone(),
            ),
        ] {
            let declarations = BTreeMap::from([(volume.clone(), declaration)]);
            assert!(matches!(
                admit_mounted_volumes(VolumeAdmissionInput {
                    namespace_id: &namespace,
                    mounted_volume_names: std::slice::from_ref(&volume),
                    declarations: &declarations,
                    volume_pins: std::slice::from_ref(&pin),
                    selected_machine_id: &machine,
                    storage_testimony: StorageTestimony::NoAnswer,
                }),
                Err(VolumeAdmissionFailure::KindConversion { .. })
            ));
        }

        let declarations = BTreeMap::from([(
            provisioned.clone(),
            VolumeSpec::Provisioned {
                max_size_bytes: size(99),
            },
        )]);
        assert!(matches!(
            admit_mounted_volumes(VolumeAdmissionInput {
                namespace_id: &namespace,
                mounted_volume_names: std::slice::from_ref(&provisioned),
                declarations: &declarations,
                volume_pins: std::slice::from_ref(&provisioned_pin),
                selected_machine_id: &machine,
                storage_testimony: StorageTestimony::NoAnswer,
            }),
            Err(VolumeAdmissionFailure::QuotaShrink { .. })
        ));
    }

    #[test]
    fn rejects_ambiguous_full_pin_state() {
        let namespace = namespace_id();
        let machine = machine_id("machine-a");
        let volume = volume_name("data");
        let declarations = BTreeMap::from([(volume.clone(), VolumeSpec::Plain)]);
        let pins = vec![
            VolumePinState::plain(namespace.clone(), volume.clone(), machine.clone()),
            VolumePinState::plain(namespace.clone(), volume.clone(), machine.clone()),
        ];

        assert_eq!(
            admit_mounted_volumes(VolumeAdmissionInput {
                namespace_id: &namespace,
                mounted_volume_names: std::slice::from_ref(&volume),
                declarations: &declarations,
                volume_pins: &pins,
                selected_machine_id: &machine,
                storage_testimony: StorageTestimony::NoAnswer,
            }),
            Err(VolumeAdmissionFailure::AmbiguousPins {
                volume_name: volume,
                pin_count: 2
            })
        );
    }

    #[test]
    fn new_provisioned_volume_requires_ready_testimony() {
        let namespace = namespace_id();
        let machine = machine_id("machine-a");
        let volume = volume_name("data");
        let declarations = BTreeMap::from([(
            volume.clone(),
            VolumeSpec::Provisioned {
                max_size_bytes: size(100),
            },
        )]);

        assert_eq!(
            admit_mounted_volumes(VolumeAdmissionInput {
                namespace_id: &namespace,
                mounted_volume_names: std::slice::from_ref(&volume),
                declarations: &declarations,
                volume_pins: &[],
                selected_machine_id: &machine,
                storage_testimony: StorageTestimony::NoAnswer,
            }),
            Err(VolumeAdmissionFailure::MachineSilent {
                machine_id: machine.clone()
            })
        );

        let unavailable_reason = StorageUnavailableReason::ZfsModuleMissing;
        let testimonies = [
            (
                StorageTestimony::Answered(None),
                VolumeAdmissionFailure::StorageTestimonyNotReported {
                    machine_id: machine.clone(),
                },
            ),
            (
                StorageTestimony::Answered(Some(&StorageCapability::Unprepared)),
                VolumeAdmissionFailure::StorageUnprepared {
                    machine_id: machine.clone(),
                },
            ),
            (
                StorageTestimony::Answered(Some(&StorageCapability::Unavailable {
                    reason: unavailable_reason.clone(),
                })),
                VolumeAdmissionFailure::StorageUnavailable {
                    machine_id: machine.clone(),
                    reason: unavailable_reason,
                },
            ),
        ];
        for (testimony, expected) in testimonies {
            assert_eq!(
                admit_mounted_volumes(VolumeAdmissionInput {
                    namespace_id: &namespace,
                    mounted_volume_names: std::slice::from_ref(&volume),
                    declarations: &declarations,
                    volume_pins: &[],
                    selected_machine_id: &machine,
                    storage_testimony: testimony,
                }),
                Err(expected)
            );
        }
    }

    #[test]
    fn batch_capacity_counts_orphans_and_replaces_growth_quota() {
        let namespace = namespace_id();
        let machine = machine_id("machine-a");
        let pool = ZfsPoolName::try_new("tank").expect("pool");
        let growing = volume_name("growing");
        let fresh = volume_name("fresh");
        let current = provisioned_pin(&namespace, &growing, &machine, &pool, 100);
        let current_dataset = match current.kind() {
            VolumeKind::Provisioned { dataset, .. } => dataset.clone(),
            VolumeKind::Plain => unreachable!(),
        };
        let orphan =
            DatasetName::for_volume(&pool, &namespace, &volume_name("orphan")).expect("dataset");
        let capability = ready(
            &pool,
            350,
            vec![
                DatasetQuotaFact {
                    dataset: current_dataset,
                    quota_bytes: 100,
                },
                DatasetQuotaFact {
                    dataset: orphan,
                    quota_bytes: 50,
                },
            ],
        );
        let declarations = BTreeMap::from([
            (
                growing.clone(),
                VolumeSpec::Provisioned {
                    max_size_bytes: size(200),
                },
            ),
            (
                fresh.clone(),
                VolumeSpec::Provisioned {
                    max_size_bytes: size(100),
                },
            ),
        ]);

        let decisions = admit_mounted_volumes(VolumeAdmissionInput {
            namespace_id: &namespace,
            mounted_volume_names: &[fresh.clone(), growing.clone()],
            declarations: &declarations,
            volume_pins: std::slice::from_ref(&current),
            selected_machine_id: &machine,
            storage_testimony: StorageTestimony::Answered(Some(&capability)),
        })
        .expect("200 replacement + 100 creation + 50 orphan exactly fits");

        let [fresh_decision, growing_decision] = decisions.as_slice() else {
            panic!("expected creation and growth decisions, got {decisions:?}");
        };
        assert_eq!(fresh_decision.desired_pin().volume_name(), &fresh);
        assert_eq!(growing_decision.desired_pin().volume_name(), &growing);
        assert!(matches!(
            fresh_decision,
            VolumeAdmissionDecision::NeedsCreation { .. }
        ));
        assert!(matches!(
            growing_decision,
            VolumeAdmissionDecision::NeedsQuotaGrowth { .. }
        ));
    }

    #[test]
    fn capacity_rejects_duplicate_rows_and_insufficient_total() {
        let namespace = namespace_id();
        let machine = machine_id("machine-a");
        let pool = ZfsPoolName::try_new("tank").expect("pool");
        let volume = volume_name("fresh");
        let orphan =
            DatasetName::for_volume(&pool, &namespace, &volume_name("orphan")).expect("dataset");
        let declarations = BTreeMap::from([(
            volume.clone(),
            VolumeSpec::Provisioned {
                max_size_bytes: size(51),
            },
        )]);

        let duplicate = ready(
            &pool,
            100,
            vec![
                DatasetQuotaFact {
                    dataset: orphan.clone(),
                    quota_bytes: 10,
                },
                DatasetQuotaFact {
                    dataset: orphan.clone(),
                    quota_bytes: 10,
                },
            ],
        );
        assert_eq!(
            admit_mounted_volumes(VolumeAdmissionInput {
                namespace_id: &namespace,
                mounted_volume_names: std::slice::from_ref(&volume),
                declarations: &declarations,
                volume_pins: &[],
                selected_machine_id: &machine,
                storage_testimony: StorageTestimony::Answered(Some(&duplicate)),
            }),
            Err(VolumeAdmissionFailure::DuplicateDatasetQuota {
                dataset: orphan.clone()
            })
        );

        let insufficient = ready(
            &pool,
            100,
            vec![DatasetQuotaFact {
                dataset: orphan,
                quota_bytes: 50,
            }],
        );
        assert_eq!(
            admit_mounted_volumes(VolumeAdmissionInput {
                namespace_id: &namespace,
                mounted_volume_names: std::slice::from_ref(&volume),
                declarations: &declarations,
                volume_pins: &[],
                selected_machine_id: &machine,
                storage_testimony: StorageTestimony::Answered(Some(&insufficient)),
            }),
            Err(VolumeAdmissionFailure::CapacityExceeded {
                total_bytes: 100,
                provisioned_used_bytes: 0,
                free_bytes: 100,
                required_headroom_bytes: 101,
                requested_total_bytes: 101,
            })
        );
    }
}
