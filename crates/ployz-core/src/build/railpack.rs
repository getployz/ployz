const RAILPACK_PINS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../config/railpack-pins.env"
));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RailpackPins<'a> {
    version: &'a str,
    install_path: &'a str,
    frontend_reference: &'a str,
    amd64: RailpackPlatformPins<'a>,
    arm64: RailpackPlatformPins<'a>,
}

impl<'a> RailpackPins<'a> {
    #[must_use]
    pub const fn version(&self) -> &'a str {
        self.version
    }

    #[must_use]
    pub const fn install_path(&self) -> &'a str {
        self.install_path
    }

    #[must_use]
    pub const fn frontend_reference(&self) -> &'a str {
        self.frontend_reference
    }

    #[must_use]
    pub fn for_architecture(&self, architecture: &str) -> Option<RailpackPlatformPins<'a>> {
        match architecture {
            "amd64" => Some(self.amd64),
            "arm64" => Some(self.arm64),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RailpackPlatformPins<'a> {
    archive: &'a str,
    archive_sha256: &'a str,
    binary_sha256: &'a str,
    frontend_digest: &'a str,
}

impl<'a> RailpackPlatformPins<'a> {
    #[must_use]
    pub const fn archive(&self) -> &'a str {
        self.archive
    }

    #[must_use]
    pub const fn archive_sha256(&self) -> &'a str {
        self.archive_sha256
    }

    #[must_use]
    pub const fn binary_sha256(&self) -> &'a str {
        self.binary_sha256
    }

    #[must_use]
    pub const fn frontend_digest(&self) -> &'a str {
        self.frontend_digest
    }
}

pub fn railpack_pins() -> Result<RailpackPins<'static>, String> {
    parse_railpack_pins(RAILPACK_PINS)
}

fn parse_railpack_pins(contents: &str) -> Result<RailpackPins<'_>, String> {
    let mut version = None;
    let mut install_path = None;
    let mut frontend_reference = None;
    let mut amd64_archive = None;
    let mut amd64_archive_sha256 = None;
    let mut amd64_binary_sha256 = None;
    let mut amd64_frontend_digest = None;
    let mut arm64_archive = None;
    let mut arm64_archive_sha256 = None;
    let mut arm64_binary_sha256 = None;
    let mut arm64_frontend_digest = None;

    for (index, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("Railpack pin line {} has no '='", index + 1));
        };
        if value.is_empty() {
            return Err(format!("Railpack pin {key} is empty"));
        }
        let slot = match key {
            "RAILPACK_VERSION" => &mut version,
            "RAILPACK_INSTALL_PATH" => &mut install_path,
            "RAILPACK_FRONTEND_REFERENCE" => &mut frontend_reference,
            "RAILPACK_AMD64_ARCHIVE" => &mut amd64_archive,
            "RAILPACK_AMD64_ARCHIVE_SHA256" => &mut amd64_archive_sha256,
            "RAILPACK_AMD64_BINARY_SHA256" => &mut amd64_binary_sha256,
            "RAILPACK_AMD64_FRONTEND_DIGEST" => &mut amd64_frontend_digest,
            "RAILPACK_ARM64_ARCHIVE" => &mut arm64_archive,
            "RAILPACK_ARM64_ARCHIVE_SHA256" => &mut arm64_archive_sha256,
            "RAILPACK_ARM64_BINARY_SHA256" => &mut arm64_binary_sha256,
            "RAILPACK_ARM64_FRONTEND_DIGEST" => &mut arm64_frontend_digest,
            _ => return Err(format!("unknown Railpack pin {key}")),
        };
        if slot.replace(value).is_some() {
            return Err(format!("duplicate Railpack pin {key}"));
        }
    }

    let version = required_pin(version, "RAILPACK_VERSION")?;
    Ok(RailpackPins {
        version,
        install_path: required_pin(install_path, "RAILPACK_INSTALL_PATH")?,
        frontend_reference: required_pin(frontend_reference, "RAILPACK_FRONTEND_REFERENCE")?,
        amd64: platform_pins(
            "amd64",
            version,
            required_pin(amd64_archive, "RAILPACK_AMD64_ARCHIVE")?,
            required_pin(amd64_archive_sha256, "RAILPACK_AMD64_ARCHIVE_SHA256")?,
            required_pin(amd64_binary_sha256, "RAILPACK_AMD64_BINARY_SHA256")?,
            required_pin(amd64_frontend_digest, "RAILPACK_AMD64_FRONTEND_DIGEST")?,
        )?,
        arm64: platform_pins(
            "arm64",
            version,
            required_pin(arm64_archive, "RAILPACK_ARM64_ARCHIVE")?,
            required_pin(arm64_archive_sha256, "RAILPACK_ARM64_ARCHIVE_SHA256")?,
            required_pin(arm64_binary_sha256, "RAILPACK_ARM64_BINARY_SHA256")?,
            required_pin(arm64_frontend_digest, "RAILPACK_ARM64_FRONTEND_DIGEST")?,
        )?,
    })
}

fn required_pin<'a>(value: Option<&'a str>, key: &str) -> Result<&'a str, String> {
    value.ok_or_else(|| format!("missing Railpack pin {key}"))
}

fn platform_pins<'a>(
    architecture: &str,
    version: &str,
    archive: &'a str,
    archive_sha256: &'a str,
    binary_sha256: &'a str,
    frontend_digest: &'a str,
) -> Result<RailpackPlatformPins<'a>, String> {
    let archive_architecture = match architecture {
        "amd64" => "x86_64",
        "arm64" => "arm64",
        _ => return Err(format!("unsupported Railpack architecture {architecture}")),
    };
    let expected_archive =
        format!("railpack-{version}-{archive_architecture}-unknown-linux-musl.tar.gz");
    if archive != expected_archive {
        return Err(format!(
            "Railpack {architecture} archive must be {expected_archive}"
        ));
    }
    validate_sha256(architecture, "archive", archive_sha256)?;
    validate_sha256(architecture, "binary", binary_sha256)?;
    let Some(frontend_sha256) = frontend_digest.strip_prefix("sha256:") else {
        return Err(format!(
            "Railpack {architecture} frontend digest is not a SHA-256 digest"
        ));
    };
    validate_sha256(architecture, "frontend", frontend_sha256)?;
    Ok(RailpackPlatformPins {
        archive,
        archive_sha256,
        binary_sha256,
        frontend_digest,
    })
}

fn validate_sha256(architecture: &str, kind: &str, digest: &str) -> Result<(), String> {
    if digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(format!(
            "Railpack {architecture} {kind} SHA-256 is not 64 lowercase hex characters"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_pins_are_complete_typed_and_shell_safe() {
        let pins = railpack_pins().expect("checked-in Railpack pins");
        assert!(pins.install_path().starts_with('/'));
        let amd64 = pins.for_architecture("amd64").expect("amd64 pins");
        assert!(amd64.archive().ends_with(".tar.gz"));
        assert_eq!(amd64.archive_sha256().len(), 64);
        assert_eq!(amd64.binary_sha256().len(), 64);
        assert!(
            RAILPACK_PINS
                .lines()
                .all(|line| !line.contains(char::is_whitespace))
        );
    }

    #[test]
    fn parser_rejects_duplicate_missing_and_malformed_pins() {
        let duplicate = format!("{RAILPACK_PINS}RAILPACK_VERSION=v0.31.0\n");
        assert!(parse_railpack_pins(&duplicate).is_err());
        let missing = RAILPACK_PINS.replace("RAILPACK_VERSION=v0.31.0\n", "");
        assert!(parse_railpack_pins(&missing).is_err());
        let bad_archive = RAILPACK_PINS.replace(
            "railpack-v0.31.0-x86_64-unknown-linux-musl.tar.gz",
            "railpack-unexpected.tar.gz",
        );
        assert!(parse_railpack_pins(&bad_archive).is_err());
        let bad_sha = RAILPACK_PINS.replace(
            "f75416cf4c452db2841d864f54dbfd8e4d77f2d4a02b23b87561e7760fa278fd",
            "not-a-sha",
        );
        assert!(parse_railpack_pins(&bad_sha).is_err());
    }
}
