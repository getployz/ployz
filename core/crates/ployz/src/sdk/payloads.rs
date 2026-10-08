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

const HEADER: &str = "// Generated from the Rust wire types by `cargo test -p ployz --test sdk_payloads`.\n// Do not edit.\n\n";

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
#[must_use]
pub fn typescript_declarations() -> String {
    let mut declarations = Declarations {
        config: Config::new()
            .with_large_int("number")
            .with_array_tuple_limit(0),
        seen: BTreeSet::new(),
        by_name: BTreeMap::new(),
    };
    declarations.add::<ployz_core::BuildGrantEnded>();
    declarations.add::<ployz_core::SourceContainerRequest>();
    declarations.add::<ployz_core::HandOverRequest>();
    declarations.add::<ployz_core::ServiceVolumeRequest>();
    declarations.add::<ployz_core::BuildGrantMinted>();
    declarations.add::<ClusterTeardown>();
    declarations.add::<ployz_core::EndBuildGrantRequest>();
    declarations.add::<ployz_core::MintBuildGrantRequest>();
    declarations.add::<ContractDescription>();
    declarations.add::<DataLossConfirmation>();
    declarations.add::<DeployEvent>();
    declarations.add::<DeployIntent>();
    declarations.add::<DeployOutcome<ExecutionError>>();
    declarations.add::<DeployPreview>();
    declarations.add::<crate::drain::DrainReport>();
    declarations.add::<crate::drain::DrainScope>();
    declarations.add::<ExecutionError>();
    declarations.add::<LocalMachineRemoved>();
    declarations.add::<MachineId>();
    declarations.add::<ployz_core::MachineDetails>();
    declarations.add::<ployz_core::MachineUpdate>();
    declarations.add::<ployz_core::MachineUpdated>();
    declarations.add::<ployz_core::RequestMachineUpgradeRequest>();
    declarations.add::<ployz_core::InspectMachineUpgradeRequest>();
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
    declarations.add::<ployz_core::EnrollmentAssignment>();
    declarations.add::<ployz_core::EnrollmentSnapshot>();
    declarations.add::<RemoveVolumesRequest>();
    declarations.add::<RequestedServiceSpec>();
    declarations.add::<RpcError>();
    declarations.add::<ployz_core::StorageCapacityError>();
    declarations.add::<RuntimeWatchView>();
    declarations.add::<super::ContainerLogRecord>();
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
    declarations.add::<ployz_store::Trusted>();
    declarations.add::<ployz_store::GitSource>();
    declarations.add::<ployz_store::SystemEvent>();
    declarations.add::<ployz_store::GithubBuild>();
    declarations.add::<ployz_store::GithubClaims>();
    declarations.add::<ployz_store::Unclaimed>();
    declarations.add::<ployz_store::OrganizationRemoved>();
    declarations.add::<ployz_store::AppliedVolume>();
    declarations.add::<super::CopyObservation>();
    declarations.add::<ployz_core::InspectVolumeCopyRequest>();
    declarations.add::<ployz_core::VolumeCopyView>();
    declarations.add::<ployz_core::AdoptLeaseRequest>();
    declarations.add::<ployz_core::DeclareMirrorRequest>();
    declarations.add::<ployz_core::MirrorRequest>();
    declarations.add::<ployz_core::CommitRequest>();
    declarations.add::<ployz_core::WarmRequest>();
    declarations.add::<ployz_core::InspectReceiveRequest>();
    declarations.add::<ployz_core::ReceiveView>();
    declarations.add::<ployz_core::SwitchReply>();
    declarations.add::<ployz_core::SwitchError>();

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
