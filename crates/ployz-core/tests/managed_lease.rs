use ployz_core::cert::{
    LeaseBearerToken, LeaseExpiresAt, LeaseIssuedAt, ManagedCertBundle, ManagedLeaseName,
    ManagedLeaseRecord,
};

#[test]
fn managed_lease_name_accepts_random_slug_label() {
    let lease = ManagedLeaseName::try_new("brisk-river-x7f3").expect("valid lease name");

    assert_eq!(lease.hostname_suffix(), "brisk-river-x7f3.up.ployz.app");
}

#[test]
fn managed_lease_name_rejects_hostnames_outside_up_ployz_app() {
    assert!(ManagedLeaseName::try_new("tenant.example.com").is_err());
}

#[test]
fn managed_bundle_covers_wildcard_and_apex_for_lease() {
    let lease = ManagedLeaseName::try_new("brisk-river-x7f3").expect("valid lease name");
    let issued_at = LeaseIssuedAt::try_new(1_700_000_000).expect("valid issued timestamp");
    let expires_at = LeaseExpiresAt::try_new(1_700_604_800).expect("valid expiry timestamp");
    let record = ManagedLeaseRecord::try_new(
        lease.clone(),
        LeaseBearerToken::try_new("lease_token_123").expect("valid token"),
        issued_at,
        expires_at,
    )
    .expect("valid lease record");

    let bundle = ManagedCertBundle::try_new(
        record.name.clone(),
        record.name.wildcard_and_apex(),
        "-----BEGIN CERTIFICATE-----\nplaceholder\n-----END CERTIFICATE-----\n".to_owned(),
        "-----BEGIN PRIVATE KEY-----\nplaceholder\n-----END PRIVATE KEY-----\n".to_owned(),
        record.issued_at,
        record.expires_at,
    )
    .expect("valid bundle");

    assert_eq!(
        bundle.dns_names(),
        [
            "*.brisk-river-x7f3.up.ployz.app",
            "brisk-river-x7f3.up.ployz.app"
        ]
    );
}
