//! What an edit says of its Typed Addresses, to a person and under `--json`: the reference
//! to type in place of each address. Nothing typed around an address is repeated, since
//! any part of it may be a secret, so only a value that is the address alone gets a line
//! that sets the reference.

use std::collections::BTreeMap;

use clap::ArgMatches;
use ployz_core::ServiceName;
use ployz_core::config::{PRIVATE_DOMAIN_KEY, ValuePart, ValuePartOwner, render_variable_parts};
use ployz_store::{Edited, EnvironmentSummary, Instead, SettingNote, SettingPath, TypedAddresses};
use serde::Serialize;

use crate::cli::{Rerun, Shown};
use crate::ui;

/// An edit's result as `--json` prints it: each Typed Address's references, never its value.
#[derive(Serialize)]
pub(super) struct Reported<'a> {
    environment: &'a EnvironmentSummary,
    staged: &'a [SettingPath],
    immediate: &'a [SettingPath],
    typed_addresses: Vec<ReportedTyped<'a>>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    notes: &'a [SettingNote],
}

impl<'a> Reported<'a> {
    pub(super) fn of(edited: &'a Edited) -> Self {
        Self {
            environment: &edited.environment,
            staged: &edited.staged,
            immediate: &edited.immediate,
            typed_addresses: edited
                .typed_addresses
                .iter()
                .map(|typed| ReportedTyped {
                    path: &typed.path,
                    services: &typed.services,
                    instead: Reference::of(typed),
                })
                .collect(),
            notes: &edited.notes,
        }
    }
}

#[derive(Serialize)]
struct ReportedTyped<'a> {
    path: &'a SettingPath,
    services: &'a [ServiceName],
    instead: Reference,
}

/// What to set instead of a value with Typed Addresses.
#[derive(Serialize)]
#[serde(tag = "kind", rename = "reference")]
struct Reference {
    /// Each of `services`' references, in its order, to type in place of its address.
    references: Vec<String>,
    /// The value to set, when it is references alone: nothing was typed around them.
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<Shown>,
}

impl Reference {
    fn of(typed: &TypedAddresses) -> Self {
        let Instead::Reference { value } = &typed.instead;
        Self {
            references: typed.services.iter().map(reference).collect(),
            value: Shown::references(value),
        }
    }
}

/// The reference to `service`'s private address, as the Store writes one.
fn reference(service: &ServiceName) -> String {
    let name = service.to_string();
    let part = ValuePart::Ref {
        owner: ValuePartOwner::Service {
            lineage_id: name.clone(),
        },
        key: PRIVATE_DOMAIN_KEY.to_owned(),
    };
    render_variable_parts(&[part], &BTreeMap::from([(name.clone(), name)]))
}

/// What a variable's Typed Addresses cost, and what to set instead in `environment`.
pub(super) fn say(matches: &ArgMatches, environment: &EnvironmentSummary, typed: &TypedAddresses) {
    let (path, services) = (&typed.path, super::super::joined(&typed.services));
    let them = if typed.services.len() == 1 {
        "it"
    } else {
        "them"
    };
    let Some(consumer) = path.node() else {
        return;
    };
    ui::warn(format!(
        "{path} types the private address of {services}, so Ployz can't see that {consumer} uses {them}: a Branch that doesn't copy {them} can't reach {them}, and a Deploy won't start {them} first."
    ));
    let reference = Reference::of(typed);
    let Some(value) = reference.value else {
        let whose = match typed.services.as_slice() {
            [service] => format!("{service}'s address"),
            _ => format!("the addresses of {services}"),
        };
        ui::note(format!(
            "Set {path} again with {} in place of {whose}.",
            reference.references.join(" and ")
        ));
        return;
    };
    let scope = super::super::store::scope_args(&environment.project, &environment.name);
    let assignment = format!("{path}={value}");
    ui::hint(
        &Rerun::then(
            matches,
            &[&["set", assignment.as_str()][..], &scope].concat(),
        )
        .into(),
    );
}
