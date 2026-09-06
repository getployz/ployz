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
    ObservedDataLoss, PlanOptions, ProjectName, RegisterRequest, Registered, RemoveVolumesRequest,
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
        if T::output_path().is_some() {
            let name = T::ident(&self.config);
            let declaration = T::decl(&self.config);
            match self.by_name.get(&name) {
                None => {
                    self.by_name.insert(name, declaration);
                }
                Some(existing) => assert_eq!(
                    existing, &declaration,
                    "two Rust types declare the TypeScript name {name}"
                ),
            }
        }
        T::visit_generics(self);
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
    declarations.add::<ClusterTeardown>();
    declarations.add::<ContractDescription>();
    declarations.add::<DataLossConfirmation>();
    declarations.add::<DeployEvent>();
    declarations.add::<DeployIntent>();
    declarations.add::<DeployOutcome<ExecutionError>>();
    declarations.add::<DeployPreview>();
    declarations.add::<ExecutionError>();
    declarations.add::<LocalMachineRemoved>();
    declarations.add::<MachineId>();
    declarations.add::<MachineTarget>();
    declarations.add::<ObservedDataLoss>();
    declarations.add::<PlanOptions>();
    declarations.add::<ProjectName>();
    declarations.add::<RegisterRequest>();
    declarations.add::<Registered>();
    declarations.add::<RemoveVolumesRequest>();
    declarations.add::<RequestedServiceSpec>();
    declarations.add::<RpcError>();
    declarations.add::<RuntimeWatchView>();
    declarations.add::<VolumeRemoval>();

    let mut out = String::from(HEADER);
    for declaration in declarations.by_name.values() {
        out.push_str("export ");
        out.push_str(declaration);
        out.push_str("\n\n");
    }
    out
}
