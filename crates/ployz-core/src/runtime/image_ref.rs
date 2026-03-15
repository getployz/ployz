pub struct DockerImageRef<'a> {
    pub from_image: &'a str,
    pub tag: Option<&'a str>,
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
    use super::parse_docker_image_ref;

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
}
