use ployz_core::deploy::VolumeName;
use ployz_core::ids::NamespaceId;

pub(crate) fn docker_volume_name(namespace_id: &NamespaceId, volume_name: &VolumeName) -> String {
    volume_name.stable_storage_name(namespace_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_volume_name_uses_the_stable_storage_identity() {
        let namespace = NamespaceId::try_new("default").expect("namespace");
        let volume = VolumeName::try_new("data").expect("volume");

        assert_eq!(
            docker_volume_name(&namespace, &volume),
            volume.stable_storage_name(&namespace)
        );
    }
}
