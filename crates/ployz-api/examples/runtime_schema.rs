use ployz_api::RuntimeWatchFrame;
use schemars::schema_for;

fn main() {
    let schema = schema_for!(RuntimeWatchFrame);
    let json = serde_json::to_string_pretty(&schema).expect("encode runtime schema");
    println!("{json}");
}
