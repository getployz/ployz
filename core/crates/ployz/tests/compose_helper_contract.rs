use ployz_core::RequestedServiceSpec;
use std::collections::BTreeMap;

#[test]
fn helper_emits_the_existing_requested_service_contract() {
    let directory =
        std::env::temp_dir().join(format!("ployz-helper-contract-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("Caddyfile"),
        "app.example { reverse_proxy :80 }\n",
    )
    .unwrap();
    std::fs::write(directory.join("app.conf"), "enabled=true\n").unwrap();
    let yaml = include_str!("fixtures/compose-helper/compose.yaml");
    // Captured from the Rust adapter before replacement; compare values, not just decoding.
    let expected: BTreeMap<String, RequestedServiceSpec> =
        serde_json::from_str(include_str!("fixtures/compose-helper/requested.json")).unwrap();
    let actual = ployz::compose::parse_normalized(yaml, &directory).unwrap();
    assert_eq!(actual.services, expected);
    std::fs::remove_dir_all(directory).unwrap();
}
