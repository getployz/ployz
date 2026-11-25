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
            "deploy",
            Error::DeployRefused(unsigned()),
            tonic::Code::PermissionDenied,
            "This server runs only deploys an approved device signed.",
            json!({
                "capability": ployz_core::SIGNED_VERSION_CAPABILITY,
                "next": "ployz deploy from an approved device",
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
            "ployz.machine.join.device-approval.v1 needs a signed join approval: the workflow approval carries no signature",
            json!({
                "capability": ployz_core::JOIN_APPROVAL_CAPABILITY,
                "next": "ployz device approve --join CODE from an approved device",
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
fn a_rule_gate_names_what_a_person_runs_only_when_that_helps() {
    use crate::machine::local_machine::rule_deploy::RuleDeployRefusal;

    let cases = [
        (
            "rule deploy, a key this Machine does not trust",
            Error::RuleDeployRefused(RuleDeployRefusal::Rule(AuthorityRefusal::UnknownPasskey)),
            json!({
                "capability": ployz_core::DEPLOY_RULE_CAPABILITY,
                "next": "Approve again under Automatic deploys in Ployz Cloud",
            }),
        ),
        (
            "rule deploy, an approval that expired",
            Error::RuleDeployRefused(RuleDeployRefusal::Rule(AuthorityRefusal::Expired)),
            json!({ "capability": ployz_core::DEPLOY_RULE_CAPABILITY }),
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
