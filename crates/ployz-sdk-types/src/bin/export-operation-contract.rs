fn main() {
    println!(
        "{}",
        serde_json::to_string_pretty(&ployz_sdk_types::typescript::operation_contract_fixture())
            .expect("operation contract fixture serializes")
    );
}
