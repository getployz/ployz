fn main() {
    println!(
        "{}",
        serde_json::to_string_pretty(&ployz_core::api_operation_contract_fixture())
            .expect("operation contract fixture serializes")
    );
}
