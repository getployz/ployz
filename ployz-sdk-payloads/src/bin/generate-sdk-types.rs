//! Write checked-in `@ployz/sdk` TypeScript and JSON fixtures from Rust types.

fn main() -> std::io::Result<()> {
    let root = ployz_sdk_payloads::sdk_package_root();
    ployz_sdk_payloads::write_generated(&root)?;
    Ok(())
}
