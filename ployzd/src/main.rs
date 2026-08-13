#[cfg(not(target_os = "linux"))]
compile_error!("ployzd supports Linux only");

fn main() {}
