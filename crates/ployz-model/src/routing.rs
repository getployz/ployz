use super::*;

validated_string_id!(pub struct SidecarId("sidecar id"););

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarRecord {
    pub id: SidecarId,
    pub machine_id: MachineId,
    pub overlay_ip: Ipv4Addr,
    pub public_key: PublicKey,
    pub sidecar_container: String,
}

validated_string_id!(pub struct InstanceId("instance id"););

validated_string_id!(pub struct DeployId("deploy id"););

validated_string_id!(pub struct SlotId("slot id"););

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceRevisionRecord {
    pub namespace: Namespace,
    pub service: String,
    pub revision_hash: String,
    pub spec_json: String,
    pub created_by: MachineId,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceReleaseRecord {
    pub namespace: Namespace,
    pub service: String,
    pub release: ServiceRelease,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceRelease {
    pub target: ServiceReleaseTarget,
    pub slots: Vec<ServiceReleaseSlot>,
    pub updated_by_deploy_id: DeployId,
    pub updated_at: u64,
}

impl ServiceRelease {
    #[must_use]
    pub fn direct(
        revision_hash: impl Into<String>,
        slots: Vec<ServiceReleaseSlot>,
        updated_by_deploy_id: DeployId,
        updated_at: u64,
    ) -> Self {
        Self {
            target: ServiceReleaseTarget::Direct {
                revision_hash: revision_hash.into(),
            },
            slots,
            updated_by_deploy_id,
            updated_at,
        }
    }

    #[must_use]
    pub fn split(
        primary_revision_hash: impl Into<String>,
        allocations: Vec<ServiceTrafficAllocation>,
        slots: Vec<ServiceReleaseSlot>,
        updated_by_deploy_id: DeployId,
        updated_at: u64,
    ) -> Self {
        Self {
            target: ServiceReleaseTarget::Split {
                primary_revision_hash: primary_revision_hash.into(),
                allocations,
            },
            slots,
            updated_by_deploy_id,
            updated_at,
        }
    }

    #[must_use]
    pub fn primary_revision_hash(&self) -> &str {
        match &self.target {
            ServiceReleaseTarget::Direct { revision_hash } => revision_hash,
            ServiceReleaseTarget::Split {
                primary_revision_hash,
                ..
            } => primary_revision_hash,
        }
    }

    #[must_use]
    pub fn referenced_revision_hashes(&self) -> Vec<String> {
        match &self.target {
            ServiceReleaseTarget::Direct { revision_hash } => vec![revision_hash.clone()],
            ServiceReleaseTarget::Split {
                primary_revision_hash,
                allocations,
            } => {
                let mut revisions = Vec::with_capacity(allocations.len() + 1);
                revisions.push(primary_revision_hash.clone());
                for allocation in allocations {
                    if !revisions.contains(&allocation.revision_hash) {
                        revisions.push(allocation.revision_hash.clone());
                    }
                }
                revisions
            }
        }
    }

    #[must_use]
    pub fn routing_policy(&self) -> ServiceRoutingPolicy {
        match &self.target {
            ServiceReleaseTarget::Direct { revision_hash } => ServiceRoutingPolicy::Direct {
                revision_hash: revision_hash.clone(),
            },
            ServiceReleaseTarget::Split { allocations, .. } => ServiceRoutingPolicy::Split {
                allocations: allocations.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceReleaseTarget {
    Direct {
        revision_hash: String,
    },
    Split {
        primary_revision_hash: String,
        allocations: Vec<ServiceTrafficAllocation>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceRoutingPolicy {
    Direct {
        revision_hash: String,
    },
    Split {
        allocations: Vec<ServiceTrafficAllocation>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceTrafficAllocation {
    pub revision_hash: String,
    pub percent: u8,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceReleaseSlot {
    pub slot_id: SlotId,
    pub machine_id: MachineId,
    pub active_instance_id: InstanceId,
    pub revision_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceBranchLineageRecord {
    pub namespace: Namespace,
    pub service: String,
    pub revision_hash: String,
    pub source_namespace: Namespace,
    pub source_service: String,
    pub source_revision_hash: String,
    pub deploy_id: DeployId,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VolumeMovementRecord {
    pub namespace: Namespace,
    pub volume_name: String,
    pub from_machine: MachineId,
    pub to_machine: MachineId,
    pub final_machine: MachineId,
    pub deploy_id: DeployId,
    pub commit_deploy_id: DeployId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<DeployPhaseId>,
    pub snapshot_name: String,
    pub snapshot_guid: u64,
    pub bytes_transferred: u64,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VolumeBranchLineageRecord {
    pub namespace: Namespace,
    pub volume_name: String,
    pub source_namespace: Namespace,
    pub source_volume_name: String,
    pub source_machine: MachineId,
    pub target_machine: MachineId,
    pub data_policy: VolumeCloneDataPolicy,
    pub consistency: VolumeCloneConsistency,
    pub deploy_id: DeployId,
    pub commit_deploy_id: DeployId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<DeployPhaseId>,
    pub snapshot_name: String,
    pub snapshot_guid: u64,
    pub target_dataset: String,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeployPhaseCommitRecord {
    pub namespace: Namespace,
    pub deploy_id: DeployId,
    pub phase_id: DeployPhaseId,
    pub commit_deploy_id: DeployId,
    pub committed_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoutingState {
    pub machines: Vec<MachineMembership>,
    pub revisions: Vec<ServiceRevisionRecord>,
    pub releases: Vec<ServiceReleaseRecord>,
    pub instances: Vec<InstanceStatusRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RouteAudience {
    PublicGlobal,
    Authority(AuthorityId),
    Group(String),
    Gateway(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RouteGrantState {
    Active { route_id: String, expires_at: u64 },
    Revoked { revoked_at: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RouteGrantRecord {
    pub grant_id: String,
    pub owner_authority: AuthorityId,
    pub audience: RouteAudience,
    pub namespace: Namespace,
    pub state: RouteGrantState,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RouteExportLifecycleEvent {
    GrantCreated(RouteGrantRecord),
    GrantRevoked {
        grant_id: String,
        owner_authority: AuthorityId,
        revoked_at: u64,
    },
    GrantExpired {
        grant_id: String,
        owner_authority: AuthorityId,
        expired_at: u64,
    },
    RouteWithdrawn {
        owner_authority: AuthorityId,
        audience: RouteAudience,
        namespace: Namespace,
        route_id: String,
        reason: String,
        sequence: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutingEvent {
    MachineUpsert(MachineMembership),
    MachineRemoved {
        id: MachineId,
    },
    RevisionUpsert(ServiceRevisionRecord),
    RevisionRemoved {
        namespace: Namespace,
        service: String,
        revision_hash: String,
    },
    ReleaseUpsert(ServiceReleaseRecord),
    ReleaseRemoved {
        namespace: Namespace,
        service: String,
    },
    InstanceUpsert(InstanceStatusRecord),
    InstanceRemoved {
        instance_id: InstanceId,
    },
}
