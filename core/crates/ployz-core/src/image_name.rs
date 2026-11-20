//! How the client and every daemon read an image name. A retention name the client writes,
//! the pinned reference a destination pulls and the tag a lookup matches must agree, so
//! they come from here.

use crate::ImageDigest;

/// What follows a repository in the tag that keeps a pinned image from being pruned. A
/// daemon of another version reads these tags, so the text does not change.
pub const RETENTION_TAG: &str = ":ployz-sha256-";

/// `image` without its tag or digest, as written, registry and port kept:
/// `registry:5000/api` of `registry:5000/api:v1@sha256:…`.
#[must_use]
pub fn image_repository(image: &str) -> &str {
    let name = image.split_once('@').map_or(image, |(name, _)| name);
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
    let (_, hex) = image.split_once("@sha256:")?;
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
    let whole = |name: &str| {
        name.parse::<oci_spec::distribution::Reference>()
            .map(|reference| reference.whole())
    };
    matches!((whole(left), whole(right)), (Ok(left), Ok(right)) if left == right)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
