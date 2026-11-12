//! TypeScript declarations for `@ployz/sdk`, derived from the Rust wire types.
//!
//! The roots are the types the SDK façade names; every type they reference is
//! collected by walking `ts_rs` dependencies, so a type reaches the package by
//! being reachable, never by being listed.

use std::{
    any::TypeId,
    collections::{BTreeMap, BTreeSet},
};

use ts_rs::{Config, TS, TypeVisitor};

use super::RuntimeWatchView;
use ployz_core::{
    ClusterTeardown, ContractDescription, DataLossConfirmation, DeployEvent, DeployIntent,
    DeployOutcome, DeployPreview, ExecutionError, LocalMachineRemoved, MachineId, MachineTarget,
    Namespace, ObservedDataLoss, PlanOptions, RegisterRequest, Registered, RemoveVolumesRequest,
    RequestedServiceSpec, RpcError, VolumeRemoval,
};

const HEADER: &str =
    "// Generated from the Rust wire types by core/scripts/build-cloud-sdk.sh. Do not edit.\n\n";

struct Declarations {
    config: Config,
    seen: BTreeSet<TypeId>,
    by_name: BTreeMap<String, String>,
}

impl Declarations {
    fn add<T: TS + 'static + ?Sized>(&mut self) {
        if !self.seen.insert(TypeId::of::<T>()) {
            return;
        }
        // `output_path` is `Some` for types with their own declaration and `None`
        // for primitives and containers, which only contribute dependencies.
        if T::output_path().is_some() {
            let name = T::ident(&self.config);
            let declaration = T::decl(&self.config);
            if let Some(previous) = self.by_name.insert(name.clone(), declaration.clone()) {
                assert_eq!(
                    previous, declaration,
                    "two Rust types declare the TypeScript name {name}"
                );
            }
        }
        T::visit_dependencies(self);
    }
}

impl TypeVisitor for Declarations {
    fn visit<T: TS + 'static + ?Sized>(&mut self) {
        self.add::<T>();
    }
}

/// Every declaration the SDK package exports, in name order.
fn typescript_declarations() -> String {
    let mut declarations = Declarations {
        config: Config::new()
            .with_large_int("number")
            .with_array_tuple_limit(0),
        seen: BTreeSet::new(),
        by_name: BTreeMap::new(),
    };
    declarations.add::<ployz_core::BuildGrantInspected>();
    declarations.add::<ployz_core::BuildGrantMinted>();
    declarations.add::<ClusterTeardown>();
    declarations.add::<ployz_core::InspectBuildGrantRequest>();
    declarations.add::<ployz_core::MintBuildGrantRequest>();
    declarations.add::<ContractDescription>();
    declarations.add::<DataLossConfirmation>();
    declarations.add::<DeployEvent>();
    declarations.add::<DeployIntent>();
    declarations.add::<DeployOutcome<ExecutionError>>();
    declarations.add::<DeployPreview>();
    declarations.add::<crate::drain::DrainReport>();
    declarations.add::<crate::sealing_backfill::BackfillSealingKeyRequest>();
    declarations.add::<crate::sealing_backfill::BackfillSealingKeyReport>();
    declarations.add::<crate::drain::DrainScope>();
    declarations.add::<crate::ingress::DoorsChanged>();
    declarations.add::<crate::private_access::AccessChanged>();
    declarations.add::<crate::private_access::PrivateAccessOn>();
    declarations.add::<ployz_core::authority::KeyApproval>();
    declarations.add::<ployz_core::authority::PrivateAccessApproval>();
    declarations.add::<ployz_core::authority::PrivateAccessTerms>();
    declarations.add::<crate::ingress::IngressCheck>();
    declarations.add::<ExecutionError>();
    declarations.add::<LocalMachineRemoved>();
    declarations.add::<MachineId>();
    declarations.add::<ployz_core::MachineDetails>();
    declarations.add::<ployz_core::authority::Signed<ployz_core::authority::RestartApproval>>();
    declarations.add::<ployz_core::authority::RestartApproval>();
    declarations.add::<ployz_core::authority::ProposalApproval>();
    declarations.add::<ployz_core::authority::DeployApproval>();
    declarations.add::<ployz_core::authority::ApprovedChange>();
    declarations.add::<ployz_core::authority::DeployTerms>();
    declarations.add::<ployz_core::authority::GrantBody>();
    declarations.add::<ployz_core::authority::VolumeApproval>();
    declarations.add::<ployz_core::authority::AutoDeployRule>();
    declarations.add::<ployz_core::authority::RuleTerms>();
    declarations.add::<ployz_core::authority::RuleDraft>();
    declarations.add::<ployz_core::authority::ReviewTerms>();
    declarations.add::<ployz_core::authority::ReviewDraft>();
    declarations.add::<ployz_core::MachineUpdate>();
    declarations.add::<ployz_core::MachineUpdated>();
    declarations.add::<ployz_core::RequestMachineUpgradeRequest>();
    declarations.add::<ployz_core::MachineUpgradeAttempt>();
    declarations.add::<ployz_core::SetManagementClientResponse>();
    declarations.add::<MachineTarget>();
    declarations.add::<ployz_core::PruneTarget>();
    declarations.add::<ployz_core::PublishCertificateMaterialRequest>();
    declarations.add::<ployz_core::CertificateMaterialPublished>();
    declarations.add::<ObservedDataLoss>();
    declarations.add::<PlanOptions>();
    declarations.add::<Namespace>();
    declarations.add::<RegisterRequest>();
    declarations.add::<Registered>();
    declarations.add::<ployz_core::EnrollStage>();
    declarations.add::<ployz_core::EnrollmentAssignment>();
    declarations.add::<ployz_core::EnrollmentSnapshot>();
    declarations.add::<RemoveVolumesRequest>();
    declarations.add::<RequestedServiceSpec>();
    declarations.add::<RpcError>();
    declarations.add::<ployz_core::StorageCapacityError>();
    declarations.add::<RuntimeWatchView>();
    declarations.add::<super::LiveLogRecord>();
    declarations.add::<super::LogHistoryRecord>();
    declarations.add::<VolumeRemoval>();
    declarations.add::<ployz_core::config::ServiceConfig>();
    declarations.add::<ployz_core::config::SavedEnvironmentIntent>();
    declarations.add::<ployz_core::config::ChangeSetInput>();
    declarations.add::<ployz_core::config::RuntimeOutcomeProjection>();
    declarations.add::<ployz_core::config::CompiledEnvironmentIntent>();
    declarations.add::<ployz_core::config::ResolveVariablesInput>();
    declarations.add::<ployz_core::config::ResolveVariablesResult>();
    declarations.add::<ployz_core::config::LiveValuesInput>();
    declarations.add::<ployz_core::config::LiveValues>();
    declarations.add::<ployz_core::config::ServiceSettingInput>();
    declarations.add::<ployz_core::config::ServiceSettingChange>();
    declarations.add::<ployz_core::config::BranchPicks>();
    declarations.add::<ployz_core::config::BranchPlan>();
    declarations.add::<ployz_store::Query>();
    declarations.add::<ployz_store::View>();
    declarations.add::<ployz_store::Command>();
    declarations.add::<ployz_store::Written>();
    declarations.add::<ployz_store::Committed>();
    declarations.add::<ployz_store::Converted>();
    declarations.add::<ployz_store::Trusted>();
    declarations.add::<ployz_store::OperationVerb>();
    declarations.add::<ployz_store::LegacyCount>();
    declarations.add::<ployz_store::Resealed>();
    declarations.add::<ployz_store::GitSource>();
    declarations.add::<ployz_store::SystemEvent>();
    declarations.add::<ployz_store::GithubBuild>();
    declarations.add::<ployz_store::GithubHandOff>();
    declarations.add::<ployz_store::GithubTreeQuery>();
    declarations.add::<ployz_store::GithubTreeView>();
    declarations.add::<ployz_store::GithubFileQuery>();
    declarations.add::<ployz_store::GithubFileView>();
    declarations.add::<ployz_store::GithubClaims>();
    declarations.add::<ployz_store::Unclaimed>();
    declarations.add::<ployz_store::OrganizationRemoved>();
    declarations.add::<ployz_store::AppliedVolume>();
    declarations.add::<super::MetricHistory>();
    declarations.add::<super::MetricHistoryInput>();
    declarations.add::<super::CopyObservation>();
    declarations.add::<super::RetireOutcome>();
    declarations.add::<ployz_core::InspectVolumeCopyRequest>();
    declarations.add::<ployz_core::SeedVolumeRequest>();
    declarations.add::<ployz_core::VolumeCopyView>();
    declarations.add::<ployz_core::SwitchError>();
    declarations.add::<ployz_core::SetVolumeCopyRequest>();
    declarations.add::<ployz_core::SetVolumeCopyReply>();
    declarations.add::<ployz_core::SetVolumeCopyRefused>();
    declarations.add::<ployz_core::SetVolumeSizeRequest>();
    declarations.add::<ployz_core::SetVolumeSizeReply>();
    declarations.add::<ployz_core::SetVolumeSizeRefused>();
    declarations.add::<ployz_core::EnsureContainerRequest>();
    declarations.add::<ployz_core::EnsuredContainer>();
    declarations.add::<ployz_store::tools::ToolDef>();
    declarations.add::<ployz_store::tools::Place>();
    declarations.add::<ployz_store::tools::Step>();
    declarations.add::<ployz_store::tools::Refusal>();
    declarations.add::<ployz_store::tools::Fix>();
    declarations.add::<ployz_store::tools::Answer>();
    declarations.add::<ployz_store::tools::cloud::ServersAnswer>();
    declarations.add::<ployz_store::tools::cloud::ContainersAnswer>();
    declarations.add::<ployz_store::tools::cloud::LogsAnswer>();
    declarations.add::<ployz_store::tools::cloud::MetricsAnswer>();
    declarations.add::<ployz_store::tools::cloud::CopiesAnswer>();
    declarations.add::<ployz_store::tools::cloud::OperationsAnswer>();

    let mut out = String::from(HEADER);
    for declaration in declarations.by_name.values() {
        out.push_str("export ");
        out.push_str(
            &declaration
                .lines()
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n"),
        );
        out.push_str("\n\n");
    }
    out
}

/// Every file the SDK package's `generated/` directory holds, by file name: the payload
/// declarations, `@ployz/sdk/capabilities` as a `CapabilityName` union and ES and CommonJS
/// modules listing every capability the daemon serves, the settings catalog, and
/// `@ployz/sdk/cloud-contract` as the constants Cloud chooses and the CLI matches.
#[must_use]
pub fn generated_files() -> [(&'static str, String); 8] {
    let catalog = ployz_store::catalog::schema(None).expect("the whole catalog always renders");
    let catalog = serde_json::to_string_pretty(&catalog).expect("a JSON value serializes");
    let [declarations, esm, cjs] = capabilities_files();
    let [contract_declarations, contract_esm, contract_cjs] = cloud_contract_files();
    [
        ("payloads.d.ts", typescript_declarations()),
        declarations,
        esm,
        cjs,
        ("catalog.json", format!("{catalog}\n")),
        contract_declarations,
        contract_esm,
        contract_cjs,
    ]
}

/// A constant of `ployz_core::cloud_contract`, as TypeScript spells its value.
enum Constant {
    Number(i128),
    Text(&'static str),
    List(&'static [&'static str]),
}

impl Constant {
    fn value(&self) -> String {
        match self {
            Self::Number(number) => number.to_string(),
            Self::Text(text) => format!("{text:?}"),
            Self::List(items) => format!("Object.freeze({})", Self::tuple(items)),
        }
    }

    fn tuple(items: &[&str]) -> String {
        let items: Vec<String> = items.iter().map(|item| format!("{item:?}")).collect();
        format!("[{}]", items.join(", "))
    }

    fn declaration(&self) -> String {
        match self {
            Self::List(items) => format!("readonly {}", Self::tuple(items)),
            Self::Number(_) | Self::Text(_) => self.value(),
        }
    }
}

fn cloud_contract_constants() -> Vec<(&'static str, Constant)> {
    use Constant::{List, Number, Text};
    use ployz_core::cloud_contract as contract;
    vec![
        ("UPLOAD_LIMIT", Number(contract::UPLOAD_LIMIT as i128)),
        ("REVISION_HEADER", Text(contract::REVISION_HEADER)),
        ("VIA_HEADER", Text(contract::VIA_HEADER)),
        ("APPROVAL_HEADER", Text(contract::APPROVAL_HEADER)),
        ("REQUEST_KEY_HEADER", Text(contract::REQUEST_KEY_HEADER)),
        ("AGENT_HEADER", Text(contract::AGENT_HEADER)),
        ("ERROR_CODE_HEADER", Text(contract::ERROR_CODE_HEADER)),
        ("CLIENT_ID", Text(contract::CLIENT_ID)),
        ("MANIFEST_FORMAT", Number(contract::MANIFEST_FORMAT.into())),
        (
            "CHECKIN_PROTOCOL_VERSION",
            Number(contract::CHECKIN_PROTOCOL_VERSION.into()),
        ),
        (
            "MAX_SKEW_SECONDS",
            Number(contract::MAX_SKEW_SECONDS.into()),
        ),
        (
            "CHECKIN_WAIT_SECONDS",
            Number(contract::CHECKIN_WAIT_SECONDS.into()),
        ),
        ("ENROLL_OPTIONS", List(contract::ENROLL_OPTIONS)),
        ("PROBE_WINDOW_MS", Number(contract::PROBE_WINDOW_MS.into())),
        (
            "PROBE_FIRST_ATTEMPT_MS",
            Number(contract::PROBE_FIRST_ATTEMPT_MS.into()),
        ),
        (
            "PROBE_ATTEMPT_EVERY_MS",
            Number(contract::PROBE_ATTEMPT_EVERY_MS.into()),
        ),
        (
            "PROBE_ATTEMPT_TIMEOUT_MS",
            Number(contract::PROBE_ATTEMPT_TIMEOUT_MS.into()),
        ),
        (
            "PROBE_GREETING_READ_MS",
            Number(contract::PROBE_GREETING_READ_MS.into()),
        ),
        (
            "PROBE_GREETING_LENGTH",
            Number(contract::PROBE_GREETING_LENGTH as i128),
        ),
        (
            "MIN_MANAGED_VOLUME_BYTES",
            Number(contract::MIN_MANAGED_VOLUME_BYTES.into()),
        ),
        (
            "MAX_LIFETIME_MILLIS",
            Number(contract::MAX_LIFETIME_MILLIS.into()),
        ),
        (
            "CLOCK_SKEW_MILLIS",
            Number(contract::CLOCK_SKEW_MILLIS.into()),
        ),
        (
            "MAX_GRANT_LIFETIME_MILLIS",
            Number(contract::MAX_GRANT_LIFETIME_MILLIS.into()),
        ),
    ]
}

fn cloud_contract_files() -> [(&'static str, String); 3] {
    let constants = cloud_contract_constants();
    let declarations: String = constants
        .iter()
        .map(|(name, constant)| {
            format!("export declare const {name}: {};\n", constant.declaration())
        })
        .collect();
    let esm: String = constants
        .iter()
        .map(|(name, constant)| format!("export const {name} = {};\n", constant.value()))
        .collect();
    let cjs: String = constants
        .iter()
        .map(|(name, constant)| format!("exports.{name} = {};\n", constant.value()))
        .collect();
    [
        ("cloud-contract.d.ts", format!("{HEADER}{declarations}")),
        ("cloud-contract.mjs", format!("{HEADER}{esm}")),
        (
            "cloud-contract.js",
            format!("{HEADER}\"use strict\";\n\n{cjs}"),
        ),
    ]
}

fn capabilities_files() -> [(&'static str, String); 3] {
    let names = ployz_core::served_capabilities();
    let union: String = names
        .iter()
        .map(|name| format!("  | \"{name}\"\n"))
        .collect();
    let list: String = names
        .iter()
        .map(|name| format!("  \"{name}\",\n"))
        .collect();
    [
        (
            "capabilities.d.ts",
            format!(
                "{HEADER}export type CapabilityName =\n{union};\n\nexport declare const capabilities: readonly CapabilityName[];\n"
            ),
        ),
        (
            "capabilities.mjs",
            format!("{HEADER}export const capabilities = Object.freeze([\n{list}]);\n"),
        ),
        (
            "capabilities.js",
            format!(
                "{HEADER}\"use strict\";\n\nexports.capabilities = Object.freeze([\n{list}]);\n"
            ),
        ),
    ]
}

#[cfg(test)]
#[path = "payloads_tests.rs"]
mod tests;
