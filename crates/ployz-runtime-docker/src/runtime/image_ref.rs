use ployz_model::ImageDigest;

pub struct DockerImageRef<'a> {
    pub from_image: &'a str,
    pub tag: Option<&'a str>,
}

#[must_use]
pub fn digest_from_image_ref(image: &str) -> Option<ImageDigest> {
    let (_, digest) = image.rsplit_once('@')?;
    ImageDigest::try_new(digest).ok()
}

pub fn require_digest_from_image_ref(image: &str) -> Result<ImageDigest, String> {
    digest_from_image_ref(image).ok_or_else(|| {
        format!("image reference '{image}' must include a digest for availability tracking")
    })
}

#[must_use]
pub fn parse_docker_image_ref(image: &str) -> DockerImageRef<'_> {
    if image.contains('@') {
        return DockerImageRef {
            from_image: image,
            tag: None,
        };
    }

    let last_slash = image.rfind('/');
    let last_colon = image.rfind(':');

    if let Some(colon_index) = last_colon
        && last_slash.is_none_or(|slash_index| colon_index > slash_index)
    {
        return DockerImageRef {
            from_image: &image[..colon_index],
            tag: Some(&image[colon_index + 1..]),
        };
    }

    DockerImageRef {
        from_image: image,
        tag: Some("latest"),
    }
}

#[cfg(test)]
mod tests {
    use super::{digest_from_image_ref, parse_docker_image_ref, require_digest_from_image_ref};

    #[test]
    fn parses_repo_without_tag() {
        let parsed = parse_docker_image_ref("ghcr.io/getployz/ployz-dns");

        assert_eq!(parsed.from_image, "ghcr.io/getployz/ployz-dns");
        assert_eq!(parsed.tag, Some("latest"));
    }

    #[test]
    fn parses_repo_with_tag() {
        let parsed = parse_docker_image_ref("ghcr.io/getployz/ployz-dns:v1");

        assert_eq!(parsed.from_image, "ghcr.io/getployz/ployz-dns");
        assert_eq!(parsed.tag, Some("v1"));
    }

    #[test]
    fn parses_registry_port_with_tag() {
        let parsed = parse_docker_image_ref("localhost:5000/ployz-dns:v1");

        assert_eq!(parsed.from_image, "localhost:5000/ployz-dns");
        assert_eq!(parsed.tag, Some("v1"));
    }

    #[test]
    fn preserves_tag_and_digest_refs() {
        let parsed = parse_docker_image_ref(
            "ghcr.io/getployz/ployz-dns:latest@sha256:67e759f7a0f0480c8dc533856344bad79296006de85b36a8943063d72f598356",
        );

        assert_eq!(
            parsed.from_image,
            "ghcr.io/getployz/ployz-dns:latest@sha256:67e759f7a0f0480c8dc533856344bad79296006de85b36a8943063d72f598356"
        );
        assert_eq!(parsed.tag, None);
    }

    #[test]
    fn extracts_digest_from_digest_pinned_ref() {
        let digest = digest_from_image_ref(
            "ghcr.io/getployz/ployz-dns@sha256:67e759f7a0f0480c8dc533856344bad79296006de85b36a8943063d72f598356",
        )
        .expect("digest");

        assert_eq!(
            digest.as_str(),
            "sha256:67e759f7a0f0480c8dc533856344bad79296006de85b36a8943063d72f598356"
        );
    }

    #[test]
    fn extracts_digest_from_tag_and_digest_ref() {
        let digest = digest_from_image_ref(
            "ghcr.io/getployz/ployz-dns:latest@sha256:67e759f7a0f0480c8dc533856344bad79296006de85b36a8943063d72f598356",
        )
        .expect("digest");

        assert_eq!(
            digest.as_str(),
            "sha256:67e759f7a0f0480c8dc533856344bad79296006de85b36a8943063d72f598356"
        );
    }

    #[test]
    fn tag_only_refs_do_not_have_availability_digest() {
        assert!(digest_from_image_ref("ghcr.io/getployz/ployz-dns:latest").is_none());
        assert!(require_digest_from_image_ref("ghcr.io/getployz/ployz-dns:latest").is_err());
    }

    #[test]
    fn invalid_digest_ref_is_not_accepted_for_availability() {
        assert!(digest_from_image_ref("ghcr.io/getployz/ployz-dns@sha256:not-hex").is_none());
    }
}
