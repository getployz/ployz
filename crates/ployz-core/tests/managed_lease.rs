use ployz_core::certificate::{
    LeaseBearerToken, LeaseExpiresAt, LeaseIssuedAt, ManagedCertBundle,
    ManagedCertificateIssuanceFailureKind, ManagedLeaseAcquired, ManagedLeaseName,
    ManagedLeaseRecord, ManagedLeaseRenewed,
};

#[test]
fn lease_bearer_tokens_are_redacted_from_debug_and_validation_errors() {
    let token = LeaseBearerToken::try_new("lease_token_123").expect("token");
    assert_eq!(format!("{token:?}"), "LeaseBearerToken(\"[REDACTED]\")");

    let secret = "secret!must-not-leak";
    let error = LeaseBearerToken::try_new(secret).expect_err("invalid token");
    assert!(!format!("{error:?}").contains(secret));
    assert!(!error.to_string().contains(secret));
}

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

#[test]
fn managed_bundle_validity_is_a_half_open_time_window() {
    let lease = ManagedLeaseName::try_new("brisk-river-x7f3").expect("valid lease name");
    let bundle = ManagedCertBundle::try_new(
        lease.clone(),
        lease.wildcard_and_apex(),
        "certificate".to_owned(),
        "private-key".to_owned(),
        LeaseIssuedAt::try_new(100).expect("issued at"),
        LeaseExpiresAt::try_new(200).expect("expires at"),
    )
    .expect("valid bundle");

    assert!(!bundle.is_valid_at(99));
    assert!(bundle.is_valid_at(100));
    assert!(bundle.is_valid_at(199));
    assert!(!bundle.is_valid_at(200));
}

#[test]
fn managed_lease_record_deserialization_rejects_inverted_validity() {
    let value = serde_json::json!({
        "name": "brisk-river-x7f3",
        "token": "lease_token_123",
        "issued_at": "1700604800",
        "expires_at": "1700000000"
    });

    assert!(serde_json::from_value::<ManagedLeaseRecord>(value).is_err());
}

#[test]
fn managed_lease_acquisition_accepts_pending_bundle() {
    let acquired: ManagedLeaseAcquired = serde_json::from_value(serde_json::json!({
        "lease": {
            "name": "brisk-river-x7f3",
            "token": "lease_token_123",
            "issued_at": "1700000000",
            "expires_at": "1700604800"
        },
        "bundle": null
    }))
    .expect("pending acquisition response");

    assert!(acquired.bundle.is_none());
}

#[test]
fn managed_lease_renewal_accepts_pending_bundle() {
    let renewed: ManagedLeaseRenewed = serde_json::from_value(serde_json::json!({
        "lease": {
            "name": "brisk-river-x7f3",
            "token": "lease_token_123",
            "issued_at": "1700000000",
            "expires_at": "1700604800"
        },
        "bundle": null
    }))
    .expect("pending renewal response");

    assert!(renewed.bundle.is_none());
}

#[test]
fn managed_certificate_issuance_failures_use_worker_wire_values() {
    let failures = [
        ManagedCertificateIssuanceFailureKind::RateLimit,
        ManagedCertificateIssuanceFailureKind::Provider5xx,
        ManagedCertificateIssuanceFailureKind::ValidationTimeout,
        ManagedCertificateIssuanceFailureKind::Caa,
        ManagedCertificateIssuanceFailureKind::DnsTxtMissing,
        ManagedCertificateIssuanceFailureKind::ProviderError,
    ];

    assert_eq!(
        serde_json::to_value(failures).expect("failure kinds serialize"),
        serde_json::json!([
            "rate_limit",
            "provider_5xx",
            "validation_timeout",
            "caa",
            "dns_txt_missing",
            "provider_error"
        ])
    );
}

#[test]
fn managed_bundle_deserialization_rejects_wrong_digest() {
    let lease = ManagedLeaseName::try_new("brisk-river-x7f3").expect("valid lease name");
    let bundle = ManagedCertBundle::try_new(
        lease.clone(),
        lease.wildcard_and_apex(),
        "certificate".to_owned(),
        "private-key".to_owned(),
        LeaseIssuedAt::try_new(1_700_000_000).expect("issued at"),
        LeaseExpiresAt::try_new(1_700_604_800).expect("expires at"),
    )
    .expect("valid bundle");
    let mut value = serde_json::to_value(bundle).expect("bundle serializes");
    value.as_object_mut().expect("bundle is an object").insert(
        "digest".to_owned(),
        serde_json::Value::String("0".repeat(64)),
    );

    assert!(serde_json::from_value::<ManagedCertBundle>(value).is_err());
}

#[test]
fn managed_bundle_deserialization_rejects_wrong_dns_names() {
    let lease = ManagedLeaseName::try_new("brisk-river-x7f3").expect("valid lease name");
    let bundle = ManagedCertBundle::try_new(
        lease.clone(),
        lease.wildcard_and_apex(),
        "certificate".to_owned(),
        "private-key".to_owned(),
        LeaseIssuedAt::try_new(1_700_000_000).expect("issued at"),
        LeaseExpiresAt::try_new(1_700_604_800).expect("expires at"),
    )
    .expect("valid bundle");
    let mut value = serde_json::to_value(bundle).expect("bundle serializes");
    value.as_object_mut().expect("bundle is an object").insert(
        "dns_names".to_owned(),
        serde_json::json!(["*.example.com", "example.com"]),
    );

    assert!(serde_json::from_value::<ManagedCertBundle>(value).is_err());
}
