const RAILPACK_PINS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../config/railpack-pins.env"
));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailpackDigestKind {
    Archive,
    Binary,
    Frontend,
}

impl std::fmt::Display for RailpackDigestKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Archive => "archive",
            Self::Binary => "binary",
            Self::Frontend => "frontend",
        };
        formatter.write_str(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RailpackPinError {
    #[error("Railpack pin line {line} has no '='")]
    MalformedLine { line: usize },
    #[error("Railpack pin {key} is empty")]
    Empty { key: String },
    #[error("unknown Railpack pin {key}")]
    Unknown { key: String },
    #[error("duplicate Railpack pin {key}")]
    Duplicate { key: String },
    #[error("missing Railpack pin {key}")]
    Missing { key: &'static str },
    #[error("Railpack {architecture} archive must be {expected}")]
    MalformedArchive {
        architecture: String,
        expected: String,
    },
    #[error("Railpack {architecture} frontend digest is not a SHA-256 digest")]
    MalformedFrontendDigest { architecture: String },
    #[error("Railpack {architecture} {kind} SHA-256 is not 64 lowercase hex characters")]
    MalformedDigest {
        architecture: String,
        kind: RailpackDigestKind,
    },
}

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

pub fn railpack_pins() -> Result<RailpackPins<'static>, RailpackPinError> {
    parse_railpack_pins(RAILPACK_PINS)
}

fn parse_railpack_pins(contents: &str) -> Result<RailpackPins<'_>, RailpackPinError> {
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
            return Err(RailpackPinError::MalformedLine { line: index + 1 });
        };
        if value.is_empty() {
            return Err(RailpackPinError::Empty {
                key: key.to_owned(),
            });
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
            _ => {
                return Err(RailpackPinError::Unknown {
                    key: key.to_owned(),
                });
            }
        };
        if slot.replace(value).is_some() {
            return Err(RailpackPinError::Duplicate {
                key: key.to_owned(),
            });
        }
    }

    let version = required_pin(version, "RAILPACK_VERSION")?;
    Ok(RailpackPins {
        version,
        install_path: required_pin(install_path, "RAILPACK_INSTALL_PATH")?,
        frontend_reference: required_pin(frontend_reference, "RAILPACK_FRONTEND_REFERENCE")?,
        amd64: platform_pins(
            "amd64",
            "x86_64",
            version,
            required_pin(amd64_archive, "RAILPACK_AMD64_ARCHIVE")?,
            required_pin(amd64_archive_sha256, "RAILPACK_AMD64_ARCHIVE_SHA256")?,
            required_pin(amd64_binary_sha256, "RAILPACK_AMD64_BINARY_SHA256")?,
            required_pin(amd64_frontend_digest, "RAILPACK_AMD64_FRONTEND_DIGEST")?,
        )?,
        arm64: platform_pins(
            "arm64",
            "arm64",
            version,
            required_pin(arm64_archive, "RAILPACK_ARM64_ARCHIVE")?,
            required_pin(arm64_archive_sha256, "RAILPACK_ARM64_ARCHIVE_SHA256")?,
            required_pin(arm64_binary_sha256, "RAILPACK_ARM64_BINARY_SHA256")?,
            required_pin(arm64_frontend_digest, "RAILPACK_ARM64_FRONTEND_DIGEST")?,
        )?,
    })
}

fn required_pin<'a>(
    value: Option<&'a str>,
    key: &'static str,
) -> Result<&'a str, RailpackPinError> {
    value.ok_or(RailpackPinError::Missing { key })
}

fn platform_pins<'a>(
    architecture: &str,
    archive_architecture: &str,
    version: &str,
    archive: &'a str,
    archive_sha256: &'a str,
    binary_sha256: &'a str,
    frontend_digest: &'a str,
) -> Result<RailpackPlatformPins<'a>, RailpackPinError> {
    let expected_archive =
        format!("railpack-{version}-{archive_architecture}-unknown-linux-musl.tar.gz");
    if archive != expected_archive {
        return Err(RailpackPinError::MalformedArchive {
            architecture: architecture.to_owned(),
            expected: expected_archive,
        });
    }
    validate_sha256(architecture, RailpackDigestKind::Archive, archive_sha256)?;
    validate_sha256(architecture, RailpackDigestKind::Binary, binary_sha256)?;
    let Some(frontend_sha256) = frontend_digest.strip_prefix("sha256:") else {
        return Err(RailpackPinError::MalformedFrontendDigest {
            architecture: architecture.to_owned(),
        });
    };
    validate_sha256(architecture, RailpackDigestKind::Frontend, frontend_sha256)?;
    Ok(RailpackPlatformPins {
        archive,
        archive_sha256,
        binary_sha256,
        frontend_digest,
    })
}

fn validate_sha256(
    architecture: &str,
    kind: RailpackDigestKind,
    digest: &str,
) -> Result<(), RailpackPinError> {
    if digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(RailpackPinError::MalformedDigest {
            architecture: architecture.to_owned(),
            kind,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_pins_are_complete_typed_and_shell_safe() {
        let pins = railpack_pins().expect("checked-in Railpack pins");
        assert_eq!(pins.version(), "v0.31.0");
        assert_eq!(
            pins.install_path(),
            "/usr/local/lib/ployz/railpack/v0.31.0/railpack"
        );
        assert_eq!(
            pins.frontend_reference(),
            "ghcr.io/railwayapp/railpack-frontend"
        );
        let amd64 = pins.for_architecture("amd64").expect("amd64 pins");
        assert_eq!(
            amd64.archive(),
            "railpack-v0.31.0-x86_64-unknown-linux-musl.tar.gz"
        );
        assert_eq!(amd64.archive_sha256().len(), 64);
        assert_eq!(amd64.binary_sha256().len(), 64);
        assert!(
            RAILPACK_PINS
                .lines()
                .all(|line| !line.contains(char::is_whitespace))
        );
    }

    #[test]
    fn parser_returns_typed_configuration_errors() {
        let duplicate = format!("{RAILPACK_PINS}RAILPACK_VERSION=v0.31.0\n");
        assert_eq!(
            parse_railpack_pins(&duplicate),
            Err(RailpackPinError::Duplicate {
                key: "RAILPACK_VERSION".to_owned(),
            })
        );
        let missing = RAILPACK_PINS.replace("RAILPACK_VERSION=v0.31.0\n", "");
        assert_eq!(
            parse_railpack_pins(&missing),
            Err(RailpackPinError::Missing {
                key: "RAILPACK_VERSION",
            })
        );
        assert_eq!(
            parse_railpack_pins("RAILPACK_VERSION"),
            Err(RailpackPinError::MalformedLine { line: 1 })
        );
        assert_eq!(
            parse_railpack_pins("UNRECOGNIZED=value"),
            Err(RailpackPinError::Unknown {
                key: "UNRECOGNIZED".to_owned(),
            })
        );
        assert_eq!(
            parse_railpack_pins("RAILPACK_VERSION="),
            Err(RailpackPinError::Empty {
                key: "RAILPACK_VERSION".to_owned(),
            })
        );
        let bad_archive = RAILPACK_PINS.replace(
            "railpack-v0.31.0-x86_64-unknown-linux-musl.tar.gz",
            "railpack-unexpected.tar.gz",
        );
        let archive_error = parse_railpack_pins(&bad_archive).expect_err("archive is malformed");
        assert!(matches!(
            archive_error,
            RailpackPinError::MalformedArchive { ref architecture, .. }
                if architecture == "amd64"
        ));
        assert_eq!(
            archive_error.to_string(),
            "Railpack amd64 archive must be railpack-v0.31.0-x86_64-unknown-linux-musl.tar.gz"
        );
        assert!(!archive_error.to_string().contains("railpack-unexpected"));
        let bad_sha = RAILPACK_PINS.replace(
            "f75416cf4c452db2841d864f54dbfd8e4d77f2d4a02b23b87561e7760fa278fd",
            "not-a-sha",
        );
        assert_eq!(
            parse_railpack_pins(&bad_sha),
            Err(RailpackPinError::MalformedDigest {
                architecture: "amd64".to_owned(),
                kind: RailpackDigestKind::Archive,
            })
        );
        let bad_frontend = RAILPACK_PINS.replacen("sha256:", "sha512:", 1);
        assert_eq!(
            parse_railpack_pins(&bad_frontend),
            Err(RailpackPinError::MalformedFrontendDigest {
                architecture: "amd64".to_owned(),
            })
        );
    }
}
