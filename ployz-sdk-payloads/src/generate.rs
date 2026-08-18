//! Deterministic TypeScript and JSON fixtures for `@ployz/sdk` public payloads.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use ployz_core::{
    CertificateAvailability, CertificateFailureKind, HealthObservation, MembershipObservation,
    RpcErrorCode,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::values::{additive_examples, catalogued_capabilities, fixtures, tagged_examples};

/// npm package name for this crate's Node artifact.
pub const PACKAGE_NAME: &str = "@ployz/sdk";

const TYPESCRIPT_PREAMBLE: &str = "\
export type JsonValue =\n  | null\n  | boolean\n  | number\n  | string\n  | JsonValue[]\n  | JsonObject;\n\n\
export type JsonObject = { readonly [key: string]: JsonValue | undefined };\n\n\
export type Additive<T extends object> = T & JsonObject;\n\n\
export type SerdeResult<T, E> =\n  | Additive<{ Ok: T }>\n  | Additive<{ Err: E }>;\n\n";

enum Shape {
    Alias(&'static str),
    OpenString(&'static [&'static str]),
    ClosedString(&'static [&'static str]),
    Additive {
        params: &'static str,
        fields: &'static [(&'static str, &'static str)],
    },
    ExternallyTagged {
        params: &'static str,
        variants: &'static [(&'static str, Option<&'static str>)],
    },
    InternallyTagged {
        tag: &'static str,
        variants: &'static [(&'static str, &'static [(&'static str, &'static str)])],
    },
}

const PAYLOADS: &[(&str, Shape)] = &[
    ("MachineId", Shape::Alias("string")),
    ("ContainerId", Shape::Alias("string")),
    ("ServiceId", Shape::Alias("string")),
    ("ServiceName", Shape::Alias("string")),
    ("MachineName", Shape::Alias("string")),
    ("MachineSubnet", Shape::Alias("string")),
    ("ManagementAddress", Shape::Alias("string")),
    ("AdvertisedEndpoint", Shape::Alias("string")),
    ("SelectedEndpoint", Shape::Alias("string")),
    ("ContainerAddress", Shape::Alias("string")),
    ("IngressHost", Shape::Alias("string")),
    ("DockerVolumeName", Shape::Alias("string")),
    ("CapabilityName", Shape::Alias("string")),
    ("WireGuardPublicKey", Shape::Alias("number[]")),
    ("RequestedServiceSpec", Shape::Alias("JsonValue")),
    ("ResolvedServiceSpec", Shape::Alias("JsonValue")),
    ("ServiceVolume", Shape::Alias("JsonValue")),
    (
        "MembershipObservation",
        Shape::OpenString(MembershipObservation::known_wires()),
    ),
    (
        "HealthObservation",
        Shape::OpenString(HealthObservation::known_wires()),
    ),
    (
        "RpcErrorCode",
        Shape::OpenString(RpcErrorCode::known_wires()),
    ),
    (
        "CertificateAvailability",
        Shape::OpenString(CertificateAvailability::known_wires()),
    ),
    (
        "CertificateFailureKind",
        Shape::OpenString(CertificateFailureKind::known_wires()),
    ),
    (
        "ContainerKind",
        Shape::ClosedString(&["service_container", "pre_deploy_hook"]),
    ),
    (
        "DockerVolumeId",
        Shape::Additive {
            params: "",
            fields: &[("machine_id", "MachineId"), ("name", "DockerVolumeName")],
        },
    ),
    (
        "DockerVolume",
        Shape::Additive {
            params: "",
            fields: &[
                ("id", "DockerVolumeId"),
                ("driver", "string"),
                ("options", "{ readonly [key: string]: string }"),
                ("labels", "{ readonly [key: string]: string }"),
            ],
        },
    ),
    (
        "ContractDescription",
        Shape::Additive {
            params: "",
            fields: &[
                ("machine_id", "MachineId"),
                ("protocol_major", "number"),
                ("daemon_version", "string"),
                ("capabilities", "CapabilityName[]"),
            ],
        },
    ),
    (
        "RpcError",
        Shape::Additive {
            params: "",
            fields: &[
                ("code", "RpcErrorCode"),
                ("message", "string"),
                ("details", "JsonValue?"),
            ],
        },
    ),
    (
        "MachineSuccess",
        Shape::Additive {
            params: "<T>",
            fields: &[("machine_id", "MachineId"), ("value", "T")],
        },
    ),
    (
        "MachineFailure",
        Shape::Additive {
            params: "<E>",
            fields: &[("machine_id", "MachineId"), ("error", "E")],
        },
    ),
    (
        "PartialResult",
        Shape::Additive {
            params: "<T, E>",
            fields: &[
                ("successes", "Array<MachineSuccess<T>>"),
                ("failures", "Array<MachineFailure<E>>"),
                ("omissions", "MachineId[]"),
            ],
        },
    ),
    (
        "ContainerRuntimeObservation",
        Shape::InternallyTagged {
            tag: "state",
            variants: &[
                ("created", &[]),
                ("running", &[("health", "HealthObservation")]),
                ("paused", &[]),
                ("restarting", &[]),
                ("exited", &[("code", "number")]),
                ("removing", &[]),
                ("dead", &[]),
            ],
        },
    ),
    (
        "PlanOptions",
        Shape::Additive {
            params: "",
            fields: &[
                ("force_recreate", "boolean"),
                ("skip_health_monitor", "boolean"),
                ("placement_seed", "number"),
            ],
        },
    ),
    (
        "ServiceAttempt",
        Shape::Additive {
            params: "",
            fields: &[("name", "ServiceName")],
        },
    ),
    (
        "DeployIntent",
        Shape::Additive {
            params: "",
            fields: &[
                ("target", "RequestedServiceSpec[]"),
                ("apply", "ServiceAttempt[]"),
                ("options", "PlanOptions"),
            ],
        },
    ),
    (
        "ReplacementOperation",
        Shape::Additive {
            params: "",
            fields: &[
                ("machine_id", "MachineId"),
                ("old_container_id", "ContainerId"),
                ("spec", "ResolvedServiceSpec"),
                ("skip_health_monitor", "boolean"),
            ],
        },
    ),
    (
        "DeployOperation",
        Shape::ExternallyTagged {
            params: "",
            variants: &[
                (
                    "CreateVolume",
                    Some("{ machine_id: MachineId; volume: ServiceVolume }"),
                ),
                (
                    "RunContainer",
                    Some(
                        "{ machine_id: MachineId; spec: ResolvedServiceSpec; skip_health_monitor: boolean }",
                    ),
                ),
                (
                    "StopContainer",
                    Some("{ machine_id: MachineId; container_id: ContainerId }"),
                ),
                (
                    "RemoveContainer",
                    Some("{ machine_id: MachineId; container_id: ContainerId }"),
                ),
                ("ReplaceContainer", Some("ReplacementOperation")),
                (
                    "StopHook",
                    Some("{ machine_id: MachineId; container_id: ContainerId }"),
                ),
                (
                    "RunHook",
                    Some(
                        "{ machine_id: MachineId; spec: ResolvedServiceSpec; old_hook_containers: Array<[MachineId, ContainerId]> }",
                    ),
                ),
            ],
        },
    ),
    (
        "RestartAttempt",
        Shape::ExternallyTagged {
            params: "<E = RpcError>",
            variants: &[
                ("NotAttempted", None),
                ("Attempted", Some("SerdeResult<null, E>")),
            ],
        },
    ),
    (
        "ReplacementCompensation",
        Shape::ExternallyTagged {
            params: "<E = RpcError>",
            variants: &[
                (
                    "StartFirst",
                    Some("{ stop_new_container: SerdeResult<null, E> }"),
                ),
                (
                    "StopFirst",
                    Some(
                        "{ stop_new_container: SerdeResult<null, E>; restart_old_container: RestartAttempt<E> }",
                    ),
                ),
            ],
        },
    ),
    (
        "FailedOperation",
        Shape::ExternallyTagged {
            params: "<E = RpcError>",
            variants: &[
                (
                    "Operation",
                    Some("{ operation: DeployOperation; error: E }"),
                ),
                (
                    "ReplacementHealth",
                    Some(
                        "{ operation: ReplacementOperation; error: E; compensation: ReplacementCompensation<E> }",
                    ),
                ),
            ],
        },
    ),
    (
        "DeployOutcome",
        Shape::ExternallyTagged {
            params: "<E = RpcError>",
            variants: &[
                ("Success", Some("{ completed: DeployOperation[] }")),
                (
                    "Failed",
                    Some(
                        "{ completed: DeployOperation[]; failed: FailedOperation<E>; unexecuted: DeployOperation[] }",
                    ),
                ),
            ],
        },
    ),
    (
        "MachineRuntime",
        Shape::Additive {
            params: "",
            fields: &[
                ("daemon_version", "string"),
                ("docker_version", "string"),
                ("hostname", "string"),
                ("architecture", "string"),
                ("os_pretty_name", "string"),
                ("kernel_version", "string"),
            ],
        },
    ),
    (
        "Machine",
        Shape::Additive {
            params: "",
            fields: &[
                ("id", "MachineId"),
                ("name", "MachineName"),
                ("subnet", "MachineSubnet"),
                ("management_address", "ManagementAddress"),
                ("public_key", "WireGuardPublicKey"),
                ("public_ip", "string?"),
                ("advertised_endpoints", "AdvertisedEndpoint[]"),
                ("runtime", "MachineRuntime"),
            ],
        },
    ),
    (
        "RttStatistics",
        Shape::Additive {
            params: "",
            fields: &[("median_ns", "number"), ("population_stddev_ns", "number")],
        },
    ),
    (
        "MachineObservation",
        Shape::Additive {
            params: "",
            fields: &[
                ("machine", "Machine"),
                ("membership", "MembershipObservation"),
                ("selected_endpoint", "SelectedEndpoint | null"),
                ("rtt", "RttStatistics?"),
            ],
        },
    ),
    (
        "ContainerObservation",
        Shape::Additive {
            params: "",
            fields: &[
                ("container_id", "ContainerId"),
                ("display_name", "string"),
                ("created_at_unix_nanos", "number"),
                ("machine_id", "MachineId"),
                ("service_id", "ServiceId"),
                ("service_name", "ServiceName"),
                ("kind", "ContainerKind"),
                ("runtime", "ContainerRuntimeObservation"),
                ("effective_healthcheck", "JsonValue | null"),
                ("resolved_spec", "ResolvedServiceSpec"),
                ("address", "ContainerAddress | null"),
                ("labels", "{ readonly [key: string]: string }"),
            ],
        },
    ),
    ("ServiceContainer", Shape::Alias("ContainerObservation")),
    ("HookContainer", Shape::Alias("ContainerObservation")),
    (
        "ServiceObservation",
        Shape::Additive {
            params: "",
            fields: &[
                ("service_id", "ServiceId"),
                ("containers", "ServiceContainer[]"),
                ("hook_containers", "HookContainer[]"),
            ],
        },
    ),
    (
        "CertificateBackoff",
        Shape::Additive {
            params: "",
            fields: &[
                ("failure_kind", "CertificateFailureKind"),
                ("next_attempt_at", "string"),
                ("failures", "number"),
            ],
        },
    ),
    (
        "CertificateObservation",
        Shape::Additive {
            params: "",
            fields: &[
                ("hostname", "IngressHost"),
                ("status", "CertificateAvailability"),
                ("last_error", "string?"),
                ("backoff", "CertificateBackoff?"),
            ],
        },
    ),
    (
        "RuntimeWatchIncompleteIds",
        Shape::Additive {
            params: "",
            fields: &[
                ("machines", "MachineId[]"),
                ("containers", "ContainerId[]"),
                ("volumes", "DockerVolumeId[]"),
                ("certificates", "IngressHost[]"),
            ],
        },
    ),
    (
        "RuntimeWatchFrame",
        Shape::Additive {
            params: "",
            fields: &[
                ("machines", "MachineObservation[]"),
                ("containers", "ContainerObservation[]"),
                ("services", "ServiceObservation[]"),
                ("volumes", "DockerVolume[]"),
                ("certificates", "CertificateObservation[]"),
                ("hosted_dns_hostname", "string?"),
                ("incomplete_ids", "RuntimeWatchIncompleteIds"),
                ("observed_at", "string"),
            ],
        },
    ),
];

/// Generated TypeScript declarations and JSON fixtures.
pub struct Artifacts {
    pub index_dts: String,
    pub fixtures_json: String,
}

/// Build the checked-in `@ployz/sdk` artifacts from Rust types.
#[must_use]
pub fn artifacts() -> Artifacts {
    check_additive_fields_match_rust();
    check_externally_tagged_variants_match_rust();
    check_internally_tagged_variants_match_rust();
    check_closed_strings_match_rust();
    Artifacts {
        index_dts: typescript(),
        fixtures_json: pretty_json(&Value::Object(fixtures().into_iter().collect())),
    }
}

/// Write generated files under the napi package root.
///
/// # Errors
///
/// Returns filesystem errors from creating `generated/` or writing files.
pub fn write_generated(root: &Path) -> std::io::Result<()> {
    let artifacts = artifacts();
    fs::create_dir_all(root.join("generated"))?;
    fs::write(root.join("index.d.ts"), artifacts.index_dts)?;
    fs::write(
        root.join("generated/fixtures.json"),
        artifacts.fixtures_json,
    )?;
    Ok(())
}

/// Compare generated output to the files checked in under `root`.
#[must_use]
pub fn drift(root: &Path) -> Option<String> {
    let expected = artifacts();
    let mut problems = Vec::new();
    match fs::read_to_string(root.join("index.d.ts")) {
        Ok(on_disk) if on_disk == expected.index_dts => {}
        Ok(_) => problems.push("index.d.ts is stale".to_owned()),
        Err(error) => problems.push(format!("index.d.ts: {error}")),
    }
    match fs::read_to_string(root.join("generated/fixtures.json")) {
        Ok(on_disk) if on_disk == expected.fixtures_json => {}
        Ok(_) => problems.push("generated/fixtures.json is stale".to_owned()),
        Err(error) => problems.push(format!("generated/fixtures.json: {error}")),
    }
    if problems.is_empty() {
        None
    } else {
        problems.push(
            "regenerate with `cargo run -p ployz-sdk-payloads --bin generate-sdk-types`".to_owned(),
        );
        Some(problems.join("\n"))
    }
}

fn typescript() -> String {
    let mut out = String::from(
        "// Generated by `cargo run -p ployz-sdk-payloads --bin generate-sdk-types`. Do not edit.\n\n",
    );
    out.push_str(TYPESCRIPT_PREAMBLE);
    for (name, shape) in PAYLOADS {
        out.push_str(&emit_shape(name, shape));
        out.push('\n');
    }
    out.push_str(&capability_typescript());
    out.push_str("export declare function packageName(): \"");
    out.push_str(PACKAGE_NAME);
    out.push_str("\";\n");
    out
}

fn emit_shape(name: &str, shape: &Shape) -> String {
    match shape {
        Shape::Alias(ts) => format!("export type {name} = {ts};\n"),
        Shape::OpenString(known) => {
            format!(
                "export type {name} = {} | (string & {{}});\n",
                quoted_union(known)
            )
        }
        Shape::ClosedString(known) => {
            format!("export type {name} = {};\n", quoted_union(known))
        }
        Shape::Additive { params, fields } => emit_additive(name, params, fields),
        Shape::ExternallyTagged { params, variants } => {
            emit_externally_tagged(name, params, variants)
        }
        Shape::InternallyTagged { tag, variants } => emit_internally_tagged(name, tag, variants),
    }
}

fn quoted_union(known: &[&str]) -> String {
    known
        .iter()
        .map(|value| format!("\"{value}\""))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn emit_additive(name: &str, params: &str, fields: &[(&str, &str)]) -> String {
    let mut body = format!("export type {name}{params} = Additive<{{\n");
    for (field, ts) in fields {
        let (ts, optional) = strip_optional(ts);
        body.push_str("  ");
        body.push_str(field);
        if optional {
            body.push('?');
        }
        body.push_str(": ");
        body.push_str(ts);
        body.push_str(";\n");
    }
    body.push_str("}>;\n");
    body
}

fn emit_externally_tagged(name: &str, params: &str, variants: &[(&str, Option<&str>)]) -> String {
    let mut body = format!("export type {name}{params} =\n");
    for (index, (variant, payload)) in variants.iter().enumerate() {
        body.push_str("  | ");
        match payload {
            None => {
                body.push('"');
                body.push_str(variant);
                body.push('"');
            }
            Some(ts) => {
                body.push_str("Additive<{ ");
                body.push_str(variant);
                body.push_str(": ");
                body.push_str(ts);
                body.push_str(" }>");
            }
        }
        if index + 1 == variants.len() {
            body.push_str(";\n");
        } else {
            body.push('\n');
        }
    }
    body
}

fn emit_internally_tagged(name: &str, tag: &str, variants: &[(&str, &[(&str, &str)])]) -> String {
    let mut arms = Vec::new();
    for (tag_value, fields) in variants {
        let mut members = vec![format!("{tag}: \"{tag_value}\"")];
        for (field, ts) in *fields {
            members.push(format!("{field}: {ts}"));
        }
        arms.push(format!("Additive<{{ {} }}>", members.join("; ")));
    }
    arms.push(format!("Additive<{{ {tag}?: string }}>"));
    format!("export type {name} =\n  | {};\n", arms.join("\n  | "))
}

fn capability_typescript() -> String {
    let rows = catalogued_capabilities();
    let mut out = String::new();
    for (rust_name, wire) in &rows {
        out.push_str("export const ");
        out.push_str(rust_name);
        out.push_str(": CapabilityName = \"");
        out.push_str(wire);
        out.push_str("\";\n");
    }
    out.push('\n');
    out.push_str("export const CATALOGUED_CAPABILITIES = [\n");
    for (_, wire) in &rows {
        out.push_str("  \"");
        out.push_str(wire);
        out.push_str("\",\n");
    }
    out.push_str("] as const;\n\n");
    out.push_str(
        "export type CataloguedCapability = (typeof CATALOGUED_CAPABILITIES)[number];\n\n",
    );
    out
}

fn check_additive_fields_match_rust() {
    let examples = additive_examples();
    for (name, shape) in PAYLOADS {
        let Shape::Additive { fields, .. } = shape else {
            continue;
        };
        let value = examples
            .get(*name)
            .unwrap_or_else(|| panic!("{name} Additive shape has no serde example"));
        let object = value
            .as_object()
            .unwrap_or_else(|| panic!("{name} serde value is not an object"));
        for key in object.keys() {
            assert!(
                fields.iter().any(|(field, _)| field == key),
                "{name} TypeScript fields missing serde key {key}"
            );
        }
        for (field, ts) in *fields {
            if strip_optional(ts).1 {
                continue;
            }
            assert!(
                object.contains_key(*field),
                "{name} serde value missing field {field}"
            );
        }
    }
}

fn check_externally_tagged_variants_match_rust() {
    let examples = tagged_examples();
    for (name, shape) in PAYLOADS {
        let Shape::ExternallyTagged { variants, .. } = shape else {
            continue;
        };
        let mut seen = BTreeSet::new();
        for value in serde_examples(name, &examples) {
            let key = external_tag(name, value);
            let payload = variants
                .iter()
                .find_map(|(variant, payload)| (*variant == key).then_some(*payload));
            assert!(
                payload.is_some(),
                "{name} TypeScript is missing variant {key}"
            );
            assert_eq!(
                payload == Some(None),
                value.is_string(),
                "{name} variant {key} unit/payload shape does not match serde"
            );
            seen.insert(key);
        }
        for (variant, _) in *variants {
            assert!(
                seen.contains(*variant),
                "{name} TypeScript variant {variant} has no serde example"
            );
        }
    }
}

fn check_internally_tagged_variants_match_rust() {
    let examples = tagged_examples();
    for (name, shape) in PAYLOADS {
        let Shape::InternallyTagged { tag, variants, .. } = shape else {
            continue;
        };
        let mut seen = BTreeSet::new();
        for value in serde_examples(name, &examples) {
            let object = value
                .as_object()
                .unwrap_or_else(|| panic!("{name} serde example is not an object"));
            let wire = object
                .get(*tag)
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{name} serde example is missing tag {tag}"));
            if variants.iter().any(|(variant, _)| *variant == wire) {
                seen.insert(wire.to_owned());
            }
        }
        for (variant, _) in *variants {
            assert!(
                seen.contains(*variant),
                "{name} TypeScript variant {variant} has no serde example"
            );
        }
    }
}

fn check_closed_strings_match_rust() {
    let examples = tagged_examples();
    for (name, shape) in PAYLOADS {
        let Shape::ClosedString(known) = shape else {
            continue;
        };
        let mut seen = BTreeSet::new();
        for value in serde_examples(name, &examples) {
            let wire = value
                .as_str()
                .unwrap_or_else(|| panic!("{name} serde example is not a string"));
            assert!(
                known.contains(&wire),
                "{name} TypeScript is missing closed wire {wire}"
            );
            seen.insert(wire.to_owned());
        }
        for wire in *known {
            assert!(
                seen.contains(*wire),
                "{name} TypeScript wire {wire} has no serde example"
            );
        }
    }
}

fn serde_examples<'a>(name: &str, examples: &'a BTreeMap<&'static str, Vec<Value>>) -> &'a [Value] {
    let values = examples
        .get(name)
        .unwrap_or_else(|| panic!("{name} shape has no serde example"));
    assert!(!values.is_empty(), "{name} shape has no serde example");
    values
}

fn external_tag(name: &str, value: &Value) -> String {
    match value {
        Value::String(wire) => wire.clone(),
        Value::Object(object) => {
            assert_eq!(
                object.len(),
                1,
                "{name} externally tagged JSON must have one variant key"
            );
            object
                .keys()
                .next()
                .expect("externally tagged object has a variant key")
                .clone()
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) => {
            panic!("{name} serde example is not a tagged enum: {value}")
        }
    }
}

fn strip_optional(ts: &str) -> (&str, bool) {
    ts.strip_suffix('?')
        .map_or((ts, false), |inner| (inner, true))
}

fn pretty_json(value: &Value) -> String {
    let mut encoded = serde_json::to_string_pretty(value).expect("fixtures are valid JSON");
    encoded.push('\n');
    encoded
}

/// Package directory for the `@ployz/sdk` napi artifact.
#[must_use]
pub fn sdk_package_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("ployz-sdk")
}

/// Decode a fixture as `T`.
#[must_use]
pub fn decode_fixture<T: DeserializeOwned>(value: &Value) -> T {
    serde::Deserialize::deserialize(value).expect("fixture matches the Rust type")
}
