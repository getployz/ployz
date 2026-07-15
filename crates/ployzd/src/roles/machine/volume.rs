use ployz_core::deploy::VolumeName;
use ployz_core::ids::NamespaceId;

pub(crate) fn docker_volume_name(namespace_id: &NamespaceId, volume_name: &VolumeName) -> String {
    let namespace_id = namespace_id.as_str();
    let volume_name = volume_name.as_str();
    format!(
        "ployz-n{}-{}-v{}-{}",
        namespace_id.len(),
        namespace_id,
        volume_name.len(),
        volume_name
    )
}

#[cfg(test)]
mod tests {
    use super::docker_volume_name;
    use ployz_core::deploy::VolumeName;
    use ployz_test_support::ids::namespace_id;

    fn volume_name(value: &str) -> VolumeName {
        VolumeName::try_new(value).expect("valid volume name")
    }

    #[test]
    fn docker_volume_names_are_framed_to_avoid_collisions() {
        let left = namespace_id("a-b");
        let right = namespace_id("a");

        assert_ne!(
            docker_volume_name(&left, &volume_name("c")),
            docker_volume_name(&right, &volume_name("b-c"))
        );
    }
}
