//! Writes the SDK package's Rust-derived `generated/` files into the directory given as the one argument.

use std::{env, fs, io, path::PathBuf};

fn main() -> io::Result<()> {
    let Some(dir) = env::args_os().nth(1).map(PathBuf::from) else {
        return Err(io::Error::other("usage: sdk-generated <dir>"));
    };
    fs::create_dir_all(&dir)?;
    for (name, contents) in ployz::sdk::generated_files() {
        fs::write(dir.join(name), contents)?;
    }
    Ok(())
}
