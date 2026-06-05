use ployzd::role::parse_role_args;

fn main() {
    match parse_role_args(std::env::args().skip(1)) {
        Ok(role) => {
            println!("ployzd role: {}", role.process_name());
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}
