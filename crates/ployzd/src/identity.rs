pub(crate) fn format_nuid_identity(prefix: &str, nuid: &str) -> String {
    format!("{prefix}{nuid}")
}

#[cfg(test)]
mod tests {
    use super::format_nuid_identity;

    #[test]
    fn generated_identity_prefixes_preserve_case_distinct_nuids() {
        for prefix in ["", "upload_", "ployz-image-", "ployz-image-upload-"] {
            let upper = format_nuid_identity(prefix, "A1");
            let lower = format_nuid_identity(prefix, "a1");

            assert_eq!(upper, format!("{prefix}A1"));
            assert_eq!(lower, format!("{prefix}a1"));
            assert_ne!(upper, lower);
        }
    }
}
