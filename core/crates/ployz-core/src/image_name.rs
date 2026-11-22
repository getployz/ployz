//! How the client and every daemon read an image name. A retention name the client writes,
//! the pinned reference a destination pulls and the tag a lookup matches must agree, so
//! they come from here.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{ValueError, is_lower_hex, value::validated_string_newtype};

/// What follows a repository in the tag that keeps a pinned image from being pruned. A
/// daemon of another version reads these tags, so the text does not change.
pub const RETENTION_TAG: &str = ":ployz-sha256-";

/// An image reference split where Docker splits it: the name before its first `@`, and the
/// digest after. Docker allows one `@`, so a digest that holds another names no content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageRef<'image> {
    /// The repository and tag, as written.
    pub name: &'image str,
    /// The text after the first `@`, unchecked.
    pub digest: Option<&'image str>,
}

impl<'image> ImageRef<'image> {
    #[must_use]
    pub fn new(image: &'image str) -> Self {
        match image.split_once('@') {
            Some((name, digest)) => Self {
                name,
                digest: Some(digest),
            },
            None => Self {
                name: image,
                digest: None,
            },
        }
    }

    /// The text that names this reference's content, unchecked: the digest after a
    /// nonempty name, or the whole reference when it is a bare `sha256:` image ID.
    #[must_use]
    pub fn content_text(self) -> Option<&'image str> {
        match self.digest {
            Some(digest) if !self.name.is_empty() => Some(digest),
            Some(_) => None,
            None => self.name.starts_with("sha256:").then_some(self.name),
        }
    }

    /// The content this reference names: `name@sha256:<64 lowercase hex>`, or a bare
    /// `sha256:<64 lowercase hex>` image ID. Any other reference resolves through a tag the
    /// image store's writers choose.
    #[must_use]
    pub fn content(self) -> Option<ImageDigest> {
        ImageDigest::parse(self.content_text()?).ok()
    }
}

/// `image` without its tag or digest, as written, registry and port kept:
/// `registry:5000/api` of `registry:5000/api:v1@sha256:…`.
#[must_use]
pub fn image_repository(image: &str) -> &str {
    let name = ImageRef::new(image).name;
    match name.rsplit_once(':') {
        Some((repository, tag)) if !tag.contains('/') => repository,
        _ => name,
    }
}

/// The tag a pinned `image` is kept under, `repository:ployz-sha256-<hex>`. A pin that also
/// names a tag (`redis:7-alpine@sha256:…`) drops it, since a Docker reference holds one tag.
/// A daemon prunes only these names, and a drain delivers a copied image under one.
#[must_use]
pub fn retention_tag(image: &str) -> Option<String> {
    let hex = ImageRef::new(image).digest?.strip_prefix("sha256:")?;
    Some(format!("{}{RETENTION_TAG}{hex}", image_repository(image)))
}

/// The content retention name `tag` keeps, or `None` when `tag` only looks like one: an
/// ordinary tag such as `repo:ployz-sha256-staging` is not a retention name.
#[must_use]
pub fn retained_digest(tag: &str) -> Option<ImageDigest> {
    let (_, hex) = tag.split_once(RETENTION_TAG)?;
    ImageDigest::parse(format!("sha256:{hex}")).ok()
}

/// Whether two tags name the same thing once Docker fills in what each leaves out:
/// `app:v1` is `docker.io/library/app:v1`, and `app` is `app:latest`. `team/app:v1` and
/// `ghcr.io/app:v1` are other repositories. A name Docker can't parse matches nothing.
#[must_use]
pub fn same_image_name(left: &str, right: &str) -> bool {
    matches!((whole_image_name(left), whole_image_name(right)), (Some(left), Some(right)) if left == right)
}

/// `name` as Docker reads it, so two spellings of one reference are one string: Docker
/// Hub's host and `library/` filled in, and `latest` when it names no tag or digest.
/// `None` when Docker can't parse it.
#[must_use]
pub fn whole_image_name(name: &str) -> Option<String> {
    name.parse::<oci_spec::distribution::Reference>()
        .ok()
        .map(|reference| reference.whole())
}

/// Whether `value` is an image reference Docker can pull, like `nginx:1.27` or `ghcr.io/acme/web@sha256:…`.
/// The Store and the dashboard both check this one rule.
// ponytail: oci-spec's parser refuses an IPv6-literal registry (`[::1]:5000/api`) that Docker accepts; widen here if anyone needs one.
#[must_use]
pub fn is_image_reference(value: &str) -> bool {
    value.parse::<oci_spec::distribution::Reference>().is_ok()
}

validated_string_newtype!(
    /// An image reference pinned to a SHA-256 digest, preserving its raw repository path.
    ImageDigestReference, "image digest reference", "an image repository pinned to a SHA-256 digest",
    |value| value.parse::<oci_spec::distribution::Reference>().is_ok_and(|reference| {
        reference.digest().and_then(|digest| digest.strip_prefix("sha256:")).is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    })
);

/// An image Ployz runs on its own, such as a helper container or BuildKit, named by
/// its content. A Machine's image store holds tags anyone with an image route can
/// write, so a tag cannot say what one of these runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PinnedImage(&'static str);

impl PinnedImage {
    /// Panics unless `reference` ends in `@sha256:` and 64 lowercase hex digits after
    /// a repository, so a `const` built from a tag alone fails to compile.
    #[must_use]
    pub const fn new(reference: &'static str) -> Self {
        const MARKER: &[u8] = b"@sha256:";
        let bytes = reference.as_bytes();
        assert!(
            bytes.len() > MARKER.len() + 64,
            "a pinned image names a repository and a sha256 digest"
        );
        let (_, tail) = bytes.split_at(bytes.len() - MARKER.len() - 64);
        let (mut marker, mut digest) = tail.split_at(MARKER.len());
        let mut expected = MARKER;
        while let ([byte, rest @ ..], [want, wanted @ ..]) = (marker, expected) {
            assert!(*byte == *want, "a pinned image ends in @sha256:<digest>");
            marker = rest;
            expected = wanted;
        }
        while let [byte, rest @ ..] = digest {
            assert!(
                byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'),
                "a pinned image's digest is 64 lowercase hex digits"
            );
            digest = rest;
        }
        Self(reference)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for PinnedImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

validated_string_newtype!(
    /// A manifest digest as the Machine stored it: `sha256:` and 64 lowercase hex.
    ImageDigest, "image digest", "`sha256:` followed by 64 lowercase hexadecimal characters",
    |value| value.strip_prefix("sha256:").is_some_and(|hex| is_lower_hex(hex, 64))
);

impl ImageDigest {
    /// The digest without its `sha256:` algorithm prefix.
    #[must_use]
    pub fn hex(&self) -> &str {
        &self.as_str()["sha256:".len()..]
    }
}

validated_string_newtype!(
    /// An image digest as a builder, Docker or regctl reported it: `sha256:` and 64 hex digits
    /// of either case, kept exactly as given. The Build boundaries that took either case before
    /// ask this; [`ImageDigest`], lowercase only, is what a Machine stores and approvals name.
    ReportedImageDigest, "reported image digest", "`sha256:` followed by 64 hexadecimal characters",
    |value| value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
);

validated_string_newtype!(
    /// The one repository a Build Grant may push into, in Docker's short form:
    /// lowercase path components and no registry or tag (`ployz-build/web`).
    BuildGrantRepository, "Build Grant repository", "a Docker repository path without a registry or tag",
    |value| (1..=255).contains(&value.len())
        && value.split('/').all(|component| {
            component.bytes().next().is_some_and(|byte| byte.is_ascii_alphanumeric())
                && component.bytes().all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
        })
);

/// The repository path every built Service's image and Build Grant lives under.
pub const BUILD_REPOSITORY_PREFIX: &str = "ployz-build/";

impl BuildGrantRepository {
    /// The one repository a build of the Service named `service` may push into.
    ///
    /// # Errors
    /// Returns an error when `service` is not a repository path component.
    pub fn for_service(service: &impl fmt::Display) -> Result<Self, ValueError> {
        Self::parse(format!("{BUILD_REPOSITORY_PREFIX}{service}"))
    }
}

/// The image a Service's plan names before its build or upload has made one, which binding
/// replaces with the verified content.
#[must_use]
pub fn pending_image(service: &impl fmt::Display) -> String {
    format!("{BUILD_REPOSITORY_PREFIX}{service}:pending")
}

/// Whether `image` is a built Service's image or its pending placeholder, which no Device
/// can pin by digest before the build exists.
#[must_use]
pub fn is_built_image(image: &str) -> bool {
    image.starts_with(BUILD_REPOSITORY_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_grant_repository_is_a_short_docker_repository_path() {
        assert!(BuildGrantRepository::parse("ployz-build/web").is_ok());
        for refused in ["", "registry.example:5000/web", "web:tag", "Web", "a//b"] {
            assert!(BuildGrantRepository::parse(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn a_reported_image_digest_keeps_either_case_and_an_image_digest_only_lowercase() {
        let upper = format!("sha256:{}", "AB".repeat(32));
        assert_eq!(
            ReportedImageDigest::parse(upper.clone()).unwrap().as_str(),
            upper
        );
        assert!(ReportedImageDigest::parse(format!("sha256:{}", "aB1".repeat(21) + "f")).is_ok());
        assert!(ImageDigest::parse(upper).is_err());
        for refused in [
            "a".repeat(64),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}g", "a".repeat(63)),
            format!("SHA256:{}", "a".repeat(64)),
            String::new(),
        ] {
            assert!(
                ReportedImageDigest::parse(refused.clone()).is_err(),
                "{refused}"
            );
        }
    }

    #[test]
    fn an_image_digest_is_lowercase_sha256() {
        let digest = ImageDigest::parse(format!("sha256:{}", "a".repeat(64))).unwrap();
        assert_eq!(digest.hex(), "a".repeat(64));
        for refused in [
            "a".repeat(64),
            format!("sha256:{}", "A".repeat(64)),
            "sha256:abc".into(),
        ] {
            assert!(ImageDigest::parse(refused.clone()).is_err(), "{refused}");
        }
    }

    #[test]
    fn a_name_matches_only_its_own_repository() {
        for (left, right) in [
            ("app:v1", "docker.io/library/app:v1"),
            ("app", "app:latest"),
            ("index.docker.io/team/app:v1", "team/app:v1"),
            ("registry:5000/app:v1", "registry:5000/app:v1"),
        ] {
            assert!(same_image_name(left, right), "{left} {right}");
        }
        for (left, right) in [
            ("app:v1", "team/app:v1"),
            ("app:v1", "ghcr.io/app:v1"),
            ("registry:5000/app:v1", "registry:5001/app:v1"),
            ("app:v1", "app:v2"),
            ("Bad Name", "Bad Name"),
        ] {
            assert!(!same_image_name(left, right), "{left} {right}");
        }
    }

    #[test]
    fn a_registry_port_is_not_a_tag() {
        for (image, repository) in [
            ("registry:5000/web", "registry:5000/web"),
            ("registry:5000/web:2", "registry:5000/web"),
            ("registry:5000/web:2@sha256:aa", "registry:5000/web"),
            ("nginx:1.29@sha256:aa", "nginx"),
        ] {
            assert_eq!(image_repository(image), repository, "{image}");
        }
    }

    #[test]
    fn a_reference_names_content_only_by_one_lowercase_digest() {
        let hex = "0123456789abcdef".repeat(4);
        let upper = hex.to_uppercase();
        let digest = format!("sha256:{hex}");
        let upper_digest = format!("sha256:{upper}");
        let two_at = format!("b@{digest}");
        let content = ImageDigest::parse(digest.clone()).ok();
        for (image, name, at, repository, named, retained) in [
            ("nginx".to_owned(), "nginx", None, "nginx", None, None),
            ("nginx:1.29".into(), "nginx:1.29", None, "nginx", None, None),
            (
                "registry:5000/web".into(),
                "registry:5000/web",
                None,
                "registry:5000/web",
                None,
                None,
            ),
            (
                format!("registry:5000/web:2@{digest}"),
                "registry:5000/web:2",
                Some(digest.as_str()),
                "registry:5000/web",
                content.clone(),
                Some(format!("registry:5000/web{RETENTION_TAG}{hex}")),
            ),
            (
                format!("web@{digest}"),
                "web",
                Some(digest.as_str()),
                "web",
                content.clone(),
                Some(format!("web{RETENTION_TAG}{hex}")),
            ),
            (
                digest.clone(),
                digest.as_str(),
                None,
                "sha256",
                content,
                None,
            ),
            (
                format!("web@{upper_digest}"),
                "web",
                Some(upper_digest.as_str()),
                "web",
                None,
                Some(format!("web{RETENTION_TAG}{upper}")),
            ),
            (
                format!("a@{two_at}"),
                "a",
                Some(two_at.as_str()),
                "a",
                None,
                None,
            ),
            (
                format!("@{digest}"),
                "",
                Some(digest.as_str()),
                "",
                None,
                Some(format!("{RETENTION_TAG}{hex}")),
            ),
            (
                "web@sha256:abc".into(),
                "web",
                Some("sha256:abc"),
                "web",
                None,
                Some(format!("web{RETENTION_TAG}abc")),
            ),
        ] {
            let reference = ImageRef::new(&image);
            assert_eq!((reference.name, reference.digest), (name, at), "{image}");
            assert_eq!(image_repository(&image), repository, "{image}");
            assert_eq!(reference.content(), named, "{image}");
            assert_eq!(retention_tag(&image), retained, "{image}");
        }
    }
}
