//! One authorization class for every catalog route, and the cases that say what an
//! enforcing Machine does with each branch an Operator sends unsigned.
//!
//! The table is keyed by the catalog's generated capability constants. Tests in
//! `tests/capabilities.rs` hold it to the catalog, and a behaviour test in `ployzd` sends
//! every case to a real Machine API. `examples/coverage_cases.rs` prints [`CASES`] as JSON
//! for the live attacker harness, so a lane follows the table as later slices move rows.

use serde::Serialize;

use crate::{
    APPROVE_JOIN_CAPABILITY, BUILD_CAPABILITY, BUILD_FREE_CAPABILITY, BUILD_GRANT_RULE_CAPABILITY,
    CLOSE_PREVIEW_CAPABILITY, CONFIRM_JOIN_CAPABILITY, DEPLOY_RULE_CAPABILITY,
    DESCRIBE_CONTRACT_CAPABILITY, EMPTY_TRASH_CAPABILITY, ENROLL_FOR_RECOVERY_CAPABILITY,
    ENROLL_MACHINE_CAPABILITY, ENSURE_CONTAINER_CAPABILITY, ENSURE_IMAGE_INGEST_CAPABILITY,
    ENSURE_PINNED_CONTAINER_CAPABILITY, EXEC_CONTAINER_CAPABILITY, FORGET_MACHINE_CAPABILITY,
    FORGET_VERSIONS_CAPABILITY, GENERATE_SEALING_KEY_CAPABILITY, INSPECT_BUILD_GRANT_CAPABILITY,
    INSPECT_CONTAINER_CAPABILITY, INSPECT_MACHINE_CAPABILITY, INSPECT_STORAGE_CAPABILITY,
    INSPECT_VOLUME_COPY_CAPABILITY, INSTALL_SEALING_KEY_CAPABILITY, JOIN_APPROVAL_CAPABILITY,
    KEY_STEP_CAPABILITY, LEAVE_MACHINE_CAPABILITY, LIST_CONTAINERS_CAPABILITY,
    LIST_IMAGES_CAPABILITY, LIST_KEPT_VOLUMES_CAPABILITY, LIST_MACHINES_CAPABILITY,
    LIST_PREVIEWS_CAPABILITY, LIST_TRASH_CAPABILITY, LIST_VOLUMES_CAPABILITY,
    METRICS_LIVE_CAPABILITY, MINT_BUILD_GRANT_CAPABILITY, PENDING_JOINS_CAPABILITY,
    PREPARE_VOLUMES_CAPABILITY, PREVIEW_HOME_CAPABILITY, PRIVATE_ACCESS_APPROVAL_CAPABILITY,
    PRUNE_IMAGES_CAPABILITY, PUBLISH_CERTIFICATE_MATERIAL_CAPABILITY, PULL_FREE_IMAGE_CAPABILITY,
    PULL_IMAGE_CAPABILITY, PULL_IMAGE_FROM_MACHINE_CAPABILITY, READ_CLUSTER_NAME_CAPABILITY,
    READ_ROUTED_ADDRESSES_CAPABILITY, READ_ROUTES_CAPABILITY, RECOVER_SEALING_KEY_CAPABILITY,
    REGISTER_MACHINE_CAPABILITY, REMOVE_KEPT_VOLUMES_CAPABILITY, REMOVE_VOLUME_CAPABILITY,
    REQUEST_MACHINE_UPGRADE_CAPABILITY, RESTORE_TRASH_CAPABILITY, RUNTIME_WATCH_CAPABILITY,
    SEED_COPY_CAPABILITY, SEED_VOLUME_CAPABILITY, SELECT_ENDPOINT_CAPABILITY,
    SET_CLUSTER_DOORS_CAPABILITY, SET_CLUSTER_NAME_CAPABILITY, SET_DEVICE_CAPABILITY,
    SET_INGRESS_DOORS_CAPABILITY, SET_KEY_CAPABILITY, SET_MANAGEMENT_CLIENT_CAPABILITY,
    SET_PRIVATE_ACCESS_CAPABILITY, SET_PUSH_TARGET_CAPABILITY, SET_VOLUME_COPY_CAPABILITY,
    SET_VOLUME_SIZE_CAPABILITY, SHARE_SEALING_KEY_CAPABILITY, SIGNED_VERSION_CAPABILITY,
    SIGNED_VERSION_PROPOSAL_CAPABILITY, TAIL_LOGS_CAPABILITY, TRASH_APPROVAL_CAPABILITY,
    UPDATE_CONTAINER_LIMITS_CAPABILITY, UPDATE_CONTAINER_PLACEMENT_CAPABILITY,
    UPDATE_MACHINE_CAPABILITY, UPGRADE_EXACT_CAPABILITY, WRAP_SEALING_KEY_CAPABILITY,
};

/// A statement a trusted key signs that admits an Operator's call.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Statement {
    Join,
    Key,
    Deploy,
    Proposal,
    Rule,
    PrivateAccess,
    Copy,
    Trash,
}

impl Statement {
    /// The capabilities a refusal names when this statement is the one missing. Empty for a
    /// statement whose refusals name the route itself.
    #[must_use]
    pub const fn capabilities(self) -> &'static [&'static str] {
        match self {
            Self::Join => &[JOIN_APPROVAL_CAPABILITY],
            Self::Key => &[KEY_STEP_CAPABILITY],
            Self::Deploy => &[SIGNED_VERSION_CAPABILITY],
            Self::Proposal => &[SIGNED_VERSION_PROPOSAL_CAPABILITY],
            Self::Rule => &[DEPLOY_RULE_CAPABILITY, BUILD_GRANT_RULE_CAPABILITY],
            Self::PrivateAccess => &[PRIVATE_ACCESS_APPROVAL_CAPABILITY],
            Self::Copy => &[SEED_COPY_CAPABILITY],
            Self::Trash => &[TRASH_APPROVAL_CAPABILITY],
        }
    }
}

/// What an Operator's call needs on a Machine that enforces Device approval.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "class", content = "detail")]
pub enum Authorization {
    /// Changes nothing on the Machine. Served.
    Read,
    /// Catalog scope Full: refused at dispatch for any caller but the local socket or the
    /// own connection of a Device no CI or agent grant holds.
    Full,
    /// Refused without one of these statements, naming the route's capability or a
    /// statement's. Empty means only a Full caller is served.
    Signed(&'static [Statement]),
    /// Served unsigned. The reason says why the worst Cloud can do with it is mislabel,
    /// reorder, delay or drop, or an effect that crosses no part of the bar.
    Bound(&'static str),
}

use Authorization::{Bound, Full, Read, Signed};
use Statement::{Deploy, Join, Key, PrivateAccess, Proposal, Rule};

const IMAGE_BOUND: &str = "a signed creation names its image by digest, and an unsigned one pulls its own with no login; checked at creation by C";
const PULL_BOUND: &str = "writes no name but a retention name its content certifies, so a Full creation's tag keeps naming what it named; a signed creation names its image by digest, and an unsigned one pulls its own with no login; checked at creation by C";

/// Every catalog route's class, in catalog order.
pub const AUTHORIZATION: &[(&str, Authorization)] = &[
    (DESCRIBE_CONTRACT_CAPABILITY, Read),
    (INSPECT_MACHINE_CAPABILITY, Read),
    (ENROLL_MACHINE_CAPABILITY, Signed(&[Join])),
    (
        ENROLL_FOR_RECOVERY_CAPABILITY,
        Bound(
            "founds a Cluster on an Uninitialized Machine, which trusts no key until a person installs one over local administration; its Sealing Key comes only through RecoverSealingKey, a Full route",
        ),
    ),
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
    (SET_KEY_CAPABILITY, Full),
    (
        CONFIRM_JOIN_CAPABILITY,
        Bound(
            "a joiner takes only restrictions its admitting Machine MAC'd under the Sealing Key for this join, and they only narrow what it trusts",
        ),
    ),
    (
        SET_PUSH_TARGET_CAPABILITY,
        Bound("crash events are data Cloud already reads"),
    ),
    (KEY_STEP_CAPABILITY, Signed(&[Key])),
    (FORGET_VERSIONS_CAPABILITY, Signed(&[Deploy, Proposal])),
    (SELECT_ENDPOINT_CAPABILITY, Bound("residual: delay or drop")),
    (LIST_MACHINES_CAPABILITY, Read),
    (
        UPDATE_MACHINE_CAPABILITY,
        Bound("residual: mislabel; approvals bind IDs"),
    ),
    (
        REQUEST_MACHINE_UPGRADE_CAPABILITY,
        Bound("installs only a release Ployz signed, and never one older than the installed"),
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
    (
        LEAVE_MACHINE_CAPABILITY,
        Bound("residual: a drop; a removed Machine keeps its Volume data on disk"),
    ),
    (
        FORGET_MACHINE_CAPABILITY,
        Bound("residual: a drop; coming back is a join, which a person signs"),
    ),
    (LIST_CONTAINERS_CAPABILITY, Read),
    (INSPECT_CONTAINER_CAPABILITY, Read),
    (
        ENSURE_CONTAINER_CAPABILITY,
        Signed(&[Deploy, Proposal, Rule]),
    ),
    (
        ENSURE_PINNED_CONTAINER_CAPABILITY,
        Signed(&[Deploy, Proposal, Rule]),
    ),
    (
        UPDATE_CONTAINER_LIMITS_CAPABILITY,
        Bound("residual: a limit can slow or stop a Service, a drop"),
    ),
    (
        UPDATE_CONTAINER_PLACEMENT_CAPABILITY,
        Bound(
            "residual: rewrites a recorded placement to any this Machine satisfies, or clears it, so a later approved Drain may place outside the region; nothing starts or stops at the call",
        ),
    ),
    (INSPECT_STORAGE_CAPABILITY, Read),
    (PREPARE_VOLUMES_CAPABILITY, Bound("checked at mount by C")),
    (LIST_VOLUMES_CAPABILITY, Read),
    (
        REMOVE_VOLUME_CAPABILITY,
        Bound("residual: a drop; the data waits 7 days in the Server's Trash"),
    ),
    (
        CLOSE_PREVIEW_CAPABILITY,
        Bound(
            "residual: a preview's own data destroyed early; other data waits 7 days in the Trash",
        ),
    ),
    (LIST_PREVIEWS_CAPABILITY, Read),
    (LIST_KEPT_VOLUMES_CAPABILITY, Read),
    (
        REMOVE_KEPT_VOLUMES_CAPABILITY,
        Bound("residual: a drop of data no Container mounts; the data leaves no Machine"),
    ),
    (INSPECT_VOLUME_COPY_CAPABILITY, Read),
    (SEED_VOLUME_CAPABILITY, Full),
    (SEED_COPY_CAPABILITY, Signed(&[Statement::Copy])),
    (
        SET_VOLUME_COPY_CAPABILITY,
        Bound("copies only between this Cluster's Machines; checked at mount by C"),
    ),
    (
        SET_VOLUME_SIZE_CAPABILITY,
        Bound("residual: a bound can refuse writes, a drop"),
    ),
    (LIST_TRASH_CAPABILITY, Read),
    (RESTORE_TRASH_CAPABILITY, Signed(&[Statement::Trash])),
    (EMPTY_TRASH_CAPABILITY, Signed(&[Statement::Trash])),
    (LIST_IMAGES_CAPABILITY, Read),
    (
        PRUNE_IMAGES_CAPABILITY,
        Bound("residual: the next deploy pulls again"),
    ),
    (ENSURE_IMAGE_INGEST_CAPABILITY, Bound(IMAGE_BOUND)),
    (PULL_IMAGE_CAPABILITY, Bound(PULL_BOUND)),
    (
        PULL_FREE_IMAGE_CAPABILITY,
        Bound(
            "for an Operator, writes no name and records an image ID only an unsigned creation reads; for a Full caller, pulls the tag like pull.v1",
        ),
    ),
    (PULL_IMAGE_FROM_MACHINE_CAPABILITY, Bound(PULL_BOUND)),
    (MINT_BUILD_GRANT_CAPABILITY, Signed(&[Rule])),
    (INSPECT_BUILD_GRANT_CAPABILITY, Read),
    (
        PUBLISH_CERTIFICATE_MATERIAL_CAPABILITY,
        Bound("DNS and ACME authority Cloud holds; thread X"),
    ),
    (READ_ROUTES_CAPABILITY, Read),
    (READ_ROUTED_ADDRESSES_CAPABILITY, Read),
    (
        SET_INGRESS_DOORS_CAPABILITY,
        Bound(
            "the request names no image; the daemon places only its pinned images; a tunnel token Cloud seals, remotely managed or not, runs a connector that reaches only DNS, Cloudflare's edge and the Tunnel Origin, which serves only public routes",
        ),
    ),
    (SET_PRIVATE_ACCESS_CAPABILITY, Signed(&[PrivateAccess])),
    (READ_CLUSTER_NAME_CAPABILITY, Read),
    (SET_CLUSTER_NAME_CAPABILITY, Bound("residual: mislabel")),
    (
        SET_CLUSTER_DOORS_CAPABILITY,
        Bound("residual: steers later joins only to Doors SetIngressDoors already opens"),
    ),
    (METRICS_LIVE_CAPABILITY, Read),
    (TAIL_LOGS_CAPABILITY, Read),
    (RUNTIME_WATCH_CAPABILITY, Read),
    (EXEC_CONTAINER_CAPABILITY, Full),
    (BUILD_CAPABILITY, Signed(&[])),
    (
        BUILD_FREE_CAPABILITY,
        Bound(
            "opens no Sealed value, takes no registry login or other Service's image, reaches no image server, and loads its image under no name; its receipt admits only an unsigned creation of that content",
        ),
    ),
];

/// The class of the route named by `capability`, or `None` for a name that is not a route.
#[must_use]
pub fn authorization(capability: &str) -> Option<Authorization> {
    AUTHORIZATION
        .iter()
        .find(|(name, _)| *name == capability)
        .map(|(_, class)| *class)
}

/// Whether an Operator caller is never served `capability`, so a client holding a Full
/// path sends it there itself: a call forwarded over the mesh arrives as an Operator's.
#[must_use]
pub fn needs_full_caller(capability: &str) -> bool {
    matches!(authorization(capability), Some(Full | Signed(&[])))
}

/// Whether an enforcing Machine refuses an Operator's `capability` call outright: no
/// statement a person signs admits it, so only a Full caller is served.
#[must_use]
pub fn refuses_operator(capability: &str) -> bool {
    matches!(authorization(capability), Some(Signed(&[])))
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

const OPENS_NOTHING: &str =
    "the upload is refused as the Sealed record arrives, and nothing opens or runs";
const LOADS_NO_NAME: &str =
    "the built image carries no tag, and only a creation with its receipt runs it unsigned";
const MOUNTS_NOTHING: &str =
    "a following unsigned EnsureContainer create that mounts it is refused";
const PULLS_AGAIN: &str =
    "a following unsigned EnsureContainer create that uses it pulls the image again itself";
const PULLS_ITS_OWN: &str = "the Machine pulls the image itself";
const WRITES_NO_NAME: &str = "the Machine pulls the image by digest and writes no name";
const PINNED_DOORS: &str = "only pinned images run";

/// Every case an enforcing Machine is held to, in catalog order.
pub const CASES: &[Case] = &[
    case(
        ENROLL_MACHINE_CAPABILITY,
        "member, no capsule",
        "an Enroll as a member whose registration carries no approved capsule",
        Expected::Refused(&[JOIN_APPROVAL_CAPABILITY]),
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
        "an Enroll as a founder, which a Participating Machine refuses as a conflict",
        Expected::Served,
    ),
    case(
        ENROLL_FOR_RECOVERY_CAPABILITY,
        "founder",
        "an EnrollForRecovery founding a Cluster that awaits recovery, which a Participating Machine refuses as a conflict",
        Expected::Served,
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
        SET_KEY_CAPABILITY,
        "from the mesh",
        "a SetKey trusting a key, from Cloud",
        Expected::Refused(&[SET_KEY_CAPABILITY]),
    ),
    case(
        CONFIRM_JOIN_CAPABILITY,
        "from Cloud",
        "a ConfirmJoin carrying restrictions Cloud wrote, from Cloud",
        Expected::Bounded("restrictions Cloud wrote change nothing the Machine trusts"),
    ),
    case(
        SET_PUSH_TARGET_CAPABILITY,
        "any target",
        "a SetPushTarget naming a URL",
        Expected::Served,
    ),
    case(
        KEY_STEP_CAPABILITY,
        "no Device signature",
        "a KeyStep removing a key, signed by a key this Machine does not trust",
        Expected::Refused(&[KEY_STEP_CAPABILITY]),
    ),
    case(
        FORGET_VERSIONS_CAPABILITY,
        "no Device signature",
        "a ForgetVersions whose version a key this Machine does not trust signed",
        Expected::Refused(&[FORGET_VERSIONS_CAPABILITY]),
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
        Expected::Refused(&[REQUEST_MACHINE_UPGRADE_CAPABILITY, UPGRADE_EXACT_CAPABILITY]),
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
        "no join approval, to a Member",
        "a ShareSealingKey to a Member this Machine trusts, sealed to the WireGuard key it trusts it under",
        Expected::Served,
    ),
    case(
        SHARE_SEALING_KEY_CAPABILITY,
        "no join approval, to another Machine",
        "a ShareSealingKey to a listed Machine that is no Member here",
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
        Expected::Served,
    ),
    case(
        FORGET_MACHINE_CAPABILITY,
        "no statement",
        "a Forget of another Machine, from Cloud",
        Expected::Served,
    ),
    case(
        ENSURE_CONTAINER_CAPABILITY,
        "create, reaches data",
        "an EnsureContainer creating a Container from a new spec that mounts a Volume",
        Expected::Refused(&[SIGNED_VERSION_CAPABILITY]),
    ),
    case(
        ENSURE_CONTAINER_CAPABILITY,
        "create, reaches no data",
        "an EnsureContainer creating a Container from a new spec that opens no secret, mounts no Volume and reaches no host, under a name that never ran signed",
        Expected::Bounded(PULLS_ITS_OWN),
    ),
    case(
        ENSURE_CONTAINER_CAPABILITY,
        "start",
        "an EnsureContainer starting a stopped Container",
        Expected::Served,
    ),
    case(
        ENSURE_CONTAINER_CAPABILITY,
        "stop",
        "an EnsureContainer stopping a running Container",
        Expected::Served,
    ),
    case(
        ENSURE_CONTAINER_CAPABILITY,
        "held",
        "an EnsureContainer holding a routed Container out of routing for a run",
        Expected::Served,
    ),
    case(
        ENSURE_CONTAINER_CAPABILITY,
        "out",
        "an EnsureContainer setting a routed Container Out",
        Expected::Served,
    ),
    case(
        ENSURE_CONTAINER_CAPABILITY,
        "remove",
        "an EnsureContainer removing a Container",
        Expected::Served,
    ),
    case(
        ENSURE_PINNED_CONTAINER_CAPABILITY,
        "create, reaches data",
        "an EnsurePinnedContainer creating a Container from a new tag spec that mounts a Volume",
        Expected::Refused(&[SIGNED_VERSION_CAPABILITY]),
    ),
    case(
        ENSURE_PINNED_CONTAINER_CAPABILITY,
        "create, reaches no data",
        "an EnsurePinnedContainer creating a Container from a new tag spec that opens no secret, mounts no Volume and reaches no host, under a name that never ran signed, pinned to an image ID",
        Expected::Bounded(PULLS_ITS_OWN),
    ),
    case(
        ENSURE_PINNED_CONTAINER_CAPABILITY,
        "digest spec",
        "an EnsurePinnedContainer pinning a spec whose image already names a digest",
        Expected::Refused(&[ENSURE_PINNED_CONTAINER_CAPABILITY]),
    ),
    case(
        ENSURE_PINNED_CONTAINER_CAPABILITY,
        "no pin",
        "an EnsurePinnedContainer creating a Container with no image ID",
        Expected::Refused(&[ENSURE_PINNED_CONTAINER_CAPABILITY]),
    ),
    case(
        UPDATE_CONTAINER_LIMITS_CAPABILITY,
        "any limit",
        "an UpdateContainerLimits lowering a Container's memory",
        Expected::Served,
    ),
    case(
        UPDATE_CONTAINER_PLACEMENT_CAPABILITY,
        "any placement",
        "an UpdateContainerPlacement on a running Container",
        Expected::Served,
    ),
    case(
        PREPARE_VOLUMES_CAPABILITY,
        "any Volume",
        "a PrepareVolumes of a new Volume",
        Expected::Bounded(MOUNTS_NOTHING),
    ),
    case(
        REMOVE_VOLUME_CAPABILITY,
        "any Volume",
        "a RemoveVolume of a Docker Volume",
        Expected::Served,
    ),
    case(
        REMOVE_KEPT_VOLUMES_CAPABILITY,
        "any Kept Volume",
        "a RemoveKeptVolumes of an anonymous Volume this Machine kept",
        Expected::Served,
    ),
    case(
        SEED_VOLUME_CAPABILITY,
        "any copy",
        "a SeedVolume from another Machine's copy",
        Expected::Refused(&[SEED_VOLUME_CAPABILITY]),
    ),
    case(
        SEED_COPY_CAPABILITY,
        "no Copy step signature",
        "a SeedCopy whose Copy step rides in an unsigned bundle",
        Expected::Refused(&[SEED_COPY_CAPABILITY]),
    ),
    case(
        SET_VOLUME_COPY_CAPABILITY,
        "a Writer want with a start",
        "a SetVolumeCopy making this copy Writer and creating its Container",
        Expected::Refused(&[SIGNED_VERSION_CAPABILITY]),
    ),
    case(
        SET_VOLUME_COPY_CAPABILITY,
        "a pull",
        "a SetVolumeCopy pulling the Volume from another Machine",
        Expected::Served,
    ),
    case(
        SET_VOLUME_COPY_CAPABILITY,
        "a cut",
        "a SetVolumeCopy cutting the Writer for a Move",
        Expected::Served,
    ),
    case(
        SET_VOLUME_COPY_CAPABILITY,
        "a promote",
        "a SetVolumeCopy making a Mirror the Writer",
        Expected::Served,
    ),
    case(
        SET_VOLUME_COPY_CAPABILITY,
        "a forget of a copy",
        "a SetVolumeCopy deleting a slot that holds a snapshot",
        Expected::Served,
    ),
    case(
        SET_VOLUME_COPY_CAPABILITY,
        "a forget of nothing",
        "a SetVolumeCopy forgetting where this Machine holds no copy",
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
        RESTORE_TRASH_CAPABILITY,
        "no approval",
        "a RestoreTrash of a deleted Volume with no person's approval, from Cloud",
        Expected::Refused(&[TRASH_APPROVAL_CAPABILITY]),
    ),
    case(
        EMPTY_TRASH_CAPABILITY,
        "no approval",
        "an EmptyTrash of a deleted Volume with no person's approval, from Cloud",
        Expected::Refused(&[TRASH_APPROVAL_CAPABILITY]),
    ),
    case(
        CLOSE_PREVIEW_CAPABILITY,
        "a preview this Server never ran",
        "a ClosePreview of a preview Namespace",
        Expected::Refused(&[PREVIEW_HOME_CAPABILITY]),
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
        Expected::Bounded(PULLS_AGAIN),
    ),
    case(
        PULL_IMAGE_CAPABILITY,
        "tag",
        "a PullImage of a public image by tag",
        Expected::Refused(&[PULL_IMAGE_CAPABILITY]),
    ),
    case(
        PULL_IMAGE_CAPABILITY,
        "digest",
        "a PullImage of a public image by digest",
        Expected::Bounded(PULLS_AGAIN),
    ),
    case(
        PULL_FREE_IMAGE_CAPABILITY,
        "any image",
        "a PullFreeImage of a public image by tag",
        Expected::Bounded(WRITES_NO_NAME),
    ),
    case(
        PULL_IMAGE_FROM_MACHINE_CAPABILITY,
        "tag",
        "a PullImageFromMachine of another Machine's image by tag",
        Expected::Refused(&[PULL_IMAGE_FROM_MACHINE_CAPABILITY]),
    ),
    case(
        PULL_IMAGE_FROM_MACHINE_CAPABILITY,
        "digest",
        "a PullImageFromMachine of another Machine's image by digest",
        Expected::Bounded(PULLS_AGAIN),
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
        "a SetIngressDoors placing the HTTP Doors, naming an image of its own",
        Expected::Bounded(PINNED_DOORS),
    ),
    case(
        SET_INGRESS_DOORS_CAPABILITY,
        "tunnel door",
        "a SetIngressDoors opening the Tunnel Door on a token for a remotely managed tunnel whose routes name the host, the mesh and a Container",
        Expected::Bounded(
            "the connector reaches only DNS, Cloudflare's edge and the Tunnel Origin",
        ),
    ),
    case(
        SET_PRIVATE_ACCESS_CAPABILITY,
        "on",
        "a SetPrivateAccess turning Private Access on",
        Expected::Refused(&[PRIVATE_ACCESS_APPROVAL_CAPABILITY]),
    ),
    case(
        SET_PRIVATE_ACCESS_CAPABILITY,
        "off",
        "a SetPrivateAccess turning Private Access off",
        Expected::Served,
    ),
    case(
        SET_CLUSTER_NAME_CAPABILITY,
        "any name",
        "a SetClusterName renaming the Cluster",
        Expected::Served,
    ),
    case(
        SET_CLUSTER_DOORS_CAPABILITY,
        "any doors",
        "a SetClusterDoors declaring the Cluster's Doors",
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
    case(
        BUILD_FREE_CAPABILITY,
        "sealed values",
        "a free Build from Cloud whose capture carries a Sealed record",
        Expected::Bounded(OPENS_NOTHING),
    ),
    case(
        BUILD_FREE_CAPABILITY,
        "no secret",
        "a free Build from Cloud of a public recipe",
        Expected::Bounded(LOADS_NO_NAME),
    ),
];
