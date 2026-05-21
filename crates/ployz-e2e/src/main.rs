mod scenarios;

fn main() {
    eprintln!(
        "ployz-e2e currently contains in-process product acceptance tests. Run `cargo test -p ployz-e2e`."
    );
    std::process::exit(2);
}
