use super::*;

/// Cloud and releases already in the field read these values. Each line restates one
/// frozen value, so an edit here fails and has to be argued as a wire change.
#[test]
fn every_value_is_frozen_by_release() {
    assert_eq!(UPLOAD_LIMIT, 99_614_720);
    assert_eq!(REVISION_HEADER, "x-ployz-manifest");
    assert_eq!(VIA_HEADER, "x-ployz-via");
    assert_eq!(APPROVAL_HEADER, "x-ployz-approval");
    assert_eq!(REQUEST_KEY_HEADER, "x-ployz-request-key");
    assert_eq!(AGENT_HEADER, "x-ployz-agent");
    assert_eq!(ERROR_CODE_HEADER, "Ployz-Error-Code");
    assert_eq!(CLIENT_ID, "ployz-cli");
    assert_eq!(MANIFEST_FORMAT, 1);
    assert_eq!(CHECKIN_PROTOCOL_VERSION, 1);
    assert_eq!(MAX_SKEW_SECONDS, 300);
    assert_eq!(CHECKIN_WAIT_SECONDS, 10);
    assert_eq!(ENROLL_OPTIONS, ["ingressClosed"]);
    assert_eq!(PROBE_WINDOW_MS, 10_000);
    assert_eq!(PROBE_FIRST_ATTEMPT_MS, 1_000);
    assert_eq!(PROBE_ATTEMPT_EVERY_MS, 1_500);
    assert_eq!(PROBE_ATTEMPT_TIMEOUT_MS, 3_000);
    assert_eq!(PROBE_GREETING_READ_MS, 500);
    assert_eq!(PROBE_GREETING_LENGTH, 22);
    assert_eq!(MIN_MANAGED_VOLUME_BYTES, 100_000_000);
}

/// The Machine checks these without Cloud, so Cloud may only read them.
#[test]
fn machine_enforced_lifetimes_are_the_authority_values() {
    assert_eq!(MAX_LIFETIME_MILLIS, 15 * 60 * 1000);
    assert_eq!(CLOCK_SKEW_MILLIS, 2 * 60 * 1000);
    assert_eq!(MAX_GRANT_LIFETIME_MILLIS, 60 * 60 * 1000);
}
