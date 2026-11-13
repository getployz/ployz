//! Prints every coverage case as one JSON array, for the live attacker harness.

fn main() {
    let cases = serde_json::to_string_pretty(ployz_core::authority::coverage::CASES)
        .expect("coverage cases are JSON");
    #[expect(clippy::print_stdout, reason = "the example's output is the JSON")]
    {
        println!("{cases}");
    }
}
