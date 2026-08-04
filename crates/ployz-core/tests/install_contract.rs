use ployz_core::install::{
    AbsoluteInstallPath, ExactPloyzVersion, HostPortAssurance, InstallArtifactSource,
    InstallArtifactSpec, InstallArtifactVersion, InstallSha256Digest,
};

#[test]
fn absolute_install_paths_require_a_file_beneath_root() {
    assert!(AbsoluteInstallPath::try_new("").is_err());
    assert!(AbsoluteInstallPath::try_new("usr/local/bin/ployzd").is_err());
    assert!(AbsoluteInstallPath::try_new("/").is_err());
    assert!(AbsoluteInstallPath::try_new("/usr/local/bin/").is_err());
    assert!(AbsoluteInstallPath::try_new("/usr/local/bin/ployzd").is_ok());
}

#[test]
fn exact_versions_reject_mutable_or_range_selectors() {
    assert!(ExactPloyzVersion::try_new("").is_err());
    assert!(ExactPloyzVersion::try_new("latest").is_err());
    assert!(ExactPloyzVersion::try_new("^1.2").is_err());

    let version = ExactPloyzVersion::try_new("v1.2.3-alpha.1").expect("exact version");
    assert_eq!(version.as_str(), "1.2.3-alpha.1");
    assert_eq!(version.tag(), "v1.2.3-alpha.1");
}

#[test]
fn artifact_sources_are_http_urls_or_absolute_paths() {
    assert!(InstallArtifactSource::try_new("ployzd").is_err());
    assert!(InstallArtifactSource::try_new("https://example.test/ployzd").is_ok());
    assert!(InstallArtifactSource::try_new("http://127.0.0.1/ployzd").is_ok());
    assert!(InstallArtifactSource::try_new("/tmp/ployzd").is_ok());
}

#[test]
fn sha256_digests_are_normalized_and_validated() {
    assert!(InstallSha256Digest::try_new("").is_err());
    assert!(InstallSha256Digest::try_new("abcd").is_err());

    let digest = InstallSha256Digest::try_new("AB".repeat(32)).expect("sha256");
    assert_eq!(digest.as_str(), "ab".repeat(32));
}

#[test]
fn artifact_specs_round_trip_without_transport_metadata() {
    let spec = InstallArtifactSpec {
        version: InstallArtifactVersion::try_new("1.2.3").expect("artifact version"),
        source: InstallArtifactSource::try_new("https://example.test/ployzd")
            .expect("artifact source"),
        sha256: InstallSha256Digest::try_new("ab".repeat(32)).expect("sha256"),
        install_path: AbsoluteInstallPath::try_new("/usr/local/bin/ployzd").expect("install path"),
    };

    let encoded = serde_json::to_value(&spec).expect("artifact spec serializes");
    assert_eq!(
        serde_json::from_value::<InstallArtifactSpec>(encoded).expect("artifact spec deserializes"),
        spec
    );
}

#[test]
fn host_port_assurance_has_explicit_modes() {
    assert_eq!(HostPortAssurance::keeper(), HostPortAssurance::Keeper);
    assert_ne!(HostPortAssurance::Keeper, HostPortAssurance::External);
}
