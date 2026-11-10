//! One authorization class for every catalog route, and the cases that say what an
//! enforcing Machine does with each branch an Operator sends unsigned.
//!
//! The table is keyed by the catalog's generated capability constants. Tests in
//! `tests/capabilities.rs` hold it to the catalog, and a behaviour test in `ployzd` sends
//! every case to a real Machine API. `examples/coverage_cases.rs` prints [`CASES`] as JSON
//! for the live attacker harness, so a lane follows the table as later slices move rows.

use serde::Serialize;

use crate::{
    APPROVE_JOIN_CAPABILITY, BUILD_CAPABILITY, BUILD_GRANT_RULE_CAPABILITY,
    DEPLOY_APPROVAL_CAPABILITY, DEPLOY_PROPOSAL_CAPABILITY, DEPLOY_RULE_CAPABILITY,
    DESCRIBE_CONTRACT_CAPABILITY, ENROLL_FOR_RECOVERY_CAPABILITY, ENROLL_MACHINE_CAPABILITY,
    ENSURE_CONTAINER_CAPABILITY, ENSURE_IMAGE_INGEST_CAPABILITY, EXEC_CONTAINER_CAPABILITY,
    FORGET_MACHINE_CAPABILITY, GENERATE_SEALING_KEY_CAPABILITY, INSPECT_BUILD_GRANT_CAPABILITY,
    INSPECT_CONTAINER_CAPABILITY, INSPECT_MACHINE_CAPABILITY, INSPECT_STORAGE_CAPABILITY,
    INSPECT_VOLUME_COPY_CAPABILITY, INSTALL_SEALING_KEY_CAPABILITY, JOIN_APPROVAL_CAPABILITY,
    LEAVE_MACHINE_CAPABILITY, LIST_CONTAINERS_CAPABILITY, LIST_IMAGES_CAPABILITY,
    LIST_MACHINES_CAPABILITY, LIST_VOLUMES_CAPABILITY, LOG_HISTORY_CAPABILITY,
    METRIC_HISTORY_CAPABILITY, MINT_BUILD_GRANT_CAPABILITY, PENDING_JOINS_CAPABILITY,
    PREPARE_VOLUMES_CAPABILITY, PROPOSAL_APPROVAL_CAPABILITY, PRUNE_IMAGES_CAPABILITY,
    PUBLISH_CERTIFICATE_MATERIAL_CAPABILITY, PULL_IMAGE_CAPABILITY,
    PULL_IMAGE_FROM_MACHINE_CAPABILITY, READ_CLUSTER_NAME_CAPABILITY,
    READ_ROUTED_ADDRESSES_CAPABILITY, READ_ROUTES_CAPABILITY, RECOVER_SEALING_KEY_CAPABILITY,
    REGISTER_MACHINE_CAPABILITY, REMOVE_TRUSTED_KEY_CAPABILITY, REMOVE_VOLUME_CAPABILITY,
    REQUEST_MACHINE_UPGRADE_CAPABILITY, RESTART_APPROVAL_CAPABILITY, RETAINED_ENV_CAPABILITY,
    RUNTIME_WATCH_CAPABILITY, SEED_VOLUME_CAPABILITY, SELECT_ENDPOINT_CAPABILITY,
    SET_CLUSTER_NAME_CAPABILITY, SET_DEVICE_CAPABILITY, SET_DEVICE_ENFORCEMENT_CAPABILITY,
    SET_INGRESS_DOORS_CAPABILITY, SET_MANAGEMENT_CLIENT_CAPABILITY, SET_PASSKEY_CAPABILITY,
    SET_PRIVATE_ACCESS_CAPABILITY, SET_PUSH_TARGET_CAPABILITY, SET_VOLUME_COPY_CAPABILITY,
    SET_VOLUME_SIZE_CAPABILITY, SHARE_SEALING_KEY_CAPABILITY, TAIL_LOGS_CAPABILITY,
    UPDATE_CONTAINER_LIMITS_CAPABILITY, UPDATE_MACHINE_CAPABILITY, UPGRADE_EXACT_CAPABILITY,
    VOLUME_APPROVAL_CAPABILITY, WRAP_SEALING_KEY_CAPABILITY,
};

/// A statement a trusted key signs that admits an Operator's call.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Statement {
    Join,
    Deploy,
    Restart,
    Proposal,
    Rule,
    Volume,
    Upgrade,
    Teardown,
    PrivateAccess,
}

impl Statement {
    /// The capabilities a refusal names when this statement is the one missing. Empty for a
    /// statement whose refusals name the route itself.
    #[must_use]
    pub const fn capabilities(self) -> &'static [&'static str] {
        match self {
            Self::Join => &[JOIN_APPROVAL_CAPABILITY],
            Self::Deploy => &[DEPLOY_APPROVAL_CAPABILITY],
            Self::Restart => &[RESTART_APPROVAL_CAPABILITY],
            Self::Proposal => &[PROPOSAL_APPROVAL_CAPABILITY, DEPLOY_PROPOSAL_CAPABILITY],
            Self::Rule => &[DEPLOY_RULE_CAPABILITY, BUILD_GRANT_RULE_CAPABILITY],
            Self::Volume | Self::Upgrade | Self::Teardown | Self::PrivateAccess => &[],
        }
    }
}

/// What an Operator's call needs on a Machine that enforces Device approval.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "class", content = "detail")]
pub enum Authorization {
    /// Changes nothing on the Machine. Served.
    Read,
    /// Catalog scope Full: refused at dispatch for any caller but the local socket or a
    /// Device's own connection.
    Full,
    /// Refused without one of these statements, naming the route's capability or a
    /// statement's. Empty means only a Full caller is served.
    Signed(&'static [Statement]),
    /// Served unsigned. The reason says why the worst Cloud can do with it is mislabel,
    /// reorder, delay or drop, or an effect that crosses no part of the bar.
    Bound(&'static str),
    /// Not gated yet. Names the slice that gates it.
    Pending(&'static str),
}

use Authorization::{Bound, Full, Pending, Read, Signed};
use Statement::{Deploy, Join, Proposal, Restart, Rule, Volume};

const IMAGE_BOUND: &str = "content-addressed; checked at creation by C";

/// Every catalog route's class, in catalog order.
pub const AUTHORIZATION: &[(&str, Authorization)] = &[
    (DESCRIBE_CONTRACT_CAPABILITY, Read),
    (INSPECT_MACHINE_CAPABILITY, Read),
    (ENROLL_MACHINE_CAPABILITY, Pending("F7")),
    (ENROLL_FOR_RECOVERY_CAPABILITY, Pending("F7")),
    (WRAP_SEALING_KEY_CAPABILITY, Full),
    (RECOVER_SEALING_KEY_CAPABILITY, Full),
    (REGISTER_MACHINE_CAPABILITY, Signed(&[Join])),
    (
        SET_MANAGEMENT_CLIENT_CAPABILITY,
        Bound("mints only Operator authority Cloud already has"),
    ),
    (PENDING_JOINS_CAPABILITY, Read),
    (APPROVE_JOIN_CAPABILITY, Signed(&[Join])),
    (SET_DEVICE_CAPABILITY, Full),
    (
        SET_PUSH_TARGET_CAPABILITY,
        Bound("crash events are data Cloud already reads"),
    ),
    (SET_DEVICE_ENFORCEMENT_CAPABILITY, Full),
    (SET_PASSKEY_CAPABILITY, Full),
    (REMOVE_TRUSTED_KEY_CAPABILITY, Full),
    (SELECT_ENDPOINT_CAPABILITY, Bound("residual: delay or drop")),
    (LIST_MACHINES_CAPABILITY, Read),
    (
        UPDATE_MACHINE_CAPABILITY,
        Bound("residual: mislabel; approvals bind IDs"),
    ),
    (
        REQUEST_MACHINE_UPGRADE_CAPABILITY,
        Bound("signed releases only, forward only"),
    ),
    (
        GENERATE_SEALING_KEY_CAPABILITY,
        Bound("the key never leaves the Machine"),
    ),
    (SHARE_SEALING_KEY_CAPABILITY, Signed(&[Join])),
    (
        INSTALL_SEALING_KEY_CAPABILITY,
        Bound("Cloud holds no Machine's key to seal from"),
    ),
    (LEAVE_MACHINE_CAPABILITY, Signed(&[])),
    (FORGET_MACHINE_CAPABILITY, Signed(&[])),
    (LIST_CONTAINERS_CAPABILITY, Read),
    (INSPECT_CONTAINER_CAPABILITY, Read),
    (
        ENSURE_CONTAINER_CAPABILITY,
        Signed(&[Deploy, Restart, Proposal, Rule]),
    ),
    (
        UPDATE_CONTAINER_LIMITS_CAPABILITY,
        Bound("residual: a limit can slow or stop a Service, a drop"),
    ),
    (INSPECT_STORAGE_CAPABILITY, Read),
    (PREPARE_VOLUMES_CAPABILITY, Bound("checked at mount by C")),
    (LIST_VOLUMES_CAPABILITY, Read),
    (REMOVE_VOLUME_CAPABILITY, Pending("E6")),
    (INSPECT_VOLUME_COPY_CAPABILITY, Read),
    (SEED_VOLUME_CAPABILITY, Signed(&[Volume])),
    (SET_VOLUME_COPY_CAPABILITY, Pending("E3 to E6")),
    (SET_VOLUME_SIZE_CAPABILITY, Pending("E7")),
    (LIST_IMAGES_CAPABILITY, Read),
    (
        PRUNE_IMAGES_CAPABILITY,
        Bound("residual: the next deploy pulls again"),
    ),
    (ENSURE_IMAGE_INGEST_CAPABILITY, Bound(IMAGE_BOUND)),
    (PULL_IMAGE_CAPABILITY, Bound(IMAGE_BOUND)),
    (PULL_IMAGE_FROM_MACHINE_CAPABILITY, Bound(IMAGE_BOUND)),
    (MINT_BUILD_GRANT_CAPABILITY, Signed(&[Rule])),
    (INSPECT_BUILD_GRANT_CAPABILITY, Read),
    (
        PUBLISH_CERTIFICATE_MATERIAL_CAPABILITY,
        Bound("DNS and ACME authority Cloud holds; thread X"),
    ),
    (READ_ROUTES_CAPABILITY, Read),
    (READ_ROUTED_ADDRESSES_CAPABILITY, Read),
    (SET_INGRESS_DOORS_CAPABILITY, Signed(&[])),
    (SET_PRIVATE_ACCESS_CAPABILITY, Signed(&[])),
    (READ_CLUSTER_NAME_CAPABILITY, Read),
    (SET_CLUSTER_NAME_CAPABILITY, Bound("residual: mislabel")),
    (METRIC_HISTORY_CAPABILITY, Read),
    (RETAINED_ENV_CAPABILITY, Read),
    (LOG_HISTORY_CAPABILITY, Read),
    (TAIL_LOGS_CAPABILITY, Read),
    (RUNTIME_WATCH_CAPABILITY, Read),
    (EXEC_CONTAINER_CAPABILITY, Full),
    (BUILD_CAPABILITY, Signed(&[])),
];

/// The class of the route named by `capability`, or `None` for a name that is not a route.
#[must_use]
pub fn authorization(capability: &str) -> Option<Authorization> {
    AUTHORIZATION
        .iter()
        .find(|(name, _)| *name == capability)
        .map(|(_, class)| *class)
}

/// Whether an Operator caller can never be served `capability` on an enforcing Machine,
/// so a client holding a Full path must send it there itself: a call forwarded over the
/// mesh arrives as an Operator's.
#[must_use]
pub fn needs_full_caller(capability: &str) -> bool {
    matches!(authorization(capability), Some(Full | Signed(&[])))
}

/// What an enforcing Machine must do with a case sent by Cloud, with no statement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "answer", content = "detail")]
pub enum Expected {
    /// Refused, naming one of these capabilities. Nothing changed.
    Refused(&'static [&'static str]),
    /// Served, and the named check holds.
    Bounded(&'static str),
    /// Served: the Machine refuses nothing for authority.
    Served,
}

/// One branch of one route, as the behaviour test and the attacker harness send it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Case {
    pub capability: &'static str,
    /// Which mutation of the route this is, unique within the route.
    pub branch: &'static str,
    /// The request the harness sends, in words.
    pub sends: &'static str,
    pub expected: Expected,
}

const fn case(
    capability: &'static str,
    branch: &'static str,
    sends: &'static str,
    expected: Expected,
) -> Case {
    Case {
        capability,
        branch,
        sends,
        expected,
    }
}

const STARTS_NOTHING: &str = "a following unsigned EnsureContainer create that uses it is refused";

/// Every case an enforcing Machine is held to, in catalog order. A `Pending` row's cases
/// are Served until its slice, except a branch already refused today.
pub const CASES: &[Case] = &[
    case(
        ENROLL_MACHINE_CAPABILITY,
        "member, no capsule",
        "an Enroll as a member whose registration carries no approved capsule",
        Expected::Served,
    ),
    case(
        ENROLL_MACHINE_CAPABILITY,
        "member, approved capsule",
        "an Enroll as a member whose registration carries an approved capsule",
        Expected::Served,
    ),
    case(
        ENROLL_MACHINE_CAPABILITY,
        "founder",
        "an Enroll as a founder",
        Expected::Refused(&[JOIN_APPROVAL_CAPABILITY]),
    ),
    case(
        ENROLL_FOR_RECOVERY_CAPABILITY,
        "founder",
        "an EnrollForRecovery founding a Cluster that awaits recovery",
        Expected::Refused(&[JOIN_APPROVAL_CAPABILITY]),
    ),
    case(
        WRAP_SEALING_KEY_CAPABILITY,
        "from the mesh",
        "a WrapSealingKey from Cloud",
        Expected::Refused(&[WRAP_SEALING_KEY_CAPABILITY]),
    ),
    case(
        RECOVER_SEALING_KEY_CAPABILITY,
        "from the mesh",
        "a RecoverSealingKey from Cloud",
        Expected::Refused(&[RECOVER_SEALING_KEY_CAPABILITY]),
    ),
    case(
        REGISTER_MACHINE_CAPABILITY,
        "no join approval",
        "a Register of a new Machine with no join approval",
        Expected::Refused(&[REGISTER_MACHINE_CAPABILITY, JOIN_APPROVAL_CAPABILITY]),
    ),
    case(
        SET_MANAGEMENT_CLIENT_CAPABILITY,
        "from Cloud's own connection",
        "a SetManagementClient of Cloud's own slot, over Cloud's connection",
        Expected::Bounded("Device slots are untouched"),
    ),
    case(
        SET_MANAGEMENT_CLIENT_CAPABILITY,
        "from the mesh, or naming a Device slot",
        "a SetManagementClient naming a Device slot, from the mesh",
        Expected::Refused(&[SET_MANAGEMENT_CLIENT_CAPABILITY]),
    ),
    case(
        APPROVE_JOIN_CAPABILITY,
        "no Device signature",
        "an ApproveJoin whose approval a key this Machine does not trust signed",
        Expected::Refused(&[APPROVE_JOIN_CAPABILITY, JOIN_APPROVAL_CAPABILITY]),
    ),
    case(
        SET_DEVICE_CAPABILITY,
        "from the mesh",
        "a SetDevice adding a key, from Cloud",
        Expected::Refused(&[SET_DEVICE_CAPABILITY]),
    ),
    case(
        SET_PUSH_TARGET_CAPABILITY,
        "any target",
        "a SetPushTarget naming a URL",
        Expected::Served,
    ),
    case(
        SET_DEVICE_ENFORCEMENT_CAPABILITY,
        "from the mesh",
        "a SetDeviceEnforcement turning enforcement off, from Cloud",
        Expected::Refused(&[SET_DEVICE_ENFORCEMENT_CAPABILITY]),
    ),
    case(
        SET_PASSKEY_CAPABILITY,
        "from the mesh",
        "a SetPasskey adding a passkey, from Cloud",
        Expected::Refused(&[SET_PASSKEY_CAPABILITY]),
    ),
    case(
        REMOVE_TRUSTED_KEY_CAPABILITY,
        "from the mesh",
        "a RemoveTrustedKey of the approved Device, from Cloud",
        Expected::Refused(&[REMOVE_TRUSTED_KEY_CAPABILITY]),
    ),
    case(
        SELECT_ENDPOINT_CAPABILITY,
        "any endpoint",
        "a SelectEndpoint choosing a peer's endpoint",
        Expected::Served,
    ),
    case(
        UPDATE_MACHINE_CAPABILITY,
        "any update",
        "an UpdateMachine renaming the Machine",
        Expected::Served,
    ),
    case(
        REQUEST_MACHINE_UPGRADE_CAPABILITY,
        "a release channel",
        "a RequestMachineUpgrade to stable",
        Expected::Served,
    ),
    case(
        REQUEST_MACHINE_UPGRADE_CAPABILITY,
        "an exact version no channel resolves to",
        "a RequestMachineUpgrade to an exact version neither channel resolves to",
        Expected::Refused(&[UPGRADE_EXACT_CAPABILITY]),
    ),
    case(
        REQUEST_MACHINE_UPGRADE_CAPABILITY,
        "a downgrade",
        "a RequestMachineUpgrade to a release older than the installed one, which no channel resolves to",
        Expected::Refused(&[UPGRADE_EXACT_CAPABILITY]),
    ),
    case(
        GENERATE_SEALING_KEY_CAPABILITY,
        "the Cluster publishes no key",
        "a GenerateSealingKey on a Machine whose Cluster publishes no Sealing Key",
        Expected::Bounded("the key stays on the Machine"),
    ),
    case(
        GENERATE_SEALING_KEY_CAPABILITY,
        "the Cluster already publishes one",
        "a GenerateSealingKey on a Machine holding no key whose Cluster publishes one",
        Expected::Refused(&[GENERATE_SEALING_KEY_CAPABILITY]),
    ),
    case(
        SHARE_SEALING_KEY_CAPABILITY,
        "no join approval",
        "a ShareSealingKey to a Machine no join approval names",
        Expected::Refused(&[SHARE_SEALING_KEY_CAPABILITY, JOIN_APPROVAL_CAPABILITY]),
    ),
    case(
        INSTALL_SEALING_KEY_CAPABILITY,
        "sealed by an accepted Machine's key",
        "an InstallSealingKey sealed to this Machine by an accepted Machine",
        Expected::Bounded("the key installed is the one the Cluster publishes"),
    ),
    case(
        INSTALL_SEALING_KEY_CAPABILITY,
        "sealed by any other key",
        "an InstallSealingKey sealed by a key no accepted Machine holds",
        Expected::Refused(&[INSTALL_SEALING_KEY_CAPABILITY]),
    ),
    case(
        LEAVE_MACHINE_CAPABILITY,
        "no statement",
        "a Leave from Cloud",
        Expected::Refused(&[LEAVE_MACHINE_CAPABILITY]),
    ),
    case(
        FORGET_MACHINE_CAPABILITY,
        "no statement",
        "a Forget of another Machine, from Cloud",
        Expected::Refused(&[FORGET_MACHINE_CAPABILITY]),
    ),
    case(
        ENSURE_CONTAINER_CAPABILITY,
        "create",
        "an EnsureContainer creating a Container from a new spec",
        Expected::Refused(&[DEPLOY_APPROVAL_CAPABILITY]),
    ),
    case(
        ENSURE_CONTAINER_CAPABILITY,
        "start",
        "an EnsureContainer starting a stopped Container",
        Expected::Refused(&[RESTART_APPROVAL_CAPABILITY]),
    ),
    case(
        ENSURE_CONTAINER_CAPABILITY,
        "stop",
        "an EnsureContainer stopping a running Container",
        Expected::Refused(&[RESTART_APPROVAL_CAPABILITY]),
    ),
    case(
        ENSURE_CONTAINER_CAPABILITY,
        "held",
        "an EnsureContainer holding a routed Container out of routing for a run",
        Expected::Refused(&[RESTART_APPROVAL_CAPABILITY]),
    ),
    case(
        ENSURE_CONTAINER_CAPABILITY,
        "out",
        "an EnsureContainer setting a routed Container Out",
        Expected::Refused(&[RESTART_APPROVAL_CAPABILITY, DEPLOY_APPROVAL_CAPABILITY]),
    ),
    case(
        ENSURE_CONTAINER_CAPABILITY,
        "remove",
        "an EnsureContainer removing a Container",
        Expected::Refused(&[RESTART_APPROVAL_CAPABILITY, DEPLOY_APPROVAL_CAPABILITY]),
    ),
    case(
        UPDATE_CONTAINER_LIMITS_CAPABILITY,
        "any limit",
        "an UpdateContainerLimits lowering a Container's memory",
        Expected::Served,
    ),
    case(
        PREPARE_VOLUMES_CAPABILITY,
        "any Volume",
        "a PrepareVolumes of a new Volume",
        Expected::Bounded(STARTS_NOTHING),
    ),
    case(
        REMOVE_VOLUME_CAPABILITY,
        "any Volume",
        "a RemoveVolume of a Docker Volume",
        Expected::Served,
    ),
    case(
        SEED_VOLUME_CAPABILITY,
        "no approval",
        "a SeedVolume from another Machine's copy",
        Expected::Refused(&[SEED_VOLUME_CAPABILITY]),
    ),
    case(
        SET_VOLUME_COPY_CAPABILITY,
        "a Writer want with a start",
        "a SetVolumeCopy making this copy Writer and creating its Container",
        Expected::Refused(&[SET_VOLUME_COPY_CAPABILITY]),
    ),
    case(
        SET_VOLUME_COPY_CAPABILITY,
        "a pull",
        "a SetVolumeCopy pulling the Volume from another Machine, with no approval",
        Expected::Refused(&[VOLUME_APPROVAL_CAPABILITY]),
    ),
    case(
        SET_VOLUME_COPY_CAPABILITY,
        "cut, promote, forget",
        "a SetVolumeCopy forgetting a copy",
        Expected::Served,
    ),
    case(
        SET_VOLUME_COPY_CAPABILITY,
        "floor, snapshot",
        "a SetVolumeCopy raising a copy's floor",
        Expected::Served,
    ),
    case(
        SET_VOLUME_SIZE_CAPABILITY,
        "grow, or a shrink",
        "a SetVolumeSize growing a Volume",
        Expected::Served,
    ),
    case(
        PRUNE_IMAGES_CAPABILITY,
        "any prune",
        "a PruneImages of unreferenced images",
        Expected::Served,
    ),
    case(
        ENSURE_IMAGE_INGEST_CAPABILITY,
        "any image",
        "an EnsureImageIngest opening an image push",
        Expected::Bounded(STARTS_NOTHING),
    ),
    case(
        PULL_IMAGE_CAPABILITY,
        "any image",
        "a PullImage of a public image",
        Expected::Bounded(STARTS_NOTHING),
    ),
    case(
        PULL_IMAGE_FROM_MACHINE_CAPABILITY,
        "any image",
        "a PullImageFromMachine of another Machine's image",
        Expected::Bounded(STARTS_NOTHING),
    ),
    case(
        MINT_BUILD_GRANT_CAPABILITY,
        "with a rule and its proof",
        "a MintBuildGrant carrying an automatic deploy rule and GitHub's proof",
        Expected::Served,
    ),
    case(
        MINT_BUILD_GRANT_CAPABILITY,
        "without",
        "a MintBuildGrant with no rule",
        Expected::Refused(&[MINT_BUILD_GRANT_CAPABILITY]),
    ),
    case(
        PUBLISH_CERTIFICATE_MATERIAL_CAPABILITY,
        "any hostname",
        "a PublishCertificateMaterial for a hostname",
        Expected::Served,
    ),
    case(
        SET_INGRESS_DOORS_CAPABILITY,
        "any doors",
        "a SetIngressDoors placing the HTTP Doors",
        Expected::Refused(&[SET_INGRESS_DOORS_CAPABILITY]),
    ),
    case(
        SET_PRIVATE_ACCESS_CAPABILITY,
        "on",
        "a SetPrivateAccess turning Private Access on",
        Expected::Refused(&[SET_PRIVATE_ACCESS_CAPABILITY]),
    ),
    case(
        SET_PRIVATE_ACCESS_CAPABILITY,
        "off",
        "a SetPrivateAccess turning Private Access off",
        Expected::Refused(&[SET_PRIVATE_ACCESS_CAPABILITY]),
    ),
    case(
        SET_CLUSTER_NAME_CAPABILITY,
        "any name",
        "a SetClusterName renaming the Cluster",
        Expected::Served,
    ),
    case(
        EXEC_CONTAINER_CAPABILITY,
        "from the mesh",
        "an Exec into a Container, from Cloud",
        Expected::Refused(&[EXEC_CONTAINER_CAPABILITY]),
    ),
    case(
        BUILD_CAPABILITY,
        "no statement",
        "a Build from Cloud",
        Expected::Refused(&[BUILD_CAPABILITY]),
    ),
];
