//! The bytes of every gate refusal `local_failure` writes: the code, the message and the
//! details a CLI, Cloud and an older daemon read, one literal per arm.

use ployz_core::authority::{AuthorityRefusal, Purpose};
use ployz_core::{MachineId, rpc::status_details};
use serde_json::json;

use super::local_failure;
use crate::machine::LocalMachineError as Error;

fn unsigned() -> AuthorityRefusal {
    AuthorityRefusal::Unsigned {
        purpose: Purpose::Workflow,
    }
}

fn written(error: Error) -> (tonic::Code, String, serde_json::Value) {
    let status = local_failure(error).expect_err("a gate refusal fails the call as a status");
    let details = status_details(status.details()).expect("a gate refusal carries details");
    (status.code(), status.message().to_owned(), details)
}

#[test]
fn each_gate_arm_writes_its_capability_and_hints_as_before() {
    let machine = MachineId::random();
    let consumed = format!("the join of Machine {machine} has already been admitted");
    let cases: Vec<(&str, Error, tonic::Code, &str, serde_json::Value)> = vec![
        (
            "restart without a signature",
            Error::RestartRefused(unsigned()),
            tonic::Code::PermissionDenied,
            "ployz.restart.device-approval.v2 needs a Device-signed restart approval",
            json!({
                "capability": ployz_core::RESTART_APPROVAL_CAPABILITY,
                "next": "ployz service restart from an approved device",
                "reason": "unsigned",
            }),
        ),
        (
            "restart of a replayed approval",
            Error::RestartRefused(AuthorityRefusal::Expired),
            tonic::Code::PermissionDenied,
            "ployz.restart.device-approval.v2 needs a Device-signed restart approval",
            json!({
                "capability": ployz_core::RESTART_APPROVAL_CAPABILITY,
                "next": "ployz service restart from an approved device",
            }),
        ),
        (
            "host-reaching restart",
            Error::RestartReachesHost,
            tonic::Code::PermissionDenied,
            "This service reaches the server itself, so only an approved device can restart or start it. Run ployz service restart on that device.",
            json!({
                "capability": ployz_core::RESTART_APPROVAL_CAPABILITY,
                "next": "ployz service restart from an approved device",
            }),
        ),
        (
            "deploy",
            Error::DeployRefused(unsigned()),
            tonic::Code::PermissionDenied,
            "This server runs only deploys an approved device signed.",
            json!({
                "capability": ployz_core::DEPLOY_APPROVAL_CAPABILITY,
                "next": "ployz deploy from an approved device",
            }),
        ),
        (
            "teardown",
            Error::TeardownRefused(unsigned()),
            tonic::Code::PermissionDenied,
            "This server takes this off only under your approval: the workflow approval carries no signature",
            json!({
                "capability": ployz_core::TEARDOWN_CAPABILITY,
                "next": "delete it again with your passkey in the dashboard, or from an approved device",
            }),
        ),
        (
            "upgrade channel",
            Error::UpgradeChannelRefused(unsigned()),
            tonic::Code::PermissionDenied,
            "ployz.machine.upgrade.channel.v1: no valid Release Channel approval for this Cluster: the workflow approval carries no signature",
            json!({
                "capability": ployz_core::UPGRADE_CHANNEL_CAPABILITY,
                "next": "ployz server upgrade-policy set --channel stable or beta from an approved device",
            }),
        ),
        (
            "forget",
            Error::DepartureRefused {
                capability: ployz_core::FORGET_MACHINE_CAPABILITY,
                refusal: unsigned(),
            },
            tonic::Code::PermissionDenied,
            "This server forgets another server only when you approve it, with ployz server forget from an approved device or your passkey in the dashboard: the workflow approval carries no signature",
            json!({
                "capability": ployz_core::FORGET_MACHINE_CAPABILITY,
                "next": "ployz server forget from an approved device, or Forget server with your passkey in the dashboard",
            }),
        ),
        (
            "leave",
            Error::DepartureRefused {
                capability: ployz_core::LEAVE_MACHINE_CAPABILITY,
                refusal: unsigned(),
            },
            tonic::Code::PermissionDenied,
            "This server leaves only when you approve it, with ployz server rm from an approved device or your passkey in the dashboard: the workflow approval carries no signature",
            json!({
                "capability": ployz_core::LEAVE_MACHINE_CAPABILITY,
                "next": "ployz server rm from an approved device, or Remove server with your passkey in the dashboard",
            }),
        ),
        (
            "private access",
            Error::PrivateAccessRefused(unsigned()),
            tonic::Code::PermissionDenied,
            "This Server joins a tailnet only with your approval: the workflow approval carries no signature",
            json!({
                "capability": ployz_core::PRIVATE_ACCESS_APPROVAL_CAPABILITY,
                "next": "ployz private-access on from an approved device, or Connect in the dashboard with your passkey",
            }),
        ),
        (
            "volume seed",
            Error::VolumeRefused {
                capability: ployz_core::VOLUME_APPROVAL_CAPABILITY,
            },
            tonic::Code::PermissionDenied,
            ployz_core::SEED_REFUSED_BY_ENFORCEMENT,
            json!({
                "capability": ployz_core::VOLUME_APPROVAL_CAPABILITY,
                "next": "approve the change from an approved device or with your passkey",
            }),
        ),
        (
            "volume step",
            Error::VolumeStepRefused(unsigned()),
            tonic::Code::PermissionDenied,
            "This Server refused the Volume step: the workflow approval carries no signature",
            json!({
                "capability": ployz_core::VOLUME_APPROVAL_CAPABILITY,
                "next": "approve the Volume change again from an approved device",
            }),
        ),
        (
            "unreceipted volume step",
            Error::VolumeStepRefused(AuthorityRefusal::Unreceipted),
            tonic::Code::PermissionDenied,
            "This Server refused the Volume step: the move's source has not handed this Volume here; this Machine becomes its writer only with the source's handoff receipt",
            json!({
                "capability": ployz_core::VOLUME_APPROVAL_CAPABILITY,
                "next": "ployz volume release asks the move's source where the move ends",
            }),
        ),
        (
            "device enforcement",
            Error::EnforcementRefused(unsigned()),
            tonic::Code::PermissionDenied,
            "ployz.machine.set-device-enforcement.v2 needs a Device-signed starting set: the workflow approval carries no signature",
            json!({
                "capability": ployz_core::SET_DEVICE_ENFORCEMENT_CAPABILITY,
                "next": "ployz device enforce from an approved device",
            }),
        ),
        (
            "consumed join",
            Error::JoinConsumed(machine),
            tonic::Code::PermissionDenied,
            &consumed,
            json!({
                "capability": ployz_core::JOIN_APPROVAL_CAPABILITY,
                "reason": "join_already_admitted",
                "inspect": ["ployz server ls"],
            }),
        ),
        (
            "join",
            Error::JoinRefused(unsigned()),
            tonic::Code::PermissionDenied,
            "ployz.machine.join.device-approval.v1 needs a Device-signed join approval: the workflow approval carries no signature",
            json!({
                "capability": ployz_core::JOIN_APPROVAL_CAPABILITY,
                "next": "ployz device approve --join CODE from an approved device",
            }),
        ),
        (
            "recipient",
            Error::RecipientRefused {
                capability: ployz_core::SET_RECIPIENT_KEY_CAPABILITY,
                refusal: unsigned(),
            },
            tonic::Code::PermissionDenied,
            "ployz.sealing.recipient.v1: the workflow approval carries no signature",
            json!({ "capability": ployz_core::SET_RECIPIENT_KEY_CAPABILITY }),
        ),
        (
            "awaiting activation",
            Error::AwaitingActivation {
                capability: ployz_core::DEPLOY_APPROVAL_CAPABILITY,
            },
            tonic::Code::PermissionDenied,
            "ployz.container.deploy-approval.v1: this Server awaits its Cluster's activation. Activate the Cluster, then try again",
            json!({
                "capability": ployz_core::DEPLOY_APPROVAL_CAPABILITY,
                "reason": "awaiting_activation",
                "next": "ployz device enforce",
            }),
        ),
        (
            "activating",
            Error::Activating,
            tonic::Code::Unavailable,
            "This Server is turning on Device enforcement and restarts in a few seconds. Try again then",
            json!({
                "capability": ployz_core::ENFORCED_CAPABILITY,
                "reason": "activating",
            }),
        ),
        (
            "build",
            Error::BuildRefused {
                capability: ployz_core::BUILD_CAPABILITY,
            },
            tonic::Code::PermissionDenied,
            ployz_core::BUILD_REFUSED_BY_ENFORCEMENT,
            json!({ "capability": ployz_core::BUILD_CAPABILITY }),
        ),
        (
            "build grant",
            Error::BuildRefused {
                capability: ployz_core::MINT_BUILD_GRANT_CAPABILITY,
            },
            tonic::Code::PermissionDenied,
            ployz_core::BUILD_REFUSED_BY_ENFORCEMENT,
            json!({ "capability": ployz_core::MINT_BUILD_GRANT_CAPABILITY }),
        ),
    ];
    for (name, error, code, message, details) in cases {
        let (got_code, got_message, got_details) = written(error);
        assert_eq!(got_code, code, "{name}: code");
        assert_eq!(got_message, message, "{name}: message");
        assert_eq!(got_details, details, "{name}: details");
    }
}

#[test]
fn a_rule_preview_or_activation_gate_names_what_a_person_runs_only_when_that_helps() {
    use crate::machine::local_machine::activation::Refusal;
    use crate::machine::local_machine::preview_deploy::PreviewRefusal;
    use crate::machine::local_machine::rule_deploy::RuleDeployRefusal;

    let legacy = MachineId::random();
    let cases = [
        (
            "rule deploy, a key this Machine does not trust",
            Error::RuleDeployRefused(RuleDeployRefusal::Rule(AuthorityRefusal::UnknownPasskey)),
            json!({
                "capability": ployz_core::DEPLOY_RULE_CAPABILITY,
                "next": "ployz github rule approve from an approved device",
            }),
        ),
        (
            "rule deploy, an approval that expired",
            Error::RuleDeployRefused(RuleDeployRefusal::Rule(AuthorityRefusal::Expired)),
            json!({ "capability": ployz_core::DEPLOY_RULE_CAPABILITY }),
        ),
        (
            "preview deploy, a key this Machine does not trust",
            Error::PreviewDeployRefused(PreviewRefusal::Rule(AuthorityRefusal::UnknownPasskey)),
            json!({
                "capability": ployz_core::DEPLOY_PREVIEW_CAPABILITY,
                "next": "ployz github preview approve from an approved device",
            }),
        ),
        (
            "preview deploy, an approval that expired",
            Error::PreviewDeployRefused(PreviewRefusal::Rule(AuthorityRefusal::Expired)),
            json!({ "capability": ployz_core::DEPLOY_PREVIEW_CAPABILITY }),
        ),
        (
            "preview grant",
            Error::PreviewGrantRefused(PreviewRefusal::Rule(AuthorityRefusal::Expired)),
            json!({ "capability": ployz_core::BUILD_GRANT_PREVIEW_CAPABILITY }),
        ),
        (
            "an activation with nothing to turn off",
            Error::ActivationRefused {
                capability: ployz_core::ACTIVATE_ENFORCEMENT_CAPABILITY,
                reason: Refusal::NoOff,
            },
            json!({
                "capability": ployz_core::ACTIVATE_ENFORCEMENT_CAPABILITY,
                "next": "ployz device enforce from an approved device, over SSH to each Server",
            }),
        ),
        (
            "an activation that names a legacy Server",
            Error::ActivationRefused {
                capability: ployz_core::ACTIVATE_ENFORCEMENT_CAPABILITY,
                reason: Refusal::LegacyMember(legacy),
            },
            json!({
                "capability": ployz_core::ACTIVATE_ENFORCEMENT_CAPABILITY,
                "next": "ployz device enforce from an approved device, over SSH to each Server",
                "legacy_member": legacy.to_string(),
            }),
        ),
    ];
    for (name, error, details) in cases {
        let message = error.to_string();
        let (code, got_message, got_details) = written(error);
        assert_eq!(code, tonic::Code::PermissionDenied, "{name}: code");
        assert_eq!(got_message, message, "{name}: message");
        assert_eq!(got_details, details, "{name}: details");
    }
}

#[test]
fn only_an_unsigned_restart_says_why() {
    let (_, _, details) = written(Error::RestartRefused(unsigned()));
    assert_eq!(details.get("reason"), Some(&json!("unsigned")), "{details}");
    for other in [AuthorityRefusal::Expired, AuthorityRefusal::UnknownPasskey] {
        let (_, _, details) = written(Error::RestartRefused(other));
        assert_eq!(details.get("reason"), None, "{details}");
    }
}
