use ployz_spec::DeployManifest;
use schemars::schema_for;

fn main() {
    let schema = schema_for!(DeployManifest);
    let json = serde_json::to_string_pretty(&schema).expect("encode deploy manifest schema");
    println!("{json}");
}
