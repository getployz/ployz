use super::*;

fn file(name: &str) -> String {
    generated_files()
        .into_iter()
        .find_map(|(file, contents)| (file == name).then_some(contents))
        .unwrap_or_else(|| panic!("{name} is not generated"))
}

#[test]
fn every_generated_file_is_named_once() {
    let names: Vec<_> = generated_files()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        names,
        [
            "payloads.d.ts",
            "capabilities.d.ts",
            "capabilities.mjs",
            "capabilities.js",
            "catalog.json",
            "cloud-contract.d.ts",
            "cloud-contract.mjs",
            "cloud-contract.js",
        ]
    );
}

/// Cloud imports the verbs it gates operations with from these declarations, so their
/// text is frozen.
#[test]
fn the_operation_verbs_cloud_names_are_declared() {
    let payloads = file("payloads.d.ts");
    let declared = payloads
        .lines()
        .find(|line| line.contains("type OperationVerb ="))
        .unwrap_or_else(|| panic!("OperationVerb is not declared"));
    assert_eq!(
        declared,
        "export type OperationVerb = \"drain\" | \"set\" | \"clean\" | \"remove\" | \"forget\" | \"release\" | \"mirror\" | \"sync\" | \"remove_mirror\" | \"stop\" | \"restart\" | \"unprotect\" | \"move\" | \"restore\" | \"resize\" | \"set_cluster_name\" | \"set_ingress_doors\" | \"set_private_access\";"
    );
}

const CLOUD_CONTRACT: &[(&str, &str, &str)] = &[
    ("UPLOAD_LIMIT", "99614720", "99614720"),
    (
        "REVISION_HEADER",
        "\"x-ployz-manifest\"",
        "\"x-ployz-manifest\"",
    ),
    ("VIA_HEADER", "\"x-ployz-via\"", "\"x-ployz-via\""),
    (
        "APPROVAL_HEADER",
        "\"x-ployz-approval\"",
        "\"x-ployz-approval\"",
    ),
    (
        "REQUEST_KEY_HEADER",
        "\"x-ployz-request-key\"",
        "\"x-ployz-request-key\"",
    ),
    ("AGENT_HEADER", "\"x-ployz-agent\"", "\"x-ployz-agent\""),
    (
        "ERROR_CODE_HEADER",
        "\"Ployz-Error-Code\"",
        "\"Ployz-Error-Code\"",
    ),
    ("CLIENT_ID", "\"ployz-cli\"", "\"ployz-cli\""),
    ("MANIFEST_FORMAT", "1", "1"),
    (
        "INGRESS_VERIFY_PATH",
        "\"/.ployz-verify\"",
        "\"/.ployz-verify\"",
    ),
    (
        "HOSTNAME_VERIFY_PATH",
        "\"/.well-known/acme-challenge/ployz-verify\"",
        "\"/.well-known/acme-challenge/ployz-verify\"",
    ),
    ("MIN_MANAGED_VOLUME_BYTES", "100000000", "100000000"),
    ("MAX_LIFETIME_MILLIS", "900000", "900000"),
    ("CLOCK_SKEW_MILLIS", "120000", "120000"),
];

/// The generated text is what Cloud's TypeScript imports, so its bytes are the contract.
#[test]
fn cloud_contract_files_spell_every_constant_in_all_three_module_forms() {
    let declarations: String = CLOUD_CONTRACT
        .iter()
        .map(|(name, declared, _)| format!("export declare const {name}: {declared};\n"))
        .collect();
    let esm: String = CLOUD_CONTRACT
        .iter()
        .map(|(name, _, value)| format!("export const {name} = {value};\n"))
        .collect();
    let cjs: String = CLOUD_CONTRACT
        .iter()
        .map(|(name, _, value)| format!("exports.{name} = {value};\n"))
        .collect();
    assert_eq!(
        file("cloud-contract.d.ts"),
        format!("{HEADER}{declarations}")
    );
    assert_eq!(file("cloud-contract.mjs"), format!("{HEADER}{esm}"));
    assert_eq!(
        file("cloud-contract.js"),
        format!("{HEADER}\"use strict\";\n\n{cjs}")
    );
}
