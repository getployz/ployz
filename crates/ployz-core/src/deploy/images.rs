//! Registry image references and validation.

use std::time::Duration;

use super::*;

pub use crate::image::{
    ImageContentLeaseExpiresAt, OciDigest, OciPlatform, RegistryCredential,
    RegistryCredentialError, RegistryCredentialSecret, RegistryCredentialUsername,
};

/// Clock and cleanup margin removed from machine content-lease availability.
pub const IMAGE_AVAILABILITY_SAFETY_MARGIN: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PlatformImage {
    pub seed: MachineId,
    pub manifest_digest: OciDigest,
    pub image_id: OciDigest,
    pub availability_expires_at: ImageAvailabilityExpiresAt,
}

positive_u64_wire_newtype! {
    /// The last Unix second before which a pushed platform is advertised as available.
    pub struct ImageAvailabilityExpiresAt;
    ts_brand: "Brand<string, \"ImageAvailabilityExpiresAt\">";
    accessor: unix_seconds;
    error: ImageAvailabilityTimestampError;
}

positive_u64_wire_error! {
    pub enum ImageAvailabilityTimestampError;
    noun: "image availability timestamp";
}

impl ImageAvailabilityExpiresAt {
    pub fn from_content_lease_expiry(
        lease_expires_at: ImageContentLeaseExpiresAt,
    ) -> Result<Self, ImageAvailabilityTimestampError> {
        Self::try_new(
            lease_expires_at
                .unix_seconds()
                .saturating_sub(IMAGE_AVAILABILITY_SAFETY_MARGIN.as_secs()),
        )
    }

    #[must_use]
    pub const fn is_expired_at(self, now_unix_seconds: u64) -> bool {
        self.unix_seconds() <= now_unix_seconds
    }
}

/// A validated pushed-image receipt whose identity describes replacement-relevant
/// platform content, independently of the machines currently serving it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(try_from = "PushedImageReceiptWire", into = "PushedImageReceiptWire")]
pub struct PushedImageReceipt {
    index_digest: OciDigest,
    #[cfg_attr(feature = "typescript", ts(type = "[OciPlatform, PlatformImage][]"))]
    platforms: BTreeMap<OciPlatform, PlatformImage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PushedImageReceiptWire {
    index_digest: OciDigest,
    platforms: Vec<(OciPlatform, PlatformImage)>,
}

impl TryFrom<PushedImageReceiptWire> for PushedImageReceipt {
    type Error = PushedImageReceiptError;

    fn try_from(value: PushedImageReceiptWire) -> Result<Self, Self::Error> {
        let PushedImageReceiptWire {
            index_digest,
            platforms,
        } = value;
        let receipt = Self::try_new(platforms)?;
        if receipt.index_digest != index_digest {
            return Err(PushedImageReceiptError::IndexDigestMismatch {
                expected: receipt.index_digest,
                actual: index_digest,
            });
        }
        Ok(receipt)
    }
}

impl From<PushedImageReceipt> for PushedImageReceiptWire {
    fn from(value: PushedImageReceipt) -> Self {
        let PushedImageReceipt {
            index_digest,
            platforms,
        } = value;
        Self {
            index_digest,
            platforms: platforms.into_iter().collect(),
        }
    }
}

impl PushedImageReceipt {
    const INDEX_ENCODING_VERSION: &'static str = "ployz.pushed_image_receipt_index.v1";

    pub fn try_new(
        entries: impl IntoIterator<Item = (OciPlatform, PlatformImage)>,
    ) -> Result<Self, PushedImageReceiptError> {
        let mut platforms = BTreeMap::new();
        for (platform, image) in entries {
            if platforms.insert(platform.clone(), image).is_some() {
                return Err(PushedImageReceiptError::DuplicatePlatform { platform });
            }
        }
        if platforms.is_empty() {
            return Err(PushedImageReceiptError::Empty);
        }
        let index_digest = receipt_index_digest(&platforms);
        Ok(Self {
            index_digest,
            platforms,
        })
    }

    #[must_use]
    pub fn index_digest(&self) -> &OciDigest {
        &self.index_digest
    }

    #[must_use]
    pub fn platforms(&self) -> impl ExactSizeIterator<Item = (&OciPlatform, &PlatformImage)> {
        self.platforms.iter()
    }

    #[must_use]
    pub fn platform(&self, platform: &OciPlatform) -> Option<&PlatformImage> {
        self.platforms.get(platform)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PushedImageReceiptError {
    #[error("pushed image receipt must contain at least one platform")]
    Empty,
    #[error(
        "pushed image receipt contains duplicate platform {}/{}",
        platform.os(),
        platform.architecture()
    )]
    DuplicatePlatform { platform: OciPlatform },
    #[error("pushed image receipt index digest mismatch: expected {expected}, got {actual}")]
    IndexDigestMismatch {
        expected: OciDigest,
        actual: OciDigest,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
enum ImageSourceWire {
    Registry,
    PushedToSeed {
        index_digest: OciDigest,
        platforms: Vec<(OciPlatform, PlatformImage)>,
    },
}

impl TryFrom<ImageSourceWire> for ImageSource {
    type Error = PushedImageReceiptError;

    fn try_from(value: ImageSourceWire) -> Result<Self, Self::Error> {
        match value {
            ImageSourceWire::Registry => Ok(Self::Registry),
            ImageSourceWire::PushedToSeed {
                index_digest,
                platforms,
            } => PushedImageReceipt::try_from(PushedImageReceiptWire {
                index_digest,
                platforms,
            })
            .map(Self::PushedToSeed),
        }
    }
}

impl From<ImageSource> for ImageSourceWire {
    fn from(value: ImageSource) -> Self {
        match value {
            ImageSource::Registry => Self::Registry,
            ImageSource::PushedToSeed(receipt) => {
                let receipt = PushedImageReceiptWire::from(receipt);
                Self::PushedToSeed {
                    index_digest: receipt.index_digest,
                    platforms: receipt.platforms,
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(
        type = "{ source: \"registry\" } | { source: \"pushed_to_seed\", index_digest: OciDigest, platforms: [OciPlatform, PlatformImage][] }"
    )
)]
#[serde(try_from = "ImageSourceWire", into = "ImageSourceWire")]
pub enum ImageSource {
    Registry,
    PushedToSeed(PushedImageReceipt),
}

impl ImageSource {
    #[must_use]
    pub const fn is_registry(&self) -> bool {
        matches!(self, Self::Registry)
    }
}

pub fn validate_pushed_image_reference(
    image: &ImageReference,
    image_source: &ImageSource,
) -> Result<(), PushedImageReferenceError> {
    let ImageSource::PushedToSeed(receipt) = image_source else {
        return Ok(());
    };
    let Some(pinned_digest) = image.pinned_digest() else {
        return Err(PushedImageReferenceError::NotDigestPinned);
    };
    if &pinned_digest != receipt.index_digest() {
        return Err(PushedImageReferenceError::IndexDigestMismatch);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PushedImageReferenceError {
    #[error("must be digest-pinned")]
    NotDigestPinned,
    #[error("digest must match its pushed index digest")]
    IndexDigestMismatch,
}

/// Apply a concrete pushed receipt's native-platform coverage to placement.
/// Registry and preview-pending images do not use this policy.
#[must_use]
pub fn classify_image_platform_usability(
    candidates: &[MachineId],
    machine_platforms: &BTreeMap<MachineId, OciPlatform>,
    image_source: &ImageSource,
) -> (Vec<MachineId>, Vec<crate::operation::UnusableMachine>) {
    let ImageSource::PushedToSeed(receipt) = image_source else {
        return (candidates.to_vec(), Vec::new());
    };
    let supported = crate::build::BuildPlatforms::try_new(
        receipt.platforms().map(|(platform, _)| platform.clone()),
    )
    .expect("validated pushed receipts contain at least one unique platform");
    let mut eligible = Vec::new();
    let mut unusable = Vec::new();
    for machine_id in candidates {
        let Some(reported) = machine_platforms.get(machine_id) else {
            unusable.push(crate::operation::UnusableMachine {
                machine_id: machine_id.clone(),
                reason: crate::machine::MachineUsabilityReason::FactsUnavailable,
            });
            continue;
        };
        if receipt.platform(reported).is_some() {
            eligible.push(machine_id.clone());
        } else {
            unusable.push(crate::operation::UnusableMachine {
                machine_id: machine_id.clone(),
                reason: crate::machine::MachineUsabilityReason::PlatformMismatch {
                    supported: supported.clone(),
                    reported: reported.clone(),
                },
            });
        }
    }
    (eligible, unusable)
}

fn receipt_index_digest(platforms: &BTreeMap<OciPlatform, PlatformImage>) -> OciDigest {
    let mut hasher = Sha256::new();
    receipt_index_frame(
        &mut hasher,
        "version",
        PushedImageReceipt::INDEX_ENCODING_VERSION.as_bytes(),
    );
    for (platform, image) in platforms {
        receipt_index_frame(&mut hasher, "platform_os", platform.os().as_bytes());
        receipt_index_frame(
            &mut hasher,
            "platform_architecture",
            platform.architecture().as_bytes(),
        );
        receipt_index_frame(
            &mut hasher,
            "manifest_digest",
            image.manifest_digest.as_str().as_bytes(),
        );
        receipt_index_frame(&mut hasher, "image_id", image.image_id.as_str().as_bytes());
    }
    OciDigest::try_new(format!("sha256:{:x}", hasher.finalize()))
        .expect("sha256 is a canonical OCI digest")
}

fn receipt_index_frame(hasher: &mut Sha256, label: &str, value: &[u8]) {
    hasher.update((label.len() as u64).to_be_bytes());
    hasher.update(label.as_bytes());
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
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
        assert_eq!(
            encoded.get("source"),
            Some(&serde_json::json!("pushed_to_seed"))
        );
        assert!(encoded.get("index_digest").is_some());
        assert!(
            encoded
                .get("platforms")
                .is_some_and(serde_json::Value::is_array)
        );
        assert!(encoded.get("receipt").is_none());
        assert_eq!(
            serde_json::from_value::<ImageSource>(encoded).expect("receipt deserializes"),
            source
        );
    }

    #[test]
    fn pushed_receipt_rejects_empty_and_duplicate_platforms() {
        let source = pushed_source();
        let mut encoded = serde_json::to_value(source).expect("receipt serializes");
        let Some(platforms) = encoded.get_mut("platforms") else {
            panic!("receipt wire includes platforms");
        };
        *platforms = serde_json::json!([]);
        assert!(serde_json::from_value::<ImageSource>(encoded.clone()).is_err());

        let duplicate = serde_json::json!([
            [platform("amd64"), platform_image('b')],
            [platform("amd64"), platform_image('c')]
        ]);
        let Some(platforms) = encoded.get_mut("platforms") else {
            panic!("receipt wire includes platforms");
        };
        *platforms = duplicate;
        assert!(serde_json::from_value::<ImageSource>(encoded).is_err());
    }

    #[test]
    fn pushed_receipt_constructor_rejects_empty_and_duplicate_platforms() {
        assert_eq!(
            PushedImageReceipt::try_new([]),
            Err(PushedImageReceiptError::Empty)
        );
        let duplicate = platform("amd64");
        assert_eq!(
            PushedImageReceipt::try_new([
                (duplicate.clone(), platform_image('b')),
                (duplicate.clone(), platform_image('c')),
            ]),
            Err(PushedImageReceiptError::DuplicatePlatform {
                platform: duplicate
            })
        );
    }

    #[test]
    fn pushed_receipt_rejects_forged_index_digest() {
        let mut encoded = serde_json::to_value(pushed_source()).expect("receipt serializes");
        let Some(image_id) = encoded.pointer_mut("/platforms/0/1/image_id") else {
            panic!("receipt wire includes the platform image id");
        };
        *image_id = serde_json::json!(digest('d'));

        assert!(matches!(
            serde_json::from_value::<ImageSource>(encoded),
            Err(error) if error.to_string().contains("index digest mismatch")
        ));
    }

    #[test]
    fn pushed_receipt_requires_a_valid_platform_availability_deadline() {
        let mut encoded = serde_json::to_value(pushed_source()).expect("receipt serializes");
        let Some(expires_at) = encoded.pointer_mut("/platforms/0/1/availability_expires_at") else {
            panic!("receipt wire includes availability expiry");
        };
        *expires_at = serde_json::json!("0");

        assert!(serde_json::from_value::<ImageSource>(encoded).is_err());
    }

    #[test]
    fn advertised_availability_precedes_the_machine_content_lease() {
        let lease = ImageContentLeaseExpiresAt::try_new(4_000).expect("lease expiry");

        let advertised = ImageAvailabilityExpiresAt::from_content_lease_expiry(lease)
            .expect("advertised expiry");

        assert_eq!(advertised.unix_seconds(), 3_700);
        assert!(!advertised.is_expired_at(3_699));
        assert!(advertised.is_expired_at(3_700));
    }

    #[test]
    fn pushed_receipt_index_is_sorted_and_excludes_seed_and_availability() {
        let amd64 = platform("amd64");
        let arm64 = platform("arm64");
        let left = PushedImageReceipt::try_new([
            (arm64.clone(), platform_image('b')),
            (amd64.clone(), platform_image('c')),
        ])
        .expect("receipt");
        let mut relocated_arm = platform_image('b');
        relocated_arm.seed = MachineId::try_new("machine_relocated").expect("machine id");
        relocated_arm.availability_expires_at =
            ImageAvailabilityExpiresAt::try_new(9_999).expect("expiry");
        let right =
            PushedImageReceipt::try_new([(amd64, platform_image('c')), (arm64, relocated_arm)])
                .expect("receipt");

        assert_eq!(left.index_digest(), right.index_digest());
        assert_eq!(
            left.index_digest().as_str(),
            "sha256:d66f93797a558cc78021e66cba67c1e1212f082847582dd283a613f2d4d96308"
        );
    }

    #[test]
    fn pushed_receipt_eligibility_reports_the_complete_supported_set() {
        let amd64 = platform("amd64");
        let arm64 = platform("arm64");
        let reported = platform("s390x");
        let machine = MachineId::try_new("machine_target").expect("machine id");
        let source = ImageSource::PushedToSeed(
            PushedImageReceipt::try_new([
                (arm64.clone(), platform_image('b')),
                (amd64.clone(), platform_image('c')),
            ])
            .expect("receipt"),
        );

        let (eligible, unusable) = classify_image_platform_usability(
            std::slice::from_ref(&machine),
            &BTreeMap::from([(machine.clone(), reported.clone())]),
            &source,
        );

        assert!(eligible.is_empty());
        let [unusable] = unusable.as_slice() else {
            panic!("one machine is unusable");
        };
        assert!(matches!(
            &unusable.reason,
            crate::machine::MachineUsabilityReason::PlatformMismatch { supported, reported: actual }
                if supported.iter().cloned().collect::<Vec<_>>() == [amd64, arm64]
                    && actual == &reported
        ));
    }

    #[test]
    fn pushed_image_reference_must_pin_the_receipt_index() {
        let source = pushed_source();
        let ImageSource::PushedToSeed(receipt) = &source else {
            panic!("pushed fixture");
        };
        let unpinned = ImageReference::try_new("local/api:build").expect("image");
        let pinned = unpinned
            .clone()
            .with_digest(receipt.index_digest())
            .expect("pinned image");

        assert_eq!(
            validate_pushed_image_reference(&unpinned, &source),
            Err(PushedImageReferenceError::NotDigestPinned)
        );
        assert_eq!(validate_pushed_image_reference(&pinned, &source), Ok(()));
    }

    fn pushed_source() -> ImageSource {
        ImageSource::PushedToSeed(
            PushedImageReceipt::try_new([(platform("amd64"), platform_image('b'))])
                .expect("receipt"),
        )
    }

    fn platform(architecture: &str) -> OciPlatform {
        OciPlatform::try_new("linux", architecture).expect("platform")
    }

    fn platform_image(value: char) -> PlatformImage {
        PlatformImage {
            seed: MachineId::try_new(format!("machine_{value}")).expect("machine id"),
            manifest_digest: digest(value),
            image_id: digest(value),
            availability_expires_at: ImageAvailabilityExpiresAt::try_new(4_000).expect("expiry"),
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
