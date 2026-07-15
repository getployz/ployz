//! Registry image references and validation.

use super::*;

pub use crate::image::{
    OciDigest, OciPlatform, RegistryCredential, RegistryCredentialError, RegistryCredentialSecret,
    RegistryCredentialUsername,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PlatformImage {
    pub seed: MachineId,
    pub manifest_digest: OciDigest,
    pub image_id: OciDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageSource {
    Registry,
    PushedToSeed {
        index_digest: OciDigest,
        #[serde(with = "platform_images_serde")]
        #[cfg_attr(feature = "typescript", ts(type = "[OciPlatform, PlatformImage][]"))]
        platforms: BTreeMap<OciPlatform, PlatformImage>,
    },
}

impl ImageSource {
    #[must_use]
    pub const fn is_registry(&self) -> bool {
        matches!(self, Self::Registry)
    }
}

mod platform_images_serde {
    use serde::de::Error as _;
    use serde::ser::SerializeSeq as _;

    use super::*;

    pub fn serialize<S>(
        platforms: &BTreeMap<OciPlatform, PlatformImage>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(platforms.len()))?;
        for entry in platforms {
            sequence.serialize_element(&entry)?;
        }
        sequence.end()
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<BTreeMap<OciPlatform, PlatformImage>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let entries = Vec::<(OciPlatform, PlatformImage)>::deserialize(deserializer)?;
        if entries.is_empty() {
            return Err(D::Error::custom(
                "pushed image receipt must contain at least one platform",
            ));
        }
        let entry_count = entries.len();
        let platforms = entries.into_iter().collect::<BTreeMap<_, _>>();
        if platforms.len() != entry_count {
            return Err(D::Error::custom(
                "pushed image receipt contains a duplicate platform",
            ));
        }
        Ok(platforms)
    }
}

pub(super) fn registry_image_source() -> ImageSource {
    ImageSource::Registry
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "Brand<string, \"ImageReference\">"))]
#[serde(try_from = "String", into = "String")]
pub struct ImageReference(String);

impl ImageReference {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ImageReferenceError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ImageReferenceError::Empty);
        }

        if value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(ImageReferenceError::InvalidCharacter { value });
        }
        let (name_and_tag, digest) = value
            .split_once('@')
            .map_or((value.as_str(), None), |(name, digest)| {
                (name, Some(digest))
            });
        if let Some(digest) = digest
            && (digest.contains('@') || OciDigest::try_new(digest).is_err())
        {
            return Err(ImageReferenceError::InvalidDigest { value });
        }
        let last_slash = name_and_tag.rfind('/');
        let (name, tag) = match name_and_tag.rfind(':') {
            Some(separator) if last_slash.is_none_or(|slash| separator > slash) => (
                &name_and_tag[..separator],
                Some(&name_and_tag[separator + 1..]),
            ),
            Some(_) | None => (name_and_tag, None),
        };
        if !valid_image_name(name) {
            return Err(ImageReferenceError::InvalidName { value });
        }
        if let Some(tag) = tag
            && !valid_image_tag(tag)
        {
            return Err(ImageReferenceError::InvalidTag { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn pinned_digest(&self) -> Option<OciDigest> {
        self.0
            .rsplit_once('@')
            .and_then(|(_, digest)| OciDigest::try_new(digest).ok())
    }

    #[must_use]
    pub fn registry(&self) -> &str {
        let name = self
            .0
            .split_once('@')
            .map_or(self.as_str(), |(name, _)| name);
        let last_slash = name.rfind('/');
        let name = match name.rfind(':') {
            Some(tag_separator) if last_slash.is_none_or(|slash| tag_separator > slash) => {
                &name[..tag_separator]
            }
            Some(_) | None => name,
        };
        let first = name.split('/').next().unwrap_or(name);
        if name.contains('/') && is_explicit_registry(first) {
            first
        } else {
            "https://index.docker.io/v1/"
        }
    }

    pub fn with_digest(&self, digest: &OciDigest) -> Result<Self, ImageReferenceError> {
        let name = self
            .0
            .split_once('@')
            .map_or(self.as_str(), |(name, _)| name);
        let last_slash = name.rfind('/');
        let name = match name.rfind(':') {
            Some(tag_separator) if last_slash.is_none_or(|slash| tag_separator > slash) => {
                &name[..tag_separator]
            }
            Some(_) | None => name,
        };
        Self::try_new(format!("{name}@{digest}"))
    }
}

impl TryFrom<String> for ImageReference {
    type Error = ImageReferenceError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ImageReference> for String {
    fn from(value: ImageReference) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImageReferenceError {
    #[error("image reference is empty")]
    Empty,
    #[error("image reference contains invalid characters: {value}")]
    InvalidCharacter { value: String },
    #[error("image reference has an invalid repository or registry name: {value}")]
    InvalidName { value: String },
    #[error("image reference has an invalid tag: {value}")]
    InvalidTag { value: String },
    #[error("image reference has an invalid digest: {value}")]
    InvalidDigest { value: String },
}

fn valid_image_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 255 {
        return false;
    }
    let mut components = name.split('/');
    let Some(first) = components.next() else {
        return false;
    };
    if name.contains('/') && is_explicit_registry(first) {
        valid_registry_domain(first) && components.all(valid_repository_component)
    } else {
        valid_repository_component(first) && components.all(valid_repository_component)
    }
}

fn is_explicit_registry(component: &str) -> bool {
    component == "localhost"
        || component.contains('.')
        || component.contains(':')
        || component.starts_with('[')
}

fn valid_registry_domain(domain: &str) -> bool {
    if let Some(ipv6) = domain.strip_prefix('[') {
        let Some((address, suffix)) = ipv6.split_once(']') else {
            return false;
        };
        if address.parse::<std::net::Ipv6Addr>().is_err() {
            return false;
        }
        return suffix.is_empty() || suffix.strip_prefix(':').is_some_and(valid_registry_port);
    }
    let (host, port) = domain
        .rsplit_once(':')
        .map_or((domain, None), |(host, port)| (host, Some(port)));
    if host.is_empty()
        || host.split('.').any(|label| {
            label.is_empty()
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                || !label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                || !label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
    {
        return false;
    }
    port.is_none_or(valid_registry_port)
}

fn valid_registry_port(port: &str) -> bool {
    !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_repository_component(component: &str) -> bool {
    let bytes = component.as_bytes();
    let mut index = 0;
    let consume_alphanumeric = |index: &mut usize| {
        let start = *index;
        while bytes
            .get(*index)
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            *index += 1;
        }
        *index > start
    };
    if !consume_alphanumeric(&mut index) {
        return false;
    }
    while index < bytes.len() {
        let Some(separator) = bytes.get(index).copied() else {
            return false;
        };
        match separator {
            b'.' => index += 1,
            b'_' => {
                index += 1;
                if bytes.get(index) == Some(&b'_') {
                    index += 1;
                }
            }
            b'-' => {
                while bytes.get(index) == Some(&b'-') {
                    index += 1;
                }
            }
            _ => return false,
        }
        if !consume_alphanumeric(&mut index) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushed_receipt_round_trips_as_ordered_platform_pairs() {
        let source = pushed_source();

        let encoded = serde_json::to_value(&source).expect("receipt serializes");
        assert!(encoded["platforms"].is_array());
        assert_eq!(
            serde_json::from_value::<ImageSource>(encoded).expect("receipt deserializes"),
            source
        );
    }

    #[test]
    fn pushed_receipt_rejects_empty_and_duplicate_platforms() {
        let source = pushed_source();
        let mut encoded = serde_json::to_value(source).expect("receipt serializes");
        encoded["platforms"] = serde_json::json!([]);
        assert!(serde_json::from_value::<ImageSource>(encoded.clone()).is_err());

        let duplicate = serde_json::json!([
            [platform("amd64"), platform_image('b')],
            [platform("amd64"), platform_image('c')]
        ]);
        encoded["platforms"] = duplicate;
        assert!(serde_json::from_value::<ImageSource>(encoded).is_err());
    }

    fn pushed_source() -> ImageSource {
        ImageSource::PushedToSeed {
            index_digest: digest('a'),
            platforms: [(platform("amd64"), platform_image('b'))]
                .into_iter()
                .collect(),
        }
    }

    fn platform(architecture: &str) -> OciPlatform {
        OciPlatform {
            os: "linux".to_owned(),
            architecture: architecture.to_owned(),
        }
    }

    fn platform_image(value: char) -> PlatformImage {
        PlatformImage {
            seed: MachineId::try_new(format!("machine_{value}")).expect("machine id"),
            manifest_digest: digest(value),
            image_id: digest(value),
        }
    }

    fn digest(value: char) -> OciDigest {
        OciDigest::try_new(format!("sha256:{}", value.to_string().repeat(64))).expect("OCI digest")
    }
}

fn valid_image_tag(tag: &str) -> bool {
    let bytes = tag.as_bytes();
    (1..=128).contains(&bytes.len())
        && bytes
            .first()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'.' | b'-'))
}

#[cfg(test)]
mod image_reference_tests {
    use super::*;

    #[test]
    fn digest_pinning_replaces_a_tag_without_mistaking_a_registry_port_for_one() {
        let digest = OciDigest::sha256(b"manifest");

        let pinned = ImageReference::try_new("registry.example:5000/team/api:latest")
            .expect("valid tagged image")
            .with_digest(&digest)
            .expect("digest-pinned image");

        assert_eq!(
            pinned.as_str(),
            format!("registry.example:5000/team/api@{digest}")
        );
        assert_eq!(pinned.pinned_digest(), Some(digest));
    }

    #[test]
    fn image_reference_validates_docker_name_and_tag_grammar() {
        for value in [
            "nginx",
            "redis:7",
            "ghcr.io/acme/api:Rev_1",
            "registry.example:5000/team/api:latest",
            "localhost/team/api",
            "[::1]:5000/team/api",
        ] {
            ImageReference::try_new(value).unwrap_or_else(|error| panic!("{value}: {error}"));
        }

        for value in [
            "repo:",
            "/repo",
            "repo/",
            "repo//child",
            "Repo/image",
            "ghcr.io/Acme/api",
            "repo:bad+tag",
            "bad_host.example/repo",
            "[not-ipv6]:5000/repo",
        ] {
            assert!(
                ImageReference::try_new(value).is_err(),
                "{value} must be rejected"
            );
        }
    }

    #[test]
    fn registry_detection_distinguishes_a_tag_from_an_explicit_registry_port() {
        assert_eq!(
            ImageReference::try_new("redis:7")
                .expect("valid tagged image")
                .registry(),
            "https://index.docker.io/v1/"
        );
        assert_eq!(
            ImageReference::try_new("registry.example:5000/team/api:7")
                .expect("valid explicit registry image")
                .registry(),
            "registry.example:5000"
        );
    }
}

#[cfg(test)]
mod registry_credential_tests {
    use super::*;

    #[test]
    fn credential_variants_validate_and_redact_secret_fields() {
        let basic = RegistryCredential::try_basic("alice", "s3cr3t").expect("valid basic auth");
        let token =
            RegistryCredential::try_identity_token("identity-token").expect("valid token auth");

        assert!(!format!("{basic:?}").contains("s3cr3t"));
        assert!(!format!("{token:?}").contains("identity-token"));
        assert!(
            serde_json::from_value::<RegistryCredential>(serde_json::json!({
                "kind": "basic",
                "username": "",
                "password": "password"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<RegistryCredential>(serde_json::json!({
                "kind": "identity_token",
                "token": ""
            }))
            .is_err()
        );
    }
}
